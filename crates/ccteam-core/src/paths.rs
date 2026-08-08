//! ccteam path resolver. Centralizes the global (`~/.ccteam/`) and
//! project-local (`~/projects/<slug>/.ccteam/`) layouts documented in
//! `docs/interfaces.md` §1 so hooks, orchestrator, and CLI agree.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::state::ProjectState;

/// Starter for the global, user-owned routing notes. The file is transported
/// verbatim by MCP `status`; the starter asserts NO routing opinion (owner
/// decision 2026-08-08: no pre-assigned roles — it only lists the harness
/// axis ccteam registers, so a session understands the multi-vendor surface,
/// and leaves the division of labor entirely to the user). Installed state,
/// versions, and per-vendor model/effort catalogs are deliberately NOT
/// snapshotted here: the vendor panel in the same `status` response is their
/// live home, and a written-once copy would go stale. A unit test pins this
/// list to `host_registry::AGENT_PROBE_SPECS` so a new vendor cannot be
/// added without updating the starter.
const DEFAULT_ROUTING_NOTES: &str = "# ccteam routing notes\n\n\
Your division of labor, in your own words. Sessions that call `status` receive\n\
this file verbatim; ccteam never parses, merges, or acts on it.\n\n\
## Harnesses ccteam can drive\n\n\
ccteam registers these agent harnesses (spawn any of them via `session_spawn`\n\
or the web/IM entry points):\n\n\
- `claude` — Claude Code (Anthropic)\n\
- `codex` — Codex CLI (OpenAI)\n\
- `grok` — Grok CLI (xAI)\n\
- `opencode` — OpenCode\n\
- `kimi` — Kimi Code (Moonshot)\n\
- `pi` — Pi (local-only harness)\n\n\
Which of them are actually installed on this machine — with versions and each\n\
vendor's advisory model + effort catalog — arrives live in the vendor panel of\n\
the same `status` response as these notes. Trust that panel, not a list here.\n\n\
## Division of labor (yours to write)\n\n\
ccteam pre-assigns no roles. Default posture: omit `model` at spawn and ride\n\
each vendor's default. When you develop preferences, write them below in your\n\
own words — e.g. one row per task type:\n\n\
| Task type | vendor / model / effort | Why |\n\
|---|---|---|\n\
| (yours) | | |\n\n\
<!--\n\
A project replaces these global notes entirely with its own\n\
<project>/.ccteam/routing.md (never merged). Never store secrets in either\n\
file: ccteam returns the selected file verbatim to sessions that call status.\n\
-->\n";

#[derive(Debug, Clone)]
pub struct CcteamPaths {
    /// `~/.ccteam/` — the global ccteam root.
    pub root: PathBuf,
    /// `~/projects/` — where project working trees live.
    pub projects_root: PathBuf,
}

