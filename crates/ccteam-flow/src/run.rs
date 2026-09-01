//! The run: public entry point, the `agent()` implementation, and cleanup.
//!
//! This is where the three halves meet — the script (engine), the admission
//! rules (scheduler) and the memory (journal) — around one `Arc<dyn
//! FlowClient>`. Everything the script can observe about the outside world
//! passes through [`Runner`].

use crate::client::{AgentOutcome, ClientError, FlowClient, HireSpec};
use crate::engine::{self, EngineInput, Host};
use crate::error::FlowError;
use crate::journal::{call_key, CacheReport, Journal, JournalEntry, PrefixHit};
use crate::meta::{assert_deterministic, extract_meta, WorkflowMeta, DETERMINISM_HINT};
use crate::progress::{ProgressCallback, ProgressEvent};
use crate::scheduler::{AdmissionError, Brakes, RunControl, Scheduler, SchedulerConfig};
use crate::schema::{self, SCHEMA_RETRY_PROMPT};
use ccteam_harness::AgentVendor;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Everything `agent()` accepts. Anything else is a hard error at call time:
/// silently ignoring `{modle: 'opus'}` would cost real money on the wrong
/// model, so the menu is honest or nothing.
const AGENT_OPTS: [&str; 11] = [
    "vendor",
    "model",
    "effort",
    "role",
    "sid",
    "keep",
    "label",
    "phase",
    "permission_mode",
    "schema",
    "retry",
];

/// Where the script text comes from.
pub enum ScriptSource {
    Inline(String),
    Path(PathBuf),
}

impl ScriptSource {
    pub fn inline(source: impl Into<String>) -> Self {
        ScriptSource::Inline(source.into())
    }

    pub fn path(path: impl Into<PathBuf>) -> Self {
        ScriptSource::Path(path.into())
    }

    fn read(self) -> Result<String, FlowError> {
        match self {
            ScriptSource::Inline(s) => Ok(s),
            ScriptSource::Path(p) => std::fs::read_to_string(&p)
                .map_err(|source| FlowError::ReadScript { path: p, source }),
        }
    }
}

/// How to run one workflow.
pub struct RunConfig {
    /// The script's `args`, verbatim. `None` reaches the script as
    /// `undefined`, matching the official contract.
    pub args: Option<Value>,
    pub brakes: Brakes,
    pub scheduler: SchedulerConfig,
    /// Directory for `journal.jsonl`, `script.js`, `run.json` and spilled
    /// results. Injected, never derived from `$HOME`, so tests and the daemon
    /// cannot collide.
    pub run_dir: PathBuf,
    /// Read the existing journal in `run_dir` as a resume cache.
    pub resume: bool,
    pub client: Arc<dyn FlowClient>,
    pub progress: Option<ProgressCallback>,
    /// Host-side pause control. Clone it before the run to keep a handle.
    pub control: RunControl,
    /// Hang guard for a script that never yields (see [`EngineInput`]).
    pub watchdog: Option<Duration>,
    /// Managed session to attribute every hire to (`HireSpec.parent_sid`).
    /// The CLI defaults it from `CCTEAM_CHAT_SID` so a flow launched inside a
    /// managed session hangs its leaves under that session, not under the
    /// anonymous enrolled runner node.
    pub parent_sid: Option<String>,
}

impl RunConfig {
    pub fn new(run_dir: impl Into<PathBuf>, client: Arc<dyn FlowClient>) -> Self {
        Self {
            args: None,
            brakes: Brakes::default(),
            scheduler: SchedulerConfig::default(),
            run_dir: run_dir.into(),
            resume: false,
            client,
            progress: None,
            control: RunControl::new(),
            watchdog: None,
            parent_sid: None,
        }
    }

    /// Attribute this run's hires to a managed session (the delegation edge).
    pub fn with_parent(mut self, sid: impl Into<String>) -> Self {
        self.parent_sid = Some(sid.into());
        self
    }

    pub fn with_args(mut self, args: Value) -> Self {
        self.args = Some(args);
        self
    }

    /// Set the script-visible `budget.total` (and the brake that enforces it).
    pub fn with_budget(mut self, total_usd: f64) -> Self {
        self.brakes.budget_total = Some(total_usd);
        self
    }

    pub fn with_progress(mut self, cb: ProgressCallback) -> Self {
        self.progress = Some(cb);
        self
    }

    pub fn resuming(mut self) -> Self {
        self.resume = true;
        self
    }
}

