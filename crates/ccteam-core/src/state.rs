//! `state.json` — project-level orchestrator state. Schema in
//! `docs/interfaces.md` §2.1; semantics (phase_state machine, parallelism
//! tiers) in `docs/tech-design.md` §3.2 / §3.3.
//!
//! Persistence guarantees:
//! - **Atomic write**: serialize → write `<path>.tmp` → rename to `<path>`.
//!   POSIX `rename(2)` makes the new contents observable atomically; readers
//!   never see a half-written file.
//! - **One-deep backup**: before each save, the prior `<path>` is rotated to
//!   `<path>.bak` via rename. Crash between rotation and rename leaves the
//!   prior state recoverable from `.bak`; load falls back automatically.
//! - **Strict deserialize**: enums (`phase_state`, `parallelism`) reject
//!   unknown values; unknown top-level fields are tolerated for
//!   forward-compat when a future ccteam adds optional metadata.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    /// Phase prompt injected; awaiting `phase_done` or `escalate` event.
    InFlight,
    /// Last phase finished; orchestrator may inject the next.
    Idle,
    /// Stop hook owns the loop (ralph-loop fix-cycle pattern, §3.5).
    FixLocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parallelism {
    /// One claude session per project; M0 only.
    Solo,
    /// Phase-internal multi-role agent team; M2+.
    AgentTeam,
    /// Project-level fan-out across sub-modules; M3+.
    MultiSession,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseHistoryEntry {
    pub phase: String,
    pub status: String,
    pub duration_s: u64,
    pub cost_usd: f64,
}

/// Project-level state, persisted as `~/projects/<slug>/.ccteam/state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectState {
    pub slug: String,
    pub created_at: DateTime<Utc>,
    pub tmux_session: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<i32>,
    pub phase_state: PhaseState,
    pub current_phase: String,
    pub parallelism: Parallelism,
    pub phase_history: Vec<PhaseHistoryEntry>,
    pub fix_cycle_count: u32,
    pub cost_used_usd: f64,
    pub soft_warn_threshold_usd: f64,
    pub hard_kill_threshold_usd: f64,
    pub context_tokens_used: u64,
    pub context_reset_threshold_tokens: u64,
    pub context_reset_count: u32,
    pub last_progress_event_at: Option<DateTime<Utc>>,
    pub last_event_type: Option<String>,
    pub last_user_interaction_at: DateTime<Utc>,
    pub user_attached: bool,
    pub user_pause_pending: bool,
}

impl ProjectState {
    /// Atomically persist to `path`. Rotates any existing file to `<path>.bak`
    /// before writing, so a load can recover the prior state if a crash
    /// interleaves with this call.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("serialize ProjectState")?;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create parent dir {}", parent.display()))?;
            }
        }

        if path.exists() {
            let bak = with_appended_extension(path, ".bak");
            std::fs::rename(path, &bak)
                .with_context(|| format!("rotate {} → {}", path.display(), bak.display()))?;
        }

        let tmp = with_appended_extension(path, ".tmp");
        std::fs::write(&tmp, json.as_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;

        Ok(())
    }

    /// Load from `path`. On failure (missing or unparseable) automatically
    /// retries `<path>.bak`; returns the original error if that also fails.
    pub fn load(path: &Path) -> Result<Self> {
        match read_and_parse(path) {
            Ok(state) => Ok(state),
            Err(primary) => {
                let bak = with_appended_extension(path, ".bak");
                if !bak.exists() {
                    return Err(primary);
                }
                tracing::warn!(
                    path = %path.display(),
                    backup = %bak.display(),
                    error = %primary,
                    "state.json unreadable; recovering from .bak",
                );
                read_and_parse(&bak).with_context(|| {
                    format!(
                        "primary load failed ({primary:#}); backup load also failed",
                    )
                })
            }
        }
    }
}

fn read_and_parse(path: &Path) -> Result<ProjectState> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", path.display()))
}

/// Append a literal suffix to a path's filename (e.g. `state.json` →
/// `state.json.bak`). Unlike `Path::with_extension` this preserves the
/// existing extension instead of replacing it.
fn with_appended_extension(path: &Path, suffix: &str) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}