impl CcteamPaths {
    /// Resolve from the running user's home directory. Honors the
    /// `CCTEAM_HOME` and `CCTEAM_PROJECTS_ROOT` env vars for tests and
    /// custom layouts. V0.4.2 F73: when `CCTEAM_PROJECTS_ROOT` is not
    /// set, fall back to `~/.ccteam/config.yaml::projects_root`
    /// before defaulting to `$HOME/projects`.
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
        let root = std::env::var("CCTEAM_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".ccteam"));
        // Priority for projects_root:
        //   1. CCTEAM_PROJECTS_ROOT env  (ad-hoc / test override)
        //   2. ~/.ccteam/config.yaml::projects_root  (V0.4.2 F73 SoT)
        //   3. $HOME/projects  (hardcoded fallback)
        let projects_root = std::env::var("CCTEAM_PROJECTS_ROOT")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                crate::config::load(&root)
                    .ok()
                    .and_then(|c| c.projects_root)
            })
            .unwrap_or_else(|| home.join("projects"));
        Ok(Self {
            root,
            projects_root,
        })
    }

    // v0.8.20 — `~/.ccteam/` is grouped by concern: `config.yaml` (user config,
    // top-level) · `hooks/` (the hook.sh dispatcher) · `secrets/` (0700: every
    // token) · `cache/` (deletable) · `run/` (live sockets) · `state/` (everything
    // the daemon writes). The override is `CCTEAM_HOME` (or `--home`), so a second
    // instance can live under `~/.ccteam2`.

    /// `~/.ccteam/state/` — everything the running daemon writes (pidfile,
    /// per-project progress, IM daemon state, statusline, pty FIFOs).
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// `~/.ccteam/secrets/` (mode 0700) — every credential/token: the admin web
    /// token, the global bot creds, and per-user (`users/<id>.json`).
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    /// `~/.ccteam/secrets/users/` — one `<id>.json` per web tenant (its web token
    /// + IM creds + profile). Replaces the single `tenants.json`.
    pub fn users_dir(&self) -> PathBuf {
        self.secrets_dir().join("users")
    }

    /// `~/.ccteam/secrets/web-token` — the admin/bootstrap web token.
    pub fn web_token_path(&self) -> PathBuf {
        self.secrets_dir().join("web-token")
    }

    /// `~/.ccteam/secrets/im-credentials.json` — the global/admin bot creds
    /// (telegram/lark). Per-tenant bots live in `users/<id>.json`.
    pub fn im_credentials_path(&self) -> PathBuf {
        self.secrets_dir().join("im-credentials.json")
    }

    /// `~/.ccteam/cache/` — ephemeral, safe to delete.
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `~/.ccteam/state/im/` — the IM daemon's state (bot registry + outbound
    /// ledger). Was the top-level `imd/` — the `im/` vs `imd/` name clash is gone
    /// (global creds are now `secrets/im-credentials.json`). v0.8.21 Wave-2: the
    /// gateway routing snapshot moved out to `state/gateway/routing.json` and the
    /// sid counter to `state/sessions/next-sid` (the old `gateway-state.json`
    /// session store is retired; `meta.json` is the session SoT).
    pub fn im_state_dir(&self) -> PathBuf {
        self.state_dir().join("im")
    }

    pub fn progress_jsonl(&self, slug: &str) -> PathBuf {
        self.progress_dir().join(format!("{slug}.jsonl"))
    }

    /// `~/.ccteam/progress/` — directory holding `<slug>.jsonl`
    /// streams. Public since V0.3 M5.2 so the `ccteam-web` watcher
    /// can attach a recursive `notify` watcher without re-deriving
    /// the path.
    pub fn progress_dir(&self) -> PathBuf {
        self.state_dir().join("progress")
    }

    /// `~/.ccteam/hub-cache/` — v0.8.9 Phase 2 local cache of the
    /// ccteam-hub catalog. `ccteam_im::hub::load_catalog` writes
    /// `index.json` here (atomic tmp + rename) on a refresh and reads it
    /// back for offline browse. Part of [`canonical_home_dirs`] so the
    /// `ccteam doctor` home-drift check does not flag it. Honours
    /// `CCTEAM_HOME` via [`CcteamPaths::from_env`] like every other
    /// `~/.ccteam/` subdir.
    pub fn hub_cache_dir(&self) -> PathBuf {
        self.cache_dir().join("hub")
    }

    /// `~/.ccteam/skills/` — the user-level global skill library. Skills may
    /// use nested ids and are discovered by their `<id>/SKILL.md` entrypoint.
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// Global user-authored routing notes. They are an advisory fallback for
    /// projects without their own `.ccteam/routing.md`.
    pub fn global_routing_notes(&self) -> PathBuf {
        self.root.join("routing.md")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.root.join("inbox")
    }

    pub fn control_dir(&self) -> PathBuf {
        self.root.join("control")
    }

    pub fn phases_dir(&self) -> PathBuf {
        self.root.join("phases")
    }

    /// `~/.ccteam/templates/` — global helper templates that phase
    /// markdown can `@`-reference (M2.4, interfaces §5). Distinct from
    /// `phases/` because helpers are *prompt fragments*, not whole
    /// phases — they have no front-matter, no DAG position.
    pub fn templates_dir(&self) -> PathBuf {
        self.root.join("templates")
    }

    /// `~/.ccteam/hooks/` — V0.6.1 F139 directory holding the
    /// `hook.sh` dispatcher (the single shell wrapper every Claude Code
    /// hook command in a ccteam project invokes). Materialized by
    /// `ccteam init` and `ccteam doctor --install-hooks`.
    pub fn hooks_dir(&self) -> PathBuf {
        self.root.join("hooks")
    }

    /// `~/.ccteam/hooks/hook.sh` — V0.6.1 F139 dispatcher script the
    /// project-level `.claude/settings.json` hook commands point at. The
    /// script POSTs the Claude Code hook stdin to the long-running
    /// daemon's `/internal/hook/:kind[/:action]` route (fast path) and
    /// falls back to `ccteam internal hook ...` when the daemon is
    /// unreachable. ~20× faster than a per-hook Rust binary spawn.
    pub fn hooks_script(&self) -> PathBuf {
        self.hooks_dir().join("hook.sh")
    }

    /// Resolve a slug to its on-disk project directory.
    ///
    /// V0.4.4 F77: lazily consults `~/.ccteam/config.yaml::projects[]`
    /// so V0.4.2 arbitrary-path installs (e.g. `/vol4/.../dex-ui`)
    /// resolve correctly without each of the ~200 callsites having to
    /// thread a registry. Falls back to `projects_root.join(slug)` when
    /// the slug isn't registered (V0.1–V0.4.1 layout, in-flight
    /// `ccteam init` before the registry write, and tests that pre-date
    /// V0.4.2 config.yaml).
    ///
    /// Config reads are atomic-rename-safe (see `config::save`) and
    /// cheap (< 10 KB file); a corrupt config silently falls back so
    /// path resolution never panics from this method.
    pub fn project_dir(&self, slug: &str) -> PathBuf {
        if let Ok(cfg) = crate::config::load(&self.root) {
            if let Some(entry) = cfg.projects.into_iter().find(|p| p.slug == slug) {
                return entry.path;
            }
        }
        self.projects_root.join(slug)
    }

    pub fn project_ccteam_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join(".ccteam")
    }

    /// Project-owned routing notes. For a local project this is its worktree's
    /// `<project>/.ccteam/routing.md`. A remote catalog entry resolves to the
    /// daemon-side project data home, keeping this control-plane preference
    /// available without adding a satellite filesystem-sync protocol.
    pub fn project_routing_notes(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug).join("routing.md")
    }

    pub fn project_state(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug).join("state.json")
    }

    pub fn progress_jsonl_for_context(&self, context: &ProjectSessionContext) -> PathBuf {
        self.progress_jsonl(&context.slug)
    }

    pub fn project_state_in(project_dir: &Path) -> PathBuf {
        project_dir.join(".ccteam").join("state.json")
    }

    /// `<project>/.ccteam/pending-inject.json` — V0.2.2 F36 deferred
    /// phase-inject record. See `crate::pending_inject` for shape +
    /// lifecycle.
    pub fn project_pending_inject(&self, slug: &str) -> PathBuf {
        self.project_ccteam_dir(slug)
            .join(crate::pending_inject::PENDING_INJECT_FILE)
    }

    /// `~/.ccteam/pty/` — V0.3.2 F56 directory holding FIFO files used
    /// by the web layer's `tmux pipe-pane` relay (one FIFO per active
    /// `<slug>` or `<slug>-<sid>` subscription). Files are created /
    /// unlinked at runtime by `ccteam_web::routes::pty_ws`.
    ///
    /// **Architectural red line** (CLAUDE.md §三, PRD §F56 §6): this
    /// directory is a presentation-layer control plane. The
    /// orchestrator never reads it; `progress.jsonl` remains the
    /// single source of truth.
    pub fn pty_dir(&self) -> PathBuf {
        self.state_dir().join("pty")
    }

    /// `~/.ccteam/harness/` — V0.3.1 F46 dual-write target for the
    /// Claude Code statusline wrapper (and future Codex equivalent).
    /// Each session deposits one `<slug>-<sid>.json` file holding the
    /// most recent harness statusline JSON; the ccteam-web watcher
    /// tails this dir and broadcasts `harness_snapshot` events.
    ///
    /// **Architectural red line** (CLAUDE.md §三, PRD §3.3): files in
    /// this directory are *presentation only*. The orchestrator state
    /// machine never reads them — `progress.jsonl` remains the single
    /// source of truth.
    pub fn harness_dir(&self) -> PathBuf {
        self.state_dir().join("harness")
    }

    /// `~/.ccteam/state/hosts/` — v0.8.24 multi-host registry (registered
    /// satellites + satellite-side `self.json` after `ccteam host join`).
    pub fn hosts_dir(&self) -> PathBuf {
        self.state_dir().join("hosts")
    }

    /// `~/.ccteam/state/hosts/registry.json` — main-daemon host registry SoT.
    pub fn host_registry_path(&self) -> PathBuf {
        self.hosts_dir().join("registry.json")
    }

    /// `~/.ccteam/secrets/host-join-tokens.json` — admin-minted join tokens.
    pub fn host_join_tokens_path(&self) -> PathBuf {
        self.secrets_dir().join("host-join-tokens.json")
    }

    /// `~/.ccteam/teams-progress.jsonl` — V0.5.0 F95 global progress
    /// stream for Anthropic Agent Teams events (`team_member_joined`
    /// / `team_member_left` / `team_message_sent` / `team_task_created`
    /// / `team_task_completed`).
    ///
    /// Distinct from per-project `progress.jsonl`: an Agent Team is
    /// owned by Anthropic (`~/.claude/teams/<name>/`), not by any
    /// ccteam workflow. Mixing team events into a project file would
    /// conflate streams. F96 web SPA reads this global file on
    /// `/teams` tab; per-project progress stays untouched.
    ///
    /// **Architectural red line** (CLAUDE.md §三): writes here are the
    /// only state side-effect F95 watcher takes; `~/.claude/teams/`
    /// itself is strictly read-only (Anthropic SoT).
    pub fn teams_progress_jsonl(&self) -> PathBuf {
        self.root.join("teams-progress.jsonl")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSessionContext {
    pub slug: String,
    pub sid: Option<String>,
    pub project_dir: PathBuf,
}

/// v0.8.6 — canonical `~/.ccteam/` subdirectory layout that `ccteam
/// init` (and the resident daemon on first start) is allowed to create.
/// This is the single source of truth for the home-layout manifest:
/// `ccteam doctor`'s home-drift check compares the real `~/.ccteam/`
/// against this set and flags any orchestrator-era leftovers
/// (`phases/`, `queue/`, `memory/`, `control/`, `templates/`, …).
///
/// The config file (`~/.ccteam/config.yaml`) and other top-level files
/// (`web-token`, `teams-progress.jsonl`, …) are intentionally excluded
/// — this lists directories only. `imd/` is created lazily by bot
/// registration, not by `init`, so it is not part of the init-time set.
///
/// v0.8.20 — the home is grouped by concern (top-level dirs): `hooks/` (the
/// hook.sh dispatcher), `run/` (live sockets), `state/` (everything the daemon
/// writes — `progress/`, `im/`, `harness/`, `pty/`, the pidfile), `secrets/`
/// (0700 — `web-token`, `im-credentials.json`, `users/<id>.json`), and `cache/`
/// (deletable — `hub/`), and `skills/` (the user-level global skill library).
/// Subdirectories are created lazily by their writers (`create_dir_all`);
/// this is the top-level manifest the doctor tolerates.
pub fn canonical_home_dirs() -> &'static [&'static str] {
    &["hooks", "run", "state", "secrets", "cache", "skills"]
}

