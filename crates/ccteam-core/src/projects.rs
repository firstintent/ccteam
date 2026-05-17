//! Project bootstrap helpers used by `ccteam new` (and reusable by the
//! M3+ inbox triage path). Pure: no tmux side effects, just file
//! creation under `~/projects/<slug>/`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::team::TeamSpec;
use crate::team_resolver::TeamResolveContext;
use crate::templates::{
    write_global_helper_templates, write_project_settings, EnabledPluginsSetting,
};

/// Slugify a free-text project request: keep `[a-z0-9]`, collapse other
/// runs to `-`, trim, lower-case, and cap at 40 chars. When the cap
/// would split a word, the slug is rolled back to the previous `-` so
/// e.g. "Build a tiny Python CLI that converts CSV to JSON" stays
/// `build-a-tiny-python-cli-that-converts` rather than `...converts-cs`.
/// Empty result is replaced by `project`.
pub fn slugify(input: &str) -> String {
    const MAX: usize = 40;
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return "project".into();
    }
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    // Hard cap at MAX, then if we cut mid-word roll back to the
    // previous `-` so we don't ship a half-token like `converts-cs`.
    // Single tokens longer than MAX (e.g. `aaaa…`) keep the hard cut
    // since there's no boundary to fall back to.
    let head = &trimmed[..MAX];
    let trimmed_head = match head.rfind('-') {
        Some(idx) if idx > 0 => &head[..idx],
        _ => head,
    };
    trimmed_head.trim_end_matches('-').to_string()
}

/// 4-char hex suffix derived from the current sub-second wallclock.
/// Good enough for collision avoidance under interactive use; the
/// caller can retry on collision.
pub fn random_suffix() -> String {
    let nanos = Utc::now().timestamp_subsec_nanos();
    format!("{:04x}", nanos & 0xFFFF)
}

/// V0.2.2 F34 Tier 4: token-aware deterministic slug generator. Used
/// when the meta-agent / `claude -p` tier is unavailable (no LLM,
/// `--no-auto-slug`, env `CCTEAM_AUTO_SLUG=off`). Improves over
/// `slugify()`'s 40-char char-level cap by:
///
/// 1. Reusing `slugify` for character normalization (`[a-z0-9]` +
///    `-` collapsing).
/// 2. Splitting on `-` into tokens; filtering out:
///    - English stop words (`a`/`an`/`the`/`of`/`to`/`for`/`with`/
///      `that`/`and`/`or`/`in`/`on`/`at`/`is`/`are`).
///    - Pure-digit tokens (`v2` / `2k` are kept because they contain
///      letters; `2` / `42` are dropped).
///    - Tokens shorter than 2 chars.
/// 3. De-duplicating consecutive repeats (`ccteam ccteam ui` →
///    `ccteam ui`).
/// 4. Taking the first 3 surviving tokens, joined by `-`.
/// 5. If everything was filtered, falling back to the raw `slugify()`
///    output so the caller never gets an empty slug.
///
/// **`slugify()` is not modified** — it still backs the meta-agent
/// `meta-<handle>` path where the handle should be normalized
/// verbatim.
pub fn slugify_brief(input: &str) -> String {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "of", "to", "for", "with", "that", "and", "or", "in", "on", "at", "is",
        "are",
    ];
    const MAX_TOKENS: usize = 3;

    let normalized = slugify(input);
    if normalized == "project" {
        // The char-level path already gave up; nothing for the
        // token filter to do.
        return normalized;
    }

    let mut kept: Vec<&str> = Vec::new();
    for token in normalized.split('-') {
        if token.len() < 2 {
            continue;
        }
        if STOP_WORDS.contains(&token) {
            continue;
        }
        if token.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if kept.last().is_some_and(|prev| *prev == token) {
            continue;
        }
        kept.push(token);
        if kept.len() >= MAX_TOKENS {
            break;
        }
    }

    if kept.is_empty() {
        // Everything got filtered (e.g. brief "to of and"). Fall
        // back to the raw normalized slug so the caller still gets
        // something usable rather than `project`.
        return normalized;
    }
    kept.join("-")
}

/// Pick an unused slug under `paths.projects_root`, prefixed with the
/// project's team name so `~/.claude/rules/ccteam-lessons-<team>.md`
/// `paths:` frontmatter (`~/projects/<team>-*`) actually matches the
/// project directory at session start (M4 main path; F22 fix, 2026-05-06).
///
/// Tries `<team>-<base>` first, then `<team>-<base>-<suffix>` with up
/// to 16 retries on collision.
///
/// V0.2.2 F34: the `base` argument is a free-text request — it gets
/// run through `slugify_brief` (token-aware) so `<team>-<base>` stays
/// readable. Callers that already have a deliberate slug (eg
/// `--slug ccteam-ui`) should use [`pick_unused_slug_verbatim`] which
/// skips token filtering and only enforces team prefix + collision
/// retry.
///
/// Meta-agent projects don't go through this function — they use
/// `meta_slug(handle)` which hand-crafts `meta-<handle>` so the
/// directory aligns with the `ccteam-meta-<handle>` tmux session.
pub fn pick_unused_slug(paths: &CcteamPaths, base: &str, team: &str) -> Result<String> {
    let base = slugify_brief(base);
    pick_unused_under_team_prefix(paths, &base, team)
}

