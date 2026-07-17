//! Pure argv builder for `kimi acp`.
//!
//! Red lines:
//! - No model / persona / permission flags (kimi has no `--agent` face;
//!   roleless-only, model switches post-handshake via `session/set_model`).
//! - Never emits any system-prompt injection surface.
//! - cwd is the process cwd.

/// Inputs for [`build_argv`] (currently unused flags reserved for future).
#[derive(Debug, Clone, Copy, Default)]
pub struct KimiSpawnInput {
    // Intentionally empty: `kimi acp` takes no model/permission argv.
}

/// Resolve the kimi binary: `CCTEAM_KIMI_BIN` else `"kimi"`.
pub fn kimi_bin() -> String {
    std::env::var(crate::KIMI_BIN_ENV).unwrap_or_else(|_| "kimi".to_string())
}

/// Build argv: `kimi acp`.
pub fn build_argv(bin: &str, _input: &KimiSpawnInput) -> Vec<String> {
    vec![bin.to_string(), "acp".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_acp_only() {
        assert_eq!(
            build_argv("kimi", &KimiSpawnInput::default()),
            vec!["kimi", "acp"]
        );
    }

    #[test]
    fn never_emits_model_or_system_prompt() {
        let argv = build_argv("/bin/kimi", &KimiSpawnInput::default());
        assert!(!argv
            .iter()
            .any(|a| a.contains("model") || a.contains("system")));
        assert!(!argv.iter().any(|a| a == "--agent" || a == "--yolo"));
    }
}