/// Idempotently materialize the global `~/.ccteam/` home so any
/// downstream session a project references can actually run.
///
/// Three steps, all idempotent and cheap:
///
/// 1. Create exactly the [`canonical_home_dirs`] manifest under
///    `paths.root` (`std::fs::create_dir_all` — a no-op when present).
/// 2. [`install_hooks`](crate::hooks_dispatcher::install_hooks)
///    materializes `~/.ccteam/hooks/hook.sh`, the dispatcher the
///    project-level `.claude/settings.local.json` SessionStart hook
///    commands point at. Re-running returns
///    [`InstallHooksAction::Unchanged`](crate::hooks_dispatcher::InstallHooksAction::Unchanged).
/// 3. Create `~/.ccteam/routing.md` with neutral starter text when absent.
///    Existing user content is never overwritten.
///
/// This is the single home-ensure every create/start path calls so the
/// home is never half-built. Historically only `ccteam init`, the
/// `chat_register` MCP tool, and `ccteam doctor --install-hooks` ran
/// `install_hooks`; the web `POST /projects` + IM/web chat create path
/// (`projects::bootstrap_project_at_dir`) wrote a project settings file
/// that *references* `hook.sh` without ever materializing it, yielding a
/// "hook.sh: not found" at the first SessionStart. Calling this from
/// every create/start path closes that gap.
pub fn ensure_ccteam_home(paths: &CcteamPaths) -> Result<()> {
    for sub in canonical_home_dirs() {
        let dir = paths.root.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // v0.8.20 — `secrets/` holds every token → 0700 (POSIX). The token/cred
    // writers set 0600 on the files; this hardens the directory itself.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(paths.secrets_dir()) {
            let mut perm = meta.permissions();
            perm.set_mode(0o700);
            let _ = std::fs::set_permissions(paths.secrets_dir(), perm);
        }
    }
    crate::hooks_dispatcher::install_hooks(paths)
        .context("install ~/.ccteam/hooks/hook.sh dispatcher")?;
    ensure_global_routing_notes(paths)?;
    Ok(())
}

