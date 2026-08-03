//! v0.9.0 W2 (F2/F5/F7) — delegation support types + pure helpers that don't
//! need the `Gateway`'s private state (those methods live in `gateway.rs`).
//!
//! Holds: the pump → notifier signal, the bounded/TTL'd idempotency cache
//! (in-memory only — honest: does NOT survive a restart), the completion
//! notification text builder, and the trailing-24h cost / budget helpers the
//! Ambient budget gate reads OFF the gateway lock (shared with the web
//! `/status` route so there is one aggregation, `web → im`, never `im → web`).

use std::collections::{BTreeMap, HashMap, VecDeque};
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
///   `TurnFailed` / `Error`): the child is now IDLE, waiting for the next
///   dispatch. This is the default (`final`) notification point.
#[derive(Debug, Clone)]
pub struct DelegationSignal {
    /// The child session whose message/turn just completed.
    pub child_sid: String,
    /// The mirrored turn id carrying this signal's `tail` (the dedup key;
    /// for a boundary signal this is the turn holding the FINAL answer).
    pub turn_id: String,
    /// Full assistant text. The notification builder applies its own bounded
    /// head/tail excerpt; the durable session ledger retains this verbatim.
    pub tail: String,
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
}

/// The `notified_turns` key recording that a turn's BOUNDARY notification was
/// handled. Distinct from the plain turn-id key (used by interim/`all`
/// notifications) so an `all`-mode watch still gets its "task finished, child
/// idle" wake-up after having been notified of the same turn's text.
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

/// Max characters of child answer embedded in a completion notification.
pub const NOTIFICATION_ANSWER_MAX_CHARS: usize = 4_000;

pub(crate) fn full_answer_marker(omitted: usize, child_sid: &str) -> String {
    format!(
        "…[truncated {omitted} chars — full text stays in the session ledger; run session_collect{{sid:{child_sid}, tail:true}} for the full answer]…"
    )
}

/// Build the task-completion notification delivered to the parent when the
/// child's vendor turn finishes. English, `[ccteam]`-prefixed, names the child
/// sid + vendor + optional title + turn id, includes a truncated final answer,
/// and — the v0.9.5 feedback fix — states EXPLICITLY that the child is now
/// idle and waiting, so a parent can always tell "task done / child stopped"
/// from mere progress narration. This is a routed user-role message (NOT a
/// system-prompt injection); `title` is a label only.
pub fn build_notification_text(
    child_sid: &str,
    vendor: AgentVendor,
    title: Option<&str>,
    turn_id: &str,
    assistant_text: &str,
    interim_notes: usize,
) -> String {
    build_notification_text_with_outcome(
        child_sid,
        vendor,
        title,
        turn_id,
        assistant_text,
        interim_notes,
        false,
    )
}

/// Outcome-aware completion notification used by the gateway. Keeping the
/// ordinary public builder as a success wrapper makes its existing output a
/// byte-for-byte compatibility contract while structured vendor failures get
/// an unmistakable leading marker.
pub(crate) fn build_notification_text_with_outcome(
    child_sid: &str,
    vendor: AgentVendor,
    title: Option<&str>,
    turn_id: &str,
    assistant_text: &str,
    interim_notes: usize,
    vendor_error: bool,
) -> String {
    let label = title
        .filter(|t| !t.is_empty())
        .map(|t| format!(" \"{t}\""))
        .unwrap_or_default();
    let excerpt = truncate_head_tail_with_marker(
        assistant_text.trim(),
        NOTIFICATION_ANSWER_MAX_CHARS,
        |omitted| full_answer_marker(omitted, child_sid),
    );
    let folded = if interim_notes > 0 {
        format!(
            " {interim_notes} interim note(s) from this turn stayed in the ledger \
             (session_collect{{sid:{child_sid}}} pages the full trail)."
        )
    } else {
        String::new()
    };
    let outcome = if vendor_error {
        "[delegation completed with VENDOR ERROR] "
    } else {
        ""
    };
    format!(
        "[ccteam] {outcome}delegated session {child_sid} ({}{label}) completed turn {turn_id} and is now IDLE, waiting for the next dispatch.{folded}\n\
         --- final answer ---\n{}\n\
         (child is idle: if the task is not actually finished, follow up with session_dispatch{{sid:{child_sid}, task:…}}; run session_collect{{sid:{child_sid}, tail:true}} for the full answer)",
        vendor_key(vendor),
        excerpt.text,
    )
}

