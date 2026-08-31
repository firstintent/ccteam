//! A deterministic [`FlowClient`] for tests.
//!
//! Every timing knob is a `tokio::time` sleep, so under `tokio::time::pause()`
//! a run that "takes" ten minutes of ramp and backoff finishes in
//! microseconds and asserts exactly. Nothing here spawns a process, opens a
//! socket, or touches `$HOME`.

use crate::client::{AgentOutcome, ClientError, FlowClient, HireSpec, Hired};
use ccteam_harness::AgentVendor;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

/// One scripted answer.
#[derive(Debug, Clone, Default)]
pub struct FakeReply {
    pub text: String,
    pub cost_usd: Option<f64>,
    pub context_pct: Option<f64>,
    /// Virtual time the turn takes.
    pub delay: Duration,
    /// The turn completed but the worker failed.
    pub failed: bool,
    pub error_kind: Option<String>,
    /// Fail the *hire* with this error instead of producing a turn.
    pub hire_error: Option<ClientError>,
}

impl FakeReply {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn with_cost(mut self, usd: f64) -> Self {
        self.cost_usd = Some(usd);
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// The turn ends but the worker reports failure — resolves to `null`.
    pub fn failing(kind: impl Into<String>) -> Self {
        Self {
            failed: true,
            error_kind: Some(kind.into()),
            ..Self::default()
        }
    }

    /// The hire itself is refused.
    pub fn hire_error(err: ClientError) -> Self {
        Self {
            hire_error: Some(err),
            ..Self::default()
        }
    }
}

/// Everything the fake was asked to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum FakeCall {
    Hire {
        task: String,
        vendor: Option<AgentVendor>,
        model: Option<String>,
        role: Option<String>,
        idempotency_key: String,
    },
    FollowUp {
        sid: String,
        task: String,
    },
    Await {
        sid: String,
    },
    Stop {
        sid: String,
    },
    Usage,
}

#[derive(Default)]
struct FakeState {
    /// FIFO answers for hires, consulted after `by_needle`.
    queue: VecDeque<FakeReply>,
    /// task-substring -> answer, first match wins.
    by_needle: Vec<(String, FakeReply)>,
    /// FIFO answers for follow-ups (the schema retry path).
    follow_ups: VecDeque<FakeReply>,
    default_reply: Option<FakeReply>,
    next_sid: usize,
    live: HashSet<String>,
    peak: usize,
    calls: Vec<FakeCall>,
    by_sid: HashMap<String, FakeReply>,
    stopped: Vec<String>,
}

/// A scripted, introspectable stand-in for the daemon.
pub struct FakeClient {
    state: Mutex<FakeState>,
    usage: Mutex<Value>,
}

impl Default for FakeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeClient {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FakeState::default()),
            usage: Mutex::new(json!({ "accounts": [] })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake client mutex poisoned")
    }

    /// Answer used when nothing else matches.
    pub fn with_default_reply(&self, reply: FakeReply) -> &Self {
        self.lock().default_reply = Some(reply);
        self
    }

    /// Queue one answer for the next unmatched hire.
    pub fn push(&self, reply: FakeReply) -> &Self {
        self.lock().queue.push_back(reply);
        self
    }

    /// Answer any hire whose task contains `needle`.
    pub fn on_task(&self, needle: impl Into<String>, reply: FakeReply) -> &Self {
        self.lock().by_needle.push((needle.into(), reply));
        self
    }

    /// Queue one answer for the next follow-up turn.
    pub fn push_follow_up(&self, reply: FakeReply) -> &Self {
        self.lock().follow_ups.push_back(reply);
        self
    }

    pub fn set_usage(&self, value: Value) -> &Self {
        *self.usage.lock().expect("usage mutex poisoned") = value;
        self
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.lock().calls.clone()
    }

    pub fn hires(&self) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|c| matches!(c, FakeCall::Hire { .. }))
            .count()
    }

    /// Highest number of simultaneously-live agents observed, where "live"
    /// spans hire -> await_outcome.
    pub fn peak_concurrency(&self) -> usize {
        self.lock().peak
    }

    pub fn stopped(&self) -> Vec<String> {
        self.lock().stopped.clone()
    }

    fn pick(&self, task: &str) -> FakeReply {
        let mut st = self.lock();
        if let Some((_, reply)) = st.by_needle.iter().find(|(n, _)| task.contains(n.as_str())) {
            return reply.clone();
        }
        st.queue
            .pop_front()
            .or_else(|| st.default_reply.clone())
            .unwrap_or_else(|| FakeReply::text("ok"))
    }

    fn outcome(reply: &FakeReply) -> AgentOutcome {
        AgentOutcome {
            text: reply.text.clone(),
            cost_usd: reply.cost_usd,
            context_pct: reply.context_pct,
            failed: reply.failed,
            error_kind: reply.error_kind.clone(),
        }
    }
}

#[async_trait::async_trait]
impl FlowClient for FakeClient {
    async fn hire(&self, spec: HireSpec) -> Result<Hired, ClientError> {
        self.lock().calls.push(FakeCall::Hire {
            task: spec.task.clone(),
            vendor: spec.vendor,
            model: spec.model.clone(),
            role: spec.role.clone(),
            idempotency_key: spec.idempotency_key.clone(),
        });
        let reply = self.pick(&spec.task);
        if let Some(err) = reply.hire_error {
            return Err(err);
        }
        let mut st = self.lock();
        st.next_sid += 1;
        let sid = format!("f{}", st.next_sid);
        st.by_sid.insert(sid.clone(), reply);
        st.live.insert(sid.clone());
        let live = st.live.len();
        st.peak = st.peak.max(live);
        Ok(Hired {
            sid,
            vendor: spec.vendor,
        })
    }

    async fn follow_up(&self, sid: &str, task: &str) -> Result<AgentOutcome, ClientError> {
        let reply = {
            let mut st = self.lock();
            st.calls.push(FakeCall::FollowUp {
                sid: sid.to_string(),
                task: task.to_string(),
            });
            st.follow_ups.pop_front()
        };
        let reply = reply.unwrap_or_else(|| self.pick(task));
        if reply.delay > Duration::ZERO {
            tokio::time::sleep(reply.delay).await;
        }
        Ok(Self::outcome(&reply))
    }

    async fn await_outcome(&self, sid: &str) -> Result<AgentOutcome, ClientError> {
        let reply = {
            let mut st = self.lock();
            st.calls.push(FakeCall::Await {
                sid: sid.to_string(),
            });
            // An unknown sid is the re-attach path: the previous run hired it,
            // this process never did.
            st.by_sid
                .get(sid)
                .cloned()
                .or_else(|| st.default_reply.clone())
                .unwrap_or_else(|| FakeReply::text("reattached"))
        };
        if reply.delay > Duration::ZERO {
            tokio::time::sleep(reply.delay).await;
        }
        self.lock().live.remove(sid);
        Ok(Self::outcome(&reply))
    }

    async fn stop(&self, sid: &str) -> Result<(), ClientError> {
        let mut st = self.lock();
        st.calls.push(FakeCall::Stop {
            sid: sid.to_string(),
        });
        st.stopped.push(sid.to_string());
        st.live.remove(sid);
        Ok(())
    }

    async fn usage(&self) -> Result<Value, ClientError> {
        self.lock().calls.push(FakeCall::Usage);
        Ok(self.usage.lock().expect("usage mutex poisoned").clone())
    }
}