/// Create the global routing-notes starter exactly once. `create_new` is the
/// no-clobber primitive: concurrent init/start calls may race, but only one can
/// create the file and neither can overwrite a user's edit.
fn ensure_global_routing_notes(paths: &CcteamPaths) -> Result<()> {
    let path = paths.global_routing_notes();
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::AlreadyExists => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("create {}", path.display())),
    };
    file.write_all(DEFAULT_ROUTING_NOTES.as_bytes())
        .with_context(|| format!("write {}", path.display()))
}

/// V0.5.0 F95 — resolve the **global** Anthropic Agent Teams progress
/// stream path (`~/.ccteam/teams-progress.jsonl`). F96 SSE channel
/// reads this file; F95 watcher writes it. Pure: honours `CCTEAM_HOME`
/// for tests (via [`CcteamPaths::from_env`]).
///
/// Returns an error only if home-dir resolution fails (extremely rare
/// — same failure mode that breaks every other ccteam command).
pub fn teams_progress_path() -> Result<PathBuf> {
    Ok(CcteamPaths::from_env()?.teams_progress_jsonl())
}

/// V0.5.0 F95 — default `~/.claude/teams/` root that the global
/// agent-teams watcher discovers. Anthropic writes config / inboxes
/// under this path; `~/.claude/tasks/<team>/` is sibling-but-separate.
/// Honours `CCTEAM_AGENT_TEAMS_ROOT` for tests so we don't have to
/// touch the user's real `~/.claude/` during integration tests.
pub fn agent_teams_root() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CCTEAM_AGENT_TEAMS_ROOT") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    Ok(home.join(".claude").join("teams"))
}