/// One `agent()` call, as reported to the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRecord {
    pub seq: usize,
    pub key: String,
    pub label: String,
    pub phase: Option<String>,
    pub vendor: Option<AgentVendor>,
    pub sid: Option<String>,
    pub cost_usd: f64,
    /// Answered from the journal — no session, no spend.
    pub cached: bool,
    /// False when the call resolved to `null`.
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunTotals {
    /// Every `agent()` call, including ones answered from the journal.
    pub agents: usize,
    /// What THIS run spent. A fully-replayed resume reports 0.0 even though
    /// its [`AgentRecord`]s carry the original costs — the money was spent by
    /// the run that first made the calls, not by the replay.
    pub cost_usd: f64,
}

/// What a finished run tells its caller.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub meta: WorkflowMeta,
    /// The script's return value.
    pub returned: Value,
    pub agents: Vec<AgentRecord>,
    pub totals: RunTotals,
    /// The brake that stopped new admissions, if one tripped.
    pub brake: Option<String>,
    /// Set when the script itself threw.
    pub script_error: Option<String>,
    pub cache: CacheReport,
    pub run_dir: PathBuf,
}

impl RunReport {
    /// True when the script completed and nothing braked it.
    pub fn ok(&self) -> bool {
        self.script_error.is_none() && self.brake.is_none()
    }
}

/// Run a workflow to completion.
pub async fn run_workflow(script: ScriptSource, cfg: RunConfig) -> Result<RunReport, FlowError> {
    let source = script.read()?;
    let meta = extract_meta(&source)?;
    assert_deterministic(&source)?;

    let journal = Arc::new(Journal::open(&cfg.run_dir, cfg.resume)?);
    let args_value = cfg.args.clone().unwrap_or(Value::Null);
    journal.write_manifest(
        &source,
        &serde_json::to_value(&meta).unwrap_or(Value::Null),
        &args_value,
    );

    let scheduler = Arc::new(Scheduler::new(
        cfg.scheduler,
        cfg.brakes,
        cfg.control.clone(),
        cfg.progress.clone(),
    ));

    emit(
        &cfg.progress,
        ProgressEvent::RunStarted {
            name: meta.name.clone(),
            description: meta.description.clone(),
            phases: meta.phases.iter().map(|p| p.title.clone()).collect(),
        },
    );

    let runner = Arc::new(Runner {
        client: Arc::clone(&cfg.client),
        parent_sid: cfg.parent_sid.clone(),
        scheduler: Arc::clone(&scheduler),
        journal: Arc::clone(&journal),
        progress: cfg.progress.clone(),
        seq: AtomicUsize::new(0),
        phase: Mutex::new(None),
        records: Mutex::new(Vec::new()),
        sessions: Mutex::new(HashMap::new()),
    });

    let input = EngineInput {
        source,
        args_json: cfg.args.as_ref().map(|v| v.to_string()),
        budget_total: scheduler.budget_total(),
        blocked_message: DETERMINISM_HINT.to_string(),
        watchdog: cfg.watchdog,
    };

    let host: Arc<dyn Host> = Arc::clone(&runner) as Arc<dyn Host>;
    let outcome = engine::execute(input, host).await;

    // Sessions outlive the script only until here: a workflow that hired 200
    // agents must not leave 200 resident sessions behind, and that is true
    // whether the script finished, threw, or hit a brake.
    runner.release_sessions().await;

    let (returned, script_error) = match outcome {
        Ok(result) => (result.returned, result.error),
        Err(err) => {
            emit(
                &cfg.progress,
                ProgressEvent::RunFinished {
                    agents: 0,
                    cost_usd: scheduler.spent(),
                    ok: false,
                },
            );
            return Err(err);
        }
    };

    let agents = runner.take_records();
    let totals = RunTotals {
        agents: agents.len(),
        cost_usd: scheduler.spent(),
    };
    let brake = scheduler.tripped();
    emit(
        &cfg.progress,
        ProgressEvent::RunFinished {
            agents: totals.agents,
            cost_usd: totals.cost_usd,
            ok: script_error.is_none() && brake.is_none(),
        },
    );

    Ok(RunReport {
        meta,
        returned,
        agents,
        totals,
        brake,
        script_error,
        cache: journal.report(),
        run_dir: cfg.run_dir,
    })
}

fn emit(cb: &Option<ProgressCallback>, event: ProgressEvent) {
    if let Some(cb) = cb {
        cb(event);
    }
}

// ───────────────────────────────────────────────────────────────────────────
// the host side of the script globals
// ───────────────────────────────────────────────────────────────────────────