/// Build the interim-note notification (an `all`-mode watch only): the child
/// posted an assistant message but its vendor turn is STILL RUNNING — labeled
/// so the parent can safely skim it without mistaking it for completion.
pub fn build_interim_notification_text(
    child_sid: &str,
    vendor: AgentVendor,
    title: Option<&str>,
    turn_id: &str,
    assistant_text: &str,
) -> String {
    let label = title
        .filter(|t| !t.is_empty())
        .map(|t| format!(" \"{t}\""))
        .unwrap_or_default();
    let excerpt = truncate_head_tail_with_marker(
        assistant_text.trim(),
        NOTIFICATION_ANSWER_MAX_CHARS,
        |omitted| full_answer_marker(omitted, child_sid),
    );
    format!(
        "[ccteam] delegated session {child_sid} ({}{label}) posted an interim note (turn {turn_id}) — still WORKING, no action needed.\n\
         --- note ---\n{}",
        vendor_key(vendor),
        excerpt.text,
    )
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

// ── cost / budget helpers (off-lock; shared with web /status) ────────────────

/// Fleet-wide trailing-24h cost: total + per-vendor. The one aggregation the
/// web `/status` route and the delegation budget gate share (both key off the
/// `agent_done` events `ccteam_core::queries::cost_summary` owns).
pub fn fleet_cost_24h(paths: &CcteamPaths) -> (f64, BTreeMap<String, f64>) {
    let mut total = 0.0_f64;
    let mut by_vendor: BTreeMap<String, f64> = BTreeMap::new();
    for project in ccteam_core::queries::collect_projects(paths).unwrap_or_default() {
        let slug = &project.state.slug;
        let summary = ccteam_core::queries::cost_summary(slug, &paths.progress_jsonl(slug), paths)
            .unwrap_or_default();
        total += summary.cost_24h_usd;
        for (vendor, usd) in summary.cost_24h_by_vendor {
            *by_vendor.entry(vendor).or_insert(0.0) += usd;
        }
    }
    (total, by_vendor)
}

/// Fleet-wide delegation observability for `GET /status`:
/// `(active_watches, notified_24h, denied_24h)`. Mirrors [`fleet_cost_24h`]'s
/// per-project sweep so the web status route shares ONE aggregation
/// (`web → im`, never `im → web`). `active_watches` = current on-disk
/// `delegation.json` count (no window); the two counts filter the project
/// `progress.jsonl` by event kind within the trailing 24h (a missing/unparseable
/// `ts` counts as recent, matching `cost_summary`).
pub fn fleet_delegations(paths: &CcteamPaths) -> (u32, u32, u32) {
    use ccteam_harness::execution::progress_bridge::{DELEGATION_DENIED, DELEGATION_NOTIFIED};
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
    let mut active = 0u32;
    let mut notified = 0u32;
    let mut denied = 0u32;
    for project in ccteam_core::queries::collect_projects(paths).unwrap_or_default() {
        let slug = &project.state.slug;
        let dir = paths.project_dir(slug);
        active += ccteam_harness::scan_delegation_watches(&dir).len() as u32;
        for ev in
            ccteam_core::progress::read_all_events(&paths.progress_jsonl(slug)).unwrap_or_default()
        {
            let kind = ev.get("event").and_then(|v| v.as_str()).unwrap_or("");
            if kind != DELEGATION_NOTIFIED && kind != DELEGATION_DENIED {
                continue;
            }
            let recent = ev
                .get("ts")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&chrono::Utc) >= cutoff)
                .unwrap_or(true);
            if !recent {
                continue;
            }
            if kind == DELEGATION_NOTIFIED {
                notified += 1;
            } else {
                denied += 1;
            }
        }
    }
    (active, notified, denied)
}

