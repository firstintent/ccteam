//! Durable delegation requests: the crash-safe record of every dispatch a
//! parent has outstanding against one child.
//!
//! Lives at `<project>/.ccteam/chat/<child_sid>/delegation.json`, written with
//! the same `atomic_write_durable` (tmp+fsync+rename) discipline as `meta.json`
//! so a power-loss can never leave a half-written record. A child holds MANY
//! requests at once (issue #201: one watch per child meant a second dispatch
//! silently took over the first one's parent, notify mode and title, and the
//! first task's completion was reported under the second task's name). Each
//! request carries its own identity, its own parent and its own notify mode
//! from the moment ccteam accepts it — before the vendor is written to — and is
//! resolved only by the execution turn it is BOUND to, never by recency,
//! timestamp or queue position.
//!
//! The gateway keeps an in-memory mirror for the hot path; this file is the SoT
//! that a daemon-restart reconcile reads to deliver any completion
//! notifications that were missed while the daemon was down (at-least-once,
//! deduped by `notified_turns`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::fs_atomic::atomic_write_durable;
use super::turns_mirror::chat_dir;
use crate::TurnRouting;

/// When a delegation request wakes its parent. The unit of the notification
/// contract is the TASK (a vendor turn), not each mirrored assistant message —
/// a chatty child (codex narrates checkpoints as separate messages inside one
/// turn) must not flood the parent's context. In every mode a request covers
/// exactly ONE task: the boundary of the turn it is BOUND to resolves it and
/// nothing else, so a child that keeps living its own life after the task — an
/// IM root, a session someone else drives — never keeps feeding the
/// dispatcher, and a second dispatch never inherits the first one's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyMode {
    /// Notify once, at the vendor turn boundary — the child finished the
    /// dispatched task and went idle — with the LONGER excerpt (interim
    /// narration stays in the ledger either way).
    Final,
    /// Same boundary as [`Self::Final`], a SHORTER excerpt — the default. The
    /// wake-up point is a property of the task; how much of the answer rides
    /// along is the parent's context budget, so the two are separate axes and
    /// only the cap differs here. Brief by default because the parent is the
    /// scarcest context in a team (issue #194: a planner paid 2000 chars ×
    /// twenty children for reports it only needed the verdict line of); the
    /// whole answer is always one exact `agent_read` call away, and a parent
    /// that wants it pushed asks for `final`.
    #[default]
    Brief,
    /// Never notify — ledger-only, the parent polls `agent_read`.
    Off,
}

/// The wire refusal for the retired `all` mode.
///
/// It promised a notification per mirrored assistant message; since the
/// notification unit became the TASK (v0.9.5) the notifier has skipped every
/// non-boundary signal unconditionally, so `all` was `final` wearing another
/// name — a decision trap that cost callers a choice and bought them nothing.
/// Removed rather than kept as an alias (pre-1.0: no compat shims).
const NOTIFY_ALL_REMOVED: &str = "notify `all` was removed; use final";

impl NotifyMode {
    /// Stable lowercase wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyMode::Final => "final",
            NotifyMode::Brief => "brief",
            NotifyMode::Off => "off",
        }
    }

    /// Parse a WIRE value: `"final"|"brief"|"off"` — plus the boolean form
    /// (`true` → `Final`, `false` → `Off`) so existing callers keep working.
    /// The retired `"all"` is a readable error here; only the state-file
    /// [`Deserialize`] impl still accepts it (as `Final`, which is what it
    /// always did).
    pub fn parse_value(v: &serde_json::Value) -> Result<Self, String> {
        match v {
            serde_json::Value::Bool(true) => Ok(NotifyMode::Final),
            serde_json::Value::Bool(false) => Ok(NotifyMode::Off),
            serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "final" | "true" => Ok(NotifyMode::Final),
                "brief" => Ok(NotifyMode::Brief),
                "off" | "false" | "none" => Ok(NotifyMode::Off),
                "all" => Err(NOTIFY_ALL_REMOVED.to_string()),
                other => Err(format!(
                    "invalid notify mode `{other}` (expected `final` | `brief` | `off`)"
                )),
            },
            other => Err(format!(
                "invalid notify value {other} (expected `final` | `brief` | `off` or a boolean)"
            )),
        }
    }
}

