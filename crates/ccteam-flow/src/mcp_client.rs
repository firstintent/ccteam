//! [`McpFlowClient`] — the real transport behind [`FlowClient`].
//!
//! Speaks ccteam's own MCP face over streamable HTTP (`POST /mcp`) carrying an
//! enrollment bearer, i.e. exactly the door a hand-started client uses. No new
//! protocol, no new credential family: a workflow is just another caller of
//! `agent` / `agent_read` / `agent_stop` / `status`.
//!
//! | trait method    | wire                                                        |
//! |-----------------|-------------------------------------------------------------|
//! | `hire`          | `agent{task, vendor?, …, title, idempotency_key, wait: 0}`   |
//! | `await_outcome` | `agent_read{sid, wait, since}` until the turn boundary       |
//! | `follow_up`     | `agent{task, sid, wait: 0}` + that same await loop           |
//! | `stop`          | `agent_stop{sid}`                                           |
//! | `usage`         | `status{detail: "usage"}` → its `usage` member                |
//!
//! Three facts about the server shape the code more than anything else:
//!
//! 1. **A tool result is JSON inside JSON.** `tools/call` answers
//!    `{result: {content: [{type: "text", text: "<compact json>"}], isError}}`,
//!    so every body is parsed twice and defensively — a server that ever
//!    answered plain prose must degrade to a failure, never to a panic.
//! 2. **A tool refusal is `isError: true` on an HTTP 200**, not a JSON-RPC
//!    error. Refusal text is the only place the *reason* exists, so it is
//!    carried verbatim into [`ClientError`] and from there into the run report.
//! 3. **`cost_usd` is the session's CUMULATIVE spend**, not this turn's. Two
//!    turns in one session would otherwise be billed as (c1) + (c1+c2). The
//!    client remembers what it already charged per sid and reports the delta.

use crate::client::{AgentOutcome, ClientError, FlowClient, HireSpec, Hired};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Seconds one `agent_read{wait}` blocks. The daemon's own ceiling is 240; ask
/// for exactly that so a quiet turn costs one request every four minutes.
const READ_WAIT_SECS: u64 = 240;

/// Turns fetched per poll. Enough to survive a burst of interim narration
/// (codex mirrors mid-turn notes as separate rows) without paging.
const READ_PAGE: u64 = 20;

/// Spin guard between polls. The long poll returns immediately whenever the
/// target is not mid-turn, which is exactly the window right after a dispatch,
/// so without this the client would busy-loop for the width of that window.
const POLL_GAP: Duration = Duration::from_secs(1);

/// Margin on top of the long-poll window before the HTTP request itself gives
/// up. A healthy `wait: 240` must never look like a network failure.
const HTTP_MARGIN: Duration = Duration::from_secs(30);

/// The `title` a hire carries is a ledger label the daemon caps at 80 chars.
const TITLE_MAX_CHARS: usize = 80;

/// Transport header the daemon answers `initialize` with and expects echoed.
const MCP_SESSION_HEADER: &str = "mcp-session-id";

/// Where to reach the daemon, and as whom.
#[derive(Debug, Clone)]
pub struct McpEndpoint {
    /// Full `POST /mcp` URL (`ccteam_harness::execution::mcp_config`
    /// resolves it from `<ccteam home>/run/mcp-url`).
    pub url: String,
    /// `ccteam-enroll:<id>:<secret>` — the machine-wide enrollment credential.
    pub bearer: String,
    /// Workspace slug. A user-scoped credential names no project, so every
    /// session-tool call has to; the first one that does binds this MCP
    /// session to that workspace for its whole life.
    pub project: String,
}

/// A [`FlowClient`] that talks to a running ccteam daemon.
pub struct McpFlowClient {
    http: reqwest::Client,
    endpoint: McpEndpoint,
    next_id: AtomicU64,
    state: Mutex<ClientState>,
    /// Give up on one turn after this long. `None` (the default) waits as long
    /// as the worker takes — a workflow's stop conditions are its brakes, not
    /// an arbitrary clock on somebody else's thinking.
    turn_timeout: Option<Duration>,
}

