//! V0.6.1 F98 — plan-approval ↔ outbox 联动 engine.
//!
//! ## Scope
//!
//! Pure state machine + parser. Inputs are filesystem snapshots (the
//! list of plan markdowns in `<project>/.ccteam/plans/`) and IM
//! decision strings (`APPROVE` / `REJECT` / `REJECT <reason>` /
//! `EDIT <comment>`). Outputs are a list of [`PlanEngineAction`]s the
//! caller dispatches:
//!
//! - `SendIm` — push the plan summary to the configured outbox.
//! - `EmitEvent` — append one of `plan_pending` / `plan_decision` /
//!   `plan_timeout` to `progress.jsonl`.
//! - `WriteDecisionFile` — atomic write of the decision body the
//!   agent picks up on resume (under
//!   `<project>/.ccteam/plan-decisions/<plan_id>.md`).
//! - `Escalate` — meta-agent ping for `on_timeout: escalate`.
//!
//! No tokio / no notify / no IM client calls live here; the
//! orchestrator (or a test harness) drives the engine on each tick
//! and the IM glue in `ccteam-im` translates the actions.
//!
//! ## Red lines (CLAUDE.md §三 + prd.md §F98)
//!
//! - `progress.jsonl` is the SoT — every state transition emits an
//!   `EmitEvent` action with one of the [`crate::progress`] constants.
//! - No prompt injection — the agent reads the decision file the
//!   engine writes; the engine never reaches into the tmux pane.
//! - 60 min default timeout matches the prd.md spec; the workflow.yaml
//!   `timeout_min: 0` disables the timeout entirely.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::progress;
use crate::workflow::{PlanApprovalOnTimeout, PlanApprovalSpec};

/// Plan markdowns live under `<project>/.ccteam/plans/`.
pub const PLANS_SUBDIR: &str = ".ccteam/plans";

/// Decision files the engine emits (one per plan; the agent reads on
/// resume).
pub const DECISIONS_SUBDIR: &str = ".ccteam/plan-decisions";

/// One plan-approval gate identifier. Derived from the plan file
/// basename (without the `.md` extension) so two plans the same agent
/// wrote at different times don't collide.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlanId(pub String);

impl PlanId {
    /// Parse a plan markdown filename. Returns `None` when the
    /// filename does not end in `.md` or has an empty stem.
    pub fn from_plan_filename(name: &str) -> Option<Self> {
        let stem = name.strip_suffix(".md")?;
        if stem.is_empty() {
            return None;
        }
        Some(Self(stem.to_string()))
    }

    /// Derive the per-plan decision file path under
    /// `<project>/.ccteam/plan-decisions/<plan_id>.md`.
    pub fn decision_path(&self, project_dir: &Path) -> PathBuf {
        project_dir
            .join(DECISIONS_SUBDIR)
            .join(format!("{}.md", self.0))
    }

    /// Borrow the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One user decision parsed from an IM reply text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanDecision {
    /// `APPROVE` (case-insensitive). The agent resumes with a
    /// machine-readable approval body.
    Approve,
    /// `REJECT` / `REJECT <reason>`. Optional free-text reason.
    Reject { reason: Option<String> },
    /// `EDIT <comment>`. Comment is the rest of the line after the
    /// `EDIT ` prefix.
    Edit { comment: String },
}

impl PlanDecision {
    /// Wire-format scalar used inside `progress.jsonl::plan_decision`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject { .. } => "reject",
            Self::Edit { .. } => "edit",
        }
    }

    /// Free-text trailer (REJECT reason / EDIT comment). `None` for
    /// `Approve` and bare `REJECT`.
    pub fn comment(&self) -> Option<&str> {
        match self {
            Self::Approve => None,
            Self::Reject { reason } => reason.as_deref(),
            Self::Edit { comment } => Some(comment),
        }
    }
}

