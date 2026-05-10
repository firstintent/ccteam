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

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::team::{HarnessKind, TeamKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    /// Phase prompt injected; awaiting `phase_done` or `escalate` event.
    InFlight,
    /// Last phase finished; orchestrator may inject the next.
    Idle,
    /// Stop hook owns the loop (ralph-loop pattern, §3.5).
    /// `alias = "fix_locked"` keeps pre-rename state.json files (F5/F6
    /// rename, 2026-05-06) loadable; new writes use `auto_locked`.
    #[serde(alias = "fix_locked")]
    AutoLocked,
    /// **M3.6**: phase produced its required outputs but flagged some
    /// sub-tasks as deferred (`ESCALATE: PHASE_DONE_PENDING`,
    /// interfaces §4.1.1). `open_decisions` lists outbox-file basenames
    /// the phase wrote that still need user attention. The orchestrator
    /// advances to the next phase only when its `required_inputs` does
    /// not overlap `open_decisions`; otherwise it writes an escalation
    /// and stays in `DonePending` until the user runs `ccteam resume`.
    ///
    /// Dropping `Copy` from `PhaseState` for this variant (Vec field)
    /// — every call site uses `matches!` or moves, so this isn't
    /// observable except in a couple of CLI render arms (handled with
    /// an explicit pattern there).
    DonePending { open_decisions: Vec<String> },
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

/// V0.3.1 F49 — one registered harness session in a flex project's
/// master `state.json::sessions` map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub harness: HarnessKind,
    pub tmux_session: String,
    pub started_at: DateTime<Utc>,
    pub pid: Option<u32>,
}

/// Project-level state, persisted as `~/projects/<slug>/.ccteam/state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectState {
    pub slug: String,
    /// Team this project runs under — selects which phase template
    /// set the orchestrator uses (M3.1 F13). serde-default `"dev"`
    /// keeps state.json files written before M3.1 loadable as the
    /// dev team, no migration script needed.
    #[serde(default = "default_team")]
    pub team: String,
    /// V0.3.1 F49 — cached team kind for hooks and lightweight readers.
    /// Old state files omit it and deserialize as workflow.
    #[serde(default, skip_serializing_if = "is_default_team_kind")]
    pub team_kind: TeamKind,
    pub created_at: DateTime<Utc>,
    pub tmux_session: String,
    pub claude_session_id: Option<String>,
    pub claude_pid: Option<i32>,
    pub phase_state: PhaseState,
    pub current_phase: String,
    pub parallelism: Parallelism,
    pub phase_history: Vec<PhaseHistoryEntry>,
    /// `alias = "fix_cycle_count"` keeps pre-rename state.json (F7,
    /// 2026-05-06) loadable; new writes use `auto_loop_cycle_count`.
    #[serde(alias = "fix_cycle_count")]
    pub auto_loop_cycle_count: u32,
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
    /// V0.3.1 F49 — flex-only session registry. Empty for workflow /
    /// multi_workflow projects and skipped on serialize for old-shape
    /// compatibility.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<String, SessionRecord>,
    /// Next sid sequence per harness. Values are monotonic and not
    /// decremented on `session rm`, so removed sids are never reused.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub next_sid_seq: BTreeMap<HarnessKind, u64>,
}

fn default_team() -> String {
    "dev".into()
}

fn is_default_team_kind(kind: &TeamKind) -> bool {
    *kind == TeamKind::Workflow
}

impl ProjectState {
    /// Default initial state for a freshly-created project. `current_phase`
    /// is left empty so the orchestrator's first tick reads it as
    /// "no phase yet" and dispatches the DAG entry node.
    pub fn initial(slug: String) -> Self {
        Self::initial_for_team(slug, default_team())
    }

    /// Like `initial` but lets the caller pin the team. Used by
    /// `bootstrap_project` so `ccteam new --team <name>` carries
    /// through to state.json.
    pub fn initial_for_team(slug: String, team: String) -> Self {
        let now = Utc::now();
        Self {
            tmux_session: format!("ccteam-{slug}"),
            slug,
            team,
            team_kind: TeamKind::Workflow,
            created_at: now,
            claude_session_id: None,
            claude_pid: None,
            phase_state: PhaseState::Idle,
            current_phase: String::new(),
            parallelism: Parallelism::Solo,
            phase_history: Vec::new(),
            auto_loop_cycle_count: 0,
            cost_used_usd: 0.0,
            soft_warn_threshold_usd: 20.0,
            hard_kill_threshold_usd: 200.0,
            context_tokens_used: 0,
            context_reset_threshold_tokens: 600_000,
            context_reset_count: 0,
            last_progress_event_at: None,
            last_event_type: None,
            last_user_interaction_at: now,
            user_attached: false,
            user_pause_pending: false,
            sessions: BTreeMap::new(),
            next_sid_seq: BTreeMap::new(),
        }
    }

    pub fn allocate_sid(&mut self, harness: HarnessKind) -> String {
        let next = self
            .next_sid_seq
            .get(&harness)
            .copied()
            .unwrap_or(1)
            .max(self.max_sid_number(harness).saturating_add(1))
            .max(1);
        self.next_sid_seq.insert(harness, next.saturating_add(1));
        format!("{}-{next}", harness_sid_prefix(harness))
    }

    pub fn reserve_sid(&mut self, harness: HarnessKind, sid: &str) {
        if let Some(n) = sid_number_for_harness(sid, harness) {
            let entry = self.next_sid_seq.entry(harness).or_insert(1);
            *entry = (*entry).max(n.saturating_add(1));
        }
    }

    fn max_sid_number(&self, harness: HarnessKind) -> u64 {
        self.sessions
            .keys()
            .filter_map(|sid| sid_number_for_harness(sid, harness))
            .max()
            .unwrap_or(0)
    }

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

pub fn harness_sid_prefix(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "claude",
        HarnessKind::Codex => "codex",
    }
}

fn sid_number_for_harness(sid: &str, harness: HarnessKind) -> Option<u64> {
    sid.strip_prefix(harness_sid_prefix(harness))
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|n| n.parse::<u64>().ok())
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
