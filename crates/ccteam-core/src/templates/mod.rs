//! V0.4.0 F60 — embedded ccteam project bootstrap templates.
//!
//! Pre-F60 this module also held the shipped phase template payloads
//! (`PHASE_TEMPLATES`, `TEAM_BUNDLES`, `write_*_phase_templates`,
//! `write_all_global_team_templates`). Those went with the rest of
//! the phase machinery; F66 reintroduces a much smaller bootstrap path
//! against the new `workflow.yaml` schema.
//!
//! There is no per-project Claude settings template: project bootstrap
//! writes nothing into `.claude/settings.local.json` (docs-local issue #212 —
//! the hooks it carried only served the retired tmux/rmux protocol).
//!
//! What survives F60:
//! - the helper-template writer (`write_global_helper_templates`) —
//!   helper markdown is referenced by user-authored agent prompts via
//!   `@~/.ccteam/templates/<name>` and is not coupled to phases.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

// Per-project `.mcp.json` merge helper for third-party servers (ccteam's own
// entry is global-only + HTTP — see `crate::mcp_register`).
pub mod project_mcp_json;
pub use project_mcp_json::{
    merge_named_mcp_server, validate_mcp_server_name, CCTEAM_MCP_SERVER_KEY,
};

// V0.6.0 Wave 2 F114 — `ccteam-creator` preset workflow.yaml templates.
// Five presets (inproc-solo / inproc-team / bg-overnight / chat-pocket /
// chat-squad) rendered via dep-free `{{var}}` substitution. The skill
// picks a preset from `mode_inferrer` + persona, fills `TemplateCtx`,
// calls `render(preset, &ctx)`, then writes the result to
// `<project>/.ccteam/workflow.yaml`.
pub mod workflow_templates;
pub use workflow_templates::{
    apply_probe_defaults as apply_probe_defaults_to_workflow_ctx,
    default_ctx as default_workflow_ctx, render as render_workflow_template,
    render_agents_block as render_workflow_agents_block, AgentTemplateEntry as WorkflowAgentEntry,
    Preset as WorkflowPreset, RenderError as WorkflowTemplateRenderError,
    TemplateCtx as WorkflowTemplateCtx,
};

// V0.6.6 F167 note: there is no separate `render_workflow_template_with_probe`
// function. Callers build the ctx, then call
// `apply_probe_defaults_to_workflow_ctx(&mut ctx, preset, &probe)` to
// overlay sensible defaults, then call `render_workflow_template` as
// usual. This keeps the render entry point single + strict, and lets
// callers stack their own ctx overrides between the probe overlay and
// the render call.

// V0.6.6 F167 — project-type probe for `/ccteam-creator` sensible
// defaults. Surfaces ProjectKind / languages / probable_scope from a
// file-existence sweep so the skill's PROJECT PLAN can pre-populate
// the rendered workflow.yaml's `scope:` field instead of shipping a
// generic stub the user has to hand-edit before first run.
pub mod project_probe;
pub use project_probe::{probe as probe_project, Language, ProjectKind, ProjectProbe};

/// M2.4: helper templates that user-authored agent / workflow markdown
/// can `@`-reference. Shipped inside the binary so a fresh install (or
/// `ccteam doctor`) can stamp them into `~/.ccteam/templates/` without
/// an external git checkout.
///
/// `(on_disk_filename, body)` — markdown references them as
/// `@~/.ccteam/templates/<on_disk_filename>` and Claude Code's native
/// `@` mechanism inlines the body at prompt-build time. The orchestrator
/// does not parse these — they're pure Claude Code surface.
///
/// V0.5.0 F101: the V0.2-era helpers `review-with-user-loop.md` and
/// `kickoff-reverse-interview.md` were removed alongside the phase-era
/// meta-agent reshape. memory_bridge_*.md lives in `memory_bridge.rs`
/// and is written to `~/.claude/rules/`, not `~/.ccteam/templates/`,
/// so it does not belong in this list. The list stays as a single
/// const so future helpers can be added back in one place.
pub const HELPER_TEMPLATES: &[(&str, &str)] = &[];

