//! V0.3 M5.1 — read-side query helpers shared by every channel layer.
//!
//! Promoted from `ccteam-cli/src/commands.rs` (where they lived as
//! `pub fn`s but were not callable from sibling crates because
//! depending on the binary `ccteam-cli` is a dep-graph anti-pattern).
//! Mirrors `actions.rs` (the M5.0 write-helper promotion):
//!
//! - the V0.3 web UI crate (`ccteam-web`) reads project state /
//!   progress events through this module without depending on
//!   `ccteam-cli`.
//! - the MCP server in `ccteam-cli::mcp_serve` consumes these helpers
//!   identically (the function bodies are unchanged from their
//!   `commands.rs` originals; only their home moves).
//! - `commands.rs::run_ls` / `run_progress` re-export the names from
//!   here so existing callers keep their current `use` lines minus the
//!   module path change.
//!
//! These helpers are **read-only**:
//!
//! - they do **not** mutate `state.json` or write progress events.
//! - they do **not** parse tmux output (architecture red line,
//!   CLAUDE.md §三 — `progress.jsonl` is the orchestrator's SoT).
//! - corrupt / unparseable files surface as logged warnings + skipped
//!   entries; never panics or crashes the caller.
//!
//! Architecture refs: `docs/v0-3/prd.md` §4 (M5.1 dashboard data
//! source), `docs/dev-coupling-audit.md` F45 (extends the M5.0
//! write-helper promotion to the read side), `docs/tech-design.md`
//! §5.5 progress.jsonl SoT.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;

use crate::paths::CcteamPaths;
use crate::state::ProjectState;

/// Project metadata with derived fields used by `ccteam ls`, the MCP
/// `ls` tool, and the V0.3 web dashboard. Pulled out so each renderer
/// (text / JSON / HTML) shares one source of truth instead of
/// re-deriving `age_seconds` / `stall_silent_seconds` per call site.
#[derive(Debug)]
pub struct ProjectSummary {
    pub state: ProjectState,
    pub age_seconds: u64,
    pub stall_silent_seconds: u64,
}

/// Walk `~/.ccteam/projects/`-equivalent (the per-`ProjectState`
/// `state.json` files under `paths.projects_root`), load each, and
/// return one `ProjectSummary` per loadable project. Slug ordering is
/// stable (sorted) so renderers don't need to re-sort.
///
/// Skips entries that are not directories, lack a `state.json`, or
/// whose `state.json` fails to parse — those get a warn-level log line
/// but do not abort the walk. A missing `paths.projects_root`
/// directory returns `Ok(Vec::new())` (a fresh install).
pub fn collect_projects(paths: &CcteamPaths) -> Result<Vec<ProjectSummary>> {
    let dir = &paths.projects_root;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(slug) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let state_path = paths.project_state(&slug);
        if !state_path.exists() {
            continue;
        }
        let state = match ProjectState::load(&state_path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(slug, error = %err, "skip project: state.json failed to load");
                continue;
            }
        };
        let now = Utc::now();
        let age = now
            .signed_duration_since(state.created_at)
            .num_seconds()
            .max(0) as u64;
        let silent = state
            .last_progress_event_at
            .map(|t| now.signed_duration_since(t).num_seconds().max(0) as u64)
            .unwrap_or(age);
        out.push(ProjectSummary {
            state,
            age_seconds: age,
            stall_silent_seconds: silent,
        });
    }
    out.sort_by(|a, b| a.state.slug.cmp(&b.state.slug));
    Ok(out)
}

/// Tail the last `n` JSON-Lines events from
/// `~/.ccteam/progress/<slug>.jsonl`. Lines that fail to parse are
/// silently dropped (matches the orchestrator's tolerant tail policy).
/// Returns `Ok(Vec::new())` if the file does not exist (a project that
/// has emitted no events yet).
pub fn collect_recent_events(paths: &CcteamPaths, slug: &str, n: usize) -> Result<Vec<Value>> {
    let path = paths.progress_jsonl(slug);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut all: Vec<Value> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if all.len() > n {
        let drop = all.len() - n;
        all.drain(..drop);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn fake_paths(root: &std::path::Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    #[test]
    fn collect_projects_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let out = collect_projects(&paths).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_projects_skips_dirs_without_state_json() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        fs::create_dir_all(paths.projects_root.join("orphan")).unwrap();
        let out = collect_projects(&paths).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_projects_loads_one_project() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let state_path = paths.project_state(slug);
        let state = ProjectState::initial(slug.to_string());
        state.save(&state_path).unwrap();

        let out = collect_projects(&paths).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state.slug, slug);
    }

    #[test]
    fn collect_recent_events_missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let out = collect_recent_events(&paths, "nope", 50).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_recent_events_tails_n_lines() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let path = paths.progress_jsonl(slug);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::new();
        for i in 0..10 {
            body.push_str(&format!("{}\n", json!({"event": "x", "i": i})));
        }
        fs::write(&path, body).unwrap();
        let out = collect_recent_events(&paths, slug, 3).unwrap();
        assert_eq!(out.len(), 3);
        // Tail = last 3 lines.
        assert_eq!(out[0]["i"], 7);
        assert_eq!(out[2]["i"], 9);
    }

    #[test]
    fn collect_recent_events_drops_corrupt_lines() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(tmp.path());
        let slug = "dev-foo";
        let path = paths.progress_jsonl(slug);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = format!(
            "{}\nnot-json-at-all\n{}\n",
            json!({"event": "ok", "i": 1}),
            json!({"event": "ok", "i": 2})
        );
        fs::write(&path, body).unwrap();
        let out = collect_recent_events(&paths, slug, 50).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["i"], 1);
        assert_eq!(out[1]["i"], 2);
    }
}
