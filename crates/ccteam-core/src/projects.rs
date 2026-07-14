//! Project bootstrap helpers used by `ccteam project new` (and reusable by the
//! M3+ inbox triage path). Pure: no tmux side effects, just file
//! creation under `~/projects/<slug>/`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::templates::{write_project_settings, EnabledPluginsSetting};

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
/// Tries `<team>-<base>` first, then appends an incrementing integer
/// (`<team>-<base>2`, `<team>-<base>3`, …) on collision.
///
/// V0.2.2 F34: the `base` argument is a free-text request — it gets
/// run through `slugify_brief` (token-aware) so `<team>-<base>` stays
/// readable. Callers that already have a deliberate slug (eg
/// `--slug ccteam-ui`) should use [`pick_unused_slug_verbatim`] which
/// skips token filtering and only enforces team prefix + collision
/// retry.
pub fn pick_unused_slug(paths: &CcteamPaths, base: &str, team: &str) -> Result<String> {
    let base = slugify_brief(base);
    pick_unused_under_team_prefix(paths, &base, team)
}

/// V0.4.2 F75 reviewer fix: validate that `slug` matches the on-disk
/// slug grammar — `[a-z0-9][a-z0-9-]*`, length ≤ 60, no leading /
/// trailing dash. Returns the trimmed string on success. Use this
/// from CLI parsers (`ccteam init --slug`, `ccteam project new <slug>`) so
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
/// - Collision retry via an incrementing numeric suffix (same as
///   `pick_unused_slug`): `base`, `base2`, `base3`, …
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
/// or a numerically-accumulated retry on collision. Shared between the
/// verbatim (`--slug`) and brief-derived paths.
fn pick_unused_under_team_prefix(paths: &CcteamPaths, base: &str, team: &str) -> Result<String> {
    let prefixed = format!("{team}-{base}");
    pick_unused_with_prefixed(paths, &prefixed)
}

/// Return `prefixed` if its directory is free, otherwise append an
/// incrementing integer until an unused slug is found: `prefixed`,
/// `prefixed2`, `prefixed3`, … (D2.6 — replaces the old random `-{4hex}`
/// suffix so repeated `demo` creates land on the readable `demo2` /
/// `demo3` rather than `demo-3f9a`).
fn pick_unused_with_prefixed(paths: &CcteamPaths, prefixed: &str) -> Result<String> {
    if !paths.project_dir(prefixed).exists() {
        return Ok(prefixed.to_string());
    }
    // Start at 2 so the first collision yields `<base>2` (human-friendly:
    // the original is the implicit "1").
    for n in 2.. {
        let candidate = format!("{prefixed}{n}");
        if !paths.project_dir(&candidate).exists() {
            return Ok(candidate);
        }
    }
    unreachable!("integer accumulation always finds a free slug")
}

/// Write the bootstrap files for a fresh project:
/// - `<project>/.ccteam/state.json` ← `ProjectState::initial_for_team(slug, team)`
/// - `<project>/.claude/settings.local.json` ← managed hooks + base
/// - `<project>/.claude/agents/cto.md` ← default IM/chat persona
///
/// v0.8.6: ccteam no longer generates a project `CLAUDE.md` /
/// `AGENTS.md` (project knowledge is vendor-native, owned by the
/// project) nor a `.ccteam/spec.md`; `.ccteam/` keeps only `state.json`
/// (+ `workflow.yaml`, written elsewhere). `request` is retained for
/// API stability but no longer persisted.
///
/// `team` lands in state.json so downstream routing can pick the
/// matching workflow.
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