/// Resolve the path to the running ccteam binary, suitable for spawning /
/// re-exec. Delegates to [`resolve_spawnable_exe`] over `current_exe()`.
///
/// Any caller that launches a **child** ccteam process (daemon detach,
/// MCP server registration, version probe) MUST route through this rather
/// than raw `current_exe()`: after `install.sh` atomically replaces the
/// running binary, `current_exe()` reports the deleted inode and spawning
/// it fails with ENOENT.
pub fn current_ccteam_bin() -> Result<std::path::PathBuf> {
    let raw = std::env::current_exe().context("std::env::current_exe")?;
    resolve_spawnable_exe(&raw)
}

/// Resolve a `current_exe()`-style `raw` path to an on-disk executable the
/// process can spawn. Pure over `raw` so it is unit-testable without
/// self-replacing the test runner's own ELF.
///
/// Order:
/// 1. `canonicalize(raw)` — the live path (a symlinked launcher pins the
///    real binary; canonicalize also normalizes `/proc/self/exe`).
/// 2. If `raw` ends in the Linux `" (deleted)"` suffix (the kernel appends
///    it to `/proc/self/exe` once the mapped inode is unlinked by an atomic
///    replace), retry against the stripped path — the NEW binary already
///    lives there.
/// 3. `raw` still on disk under its own name (canonicalize rejected a
///    `/proc`-style path in some container test envs, but the file exists).
/// 4. Otherwise a clear error (a bare deleted path would only ENOENT at
///    spawn) — set `CCTEAM_DAEMON_BIN` or re-run from a fresh shell.
pub fn resolve_spawnable_exe(raw: &Path) -> Result<std::path::PathBuf> {
    if let Ok(canon) = raw.canonicalize() {
        return Ok(canon);
    }
    if let Some(stripped) = raw.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        let candidate = std::path::PathBuf::from(stripped);
        if let Ok(canon) = candidate.canonicalize() {
            return Ok(canon);
        }
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if raw.is_file() {
        return Ok(raw.to_path_buf());
    }
    Err(anyhow!(
        "cannot resolve an on-disk ccteam binary (current_exe={}); \
         set CCTEAM_DAEMON_BIN or re-run from a fresh shell",
        raw.display()
    ))
}

/// M2.4 / V0.5.0 F101: ensure `<global_dir>/templates/` exists + write
/// any embedded payloads in [`HELPER_TEMPLATES`] so user-authored
/// markdown's `@~/.ccteam/templates/<filename>` reference resolves.
/// Idempotent; `force == false` preserves operator hand-edits.
///
/// V0.5.0 F101 emptied `HELPER_TEMPLATES`, so the writer now just
/// guarantees the directory exists. Future helpers added back to the
/// list will be stamped here automatically.
pub fn write_global_helper_templates(global_dir: &Path, force: bool) -> Result<()> {
    let dir = global_dir.join("templates");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (filename, body) in HELPER_TEMPLATES {
        let path = dir.join(filename);
        if path.exists() && !force {
            continue;
        }
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolve_spawnable_exe_prefers_live_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("ccteam");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let got = resolve_spawnable_exe(&bin).unwrap();
        assert_eq!(got, bin.canonicalize().unwrap());
    }

    #[test]
    fn resolve_spawnable_exe_strips_linux_deleted_suffix() {
        // Simulate the post-atomic-swap state: `current_exe()` returns
        // `<path> (deleted)` while the NEW binary already sits at `<path>`.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("ccteam");
        std::fs::write(&real, b"#!/bin/sh\n").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        let got = resolve_spawnable_exe(&deleted).unwrap();
        assert_eq!(got, real.canonicalize().unwrap());
    }

    #[test]
    fn resolve_spawnable_exe_errors_when_stripped_target_missing() {
        // Deleted-and-not-replaced: a bare deleted path with no on-disk
        // file must be a clear error, not a silent fall-back that ENOENTs
        // later at spawn time.
        let dir = tempfile::tempdir().unwrap();
        let deleted = PathBuf::from(format!("{}/gone (deleted)", dir.path().display()));
        assert!(resolve_spawnable_exe(&deleted).is_err());
    }
}