/// Parse an IM reply text. Accepts:
///
/// - `APPROVE` / `approve` (case-insensitive, with optional trailing
///   whitespace).
/// - `REJECT` / `REJECT <reason>`.
/// - `EDIT <comment>` (comment is required — bare `EDIT` is rejected).
///
/// Returns `None` for anything else so the caller can distinguish a
/// plan-decision reply from a general chat turn.
pub fn parse_decision(text: &str) -> Option<PlanDecision> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Case-insensitive verb match on the first token; payload is the
    // rest of the line preserving original casing.
    let (verb, rest) = match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
        None => (trimmed, ""),
    };
    let verb_upper = verb.to_ascii_uppercase();
    match verb_upper.as_str() {
        "APPROVE" => Some(PlanDecision::Approve),
        "REJECT" => {
            let reason = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            Some(PlanDecision::Reject { reason })
        }
        "EDIT" => {
            if rest.is_empty() {
                None
            } else {
                Some(PlanDecision::Edit {
                    comment: rest.to_string(),
                })
            }
        }
        _ => None,
    }
}

/// Per-plan state tracked by the engine. Round-trippable via serde so
/// callers can persist to a sidecar `plan-state.json` if desired (the
/// engine itself does not write — progress.jsonl is the SoT).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlanRecordState {
    /// New plan file detected; engine has not yet sent the IM notice.
    /// `next` tick → emit `SendIm` + `EmitEvent(plan_pending)`.
    PendingNotify,
    /// IM notice sent at `notified_at`; awaiting decision until the
    /// configured timeout fires.
    Notified { notified_at: DateTime<Utc> },
    /// User decided. The engine records the decision but keeps the
    /// entry so duplicate-IM replies are idempotent (a second
    /// `APPROVE` for the same plan is a no-op).
    Decided {
        decision: PlanDecision,
        decided_at: DateTime<Utc>,
    },
    /// `timeout_min` elapsed without a user reply. The engine has
    /// already dispatched the `on_timeout` action (escalate / synthetic
    /// approve / synthetic reject); the entry stays for diagnostics.
    TimedOut,
}

/// One plan-approval entry tracked by the engine.
#[derive(Debug, Clone)]
pub struct PlanRecord {
    /// Plan id (= file stem under `.ccteam/plans/`).
    pub plan_id: PlanId,
    /// Absolute path of the plan markdown.
    pub plan_path: PathBuf,
    /// Agent role responsible for the plan. Derived by splitting the
    /// plan id on `-` (`<agent>-<ts>` convention); `unknown` when the
    /// id lacks a `-`.
    pub agent: String,
    /// First time the engine saw the plan file.
    pub created_at: DateTime<Utc>,
    /// Current state.
    pub state: PlanRecordState,
}

/// One action the caller must dispatch.
#[derive(Debug, Clone)]
pub enum PlanEngineAction {
    /// Push an IM message to the configured outbox. The caller is
    /// responsible for resolving `outbox` → `Channel` + recipient.
    SendIm {
        plan_id: PlanId,
        agent: String,
        outbox: String,
        /// Pre-formatted body (head -20 of plan + APPROVE/REJECT hint).
        body: String,
    },
    /// Append one event line to `progress.jsonl`.
    EmitEvent { value: Value },
    /// Atomic write of the decision file the agent picks up on
    /// resume. Body shape mirrors the inbox front-matter convention
    /// used by [`crate::actions::inject_decision`] but the engine
    /// stays free-form (V0.6.1 narrow scope).
    WriteDecisionFile {
        plan_id: PlanId,
        path: PathBuf,
        body: String,
    },
    /// Meta-agent escalation (`on_timeout: escalate`). The caller
    /// pushes the body to `<project>/.ccteam/inbox/<ts>-plan-timeout.md`
    /// or the meta-agent's preferred channel.
    Escalate {
        plan_id: PlanId,
        agent: String,
        body: String,
    },
}