#[derive(Default)]
struct ClientState {
    /// The transport binding from `initialize`. Absent until the first call,
    /// and dropped whenever the daemon says the binding is gone.
    session: Option<Session>,
    /// sid → the cursor `await_outcome` must page from. Present only while a
    /// dispatch THIS process made is still unanswered; its absence is what
    /// tells `await_outcome` it is re-attaching to somebody else's dispatch.
    awaiting: HashMap<String, Option<String>>,
    /// sid → newest `turn_id` this client has seen, so a follow-up knows where
    /// the previous answer ended without re-reading.
    cursor: HashMap<String, String>,
    /// sid → cumulative session cost already charged to this run.
    charged: HashMap<String, f64>,
    /// sid → an outcome the dispatch itself already carried (the daemon may
    /// answer inline even for `wait: 0`; do not go and ask again).
    inline: HashMap<String, AgentOutcome>,
}

#[derive(Debug, Clone)]
struct Session {
    id: String,
    /// The protocol revision the server named at `initialize`. Echoed back so
    /// the transport's version gate sees a value it definitely speaks.
    protocol: Option<String>,
}

/// One HTTP answer, before it is interpreted.
struct RawResponse {
    status: reqwest::StatusCode,
    session_id: Option<String>,
    body: String,
}

/// May a failed attempt be sent again?
///
/// The distinction is not academic: `agent{task, sid}` (a follow-up) has no
/// idempotency key, so a request that was delivered but whose answer was lost
/// would hand the same worker the same task twice. Everything else the runner
/// sends is safe — reads and `agent_stop` are idempotent by nature, and a hire
/// carries the runner's own `idempotency_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    Idempotent,
    Once,
}

/// What one attempt at a `tools/call` produced.
enum Attempt {
    /// The tool's own body (already unwrapped from `content[0].text`).
    Body(Value),
    /// The binding is gone; re-`initialize` and try once more.
    Reinitialize,
    Failed(ClientError),
}