/// Materialize the daemon-side state home for a remote project without
/// treating it as a vendor working tree. This intentionally writes only
/// `.ccteam/state.json`: no Claude settings, hooks, trust entry, workflow,
/// or project knowledge scaffold belongs in a data directory.
pub fn ensure_project_data_home(
    data_home: &Path,
    slug: &str,
    owner: Option<String>,
) -> Result<PathBuf> {
    let ccteam_dir = data_home.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;
    let state_path = CcteamPaths::project_state_in(data_home);
    let mut state = if state_path.exists() {
        ProjectState::load(&state_path)?
    } else {
        ProjectState::initial(slug.to_string())
    };
    state.slug = slug.to_string();
    state.owner = owner;
    state.save(&state_path)?;
    Ok(data_home.to_path_buf())
}

/// V0.4.2 F72: `bootstrap_project` generalized to install at an
/// arbitrary `target_dir`. Used by `ccteam init` to bring an existing
/// repo under management without moving files. Side effects (trust-marking)
/// still use `paths.root` / `paths.projects_root`, since those are global
/// concerns.
///
/// `target_dir` is created if it doesn't exist. Existing files inside
/// it are left alone — this is the **fresh-install** path. Idempotent
/// refresh of an already-installed project lives in `ccteam init`'s
/// refresh helpers; this function assumes the caller already decided
/// "this is a new install".
/// v0.8.9 (owner decision) — scaffold body for an EMPTY project's `AGENTS.md`.
/// It tells a bare / roleless agent the project is uninitialized so it asks the
/// user to set it up instead of spinning on an under-specified task. Written
/// ONLY when the project has neither `CLAUDE.md` nor `AGENTS.md` (see
/// [`bootstrap_project_at_dir`]); never overwrites existing content.
const EMPTY_PROJECT_AGENTS_MD: &str = "\
# Project guide

> ⚠️ This project has not been initialized yet — ccteam created this file as a
> placeholder.

**Before doing any substantial work here, remind the user to initialize the
project.** Ask them to describe, in this file:

- what this project is and what it is for;
- its layout and the key directories / files;
- the conventions, constraints, and tooling to follow;
- how agents should operate here — what to do, and what to avoid.

Until this file is filled in, keep replies short, ask clarifying questions, and
do **not** guess at the task or start open-ended exploration.
";

