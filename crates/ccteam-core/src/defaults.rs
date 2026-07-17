//! Cross-crate primitive defaults.
//!
//! Keep values here when they are consumed outside the engine modules
//! that originally introduced them.

use std::path::PathBuf;

/// Default sid for flex projects that haven't opted into multi-session
/// project layout.
pub const DEFAULT_CLAUDE_SID: &str = "claude-1";

/// Environment override for the directory under which Claude Code
/// writes per-job `state.json` files. Defaults to `~/.claude/jobs/` when
/// unset. Tests override this to a tempdir.
pub const CLAUDE_JOBS_DIR_ENV: &str = "CCTEAM_CLAUDE_JOBS_DIR";

/// Environment override for the `claude` binary path. Tests set this to
/// fake scripts so execution paths stay hermetic.
pub const CLAUDE_BIN_ENV: &str = "CCTEAM_CLAUDE_BIN";

/// Environment override for the `codex` binary path. Tests set this to
/// fake scripts for `codex exec --json` and app-server probes.
pub const CODEX_BIN_ENV: &str = "CCTEAM_CODEX_BIN";

/// Environment override for the `grok` binary path. Tests set this to
/// a fake ACP stdio script so harness tests stay hermetic.
pub const GROK_BIN_ENV: &str = "CCTEAM_GROK_BIN";
pub const OPENCODE_BIN_ENV: &str = "CCTEAM_OPENCODE_BIN";

/// Environment override for the `kimi` binary path. Tests set this to
/// a fake ACP stdio script so harness tests stay hermetic.
pub const KIMI_BIN_ENV: &str = "CCTEAM_KIMI_BIN";

/// V0.6.8 F195 — per-turn watchdog default (seconds).
///
/// 90s leaves enough headroom for normal multi-tool turns to finish
/// without triggering the "still working" notice, while keeping the
/// silent-stall feedback loop tight enough that a stuck Stop hook /
/// tail loop / claude hang doesn't go unsurfaced for minutes.
pub const DEFAULT_TURN_TIMEOUT_SECS: u32 = 90;

/// Resolve the Claude Code jobs directory, honoring
/// [`CLAUDE_JOBS_DIR_ENV`] for hermetic tests.
pub fn claude_jobs_dir_from_env() -> PathBuf {
    std::env::var_os(CLAUDE_JOBS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".claude")
                .join("jobs")
        })
}

/// Resolve the absolute path to `state.json` for a Claude Code
/// background job.
pub fn state_json_path(job_id: &str) -> PathBuf {
    claude_jobs_dir_from_env().join(job_id).join("state.json")
}
