//! V0.4.0 F60 — embedded ccteam project bootstrap templates.
//!
//! Pre-F60 this module also held the shipped phase template payloads
//! (`PHASE_TEMPLATES`, `TEAM_BUNDLES`, `write_*_phase_templates`,
//! `write_all_global_team_templates`). Those went with the rest of
//! the phase machinery; F66 reintroduces a much smaller bootstrap path
//! against the new `workflow.yaml` schema.
//!
//! What survives F60:
//! - the per-project `.claude/settings.json` template + render helper —
//!   produced-project hook plumbing has not changed shape, so we still
//!   ship the same settings.json scaffold;
//! - the helper-template writer (`write_global_helper_templates`) —
//!   helper markdown is referenced by user-authored agent prompts via
//!   `@~/.ccteam/templates/<name>` and is not coupled to phases.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

// V0.6.0 F111 — per-project `.mcp.json` template + merge helper.
pub mod project_mcp_json;
pub use project_mcp_json::{
    merge_project_mcp_json, render_project_mcp_json, CCTEAM_MCP_SERVER_KEY,
};

// V0.6.0 Wave 2 F114 — `ccteam-creator` preset workflow.yaml templates.
// Five presets (inproc-solo / inproc-team / bg-overnight / chat-pocket /
// chat-squad) rendered via dep-free `{{var}}` substitution. The skill
// picks a preset from `mode_inferrer` + persona, fills `TemplateCtx`,
// calls `render(preset, &ctx)`, then writes the result to
// `<project>/.ccteam/workflow.yaml`.
pub mod workflow_templates;
pub use workflow_templates::{
    default_ctx as default_workflow_ctx, render as render_workflow_template,
    Preset as WorkflowPreset, RenderError as WorkflowTemplateRenderError,
    TemplateCtx as WorkflowTemplateCtx,
};

/// Per-project `.claude/settings.json` template.
///
/// V0.6.1 F139: hook commands route through a single shell wrapper
/// (`__CCTEAM_HOOK_SH__`, materialized at `~/.ccteam/hooks/hook.sh` by
/// `ccteam init` / `ccteam doctor --install-hooks`). The wrapper POSTs
/// the hook stdin to the long-running ccteam daemon's
/// `/internal/hook/:kind[/:action]` route so per-hook latency drops from
/// ~200 ms (cold Rust binary spawn) to ~10 ms (curl round-trip). The
/// wrapper falls back to `ccteam internal hook ...` when the daemon is
/// down so the behaviour matches the pre-F139 path. Pre-F139 templates
/// substituted `__CCTEAM_BIN__` with the absolute `ccteam` binary path;
/// the `rewrite_legacy_hook_commands` rewriter (`tool_surface.rs`) +
/// `ccteam doctor --migrate-hook-commands` rewrite older renders into
/// the new shape.
pub const PROJECT_SETTINGS_JSON: &str = include_str!("settings.json");

/// V0.5.0 F93b + F94 — per-project `.claude/settings.json` template for
/// `mode: agent-team` workflows. Same shape as
/// [`PROJECT_SETTINGS_JSON`] (`__CCTEAM_BIN__` placeholder) plus three
/// new hook entries: `TeammateIdle`, `TaskCreated`, `TaskCompleted`.
/// Used only by `ccteam init --mode agent-team` (per PRD F94 红线:
/// advanced path only).
pub const PROJECT_SETTINGS_AGENT_TEAM_JSON: &str =
    include_str!("settings.agent-team.json");

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

/// Resolve the path to the running ccteam binary. Falls back from
/// canonicalized to raw `current_exe()` because `canonicalize` rejects
/// some `/proc/self/exe`-style paths in container test environments.
pub fn current_ccteam_bin() -> Result<std::path::PathBuf> {
    let raw = std::env::current_exe().context("std::env::current_exe")?;
    Ok(raw.canonicalize().unwrap_or(raw))
}

/// Extra `env` keys to inject into the rendered settings.json. Used to
/// freeze `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` per-project so hook
/// subprocesses see the same paths the orchestrator uses, regardless of
/// the tmux server's potentially stale environment block.
#[derive(Debug, Default, Clone)]
pub struct SettingsEnv {
    pub ccteam_home: Option<String>,
    pub ccteam_projects_root: Option<String>,
}

/// Optional `enabledPlugins` map for the rendered settings.json.
///
/// Each entry is the `<plugin>@<marketplace>` key Claude Code's plugin
/// pipeline expects.
#[derive(Debug, Default, Clone)]
pub struct EnabledPluginsSetting {
    pub plugin_ids: std::collections::BTreeSet<String>,
}

