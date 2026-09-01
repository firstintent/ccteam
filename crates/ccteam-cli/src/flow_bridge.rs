//! `ccteam flow run` → the project ledger.
//!
//! A flow's hires are already visible: each `agent()` is an ordinary
//! delegation with a real sid and `parent_sid` attribution, so the team view
//! draws them today. The ENVELOPE around them was not visible anywhere — which
//! hires belonged to one run, what that run was called, whether it is still
//! going and how it ended lived only in the run directory of the machine that
//! typed the command. This module is the one-way bridge that puts that
//! envelope on `progress.jsonl`, where every other reader already looks.
//!
//! Three properties are deliberate:
//!
//! 1. **Best-effort, always.** A run must behave identically whether the daemon
//!    is up, down or wedged. Every failure here is a `tracing::warn!` and the
//!    run continues; nothing in this file can change a RunReport.
//! 2. **Off the run's thread.** Submission happens on one worker thread behind
//!    a channel, so an unreachable daemon costs the run a channel send, not an
//!    HTTP timeout. One thread (not one per event) also keeps `started` strictly
//!    before `finished` on the wire.
//! 3. **Envelope only.** Per-agent rows are not re-sent from here. They are
//!    already on the ledger from the daemon side, and a second writer for the
//!    same fact is how two sources of truth get born.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ccteam_core::CcteamPaths;
use ccteam_flow::{ProgressCallback, ProgressEvent};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

/// Per-submission budget. The daemon is on loopback and the handler does one
/// file append, so this is generous; it exists only so a wedged daemon cannot
/// hold the worker thread past the end of the run.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Run both callbacks for every event, in order.
///
/// The stderr renderer stays exactly what it was — the ledger sink is an
/// addition to the run's output, never a replacement for the operator's.
pub fn tee(first: ProgressCallback, second: ProgressCallback) -> ProgressCallback {
    Arc::new(move |event: ProgressEvent| {
        first(event.clone());
        second(event);
    })
}

/// Everything the worker needs to reach this host's daemon, resolved once.
struct Endpoint {
    /// `http://host:port` — no path. The hook path is appended per submission.
    base_url: String,
    /// `ccteam:<hex>` when the daemon minted a web token. Absent is normal: a
    /// loopback-bound daemon does not require auth.
    token: Option<String>,
}

/// One queued envelope row: `POST /internal/hook/flow-run/{action}` + body.
#[derive(Debug, Clone, PartialEq)]
struct Submission {
    action: &'static str,
    body: Value,
}

/// The parts of a run that are the same on every row it emits.
#[derive(Debug, Clone)]
struct RunIdentity {
    run_id: String,
    project: String,
    script_path: String,
    parent_sid: Option<String>,
}

/// The live bridge for one run. Dropping it closes the queue and waits for the
/// worker, so the `finished` row is on disk before the CLI process exits —
/// including on the error paths where the run ends by `?`.
pub struct LedgerBridge {
    tx: Option<Sender<Submission>>,
    worker: Option<JoinHandle<()>>,
    /// Identity of this run, stamped onto every row.
    run_id: String,
    project: String,
    script_path: String,
    parent_sid: Option<String>,
    /// The brake reason, if one tripped. Held so the terminal row can say
    /// *braked* rather than merely *not ok* — a run that hit its ceiling and a
    /// run whose script threw are different outcomes to whoever reads the tab.
    brake: Arc<Mutex<Option<String>>>,
}

