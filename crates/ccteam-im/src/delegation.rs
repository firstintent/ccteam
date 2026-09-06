//! v0.9.0 W2 (F2/F5/F7) — delegation support types + pure helpers that don't
//! need the `Gateway`'s private state (those methods live in `gateway.rs`).
//!
//! Holds the pump → notifier signal, bounded/TTL'd idempotency cache, shared
//! delegation summary renderers, and the workflow budget-cap parser.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use ccteam_core::CcteamPaths;
use ccteam_harness::AgentVendor;

/// The pump signals the delegation notifier after it durably appends a child's
/// assistant message. The notifier (which owns a `GatewayHandle`) then checks
/// the child's watch and, per its [`ccteam_harness::NotifyMode`], submits the
/// notification to the parent. Carries the exact `turn_id` the pump wrote so
/// dedup is precise.
///
/// Two shapes flow on this channel (v0.9.5 feedback fix — the notification
/// unit is the TASK, not each mirrored message):
/// - `boundary: false` — an interim assistant message inside a still-running
///   vendor turn (codex narrates checkpoints this way). Only an `all` watch
///   ever notifies on these.
/// - `boundary: true` — the vendor turn finished (`TurnCompleted` /
///   `TurnFailed` / `Error`). This is the default (`final`) notification point.
#[derive(Debug, Clone)]
pub struct DelegationSignal {
    /// The child session whose message/turn just completed.
    pub child_sid: String,
    /// The mirrored turn id carrying this signal's `tail` (the dedup key;
    /// for a boundary signal this is the turn holding the FINAL answer).
    pub turn_id: String,
    /// Full assistant text. The notification builder applies its own bounded
    /// excerpt; the durable session ledger retains this verbatim.
    pub tail: String,
    /// The turn's conclusion (`TurnRecord::conclusion`): the text after the
    /// child's last tool call, when the vendor marks it and it is shorter
    /// than `tail`. The excerpt prefers it over the head of the narration.
    pub conclusion: Option<String>,
    /// The child's vendor (for the notification text + the progress event).
    pub vendor: AgentVendor,
    /// The child's host (`local` or a satellite id).
    pub host: String,
    /// True = the vendor turn boundary (child idle); false = interim message.
    pub boundary: bool,
    /// True when the boundary came from the vendor's structured fatal-turn
    /// outcome (`TurnFailed` / terminal `Error`), rather than normal completion.
    pub vendor_error: bool,
    /// Interim assistant messages that preceded this boundary within the same
    /// vendor turn (0 for interim signals and single-message turns).
    pub interim_notes: usize,
    /// Every mirrored turn id this signal covers (self included). On a
    /// boundary these are recorded into `notified_turns` in one batch so a
    /// daemon-restart reconcile never re-delivers folded interim messages.
    pub covered_turns: Vec<String>,
    /// Child context usage carried into the parent only at the turn boundary.
    pub context_pct: Option<u64>,
    /// Vendor turn ordinal shown in status surfaces.
    pub turn: u64,
    /// Stable failure kind when the boundary is a vendor error.
    pub error_kind: Option<String>,
}

/// One source of truth for completion notifications and inline wait results.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DelegationSummary<'a> {
    pub sid: &'a str,
    /// The child's harness (`claude` / `codex` / …): the parent reads it off
    /// the one-line header, so it never has to ask which vendor answered.
    pub vendor: &'a str,
    pub turn_id: &'a str,
    pub turn: u64,
    pub outcome: DelegationOutcome,
    pub context_pct: Option<u64>,
    pub cost_usd: Option<f64>,
    /// Every text block of the turn, in order (the ledger row's `assistant`).
    pub answer: &'a str,
    /// The turn's conclusion when the vendor marked one shorter than `answer`
    /// (the ledger row's `conclusion`): what a bounded excerpt shows first.
    pub conclusion: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DelegationOutcome {
    Done,
    Failed {
        kind: Option<String>,
        error: Option<String>,
    },
}

/// Cap on the answer text an INLINE `agent{wait}` result carries: the `final`
/// tier. The caller blocked for this answer, so it gets the widest excerpt any
/// wake-up carries — but not a transcript, and not its own private tier four
/// times that (issue #195: `notify` went frugal by default while the inline
/// path, the one a correct orchestrator actually takes, kept 4000). The
/// excerpt's marker names the exact `agent_read` call for the rest.
pub(crate) const INLINE_RESULT_MAX_CHARS: usize = NOTIFICATION_ANSWER_MAX_CHARS;

pub(crate) fn context_pct(status: Option<&ccteam_harness::TurnStatus>) -> Option<u64> {
    status
        .and_then(|status| status.context.as_ref())
        .and_then(|context| context.pct())
        .map(|pct| pct.round() as u64)
}