/// V0.5.0 F95 — sibling task-list root for Anthropic Agent Teams. Same
/// override env (`CCTEAM_AGENT_TASKS_ROOT`) for tests.
pub fn agent_tasks_root() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CCTEAM_AGENT_TASKS_ROOT") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    Ok(home.join(".claude").join("tasks"))
}

/// Read a project's slug by loading `<project_dir>/.ccteam/state.json`.
/// Hooks use this to bridge from the `cwd` field of a Claude Code hook
/// payload to the global progress.jsonl path.
pub fn slug_from_project_dir(project_dir: &Path) -> Result<String> {
    let state_path = CcteamPaths::project_state_in(project_dir);
    let bytes =
        std::fs::read(&state_path).with_context(|| format!("read {}", state_path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", state_path.display()))?;
    let slug = v.get("slug").and_then(|s| s.as_str()).ok_or_else(|| {
        anyhow!(
            "state.json missing `slug` field at {}",
            state_path.display()
        )
    })?;
    Ok(slug.to_string())
}

pub fn session_context_from_cwd(cwd: &Path, paths: &CcteamPaths) -> Result<ProjectSessionContext> {
    if cwd
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!("cwd must not contain `..`: {}", cwd.display()));
    }
    // V0.4.4 F77: hooks fire from any cwd a Claude Code session opens
    // — for V0.4.2 arbitrary-path installs (e.g. `/vol4/.../dex-ui`)
    // the cwd is NOT under `paths.projects_root`. Walk upward looking
    // for `<dir>/.ccteam/state.json`; the first hit is `project_dir`.
    // Subsumes the V0.1-V0.4.1 `~/projects/<slug>/.ccteam/state.json`
    // layout (the walk halts there too) so callers see one shape.
    let (project_dir, rel) = find_project_root(cwd).ok_or_else(|| {
        anyhow!(
            "cwd {} is not under any ccteam project (no `.ccteam/state.json` found walking up; \
             run `ccteam init` inside this directory or check `~/.ccteam/config.yaml`)",
            cwd.display()
        )
    })?;
    let state = ProjectState::load(&CcteamPaths::project_state_in(&project_dir))
        .with_context(|| format!("load state.json for project at {}", project_dir.display()))?;
    let sid = sid_from_components(rel.components());
    let _ = paths; // signature kept for API stability; slug-keyed paths read elsewhere
    Ok(ProjectSessionContext {
        slug: state.slug,
        sid,
        project_dir,
    })
}

