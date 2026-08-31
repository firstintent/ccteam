//! The one door out of the runner.
//!
//! Every side effect a workflow can cause — hiring an agent, following up on
//! it, waiting for its turn to end, stopping it, reading account usage — goes
//! through [`FlowClient`]. The runner itself never spawns a process, opens a
//! socket or writes to a session; that keeps the scheduler, the journal and
//! the script semantics testable against a deterministic fake, and leaves F0b
//! free to implement the trait over `POST /mcp` without touching anything
//! here.
//!
//! The trait is intentionally *narrower* than the MCP tool surface: the runner
//! has no business browsing sessions or projects. It hires, follows up, waits,
//! stops, and asks about quota.

use ccteam_harness::AgentVendor;

/// What the script asked for when it called `agent(task, opts)`.
///
/// Everything except `task` is optional because ccteam's default is a roleless
/// hire (bare vendor reading the project's own `CLAUDE.md`); a workflow that
/// names nothing still gets a working agent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HireSpec {
    /// The prompt. Non-empty; the runner rejects empty tasks before it gets
    /// here.
    pub task: String,
    /// Harness to hire on. `None` lets the client pick its own default —
    /// the runner refuses to invent one (mirrors `AgentVendor` having no
    /// `Default`).
    pub vendor: Option<AgentVendor>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Project role file (`.claude/agents/<role>.md`), read by the vendor
    /// itself. The runner never injects a system prompt.
    pub role: Option<String>,
    /// Display label; also what shows up in progress events.
    pub label: Option<String>,
    pub permission_mode: Option<String>,
    /// The journal key for this call. Handed to the client so a retried
    /// dispatch after a crash is deduplicated at the daemon (the `agent`
    /// tool's `idempotency_key`) rather than double-hiring.
    pub idempotency_key: String,
}

/// A session that now exists and has been handed the task.
#[derive(Debug, Clone, PartialEq)]
pub struct Hired {
    /// Persistent ccteam sid (`s<N>`). This is the agent's identity for the
    /// rest of the run — journal, follow-ups, stop.
    pub sid: String,
    /// The harness actually used. May differ from the request when the
    /// client applies a default.
    pub vendor: Option<AgentVendor>,
}

/// The result of one worker turn.
///
/// `failed` is a *worker-side* verdict, not a transport failure: the turn
/// completed but the agent errored, was refused by policy, or produced
/// nothing usable. Both `failed: true` and a transport `ClientError` end up as
/// `null` in the script, but only the latter can drive pool backoff.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentOutcome {
    /// The worker's final text for the turn.
    pub text: String,
    /// Cost the client attributed to this turn, when it knows it. Feeds
    /// `budget.spent()` and the `max_cost_usd` brake; `None` simply
    /// contributes nothing rather than guessing.
    pub cost_usd: Option<f64>,
    /// Context window consumed, 0..100. Carried for observability only — the
    /// runner takes no scheduling decision on it (that is the session's own
    /// business).
    pub context_pct: Option<f64>,
    pub failed: bool,
    /// Short machine-ish tag for *why* it failed, when the client knows
    /// (`policy_refused`, `worker_error`, `timeout`, ...).
    pub error_kind: Option<String>,
}

impl AgentOutcome {
    /// Convenience for clients and tests: a plain successful turn.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// Why a client call did not produce an outcome at all.
///
/// The split exists for exactly one reason: [`ClientError::VendorLimit`] is
/// the only variant that means "this harness is rate/quota limited right
/// now", and that is what drives the scheduler's per-pool exponential
/// backoff. Everything else is a plain failure — retrying it immediately
/// would just fail again.
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum ClientError {
    /// Vendor quota, rate limit, or daemon backpressure. Backoff-worthy.
    #[error("harness limit: {0}")]
    VendorLimit(String),
    /// A guardrail said no (depth/children/cycle/budget/ACL). Never retried:
    /// the answer will not change.
    #[error("refused: {0}")]
    Refused(String),
    /// Anything else — transport, malformed reply, unknown sid.
    #[error("{0}")]
    Failed(String),
}

impl ClientError {
    /// True when the scheduler should put the vendor's pool into backoff.
    pub fn is_vendor_limit(&self) -> bool {
        matches!(self, ClientError::VendorLimit(_))
    }
}

/// The runner's abstract view of ccteam's A2A surface.
///
/// `async_trait` rather than native AFIT because the runner stores this as
/// `Arc<dyn FlowClient>` — the whole point is that the JS host functions do
/// not know which implementation they are talking to.
#[async_trait::async_trait]
pub trait FlowClient: Send + Sync {
    /// Spawn a session and dispatch `spec.task` to it. Returns as soon as the
    /// task is accepted; the turn is awaited separately so the scheduler can
    /// account for "hired" and "finished" independently.
    async fn hire(&self, spec: HireSpec) -> Result<Hired, ClientError>;

    /// Send another task to an existing sid and wait for that turn to end.
    /// Used both by scripts targeting a kept session (`opts.sid`) and by the
    /// schema retry loop, which must stay in the SAME session so the worker
    /// still has its own answer in context.
    async fn follow_up(&self, sid: &str, task: &str) -> Result<AgentOutcome, ClientError>;

    /// Block until the session's current turn ends. F0b implements this as
    /// `agent{wait}` plus an `agent_read{wait}` loop; the fake implements it
    /// as a virtual-time sleep.
    async fn await_outcome(&self, sid: &str) -> Result<AgentOutcome, ClientError>;

    /// Release a session. The runner calls this for every non-kept sid — a
    /// workflow that hires 200 agents must not leave 200 resident sessions
    /// behind. Stopping is a *user-explicit* action in ccteam's model, and a
    /// workflow's own hires are exactly that: the workflow asked for them.
    async fn stop(&self, sid: &str) -> Result<(), ClientError>;

    /// Account quota snapshot, passed through to `usage()` verbatim. The
    /// runner does not interpret it — scripts decide what "nearly out" means.
    async fn usage(&self) -> Result<serde_json::Value, ClientError>;
}