struct Runner {
    client: Arc<dyn FlowClient>,
    parent_sid: Option<String>,
    scheduler: Arc<Scheduler>,
    journal: Arc<Journal>,
    progress: Option<ProgressCallback>,
    seq: AtomicUsize,
    phase: Mutex<Option<String>>,
    records: Mutex<Vec<AgentRecord>>,
    /// sid -> still needs stopping. `false` means kept (script asked) or
    /// already stopped.
    sessions: Mutex<HashMap<String, bool>>,
}

/// Identity of one `agent()` call, fixed before anything can fail so every
/// journal line, progress event and record agrees on it.
struct CallIdent {
    seq: usize,
    key: String,
    label: String,
    vendor: Option<AgentVendor>,
}

/// What one dispatch produced.
struct Dispatched {
    /// The value handed back to the script (`null` on any worker-side
    /// failure).
    value: Value,
    sid: Option<String>,
    cost_usd: f64,
    /// Why the call resolved to `null`, when it did.
    error: Option<String>,
    /// The worker's final text, kept for the progress event even when the
    /// schema layer discarded it.
    text: String,
}

/// Parsed `agent()` options.
struct AgentOpts {
    vendor: Option<AgentVendor>,
    model: Option<String>,
    effort: Option<String>,
    role: Option<String>,
    sid: Option<String>,
    keep: bool,
    label: Option<String>,
    phase: Option<String>,
    permission_mode: Option<String>,
    schema: Option<Value>,
    retry_max: u32,
    retry_prompt: String,
}

fn env_ok(value: Value) -> String {
    json!({ "k": "ok", "v": value }).to_string()
}

fn env_throw(message: impl Into<String>) -> String {
    json!({ "k": "throw", "m": message.into() }).to_string()
}

fn parse_vendor(name: &str) -> Option<AgentVendor> {
    AgentVendor::ALL
        .iter()
        .copied()
        .find(|v| v.wire_name() == name)
}

fn vendor_menu() -> String {
    AgentVendor::ALL
        .iter()
        .map(|v| v.wire_name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Truncate on a char boundary for display labels.
fn short_label(task: &str) -> String {
    let cleaned = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= 60 {
        return cleaned;
    }
    let cut: String = cleaned.chars().take(59).collect();
    format!("{cut}…")
}

fn parse_opts(raw: &Value) -> Result<AgentOpts, String> {
    let map = match raw {
        Value::Null => serde_json::Map::new(),
        Value::Object(m) => m.clone(),
        _ => return Err("agent(task, opts) — opts must be an object".to_string()),
    };

    if let Some(unknown) = map.keys().find(|k| !AGENT_OPTS.contains(&k.as_str())) {
        return Err(format!(
            "agent() does not accept the option `{unknown}`. Supported: {}",
            AGENT_OPTS.join(", ")
        ));
    }

    let as_str = |key: &str| -> Result<Option<String>, String> {
        match map.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("agent() option `{key}` must be a string")),
        }
    };

    let vendor = match as_str("vendor")? {
        None => None,
        Some(name) => Some(parse_vendor(&name).ok_or_else(|| {
            format!(
                "agent() option `vendor` must be one of: {} (got `{name}`)",
                vendor_menu()
            )
        })?),
    };

    let keep = match map.get("keep") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("agent() option `keep` must be a boolean".to_string()),
    };

    let schema = match map.get("schema") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v.clone()),
        Some(_) => return Err("agent() option `schema` must be a JSON Schema object".to_string()),
    };

    let (retry_max, retry_prompt) = match map.get("retry") {
        None | Some(Value::Null) => (2, SCHEMA_RETRY_PROMPT.to_string()),
        Some(Value::Object(r)) => {
            for k in r.keys() {
                if k != "max" && k != "prompt" {
                    return Err(format!(
                        "agent() option `retry` does not accept `{k}`. Supported: max, prompt"
                    ));
                }
            }
            let max = match r.get("max") {
                None | Some(Value::Null) => 2,
                Some(v) => v
                    .as_u64()
                    .ok_or_else(|| "agent() option `retry.max` must be a number".to_string())?
                    .min(10) as u32,
            };
            let prompt = match r.get("prompt") {
                None | Some(Value::Null) => SCHEMA_RETRY_PROMPT.to_string(),
                Some(Value::String(s)) => s.clone(),
                Some(_) => return Err("agent() option `retry.prompt` must be a string".to_string()),
            };
            (max, prompt)
        }
        Some(_) => return Err("agent() option `retry` must be an object".to_string()),
    };

    Ok(AgentOpts {
        vendor,
        model: as_str("model")?,
        effort: as_str("effort")?,
        role: as_str("role")?,
        sid: as_str("sid")?,
        keep,
        label: as_str("label")?,
        phase: as_str("phase")?,
        permission_mode: as_str("permission_mode")?,
        schema,
        retry_max,
        retry_prompt,
    })
}