pub fn bootstrap_project_at_dir(
    // `paths` is the global `~/.ccteam/` root. It feeds the
    // home-ensure below (`ensure_ccteam_home`) so the `hook.sh`
    // dispatcher the project settings reference actually exists. (It no
    // longer feeds the dead helper-template stamp removed in v0.8.6
    // review-fix #3.)
    paths: &CcteamPaths,
    target_dir: &Path,
    slug: &str,
    // v0.8.6: `request` is no longer persisted (no `.ccteam/spec.md`,
    // no generated `CLAUDE.md`); kept in the signature for API
    // stability so existing callers don't churn.
    _request: &str,
    team: &str,
) -> Result<PathBuf> {
    // Materialize the global `~/.ccteam/` home (canonical dirs +
    // `hooks/hook.sh` dispatcher) BEFORE writing the project settings:
    // `write_project_settings` lays down a `.claude/settings.local.json`
    // whose SessionStart hook command points at
    // `<paths.root>/hooks/hook.sh`. Historically only `ccteam init` /
    // `chat_register` / `doctor --install-hooks` ran `install_hooks`, so
    // the web `POST /projects` + IM/web chat create path that flows
    // through here referenced a hook.sh that was never written →
    // "hook.sh: not found" at first launch. Idempotent (cheap no-op when
    // the home is already complete).
    crate::ensure_ccteam_home(paths).context("ensure ~/.ccteam/ home before project settings")?;

    let project_dir = target_dir.to_path_buf();
    let ccteam_dir = project_dir.join(".ccteam");
    std::fs::create_dir_all(&ccteam_dir)
        .with_context(|| format!("create {}", ccteam_dir.display()))?;

    let state = ProjectState::initial_for_team(slug.to_string(), team.to_string());
    state.save(&CcteamPaths::project_state_in(&project_dir))?;

    // V0.4.0 F60: the phase-template-driven `enabledPlugins` resolver
    // (`compute_enabled_plugins` + `load_phase_templates_for_bootstrap`)
    // was deleted with the rest of the phase machinery. F66 will
    // reintroduce the plugin enablement set computed from
    // `workflow.yaml::agents.<role>.executor`. For now we lay down
    // managed settings (`.claude/settings.local.json`) with an empty
    // plugin set so the project bootstraps cleanly; user-authored agent
    // prompts still resolve their plugin surface via the global
    // `~/.claude/agents/` layer.
    let enabled_plugins = EnabledPluginsSetting::default();

    write_project_settings(&project_dir, &enabled_plugins)?;
    // v0.8.6 (review-fix #3): no longer stamp `~/.ccteam/templates/`.
    // `HELPER_TEMPLATES` has been empty since V0.5.0 F101, so the writer
    // only ever created an empty `templates/` dir that is *not* in
    // `canonical_home_dirs()` — making a fresh `ccteam init` report
    // self-inflicted home-layout drift. init now creates exactly the
    // canonical set; the orphaned helper-template path is gone.
    if let Err(err) = pre_trust_project(&project_dir) {
        // Failing to pre-trust is annoying (next launch shows the
        // "Trust this folder?" prompt) but not fatal — log + continue.
        tracing::warn!(
            project_dir = %project_dir.display(),
            error = %err,
            "could not pre-trust project in ~/.claude.json; first claude launch may show trust prompt",
        );
    }

    // v0.9.0 W2 (F6.1) — engine neutralization: ccteam seeds NO role. A fresh
    // project's `.claude/agents/` is left untouched (not even created); the
    // default session is roleless (bare vendor reads the project CLAUDE.md /
    // AGENTS.md). Orchestration personas live 100% in user space / the hub.

    // v0.8.9 (owner decision) — scaffold a minimal project brain for an EMPTY
    // project so a roleless / bare-claude session asks the user to initialize
    // it instead of spinning on an under-specified task. ONLY when the project
    // has NEITHER CLAUDE.md NOR AGENTS.md (either present ⇒ a real project —
    // never touch it). This deliberately relaxes the earlier "ccteam never
    // generates the project knowledge layer" red-line, scoped to the
    // empty-project bootstrap and never overwriting existing content.
    let claude_md = project_dir.join("CLAUDE.md");
    let agents_md = project_dir.join("AGENTS.md");
    if !claude_md.exists() && !agents_md.exists() {
        std::fs::write(&agents_md, EMPTY_PROJECT_AGENTS_MD)
            .with_context(|| format!("write {}", agents_md.display()))?;
        // CLAUDE.md reuses AGENTS.md as the single source of truth via Claude
        // Code's `@import` (Codex reads AGENTS.md directly).
        std::fs::write(&claude_md, "@AGENTS.md\n")
            .with_context(|| format!("write {}", claude_md.display()))?;
    }

    // v0.8.9 (owner request) — keep `.ccteam/` out of git: add a `.ccteam/`
    // line to the project's .gitignore (create it if absent; append only when
    // no equivalent ignore is already present). Best-effort — a .gitignore
    // write failure must never fail project creation.
    {
        let gitignore = project_dir.join(".gitignore");
        let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
        let already = existing
            .lines()
            .any(|l| matches!(l.trim().trim_end_matches('/'), ".ccteam" | "/.ccteam"));
        if !already {
            let mut body = existing;
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(".ccteam/\n");
            if let Err(err) = std::fs::write(&gitignore, &body) {
                tracing::warn!(
                    project_dir = %project_dir.display(),
                    error = %err,
                    "could not add .ccteam/ to .gitignore",
                );
            }
        }
    }

    Ok(project_dir)
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
/// v0.8.24 Q7(默认:仅展示)— best-effort current git branch of a project
/// working tree. `None` when the dir is not a git repo (or unreadable) —
/// the UI then hides the branch dimension. Handles both a `.git` DIRECTORY
/// and a worktree `.git` FILE (`gitdir: <path>`, one hop). A detached HEAD
/// reads as the short 12-hex commit id (still honest/renderable). Read-only:
/// never shells out to `git`, never mutates.
pub fn read_current_branch(project_dir: &Path) -> Option<String> {
    let dot_git = project_dir.join(".git");
    let head_path = if dot_git.is_dir() {
        dot_git.join("HEAD")
    } else if dot_git.is_file() {
        let raw = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir = raw.trim().strip_prefix("gitdir:")?.trim();
        let base = Path::new(gitdir);
        let resolved = if base.is_absolute() {
            base.to_path_buf()
        } else {
            project_dir.join(base)
        };
        resolved.join("HEAD")
    } else {
        return None;
    };
    let head = std::fs::read_to_string(head_path).ok()?;
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref:") {
        let r = r.trim();
        return Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string())
            .filter(|s| !s.is_empty());
    }
    if head.len() >= 12 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(head[..12].to_string());
    }
    None
}

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
    let jobs_dir = crate::claude_jobs_dir_from_env();
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
        // D2.6: a collision accumulates an incrementing integer, so the
        // first retry is `<team>-<base>2`, the next `<team>-<base>3`, …
        std::fs::create_dir_all(paths.project_dir("dev-todo-cli")).unwrap();
        let s = pick_unused_slug(&paths, "todo cli", "dev").unwrap();
        assert_eq!(s, "dev-todo-cli2");
        // Pre-create `dev-todo-cli2` too; next pick must roll to `3`.
        std::fs::create_dir_all(paths.project_dir("dev-todo-cli2")).unwrap();
        let s3 = pick_unused_slug(&paths, "todo cli", "dev").unwrap();
        assert_eq!(s3, "dev-todo-cli3");
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
        // D2.6: numeric accumulation — first collision yields `dev-x2`.
        let s = pick_unused_slug_verbatim(&paths, "x", "dev").unwrap();
        assert_eq!(s, "dev-x2");
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
    fn bootstrap_project_does_not_create_templates_dir() {
        // v0.8.6 (review-fix #3): `HELPER_TEMPLATES` has been empty since
        // V0.5.0 F101, so the old `write_global_helper_templates` call only
        // ever produced an empty `~/.ccteam/templates/` dir that is *not*
        // in `canonical_home_dirs()`. That made a fresh `ccteam init`
        // report self-inflicted home-layout drift. The call is gone;
        // bootstrap must no longer create `templates/`.
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let templates = paths.root.join("templates");
        assert!(
            !templates.exists(),
            "~/.ccteam/templates/ must NOT be created by bootstrap (review-fix #3); got {}",
            templates.display(),
        );
    }

    #[test]
    fn bootstrap_project_settings_routes_through_hook_sh() {
        // V0.6.1 F139: fresh managed settings render route hook commands
        // through `~/.ccteam/hooks/hook.sh` (the daemon-aware wrapper)
        // instead of cold-spawning `ccteam internal hook ...`.
        //
        // v0.8.6: managed settings land in `.claude/settings.local.json`
        // (NOT the user-committed `settings.json`) so ccteam never
        // dirties the project's checked-in settings.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // Point CCTEAM_HOME at the tempdir so `effective_hook_sh_path`
        // returns the predictable per-test path.
        std::env::set_var("CCTEAM_HOME", &paths.root);
        let expected_hook = paths.hooks_script();
        let result = bootstrap_project(&paths, "demo", "demo request", "dev");
        std::env::remove_var("CCTEAM_HOME");
        result.unwrap();

        let settings = paths
            .project_dir("demo")
            .join(".claude/settings.local.json");
        let body = std::fs::read_to_string(&settings).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            cmd.starts_with('/'),
            "settings.local.json hook command must be an absolute path, got: {cmd}",
        );
        assert!(
            cmd.contains(expected_hook.to_str().unwrap()),
            "settings.local.json hook should invoke {}, got: {cmd}",
            expected_hook.display(),
        );
        assert!(
            cmd.ends_with(" load-context"),
            "settings.local.json SessionStart[0] should pass `load-context`, got: {cmd}",
        );
        assert!(
            !cmd.contains("__CCTEAM_HOOK_SH__"),
            "placeholder should be substituted, got: {cmd}",
        );
        assert!(
            !cmd.contains("__CCTEAM_BIN__"),
            "retired F89 placeholder should not return, got: {cmd}",
        );
    }

    /// Root-cause regression: a project created via the web / IM /
    /// daemon path (which all flow through `bootstrap_project_at_dir`)
    /// writes a `.claude/settings.local.json` SessionStart hook that
    /// points at `<paths.root>/hooks/hook.sh` — but that dispatcher was
    /// never materialized on this path, yielding "hook.sh: not found" at
    /// first launch. `bootstrap_project_at_dir` now calls
    /// `ensure_ccteam_home` up front, so hook.sh must land + be exec.
    #[test]
    fn bootstrap_project_at_dir_materializes_hook_sh() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let project_dir = tmp.path().join("workspace").join("my-repo");

        bootstrap_project_at_dir(&paths, &project_dir, "demo", "(from web/IM chat)", "dev")
            .unwrap();

        let hook_sh = paths.hooks_script();
        assert!(
            hook_sh.exists(),
            "bootstrap_project_at_dir must materialize {} (the dispatcher the project settings reference)",
            hook_sh.display(),
        );
        let body = std::fs::read_to_string(&hook_sh).unwrap();
        assert_eq!(
            body,
            crate::HOOK_DISPATCHER_SH,
            "materialized hook.sh must be the embedded F139 dispatcher body",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&hook_sh).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o755,
                "hook.sh must be chmod 0755 so Claude Code can exec it"
            );
        }
        // Canonical home dirs are present too (the second half of the
        // home-ensure contract).
        for sub in crate::canonical_home_dirs() {
            assert!(
                paths.root.join(sub).is_dir(),
                "canonical home dir {sub:?} must exist under {}",
                paths.root.display(),
            );
        }
    }

    /// `ensure_ccteam_home` is the shared idempotent home-ensure: a
    /// re-run must succeed and leave hook.sh in place (it underpins the
    /// daemon-start + every create path, all of which may run repeatedly).
    #[test]
    fn bootstrap_empty_project_scaffolds_claude_and_agents_md() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let project_dir = tmp.path().join("workspace").join("empty-repo");

        bootstrap_project_at_dir(&paths, &project_dir, "demo", "(from web/IM chat)", "dev")
            .unwrap();

        let agents_md = project_dir.join("AGENTS.md");
        let claude_md = project_dir.join("CLAUDE.md");
        assert!(
            agents_md.exists(),
            "an empty project must get a scaffolded AGENTS.md"
        );
        assert!(
            claude_md.exists(),
            "an empty project must get a scaffolded CLAUDE.md"
        );
        assert_eq!(
            std::fs::read_to_string(&claude_md).unwrap(),
            "@AGENTS.md\n",
            "CLAUDE.md must @-import AGENTS.md (single source of truth)"
        );
        assert!(
            std::fs::read_to_string(&agents_md)
                .unwrap()
                .contains("has not been initialized"),
            "scaffolded AGENTS.md must prompt the user to initialize the project"
        );
    }

    #[test]
    fn bootstrap_nonempty_project_keeps_existing_knowledge_layer() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // A project that ALREADY has a CLAUDE.md is non-empty → bootstrap must
        // neither overwrite it nor create an AGENTS.md scaffold.
        let project_dir = tmp.path().join("workspace").join("real-repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("CLAUDE.md"),
            "# Real project\nuser content\n",
        )
        .unwrap();

        bootstrap_project_at_dir(&paths, &project_dir, "demo", "(from web/IM chat)", "dev")
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(project_dir.join("CLAUDE.md")).unwrap(),
            "# Real project\nuser content\n",
            "bootstrap must NOT overwrite an existing CLAUDE.md"
        );
        assert!(
            !project_dir.join("AGENTS.md").exists(),
            "a project that already has CLAUDE.md is non-empty → no AGENTS.md scaffold"
        );
    }

    #[test]
    fn bootstrap_adds_ccteam_to_gitignore_idempotently() {
        ensure_isolation();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // Existing .gitignore with user content (no trailing newline) —
        // bootstrap must APPEND `.ccteam/` without clobbering, exactly once.
        let project_dir = tmp.path().join("workspace").join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(".gitignore"), "target\nnode_modules").unwrap();

        bootstrap_project_at_dir(&paths, &project_dir, "demo", "(x)", "dev").unwrap();
        let body = std::fs::read_to_string(project_dir.join(".gitignore")).unwrap();
        assert!(
            body.contains("target") && body.contains("node_modules"),
            "must preserve existing .gitignore entries; got: {body:?}"
        );
        assert_eq!(
            body.lines()
                .filter(|l| l.trim().trim_end_matches('/') == ".ccteam")
                .count(),
            1,
            "exactly one .ccteam ignore line; got: {body:?}"
        );

        // Re-bootstrap (idempotent / re-create) must NOT add a duplicate line.
        bootstrap_project_at_dir(&paths, &project_dir, "demo", "(x)", "dev").unwrap();
        let body2 = std::fs::read_to_string(project_dir.join(".gitignore")).unwrap();
        assert_eq!(
            body2
                .lines()
                .filter(|l| l.trim().trim_end_matches('/') == ".ccteam")
                .count(),
            1,
            "re-bootstrap must not duplicate the .ccteam ignore; got: {body2:?}"
        );
    }

    #[test]
    fn ensure_ccteam_home_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        crate::ensure_ccteam_home(&paths).unwrap();
        let hook_sh = paths.hooks_script();
        assert!(hook_sh.exists(), "first ensure must create hook.sh");

        // Second call: still Ok, hook.sh still present + unchanged.
        crate::ensure_ccteam_home(&paths).unwrap();
        assert!(
            hook_sh.exists(),
            "hook.sh must survive the idempotent re-run"
        );
        assert_eq!(
            std::fs::read_to_string(&hook_sh).unwrap(),
            crate::HOOK_DISPATCHER_SH,
        );
    }

    #[test]
    fn read_current_branch_ref_detached_worktree_and_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Not a repo → None.
        assert_eq!(super::read_current_branch(tmp.path()), None);

        // Normal repo on a branch.
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/dev\n").unwrap();
        assert_eq!(super::read_current_branch(&repo).as_deref(), Some("dev"));

        // Detached HEAD → short 12-hex.
        std::fs::write(
            repo.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(
            super::read_current_branch(&repo).as_deref(),
            Some("0123456789ab")
        );

        // Worktree: `.git` FILE pointing at a gitdir.
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let gitdir = tmp.path().join("gitdir-wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature-x\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert_eq!(
            super::read_current_branch(&wt).as_deref(),
            Some("feature-x")
        );
    }

    #[test]
    fn remote_data_home_contains_only_ccteam_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let data_home = tmp.path().join("projects/catalog-demo");
        ensure_project_data_home(&data_home, "catalog-demo", Some("user:alice".into())).unwrap();
        let state = ProjectState::load(&CcteamPaths::project_state_in(&data_home)).unwrap();
        assert_eq!(state.slug, "catalog-demo");
        assert_eq!(state.owner.as_deref(), Some("user:alice"));
        assert!(!data_home.join(".claude").exists());
        assert!(!data_home.join("AGENTS.md").exists());
        assert!(!data_home.join("CLAUDE.md").exists());
    }
}
