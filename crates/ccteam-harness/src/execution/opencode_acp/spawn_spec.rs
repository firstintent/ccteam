//! Pure argv/env builders for `opencode acp`.
//!
//! Red lines (dev-plan L2/L15):
//! - No `--model` / `--agent` (those belong to `opencode run`).
//! - No persona / system-prompt injection (first ship is roleless).
//! - cwd is the process cwd (optional `--cwd` is redundant).
//! - Skip → `OPENCODE_PERMISSION` allow-all env; Hitl sets nothing.

use crate::PermissionMode;

/// Inputs for [`permission_env`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OpencodeSpawnInput {
    /// Drives the process-level permission posture. Skip → allow-all env.
    pub permission_mode: PermissionMode,
}

/// Resolve the opencode binary: `CCTEAM_OPENCODE_BIN` else `"opencode"`.
pub fn opencode_bin() -> String {
    std::env::var(crate::OPENCODE_BIN_ENV).unwrap_or_else(|_| "opencode".to_string())
}

/// Build argv: `opencode acp` — exactly that, no flags.
///
/// `opencode acp` accepts NO permission flag at any version (its options are
/// `--print-logs/--log-level/--pure/--port/--hostname/--mdns/--cors/--cwd`).
/// A `--auto` flag shipped here once (2026-08-09) on the strength of
/// argv-shape unit tests alone; the real binary exits 1 with a usage dump, the
/// pipes close before `initialize` is answered, and EVERY managed opencode
/// spawn died as `jsonrpc peer closed`. Permission posture is an ENV fact for
/// this vendor — see [`permission_env`].
pub fn build_argv(bin: &str) -> Vec<String> {
    vec![bin.to_string(), "acp".into()]
}

/// Process-level permission posture, as child env.
///
/// **Why process-level when ACP already auto-allows.** The in-band
/// `AutoAllowPermission` policy can only answer permission requests that reach
/// the wire as `session/request_permission`. OpenCode's own sub-sessions
/// (`task` / `@explore`) resolve permissions INTERNALLY: an
/// `external_directory` ask from a sub-session never becomes an ACP frame, so
/// there is nobody to answer it and the parent's `task` tool stays `running`
/// forever (2026-08-09: s279, 36 minutes, zero auto-allow frames while the
/// sub-session sat on `asking`).
///
/// `OPENCODE_PERMISSION` is opencode's own seam for exactly this: a JSON
/// object merged over the config's `permission` table (verified in the 1.18.16
/// bundle), covering the asks the wire never shows us. The ACP policy stays as
/// the second line for asks that DO surface. Hitl sets nothing — it must never
/// silently allow.
pub fn permission_env(input: &OpencodeSpawnInput) -> Vec<(String, String)> {
    if input.permission_mode.is_hitl() {
        return Vec::new();
    }
    vec![(
        "OPENCODE_PERMISSION".to_string(),
        r#"{"edit":"allow","bash":"allow","webfetch":"allow","external_directory":"allow"}"#
            .to_string(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_is_bare_acp_because_the_binary_accepts_no_permission_flag() {
        // The regression this locks out: `--auto` does not exist, opencode
        // exits 1 on it, and every managed spawn died as "jsonrpc peer
        // closed" before `initialize` was ever answered.
        assert_eq!(build_argv("opencode"), vec!["opencode", "acp"]);
        assert_eq!(build_argv("/bin/opencode"), vec!["/bin/opencode", "acp"]);
    }

    #[test]
    fn skip_auto_approves_via_the_vendors_own_env_seam() {
        // The sub-session hang fix (s279): in-band ACP auto-allow cannot
        // reach an ask that never leaves the opencode process.
        let envs = permission_env(&OpencodeSpawnInput {
            permission_mode: PermissionMode::Skip,
        });
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].0, "OPENCODE_PERMISSION");
        let parsed: serde_json::Value = serde_json::from_str(&envs[0].1).expect("valid JSON");
        assert_eq!(parsed["external_directory"], "allow");
        assert_eq!(parsed["bash"], "allow");
        // Default is skip.
        assert_eq!(permission_env(&OpencodeSpawnInput::default()).len(), 1);
    }

    #[test]
    fn hitl_never_auto_approves() {
        assert!(
            permission_env(&OpencodeSpawnInput {
                permission_mode: PermissionMode::Hitl,
            })
            .is_empty(),
            "hitl must never silently allow"
        );
    }

    #[test]
    fn never_emits_model_or_system_prompt() {
        let argv = build_argv("/bin/opencode");
        assert!(!argv
            .iter()
            .any(|a| a.contains("model") || a.contains("system")));
        assert!(!argv.iter().any(|a| a == "--agent" || a == "--yolo"));
    }
}