/// V0.4.2 F75 reviewer fix: validate that `slug` matches the on-disk
/// slug grammar — `[a-z0-9][a-z0-9-]*`, length ≤ 60, no leading /
/// trailing dash. Returns the trimmed string on success. Use this
/// from CLI parsers (`ccteam init --slug`, `ccteam new <slug>`) so
/// invalid input fails loud before any directory is created.
pub fn validate_slug_format(slug: &str) -> Result<String> {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("slug must be non-empty"));
    }
    if trimmed.len() > 60 {
        return Err(anyhow!(
            "slug too long ({} chars > 60); use a shorter name",
            trimmed.len()
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(anyhow!(
            "slug must match [a-z0-9-]+ (lowercase ASCII, digits, dashes only); got {trimmed:?}",
        ));
    }
    if trimmed.starts_with('-') || trimmed.ends_with('-') {
        return Err(anyhow!(
            "slug must not start or end with `-`; got {trimmed:?}",
        ));
    }
    Ok(trimmed.to_string())
}

/// V0.2.2 F34 Tier 1: pick an unused slug from a deliberate user-
/// chosen base. Skips token filtering (the user has already named
/// the project) and only does:
///
/// - Validate `[a-z0-9-]+`, length ≤ 60, no leading / trailing dash.
/// - B2 prefix semantics: if `slug` already starts with `<team>-`
///   keep it verbatim; otherwise prepend `<team>-`.
/// - Collision retry via `-{4hex}` suffix (same as `pick_unused_slug`).
pub fn pick_unused_slug_verbatim(paths: &CcteamPaths, slug: &str, team: &str) -> Result<String> {
    let trimmed = validate_slug_format(slug)?;
    let team_prefix = format!("{team}-");
    let prefixed = if trimmed.starts_with(&team_prefix) || trimmed == team {
        trimmed.to_string()
    } else {
        format!("{team_prefix}{trimmed}")
    };
    pick_unused_with_prefixed(paths, &prefixed)
}

/// Internal helper: takes an already-prefixed slug, returns the same
/// or a `-{4hex}` retry on collision. Shared between the verbatim
/// (`--slug`) and brief-derived paths.
fn pick_unused_under_team_prefix(paths: &CcteamPaths, base: &str, team: &str) -> Result<String> {
    let prefixed = format!("{team}-{base}");
    pick_unused_with_prefixed(paths, &prefixed)
}

fn pick_unused_with_prefixed(paths: &CcteamPaths, prefixed: &str) -> Result<String> {
    if !paths.project_dir(prefixed).exists() {
        return Ok(prefixed.to_string());
    }
    for _ in 0..16 {
        let candidate = format!("{prefixed}-{}", random_suffix());
        if !paths.project_dir(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "could not pick an unused slug after 16 attempts (base: {prefixed})",
    ))
}

/// Write the bootstrap files for a fresh project:
/// - `<project>/.ccteam/spec.md` ← `request`
/// - `<project>/.ccteam/state.json` ← `ProjectState::initial_for_team(slug, team)`
/// - `<project>/.claude/settings.json` ← M0.4 template
/// - `<project>/CLAUDE.md` ← header + spec link
///
/// `team` lands in state.json so the orchestrator can route this
/// project through the matching phase set (M3.1 F12/F13).
///
/// Returns the full project directory path.
pub fn bootstrap_project(
    paths: &CcteamPaths,
    slug: &str,
    request: &str,
    team: &str,
) -> Result<PathBuf> {
    bootstrap_project_at_dir(paths, &paths.project_dir(slug), slug, request, team)
}

/// V0.4.2 F72: `bootstrap_project` generalized to install at an
/// arbitrary `target_dir`. Used by `ccteam init` to bring an existing
/// repo under management without moving files. Side effects (helper
/// templates, trust-marking) still use `paths.root` / `paths.projects_root`,
/// since those are global concerns.
///
/// `target_dir` is created if it doesn't exist. Existing files inside
/// it are left alone — this is the **fresh-install** path. Idempotent
/// refresh of an already-installed project lives in `ccteam init`'s
/// refresh helpers; this function assumes the caller already decided
/// "this is a new install".
pub fn bootstrap_project_at_dir(
    paths: &CcteamPaths,
    target_dir: &Path,
    slug: &str,
    request: &str,
    team: &str,
) -> Result<PathBuf> {
    let project_dir = target_dir.to_path_buf();
    let ccteam_dir = project_dir.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let spec_path = ccteam_dir.join("spec.md");
    let spec_body = format!(
        "---\nslug: {slug}\ncreated_at: {now}\nteam: {team}\n---\n\n# 用户需求\n\n{request}\n",
    );
    std::fs::write(&spec_path, spec_body)
        .with_context(|| format!("write {}", spec_path.display()))?;

    let state = ProjectState::initial_for_team(slug.to_string(), team.to_string());
    state.save(&CcteamPaths::project_state_in(&project_dir))?;

    // V0.4.0 F60: the phase-template-driven `enabledPlugins` resolver
    // (`compute_enabled_plugins` + `load_phase_templates_for_bootstrap`)
    // was deleted with the rest of the phase machinery. F66 will
    // reintroduce the plugin enablement set computed from
    // `workflow.yaml::agents.<role>.executor`. For now we lay down
    // settings.json with an empty plugin set so the project bootstraps
    // cleanly; user-authored agent prompts still resolve their plugin
    // surface via the global `~/.claude/agents/` layer.
    let resolved_spec = resolve_project_team_spec(paths, team);
    let enabled_plugins = EnabledPluginsSetting::default();

    write_project_settings(&project_dir, &enabled_plugins)?;
    // M2.4: ensure ~/.ccteam/templates/ has the helper templates so
    // user-authored agent / workflow markdown's `@~/.ccteam/templates/<name>.md`
    // reference resolves. Idempotent — no-ops when files already exist,
    // so this doesn't fight `ccteam init` if the operator ran it first.
    if let Err(err) = write_global_helper_templates(&paths.root, false) {
        tracing::warn!(
            global_dir = %paths.root.display(),
            error = %err,
            "could not stamp helper templates into ~/.ccteam/templates/; markdown using @-references may fail",
        );
    }
    if let Err(err) = pre_trust_project(&project_dir) {
        // Failing to pre-trust is annoying (next launch shows the
        // "Trust this folder?" prompt) but not fatal — log + continue.
        tracing::warn!(
            project_dir = %project_dir.display(),
            error = %err,
            "could not pre-trust project in ~/.claude.json; first claude launch may show trust prompt",
        );
    }

    let claude_md = project_dir.join("CLAUDE.md");
    if !claude_md.exists() {
        let body = render_project_claude_md(slug, team, resolved_spec.as_ref());
        std::fs::write(&claude_md, body)
            .with_context(|| format!("write {}", claude_md.display()))?;
    }

    Ok(project_dir)
}

fn resolve_project_team_spec(paths: &CcteamPaths, team: &str) -> Option<TeamSpec> {
    let user_staging = crate::team_resolver::default_user_staging_dir();
    let ctx = TeamResolveContext::for_orchestrator(&paths.root, &user_staging);
    crate::team_resolver::resolve_team(team, &ctx).ok()
}

/// Build the `<project>/CLAUDE.md` body for `team`. The per-team body
/// lives in `team.yaml.claude_md_template`; templates contain the
/// literal placeholders `{slug}` / `{team}`, substituted here.
///
/// Lookup precedence:
/// 1. The resolved `TeamSpec`'s `claude_md_template` (for user-authored
///    teams resolved via `team_resolver`).
/// 2. A generic body that doesn't bake in dev / research assumptions —
///    used for unknown teams (user-authored without a template).
///
/// V0.4.0 F60: the shipped `TEAM_BUNDLES` fallback was deleted with the
/// rest of the phase machinery; user-authored teams must carry their
/// own `claude_md_template`, otherwise the generic body is rendered.
fn render_project_claude_md(slug: &str, team: &str, resolved_spec: Option<&TeamSpec>) -> String {
    let template = resolved_spec
        .filter(|spec| !spec.claude_md_template.trim().is_empty())
        .map(|spec| spec.claude_md_template.clone());
    let body = match template {
        Some(t) => t,
        None => generic_claude_md_template().to_string(),
    };
    body.replace("{slug}", slug).replace("{team}", team)
}

/// Fallback body for teams without an explicit `claude_md_template`.
/// Carries no team-specific contract — phase markdown owns that.
fn generic_claude_md_template() -> &'static str {
    "# CLAUDE.md (auto-managed by ccteam)\n\
     \n\
     ## 项目上下文\n\
     - slug: {slug}\n\
     - team: {team}\n\
     - 用户原始需求: 见 .ccteam/spec.md\n\
     \n\
     ## 工作约定\n\
     - 跟随该 team 的 workflow.yaml / agent 指示。\n\
     - 不要修改 .ccteam/ 之外的元数据。\n"
}

