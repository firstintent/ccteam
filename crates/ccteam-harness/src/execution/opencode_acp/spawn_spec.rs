//! Pure argv builder for `opencode acp`.
//!
//! Red lines (dev-plan L2/L15):
//! - No `--model` / `--agent` / permission flags (those belong to `opencode run`).
//! - No persona / system-prompt injection (first ship is roleless).
//! - cwd is the process cwd (optional `--cwd` is redundant).

/// Inputs for [`build_argv`] (currently unused flags reserved for future).
#[derive(Debug, Clone, Copy, Default)]
pub struct OpencodeSpawnInput {
    // Intentionally empty: opencode acp takes no model/permission argv.
}

/// Resolve the opencode binary: `CCTEAM_OPENCODE_BIN` else `"opencode"`.
pub fn opencode_bin() -> String {
    std::env::var(crate::OPENCODE_BIN_ENV).unwrap_or_else(|_| "opencode".to_string())
}

/// Build argv: `opencode acp`.
pub fn build_argv(bin: &str, _input: &OpencodeSpawnInput) -> Vec<String> {
    vec![bin.to_string(), "acp".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_acp_only() {
        assert_eq!(
            build_argv("opencode", &OpencodeSpawnInput::default()),
            vec!["opencode", "acp"]
        );
    }

    #[test]
    fn never_emits_model_or_system_prompt() {
        let argv = build_argv("/bin/opencode", &OpencodeSpawnInput::default());
        assert!(!argv
            .iter()
            .any(|a| a.contains("model") || a.contains("system")));
        assert!(!argv
            .iter()
            .any(|a| a == "--agent" || a == "--auto" || a == "--yolo"));
    }
}