/// How full one session's context window is right now, read from the LATEST
/// turn status in its transcript mirror. `None` when the session has no turn
/// yet, or the vendor reported no window (never a fabricated 0).
///
/// One reader, every surface: the MCP roster rows, the caller's own `you` row
/// on `status{detail:"usage"}`, and the web session list all answer "how much
/// room is left" from this — a second copy of the read is how two surfaces
/// start disagreeing about one session.
///
/// BOUNDED BY CONSTRUCTION. It costs one BACKWARDS tail of `turns.jsonl`, not a
/// whole-transcript scan: this is per-row on endpoints a browser polls, and a
/// long-lived session's transcript is megabytes. A window is a live fact, so
/// only the recent tail can hold a current one — past [`CONTEXT_TAIL_TURNS`]
/// statusless turns the honest answer is "unobserved", not a reading from an
/// hour of work ago. Callers still pay it only for the rows they actually
/// emit, never for a whole fleet before a cut.
pub fn latest_context_pct(project_dir: &std::path::Path, sid: &str) -> Option<u64> {
    let turns =
        ccteam_harness::execution::turns_mirror::last_n_turns(project_dir, sid, CONTEXT_TAIL_TURNS)
            .ok()?;
    let status = turns.into_iter().rev().find_map(|turn| turn.status)?;
    context_pct(Some(&status))
}

/// How far back [`latest_context_pct`] looks for a turn that carried a status.
/// Vendors that report one report it at nearly every turn boundary, so this is
/// far past "the last one that had it" while keeping the read O(tail).
const CONTEXT_TAIL_TURNS: usize = 50;

impl DelegationSummary<'_> {
    /// The completion turn the parent receives. `max_chars` is the excerpt cap
    /// its watch asked for ([`NOTIFICATION_ANSWER_MAX_CHARS`] for `final`,
    /// [`BRIEF_NOTIFICATION_ANSWER_MAX_CHARS`] for `brief`): the wake-up POINT
    /// is a property of the task, how much rides along is the parent's context
    /// budget, so they are separate axes. What the cap keeps is decided by
    /// [`answer_excerpt`]: the whole answer when it fits, else the child's
    /// conclusion before its narration.
    pub(crate) fn notification_text(&self, max_chars: usize) -> String {
        let outcome = match &self.outcome {
            DelegationOutcome::Done => format!("{} done", self.sid),
            DelegationOutcome::Failed { kind, .. } => format!(
                "{} FAILED ({})",
                self.sid,
                kind.as_deref()
                    .filter(|kind| !kind.is_empty())
                    .unwrap_or("vendor error")
            ),
        };
        // `s12 done · codex · turn 7 · ctx 19%` — the same field order as the
        // web bubble footer and the IM status line (vendor before the metrics).
        let vendor = if self.vendor.is_empty() {
            String::new()
        } else {
            format!(" · {}", self.vendor)
        };
        let first = match self.context_pct {
            Some(pct) => format!(
                "{outcome}{vendor} · turn {} · ctx {pct}%{}",
                self.turn,
                if pct >= 85 { "⚠" } else { "" }
            ),
            None => format!("{outcome}{vendor} · turn {}", self.turn),
        };
        let answer_text = self.answer.trim();
        let total_chars = answer_text.chars().count();
        let answer = answer_excerpt(answer_text, self.conclusion, max_chars, |omitted| {
            full_answer_marker(omitted, total_chars, self.sid)
        });
        if answer.text.is_empty() {
            first
        } else {
            format!("{first}\n{}", answer.text)
        }
    }

    pub(crate) fn inline_result(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut result = serde_json::Map::new();
        result.insert("sid".into(), serde_json::json!(self.sid));
        result.insert("turn_id".into(), serde_json::json!(self.turn_id));
        result.insert("turn".into(), serde_json::json!(self.turn));
        let (kind, error) = match &self.outcome {
            DelegationOutcome::Done => {
                result.insert("status".into(), serde_json::json!("completed"));
                (None, None)
            }
            DelegationOutcome::Failed { kind, error } => {
                result.insert("status".into(), serde_json::json!("failed"));
                (kind.as_deref(), error.as_deref())
            }
        };
        if let Some(pct) = self.context_pct {
            result.insert("context_pct".into(), serde_json::json!(pct));
        }
        let total_chars = self.answer.chars().count();
        let answer = answer_excerpt(
            self.answer,
            self.conclusion,
            INLINE_RESULT_MAX_CHARS,
            |omitted| full_answer_marker(omitted, total_chars, self.sid),
        );
        result.insert("result_text".into(), serde_json::json!(answer.text));
        if let Some(kind) = kind.filter(|kind| !kind.is_empty()) {
            result.insert("error_kind".into(), serde_json::json!(kind));
        }
        if let Some(error) = error.filter(|error| !error.is_empty()) {
            result.insert("error".into(), serde_json::json!(error));
        }
        if let Some(cost) = self.cost_usd {
            result.insert("cost_usd".into(), serde_json::json!(cost));
        }
        result
    }
}