impl Serialize for NotifyMode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NotifyMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // One parser for the wire and for the state file. The retired `all`
        // used to be tolerated here so a `delegation.json` written before its
        // removal still loaded; that shape is now rejected wholesale by
        // [`DELEGATION_SCHEMA`], so the exception has nothing left to protect.
        let v = serde_json::Value::deserialize(d)?;
        NotifyMode::parse_value(&v).map_err(serde::de::Error::custom)
    }
}

/// Where one accepted dispatch stands. The four facts a parent needs are
/// distinct on purpose (issue #201): ccteam accepting a request, ccteam
/// retaining it in a queue, the bytes reaching the harness, and the harness
/// being observed running it are four different claims, and a stdin flush is
/// not proof the model read anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// Persisted by ccteam; the harness has not been written to yet. After a
    /// crash here delivery is UNKNOWN, never "not delivered".
    Accepted,
    /// Retained in a FIFO ahead of the harness — the vendor has not seen it.
    Queued,
    /// Written to the harness. Nothing has been observed executing it yet.
    Submitted,
    /// The harness opened the turn this request is bound to.
    Executing,
    /// That turn completed normally.
    Answered,
    /// That turn ended in a vendor error.
    Failed,
    /// The turn was cut short (an explicit stop).
    Interrupted,
    /// Confirmed never handed to the vendor.
    Undelivered,
}

impl RequestState {
    /// Terminal states no longer wait for a turn boundary.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RequestState::Answered
                | RequestState::Failed
                | RequestState::Interrupted
                | RequestState::Undelivered
        )
    }

    /// Stable lowercase wire token (the `agent_read{sid}` request rows).
    pub fn as_str(self) -> &'static str {
        match self {
            RequestState::Accepted => "accepted",
            RequestState::Queued => "queued",
            RequestState::Submitted => "submitted",
            RequestState::Executing => "executing",
            RequestState::Answered => "answered",
            RequestState::Failed => "failed",
            RequestState::Interrupted => "interrupted",
            RequestState::Undelivered => "undelivered",
        }
    }
}

/// One accepted dispatch, with the identity everything else keys off.
///
/// Minted and written to disk BEFORE the vendor submit, so a crash in the
/// window between acceptance and delivery leaves a request that says exactly
/// that instead of vanishing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRequest {
    /// Opaque, monotone-per-process request identity (`req-<nanos>-<seq>`).
    /// The ONLY key a completion is matched on.
    pub request_id: String,
    /// The sid of the session to notify when this request's turn completes
    /// (the dispatcher's principal — usually, but not necessarily, the
    /// spawner). Per REQUEST: two parents dispatching to one child each get
    /// their own answer.
    pub parent_sid: String,
    /// When completion delivers a notification turn to `parent_sid` — see
    /// [`NotifyMode`]. Per request: a follow-up that named no mode inherits
    /// this parent's most recent outstanding request on this child, so a
    /// deliberate `final` is never silently downgraded to the default.
    pub notify: NotifyMode,
    /// Optional short label carried into the notification / visualization
    /// (ledger-only — NEVER concatenated into any dispatched prompt). Belongs
    /// to THIS request; a later dispatch cannot rename it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The routing the dispatcher asked for. What the adapter actually did is
    /// [`Self::state`] + [`Self::turn_id`].
    pub routing: TurnRouting,
    /// The caller's idempotency key, when it supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// ISO-8601 acceptance time (diagnostic; NEVER a matching key).
    pub created_at: String,
    pub state: RequestState,
    /// 1-based waiting position while [`RequestState::Queued`], when the
    /// adapter can observe its own queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
    /// The adapter's EXECUTION turn id this request is bound to. Several
    /// injected requests legitimately share one; a queued request carries the
    /// id of the turn its parked line will open (minted at park time and kept
    /// across a restart, so a reconcile rebinds by identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// The `turns.jsonl` row id that answered it — what an exact re-read of
    /// this request's own answer selects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_turn: Option<String>,
    /// Whether the parent has been told. Separate from `state` so an
    /// at-least-once redelivery after a restart cannot double-notify.
    #[serde(default)]
    pub notified: bool,
    /// Why this request's binding could not be made durable, when that
    /// happened. NEVER persisted — the whole point is that the write failed —
    /// and never read back: it exists so the live surfaces report `unknown`
    /// instead of a confident `queued` for a request whose correlation would
    /// not survive a restart.
    #[serde(skip)]
    pub bind_error: Option<String>,
}