/// Engine configuration — usually built from a single workflow.yaml
/// `plan_approval:` block plus the project root.
#[derive(Debug, Clone)]
pub struct PlanApprovalEngineConfig {
    /// Project root (the engine joins this with [`PLANS_SUBDIR`] etc.).
    pub project_dir: PathBuf,
    /// Outbox channel id from workflow.yaml (`telegram`, `slack`, ...).
    pub outbox: String,
    /// Approval window. `Duration::ZERO` disables the timeout.
    pub timeout: Duration,
    /// Policy on timeout.
    pub on_timeout: PlanApprovalOnTimeout,
}

impl PlanApprovalEngineConfig {
    /// Build from a workflow.yaml `plan_approval:` spec.
    pub fn from_spec(project_dir: PathBuf, spec: &PlanApprovalSpec) -> Self {
        Self {
            project_dir,
            outbox: spec.outbox.clone(),
            timeout: Duration::from_secs(u64::from(spec.timeout_min) * 60),
            on_timeout: spec.on_timeout,
        }
    }
}

/// The state machine. Tracks plans in a `BTreeMap` keyed by
/// [`PlanId`] so iteration / diff output is deterministic.
#[derive(Debug)]
pub struct PlanApprovalEngine {
    config: PlanApprovalEngineConfig,
    plans: BTreeMap<PlanId, PlanRecord>,
}

impl PlanApprovalEngine {
    /// Build an empty engine.
    pub fn new(config: PlanApprovalEngineConfig) -> Self {
        Self {
            config,
            plans: BTreeMap::new(),
        }
    }

    /// Borrow the tracked plans (read-only; tests + diagnostics).
    pub fn plans(&self) -> &BTreeMap<PlanId, PlanRecord> {
        &self.plans
    }

    /// `<project>/.ccteam/plans/` — the directory the engine scans.
    pub fn plans_dir(&self) -> PathBuf {
        self.config.project_dir.join(PLANS_SUBDIR)
    }

    /// `<project>/.ccteam/plan-decisions/` — the directory the engine
    /// writes decision files into.
    pub fn decisions_dir(&self) -> PathBuf {
        self.config.project_dir.join(DECISIONS_SUBDIR)
    }