/// Pre-mark `project_dir` as trusted in `~/.claude.json` so the first
/// `claude --dangerously-skip-permissions` launch in this directory
/// doesn't sit on the "Trust this folder?" prompt waiting for the
/// keyboard.
///
/// **Test isolation** (two opt-in mechanisms — same shape as
/// `setup_tool_surface`):
///
/// 1. `CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP=1` → no-op entirely.
///    Tests that exercise `bootstrap_project` for unrelated assertions
///    set this via `disable_tool_surface_bootstrap_for_tests()` and
///    don't want either the agent symlinks or the trust entry leaking
///    into the developer's real home.
/// 2. `CLAUDE_CONFIG_HOME=<dir>` → write to `<dir>/../.claude.json`
///    instead of `$HOME/.claude.json`. Mirrors the resolution
///    `user_claude_dir()` does for the agents/skills surface so a test
///    setting just `CLAUDE_CONFIG_HOME=<tmp>/.claude` gets full
///    redirection across the whole tool surface.
///
/// Without these guards, every test invoking `bootstrap_project` would
/// append a `/tmp/.tmpXXXXXX/projects/<slug>` entry to the developer's
/// real `~/.claude.json`, eventually bloating the file enough to break
/// Claude login (regression observed 2026-05-06).
///
/// No-ops gracefully if the resolved `.claude.json` is unwritable.
pub fn pre_trust_project(project_dir: &Path) -> Result<()> {
    if std::env::var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
    {
        tracing::debug!(
            "CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP set; skipping ~/.claude.json trust entry write",
        );
        return Ok(());
    }
    let claude_json = resolve_claude_json_path()?;
    write_trust_entry(&claude_json, project_dir)
}