impl McpFlowClient {
    /// Build a client. Nothing is sent until the first call — construction
    /// must not fail because a daemon is momentarily down.
    pub fn new(endpoint: McpEndpoint) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(READ_WAIT_SECS) + HTTP_MARGIN)
            // An ambient HTTP_PROXY must never be consulted for a loopback
            // daemon: it turns "the daemon is right here" into a 502.
            .no_proxy()
            .build()
            .map_err(|err| ClientError::Failed(format!("http client: {err}")))?;
        Ok(Self {
            http,
            endpoint,
            next_id: AtomicU64::new(1),
            state: Mutex::new(ClientState::default()),
            turn_timeout: None,
        })
    }

    /// Cap how long one worker turn may take before the call resolves to a
    /// `timeout` failure (which the script sees as `null`, not an exception).
    pub fn with_turn_timeout(mut self, limit: Duration) -> Self {
        self.turn_timeout = Some(limit);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ClientState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    // ── transport ──────────────────────────────────────────────────────────

    async fn post(&self, body: &Value, session: Option<&Session>) -> Result<RawResponse, String> {
        let mut req = self
            .http
            .post(&self.endpoint.url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.endpoint.bearer),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(session) = session {
            req = req.header(MCP_SESSION_HEADER, session.id.as_str());
            if let Some(protocol) = &session.protocol {
                // Only ever the server's OWN answer: sending a version it does
                // not speak is a hard 400 at the transport.
                req = req.header("mcp-protocol-version", protocol.as_str());
            }
        }
        let resp = req
            .body(body.to_string())
            .send()
            .await
            .map_err(|err| format!("POST {}: {err}", self.endpoint.url))?;
        let status = resp.status();
        let session_id = resp
            .headers()
            .get(MCP_SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp
            .text()
            .await
            .map_err(|err| format!("read {} response: {err}", self.endpoint.url))?;
        Ok(RawResponse {
            status,
            session_id,
            body,
        })
    }

    /// The current binding, opening one if there is none.
    async fn session(&self) -> Result<Session, ClientError> {
        if let Some(session) = self.lock().session.clone() {
            return Ok(session);
        }
        let session = self.open_session().await?;
        self.lock().session = Some(session.clone());
        Ok(session)
    }

    async fn open_session(&self) -> Result<Session, ClientError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "initialize",
            // No `protocolVersion`: an absent one is the spec's own
            // negotiation — the server names the revision it speaks and we
            // echo that back on every later request.
            "params": {
                "capabilities": {},
                "clientInfo": { "name": "ccteam-flow", "version": env!("CARGO_PKG_VERSION") },
            },
        });
        let raw = self.post(&body, None).await.map_err(ClientError::Failed)?;
        if !raw.status.is_success() {
            return Err(ClientError::Failed(format!(
                "MCP initialize refused ({}): {}",
                raw.status,
                raw.body.trim()
            )));
        }
        let id = raw.session_id.filter(|id| !id.is_empty()).ok_or_else(|| {
            ClientError::Failed(format!(
                "MCP initialize answered without an {MCP_SESSION_HEADER} header: {}",
                raw.body.trim()
            ))
        })?;
        let protocol = serde_json::from_str::<Value>(&raw.body)
            .ok()
            .and_then(|v| {
                v.pointer("/result/protocolVersion")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|p| !p.is_empty());
        let session = Session { id, protocol };
        // Spec handshake completion. The daemon does not track it (a
        // notification answers 202 and nothing else), so a failure here is not
        // worth failing the run over.
        let notify = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let _ = self.post(&notify, Some(&session)).await;
        Ok(session)
    }

    /// One `tools/call`, with at most one retry.
    ///
    /// Two failures a second attempt fixes: a transport blip, and a binding the
    /// daemon no longer has (restart, or the idle reaper). The second is retried
    /// even under [`Retry::Once`] — a rejected binding is refused BEFORE the
    /// tool runs, so nothing was dispatched and nothing can be duplicated.
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        retry: Retry,
    ) -> Result<Value, ClientError> {
        let mut last: Option<ClientError> = None;
        for attempt in 0..2 {
            let session = self.session().await?;
            let body = json!({
                "jsonrpc": "2.0",
                "id": self.next_id(),
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            });
            let attempt_result = match self.post(&body, Some(&session)).await {
                Ok(raw) => interpret(name, raw),
                Err(err) => Attempt::Failed(ClientError::Failed(err)),
            };
            match attempt_result {
                Attempt::Body(value) => return Ok(value),
                Attempt::Reinitialize => {
                    self.forget_session(&session);
                    last = Some(ClientError::Failed(format!(
                        "{name}: the daemon no longer knows this MCP session"
                    )));
                }
                Attempt::Failed(err) => {
                    // A refusal and a harness limit are verdicts, not blips:
                    // asking again immediately gets the same answer. And a
                    // send-once call cannot be repeated at all — the request
                    // may well have landed.
                    if !matches!(err, ClientError::Failed(_)) || retry == Retry::Once {
                        return Err(err);
                    }
                    last = Some(err);
                }
            }
            if attempt == 0 {
                tokio::time::sleep(POLL_GAP).await;
            }
        }
        Err(last.unwrap_or_else(|| ClientError::Failed(format!("{name}: no answer"))))
    }

    /// Drop the cached binding — but only if nobody else already replaced it,
    /// or two calls racing a restart would tear down each other's fresh one.
    fn forget_session(&self, stale: &Session) {
        let mut state = self.lock();
        if state.session.as_ref().map(|s| s.id.as_str()) == Some(stale.id.as_str()) {
            state.session = None;
        }
    }

    // ── bookkeeping ────────────────────────────────────────────────────────

    /// Turn the session's cumulative spend into what THIS turn cost.
    fn charge(&self, sid: &str, mut outcome: AgentOutcome) -> AgentOutcome {
        let Some(total) = outcome.cost_usd else {
            return outcome;
        };
        let mut state = self.lock();
        let seen = state.charged.entry(sid.to_string()).or_insert(0.0);
        // A cumulative figure only ever grows; a smaller one means the ledger
        // was reset under us, and charging a negative delta would credit the
        // budget for money that was spent.
        let delta = (total - *seen).max(0.0);
        *seen = total;
        outcome.cost_usd = Some(delta);
        outcome
    }

    fn remember_cursor(&self, sid: &str, cursor: &str) {
        self.lock()
            .cursor
            .insert(sid.to_string(), cursor.to_string());
    }

    /// Where a follow-up's answer will start. Cached when this client has
    /// already read the session; otherwise one cheap non-blocking read.
    ///
    /// A failure here PROPAGATES rather than degrading to "no cursor": without
    /// one, the very next read would hand back the session's PREVIOUS answer as
    /// this task's result. Failing the call is the honest outcome; silently
    /// answering the wrong question is not. `Ok(None)` means the session has
    /// genuinely said nothing yet.
    async fn cursor_for(&self, sid: &str) -> Result<Option<String>, ClientError> {
        if let Some(cursor) = self.lock().cursor.get(sid).cloned() {
            return Ok(Some(cursor));
        }
        let args = json!({
            "sid": sid,
            "n": 1,
            "tail": true,
            "wait": 0,
            "project": self.endpoint.project,
        });
        let body = self
            .call_tool("agent_read", args, Retry::Idempotent)
            .await?;
        let Some(cursor) = body.get("cursor").and_then(Value::as_str) else {
            return Ok(None);
        };
        self.remember_cursor(sid, cursor);
        Ok(Some(cursor.to_string()))
    }

    // ── the turn boundary ──────────────────────────────────────────────────

    /// Block until `sid`'s turn ends, then return its final text.
    ///
    /// `since` pins where the answer must start and `require_new` says whether
    /// a turn that was already there counts. They differ for exactly one
    /// caller: `await_outcome` on a sid a PREVIOUS process dispatched to (the
    /// journal's re-attach path) has no dispatch of its own to wait for, so the
    /// newest answer IS the answer — requiring a new one would hang forever.
    ///
    /// `since` is deliberately NOT advanced inside the loop. A vendor that
    /// narrates mid-turn (codex) produces rows before its final one, and paging
    /// past them would step over the real answer whenever the turn ended in the
    /// same instant the activity snapshot was taken.
    async fn await_turn(
        &self,
        sid: &str,
        since: Option<String>,
        require_new: bool,
    ) -> Result<AgentOutcome, ClientError> {
        let deadline = self
            .turn_timeout
            .map(|limit| tokio::time::Instant::now() + limit);
        loop {
            let mut args = Map::new();
            args.insert("sid".into(), json!(sid));
            args.insert("wait".into(), json!(READ_WAIT_SECS));
            args.insert("n".into(), json!(READ_PAGE));
            args.insert("tail".into(), json!(true));
            args.insert("project".into(), json!(self.endpoint.project));
            if let Some(cursor) = &since {
                args.insert("since".into(), json!(cursor));
            }
            let body = self
                .call_tool("agent_read", Value::Object(args), Retry::Idempotent)
                .await?;

            let working = body.get("activity").and_then(Value::as_str) == Some("working");
            let rows = body
                .get("turns")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(cursor) = body.get("cursor").and_then(Value::as_str) {
                self.remember_cursor(sid, cursor);
            }
            if !working {
                if let Some(row) = rows.last() {
                    return Ok(self.charge(sid, outcome_from_read(&body, row)));
                }
                if !require_new {
                    // Re-attach to a session that is idle and has said nothing:
                    // the answer this run is waiting for will never come.
                    return Ok(AgentOutcome {
                        failed: true,
                        error_kind: Some("no_answer".into()),
                        ..AgentOutcome::default()
                    });
                }
            }
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                return Ok(AgentOutcome {
                    failed: true,
                    error_kind: Some("timeout".into()),
                    ..AgentOutcome::default()
                });
            }
            tokio::time::sleep(POLL_GAP).await;
        }
    }
}