impl LedgerBridge {
    /// Resolve the daemon and start the worker.
    ///
    /// `run_dir`'s basename is the `run_id`: it is stable for the life of the
    /// run, survives `--resume` (which continues the same directory), and is
    /// therefore the join key between the ledger row and the on-disk journal an
    /// evaluation reads.
    pub fn start(
        paths: &CcteamPaths,
        project: &str,
        run_dir: &Path,
        script: &Path,
        parent_sid: Option<String>,
    ) -> Self {
        let endpoint = resolve_endpoint(paths);
        let (tx, rx) = mpsc::channel::<Submission>();
        let worker = std::thread::Builder::new()
            .name("ccteam-flow-ledger".into())
            .spawn(move || {
                // No daemon to talk to: drain the queue so the run's sends stay
                // non-blocking, and stay quiet — `start` already said why once.
                let Some(endpoint) = endpoint else {
                    for _ in rx {}
                    return;
                };
                submit_loop(&endpoint, rx);
            })
            .inspect_err(|err| {
                tracing::warn!(error = %err, "flow ledger bridge: worker thread not started");
            })
            .ok();

        Self {
            // A failed spawn leaves no receiver; drop the sender with it so
            // every later send is a cheap, silent error instead of a leak.
            tx: worker.is_some().then_some(tx),
            worker,
            run_id: run_id_for(run_dir),
            project: project.to_string(),
            script_path: absolute_script(script),
            parent_sid,
            brake: Arc::new(Mutex::new(None)),
        }
    }

    /// The [`ProgressCallback`] to hand the runner. Only the three run-level
    /// events are translated; phases, logs and per-agent events are the run's
    /// own narration and stay on stderr.
    pub fn sink(&self) -> ProgressCallback {
        let tx = self.tx.clone();
        let identity = RunIdentity {
            run_id: self.run_id.clone(),
            project: self.project.clone(),
            script_path: self.script_path.clone(),
            parent_sid: self.parent_sid.clone(),
        };
        let brake = Arc::clone(&self.brake);

        Arc::new(move |event: ProgressEvent| {
            let Some(tx) = tx.as_ref() else { return };
            let Some(submission) = submission_for(&identity, &brake, event) else {
                return;
            };
            // A closed channel means the worker is gone; the run does not care.
            let _ = tx.send(submission);
        })
    }
}

/// Translate one in-process runner event into an envelope submission, or
/// `None` when the event is not part of the envelope.
///
/// Pure but for the clock and the remembered brake, so the translation — the
/// part that decides what the ledger will say about a run — is testable without
/// a daemon, a socket or a thread.
fn submission_for(
    identity: &RunIdentity,
    brake: &Mutex<Option<String>>,
    event: ProgressEvent,
) -> Option<Submission> {
    Some(match event {
        ProgressEvent::RunStarted {
            name, description, ..
        } => Submission {
            action: ccteam_hooks::flow_run::ACTION_STARTED,
            body: json!({
                "project": identity.project,
                "run_id": identity.run_id,
                "parent_sid": identity.parent_sid,
                "name": name,
                "description": description,
                "script_path": identity.script_path,
                "started_at": now_rfc3339(),
            }),
        },
        ProgressEvent::BrakeTripped { reason } => {
            // Remembered so the terminal row can say *braked* rather than
            // merely *not ok* — the runner reports those as the same `ok`.
            if let Ok(mut held) = brake.lock() {
                *held = Some(reason.clone());
            }
            Submission {
                action: ccteam_hooks::flow_run::ACTION_BRAKE,
                body: json!({
                    "project": identity.project,
                    "run_id": identity.run_id,
                    "reason": reason,
                }),
            }
        }
        ProgressEvent::RunFinished {
            agents,
            cost_usd,
            ok,
        } => Submission {
            action: ccteam_hooks::flow_run::ACTION_FINISHED,
            body: json!({
                "project": identity.project,
                "run_id": identity.run_id,
                "agents": agents,
                "cost_usd": cost_usd,
                "ok": ok,
                "brake": brake.lock().ok().and_then(|held| held.clone()),
                "finished_at": now_rfc3339(),
            }),
        },
        // Not envelope facts — the ledger already carries the leaves.
        ProgressEvent::PhaseStarted { .. }
        | ProgressEvent::AgentStarted { .. }
        | ProgressEvent::AgentFinished { .. }
        | ProgressEvent::Log { .. } => return None,
    })
}

impl Drop for LedgerBridge {
    fn drop(&mut self) {
        // Close the queue first: the worker ends when the channel does.
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            // Bounded by SUBMIT_TIMEOUT per queued row (at most three).
            let _ = worker.join();
        }
    }
}