/// The `notified_turns` key recording that a turn's BOUNDARY notification was
/// handled. Distinct from the plain turn-id keys the reconcile bookkeeping
/// records per mirrored turn, so a folded interim turn can never be mistaken
/// for its task's delivered completion.
pub fn final_dedup_key(turn_id: &str) -> String {
    format!("{turn_id}#final")
}

/// Result of a Unicode-safe character cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedText {
    pub text: String,
    pub truncated: bool,
    /// Original character count before truncation.
    pub total_chars: usize,
}

/// Keep a 70% head / 30% tail excerpt while fitting the supplied marker and
/// content inside `max_chars`. All arithmetic is in Unicode scalar values, so
/// slicing never lands inside a UTF-8 codepoint. `marker` receives the exact
/// number of omitted source characters.
pub(crate) fn truncate_head_tail_with_marker(
    text: &str,
    max_chars: usize,
    marker: impl Fn(usize) -> String,
) -> BoundedText {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return BoundedText {
            text: text.to_string(),
            truncated: false,
            total_chars,
        };
    }

    // Marker length depends on the omitted-count digits. Iterate to the small
    // fixed point instead of reporting an approximate count.
    let mut keep_chars = max_chars;
    let mut resolved = None;
    for _ in 0..16 {
        let omitted = total_chars.saturating_sub(keep_chars);
        let candidate = marker(omitted);
        let next_keep = max_chars.saturating_sub(candidate.chars().count());
        if next_keep == keep_chars {
            resolved = Some((candidate, keep_chars));
            break;
        }
        keep_chars = next_keep;
    }
    let (marker_text, keep_chars) = resolved.unwrap_or_else(|| {
        let candidate = marker(total_chars.saturating_sub(keep_chars));
        let next_keep = max_chars.saturating_sub(candidate.chars().count());
        (candidate, next_keep)
    });

    if marker_text.chars().count() >= max_chars {
        return BoundedText {
            text: marker_text.chars().take(max_chars).collect(),
            truncated: true,
            total_chars,
        };
    }

    let head_chars = keep_chars.saturating_mul(70) / 100;
    let tail_chars = keep_chars.saturating_sub(head_chars);
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .skip(total_chars.saturating_sub(tail_chars))
        .collect();
    BoundedText {
        text: format!("{head}{marker_text}{tail}"),
        truncated: true,
        total_chars,
    }
}

/// What a bounded surface shows of one turn's answer within `max_chars`:
///
/// 1. the whole answer when it fits — the cap is a budget, not a filter;
/// 2. otherwise the turn's CONCLUSION (the text after the child's last tool
///    call, when the vendor marked one shorter than the answer) behind ONE
///    marker standing in for the narration before it; the conclusion is
///    head/tail-cut itself only when it cannot fit next to that marker;
/// 3. otherwise the 70/30 head/tail preview.
///
/// Rule 2 is issue #196: since #192 the answer is narration + conclusion in
/// stream order, so a head-biased preview showed "Reading the tests…" and cut
/// the receipt the worker wrote last — the one thing the parent woke up for.
/// `marker` receives the exact number of omitted source characters whichever
/// rule fired, so the read recipe it carries is always for the whole answer.
pub(crate) fn answer_excerpt(
    answer: &str,
    conclusion: Option<&str>,
    max_chars: usize,
    marker: impl Fn(usize) -> String,
) -> BoundedText {
    let total_chars = answer.chars().count();
    if total_chars <= max_chars {
        return BoundedText {
            text: answer.to_string(),
            truncated: false,
            total_chars,
        };
    }
    let conclusion = conclusion
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .filter(|c| c.chars().count() < total_chars);
    let Some(conclusion) = conclusion else {
        return truncate_head_tail_with_marker(answer, max_chars, marker);
    };
    let conclusion_chars = conclusion.chars().count();
    let narration_chars = total_chars - conclusion_chars;
    let lead = marker(narration_chars);
    if lead.chars().count() + 1 + conclusion_chars <= max_chars {
        return BoundedText {
            text: format!("{lead}\n{conclusion}"),
            truncated: true,
            total_chars,
        };
    }
    // The conclusion cannot ride whole next to its marker: head/tail-cut IT,
    // the marker counting the narration it also stands in for. The cap is
    // pulled under the conclusion's own length so the cut (and the recipe)
    // always happens — every excerpt of a cut answer names the whole read.
    let cap = max_chars.min(conclusion_chars.saturating_sub(1));
    let cut = truncate_head_tail_with_marker(conclusion, cap, |omitted| {
        marker(omitted + narration_chars)
    });
    BoundedText {
        text: cut.text,
        truncated: true,
        total_chars,
    }
}