// ── pure response handling (unit-tested against canned JSON) ────────────────

/// Unwrap one HTTP answer into the tool's own body.
fn interpret(tool: &str, raw: RawResponse) -> Attempt {
    // 429 is the transport saying "later" in the one language every proxy and
    // gateway agrees on; treat it as a harness limit so the pool backs off.
    if raw.status.as_u16() == 429 {
        return Attempt::Failed(ClientError::VendorLimit(format!(
            "{tool}: HTTP 429 {}",
            raw.body.trim()
        )));
    }
    let parsed = serde_json::from_str::<Value>(&raw.body).ok();
    if let Some(error) = parsed.as_ref().and_then(|v| v.get("error")) {
        // -32001 is the transport's own "re-initialize" signal.
        if error.get("code").and_then(Value::as_i64) == Some(-32001) {
            return Attempt::Reinitialize;
        }
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(raw.body.trim());
        return Attempt::Failed(ClientError::Failed(format!("{tool}: {message}")));
    }
    if !raw.status.is_success() {
        return Attempt::Failed(ClientError::Failed(format!(
            "{tool}: HTTP {} {}",
            raw.status,
            raw.body.trim()
        )));
    }
    let Some(result) = parsed.as_ref().and_then(|v| v.get("result")) else {
        return Attempt::Failed(ClientError::Failed(format!(
            "{tool}: unreadable answer: {}",
            raw.body.trim()
        )));
    };
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Attempt::Failed(classify_refusal(tool, text));
    }
    // The body is JSON inside a text block. A server that answered prose is
    // not a crash — it is a call that produced nothing usable.
    match serde_json::from_str::<Value>(text) {
        Ok(value) => Attempt::Body(value),
        Err(_) if text.is_empty() => Attempt::Body(Value::Null),
        Err(_) => Attempt::Failed(ClientError::Failed(format!("{tool}: {text}"))),
    }
}

