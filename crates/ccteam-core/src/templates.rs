//! Embedded ccteam templates rendered into produced-project trees by
//! `ccteam new` (M0.11). M0 ships only `settings.json`; the per-project
//! CLAUDE.md template lands in M0.10's context-bridging work.

use std::path::Path;

use anyhow::{Context, Result};

/// Per-project `.claude/settings.json` template. This is the universal
/// content — no slug or path substitution — installed into every
/// produced project's `.claude/` dir. Format matches `docs/interfaces.md`
/// §6.1; the only deviation in M0 is the omission of the
/// `PostToolUse(Bash:git push.*)` block-push matcher, which lands in
/// M1+ along with the matching `block-push` hook handler.
pub const PROJECT_SETTINGS_JSON: &str = include_str!("templates/settings.json");

/// Write `PROJECT_SETTINGS_JSON` into `<project_dir>/.claude/settings.json`.
/// Creates the parent dir if missing. Used by `ccteam new` (M0.11) and
/// can be re-run idempotently to refresh the template after a ccteam
/// upgrade.
pub fn write_project_settings(project_dir: &Path) -> Result<()> {
    let dir = project_dir.join(".claude");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("settings.json");
    std::fs::write(&path, PROJECT_SETTINGS_JSON)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_valid_json_with_expected_hook_keys() {
        let v: serde_json::Value =
            serde_json::from_str(PROJECT_SETTINGS_JSON).expect("template must be valid JSON");
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
    fn template_session_start_invokes_load_context_then_progress_append() {
        let v: serde_json::Value = serde_json::from_str(PROJECT_SETTINGS_JSON).unwrap();
        let entries = v["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap();
        let cmds: Vec<&str> = entries
            .iter()
            .map(|e| e["command"].as_str().unwrap())
            .collect();
        assert_eq!(
            cmds,
            vec![
                "ccteam hook load-context",
                "ccteam hook progress-append session_start"
            ],
        );
    }
}