impl Runner {
    fn current_phase(&self) -> Option<String> {
        self.phase.lock().expect("phase mutex poisoned").clone()
    }

    fn take_records(&self) -> Vec<AgentRecord> {
        let mut records = self.records.lock().expect("records mutex poisoned");
        records.sort_by_key(|r| r.seq);
        records.clone()
    }

    fn note_session(&self, sid: &str, keep: bool) {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(sid.to_string(), !keep);
    }

    fn clear_session(&self, sid: &str) {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(sid.to_string(), false);
    }

    /// Stop every session this run opened and did not keep.
    async fn release_sessions(&self) {
        let pending: Vec<String> = {
            let sessions = self.sessions.lock().expect("sessions mutex poisoned");
            sessions
                .iter()
                .filter(|(_, needs_stop)| **needs_stop)
                .map(|(sid, _)| sid.clone())
                .collect()
        };
        for sid in pending {
            self.stop(&sid).await;
        }
    }

    /// Best-effort stop. A workflow's own hires are the workflow's to
    /// release; a failure here is logged, never fatal.
    async fn stop(&self, sid: &str) {
        if let Err(err) = self.client.stop(sid).await {
            tracing::warn!(%err, sid, "workflow: could not stop session");
        }
        self.clear_session(sid);
    }

    fn record(&self, record: AgentRecord) {
        self.records
            .lock()
            .expect("records mutex poisoned")
            .push(record);
    }

    /// The whole of `agent()`.
    async fn agent_call(&self, task: String, opts_raw: Value) -> String {
        if task.trim().is_empty() {
            return env_throw("agent(task) — task must be a non-empty string");
        }
        let opts = match parse_opts(&opts_raw) {
            Ok(o) => o,
            Err(message) => return env_throw(message),
        };

        let ident = CallIdent {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            key: call_key(&task, &opts_raw),
            label: opts.label.clone().unwrap_or_else(|| short_label(&task)),
            vendor: opts.vendor,
        };
        let phase = opts.phase.clone().or_else(|| self.current_phase());

        // ── resume: the journal may already know the answer ──────────────
        let hit = self.journal.lookup(ident.seq, &ident.key, &ident.label);
        if let PrefixHit::Done { result, cost_usd } = hit {
            emit(
                &self.progress,
                ProgressEvent::AgentStarted {
                    seq: ident.seq,
                    label: ident.label.clone(),
                    vendor: ident.vendor,
                    cached: true,
                },
            );
            emit(
                &self.progress,
                ProgressEvent::AgentFinished {
                    seq: ident.seq,
                    label: ident.label.clone(),
                    outcome: None,
                    cost_usd: 0.0,
                },
            );
            self.record(AgentRecord {
                seq: ident.seq,
                key: ident.key,
                label: ident.label,
                phase,
                vendor: ident.vendor,
                sid: None,
                cost_usd: cost_usd.unwrap_or(0.0),
                cached: true,
                ok: !result.is_null(),
                error: None,
            });
            return env_ok(result);
        }
        let reattach = match hit {
            PrefixHit::InFlight { sid } => Some(sid),
            _ => None,
        };

        // ── admission ────────────────────────────────────────────────────
        let admission = match self.scheduler.admit(opts.vendor).await {
            Ok(a) => a,
            Err(AdmissionError::Brake(m) | AdmissionError::QueueFull(m)) => return env_throw(m),
        };

        emit(
            &self.progress,
            ProgressEvent::AgentStarted {
                seq: ident.seq,
                label: ident.label.clone(),
                vendor: ident.vendor,
                cached: false,
            },
        );

        let done = self.dispatch(&ident, &task, &opts, reattach).await;
        drop(admission);

        emit(
            &self.progress,
            ProgressEvent::AgentFinished {
                seq: ident.seq,
                label: ident.label.clone(),
                outcome: done.error.is_none().then(|| AgentOutcome {
                    text: done.text.clone(),
                    cost_usd: Some(done.cost_usd),
                    ..AgentOutcome::default()
                }),
                cost_usd: done.cost_usd,
            },
        );
        self.record(AgentRecord {
            seq: ident.seq,
            key: ident.key,
            label: ident.label,
            phase,
            vendor: ident.vendor,
            sid: done.sid,
            cost_usd: done.cost_usd,
            cached: false,
            ok: done.error.is_none(),
            error: done.error,
        });
        env_ok(done.value)
    }