/// A tool said no. Every refusal is a VERDICT, never a backoff signal: the
/// text may be a guardrail's wording, the daily-budget denial (waiting
/// 30s-10m cannot help a window that slides in hours), or a user's own
/// pre-agent policy prose — "quota low, use codex" must reach the script as
/// the reason it null'd, not spin a pool into vendor-limit backoff because it
/// contains the word "quota". `VendorLimit` is reserved for structured
/// transport signals (HTTP 429 in `interpret`); prose is never sniffed.
fn classify_refusal(tool: &str, text: &str) -> ClientError {
    let message = if text.is_empty() {
        format!("{tool}: refused")
    } else {
        format!("{tool}: {text}")
    };
    ClientError::Refused(message)
}

/// An `agent` answer that already carries the turn's result (`status` is
/// terminal). `None` for the ordinary `pending` / `queued` dispatch.
fn inline_outcome(body: &Value) -> Option<AgentOutcome> {
    let status = body.get("status").and_then(Value::as_str)?;
    let failed = match status {
        "completed" => false,
        "failed" => true,
        _ => return None,
    };
    Some(AgentOutcome {
        text: body
            .get("result_text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        cost_usd: body.get("cost_usd").and_then(Value::as_f64),
        context_pct: body.get("context_pct").and_then(Value::as_f64),
        failed,
        error_kind: failure_reason(body),
    })
}

/// One transcript row plus its session envelope → the runner's outcome.
fn outcome_from_read(body: &Value, row: &Value) -> AgentOutcome {
    let failed = row.get("outcome").and_then(Value::as_str) == Some("failed");
    AgentOutcome {
        text: row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        cost_usd: body.get("cost_usd").and_then(Value::as_f64),
        context_pct: body.get("context_pct").and_then(Value::as_f64),
        failed,
        error_kind: if failed { failure_reason(row) } else { None },
    }
}

/// `error_kind` is a tag and `error` is the sentence; a report that shows only
/// the tag ("worker_error") never says what happened, so keep both when both
/// are there.
fn failure_reason(value: &Value) -> Option<String> {
    let kind = value
        .get("error_kind")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let error = value
        .get("error")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (kind, error) {
        (Some(kind), Some(error)) => Some(format!("{kind}: {error}")),
        (Some(kind), None) => Some(kind.to_string()),
        (None, Some(error)) => Some(error.to_string()),
        (None, None) => None,
    }
}

/// Ledger labels are capped at 80 chars by the daemon; cut on a char boundary.
fn ledger_title(label: &str) -> String {
    if label.chars().count() <= TITLE_MAX_CHARS {
        return label.to_string();
    }
    label.chars().take(TITLE_MAX_CHARS).collect()
}

fn put_str(args: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
        args.insert(key.to_string(), json!(value));
    }
}