/// Resolve which `.claude.json` to write the trust entry into.
///
/// `CLAUDE_CONFIG_HOME` takes precedence so the production-equivalent
/// isolation tests (`tool_surface_e2e_test`-style) get a redirected
/// trust write too. Production sets neither and falls through to
/// `dirs::home_dir()`.
///
/// Also reused by `ccteam doctor --install-mcp` (V0.2.1 F26) so the
/// MCP install path honors the same env redirection as the trust-entry
/// writer and the sibling `--install-skill` / `--install-memory-bridge`
/// paths.
pub fn resolve_claude_json_path() -> Result<PathBuf> {
    resolve_claude_json_path_from_env(std::env::var("CLAUDE_CONFIG_HOME").ok(), dirs::home_dir())
}

/// Pure resolution helper for `resolve_claude_json_path`. Factored out
/// so unit tests can exercise the path logic without mutating process
/// env vars (which would race against parallel tests in the same
/// binary).
fn resolve_claude_json_path_from_env(
    config_home: Option<String>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(s) = config_home {
        let claude_dir = PathBuf::from(s);
        // CLAUDE_CONFIG_HOME points at the `.claude/` dir; `.claude.json`
        // is its sibling. If the env var has no parent (root path,
        // weird input), fall back to writing inside it — better than
        // silently touching the real home.
        return Ok(match claude_dir.parent() {
            Some(parent) => parent.join(".claude.json"),
            None => claude_dir.join(".claude.json"),
        });
    }
    let h = home.ok_or_else(|| anyhow!("could not resolve home directory for ~/.claude.json"))?;
    Ok(h.join(".claude.json"))
}

/// `pre_trust_project` core, factored out for unit testing with an
/// injected `~/.claude.json` location.
pub(crate) fn write_trust_entry(claude_json: &Path, project_dir: &Path) -> Result<()> {
    let project_key = project_dir
        .to_str()
        .ok_or_else(|| anyhow!("project_dir not valid UTF-8: {}", project_dir.display()))?;

    let mut root = if claude_json.exists() {
        let bytes = std::fs::read(claude_json)
            .with_context(|| format!("read {}", claude_json.display()))?;
        let v: Value = if bytes.is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", claude_json.display()))?
        };
        match v {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    } else {
        Map::new()
    };

    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    let projects_map = match projects {
        Value::Object(m) => m,
        // someone (or a corrupted file) put a non-object at `projects` —
        // overwrite rather than refusing, since refusing means every
        // future launch sits at the trust prompt.
        _ => {
            *projects = Value::Object(Map::new());
            projects.as_object_mut().unwrap()
        }
    };

    let entry = projects_map
        .entry(project_key)
        .or_insert_with(|| Value::Object(Map::new()));
    let entry_map = match entry {
        Value::Object(m) => m,
        _ => {
            *entry = Value::Object(Map::new());
            entry.as_object_mut().unwrap()
        }
    };
    entry_map.insert("hasTrustDialogAccepted".into(), Value::Bool(true));

    let body =
        serde_json::to_string_pretty(&Value::Object(root)).context("serialize ~/.claude.json")?;

    if let Some(parent) = claude_json.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = {
        let mut s = claude_json.as_os_str().to_owned();
        s.push(".ccteam.tmp");
        PathBuf::from(s)
    };
    std::fs::write(&tmp, body.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, claude_json)
        .with_context(|| format!("rename {} → {}", tmp.display(), claude_json.display()))?;
    Ok(())
}

/// V0.4.6 F81 — categorised liveness verdict for a project that
/// `ccteam remove` is about to delete. Each variant maps to a concrete
/// CLAUDE.md §三 "永不主动 kill 长 session" refusal reason; the CLI
/// renders the message verbatim so the user sees how to drain the
/// session before re-running.
///
/// The categorisation is intentionally narrow: each refusal pins one
/// specific resource the user must clean up (tmux session id, claude
/// bg job short id, or the open `agent_spawn` count). `--force`
/// bypasses these checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveSessionRefusal {
    /// A `tmux ls`-visible session named `ccteam-<slug>` is still
    /// running. The user must `tmux kill-session -t <name>` first
    /// (or attach + exit cleanly).
    TmuxSessionAlive { session_name: String },
    /// `~/.claude/jobs/<id>/state.json::cwd == project_dir` and
    /// `state == "working"` — a live `claude --bg` worker. The user
    /// should let it finish (or `claude stop <job_id>` first).
    ClaudeBgJobAlive {
        job_id: String,
        state_json_path: PathBuf,
    },
    /// `progress.jsonl` shows N open `agent_spawn` rows with no
    /// matching terminal `agent_done`. Often this resolves itself on
    /// the next orchestrator tick (F80 cleanup); user should consult
    /// `ccteam show <slug>` to decide.
    OpenAgentSpawns { running_count: u32 },
}

impl ActiveSessionRefusal {
    /// Human-readable reason for the refusal — the CLI prints this
    /// verbatim (one line) so users see the exact tool surface they
    /// need to touch.
    pub fn message(&self, slug: &str) -> String {
        match self {
            Self::TmuxSessionAlive { session_name } => format!(
                "refusing — tmux session `{session_name}` is still alive; \
                 run `tmux kill-session -t {session_name}` first, or pass `--force`"
            ),
            Self::ClaudeBgJobAlive {
                job_id,
                state_json_path,
            } => format!(
                "refusing — claude --bg job `{job_id}` is still working for `{slug}` \
                 (see {}); let it finish or pass `--force`",
                state_json_path.display()
            ),
            Self::OpenAgentSpawns { running_count } => format!(
                "refusing — progress.jsonl shows {running_count} running agent session(s) for \
                 `{slug}`; run `ccteam show {slug}` to inspect or pass `--force`"
            ),
        }
    }
}