    /// Hire (or re-attach / follow up), await the turn, apply the schema
    /// contract, journal both ends, and release the session.
    ///
    /// Worker-side failures come back as `Value::Null` plus an `error`
    /// string — never as a thrown script error, because the contract the
    /// author writes against is `.filter(Boolean)`, not try/catch.
    async fn dispatch(
        &self,
        ident: &CallIdent,
        task: &str,
        opts: &AgentOpts,
        reattach: Option<String>,
    ) -> Dispatched {
        let mut cost = 0.0;

        // Three ways in: re-attach to a session the previous run left
        // mid-turn, follow up on a session the script named, or hire a new
        // one.
        let (sid, first) = if let Some(sid) = reattach {
            self.note_session(&sid, opts.keep);
            let outcome = self.client.await_outcome(&sid).await;
            (sid, outcome)
        } else if let Some(sid) = &opts.sid {
            // A script-named sid is somebody else's session; the script owns
            // its lifetime, so it is never stopped implicitly.
            self.note_session(sid, true);
            (sid.clone(), self.client.follow_up(sid, task).await)
        } else {
            match self.hire(task, opts, ident).await {
                Ok(sid) => {
                    self.note_session(&sid, opts.keep);
                    // Dispatch line: written the moment a sid exists, so a
                    // run killed here re-attaches instead of re-hiring.
                    self.journal.append(JournalEntry {
                        seq: ident.seq,
                        key: ident.key.clone(),
                        sid: Some(sid.clone()),
                        done: false,
                        result: None,
                        result_ref: None,
                        cost_usd: None,
                        label: Some(ident.label.clone()),
                        vendor: ident.vendor.map(|v| v.wire_name().to_string()),
                    });
                    let outcome = self.client.await_outcome(&sid).await;
                    (sid, outcome)
                }
                Err(message) => {
                    self.journal.append(JournalEntry {
                        seq: ident.seq,
                        key: ident.key.clone(),
                        sid: None,
                        done: true,
                        result: Some(Value::Null),
                        result_ref: None,
                        cost_usd: None,
                        label: Some(ident.label.clone()),
                        vendor: ident.vendor.map(|v| v.wire_name().to_string()),
                    });
                    return Dispatched {
                        value: Value::Null,
                        sid: None,
                        cost_usd: 0.0,
                        error: Some(message),
                        text: String::new(),
                    };
                }
            }
        };

        let mut outcome = match first {
            Ok(o) => o,
            Err(err) => {
                self.finish_journal(ident, &sid, Value::Null, cost);
                self.release(&sid, opts).await;
                return Dispatched {
                    value: Value::Null,
                    sid: Some(sid),
                    cost_usd: cost,
                    error: Some(err.to_string()),
                    text: String::new(),
                };
            }
        };
        cost += self.account(&outcome);

        if outcome.failed {
            let why = outcome
                .error_kind
                .clone()
                .unwrap_or_else(|| "worker reported failure".to_string());
            self.finish_journal(ident, &sid, Value::Null, cost);
            self.release(&sid, opts).await;
            return Dispatched {
                value: Value::Null,
                sid: Some(sid),
                cost_usd: cost,
                error: Some(why),
                text: outcome.text,
            };
        }

        // ── schema contract ──────────────────────────────────────────────
        let mut schema_error = None;
        let value = match &opts.schema {
            None => Value::String(outcome.text.clone()),
            Some(schema) => {
                let mut attempt = 0u32;
                loop {
                    let parsed = schema::extract_json(&outcome.text)
                        .ok_or_else(|| "reply contained no JSON value".to_string())
                        .and_then(|v| schema::validate(schema, &v).map(|()| v));
                    match parsed {
                        Ok(v) => break v,
                        Err(reason) => {
                            if attempt >= opts.retry_max {
                                schema_error = Some(format!(
                                    "reply never matched the requested schema ({reason})"
                                ));
                                break Value::Null;
                            }
                            attempt += 1;
                            // The retry stays in the SAME session on purpose:
                            // the worker still has its own answer in context,
                            // so "reply with only the JSON" is a cheap turn.
                            match self.client.follow_up(&sid, &opts.retry_prompt).await {
                                Ok(next) => {
                                    cost += self.account(&next);
                                    if next.failed {
                                        schema_error =
                                            Some("worker failed during schema retry".to_string());
                                        break Value::Null;
                                    }
                                    outcome = next;
                                }
                                Err(err) => {
                                    schema_error = Some(format!("schema retry failed: {err}"));
                                    break Value::Null;
                                }
                            }
                        }
                    }
                }
            }
        };

        self.finish_journal(ident, &sid, value.clone(), cost);
        self.release(&sid, opts).await;
        Dispatched {
            value,
            sid: Some(sid),
            cost_usd: cost,
            error: schema_error,
            text: outcome.text,
        }
    }