// ── the trait ───────────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl FlowClient for McpFlowClient {
    async fn hire(&self, spec: HireSpec) -> Result<Hired, ClientError> {
        let mut args = Map::new();
        args.insert("task".into(), json!(spec.task));
        // Always async: the runner accounts for "hired" and "finished"
        // separately, and an inline wait would hold a hire slot open.
        args.insert("wait".into(), json!(0));
        args.insert("project".into(), json!(self.endpoint.project));
        args.insert("idempotency_key".into(), json!(spec.idempotency_key));
        if let Some(parent) = &spec.parent_sid {
            // The delegation edge back to the managed session that launched
            // this run (the runner itself is only an enrolled client).
            args.insert("parent_sid".into(), json!(parent));
        }
        if let Some(vendor) = spec.vendor {
            args.insert("vendor".into(), json!(vendor.wire_name()));
        }
        put_str(&mut args, "model", &spec.model);
        put_str(&mut args, "effort", &spec.effort);
        put_str(&mut args, "role", &spec.role);
        put_str(&mut args, "permission_mode", &spec.permission_mode);
        if let Some(label) = spec.label.as_deref().filter(|l| !l.is_empty()) {
            args.insert("title".into(), json!(ledger_title(label)));
        }

        // Safe to retry: the runner's `idempotency_key` makes the daemon replay
        // the original call instead of hiring a second worker.
        let body = self
            .call_tool("agent", Value::Object(args), Retry::Idempotent)
            .await?;
        let sid = body
            .get("sid")
            .and_then(Value::as_str)
            .filter(|sid| !sid.is_empty())
            .ok_or_else(|| ClientError::Failed(format!("agent: answer carried no sid: {body}")))?
            .to_string();

        {
            let mut state = self.lock();
            // A fresh session has no earlier turns, so "the answer to THIS
            // dispatch" is simply "any turn at all".
            state.awaiting.insert(sid.clone(), None);
            if let Some(outcome) = inline_outcome(&body) {
                state.inline.insert(sid.clone(), outcome);
            }
        }
        Ok(Hired {
            sid,
            vendor: spec.vendor,
        })
    }

    async fn follow_up(&self, sid: &str, task: &str) -> Result<AgentOutcome, ClientError> {
        // Taken BEFORE the dispatch: a session that has already answered once
        // would otherwise hand back its previous answer the instant we asked.
        let since = self.cursor_for(sid).await?;
        let args = json!({
            "sid": sid,
            "task": task,
            "wait": 0,
            "project": self.endpoint.project,
        });
        let body = self.call_tool("agent", args, Retry::Once).await?;
        if let Some(outcome) = inline_outcome(&body) {
            return Ok(self.charge(sid, outcome));
        }
        self.await_turn(sid, since, true).await
    }

    async fn await_outcome(&self, sid: &str) -> Result<AgentOutcome, ClientError> {
        let (dispatched, inline) = {
            let mut state = self.lock();
            (state.awaiting.remove(sid), state.inline.remove(sid))
        };
        if let Some(outcome) = inline {
            return Ok(self.charge(sid, outcome));
        }
        match dispatched {
            Some(since) => self.await_turn(sid, since, true).await,
            // Re-attach: the journal says a previous run dispatched here.
            None => self.await_turn(sid, None, false).await,
        }
    }

    async fn stop(&self, sid: &str) -> Result<(), ClientError> {
        let args = json!({ "sid": sid, "project": self.endpoint.project });
        match self.call_tool("agent_stop", args, Retry::Idempotent).await {
            Ok(_) => Ok(()),
            // "There is nothing to stop" is what the caller wanted. Note that a
            // resumed run's earlier hires are NOT its descendants (it is a new
            // ledger node), so the daemon refuses those on purpose — the runner
            // logs it and moves on rather than failing the workflow.
            Err(ClientError::Refused(message)) if already_gone(&message) => Ok(()),
            Err(err) => Err(err),
        }
    }

    async fn usage(&self) -> Result<Value, ClientError> {
        let body = self
            .call_tool("status", json!({ "detail": "usage" }), Retry::Idempotent)
            .await?;
        Ok(body.get("usage").cloned().unwrap_or(Value::Null))
    }
}

