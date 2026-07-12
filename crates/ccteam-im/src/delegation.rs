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
/// completed turn. The notifier (which owns a `GatewayHandle`) then checks the
/// child's watch and, if armed + not-yet-notified, submits the notification to
/// the parent. Carries the exact `turn_id` the pump wrote so dedup is precise.
#[derive(Debug, Clone)]
pub struct DelegationSignal {
    /// The child session whose turn just completed.
    pub child_sid: String,
    /// The exact turn id the pump durably appended (the dedup key).
    pub turn_id: String,
    /// Tail of the assistant text (already truncated to the notification cap).
    pub tail: String,
    /// The child's vendor (for the notification text + the progress event).
    pub vendor: AgentVendor,
    /// The child's host (`local` or a satellite id).
    pub host: String,
}

/// Max chars of the child's assistant text echoed into the notification tail.
pub const NOTIFICATION_TAIL_CHARS: usize = 500;

/// The last [`NOTIFICATION_TAIL_CHARS`] chars of `text` (trimmed), with a
/// leading `…` when truncated. A completion tail = the child's CONCLUSION (the
/// end of its output), so we keep the tail, not the head.
pub fn notification_tail(text: &str) -> String {
    let collapsed = text.trim();
    let chars: Vec<char> = collapsed.chars().collect();
    if chars.len() > NOTIFICATION_TAIL_CHARS {
        let start = chars.len() - NOTIFICATION_TAIL_CHARS;
        let tail: String = chars[start..].iter().collect();
        format!("…{tail}")
    } else {
        collapsed.to_string()
    }
}

/// Build the completion-notification text delivered to the parent. English,
/// `[ccteam]`-prefixed, names the child sid + vendor + optional title + turn
/// id, includes a truncated tail, and hints at `session_collect` for the full
/// output. This is a routed user-role message (NOT a system-prompt injection);
/// `title` is a label only.
pub fn build_notification_text(
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
    let tail = notification_tail(assistant_text);
    format!(
        "[ccteam] delegated session {child_sid} ({}{label}) completed turn {turn_id}.\n\
         --- tail ---\n{tail}\n\
         (use session_collect{{sid:\"{child_sid}\"}} for the full output)",
        vendor_key(vendor),
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
    }
}

fn vendor_to_cost(vendor: AgentVendor) -> ccteam_cost::Vendor {
    match vendor {
        AgentVendor::Claude => ccteam_cost::Vendor::Claude,
        AgentVendor::Codex => ccteam_cost::Vendor::Codex,
        AgentVendor::Grok => ccteam_cost::Vendor::Grok,
        AgentVendor::Opencode => ccteam_cost::Vendor::Opencode,
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
/// cap. No cap configured → `false` (never gates). grok/opencode have no price
/// table (cost aggregates to 0) so the gate is naturally inert for them.
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
    fn notification_text_shape() {
        let t = build_notification_text("s7", AgentVendor::Grok, Some("research"), "s7-3", "hello");
        assert!(
            t.starts_with("[ccteam] delegated session s7 (grok \"research\") completed turn s7-3.")
        );
        assert!(t.contains("hello"));
        assert!(t.contains("session_collect{sid:\"s7\"}"));
    }

    #[test]
    fn notification_tail_truncates() {
        let long = "x".repeat(NOTIFICATION_TAIL_CHARS + 50);
        let t = build_notification_text("s1", AgentVendor::Claude, None, "s1-1", &long);
        // The tail is capped + ellipsized; no title parens content.
        assert!(t.contains('…'));
        assert!(t.contains("(claude)"));
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
