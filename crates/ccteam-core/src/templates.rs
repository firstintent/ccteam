//! Embedded ccteam templates rendered into produced-project trees by
//! `ccteam new` and the global `~/.ccteam/` skeleton produced by
//! `ccteam init`. The settings.json source uses the placeholder
//! `__CCTEAM_BIN__` for the binary path so we can rewrite hook commands
//! to absolute paths at install time (otherwise hook subprocesses
//! inherit Claude Code's PATH and silently fail to find `ccteam`).

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// Per-project `.claude/settings.json` template. The template uses
/// `__CCTEAM_BIN__` everywhere a real install would name `ccteam`; we
/// substitute the running binary's absolute path at write time so hook
/// subprocesses don't depend on the user's PATH (which Claude Code
/// inherits and may not include the ccteam install dir).
pub const PROJECT_SETTINGS_JSON: &str = include_str!("templates/settings.json");

/// Phase template payloads — `(global_filename_with_index_prefix, body)`.
/// Project-local copies use the unprefixed name (e.g. `plan-eng.md`)
/// because the phase prompt in `progress::build_phase_prompt` references
/// `@.ccteam/phases/<phase>.md` without the numeric prefix.
pub const PHASE_TEMPLATES: &[(&str, &str)] = &[
    ("02-plan-eng.md", include_str!("../../../phases/02-plan-eng.md")),
    ("03-implement.md", include_str!("../../../phases/03-implement.md")),
    ("04-test-author.md", include_str!("../../../phases/04-test-author.md")),
    ("05-test-run.md", include_str!("../../../phases/05-test-run.md")),
    ("06-fix.md", include_str!("../../../phases/06-fix.md")),
    ("09-ship.md", include_str!("../../../phases/09-ship.md")),
];

/// Strip a leading `NN-` index prefix off a global phase filename so it
/// matches the path the phase prompt asks claude to read
/// (`@.ccteam/phases/<phase>.md`). Returns the original name unchanged
/// when no prefix is present.
pub fn project_phase_filename(global: &str) -> &str {
    let bytes = global.as_bytes();
    if bytes.len() > 3
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b'-'
    {
        &global[3..]
    } else {
        global
    }
}

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

/// Render `PROJECT_SETTINGS_JSON` with `__CCTEAM_BIN__` replaced by the
/// given absolute binary path and `extra_env` merged into the top-level
/// `env` block. Validates that the rewritten body is still valid JSON
/// so a path with shell-hostile characters can't silently corrupt the
/// settings file.
pub fn render_project_settings(ccteam_bin: &Path, extra_env: &SettingsEnv) -> Result<String> {
    let bin = ccteam_bin
        .to_str()
        .ok_or_else(|| anyhow!("ccteam binary path not valid UTF-8: {}", ccteam_bin.display()))?;
    if bin.contains('"') || bin.contains('\\') {
        return Err(anyhow!(
            "ccteam binary path contains characters that can't be embedded in settings.json: {bin}"
        ));
    }
    let body = PROJECT_SETTINGS_JSON.replace("__CCTEAM_BIN__", bin);
    let mut v: Value = serde_json::from_str(&body)
        .with_context(|| format!("rendered settings.json is not valid JSON (bin={bin})"))?;
    if let Some(env) = v.get_mut("env").and_then(|e| e.as_object_mut()) {
        if let Some(home) = &extra_env.ccteam_home {
            env.insert("CCTEAM_HOME".into(), Value::String(home.clone()));
        }
        if let Some(proj) = &extra_env.ccteam_projects_root {
            env.insert(
                "CCTEAM_PROJECTS_ROOT".into(),
                Value::String(proj.clone()),
            );
        }
    }
    serde_json::to_string_pretty(&v).context("serialize rendered settings.json")
}