/// Refusal texts that mean the session is already not running.
fn already_gone(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "already stopped",
        "unknown session",
        "no such session",
        "not found",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(status: u16, body: &str) -> RawResponse {
        RawResponse {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            session_id: None,
            body: body.to_string(),
        }
    }

    fn body_of(attempt: Attempt) -> Value {
        match attempt {
            Attempt::Body(value) => value,
            Attempt::Reinitialize => panic!("expected a body, got a re-initialize signal"),
            Attempt::Failed(err) => panic!("expected a body, got {err}"),
        }
    }

    fn error_of(attempt: Attempt) -> ClientError {
        match attempt {
            Attempt::Failed(err) => err,
            Attempt::Body(value) => panic!("expected a failure, got {value}"),
            Attempt::Reinitialize => panic!("expected a failure, got a re-initialize signal"),
        }
    }

    /// A tool body is JSON inside `content[0].text` — two decodes, not one.
    #[test]
    fn a_pending_dispatch_unwraps_to_its_body() {
        let response = r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text",
            "text":"{\"sid\":\"s7\",\"turn_id\":\"s7-1\",\"status\":\"pending\"}"}],
            "isError":false}}"#;
        let body = body_of(interpret("agent", raw(200, response)));
        assert_eq!(body["sid"], json!("s7"));
        assert_eq!(body["status"], json!("pending"));
        assert!(inline_outcome(&body).is_none(), "pending is not an outcome");
    }

    #[test]
    fn an_inline_completion_is_an_outcome() {
        let body = json!({
            "sid": "s7", "turn_id": "s7-1", "turn": 1, "status": "completed",
            "result_text": "42", "cost_usd": 0.5, "context_pct": 19,
        });
        let outcome = inline_outcome(&body).expect("terminal status is an outcome");
        assert_eq!(outcome.text, "42");
        assert_eq!(outcome.cost_usd, Some(0.5));
        assert_eq!(outcome.context_pct, Some(19.0));
        assert!(!outcome.failed);
    }

    #[test]
    fn an_inline_failure_keeps_both_the_tag_and_the_sentence() {
        let body = json!({
            "sid": "s7", "status": "failed", "result_text": "",
            "error_kind": "server_overloaded", "error": "Selected model is at capacity.",
        });
        let outcome = inline_outcome(&body).expect("failed is terminal too");
        assert!(outcome.failed);
        assert_eq!(
            outcome.error_kind.as_deref(),
            Some("server_overloaded: Selected model is at capacity.")
        );
    }

    #[test]
    fn a_transcript_row_becomes_the_turns_outcome() {
        let body = json!({
            "activity": "idle", "context_pct": 19, "cost_usd": 0.25, "cursor": "t2",
            "turns": [
                { "turn_id": "t1", "content": "thinking" },
                { "turn_id": "t2", "content": "the answer" },
            ],
        });
        let row = body["turns"].as_array().unwrap().last().unwrap();
        let outcome = outcome_from_read(&body, row);
        assert_eq!(outcome.text, "the answer");
        assert_eq!(outcome.cost_usd, Some(0.25));
        assert_eq!(outcome.context_pct, Some(19.0));
        assert!(!outcome.failed);
    }

    #[test]
    fn a_failed_transcript_row_carries_its_reason() {
        let body = json!({ "activity": "idle", "turns": [] });
        let row = json!({
            "turn_id": "t1", "content": "", "outcome": "failed",
            "error_kind": "transport", "error": "connection reset",
        });
        let outcome = outcome_from_read(&body, &row);
        assert!(outcome.failed);
        assert_eq!(
            outcome.error_kind.as_deref(),
            Some("transport: connection reset")
        );
    }

    /// The daily-budget denial is a VERDICT: backoff cannot help a window
    /// that slides in hours, so it must fail plainly with its reason intact.
    #[test]
    fn a_budget_refusal_is_a_verdict_not_a_backoff() {
        let text = "agent: delegation denied: vendor `codex` has reached its 24h budget \
                    for project `alpha` (adjust budgets or wait for the window to slide)";
        let response = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "content": [{ "type": "text", "text": text }], "isError": true },
        })
        .to_string();
        let err = error_of(interpret("agent", raw(200, &response)));
        assert!(!err.is_vendor_limit(), "{err}");
        assert!(matches!(err, ClientError::Refused(_)), "{err}");
        assert!(err.to_string().contains("24h budget"), "{err}");
    }

    /// The exact hazard that retired prose-sniffing: a user's pre-agent
    /// policy wrote "quota" in its own words. That is a reason, not a limit.
    #[test]
    fn policy_text_mentioning_quota_is_never_a_limit() {
        let response = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "content": [{ "type": "text",
                "text": "agent: delegation denied by policy: quota low, use codex" }],
                "isError": true },
        })
        .to_string();
        let err = error_of(interpret("agent", raw(200, &response)));
        assert!(!err.is_vendor_limit(), "{err}");
        assert!(err.to_string().contains("use codex"), "{err}");
    }

    #[test]
    fn http_429_is_a_vendor_limit_too() {
        let err = error_of(interpret("agent", raw(429, "slow down")));
        assert!(err.is_vendor_limit(), "{err}");
    }

    /// A guardrail refusal is never retried and its reason is the only thing
    /// the run report can show, so it must survive verbatim.
    #[test]
    fn a_guardrail_refusal_keeps_its_reason_and_is_not_a_limit() {
        let text = "agent: delegation denied: cannot dispatch a session to itself (cycle)";
        let response = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "content": [{ "type": "text", "text": text }], "isError": true },
        })
        .to_string();
        let err = error_of(interpret("agent", raw(200, &response)));
        assert!(!err.is_vendor_limit(), "{err}");
        assert!(matches!(err, ClientError::Refused(_)), "{err}");
        assert!(err.to_string().contains("(cycle)"), "{err}");
    }

    #[test]
    fn an_unknown_mcp_session_asks_for_a_re_initialize() {
        let response = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": { "code": -32001, "message": "no such MCP session" },
        })
        .to_string();
        assert!(matches!(
            interpret("agent", raw(404, &response)),
            Attempt::Reinitialize
        ));
    }

    #[test]
    fn a_401_is_a_plain_failure_carrying_the_servers_words() {
        let err = error_of(interpret(
            "agent",
            raw(
                401,
                r#"{"error":"auth required: POST /mcp accepts two families"}"#,
            ),
        ));
        assert!(matches!(err, ClientError::Failed(_)), "{err}");
        assert!(err.to_string().contains("auth required"), "{err}");
    }

    #[test]
    fn prose_where_json_was_promised_is_a_failure_not_a_panic() {
        let response = json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "content": [{ "type": "text", "text": "I am a teapot" }], "isError": false },
        })
        .to_string();
        let err = error_of(interpret("agent_read", raw(200, &response)));
        assert!(err.to_string().contains("I am a teapot"), "{err}");
    }

    /// Cumulative → per-turn. Two turns in one session must bill c1 then c2,
    /// never c1 then c1+c2.
    #[test]
    fn cumulative_session_cost_is_charged_as_a_delta() {
        let client = McpFlowClient::new(McpEndpoint {
            url: "http://127.0.0.1:1/mcp".into(),
            bearer: "ccteam-enroll:dead:beef".into(),
            project: "alpha".into(),
        })
        .expect("build client");
        let first = client.charge(
            "s1",
            AgentOutcome {
                cost_usd: Some(0.30),
                ..AgentOutcome::default()
            },
        );
        let second = client.charge(
            "s1",
            AgentOutcome {
                cost_usd: Some(0.50),
                ..AgentOutcome::default()
            },
        );
        assert_eq!(first.cost_usd, Some(0.30));
        assert_eq!(second.cost_usd, Some(0.20));
    }

    #[test]
    fn a_ledger_title_is_cut_on_a_char_boundary() {
        let label = "汉".repeat(200);
        let title = ledger_title(&label);
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn stopping_something_already_gone_is_success() {
        assert!(already_gone("agent_stop: unknown session s9"));
        assert!(already_gone("agent_stop failed: session already stopped"));
        assert!(!already_gone(
            "agent_stop: permission denied — session s9 is not a descendant"
        ));
    }
}