impl DelegationRequest {
    /// A freshly accepted request: identity and parent are fixed here, before
    /// anything is written to the vendor.
    pub fn accepted(
        parent_sid: impl Into<String>,
        notify: NotifyMode,
        title: Option<String>,
        routing: TurnRouting,
        idempotency_key: Option<String>,
    ) -> Self {
        Self {
            request_id: mint_request_id(),
            parent_sid: parent_sid.into(),
            notify,
            title,
            routing,
            idempotency_key,
            created_at: chrono::Utc::now().to_rfc3339(),
            state: RequestState::Accepted,
            queue_position: None,
            turn_id: None,
            answered_turn: None,
            notified: false,
            bind_error: None,
        }
    }
}

static REQUEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Process-unique request identity. Time-ordered for readability only —
/// nothing ever matches on the timestamp.
pub fn mint_request_id() -> String {
    let seq = REQUEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req-{nanos:x}-{seq:x}")
}

/// The on-disk shape version. A file that does not carry exactly this is
/// unreadable, logged and ignored — pre-1.0 there is no migration, and a
/// half-understood watch is worse than none (it would deliver one parent's
/// answer to another).
pub const DELEGATION_SCHEMA: u32 = 2;

/// Every request one child holds, outstanding and recently resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRequests {
    /// Rejects any older shape outright (see [`DELEGATION_SCHEMA`]).
    pub schema: u32,
    /// Outstanding requests first, in acceptance order, then a bounded tail of
    /// resolved ones so a parent that reads late still sees what happened.
    pub requests: Vec<DelegationRequest>,
    /// Completed child turns already notified — the at-least-once dedup set. A
    /// reconcile after a daemon restart delivers only the completed turns NOT
    /// already in here.
    #[serde(default)]
    pub notified_turns: Vec<String>,
}

/// How many resolved requests are kept per child. Enough that a parent which
/// slept through several completions can still read what happened, bounded so
/// a long-lived child's file cannot grow without limit.
pub const RESOLVED_REQUEST_HISTORY: usize = 20;

/// How many delivered turn keys the at-least-once dedup set remembers.
///
/// It used to remember all of them, so a child that ran for weeks carried
/// every turn it had ever answered in a file rewritten on each dispatch. The
/// oldest are dropped first, and dropping one is safe: the set only guards a
/// restart reconcile, which delivers nothing for a turn whose request is no
/// longer outstanding — and a notified turn's request is terminal by
/// definition. The bound is far larger than any plausible
/// crash-window backlog so the guard keeps working where it matters.
pub const NOTIFIED_TURN_HISTORY: usize = 200;

impl Default for DelegationRequests {
    fn default() -> Self {
        Self {
            schema: DELEGATION_SCHEMA,
            requests: Vec::new(),
            notified_turns: Vec::new(),
        }
    }
}

impl DelegationRequests {
    /// Append a newly accepted request (never replaces an existing one).
    pub fn accept(&mut self, request: DelegationRequest) {
        self.requests.push(request);
        self.prune();
    }

    pub fn get(&self, request_id: &str) -> Option<&DelegationRequest> {
        self.requests.iter().find(|r| r.request_id == request_id)
    }

    pub fn get_mut(&mut self, request_id: &str) -> Option<&mut DelegationRequest> {
        self.requests
            .iter_mut()
            .find(|r| r.request_id == request_id)
    }

    /// Requests still waiting for an answer, in acceptance order.
    pub fn outstanding(&self) -> impl Iterator<Item = &DelegationRequest> {
        self.requests.iter().filter(|r| !r.state.is_terminal())
    }

    /// Outstanding requests bound to `turn_id` — exactly the ones a boundary
    /// on that turn resolves. Never falls back to "the newest": an unbound
    /// request belongs to no turn yet, and guessing is how one task's answer
    /// was reported under another task's name.
    pub fn bound_to<'a>(&'a self, turn_id: &'a str) -> impl Iterator<Item = &'a DelegationRequest> {
        self.requests
            .iter()
            .filter(move |r| !r.state.is_terminal() && r.turn_id.as_deref() == Some(turn_id))
    }

    /// This parent's most recent outstanding request on this child — the
    /// precedent an omitted `notify` inherits.
    pub fn latest_outstanding_for(&self, parent_sid: &str) -> Option<&DelegationRequest> {
        self.requests
            .iter()
            .rev()
            .find(|r| !r.state.is_terminal() && r.parent_sid == parent_sid)
    }