/// Write `<project_dir>/.claude/settings.json` with hook commands
/// pointing at the running ccteam binary by absolute path **and** the
/// effective `CCTEAM_HOME` / `CCTEAM_PROJECTS_ROOT` baked into `env` so
/// hook subprocesses don't depend on tmux env propagation. Creates the
/// parent dir if missing. Idempotent — overwrites any prior render so
/// re-running after a ccteam upgrade refreshes paths.
pub fn write_project_settings(project_dir: &Path) -> Result<()> {
    let dir = project_dir.join(".claude");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("settings.json");
    let bin = current_ccteam_bin()?;
    let extra = SettingsEnv {
        ccteam_home: std::env::var("CCTEAM_HOME").ok(),
        ccteam_projects_root: std::env::var("CCTEAM_PROJECTS_ROOT").ok(),
    };
    let body = render_project_settings(&bin, &extra)?;
    std::fs::write(&path, body)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Write each `PHASE_TEMPLATES` entry into `<project_dir>/.ccteam/phases/`
/// under its **unprefixed** name (e.g. `plan-eng.md`) so it matches the
/// path embedded in `progress::build_phase_prompt`. Idempotent; later
/// calls overwrite earlier copies.
pub fn write_project_phase_templates(project_dir: &Path) -> Result<()> {
    let dir = project_dir.join(".ccteam").join("phases");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    for (global, body) in PHASE_TEMPLATES {
        let name = project_phase_filename(global);
        let path = dir.join(name);
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Write each `PHASE_TEMPLATES` entry into `<global_dir>/phases/` under
/// its full prefixed name (e.g. `02-plan-eng.md`). Used by `ccteam init`
/// so the orchestrator can load + validate templates from `~/.ccteam/`.
/// `force == false` skips files already on disk so an operator can hand-
/// edit a global template and not lose it on re-init.
pub fn write_global_phase_templates(global_dir: &Path, force: bool) -> Result<()> {
    let dir = global_dir.join("phases");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    for (global, body) in PHASE_TEMPLATES {
        let path = dir.join(global);
        if path.exists() && !force {
            continue;
        }
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn template_is_valid_json_with_expected_hook_keys() {
        let body =
            render_project_settings(Path::new("/usr/local/bin/ccteam"), &SettingsEnv::default())
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
        let body =
            render_project_settings(Path::new("/usr/local/bin/ccteam"), &SettingsEnv::default())
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
                "/usr/local/bin/ccteam hook load-context",
                "/usr/local/bin/ccteam hook progress-append session_start"
            ],
        );
    }

    #[test]
    fn raw_template_uses_placeholder_not_bare_ccteam() {
        // Guard against accidentally re-introducing `"command": "ccteam …"`
        // — the un-substituted form makes hook subprocesses depend on PATH,
        // which Claude Code inherits from its parent and which often does
        // not include the ccteam install dir.
        assert!(
            PROJECT_SETTINGS_JSON.contains("__CCTEAM_BIN__"),
            "template should reference __CCTEAM_BIN__ placeholder",
        );
        assert!(
            !PROJECT_SETTINGS_JSON.contains("\"ccteam hook"),
            "template should not embed bare `ccteam hook` — see render_project_settings",
        );
    }

    #[test]
    fn project_phase_filename_strips_two_digit_prefix() {
        assert_eq!(project_phase_filename("02-plan-eng.md"), "plan-eng.md");
        assert_eq!(project_phase_filename("09-ship.md"), "ship.md");
        assert_eq!(project_phase_filename("plan-eng.md"), "plan-eng.md");
        assert_eq!(project_phase_filename("ab-foo.md"), "ab-foo.md"); // not digits
    }

    #[test]
    fn write_project_phase_templates_drops_index_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_project_phase_templates(tmp.path()).unwrap();
        let phases = tmp.path().join(".ccteam/phases");
        assert!(phases.join("plan-eng.md").exists());
        assert!(phases.join("ship.md").exists());
        assert!(!phases.join("02-plan-eng.md").exists());
    }

    #[test]
    fn write_global_phase_templates_keeps_index_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_global_phase_templates(tmp.path(), false).unwrap();
        let phases = tmp.path().join("phases");
        assert!(phases.join("02-plan-eng.md").exists());
        assert!(phases.join("09-ship.md").exists());
    }

    #[test]
    fn write_global_phase_templates_preserves_user_edits_without_force() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_global_phase_templates(tmp.path(), false).unwrap();
        let path = tmp.path().join("phases/02-plan-eng.md");
        std::fs::write(&path, "USER EDITED").unwrap();
        write_global_phase_templates(tmp.path(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER EDITED");
        write_global_phase_templates(tmp.path(), true).unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), "USER EDITED");
    }

    #[test]
    fn render_project_settings_rejects_quoted_path() {
        let err = render_project_settings(
            Path::new("/tmp/has\"quote/ccteam"),
            &SettingsEnv::default(),
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
        let body = render_project_settings(Path::new("/usr/local/bin/ccteam"), &env).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["env"]["CCTEAM_HOME"], "/tmp/sandbox/home");
        assert_eq!(v["env"]["CCTEAM_PROJECTS_ROOT"], "/tmp/sandbox/projects");
        // Existing env keys must survive the merge.
        assert_eq!(v["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"], "1");
    }

    #[test]
    fn render_project_settings_omits_env_when_default() {
        let body =
            render_project_settings(Path::new("/usr/local/bin/ccteam"), &SettingsEnv::default())
                .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["env"]["CCTEAM_HOME"].is_null());
    }
}