    fn account(&self, outcome: &AgentOutcome) -> f64 {
        let cost = outcome.cost_usd.unwrap_or(0.0);
        self.scheduler.add_cost(cost);
        cost
    }

    /// Completion line: the answer, the cost, and `done: true`.
    fn finish_journal(&self, ident: &CallIdent, sid: &str, value: Value, cost: f64) {
        self.journal.append(JournalEntry {
            seq: ident.seq,
            key: ident.key.clone(),
            sid: Some(sid.to_string()),
            done: true,
            result: Some(value),
            result_ref: None,
            cost_usd: Some(cost),
            label: Some(ident.label.clone()),
            vendor: ident.vendor.map(|v| v.wire_name().to_string()),
        });
    }

    /// Stop the session unless the script asked to keep it, or named it
    /// itself.
    async fn release(&self, sid: &str, opts: &AgentOpts) {
        if opts.keep || opts.sid.is_some() {
            self.clear_session(sid);
            return;
        }
        self.stop(sid).await;
    }

    /// Hire, standing down on a harness limit instead of hammering it.
    async fn hire(
        &self,
        task: &str,
        opts: &AgentOpts,
        ident: &CallIdent,
    ) -> Result<String, String> {
        let spec = HireSpec {
            task: task.to_string(),
            vendor: opts.vendor,
            model: opts.model.clone(),
            effort: opts.effort.clone(),
            role: opts.role.clone(),
            label: Some(ident.label.clone()),
            parent_sid: self.parent_sid.clone(),
            permission_mode: opts.permission_mode.clone(),
            // Unique per CALL and stable across a crash-then-resume: run
            // token (minted per run, persisted) + call seq + content key.
            // Hashing the label alone would let two different tasks sharing
            // a label collide at the daemon's replay cache.
            idempotency_key: format!(
                "flow-{}-{}-{}",
                self.journal.run_token(),
                ident.seq,
                &ident.key[..ident.key.len().min(12)]
            ),
        };
        let attempts = self.scheduler.hire_attempts();
        let mut last: Option<ClientError> = None;
        for attempt in 0..attempts {
            match self.client.hire(spec.clone()).await {
                Ok(hired) => {
                    self.scheduler.note_vendor_ok(opts.vendor);
                    return Ok(hired.sid);
                }
                Err(err) => {
                    let backoff = self.scheduler.backoff_for(opts.vendor, &err);
                    last = Some(err);
                    match backoff {
                        // Only a harness limit is worth waiting out: a policy
                        // refusal or a transport failure will say the same
                        // thing in 30 seconds.
                        Some(_) if attempt + 1 < attempts => {
                            self.scheduler.wait_backoff(opts.vendor).await;
                        }
                        _ => break,
                    }
                }
            }
        }
        Err(last
            .map(|e| e.to_string())
            .unwrap_or_else(|| "hire failed".to_string()))
    }
}

#[async_trait::async_trait]
impl Host for Runner {
    async fn agent(&self, task: String, opts_json: String) -> String {
        let opts = match serde_json::from_str::<Value>(&opts_json) {
            Ok(v) => v,
            Err(err) => return env_throw(format!("agent() options are not valid JSON: {err}")),
        };
        self.agent_call(task, opts).await
    }

    async fn usage(&self) -> String {
        match self.client.usage().await {
            Ok(value) => env_ok(value),
            Err(err) => env_throw(format!("usage() failed: {err}")),
        }
    }

    fn phase(&self, title: String) {
        *self.phase.lock().expect("phase mutex poisoned") = Some(title.clone());
        emit(&self.progress, ProgressEvent::PhaseStarted { title });
    }

    fn log(&self, message: String) {
        let phase = self.current_phase();
        emit(&self.progress, ProgressEvent::Log { message, phase });
    }

    fn spent(&self) -> f64 {
        self.scheduler.spent()
    }
}