    /// Scan [`Self::plans_dir`] for new `.md` files and surface
    /// `SendIm` + `EmitEvent(plan_pending)` actions for each one.
    /// Idempotent — calling twice with no new files returns `vec![]`.
    pub fn scan_plans(&mut self, now: DateTime<Utc>) -> std::io::Result<Vec<PlanEngineAction>> {
        let plans_dir = self.plans_dir();
        if !plans_dir.exists() {
            return Ok(Vec::new());
        }
        let mut actions = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&plans_dir)?
            .filter_map(|r| r.ok())
            .collect();
        // Deterministic order — file_name lexicographic.
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(plan_id) = PlanId::from_plan_filename(fname) else {
                continue;
            };
            if self.plans.contains_key(&plan_id) {
                continue;
            }
            let agent = agent_from_plan_id(&plan_id);
            let head = read_plan_head(&path, 20);
            let body = format_im_notice(&self.config, &plan_id, &agent, &head);
            self.plans.insert(
                plan_id.clone(),
                PlanRecord {
                    plan_id: plan_id.clone(),
                    plan_path: path.clone(),
                    agent: agent.clone(),
                    created_at: now,
                    state: PlanRecordState::Notified { notified_at: now },
                },
            );
            actions.push(PlanEngineAction::SendIm {
                plan_id: plan_id.clone(),
                agent: agent.clone(),
                outbox: self.config.outbox.clone(),
                body,
            });
            actions.push(PlanEngineAction::EmitEvent {
                value: progress::build_plan_pending_event(
                    plan_id.as_str(),
                    &agent,
                    &path.to_string_lossy(),
                    &self.config.outbox,
                    timeout_to_minutes(self.config.timeout),
                ),
            });
        }
        Ok(actions)
    }

    /// Record a user IM reply. Returns the actions the caller must
    /// dispatch:
    ///
    /// - on a valid decision matching a `Notified` plan: write the
    ///   decision file + emit `plan_decision`.
    /// - on a duplicate decision (plan already `Decided`): no actions
    ///   (idempotent).
    /// - on an unknown plan id: empty vec + the engine notes it (the
    ///   IM glue is expected to reply with a "no pending plan" toast).
    pub fn apply_decision(
        &mut self,
        plan_id: &PlanId,
        decision: PlanDecision,
        now: DateTime<Utc>,
    ) -> Vec<PlanEngineAction> {
        let Some(record) = self.plans.get_mut(plan_id) else {
            return Vec::new();
        };
        // Already decided / timed out → drop (idempotent).
        if matches!(
            record.state,
            PlanRecordState::Decided { .. } | PlanRecordState::TimedOut
        ) {
            return Vec::new();
        }
        record.state = PlanRecordState::Decided {
            decision: decision.clone(),
            decided_at: now,
        };
        let body = format_decision_body(plan_id, &record.agent, &decision);
        let path = plan_id.decision_path(&self.config.project_dir);
        vec![
            PlanEngineAction::WriteDecisionFile {
                plan_id: plan_id.clone(),
                path,
                body,
            },
            PlanEngineAction::EmitEvent {
                value: progress::build_plan_decision_event(
                    plan_id.as_str(),
                    &record.agent,
                    decision.as_str(),
                    decision.comment(),
                ),
            },
        ]
    }

    /// Walk all `Notified` plans; if their `notified_at` + timeout has
    /// elapsed, transition to `TimedOut` and dispatch per `on_timeout`.
    pub fn tick_timeouts(&mut self, now: DateTime<Utc>) -> Vec<PlanEngineAction> {
        if self.config.timeout.is_zero() {
            return Vec::new();
        }
        let deadline = chrono::Duration::from_std(self.config.timeout).unwrap_or_else(|_| {
            // Saturating fallback — 100 years is "never" in practice.
            chrono::Duration::days(365 * 100)
        });
        let mut actions = Vec::new();
        // Collect ids first so we don't hold a borrow while mutating.
        let timed_out: Vec<(PlanId, String, DateTime<Utc>)> = self
            .plans
            .iter()
            .filter_map(|(id, rec)| match rec.state {
                PlanRecordState::Notified { notified_at }
                    if now.signed_duration_since(notified_at) >= deadline =>
                {
                    Some((id.clone(), rec.agent.clone(), notified_at))
                }
                _ => None,
            })
            .collect();
        for (plan_id, agent, _notified_at) in timed_out {
            // Always emit plan_timeout first.
            actions.push(PlanEngineAction::EmitEvent {
                value: progress::build_plan_timeout_event(
                    plan_id.as_str(),
                    &agent,
                    on_timeout_str(self.config.on_timeout),
                ),
            });
            match self.config.on_timeout {
                PlanApprovalOnTimeout::Escalate => {
                    if let Some(rec) = self.plans.get_mut(&plan_id) {
                        rec.state = PlanRecordState::TimedOut;
                    }
                    let body = format!(
                        "plan-approval timeout for `{plan}` ({agent}): no APPROVE / REJECT \
                         received within {mins} minute(s). Please decide manually.",
                        plan = plan_id.as_str(),
                        agent = agent,
                        mins = timeout_to_minutes(self.config.timeout),
                    );
                    actions.push(PlanEngineAction::Escalate {
                        plan_id: plan_id.clone(),
                        agent: agent.clone(),
                        body,
                    });
                }
                PlanApprovalOnTimeout::AutoApprove => {
                    let synthetic = PlanDecision::Approve;
                    if let Some(rec) = self.plans.get_mut(&plan_id) {
                        rec.state = PlanRecordState::Decided {
                            decision: synthetic.clone(),
                            decided_at: now,
                        };
                    }
                    let path = plan_id.decision_path(&self.config.project_dir);
                    let body = format_decision_body(&plan_id, &agent, &synthetic);
                    actions.push(PlanEngineAction::WriteDecisionFile {
                        plan_id: plan_id.clone(),
                        path,
                        body,
                    });
                    actions.push(PlanEngineAction::EmitEvent {
                        value: progress::build_plan_decision_event(
                            plan_id.as_str(),
                            &agent,
                            synthetic.as_str(),
                            Some("auto-approve on timeout"),
                        ),
                    });
                }
                PlanApprovalOnTimeout::Reject => {
                    let synthetic = PlanDecision::Reject {
                        reason: Some("timeout".into()),
                    };
                    if let Some(rec) = self.plans.get_mut(&plan_id) {
                        rec.state = PlanRecordState::Decided {
                            decision: synthetic.clone(),
                            decided_at: now,
                        };
                    }
                    let path = plan_id.decision_path(&self.config.project_dir);
                    let body = format_decision_body(&plan_id, &agent, &synthetic);
                    actions.push(PlanEngineAction::WriteDecisionFile {
                        plan_id: plan_id.clone(),
                        path,
                        body,
                    });
                    actions.push(PlanEngineAction::EmitEvent {
                        value: progress::build_plan_decision_event(
                            plan_id.as_str(),
                            &agent,
                            synthetic.as_str(),
                            synthetic.comment(),
                        ),
                    });
                }
            }
        }
        actions
    }
}