/// Max characters of child answer embedded in a `final` completion
/// notification — the default. A parent that wants the whole thing calls
/// `agent_read`; a parent that wants less asks for `notify:"brief"`.
pub const NOTIFICATION_ANSWER_MAX_CHARS: usize = 2_000;

/// The `notify:"brief"` excerpt cap: enough to know what happened, cheap
/// enough to wake a parent on many children.
pub const BRIEF_NOTIFICATION_ANSWER_MAX_CHARS: usize = 500;

/// The excerpt cap a watch's notify mode asks for.
pub fn notification_answer_max_chars(mode: ccteam_harness::NotifyMode) -> usize {
    match mode {
        ccteam_harness::NotifyMode::Brief => BRIEF_NOTIFICATION_ANSWER_MAX_CHARS,
        _ => NOTIFICATION_ANSWER_MAX_CHARS,
    }
}

/// `agent_read{max_chars}` — the character budget across the turns one
/// transcript read returns: its default and the bounds the parameter accepts.
/// The default is a deliberate step above the brief notification's 500: a bare
/// read after a notification should add something, and the pointer on any
/// excerpt names the exact budget for the rest.
pub(crate) const AGENT_READ_DEFAULT_MAX_CHARS: usize = 1_000;
pub(crate) const AGENT_READ_MIN_MAX_CHARS: usize = 100;
pub(crate) const AGENT_READ_MAX_MAX_CHARS: usize = 50_000;

/// The `max_chars` that reads a `total_chars`-long turn whole, within what the
/// parameter accepts.
pub(crate) fn read_budget_for(total_chars: usize) -> usize {
    total_chars.clamp(AGENT_READ_MIN_MAX_CHARS, AGENT_READ_MAX_MAX_CHARS)
}

/// The pointer a truncated answer carries: the exact one-call recipe for the
/// whole text (`n:1` = the newest turn, which the answer is at delivery), so
/// the reader never has to guess a budget or page through history (issue
/// #194: the frugal, correct read was discoverable only by trial).
pub(crate) fn full_answer_marker(omitted: usize, total_chars: usize, child_sid: &str) -> String {
    format!(
        "…[+{omitted} chars: agent_read{{sid:{child_sid},n:1,max_chars:{}}}]…",
        read_budget_for(total_chars)
    )
}

/// Render the ordinary user-role completion turn sent to the parent.
pub(crate) fn build_notification_text_with_outcome(
    summary: &DelegationSummary<'_>,
    max_chars: usize,
) -> String {
    summary.notification_text(max_chars)
}

/// Lowercase wire name of a vendor — the key `cost_24h_by_vendor` uses and the
/// `delegation_*` progress `vendor` field.
pub fn vendor_key(vendor: AgentVendor) -> &'static str {
    match vendor {
        AgentVendor::Claude => "claude",
        AgentVendor::Codex => "codex",
        AgentVendor::Grok => "grok",
        AgentVendor::Opencode => "opencode",
        AgentVendor::Kimi => "kimi",
        AgentVendor::Pi => "pi",
        AgentVendor::Dsh => "dsh",
    }
}

fn vendor_to_cost(vendor: AgentVendor) -> ccteam_cost::Vendor {
    match vendor {
        AgentVendor::Claude => ccteam_cost::Vendor::Claude,
        AgentVendor::Codex => ccteam_cost::Vendor::Codex,
        AgentVendor::Grok => ccteam_cost::Vendor::Grok,
        AgentVendor::Opencode => ccteam_cost::Vendor::Opencode,
        AgentVendor::Kimi => ccteam_cost::Vendor::Kimi,
        AgentVendor::Pi => ccteam_cost::Vendor::Pi,
        AgentVendor::Dsh => ccteam_cost::Vendor::Dsh,
    }
}

// ── idempotency cache ───────────────────────────────────────────────────────

/// Default idempotency cache capacity (per gateway map).
pub const IDEM_CAP: usize = 256;
/// Default idempotency entry TTL.
pub const IDEM_TTL: Duration = Duration::from_secs(3600);

