//! Pure argv builder for `opencode acp`.
//!
//! Red lines (dev-plan L2/L15):
//! - No `--model` / `--agent` (those belong to `opencode run`).
//! - No persona / system-prompt injection (first ship is roleless).
//! - cwd is the process cwd (optional `--cwd` is redundant).
//! - Skip → `--auto`; Hitl omits it (mirrors grok's `--always-approve`).

use crate::PermissionMode;

/// Inputs for [`build_argv`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OpencodeSpawnInput {
    /// Drives the process-level auto-approve flag. Skip → `--auto`.
    pub permission_mode: PermissionMode,
}

/// Resolve the opencode binary: `CCTEAM_OPENCODE_BIN` else `"opencode"`.
pub fn opencode_bin() -> String {
    std::env::var(crate::OPENCODE_BIN_ENV).unwrap_or_else(|_| "opencode".to_string())
}

/// Build argv: `opencode acp [--auto]`.
///
/// **Why a process-level flag when ACP already auto-allows.** The in-band
/// `AutoAllowPermission` policy can only answer permission requests that reach
/// the wire as `session/request_permission`. OpenCode's own sub-sessions
/// (`task` / `@explore`) resolve permissions INTERNALLY: an `external_directory`
/// ask from a sub-session never becomes an ACP frame, so there is nobody to
/// answer it and the parent's `task` tool stays `running` forever — a session
/// that looks alive and is永久 stuck (2026-08-09: s279, 36 minutes, zero
/// auto-allow frames in the daemon while the sub-session sat on `asking`).
///
/// `--auto` approves at the process level, which covers the sub-sessions the
/// wire never shows us. The ACP policy stays as the second line for asks that
/// DO surface. Hitl gets neither — it must never silently allow.
pub fn build_argv(bin: &str, input: &OpencodeSpawnInput) -> Vec<String> {
    let mut argv = vec![bin.to_string(), "acp".into()];
    if !input.permission_mode.is_hitl() {
        argv.push("--auto".into());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_auto_approves_at_the_process_level() {
        // The sub-session hang fix: in-band ACP auto-allow cannot reach an ask
        // that never leaves the opencode process.
        assert_eq!(
            build_argv(
                "opencode",
                &OpencodeSpawnInput {
                    permission_mode: PermissionMode::Skip,
                }
            ),
            vec!["opencode", "acp", "--auto"]
        );
        // Default is skip.
        assert_eq!(
            build_argv("opencode", &OpencodeSpawnInput::default()),
            vec!["opencode", "acp", "--auto"]
        );
    }

    #[test]
    fn hitl_never_auto_approves() {
        let argv = build_argv(
            "opencode",
            &OpencodeSpawnInput {
                permission_mode: PermissionMode::Hitl,
            },
        );
        assert_eq!(argv, vec!["opencode", "acp"]);
        assert!(
            !argv.iter().any(|a| a == "--auto"),
            "hitl must never silently allow"
        );
    }

    #[test]
    fn never_emits_model_or_system_prompt() {
        let argv = build_argv("/bin/opencode", &OpencodeSpawnInput::default());
        assert!(!argv
            .iter()
            .any(|a| a.contains("model") || a.contains("system")));
        assert!(!argv.iter().any(|a| a == "--agent" || a == "--yolo"));
    }
}