/// Wire-format scalar for the timeout policy (matches kebab-case used
/// in workflow.yaml so dashboards stay readable).
fn on_timeout_str(p: PlanApprovalOnTimeout) -> &'static str {
    match p {
        PlanApprovalOnTimeout::Escalate => "escalate",
        PlanApprovalOnTimeout::AutoApprove => "auto-approve",
        PlanApprovalOnTimeout::Reject => "reject",
    }
}

fn timeout_to_minutes(timeout: Duration) -> u32 {
    let secs = timeout.as_secs();
    if secs == 0 {
        0
    } else {
        u32::try_from(secs.div_ceil(60)).unwrap_or(u32::MAX)
    }
}

/// Derive `<agent>` from a plan id like `<agent>-<ts>`. Returns
/// `"unknown"` when the id lacks a `-`.
fn agent_from_plan_id(id: &PlanId) -> String {
    match id.as_str().split_once('-') {
        Some((agent, _)) if !agent.is_empty() => agent.to_string(),
        _ => "unknown".to_string(),
    }
}

/// Read the first `max_lines` lines of `path`, joined with `\n`.
/// Truncates each line at 200 chars to keep the IM body bounded.
fn read_plan_head(path: &Path, max_lines: usize) -> String {
    let body = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = String::new();
    for (idx, raw) in body.lines().enumerate() {
        if idx >= max_lines {
            break;
        }
        let trimmed: String = raw.chars().take(200).collect();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&trimmed);
    }
    out
}

/// Build the IM notice body. Matches the prd.md F98 §2 example shape.
fn format_im_notice(
    config: &PlanApprovalEngineConfig,
    plan_id: &PlanId,
    agent: &str,
    head: &str,
) -> String {
    let timeout_min = timeout_to_minutes(config.timeout);
    let timeout_line = if timeout_min == 0 {
        "Reply APPROVE / REJECT / EDIT <comment>.".to_string()
    } else {
        format!("Reply APPROVE / REJECT / EDIT <comment> within {timeout_min} min.")
    };
    format!(
        "[plan-approval] {agent} wrote `{plan}`:\n\n{head}\n\n{timeout_line}",
        agent = agent,
        plan = plan_id.as_str(),
        head = head,
        timeout_line = timeout_line,
    )
}

