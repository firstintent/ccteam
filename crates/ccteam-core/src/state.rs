//! `state.json` — project-level orchestrator state. Schema reference in
//! `docs/interfaces.md` §2.1.
//!
//! V0.4.0 F60: the pre-F60 phase state machine (PhaseState variants
//! InFlight / DonePending / AutoLocked) is gone — F66 reintroduces an
//! `agent_sessions` shape against `workflow.yaml`. Until then we keep
//! the identity / cost / lifecycle fields plus the V0.3.1 F49 flex
//! session registry. `current_phase`, `phase_history`, and
//! `last_event_type` survive as **serde-only compat fields** with
//! `skip_serializing_if` so fresh writes drop them but old state.json
//! files load unchanged (the F66 workflow loop won't read these — it
//! tracks dispatch on the new shape).
//!
//! Persistence guarantees:
//! - **Atomic write**: serialize → write `<path>.tmp` → rename to `<path>`.
//!   POSIX `rename(2)` makes the new contents observable atomically.
//! - **One-deep backup**: before each save, the prior `<path>` is rotated to
//!   `<path>.bak` via rename. Load falls back automatically on parse failure.
//! - **Strict deserialize**: enums (`phase_state`, `parallelism`) reject
//!   unknown values.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::team::{HarnessKind, TeamKind};

/// V0.4.0 F60 — only `Idle` and `Done` survive the phase-machine purge.
/// `Idle` keeps existing tests / hook code that reads `state.phase_state`
/// loadable; `Done` is reserved for F66 workflow completion.
///
/// `alias = "in_flight" / "done_pending" / "auto_locked" / "fix_locked"`
/// lets pre-F60 state.json files still load (every legacy variant is
/// coerced to `Idle` on read — the F66 loop will re-evaluate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseState {
    /// Project is alive but not currently driving any phase. The F66
    /// workflow loop will subsume the dispatch logic; the variant
    /// remains so old `state.json` files load without migration.
    #[serde(
        alias = "in_flight",
        alias = "done_pending",
        alias = "auto_locked",
        alias = "fix_locked"
    )]
    Idle,
    /// Project terminated successfully. The F66 workflow loop will
    /// transition to this when every gated artifact resolves.
    Done,
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

/// V0.4.0 F60 — preserved as a serde-only compat type so old
/// state.json files load. F66 will replace phase tracking with
/// workflow agent-session tracking; until then this struct is
/// inhabitable but unused by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseHistoryEntry {
    pub phase: String,
    pub status: String,
    pub duration_s: u64,
    pub cost_usd: f64,
}

/// V0.3.1 F49 — one registered harness session in a flex project's
/// master `state.json::sessions` map.
///
/// V0.4.0 F61 added `job_id` for the Claude Code `--bg` background-job
/// id. Codex rows leave it `None`; old state.json files written before
/// F61 also deserialize with `None` (serde default), keeping the
/// upgrade path migration-free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub harness: HarnessKind,
    pub tmux_session: String,
    pub started_at: DateTime<Utc>,
    pub pid: Option<u32>,
    /// V0.4.0 F61 — Claude Code background-job id. `None` for codex
    /// sessions and for legacy rows written before F61.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