/// Bounded, TTL'd in-memory idempotency map (`key → recorded id`). **Honest
/// scope: in-memory only — a daemon restart forgets every key**, so a retry
/// that straddles a restart may double-act (documented in the tool
/// description + handoff). Within one daemon lifetime a replay returns the
/// recorded id and performs NO side effect. Composite keys scope it
/// per-project (spawn) / per-child (dispatch).
pub struct IdemCache {
    map: HashMap<String, (String, Instant)>,
    order: VecDeque<String>,
    cap: usize,
    ttl: Duration,
}

impl Default for IdemCache {
    fn default() -> Self {
        Self::new(IDEM_CAP, IDEM_TTL)
    }
}

impl IdemCache {
    /// Build a cache with an explicit capacity (min 1) + entry TTL.
    pub fn new(cap: usize, ttl: Duration) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
            ttl,
        }
    }

    /// Compose a scoped key (`scope\0key`) so spawn keys are per-project and
    /// dispatch keys are per-child without colliding.
    pub fn scoped(scope: &str, key: &str) -> String {
        format!("{scope}\u{0}{key}")
    }

    /// Look up a live (non-expired) entry, pruning it if stale.
    pub fn get(&mut self, key: &str) -> Option<String> {
        let expired = match self.map.get(key) {
            Some((_, at)) => at.elapsed() >= self.ttl,
            None => return None,
        };
        if expired {
            self.map.remove(key);
            self.order.retain(|k| k != key);
            return None;
        }
        self.map.get(key).map(|(v, _)| v.clone())
    }

    /// Record `key → id`, evicting the oldest entry when over capacity.
    pub fn put(&mut self, key: String, id: String) {
        if self.map.contains_key(&key) {
            self.map.insert(key.clone(), (id, Instant::now()));
            self.order.retain(|k| k != &key);
            self.order.push_back(key);
            return;
        }
        while self.order.len() >= self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
        self.map.insert(key.clone(), (id, Instant::now()));
        self.order.push_back(key);
    }
}

// ── budget helpers ──────────────────────────────────────────────────────────

/// Per-vendor 24h cap from the project's `workflow.yaml::budgets_v060`, if any
/// (same precedence as the web status route: nested `.ccteam/` first). `None`
/// = no cap configured for this vendor → the gate is inert.
pub fn project_vendor_budget_cap(
    paths: &CcteamPaths,
    slug: &str,
    vendor: AgentVendor,
) -> Option<f64> {
    #[derive(serde::Deserialize)]
    struct BudgetView {
        #[serde(default)]
        budgets_v060: Option<ccteam_cost::Budgets>,
    }
    let project_dir = paths.project_dir(slug);
    let nested = project_dir.join(".ccteam").join("workflow.yaml");
    let direct = project_dir.join("workflow.yaml");
    let path = if nested.exists() { nested } else { direct };
    let raw = std::fs::read_to_string(&path).ok()?;
    let view: BudgetView = serde_yaml::from_str(&raw).ok()?;
    view.budgets_v060
        .as_ref()
        .and_then(|b| b.cap_for(vendor_to_cost(vendor)).max_cost_usd_per_24h)
}

// ── guardrail denial reasons ────────────────────────────────────────────────

/// Why a delegation was denied — the `reason` tag on the `delegation_denied`
/// (engine guardrails) / `delegation_policy_denied` (Card H user hook) progress
/// event + the human-readable error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Child depth would exceed `delegation.max_depth`.
    Depth,
    /// The parent already holds `delegation.max_children` active children.
    Children,
    /// The project already holds `delegation.max_delegated` active delegates.
    Delegated,
    /// The dispatch target is the caller itself or one of its ancestors.
    Cycle,
    /// The vendor's trailing-24h project cost has reached its budget cap.
    Budget,
    /// Card H — the user's own `pre-agent` policy hook exited 2 (deny). The
    /// engine has no opinion here: it relays the script's verdict.
    Policy,
    /// Card H — the `pre-agent` hook could not deliver a verdict (timeout, an
    /// exit code outside the dialect, not executable). Fail-closed like a deny,
    /// but tagged apart on purpose: a rule that says no and a script that is
    /// broken need different humans to act.
    PolicyScriptError,
}

impl DenyReason {
    /// The stable lowercase tag used in the `delegation_*denied{reason}` event
    /// + the human-readable error.
    pub fn tag(self) -> &'static str {
        match self {
            DenyReason::Depth => "depth",
            DenyReason::Children => "children",
            DenyReason::Delegated => "delegated",
            DenyReason::Cycle => "cycle",
            DenyReason::Budget => "budget",
            DenyReason::Policy => "policy",
            DenyReason::PolicyScriptError => "policy_script_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_uses_minimal_done_header() {
        let t = DelegationSummary {
            sid: "s444",
            vendor: "codex",
            turn_id: "codex-uuid",
            turn: 3,
            outcome: DelegationOutcome::Done,
            context_pct: Some(31),
            cost_usd: None,
            answer: "hello",
            conclusion: None,
        }
        .notification_text(NOTIFICATION_ANSWER_MAX_CHARS);
        assert_eq!(t, "s444 done · codex · turn 3 · ctx 31%\nhello");
        assert!(!t.contains("[ccteam]"));
        assert!(!t.contains("--- final answer ---"));
        assert!(!t.contains("child is idle"));
        assert!(!t.contains("interim note(s)"));
    }