    /// Keep every outstanding request and only the newest resolved ones.
    fn prune(&mut self) {
        let resolved = self
            .requests
            .iter()
            .filter(|r| r.state.is_terminal())
            .count();
        if resolved <= RESOLVED_REQUEST_HISTORY {
            return;
        }
        let mut drop_count = resolved - RESOLVED_REQUEST_HISTORY;
        self.requests.retain(|r| {
            if drop_count > 0 && r.state.is_terminal() {
                drop_count -= 1;
                false
            } else {
                true
            }
        });
    }

    /// Record a delivered boundary so a restart reconcile never repeats it,
    /// keeping only the most recent [`NOTIFIED_TURN_HISTORY`].
    pub fn record_notified(&mut self, turn_key: &str) {
        if self.notified_turns.iter().any(|seen| seen == turn_key) {
            return;
        }
        self.notified_turns.push(turn_key.to_string());
        let over = self
            .notified_turns
            .len()
            .saturating_sub(NOTIFIED_TURN_HISTORY);
        if over > 0 {
            self.notified_turns.drain(..over);
        }
    }

    /// True when nothing is left to remember (the file can go).
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty() && self.notified_turns.is_empty()
    }
}

/// `<project>/.ccteam/chat/<child_sid>/delegation.json`.
pub fn delegation_path(project_dir: &Path, child_sid: &str) -> PathBuf {
    chat_dir(project_dir, child_sid).join("delegation.json")
}

/// Read one child's requests. A missing file is `None`; a file this build
/// cannot read is logged and treated as `None` (fail-closed: an unparseable
/// record delivers nothing rather than delivering it to the wrong parent).
pub fn read_delegation_requests(project_dir: &Path, child_sid: &str) -> Option<DelegationRequests> {
    let path = delegation_path(project_dir, child_sid);
    let raw = std::fs::read_to_string(&path).ok()?;
    parse_delegation_requests(&raw, &path)
}

fn parse_delegation_requests(raw: &str, path: &Path) -> Option<DelegationRequests> {
    match serde_json::from_str::<DelegationRequests>(raw) {
        Ok(store) if store.schema == DELEGATION_SCHEMA => Some(store),
        Ok(store) => {
            tracing::warn!(
                path = %path.display(),
                schema = store.schema,
                want = DELEGATION_SCHEMA,
                "delegation record has an unreadable schema; ignoring it"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "delegation record is unreadable; ignoring it"
            );
            None
        }
    }
}

/// Durably write one child's requests (tmp+fsync+rename — same discipline as
/// `meta.json`). An empty store removes the file.
pub fn write_delegation_requests(
    project_dir: &Path,
    child_sid: &str,
    store: &DelegationRequests,
) -> Result<()> {
    let path = delegation_path(project_dir, child_sid);
    if store.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(path.parent().expect("path has parent"))?;
    atomic_write_durable(&path, serde_json::to_string_pretty(store)?.as_bytes())
}

/// Drop every request for `child_sid` (best-effort — used when the child is
/// gone and nothing can ever fire).
pub fn remove_delegation_requests(project_dir: &Path, child_sid: &str) {
    let _ = std::fs::remove_file(delegation_path(project_dir, child_sid));
}