/// Where this host's daemon is, and how to prove we may talk to it.
///
/// The URL comes from the same resolver the flow's MCP client uses, so "which
/// daemon" has exactly one answer per process (env override, else the address
/// the running daemon recorded, else the compiled-in loopback default). The
/// token is the admin web token the daemon writes when it binds with auth; a
/// loopback daemon runs without auth and the absent file is not an error.
fn resolve_endpoint(paths: &CcteamPaths) -> Option<Endpoint> {
    let mcp_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run"));
    let base_url = mcp_url.strip_suffix("/mcp").unwrap_or(&mcp_url).to_string();
    if base_url.is_empty() {
        tracing::warn!("flow ledger bridge: no daemon address; run stays out of the ledger");
        return None;
    }
    let token = std::fs::read_to_string(paths.web_token_path())
        .ok()
        .map(|body| body.trim().to_string())
        .filter(|token| !token.is_empty())
        .map(|hex| format!("ccteam:{hex}"));
    Some(Endpoint { base_url, token })
}

/// Drain the queue, one POST at a time, on the worker thread.
fn submit_loop(endpoint: &Endpoint, rx: mpsc::Receiver<Submission>) {
    // A current-thread runtime local to this worker: the CLI's own runtime is
    // busy driving the run and must never wait on observability.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(error = %err, "flow ledger bridge: no runtime; run stays out of the ledger");
            for _ in rx {}
            return;
        }
    };
    let client = match reqwest::Client::builder()
        .timeout(SUBMIT_TIMEOUT)
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "flow ledger bridge: no http client; run stays out of the ledger");
            for _ in rx {}
            return;
        }
    };

    for submission in rx.iter() {
        let url = format!(
            "{}/internal/hook/flow-run/{}",
            endpoint.base_url, submission.action
        );
        let mut request = client.post(&url).json(&submission.body);
        if let Some(token) = &endpoint.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        match runtime.block_on(request.send()) {
            Ok(response) if response.status().is_success() => {}
            // The daemon answered but said no — it is alive, keep trying the
            // remaining rows (a later row may be acceptable where this one
            // was not).
            Ok(response) => {
                tracing::warn!(
                    action = submission.action,
                    status = %response.status(),
                    "flow ledger bridge: daemon refused a run envelope row"
                );
            }
            // Transport failure: the daemon is down or wedged, and every
            // remaining row would eat its own SUBMIT_TIMEOUT — serially, in
            // `Drop`, on the way OUT of the CLI. One timeout is the price of
            // finding out; the rest of the queue is drained without HTTP so
            // process exit is bounded by ONE timeout, not one per row
            // (checker s523 R1).
            Err(err) => {
                tracing::warn!(
                    action = submission.action,
                    error = %err,
                    "flow ledger bridge: daemon unreachable; remaining envelope rows dropped"
                );
                for _ in rx.iter() {}
                return;
            }
        }
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The run directory's basename. `--run-dir /` and other pathological inputs
/// fall back to a constant rather than an empty id: an unnamed run is still a
/// run, and the reader deduplicates on `run_id` + `started_at`.
fn run_id_for(run_dir: &Path) -> String {
    run_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "run".to_string())
}