    #[test]
    fn notification_text_folds_interim_notes() {
        let summary = DelegationSummary {
            sid: "s69",
            vendor: "claude",
            turn_id: "s69-54",
            turn: 3,
            outcome: DelegationOutcome::Done,
            context_pct: Some(3),
            cost_usd: Some(0.42),
            answer: "wave done",
            conclusion: None,
        };
        assert_eq!(
            build_notification_text_with_outcome(&summary, NOTIFICATION_ANSWER_MAX_CHARS),
            "s69 done · claude · turn 3 · ctx 3%\nwave done"
        );
    }

    #[test]
    fn final_dedup_key_is_distinct_from_plain_turn_id() {
        assert_eq!(final_dedup_key("s7-3"), "s7-3#final");
        assert_ne!(final_dedup_key("s7-3"), "s7-3");
    }

    #[test]
    fn notification_answer_truncates_with_head_tail_and_pointer() {
        let long = format!(
            "HEAD{}TAIL",
            "x".repeat(NOTIFICATION_ANSWER_MAX_CHARS + 500)
        );
        let summary = DelegationSummary {
            sid: "s1",
            vendor: "codex",
            turn_id: "s1-1",
            turn: 4,
            outcome: DelegationOutcome::Done,
            context_pct: None,
            cost_usd: None,
            answer: &long,
            conclusion: None,
        };
        // `final` (the default) and `brief` differ only in the excerpt cap.
        for cap in [
            NOTIFICATION_ANSWER_MAX_CHARS,
            BRIEF_NOTIFICATION_ANSWER_MAX_CHARS,
        ] {
            let t = summary.notification_text(cap);
            assert!(t.contains("HEAD"));
            assert!(t.contains("TAIL"));
            assert!(t.contains("…[+"));
            // The pointer is the exact recipe for the whole answer: 4 + 2500 + 4
            // characters, read as the newest turn.
            assert!(t.contains("agent_read{sid:s1,n:1,max_chars:2508}"), "{t}");
            let embedded = t.split_once('\n').unwrap().1;
            assert_eq!(embedded.chars().count(), cap);
        }
    }

