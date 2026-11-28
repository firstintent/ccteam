//! Pure argv builder for `agy acp`.
//!
//! Red lines:
//! - Never emits a system-prompt surface. agy advertises
//!   `_meta.antigravity.systemPrompt.append` on `initialize`; that is a wire
//!   field ccteam does not send, and there is no argv twin to leak it.
//! - No model flag: `agy acp` has none; model / effort / mode are set after
//!   the handshake through agy's own ACP methods.
//! - No PTY / tmux flags; cwd is the process cwd.
//!
//! Every flag here was run against a live agy 1.5.18 (2026-11-27), not just
//! unit-tested for shape: an unknown flag makes `agy acp` print usage and
//! exit before `initialize` is answered, which kills every managed spawn.

use crate::PermissionMode;

/// Inputs for [`build_argv`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AgySpawnInput<'a> {
    /// Skip → `--dangerously-skip-permissions`; Hitl adds nothing (it must
    /// never silently allow).
    pub permission_mode: PermissionMode,
    /// Session role; empty = roleless. A non-empty role selects agy's own
    /// agent of that name (`--agent`), resolved by agy from its own files.
    pub role: &'a str,
}

/// Resolve the agy binary: `CCTEAM_AGY_BIN` else `"agy"`.
pub fn agy_bin() -> String {
    std::env::var(crate::AGY_BIN_ENV).unwrap_or_else(|_| "agy".to_string())
}

/// Build argv: `agy acp [--dangerously-skip-permissions] [--agent <role>]`.
pub fn build_argv(bin: &str, input: &AgySpawnInput<'_>) -> Vec<String> {
    let mut argv = vec![bin.to_string(), "acp".into()];
    if !input.permission_mode.is_hitl() {
        argv.push("--dangerously-skip-permissions".into());
    }
    let role = input.role.trim();
    if !role.is_empty() {
        argv.push("--agent".into());
        argv.push(role.to_string());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(permission_mode: PermissionMode, role: &str) -> AgySpawnInput<'_> {
        AgySpawnInput {
            permission_mode,
            role,
        }
    }

    #[test]
    fn skip_roleless_is_acp_plus_the_permission_switch() {
        assert_eq!(
            build_argv("agy", &input(PermissionMode::Skip, "")),
            vec!["agy", "acp", "--dangerously-skip-permissions"]
        );
    }

    #[test]
    fn hitl_never_passes_the_permission_switch() {
        assert_eq!(
            build_argv("agy", &input(PermissionMode::Hitl, "  ")),
            vec!["agy", "acp"]
        );
    }

    #[test]
    fn a_role_selects_agys_own_agent() {
        assert_eq!(
            build_argv("/bin/agy", &input(PermissionMode::Hitl, " reviewer ")),
            vec!["/bin/agy", "acp", "--agent", "reviewer"]
        );
    }

    #[test]
    fn never_emits_model_or_system_prompt_or_pty_flags() {
        for mode in [PermissionMode::Skip, PermissionMode::Hitl] {
            let argv = build_argv("agy", &input(mode, "reviewer"));
            assert!(!argv.iter().any(|a| a.contains("model")
                || a.contains("system")
                || a.contains("prompt")
                || a.contains("tmux")
                || a.contains("rmux")));
        }
    }
}