/// Record the script by absolute path when one can be resolved: a relative
/// path is meaningless to a reader who was not standing in the same directory.
fn absolute_script(script: &Path) -> String {
    std::fs::canonicalize(script)
        .unwrap_or_else(|_| PathBuf::from(script))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_is_the_run_directory_basename() {
        assert_eq!(
            run_id_for(Path::new("/home/u/.ccteam/runs/audit-20260901-101500")),
            "audit-20260901-101500"
        );
        assert_eq!(run_id_for(Path::new("/")), "run");
    }

    fn identity() -> RunIdentity {
        RunIdentity {
            run_id: "audit-20260901-101500".into(),
            project: "alpha".into(),
            script_path: "/w/flow.js".into(),
            parent_sid: Some("s42".into()),
        }
    }

    #[test]
    fn the_envelope_is_three_events_and_nothing_else() {
        let brake = Mutex::new(None);
        let id = identity();

        // The leaves are already on the ledger from the daemon side; sending
        // them again from here would make a second writer for the same fact.
        for narration in [
            ProgressEvent::PhaseStarted {
                title: "audit".into(),
            },
            ProgressEvent::Log {
                message: "hello".into(),
                phase: None,
            },
            ProgressEvent::AgentStarted {
                seq: 1,
                label: "f.rs".into(),
                vendor: None,
                cached: false,
            },
            ProgressEvent::AgentFinished {
                seq: 1,
                label: "f.rs".into(),
                outcome: None,
                cost_usd: 0.1,
            },
        ] {
            assert!(
                submission_for(&id, &brake, narration.clone()).is_none(),
                "{narration:?} must not reach the ledger"
            );
        }

        let started = submission_for(
            &id,
            &brake,
            ProgressEvent::RunStarted {
                name: "audit-routes".into(),
                description: "Audit route handlers".into(),
                phases: vec![],
            },
        )
        .expect("run start is an envelope fact");
        assert_eq!(started.action, "started");
        assert_eq!(started.body["run_id"], "audit-20260901-101500");
        assert_eq!(started.body["project"], "alpha");
        assert_eq!(started.body["parent_sid"], "s42");
        assert_eq!(started.body["name"], "audit-routes");
        assert_eq!(started.body["script_path"], "/w/flow.js");
        assert!(started.body["started_at"]
            .as_str()
            .is_some_and(|ts| ts.ends_with('Z')));
    }

    #[test]
    fn a_brake_is_remembered_onto_the_terminal_row() {
        let brake = Mutex::new(None);
        let id = identity();

        let tripped = submission_for(
            &id,
            &brake,
            ProgressEvent::BrakeTripped {
                reason: "max_agents".into(),
            },
        )
        .expect("a brake is an envelope fact");
        assert_eq!(tripped.action, "brake");
        assert_eq!(tripped.body["reason"], "max_agents");

        // The runner reports a braked run and a thrown script identically
        // (`ok:false`); carrying the reason across is what keeps them apart.
        let finished = submission_for(
            &id,
            &brake,
            ProgressEvent::RunFinished {
                agents: 5,
                cost_usd: 1.25,
                ok: false,
            },
        )
        .expect("run end is an envelope fact");
        assert_eq!(finished.action, "finished");
        assert_eq!(finished.body["brake"], "max_agents");
        assert_eq!(finished.body["agents"], 5);
        assert_eq!(finished.body["cost_usd"], 1.25);
        assert_eq!(finished.body["ok"], false);
    }

    #[test]
    fn a_clean_run_names_no_brake() {
        let brake = Mutex::new(None);
        let finished = submission_for(
            &identity(),
            &brake,
            ProgressEvent::RunFinished {
                agents: 2,
                cost_usd: 0.5,
                ok: true,
            },
        )
        .expect("run end is an envelope fact");
        assert_eq!(finished.body["ok"], true);
        assert!(finished.body["brake"].is_null());
    }

    #[test]
    fn tee_delivers_every_event_to_both_sinks() {
        let left = Arc::new(Mutex::new(Vec::new()));
        let right = Arc::new(Mutex::new(Vec::new()));
        let seen_left = Arc::clone(&left);
        let seen_right = Arc::clone(&right);

        let combined = tee(
            Arc::new(move |event| seen_left.lock().unwrap().push(format!("{event:?}"))),
            Arc::new(move |event| seen_right.lock().unwrap().push(format!("{event:?}"))),
        );
        combined(ProgressEvent::BrakeTripped {
            reason: "max_agents".into(),
        });
        combined(ProgressEvent::RunFinished {
            agents: 1,
            cost_usd: 0.5,
            ok: false,
        });

        assert_eq!(left.lock().unwrap().len(), 2);
        assert_eq!(*left.lock().unwrap(), *right.lock().unwrap());
    }
}