/// Project-level state, persisted as `~/projects/<slug>/.ccteam/state.json`.
///
/// V0.4.0 F60: phase state machine fields removed. The F66 workflow
/// loop reintroduces dispatch tracking on a fresh shape; until then
/// only identity / lifecycle / cost / context-budget fields remain,
/// plus the V0.3.1 F49 flex `sessions` registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectState {
    pub slug: String,
    /// Team this project runs under. serde-default `"dev"` keeps state.json
    /// files written before M3.1 loadable as the dev team, no migration
    /// script needed.
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
    /// Coarse-grained lifecycle state. F60 collapsed this to
    /// `Idle` / `Done`; F66 will either extend the enum or replace
    /// it with workflow-aware tracking.
    #[serde(default = "default_phase_state")]
    pub phase_state: PhaseState,
    /// V0.4.0 F60 retained for serde-compat: old state.json files
    /// recorded a `parallelism` field. Default `Solo` keeps loading
    /// without migration; nothing currently reads it.
    #[serde(default = "default_parallelism")]
    pub parallelism: Parallelism,
    /// V0.4.0 F60 compat fields — retained so old state.json files
    /// load and CLI/test sites that still read these strings keep
    /// compiling. F66 wires the new dispatch loop to a fresh set of
    /// agent-session fields; until then writes default-skip these so
    /// fresh state.json files don't propagate the legacy shape.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub current_phase: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_history: Vec<PhaseHistoryEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_type: Option<String>,
    /// `alias = "fix_cycle_count"` keeps pre-rename state.json (F7,
    /// 2026-05-06) loadable; new writes use `auto_loop_cycle_count`.
    #[serde(
        default,
        alias = "fix_cycle_count",
        skip_serializing_if = "is_zero_u32"
    )]
    pub auto_loop_cycle_count: u32,
    /// V0.4.6 F91 — frozen cost accumulator. Pre-F91 this was bumped
    /// by `Hook::CostAccumulate` (PostToolUse) and the F80 orchestrator
    /// cleanup. F91 retired both write paths; the new SoT is
    /// `cost_summary(slug, &progress_path, &paths)` which reads
    /// `progress.jsonl::agent_done::cost_usd` (historical) plus each
    /// open spawn's `~/.claude/jobs/<id>/state.json::cost_usd_total`
    /// (live). The field stays for serde compat — `#[serde(default)]`
    /// lets old state.json files load — but new writes never mutate it,
    /// so values frozen at the moment of F91 ship will linger until
    /// V0.5 drops the field entirely. Do **NOT** read this for cost in
    /// new code; use [`crate::CostSummary`] / `cost_summary` instead.
    #[deprecated(
        note = "V0.4.6 F91: replaced by `cost_summary(...)`; field frozen pending V0.5 removal"
    )]
    #[serde(default)]
    pub cost_used_usd: f64,
    pub soft_warn_threshold_usd: f64,
    pub hard_kill_threshold_usd: f64,
    pub context_tokens_used: u64,
    pub context_reset_threshold_tokens: u64,
    pub context_reset_count: u32,
    pub last_progress_event_at: Option<DateTime<Utc>>,
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
    /// V0.5.0 F97 — agent-team mode marker for `cleanup_on_stop:
    /// leave-running`. When `true`, `ccteam stop <slug>` dropped the
    /// ccteam-side watch but left the lead bg job alive; a subsequent
    /// `ccteam start <slug>` (without `--restart-team`) must refuse to
    /// avoid spawning a second lead while the first is still running.
    /// `#[serde(default)]` keeps old state.json files loadable.
    /// Skipped on serialize when `false` (the default) so V0.4.6
    /// state.json files don't accumulate the field unnecessarily.
    #[serde(default, skip_serializing_if = "is_false")]
    pub detached: bool,
}

fn default_team() -> String {
    "dev".into()
}

fn default_phase_state() -> PhaseState {
    PhaseState::Idle
}

fn default_parallelism() -> Parallelism {
    Parallelism::Solo
}

fn is_default_team_kind(kind: &TeamKind) -> bool {
    *kind == TeamKind::Workflow
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ProjectState {
    /// Default initial state for a freshly-created project.
    pub fn initial(slug: String) -> Self {
        Self::initial_for_team(slug, default_team())
    }

    /// Like `initial` but lets the caller pin the team. Used by
    /// `bootstrap_project` so `ccteam new --team <name>` carries
    /// through to state.json.
    pub fn initial_for_team(slug: String, team: String) -> Self {
        let now = Utc::now();
        // V0.4.6 F91 — `cost_used_usd` is deprecated but still required
        // by the struct literal. We initialize it to 0.0 (never bumped
        // post-F91) and silence the deprecation warning at the single
        // initialization site; readers go through `cost_summary` per
        // the deprecation note.
        #[allow(deprecated)]
        Self {
            tmux_session: format!("ccteam-{slug}"),
            slug,
            team,
            team_kind: TeamKind::Workflow,
            created_at: now,
            claude_session_id: None,
            claude_pid: None,
            phase_state: PhaseState::Idle,
            parallelism: Parallelism::Solo,
            current_phase: String::new(),
            phase_history: Vec::new(),
            last_event_type: None,
            auto_loop_cycle_count: 0,
            cost_used_usd: 0.0,
            soft_warn_threshold_usd: 20.0,
            hard_kill_threshold_usd: 200.0,
            context_tokens_used: 0,
            context_reset_threshold_tokens: 600_000,
            context_reset_count: 0,
            last_progress_event_at: None,
            last_user_interaction_at: now,
            user_attached: false,
            user_pause_pending: false,
            sessions: BTreeMap::new(),
            next_sid_seq: BTreeMap::new(),
            detached: false,
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
        let json = serde_json::to_string_pretty(self).context("serialize ProjectState")?;

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
                    format!("primary load failed ({primary:#}); backup load also failed",)
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
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Append a literal suffix to a path's filename (e.g. `state.json` →
/// `state.json.bak`). Unlike `Path::with_extension` this preserves the
/// existing extension instead of replacing it.
fn with_appended_extension(path: &Path, suffix: &str) -> PathBuf {
    let mut s: OsString = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}
