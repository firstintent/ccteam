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

/// Per-project `.claude/settings.json` template. The template uses
/// `__CCTEAM_BIN__` everywhere a real install would name the absolute
/// `ccteam` binary path; we substitute the running binary's absolute path
/// at write time so hook subprocesses don't depend on the user's PATH
/// (which Claude Code inherits and may not include the ccteam install dir).
pub const PROJECT_SETTINGS_JSON: &str = include_str!("templates/settings.json");

/// V0.5.0 F93b + F94 — per-project `.claude/settings.json` template for
/// `mode: agent-team` workflows. Same shape as
/// [`PROJECT_SETTINGS_JSON`] (`__CCTEAM_BIN__` placeholder) plus three
/// new hook entries: `TeammateIdle`, `TaskCreated`, `TaskCompleted`.
/// Used only by `ccteam init --mode agent-team` (per PRD F94 红线:
/// advanced path only).
pub const PROJECT_SETTINGS_AGENT_TEAM_JSON: &str =
    include_str!("templates/settings.agent-team.json");

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

/// Render `PROJECT_SETTINGS_JSON` with `__CCTEAM_BIN__` replaced by the
/// given absolute binary path, `extra_env` merged into the top-level
/// `env` block, and `enabled` written under `enabledPlugins`. Validates
/// that the rewritten body is still valid JSON so a path with
/// shell-hostile characters can't silently corrupt the settings file.
pub fn render_project_settings(
    ccteam_bin: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    render_settings_template(PROJECT_SETTINGS_JSON, ccteam_bin, extra_env, enabled)
}

/// V0.5.0 F93b + F94 — same as [`render_project_settings`] but
/// renders the agent-team template (with `TeammateIdle` /
/// `TaskCreated` / `TaskCompleted` hooks). Used by
/// `ccteam init --mode agent-team`.
pub fn render_project_settings_agent_team(
    ccteam_bin: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    render_settings_template(
        PROJECT_SETTINGS_AGENT_TEAM_JSON,
        ccteam_bin,
        extra_env,
        enabled,
    )
}

/// Shared implementation for [`render_project_settings`] +
/// [`render_project_settings_agent_team`]. Takes the raw template
/// `&str` so callers pick which placeholder text to substitute into.
fn render_settings_template(
    template: &str,
    ccteam_bin: &Path,
    extra_env: &SettingsEnv,
    enabled: &EnabledPluginsSetting,
) -> Result<String> {
    let bin = ccteam_bin.to_str().ok_or_else(|| {
        anyhow!(
            "ccteam binary path not valid UTF-8: {}",
            ccteam_bin.display()
        )
    })?;
    if bin.contains('"') || bin.contains('\\') {
        return Err(anyhow!(
            "ccteam binary path contains characters that can't be embedded in settings.json: {bin}"
        ));
    }
    let body = template.replace("__CCTEAM_BIN__", bin);
    let mut v: Value = serde_json::from_str(&body)
        .with_context(|| format!("rendered settings.json is not valid JSON (bin={bin})"))?;
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
    let bin = current_ccteam_bin()?;
    let extra = SettingsEnv {
        ccteam_home: std::env::var("CCTEAM_HOME").ok(),
        ccteam_projects_root: std::env::var("CCTEAM_PROJECTS_ROOT").ok(),
    };
    let body = match kind {
        ProjectSettingsKind::ArtifactDriven => render_project_settings(&bin, &extra, enabled)?,
        ProjectSettingsKind::AgentTeam => {
            render_project_settings_agent_team(&bin, &extra, enabled)?
        }
    };
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
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
            Path::new("/usr/local/bin/ccteam"),
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
    fn template_session_start_uses_absolute_ccteam_path() {
        // V0.4.6 F89: hook commands now route through the `internal`
        // subcommand. Old top-level `ccteam hook …` stays accepted at
        // dispatch time (with stderr WARN), but freshly rendered
        // settings.json uses the new path.
        let body = render_project_settings(
            Path::new("/usr/local/bin/ccteam"),
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
                "/usr/local/bin/ccteam internal hook load-context",
                "/usr/local/bin/ccteam internal hook progress-append session_start"
            ],
        );
    }

    #[test]
    fn raw_template_uses_placeholder_not_bare_ccteam() {
        assert!(
            PROJECT_SETTINGS_JSON.contains("__CCTEAM_BIN__"),
            "template should reference __CCTEAM_BIN__ placeholder",
        );
        // V0.4.6 F89: template wires the new `internal hook` path. The
        // old bare `ccteam hook` form must not survive the migration.
        assert!(
            !PROJECT_SETTINGS_JSON.contains("\"ccteam hook"),
            "template should not embed bare `ccteam hook` — see render_project_settings",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains(" ccteam hook"),
            "template must use new `ccteam internal hook` form (V0.4.6 F89)",
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
            Path::new("/tmp/has\"quote/ccteam"),
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
            Path::new("/usr/local/bin/ccteam"),
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
            Path::new("/usr/local/bin/ccteam"),
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
            Path::new("/usr/local/bin/ccteam"),
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
            Path::new("/usr/local/bin/ccteam"),
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