/// Render the decision file the agent reads on resume. YAML
/// front-matter + freeform body — schema is internal to F98 and not
/// shared with the inbox `InboxMessage`.
fn format_decision_body(plan_id: &PlanId, agent: &str, decision: &PlanDecision) -> String {
    let mut body = String::new();
    body.push_str("---\n");
    body.push_str(&format!("plan_id: {}\n", plan_id.as_str()));
    body.push_str(&format!("agent: {}\n", agent));
    body.push_str(&format!("decision: {}\n", decision.as_str()));
    if let Some(comment) = decision.comment() {
        // YAML scalar — newlines turn into literal `\n` inside a
        // double-quoted string. Decision comments are short by
        // convention; we don't try to handle multi-line bodies here.
        body.push_str(&format!(
            "comment: {}\n",
            serde_json::to_string(comment).unwrap_or_else(|_| "\"\"".to_string())
        ));
    }
    body.push_str("---\n\n");
    match decision {
        PlanDecision::Approve => body.push_str("APPROVED. Proceed with the plan.\n"),
        PlanDecision::Reject { reason } => {
            body.push_str("REJECTED.");
            if let Some(r) = reason {
                body.push_str(" Reason: ");
                body.push_str(r);
            }
            body.push('\n');
        }
        PlanDecision::Edit { comment } => {
            body.push_str("EDIT requested:\n");
            body.push_str(comment);
            body.push('\n');
        }
    }
    body
}

/// Convenience helper — dispatch one action to the filesystem +
/// progress.jsonl. The orchestrator wires its IM channel + escalation
/// path on top of this; tests call it directly to drive the loop.
pub fn apply_action(
    action: &PlanEngineAction,
    progress_path: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match action {
        PlanEngineAction::EmitEvent { value } => {
            progress::append_event(progress_path, value)?;
        }
        PlanEngineAction::WriteDecisionFile { path, body, .. } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("md.tmp");
            std::fs::write(&tmp, body.as_bytes())?;
            std::fs::rename(&tmp, path)?;
        }
        // SendIm / Escalate are pure "tell the IM glue" actions —
        // caller dispatches.
        PlanEngineAction::SendIm { .. } | PlanEngineAction::Escalate { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decision_approve_case_insensitive() {
        assert_eq!(parse_decision("APPROVE"), Some(PlanDecision::Approve));
        assert_eq!(parse_decision(" approve "), Some(PlanDecision::Approve));
        assert_eq!(parse_decision("Approve"), Some(PlanDecision::Approve));
    }

    #[test]
    fn parse_decision_reject_with_optional_reason() {
        assert_eq!(
            parse_decision("REJECT"),
            Some(PlanDecision::Reject { reason: None })
        );
        assert_eq!(
            parse_decision("reject too risky"),
            Some(PlanDecision::Reject {
                reason: Some("too risky".to_string())
            })
        );
    }

    #[test]
    fn parse_decision_edit_requires_comment() {
        assert_eq!(parse_decision("EDIT"), None);
        assert_eq!(
            parse_decision("EDIT please add tests"),
            Some(PlanDecision::Edit {
                comment: "please add tests".to_string()
            })
        );
    }

    #[test]
    fn parse_decision_rejects_random_text() {
        assert_eq!(parse_decision("hello"), None);
        assert_eq!(parse_decision(""), None);
        assert_eq!(parse_decision("  "), None);
        assert_eq!(parse_decision("apple"), None);
    }

    #[test]
    fn plan_id_round_trip_strips_md() {
        assert_eq!(
            PlanId::from_plan_filename("reviewer-202605191200.md"),
            Some(PlanId("reviewer-202605191200".to_string()))
        );
        assert_eq!(PlanId::from_plan_filename("missing-ext"), None);
        assert_eq!(PlanId::from_plan_filename(".md"), None);
    }

    #[test]
    fn agent_from_plan_id_splits_on_hyphen() {
        assert_eq!(
            agent_from_plan_id(&PlanId("reviewer-202605191200".to_string())),
            "reviewer"
        );
        assert_eq!(agent_from_plan_id(&PlanId("solo".to_string())), "unknown");
    }

    #[test]
    fn timeout_to_minutes_rounds_up() {
        assert_eq!(timeout_to_minutes(Duration::from_secs(0)), 0);
        assert_eq!(timeout_to_minutes(Duration::from_secs(60)), 1);
        assert_eq!(timeout_to_minutes(Duration::from_secs(61)), 2);
        assert_eq!(timeout_to_minutes(Duration::from_secs(3600)), 60);
    }
}