/// Scan `<project>/.ccteam/chat/*/delegation.json` and return every readable
/// `(child_sid, requests)` pair (child_sid = the chat dir name). Used by the
/// daemon-startup reconcile.
pub fn scan_delegation_requests(project_dir: &Path) -> Vec<(String, DelegationRequests)> {
    let chat_base = project_dir.join(".ccteam").join("chat");
    let Ok(entries) = std::fs::read_dir(&chat_base) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in entries.flatten() {
        let Some(child_sid) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let path = entry.path().join("delegation.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(store) = parse_delegation_requests(&raw, &path) {
            out.push((child_sid, store));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn accepted(parent: &str, notify: NotifyMode, title: Option<&str>) -> DelegationRequest {
        DelegationRequest::accepted(
            parent,
            notify,
            title.map(str::to_string),
            TurnRouting::Inject,
            None,
        )
    }

    #[test]
    fn write_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, Some("research")));
        write_delegation_requests(tmp.path(), "s2", &store).unwrap();
        let back = read_delegation_requests(tmp.path(), "s2").expect("requests read back");
        assert_eq!(back.requests.len(), 1);
        assert_eq!(back.requests[0].parent_sid, "s1");
        assert_eq!(back.requests[0].notify, NotifyMode::Final);
        assert_eq!(back.requests[0].title.as_deref(), Some("research"));
        assert_eq!(back.requests[0].state, RequestState::Accepted);
        assert!(back.notified_turns.is_empty());
    }

    #[test]
    fn notify_mode_wire_forms() {
        // String forms round-trip; the pre-v0.9.5 boolean form still parses.
        for (raw, want) in [
            ("\"final\"", NotifyMode::Final),
            ("\"brief\"", NotifyMode::Brief),
            ("\"off\"", NotifyMode::Off),
            ("true", NotifyMode::Final),
            ("false", NotifyMode::Off),
        ] {
            let got: NotifyMode = serde_json::from_str(raw).unwrap();
            assert_eq!(got, want, "parsing {raw}");
        }
        assert_eq!(
            serde_json::to_string(&NotifyMode::Brief).unwrap(),
            "\"brief\""
        );
        assert!(serde_json::from_str::<NotifyMode>("\"sometimes\"").is_err());
        assert!(serde_json::from_str::<NotifyMode>("3").is_err());
    }

    /// The retired `all` is refused on the WIRE with the way out spelled…
    #[test]
    fn notify_all_is_refused_on_the_wire() {
        let err = NotifyMode::parse_value(&serde_json::json!("all")).unwrap_err();
        assert_eq!(err, "notify `all` was removed; use final");
        // …and it is not a value this server will ever emit again.
        for mode in [NotifyMode::Final, NotifyMode::Brief, NotifyMode::Off] {
            assert_ne!(mode.as_str(), "all");
        }
    }

    /// A record from before the per-request shape is unreadable, not
    /// half-understood: loading a single-watch file as "the" request would
    /// hand one parent's answer to another. Fail-closed, no migration.
    #[test]
    fn a_pre_request_watch_file_is_ignored() {
        let tmp = TempDir::new().unwrap();
        let path = delegation_path(tmp.path(), "s2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"parent_sid":"s1","notify":"final","dispatched_at":"2026-01-01T00:00:00Z","notified_turns":["s2-1"]}"#,
        )
        .unwrap();
        assert!(read_delegation_requests(tmp.path(), "s2").is_none());
        assert!(scan_delegation_requests(tmp.path()).is_empty());
    }

    /// A future shape is refused just as loudly as a past one.
    #[test]
    fn a_foreign_schema_is_ignored() {
        let tmp = TempDir::new().unwrap();
        let path = delegation_path(tmp.path(), "s2");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"schema":999,"requests":[],"notified_turns":[]}"#).unwrap();
        assert!(read_delegation_requests(tmp.path(), "s2").is_none());
    }

    /// The regression this shape exists for: a second dispatch to a busy child
    /// must not take over the first one's parent, notify mode or title.
    #[test]
    fn a_second_request_never_overwrites_the_first() {
        let tmp = TempDir::new().unwrap();
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, Some("verdict")));
        write_delegation_requests(tmp.path(), "s2", &store).unwrap();

        let mut store = read_delegation_requests(tmp.path(), "s2").unwrap();
        store.accept(accepted("s9", NotifyMode::Off, Some("cleanup")));
        write_delegation_requests(tmp.path(), "s2", &store).unwrap();

        let back = read_delegation_requests(tmp.path(), "s2").unwrap();
        assert_eq!(back.requests.len(), 2);
        assert_eq!(back.requests[0].parent_sid, "s1");
        assert_eq!(back.requests[0].notify, NotifyMode::Final);
        assert_eq!(back.requests[0].title.as_deref(), Some("verdict"));
        assert_eq!(back.requests[1].parent_sid, "s9");
        assert_eq!(back.requests[1].notify, NotifyMode::Off);
        assert_ne!(back.requests[0].request_id, back.requests[1].request_id);
    }

    /// A boundary resolves the requests BOUND to that turn — never the newest
    /// one, and never an unbound one.
    #[test]
    fn bound_to_selects_only_that_turns_requests() {
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, Some("A")));
        store.accept(accepted("s1", NotifyMode::Brief, Some("B")));
        store.accept(accepted("s7", NotifyMode::Final, Some("C")));
        let ids: Vec<String> = store
            .requests
            .iter()
            .map(|r| r.request_id.clone())
            .collect();
        store.get_mut(&ids[0]).unwrap().turn_id = Some("x-1".into());
        store.get_mut(&ids[2]).unwrap().turn_id = Some("x-1".into());
        store.get_mut(&ids[1]).unwrap().turn_id = Some("x-2".into());

        let on_first: Vec<&str> = store
            .bound_to("x-1")
            .map(|r| r.title.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(on_first, ["A", "C"]);

        // Resolved requests drop out even when the id still matches.
        store.get_mut(&ids[0]).unwrap().state = RequestState::Answered;
        let on_first: Vec<&str> = store
            .bound_to("x-1")
            .map(|r| r.title.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(on_first, ["C"]);
        assert_eq!(store.bound_to("x-9").count(), 0);
    }

    /// An omitted `notify` inherits THIS parent's precedent, not the child's
    /// last dispatch from somebody else.
    #[test]
    fn notify_precedent_is_per_parent_and_outstanding_only() {
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, None));
        store.accept(accepted("s9", NotifyMode::Off, None));
        assert_eq!(
            store.latest_outstanding_for("s1").map(|r| r.notify),
            Some(NotifyMode::Final)
        );
        assert_eq!(store.latest_outstanding_for("s4"), None);

        // Once the precedent is resolved it is no longer outstanding.
        let id = store.requests[0].request_id.clone();
        store.get_mut(&id).unwrap().state = RequestState::Answered;
        assert_eq!(store.latest_outstanding_for("s1"), None);
    }

    #[test]
    fn resolved_history_is_bounded_and_outstanding_is_never_pruned() {
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, Some("keep-me")));
        for _ in 0..(RESOLVED_REQUEST_HISTORY + 5) {
            let mut done = accepted("s1", NotifyMode::Final, None);
            done.state = RequestState::Answered;
            store.accept(done);
        }
        assert_eq!(store.outstanding().count(), 1);
        assert_eq!(
            store
                .requests
                .iter()
                .filter(|r| r.state.is_terminal())
                .count(),
            RESOLVED_REQUEST_HISTORY
        );
        assert!(store
            .requests
            .iter()
            .any(|r| r.title.as_deref() == Some("keep-me")));
    }

    #[test]
    fn scan_finds_every_child() {
        let tmp = TempDir::new().unwrap();
        for child in ["s2", "s3"] {
            let mut store = DelegationRequests::default();
            store.accept(accepted("s1", NotifyMode::Final, None));
            write_delegation_requests(tmp.path(), child, &store).unwrap();
        }
        let mut found = scan_delegation_requests(tmp.path());
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "s2");
        assert_eq!(found[1].0, "s3");
    }

    #[test]
    fn read_missing_is_none() {
        let tmp = TempDir::new().unwrap();
        assert!(read_delegation_requests(tmp.path(), "nope").is_none());
    }

    #[test]
    fn remove_drops_the_record() {
        let tmp = TempDir::new().unwrap();
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, None));
        write_delegation_requests(tmp.path(), "s2", &store).unwrap();
        remove_delegation_requests(tmp.path(), "s2");
        assert!(read_delegation_requests(tmp.path(), "s2").is_none());
    }

    /// Writing an empty store removes the file rather than leaving `{}` behind.
    #[test]
    fn an_empty_store_removes_the_file() {
        let tmp = TempDir::new().unwrap();
        let mut store = DelegationRequests::default();
        store.accept(accepted("s1", NotifyMode::Final, None));
        write_delegation_requests(tmp.path(), "s2", &store).unwrap();
        write_delegation_requests(tmp.path(), "s2", &DelegationRequests::default()).unwrap();
        assert!(!delegation_path(tmp.path(), "s2").exists());
    }

    #[test]
    fn notified_turns_dedupe() {
        let mut store = DelegationRequests::default();
        store.record_notified("s2-4#final");
        store.record_notified("s2-4#final");
        assert_eq!(store.notified_turns, vec!["s2-4#final".to_string()]);
    }

    #[test]
    fn request_ids_are_unique() {
        let a = mint_request_id();
        let b = mint_request_id();
        assert_ne!(a, b);
        assert!(a.starts_with("req-"));
    }
}