/// Trailing-24h cost for one vendor in one project (the budget gate's number).
pub fn project_vendor_cost_24h(paths: &CcteamPaths, slug: &str, vendor: AgentVendor) -> f64 {
    let summary = ccteam_core::queries::cost_summary(slug, &paths.progress_jsonl(slug), paths)
        .unwrap_or_default();
    summary
        .cost_24h_by_vendor
        .get(vendor_key(vendor))
        .copied()
        .unwrap_or(0.0)
}

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

/// True when the vendor's trailing-24h project cost has reached its configured
/// cap. No cap configured → `false` (never gates). grok/opencode/kimi have no
/// price table (cost aggregates to 0) so the gate is naturally inert for them.
pub fn budget_exceeded(paths: &CcteamPaths, slug: &str, vendor: AgentVendor) -> bool {
    match project_vendor_budget_cap(paths, slug, vendor) {
        Some(cap) => project_vendor_cost_24h(paths, slug, vendor) >= cap,
        None => false,
    }
}

// ── guardrail denial reasons ────────────────────────────────────────────────

/// Why an Ambient delegation was denied — the `reason` tag on the
/// `delegation_denied` progress event + the human-readable error.
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
}

impl DenyReason {
    /// The stable lowercase tag used in the `delegation_denied{reason}` event
    /// + the human-readable error.
    pub fn tag(self) -> &'static str {
        match self {
            DenyReason::Depth => "depth",
            DenyReason::Children => "children",
            DenyReason::Delegated => "delegated",
            DenyReason::Cycle => "cycle",
            DenyReason::Budget => "budget",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_states_task_done_and_idle() {
        let t = build_notification_text(
            "s7",
            AgentVendor::Grok,
            Some("research"),
            "s7-3",
            "hello",
            0,
        );
        assert!(t.starts_with(
            "[ccteam] delegated session s7 (grok \"research\") completed turn s7-3 and is now IDLE"
        ));
        assert!(t.contains("hello"));
        assert!(t.contains("session_collect{sid:s7, tail:true}"));
        // Single-message turn → no interim-fold sentence.
        assert!(!t.contains("interim note(s)"));
        assert_eq!(
            t,
            "[ccteam] delegated session s7 (grok \"research\") completed turn s7-3 and is now IDLE, waiting for the next dispatch.\n\
             --- final answer ---\n\
             hello\n\
             (child is idle: if the task is not actually finished, follow up with session_dispatch{sid:s7, task:…}; run session_collect{sid:s7, tail:true} for the full answer)"
        );
    }

    #[test]
    fn notification_text_folds_interim_notes() {
        let t = build_notification_text("s69", AgentVendor::Codex, None, "s69-54", "wave done", 53);
        assert!(t.contains("is now IDLE, waiting for the next dispatch"));
        assert!(t.contains("53 interim note(s) from this turn stayed in the ledger"));
        assert!(t.contains("session_dispatch{sid:s69"));
    }

    #[test]
    fn interim_notification_text_states_still_working() {
        let t = build_interim_notification_text(
            "s69",
            AgentVendor::Codex,
            Some("wave"),
            "s69-3",
            "reading queue",
        );
        assert!(t.starts_with("[ccteam] delegated session s69 (codex \"wave\") posted an interim note (turn s69-3) — still WORKING"));
        assert!(t.contains("no action needed"));
        assert!(t.contains("reading queue"));
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
        let t = build_notification_text("s1", AgentVendor::Claude, None, "s1-1", &long, 0);
        assert!(t.contains("HEAD"));
        assert!(t.contains("TAIL"));
        assert!(t.contains("truncated"));
        assert!(t.contains("session_collect{sid:s1, tail:true}"));
        assert!(t.contains("(claude)"));
        let embedded = t
            .split_once("--- final answer ---\n")
            .unwrap()
            .1
            .split_once("\n(child is idle:")
            .unwrap()
            .0;
        assert_eq!(embedded.chars().count(), NOTIFICATION_ANSWER_MAX_CHARS);
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
