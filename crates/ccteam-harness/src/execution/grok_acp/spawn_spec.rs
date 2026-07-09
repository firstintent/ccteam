//! Pure argv/env/cwd builder for `grok agent … stdio`.
//!
//! Red lines:
//! - Never emits `--system-prompt-override` (no prompt injection).
//! - All options come **before** the `stdio` mode token.
//! - Skip → `--always-approve`; Hitl omits it.

use crate::PermissionMode;

/// Inputs for [`build_argv`].
#[derive(Debug, Clone, Copy)]
pub struct GrokSpawnInput<'a> {
    pub permission_mode: PermissionMode,
    pub model_id: Option<&'a str>,
}

/// Resolve the grok binary: `CCTEAM_GROK_BIN` else `"grok"`.
pub fn grok_bin() -> String {
    std::env::var(crate::GROK_BIN_ENV).unwrap_or_else(|_| "grok".to_string())
}

/// Build argv: `grok agent [--always-approve] [-m MODEL] stdio`.
pub fn build_argv(bin: &str, input: &GrokSpawnInput<'_>) -> Vec<String> {
    let mut argv = vec![bin.to_string(), "agent".into()];
    if !input.permission_mode.is_hitl() {
        // Skip (default) → auto-approve tools; no session/request_permission.
        argv.push("--always-approve".into());
    }
    if let Some(model) = input.model_id.map(str::trim).filter(|m| !m.is_empty()) {
        argv.push("-m".into());
        argv.push(model.to_string());
    }
    // Mode last — `grok agent --help` requires options before the mode.
    argv.push("stdio".into());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_emits_always_approve_before_stdio() {
        let argv = build_argv(
            "grok",
            &GrokSpawnInput {
                permission_mode: PermissionMode::Skip,
                model_id: None,
            },
        );
        assert_eq!(argv, vec!["grok", "agent", "--always-approve", "stdio"]);
    }

    #[test]
    fn hitl_omits_always_approve() {
        let argv = build_argv(
            "grok",
            &GrokSpawnInput {
                permission_mode: PermissionMode::Hitl,
                model_id: Some("grok-4.5"),
            },
        );
        assert_eq!(argv, vec!["grok", "agent", "-m", "grok-4.5", "stdio"]);
        assert!(!argv.iter().any(|a| a == "--always-approve"));
    }

    #[test]
    fn never_emits_system_prompt_override() {
        let argv = build_argv(
            "grok",
            &GrokSpawnInput {
                permission_mode: PermissionMode::Skip,
                model_id: Some("x"),
            },
        );
        assert!(!argv.iter().any(|a| a.contains("system-prompt")));
    }
}