/// V0.4.6 F81 — implement CLAUDE.md §三 "永不主动 kill 长 session" for
/// the `ccteam remove <slug>` path. Three checks, returned in priority
/// order so the user sees the most concrete refusal first:
///
/// 1. `ccteam-<slug>` tmux session alive (Codex flex sessions also
///    surface here via `session_name_for_project`).
/// 2. Any `~/.claude/jobs/<id>/state.json` with `cwd == project_dir` /
///    `state == "working"` / no `firstTerminalAt` (live `claude --bg`).
/// 3. `progress.jsonl` carries an open `agent_spawn` whose live job
///    probe still reports `Running` (F80 liveness logic, so phantom
///    rows from prior SIGKILL casualties don't block removal).
///
/// `Ok(None)` → safe to proceed (no active session detected).
/// `Ok(Some(refusal))` → caller refuses unless `--force` is set.
/// `Err(_)` → IO failure on the registry / progress file (callers
/// should treat as fatal — silently degrading would risk deleting a
/// project mid-session).
///
/// **Red line.** This function never modifies state. The caller is
/// the only mutator; this helper is a pure read on filesystem + env.
pub fn refuses_active_session(
    paths: &CcteamPaths,
    slug: &str,
) -> Result<Option<ActiveSessionRefusal>> {
    // 1. tmux session check — uses `session_name_for_project` so
    // meta-agent layouts (`ccteam-meta-<handle>`) are detected too.
    let tmux_name = crate::tmux::session_name_for_project(paths, slug);
    let tmux = crate::tmux::TmuxSession::from_name(tmux_name.clone());
    if tmux.exists() {
        return Ok(Some(ActiveSessionRefusal::TmuxSessionAlive {
            session_name: tmux_name,
        }));
    }

    // 2. claude --bg job check — scan `~/.claude/jobs/*/state.json`
    // (or the `CCTEAM_CLAUDE_JOBS_DIR` redirection). Match on
    // canonical cwd; treat unparseable / missing entries as "no
    // match" rather than IO-erroring (a corrupt state.json elsewhere
    // shouldn't block removal of this project).
    let project_dir = paths.project_dir(slug);
    let canon_project = std::fs::canonicalize(&project_dir).unwrap_or(project_dir.clone());
    let jobs_dir = std::env::var_os(crate::harness::CLAUDE_JOBS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        });
    if let Ok(read) = std::fs::read_dir(&jobs_dir) {
        for entry in read.flatten() {
            let state_path = entry.path().join("state.json");
            let Ok(body) = std::fs::read_to_string(&state_path) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&body) else {
                continue;
            };
            let cwd = v.get("cwd").and_then(|s| s.as_str()).unwrap_or("");
            if cwd.is_empty() {
                continue;
            }
            let canon = std::fs::canonicalize(cwd).unwrap_or(PathBuf::from(cwd));
            if canon != canon_project {
                continue;
            }
            // Liveness: terminal job → ignore (firstTerminalAt set or
            // state in terminal set). Use the same classifier the
            // orchestrator + watcher consume so this stays consistent.
            if matches!(
                crate::claude_job::classify(&v),
                crate::claude_job::JobLiveness::Running
            ) {
                let job_id = entry.file_name().to_string_lossy().to_string();
                return Ok(Some(ActiveSessionRefusal::ClaudeBgJobAlive {
                    job_id,
                    state_json_path: state_path,
                }));
            }
        }
    }

    // 3. progress.jsonl open `agent_spawn` check — F80 liveness logic
    // so phantom rows (SIGKILL casualties) don't block. We use
    // `current_agent_sessions_with_liveness` which the web/orchestrator
    // already consume; this stays consistent with `workflow_summary`
    // counts the user sees in `ccteam show`.
    let progress_path = paths.progress_jsonl(slug);
    if progress_path.exists() {
        let events = crate::progress::read_all_events(&progress_path).unwrap_or_default();
        let sessions = crate::progress::current_agent_sessions_with_liveness(&events, |job_id| {
            crate::claude_job::probe_job(job_id)
        });
        let running = sessions
            .iter()
            .filter(|s| matches!(s.status, crate::progress::AgentSessionStatus::Running))
            .count() as u32;
        if running > 0 {
            return Ok(Some(ActiveSessionRefusal::OpenAgentSpawns {
                running_count: running,
            }));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    static DISABLE_TOOL_SURFACE: OnceLock<()> = OnceLock::new();
    fn ensure_isolation() {
        DISABLE_TOOL_SURFACE
            .get_or_init(crate::tool_surface::disable_tool_surface_bootstrap_for_tests);
    }

    #[test]
    fn slugify_keeps_alphanumeric_lowercase() {
        assert_eq!(slugify("Hello World 123"), "hello-world-123");
        assert_eq!(slugify("Bookmark Manager (PWA)"), "bookmark-manager-pwa");
        assert_eq!(slugify("--leading-and-trailing--"), "leading-and-trailing");
        assert_eq!(slugify("multiple   spaces"), "multiple-spaces");
        assert_eq!(slugify("CamelCaseName"), "camelcasename");
    }

    #[test]
    fn slugify_falls_back_to_project_for_empty_input() {
        assert_eq!(slugify(""), "project");
        assert_eq!(slugify("中文 only"), "only");
    }

    #[test]
    fn slugify_truncates_to_40_chars() {
        let long = "a".repeat(80);
        assert!(slugify(&long).len() <= 40);
    }

    #[test]
    fn slugify_rolls_back_to_dash_boundary_when_cut_would_split_word() {
        // Without rollback this would yield `build-a-tiny-python-cli-that-converts-cs`
        // (40 chars) — a half token. With rollback we drop `cs` and keep the slug
        // ending on the prior `-`.
        let s = slugify("Build a tiny Python CLI that converts CSV to JSON");
        assert!(
            s.len() <= 40,
            "slug must respect 40-char cap, got len={}: {s}",
            s.len()
        );
        assert!(
            !s.ends_with("-cs"),
            "slug should roll back past the half-token, got: {s}",
        );
        assert!(
            s.ends_with("converts"),
            "expected slug to end at the dash boundary `converts`, got: {s}",
        );
    }

    #[test]
    fn slugify_keeps_single_long_token_truncated() {
        // No `-` to fall back to, so a single megaword keeps the hard cap.
        let s = slugify(&"a".repeat(80));
        assert_eq!(s.len(), 40);
        assert!(s.chars().all(|c| c == 'a'));
    }

    fn pick_paths(tmp: &tempfile::TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("ccteam-home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn pick_unused_slug_prefixes_team_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let dev = pick_unused_slug(&paths, "make a todo cli", "dev").unwrap();
        let pr = pick_unused_slug(&paths, "AI recipe generator", "product-research").unwrap();
        // V0.2.2 F34: `slugify_brief` drops stop-words (`a`) so the
        // brief-derived base is `make-todo-cli`, not `make-a-todo-cli`.
        assert_eq!(dev, "dev-make-todo-cli");
        assert_eq!(pr, "product-research-ai-recipe-generator");
    }

    #[test]
    fn pick_unused_slug_appends_suffix_on_collision_under_team_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        // Pre-create the bare prefixed slug directory so the next pick
        // must fall back to a `<team>-<base>-<suffix>` form.
        std::fs::create_dir_all(paths.project_dir("dev-todo-cli")).unwrap();
        let s = pick_unused_slug(&paths, "todo cli", "dev").unwrap();
        assert!(
            s.starts_with("dev-todo-cli-"),
            "expected suffix retry, got {s}"
        );
        assert_ne!(s, "dev-todo-cli");
    }

    #[test]
    fn pick_unused_slug_keeps_team_prefix_distinct_per_team() {
        // Same brief under different teams must produce distinct slugs.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let dev = pick_unused_slug(&paths, "shared brief", "dev").unwrap();
        let pr = pick_unused_slug(&paths, "shared brief", "product-research").unwrap();
        assert_eq!(dev, "dev-shared-brief");
        assert_eq!(pr, "product-research-shared-brief");
        assert_ne!(dev, pr);
    }

    // --- V0.2.2 F34 — slugify_brief() Tier 4 deterministic ---

    #[test]
    fn slugify_brief_drops_pure_digit_tokens_and_keeps_mixed() {
        // PRD §3.2.4 case 1:
        // `ccteam ui — V1.2 session subagent 3` → tokens
        // [ccteam, ui, v1, 2, session, subagent, 3] →
        // drop `2`/`3` (pure digit), keep `v1` (letter+digit) →
        // first 3 → `ccteam-ui-v1`.
        assert_eq!(
            slugify_brief("ccteam ui — V1.2 session subagent 3"),
            "ccteam-ui-v1"
        );
    }

    #[test]
    fn slugify_brief_drops_stop_words() {
        // `Build a tiny Python CLI that converts CSV to JSON` →
        // tokens drop `a`/`that`/`to` → first 3 = build, tiny, python.
        assert_eq!(
            slugify_brief("Build a tiny Python CLI that converts CSV to JSON"),
            "build-tiny-python"
        );
    }

    #[test]
    fn slugify_brief_keeps_brand_and_caps_at_three_tokens() {
        assert_eq!(
            slugify_brief("AI recipe generator from fridge photo"),
            "ai-recipe-generator"
        );
        assert_eq!(
            slugify_brief("HermesTrade DEX home"),
            "hermestrade-dex-home"
        );
    }

    #[test]
    fn slugify_brief_handles_short_briefs_unchanged() {
        // Three real tokens, nothing to filter.
        assert_eq!(slugify_brief("Predict market + DEX"), "predict-market-dex");
    }

    #[test]
    fn slugify_brief_falls_back_when_all_filtered() {
        // Only stop-words → fall back to raw `slugify` so the caller
        // never gets `project` from a degenerate filter pass.
        assert_eq!(slugify_brief("to of and"), "to-of-and");
    }

    #[test]
    fn slugify_brief_dedups_consecutive_repeats() {
        // `ccteam ccteam ui` → token list `[ccteam, ccteam, ui]` →
        // dedup last → `[ccteam, ui]` → joined = `ccteam-ui`.
        assert_eq!(slugify_brief("ccteam ccteam ui"), "ccteam-ui");
    }

    #[test]
    fn slugify_brief_drops_stop_word_do() {
        // `do the thing` → drop `the` (stop) → `[do, thing]`.
        // `do` is len 2 + not in stop list → kept.
        assert_eq!(slugify_brief("do the thing"), "do-thing");
    }

    #[test]
    fn slugify_brief_falls_back_to_project_for_empty_input() {
        assert_eq!(slugify_brief(""), "project");
        assert_eq!(slugify_brief("中文"), "project");
    }

    // --- V0.2.2 F34 — pick_unused_slug_verbatim (--slug flag path) ---

    #[test]
    fn verbatim_prefixes_team_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let s = pick_unused_slug_verbatim(&paths, "ccteam-ui", "dev").unwrap();
        assert_eq!(s, "dev-ccteam-ui");
    }

    #[test]
    fn verbatim_keeps_team_prefix_when_already_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let s = pick_unused_slug_verbatim(&paths, "dev-ccteam-ui", "dev").unwrap();
        assert_eq!(s, "dev-ccteam-ui");
    }

    #[test]
    fn verbatim_does_not_match_partial_prefix() {
        // `dev` ≠ `product-research`, so `--slug product-research-foo
        // --team dev` must prepend `dev-` even though the slug starts
        // with the substring `product`. (PRD §3.2.1 row 4.)
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let s = pick_unused_slug_verbatim(&paths, "product-research-foo", "dev").unwrap();
        assert_eq!(s, "dev-product-research-foo");
    }

    #[test]
    fn verbatim_rejects_illegal_chars() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let err = pick_unused_slug_verbatim(&paths, "Bad Name!", "dev").unwrap_err();
        assert!(
            err.to_string().contains("[a-z0-9-]+"),
            "expected fail-loud regex hint, got {err}",
        );
    }

    #[test]
    fn verbatim_rejects_empty_and_dash_edges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        assert!(pick_unused_slug_verbatim(&paths, "", "dev").is_err());
        assert!(pick_unused_slug_verbatim(&paths, "   ", "dev").is_err());
        assert!(pick_unused_slug_verbatim(&paths, "-leading", "dev").is_err());
        assert!(pick_unused_slug_verbatim(&paths, "trailing-", "dev").is_err());
    }

    #[test]
    fn verbatim_rejects_too_long() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        let long = "a".repeat(61);
        let err = pick_unused_slug_verbatim(&paths, &long, "dev").unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn verbatim_collision_retries_with_suffix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = pick_paths(&tmp);
        std::fs::create_dir_all(paths.project_dir("dev-x")).unwrap();
        let s = pick_unused_slug_verbatim(&paths, "x", "dev").unwrap();
        assert!(s.starts_with("dev-x-"), "expected suffix retry, got {s}");
        assert_ne!(s, "dev-x");
    }

    #[test]
    fn write_trust_entry_creates_file_with_project_marked_trusted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        let project = tmp.path().join("projects/abc");
        std::fs::create_dir_all(&project).unwrap();

        write_trust_entry(&claude_json, &project).unwrap();
        let body = std::fs::read_to_string(&claude_json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let key = project.to_str().unwrap();
        assert_eq!(
            v["projects"][key]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "expected projects[{key}].hasTrustDialogAccepted=true; got {body}",
        );
    }

    #[test]
    fn write_trust_entry_preserves_existing_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        // Pre-existing config with another project + unrelated top-level keys.
        std::fs::write(
            &claude_json,
            r#"{
              "userID": "rob",
              "projects": {
                "/some/other/project": {"hasTrustDialogAccepted": true, "extra": 7}
              }
            }"#,
        )
        .unwrap();

        let project = tmp.path().join("projects/new");
        std::fs::create_dir_all(&project).unwrap();
        write_trust_entry(&claude_json, &project).unwrap();

        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["userID"], "rob");
        assert_eq!(
            v["projects"]["/some/other/project"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
        assert_eq!(v["projects"]["/some/other/project"]["extra"], 7);
        let key = project.to_str().unwrap();
        assert_eq!(
            v["projects"][key]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
    }

    // ---- resolution logic: pure-function tests, no env mutation ----

    #[test]
    fn resolve_claude_json_path_honors_claude_config_home() {
        // CLAUDE_CONFIG_HOME points at the .claude/ dir; .claude.json is
        // its sibling. Resolution must redirect there (mirroring
        // user_claude_dir's CLAUDE_CONFIG_HOME handling).
        let resolved = resolve_claude_json_path_from_env(
            Some("/some/test/.claude".to_string()),
            Some(PathBuf::from("/should/not/be/used")),
        )
        .unwrap();
        assert_eq!(resolved, std::path::Path::new("/some/test/.claude.json"));
    }

    #[test]
    fn resolve_claude_json_path_handles_claude_config_home_at_root() {
        // Defensive: CLAUDE_CONFIG_HOME without a parent (e.g. exactly
        // "/") falls back to writing inside the dir rather than silently
        // resolving to "/.claude.json" (which would still be on the
        // wrong filesystem from the user's perspective).
        let resolved = resolve_claude_json_path_from_env(
            Some("/".to_string()),
            Some(PathBuf::from("/home/rob")),
        )
        .unwrap();
        // "/" has parent = None per std::path semantics, so we expect
        // the inner-dir join.
        assert_eq!(resolved, std::path::Path::new("/.claude.json"));
    }

    #[test]
    fn resolve_claude_json_path_falls_back_to_home_when_env_unset() {
        let resolved =
            resolve_claude_json_path_from_env(None, Some(PathBuf::from("/home/rob"))).unwrap();
        assert_eq!(resolved, std::path::Path::new("/home/rob/.claude.json"));
    }

    #[test]
    fn resolve_claude_json_path_errors_when_neither_available() {
        let err = resolve_claude_json_path_from_env(None, None).unwrap_err();
        assert!(format!("{err:#}").contains("home directory"));
    }

    // ---- side-effect guards ----
    //
    // The full "bootstrap_project doesn't write to real ~/.claude.json"
    // assertion lives in `crates/ccteam-core/tests/tool_surface_e2e_test.rs`
    // where CLAUDE_CONFIG_HOME redirection runs in its own test binary
    // process — there it's safe to mutate the env var because no
    // other test in the same binary reads it concurrently.
    //
    // Inline tests below verify the *guard logic* without mutating any
    // process-wide env var, since `bootstrap_project_*` siblings in
    // this same binary read CLAUDE_CONFIG_HOME via bootstrap_project →
    // pre_trust_project → resolve_claude_json_path; an env_lock here
    // would protect our tests from each other but not from those
    // siblings, which is precisely the race condition that broke an
    // earlier draft of these tests.

    #[test]
    fn disable_flag_recognized_when_ensure_isolation_ran() {
        // Regression hook for the 2026-05-06 ~/.claude.json bloat: the
        // disable flag was wired only into setup_tool_surface, not
        // pre_trust_project. The unit-level guarantee we want is that
        // ensure_isolation() — which all bootstrap_project-touching
        // tests call — surfaces a `true` from the same env-var check
        // pre_trust_project uses.
        ensure_isolation();
        let v = std::env::var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP").ok();
        assert!(
            matches!(v.as_deref(), Some("1") | Some("true") | Some("yes")),
            "ensure_isolation must set CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP \
             to a truthy value the disable check recognizes; got {v:?}",
        );
    }

    #[test]
    fn bootstrap_project_creates_templates_dir() {
        // M2.4 / V0.5.0 F101: bootstrap_project ensures
        // ~/.ccteam/templates/ exists so phase markdown's
        // `@~/.ccteam/templates/<name>` reference resolves even when
        // the user skipped `ccteam init`. F101 deleted the V0.2-era
        // helper template payloads (`review-with-user-loop.md` /
        // `kickoff-reverse-interview.md`), leaving an empty
        // HELPER_TEMPLATES — but the directory itself still gets
        // created so future helpers can land alongside per-user files.
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let templates = paths.root.join("templates");
        assert!(
            templates.is_dir(),
            "~/.ccteam/templates/ should exist after bootstrap; got {}",
            templates.display(),
        );
    }

    // ---------------- V0.2 M0.16.3: claude_md_template ----------------

    #[test]
    fn render_project_claude_md_falls_back_to_generic_when_no_resolved_spec() {
        // V0.4.0 F60: the shipped TEAM_BUNDLES fallback was deleted with
        // the phase machinery. With no resolved_spec, every team gets
        // the generic body — substitution still applies.
        let body = render_project_claude_md("dev-build-todo", "dev", None);
        assert!(body.contains("# CLAUDE.md (auto-managed by ccteam)"));
        assert!(body.contains("- slug: dev-build-todo"));
        assert!(body.contains("- team: dev"));
        assert!(body.contains("workflow.yaml"));
    }

    #[test]
    fn render_project_claude_md_falls_back_to_generic_for_unknown_team() {
        let body = render_project_claude_md("custom-foo-1", "custom-team", None);
        assert!(body.contains("- slug: custom-foo-1"));
        assert!(body.contains("- team: custom-team"));
        assert!(body.contains("workflow.yaml"));
    }

    #[test]
    fn render_project_claude_md_prefers_resolved_team_template() {
        let spec = TeamSpec::parse(
            "name: custom-flex\n\
             kind: flex\n\
             claude_md_template: \"custom flex template for {slug} / {team}\"\n",
        )
        .unwrap();
        let body = render_project_claude_md("custom-flex-demo", "custom-flex", Some(&spec));
        assert!(body.contains("custom flex template for custom-flex-demo / custom-flex"));
    }

    #[test]
    fn render_project_claude_md_falls_back_to_generic_when_template_field_empty() {
        // meta-agent ships with an empty `claude_md_template` (the role
        // prompt overwrites the file via a different path), so the
        // generic body is the right fallback. This guards against
        // accidentally writing "" as the body.
        let body = render_project_claude_md("meta-rob", "meta-agent", None);
        assert!(!body.is_empty());
        assert!(body.contains("- slug: meta-rob"));
        assert!(body.contains("- team: meta-agent"));
    }

    #[test]
    fn bootstrap_project_settings_uses_absolute_ccteam_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let settings = paths.project_dir("demo").join(".claude/settings.json");
        let body = std::fs::read_to_string(&settings).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            cmd.starts_with('/'),
            "settings.json hook command must be an absolute path, got: {cmd}",
        );
        // V0.4.6 F89: fresh settings.json renders use the new
        // `internal hook …` path. Old `hook …` form would still resolve
        // via the deprecated top-level alias, but new installs should
        // produce the new form.
        assert!(
            cmd.ends_with(" internal hook load-context"),
            "settings.json hook command should invoke `internal hook load-context`, got: {cmd}",
        );
        // Reject the placeholder having survived the render — that's the
        // PATH-dependent failure mode we're guarding against.
        assert!(
            !cmd.contains("{{CCT_BIN}}"),
            "settings.json placeholder should be substituted, got: {cmd}",
        );
        assert!(
            !cmd.contains("__CCTEAM_BIN__"),
            "legacy F39 placeholder should not return, got: {cmd}",
        );
    }
}