/// Render `PROJECT_SETTINGS_JSON` with `__CCTEAM_HOOK_SH__` replaced by
/// the given absolute path to the `ccteam` hook dispatcher
/// (`~/.ccteam/hooks/hook.sh` in a real install), `extra_env` merged
/// into the top-level `env` block, and `enabled` written under
/// `enabledPlugins`. Validates that the rewritten body is still valid
/// JSON so a path with shell-hostile characters can't silently corrupt
/// the settings file.
///
/// V0.6.1 F139 swap: the V0.4.6 `__CCTEAM_BIN__` placeholder (substituted
/// with the absolute ccteam binary path) is retired. Hook commands now
/// invoke a thin shell wrapper that routes through the long-running
/// daemon's HTTP server (`POST /internal/hook/...`) with a CLI fallback,
/// shaving ~190 ms per hook firing.
pub fn render_project_settings(
    hook_sh: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    render_settings_template(PROJECT_SETTINGS_JSON, hook_sh, extra_env, enabled)
}

/// V0.5.0 F93b + F94 — same as [`render_project_settings`] but
/// renders the agent-team template (with `TeammateIdle` /
/// `TaskCreated` / `TaskCompleted` hooks). Used by
/// `ccteam init --mode agent-team`.
pub fn render_project_settings_agent_team(
    hook_sh: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    render_settings_template(
        PROJECT_SETTINGS_AGENT_TEAM_JSON,
        hook_sh,
        extra_env,
        enabled,
    )
}

/// Shared implementation for [`render_project_settings`] +
/// [`render_project_settings_agent_team`]. Takes the raw template
/// `&str` so callers pick which placeholder text to substitute into.
fn render_settings_template(
    template: &str,
    hook_sh: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    let hook = hook_sh.to_str().ok_or_else(|| {
        anyhow!(
            "ccteam hook.sh path not valid UTF-8: {}",
            hook_sh.display()
        )
    })?;
    if hook.contains('"') || hook.contains('\\') {
        return Err(anyhow!(
            "ccteam hook.sh path contains characters that can't be embedded in settings.json: {hook}"
        ));
    }
    let body = template.replace("__CCTEAM_HOOK_SH__", hook);
    let mut v: Value = serde_json::from_str(&body)
        .with_context(|| format!("rendered settings.json is not valid JSON (hook_sh={hook})"))?;
    if let Some(env) = v.get_mut("env").and_then(|e| e.as_object_mut()) {
        if let Some(home) = &extra_env.ccteam_home {
            env.insert("CCTEAM_HOME".into(), Value::String(home.clone()));
        }
        if let Some(proj) = &extra_env.ccteam_projects_root {
            env.insert("CCTEAM_PROJECTS_ROOT".into(), Value::String(proj.clone()));
        }
    }
    if !enabled.plugin_ids.is_empty() {
        let mut map = serde_json::Map::new();
        for id in &enabled.plugin_ids {
            map.insert(id.clone(), Value::Bool(true));
        }
        v.as_object_mut()
            .ok_or_else(|| anyhow!("settings.json root is not an object"))?
            .insert("enabledPlugins".into(), Value::Object(map));
    }
    serde_json::to_string_pretty(&v).context("serialize rendered settings.json")
}

/// Write `<project_dir>/.claude/settings.json` with hook commands
/// pointing at the running ccteam binary by absolute path, the
/// effective `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` baked into `env`,
/// and the spawned-session `enabledPlugins` set. Creates the parent
/// dir if missing. Idempotent — overwrites any prior render so
/// re-running after a ccteam upgrade refreshes paths and the
/// plugin set.
pub fn write_project_settings(project_dir: &Path, enabled: &EnabledPluginsSetting) -> Result<()> {
    write_settings_template(project_dir, enabled, ProjectSettingsKind::ArtifactDriven)
}

/// V0.5.0 F93b + F94 — same as [`write_project_settings`] but writes
/// the agent-team variant of the settings template (with
/// `TeammateIdle` / `TaskCreated` / `TaskCompleted` hooks per F94).
/// Used by `ccteam init --mode agent-team`. Idempotent.
pub fn write_project_settings_agent_team(
    project_dir: &Path,
    enabled: &EnabledPluginsSetting,
) -> Result<()> {
    write_settings_template(project_dir, enabled, ProjectSettingsKind::AgentTeam)
}

/// V0.5.0 F93b — discriminator for [`write_project_settings`] +
/// [`write_project_settings_agent_team`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectSettingsKind {
    ArtifactDriven,
    AgentTeam,
}