/// Walk up from `cwd` searching for the closest directory `D` such
/// that `D/.ccteam/state.json` exists. Returns `(D, rel)` where `rel`
/// is the path from `D` down to `cwd` (empty if `cwd == D`).
fn find_project_root(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut current: Option<&Path> = Some(cwd);
    while let Some(dir) = current {
        if CcteamPaths::project_state_in(dir).is_file() {
            let rel = cwd.strip_prefix(dir).unwrap_or(Path::new("")).to_path_buf();
            return Some((dir.to_path_buf(), rel));
        }
        current = dir.parent();
    }
    None
}

fn sid_from_components<'a>(
    mut comps: impl Iterator<Item = std::path::Component<'a>>,
) -> Option<String> {
    match comps.next() {
        Some(std::path::Component::Normal(n)) if n == ".ccteam" => {}
        _ => return None,
    }
    match comps.next() {
        Some(std::path::Component::Normal(n)) if n == "sessions" => {}
        _ => return None,
    }
    match comps.next() {
        Some(std::path::Component::Normal(n)) => n.to_str().map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        }
    }

    #[test]
    fn session_context_detects_sid_subdir() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let slug = "demo";
        let project_dir = paths.project_dir(slug);
        let session_dir = paths
            .project_ccteam_dir(slug)
            .join("sessions")
            .join("claude-1");
        std::fs::create_dir_all(&session_dir).unwrap();
        ProjectState::initial_for_team(slug.into(), "dev".into())
            .save(&paths.project_state(slug))
            .unwrap();

        let cwd = session_dir.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let context = session_context_from_cwd(&cwd, &paths).unwrap();
        assert_eq!(context.slug, slug);
        assert_eq!(context.sid.as_deref(), Some("claude-1"));
        assert_eq!(context.project_dir, project_dir);
        // Progress routing is flat regardless of sid sub-dir.
        assert_eq!(
            paths.progress_jsonl_for_context(&context),
            paths.progress_jsonl(slug),
        );
    }

    #[test]
    fn session_context_keeps_workflow_progress_flat() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let slug = "dev-demo";
        std::fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
        ProjectState::initial_for_team(slug.into(), "dev".into())
            .save(&paths.project_state(slug))
            .unwrap();

        let context = session_context_from_cwd(&paths.project_dir(slug), &paths).unwrap();
        assert_eq!(context.sid, None);
        assert_eq!(
            paths.progress_jsonl_for_context(&context),
            paths.progress_jsonl(slug),
        );
    }

    /// V0.4.4 F77: hooks fire from an arbitrary install path
    /// (e.g. `/vol4/.../dex-ui`) — cwd is NOT under `projects_root`.
    /// `session_context_from_cwd` must walk up, find the project's
    /// `.ccteam/state.json`, and return the real `project_dir`.
    #[test]
    fn f77_session_context_resolves_arbitrary_path_install() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let elsewhere = tmp.path().join("workspace").join("dex-ui");
        std::fs::create_dir_all(elsewhere.join(".ccteam")).unwrap();
        let slug = "dex-ui";
        ProjectState::initial_for_team(slug.into(), "dev".into())
            .save(&CcteamPaths::project_state_in(&elsewhere))
            .unwrap();

        let context = session_context_from_cwd(&elsewhere, &paths).unwrap();
        assert_eq!(context.slug, slug);
        assert_eq!(context.sid, None);
        assert_eq!(context.project_dir, elsewhere);
    }

    /// V0.4.4 F77: when the hook's cwd is a *subdirectory* of an
    /// arbitrary-path project (e.g. the Claude session is inside
    /// `/vol4/.../dex-ui/src/main`), walk-up still finds the project
    /// root and returns the project_dir + empty sid.
    #[test]
    fn f77_session_context_resolves_subdir_of_arbitrary_path_install() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let elsewhere = tmp.path().join("vol4").join("repos").join("api-svc");
        let cwd = elsewhere.join("src").join("handlers");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(elsewhere.join(".ccteam")).unwrap();
        let slug = "api-svc";
        ProjectState::initial_for_team(slug.into(), "dev".into())
            .save(&CcteamPaths::project_state_in(&elsewhere))
            .unwrap();

        let context = session_context_from_cwd(&cwd, &paths).unwrap();
        assert_eq!(context.slug, slug);
        assert_eq!(context.sid, None);
        assert_eq!(context.project_dir, elsewhere);
    }

    /// V0.4.4 F77: cwd has no `.ccteam/state.json` upward → fail loud
    /// with an actionable message (not the legacy "is not under
    /// `projects_root`" which was both wrong and unhelpful).
    #[test]
    fn f77_session_context_fails_loud_outside_any_project() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let cwd = tmp.path().join("nowhere").join("nada");
        std::fs::create_dir_all(&cwd).unwrap();
        let err = session_context_from_cwd(&cwd, &paths).unwrap_err();
        let s = format!("{err:#}");
        assert!(s.contains("no `.ccteam/state.json` found"), "got: {s}");
    }

    /// V0.4.4 F77: `paths.project_dir(slug)` consults
    /// `~/.ccteam/config.yaml::projects[]` first, so arbitrary-path
    /// installs route correctly without each call site needing fixup.
    /// Falls back to `projects_root.join(slug)` when slug is not in
    /// the registry.
    #[test]
    fn f77_project_dir_consults_config_registry() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);
        let elsewhere = tmp.path().join("foo").join("bar");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(&paths.root).unwrap();

        crate::config::upsert_project(
            &paths.root,
            crate::config::ProjectEntry {
                slug: "anywhere".into(),
                path: elsewhere.clone(),
                host: crate::config::default_project_host(),
                remote_slug: None,
                remote_path: None,
                team: "dev".into(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();

        assert_eq!(paths.project_dir("anywhere"), elsewhere);
        // Fallback for unregistered slugs preserves legacy layout.
        assert_eq!(
            paths.project_dir("unregistered"),
            paths.projects_root.join("unregistered"),
        );
    }

    #[test]
    fn routing_notes_paths_are_global_and_project_owned() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);

        assert_eq!(paths.global_routing_notes(), paths.root.join("routing.md"));
        assert_eq!(
            paths.project_routing_notes("demo"),
            paths
                .projects_root
                .join("demo")
                .join(".ccteam")
                .join("routing.md")
        );
    }

    #[test]
    fn skills_dir_is_in_canonical_manifest_and_home_ensure_creates_it() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);

        assert_eq!(paths.skills_dir(), paths.root.join("skills"));
        assert!(canonical_home_dirs().contains(&"skills"));
        ensure_ccteam_home(&paths).unwrap();
        assert!(paths.skills_dir().is_dir());
    }

    #[test]
    fn ensure_home_creates_default_routing_notes_without_overwriting() {
        let tmp = TempDir::new().unwrap();
        let paths = paths(&tmp);

        ensure_ccteam_home(&paths).unwrap();
        let routing = paths.global_routing_notes();
        assert!(routing.is_file());
        assert_eq!(
            std::fs::read_to_string(&routing).unwrap(),
            DEFAULT_ROUTING_NOTES
        );

        std::fs::write(&routing, "# My routing\nkeep this\n").unwrap();
        ensure_ccteam_home(&paths).unwrap();
        assert_eq!(
            std::fs::read_to_string(&routing).unwrap(),
            "# My routing\nkeep this\n"
        );
    }

    /// The starter lists every harness ccteam registers and assigns none of
    /// them a role: adding a vendor to `AGENT_PROBE_SPECS` must update the
    /// starter, and the starter must never regrow a pre-filled assignment
    /// table (owner decision 2026-08-08 — division of labor is the user's).
    #[test]
    fn default_routing_notes_list_the_vendor_axis_without_assigning_roles() {
        for spec in crate::host_registry::AGENT_PROBE_SPECS {
            assert!(
                DEFAULT_ROUTING_NOTES.contains(&format!("`{}`", spec.vendor)),
                "starter must list vendor `{}`",
                spec.vendor
            );
        }
        // The division-of-labor table stays a blank skeleton for the user.
        assert!(DEFAULT_ROUTING_NOTES.contains("| (yours) | | |"));
        let table_rows = DEFAULT_ROUTING_NOTES
            .lines()
            .filter(|l| l.starts_with('|') && !l.starts_with("|---"))
            .count();
        assert_eq!(table_rows, 2, "header + blank row only — no sample roles");
    }
}