    /// issue #196 — the shape measured on excore s908: five narration blocks
    /// (647 chars) then a 420-char receipt. A brief (500) excerpt used to show
    /// the narration's head and ~135 chars of the receipt's tail; it now shows
    /// the receipt whole behind one marker for the narration it skipped.
    #[test]
    fn brief_excerpt_is_the_conclusion_not_the_narrations_head() {
        let narration = (1..=5)
            .map(|n| format!("Narration block {n}: {}", "reading the tests… ".repeat(6)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let receipt = format!(
            "READY for review · card@abc123 · checks: {}GREEN",
            "GREEN · ".repeat(44)
        );
        let answer = format!("{narration}\n\n{receipt}");
        let total = answer.chars().count();
        assert!(total > 1_000 && receipt.chars().count() < 450, "{total}");
        let summary = DelegationSummary {
            sid: "s908",
            vendor: "claude",
            turn_id: "s908-1",
            turn: 1,
            outcome: DelegationOutcome::Done,
            context_pct: Some(17),
            cost_usd: None,
            answer: &answer,
            conclusion: Some(&receipt),
        };
        let text = summary.notification_text(BRIEF_NOTIFICATION_ANSWER_MAX_CHARS);
        let (header, excerpt) = text.split_once('\n').unwrap();
        assert_eq!(header, "s908 done · claude · turn 1 · ctx 17%");
        assert!(
            excerpt.chars().count() <= BRIEF_NOTIFICATION_ANSWER_MAX_CHARS,
            "{excerpt}"
        );
        assert!(
            excerpt.ends_with(&receipt),
            "the receipt rides whole: {excerpt}"
        );
        assert!(!excerpt.contains("Narration block 1"), "{excerpt}");
        // The marker counts exactly the narration (+ its separator) and names
        // the read of the WHOLE answer, not of the receipt.
        let omitted = total - receipt.chars().count();
        assert!(
            excerpt.starts_with(&format!(
                "…[+{omitted} chars: agent_read{{sid:s908,n:1,max_chars:{total}}}]…\n"
            )),
            "{excerpt}"
        );
        // The inline result applies the same rule at its own cap.
        let inline = summary.inline_result();
        assert_eq!(
            inline["result_text"],
            serde_json::json!(answer),
            "fits 2000 → whole"
        );
    }

    /// A conclusion that cannot ride whole next to its marker is head/tail-cut
    /// itself, and the marker still counts the narration it stands in for.
    #[test]
    fn an_oversized_conclusion_is_cut_with_the_narration_counted() {
        let narration = "n".repeat(300);
        let conclusion = format!("HEAD{}TAIL", "c".repeat(792));
        let answer = format!("{narration}\n\n{conclusion}");
        let got = answer_excerpt(&answer, Some(&conclusion), 500, |omitted| {
            format!("[+{omitted}]")
        });
        assert!(got.truncated);
        assert_eq!(got.total_chars, 1_102);
        assert_eq!(got.text.chars().count(), 500);
        assert!(got.text.starts_with("HEAD"), "{}", got.text);
        assert!(got.text.ends_with("TAIL"), "{}", got.text);
        // 1102 total − 500 shown + the marker's own length = what the marker
        // must report as omitted; it must exceed the narration alone.
        let marker_start = got.text.find("[+").unwrap();
        let marker_end = got.text[marker_start..].find(']').unwrap() + marker_start;
        let omitted: usize = got.text[marker_start + 2..marker_end].parse().unwrap();
        assert!(omitted > 300, "{omitted}");
        assert_eq!(omitted, 1_102 - (500 - (marker_end + 1 - marker_start)));

        // The band where the conclusion fits the cap alone but not with its
        // marker still yields a cut WITH a recipe — never a bare conclusion.
        // 498 chars: fits the cap alone, not next to a marker.
        let conclusion = format!("HEAD{}TAIL", "c".repeat(490));
        let answer = format!("{narration}\n\n{conclusion}");
        let got = answer_excerpt(&answer, Some(&conclusion), 500, |omitted| {
            format!("[+{omitted}]")
        });
        assert!(got.truncated);
        assert!(got.text.contains("[+"), "{}", got.text);
        assert!(got.text.chars().count() <= 500);
        assert!(got.text.starts_with("HEAD") && got.text.ends_with("TAIL"));
    }

    /// The cap is a budget, not a filter: an answer that fits is shown whole
    /// even when a conclusion is known; without a conclusion (or with one that
    /// IS the answer) the head/tail preview stands.
    #[test]
    fn answer_excerpt_rules_in_order() {
        let fits = "short narration\n\nshort receipt";
        let got = answer_excerpt(fits, Some("short receipt"), 500, |n| format!("[+{n}]"));
        assert!(!got.truncated);
        assert_eq!(got.text, fits);

        let long = format!("HEAD{}TAIL", "x".repeat(1_000));
        for conclusion in [None, Some(long.as_str()), Some(""), Some("   ")] {
            let got = answer_excerpt(&long, conclusion, 100, |n| format!("[+{n}]"));
            assert!(got.truncated);
            assert_eq!(got.text.chars().count(), 100);
            assert!(
                got.text.starts_with("HEAD") && got.text.ends_with("TAIL"),
                "{conclusion:?}"
            );
        }
    }

    /// The read recipe stays inside what `agent_read{max_chars}` accepts.
    #[test]
    fn read_budget_is_clamped_to_the_parameter_bounds() {
        assert_eq!(read_budget_for(7), AGENT_READ_MIN_MAX_CHARS);
        assert_eq!(read_budget_for(2_508), 2_508);
        assert_eq!(read_budget_for(1_000_000), AGENT_READ_MAX_MAX_CHARS);
        assert_eq!(AGENT_READ_DEFAULT_MAX_CHARS, 1_000);
    }

    /// The documented tiers — and `brief` is what a dispatch gets unless it
    /// asks (issue #194: frugality is the default, not a discovery).
    #[test]
    fn notification_caps_are_the_documented_tiers() {
        use ccteam_harness::NotifyMode;
        assert_eq!(NOTIFICATION_ANSWER_MAX_CHARS, 2_000);
        assert_eq!(BRIEF_NOTIFICATION_ANSWER_MAX_CHARS, 500);
        assert_eq!(INLINE_RESULT_MAX_CHARS, 2_000);
        assert_eq!(notification_answer_max_chars(NotifyMode::Final), 2_000);
        assert_eq!(notification_answer_max_chars(NotifyMode::Brief), 500);
        assert_eq!(notification_answer_max_chars(NotifyMode::Off), 2_000);
        assert_eq!(NotifyMode::default(), NotifyMode::Brief);
        assert_eq!(notification_answer_max_chars(NotifyMode::default()), 500);
    }

    /// The inline result names the session it came from: an `agent` reply is
    /// the only place a caller learns the sid it just hired.
    #[test]
    fn inline_result_carries_the_child_sid() {
        let result = DelegationSummary {
            sid: "s5",
            vendor: "codex",
            turn_id: "s5-1",
            turn: 1,
            outcome: DelegationOutcome::Done,
            context_pct: None,
            cost_usd: None,
            answer: "ok",
            conclusion: None,
        }
        .inline_result();
        assert_eq!(result["sid"], serde_json::json!("s5"));
        assert_eq!(result["status"], serde_json::json!("completed"));
        assert_eq!(result["result_text"], serde_json::json!("ok"));
        // A completed answer of "ok" must stay tiny.
        let bytes = serde_json::to_string(&result).unwrap().len();
        assert!(bytes <= 250, "inline completion is {bytes} B: {result:?}");
    }

    #[test]
    fn notification_text_failed_header_and_context_warning() {
        let summary = DelegationSummary {
            sid: "s444",
            vendor: "codex",
            turn_id: "codex-uuid",
            turn: 3,
            outcome: DelegationOutcome::Failed {
                kind: Some("turn_timeout".into()),
                error: Some("oops".into()),
            },
            context_pct: Some(31),
            cost_usd: None,
            answer: "oops",
            conclusion: None,
        };
        assert_eq!(
            build_notification_text_with_outcome(&summary, NOTIFICATION_ANSWER_MAX_CHARS),
            "s444 FAILED (turn_timeout) · codex · turn 3 · ctx 31%\noops"
        );
    }

    #[test]
    fn notification_text_omits_unknown_context() {
        let t = DelegationSummary {
            sid: "s444",
            vendor: "codex",
            turn_id: "s1-1",
            turn: 3,
            outcome: DelegationOutcome::Done,
            context_pct: None,
            cost_usd: None,
            answer: "ok",
            conclusion: None,
        }
        .notification_text(NOTIFICATION_ANSWER_MAX_CHARS);
        assert_eq!(t.lines().next(), Some("s444 done · codex · turn 3"));
    }

    #[test]
    fn bounded_text_under_cap_is_untouched() {
        let got = truncate_head_tail_with_marker("hello", 5, |n| format!("[{n}]"));
        assert_eq!(got.text, "hello");
        assert!(!got.truncated);
        assert_eq!(got.total_chars, 5);
    }

    #[test]
    fn bounded_text_over_cap_keeps_head_tail_and_marker() {
        let input = format!("HEAD{}TAIL", "x".repeat(200));
        let got = truncate_head_tail_with_marker(&input, 100, |n| format!("[cut {n}]"));
        assert!(got.truncated);
        assert_eq!(got.text.chars().count(), 100);
        assert!(got.text.starts_with("HEAD"));
        assert!(got.text.ends_with("TAIL"));
        assert!(got.text.contains("[cut "));
    }

    #[test]
    fn bounded_text_is_unicode_safe() {
        let input = format!("开头{}结尾", "🦀数据".repeat(100));
        let got = truncate_head_tail_with_marker(&input, 80, |n| format!("[省略{n}字]"));
        assert_eq!(got.text.chars().count(), 80);
        assert!(got.text.starts_with("开头"));
        assert!(got.text.ends_with("结尾"));
    }

    #[test]
    fn idem_cache_replay_returns_same_id() {
        let mut c = IdemCache::new(4, IDEM_TTL);
        let k = IdemCache::scoped("demo", "key-1");
        assert!(c.get(&k).is_none());
        c.put(k.clone(), "s5".into());
        assert_eq!(c.get(&k).as_deref(), Some("s5"));
    }

    #[test]
    fn idem_cache_evicts_oldest_over_cap() {
        let mut c = IdemCache::new(2, IDEM_TTL);
        c.put("a".into(), "1".into());
        c.put("b".into(), "2".into());
        c.put("c".into(), "3".into()); // evicts "a"
        assert!(c.get("a").is_none());
        assert_eq!(c.get("b").as_deref(), Some("2"));
        assert_eq!(c.get("c").as_deref(), Some("3"));
    }

    #[test]
    fn idem_cache_ttl_expires() {
        let mut c = IdemCache::new(4, Duration::from_millis(1));
        c.put("a".into(), "1".into());
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.get("a").is_none());
    }

    #[test]
    fn deny_reason_tags() {
        assert_eq!(DenyReason::Depth.tag(), "depth");
        assert_eq!(DenyReason::Budget.tag(), "budget");
        assert_eq!(DenyReason::Cycle.tag(), "cycle");
    }
}