fn write_settings_template(
    project_dir: &Path,
    enabled: &EnabledPluginsSetting,
    kind: ProjectSettingsKind,
) -> Result<()> {
    let dir = project_dir.join(".claude");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("settings.json");
    let hook_sh = effective_hook_sh_path()?;
    let extra = SettingsEnv {
        ccteam_home: std::env::var("CCTEAM_HOME").ok(),
        ccteam_projects_root: std::env::var("CCTEAM_PROJECTS_ROOT").ok(),
    };
    let body = match kind {
        ProjectSettingsKind::ArtifactDriven => render_project_settings(&hook_sh, &extra, enabled)?,
        ProjectSettingsKind::AgentTeam => {
            render_project_settings_agent_team(&hook_sh, &extra, enabled)?
        }
    };
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// V0.6.1 F139 — resolve the absolute path to `~/.ccteam/hooks/hook.sh`
/// honouring `CCTEAM_HOME` (test seam + ops override). Used by
/// [`write_project_settings`] so freshly-rendered settings.json files
/// point at the dispatcher actually materialized on disk by
/// `ccteam init` / `ccteam doctor --install-hooks`.
fn effective_hook_sh_path() -> Result<std::path::PathBuf> {
    let paths = crate::CcteamPaths::from_env()?;
    Ok(paths.hooks_script())
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
    use std::path::Path;

    #[test]
    fn template_is_valid_json_with_expected_hook_keys() {
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&body).expect("rendered template must be valid JSON");
        let hooks = v["hooks"].as_object().expect("hooks object");
        for required in [
            "SessionStart",
            "Stop",
            "Notification",
            "PreToolUse",
            "PostToolUse",
            "SubagentStop",
            "SessionEnd",
        ] {
            assert!(
                hooks.contains_key(required),
                "settings.json template missing `{required}` hook entry",
            );
        }
    }

    #[test]
    fn template_session_start_routes_through_hook_sh() {
        // V0.6.1 F139: hook commands now route through the
        // `~/.ccteam/hooks/hook.sh` wrapper (HTTP-to-daemon + CLI
        // fallback) instead of cold-spawning `ccteam internal hook ...`
        // every firing.
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entries = v["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap();
        let cmds: Vec<&str> = entries
            .iter()
            .map(|e| e["command"].as_str().unwrap())
            .collect();
        assert_eq!(
            cmds,
            vec![
                "/home/u/.ccteam/hooks/hook.sh load-context",
                "/home/u/.ccteam/hooks/hook.sh progress-append session_start"
            ],
        );
    }

    #[test]
    fn raw_template_uses_hook_sh_placeholder() {
        assert!(
            PROJECT_SETTINGS_JSON.contains("__CCTEAM_HOOK_SH__"),
            "template should reference __CCTEAM_HOOK_SH__ placeholder (V0.6.1 F139)",
        );
        // V0.4.6 F89 / V0.6.1 F139: legacy bare `ccteam hook` forms
        // must not survive the migration.
        assert!(
            !PROJECT_SETTINGS_JSON.contains("\"ccteam hook"),
            "template should not embed bare `ccteam hook`",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains(" ccteam hook"),
            "template must not embed bare `ccteam hook` form",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains("__CCTEAM_BIN__"),
            "V0.4.6 `__CCTEAM_BIN__` placeholder was retired by F139",
        );
        // F44 sweep guard: F39's `{{CCT_BIN}}` placeholder must not return.
        assert!(
            !PROJECT_SETTINGS_JSON.contains("{{CCT_BIN}}"),
            "template should not embed F39-era `{{CCT_BIN}}` placeholder (F44)",
        );
    }

    #[test]
    fn render_project_settings_rejects_quoted_path() {
        let err = render_project_settings(
            Path::new("/tmp/has\"quote/hook.sh"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("characters"));
    }

    #[test]
    fn render_project_settings_injects_ccteam_env_when_provided() {
        let env = SettingsEnv {
            ccteam_home: Some("/tmp/sandbox/home".into()),
            ccteam_projects_root: Some("/tmp/sandbox/projects".into()),
        };
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &env,
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["env"]["CCTEAM_HOME"], "/tmp/sandbox/home");
        assert_eq!(v["env"]["CCTEAM_PROJECTS_ROOT"], "/tmp/sandbox/projects");
        // Existing env keys must survive the merge.
        assert_eq!(v["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1");
    }

    #[test]
    fn render_project_settings_omits_env_when_default() {
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["env"]["CCTEAM_HOME"].is_null());
    }

    #[test]
    fn render_project_settings_omits_enabled_plugins_when_empty() {
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &EnabledPluginsSetting::default(),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("enabledPlugins").is_none());
    }

    #[test]
    fn render_project_settings_writes_enabled_plugins_when_present() {
        let mut enabled = EnabledPluginsSetting::default();
        enabled
            .plugin_ids
            .insert("pr-review-toolkit@claude-plugins-official".into());
        enabled
            .plugin_ids
            .insert("feature-dev@claude-plugins-official".into());
        let body = render_project_settings(
            Path::new("/home/u/.ccteam/hooks/hook.sh"),
            &SettingsEnv::default(),
            &enabled,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let map = v["enabledPlugins"]
            .as_object()
            .expect("enabledPlugins object");
        assert_eq!(map.len(), 2);
        assert_eq!(map["pr-review-toolkit@claude-plugins-official"], true);
        assert_eq!(map["feature-dev@claude-plugins-official"], true);
    }
}
