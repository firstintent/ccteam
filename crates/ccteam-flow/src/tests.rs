//! End-to-end runner tests.
//!
//! Every one of these drives the real engine, scheduler and journal against
//! [`FakeClient`]. Nothing spawns a process, opens a socket or reads `$HOME`,
//! and every wait is virtual (`start_paused`), so a workflow whose scripted
//! delays add up to ten minutes asserts in microseconds.

use crate::client::ClientError;
use crate::fake::{FakeCall, FakeClient, FakeReply};
use crate::progress::{ProgressCallback, ProgressEvent};
use crate::run::{run_workflow, RunConfig, RunReport, ScriptSource};
use crate::scheduler::{Brakes, SchedulerConfig, VendorPools};
use ccteam_harness::AgentVendor;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const META: &str = "export const meta = { name: 't', description: 'a test workflow' }\n";

/// A scheduler that does not throttle: used wherever the assertion is about
/// script semantics rather than about the ramp.
fn unthrottled() -> SchedulerConfig {
    SchedulerConfig {
        spawn_rate_per_sec: 100_000.0,
        ..SchedulerConfig::default()
    }
}

struct Harness {
    dir: tempfile::TempDir,
    client: Arc<FakeClient>,
    events: Arc<Mutex<Vec<ProgressEvent>>>,
}

impl Harness {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
            client: Arc::new(FakeClient::new()),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn config(&self) -> RunConfig {
        let events = Arc::clone(&self.events);
        let sink: ProgressCallback = Arc::new(move |event| {
            events.lock().expect("events mutex poisoned").push(event);
        });
        RunConfig {
            scheduler: unthrottled(),
            brakes: Brakes {
                max_agents: 500,
                ..Brakes::default()
            },
            ..RunConfig::new(self.dir.path(), Arc::clone(&self.client) as _)
        }
        .with_progress(sink)
    }

    async fn run(&self, body: &str) -> RunReport {
        self.run_with(body, self.config()).await
    }

    async fn run_with(&self, body: &str, cfg: RunConfig) -> RunReport {
        run_workflow(ScriptSource::inline(format!("{META}{body}")), cfg)
            .await
            .expect("run completes")
    }

    fn events(&self) -> Vec<ProgressEvent> {
        self.events.lock().expect("events mutex poisoned").clone()
    }

    fn hired_tasks(&self) -> Vec<String> {
        self.client
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                FakeCall::Hire { task, .. } => Some(task),
                _ => None,
            })
            .collect()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// agent()
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn agent_returns_the_workers_final_text() {
    let h = Harness::new();
    h.client.push(FakeReply::text("42").with_cost(0.25));
    let report = h.run("return await agent('what is the answer')").await;

    assert_eq!(report.returned, json!("42"));
    assert_eq!(report.totals.agents, 1);
    assert!((report.totals.cost_usd - 0.25).abs() < f64::EPSILON);
    assert_eq!(h.hired_tasks(), vec!["what is the answer".to_string()]);
    assert!(report.ok(), "{report:?}");
}

#[tokio::test(start_paused = true)]
async fn hire_options_reach_the_client() {
    let h = Harness::new();
    let report = h
        .run("return await agent('go', {vendor: 'codex', model: 'gpt-x', role: 'checker'})")
        .await;
    assert!(report.ok(), "{report:?}");
    let hire = h
        .client
        .calls()
        .into_iter()
        .find(|c| matches!(c, FakeCall::Hire { .. }))
        .expect("a hire happened");
    let FakeCall::Hire {
        task,
        vendor,
        model,
        role,
        idempotency_key,
    } = hire
    else {
        unreachable!("filtered to a hire above");
    };
    assert_eq!(task, "go");
    assert_eq!(vendor, Some(AgentVendor::Codex));
    assert_eq!(model, Some("gpt-x".to_string()));
    assert_eq!(role, Some("checker".to_string()));
    // The key itself is run-scoped and dynamic; its shape is the contract.
    assert!(idempotency_key.starts_with("flow-"), "{idempotency_key}");
}

#[tokio::test(start_paused = true)]
async fn an_unknown_option_is_a_hard_error_at_call_time() {
    let h = Harness::new();
    let report = h.run("return await agent('go', {modle: 'opus'})").await;
    let err = report.script_error.expect("the script must throw");
    assert!(err.contains("modle"), "the typo must be named: {err}");
    assert!(err.contains("vendor"), "the menu must be shown: {err}");
    assert_eq!(h.client.hires(), 0, "nothing may be hired on a misuse");
}

#[tokio::test(start_paused = true)]
async fn an_unknown_harness_is_a_hard_error() {
    let h = Harness::new();
    let report = h.run("return await agent('go', {vendor: 'gpt5'})").await;
    let err = report.script_error.expect("the script must throw");
    assert!(err.contains("gpt5") && err.contains("claude"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn an_empty_task_is_a_hard_error() {
    let h = Harness::new();
    let report = h.run("return await agent('   ')").await;
    assert!(report
        .script_error
        .expect("throws")
        .contains("non-empty string"));
}

#[tokio::test(start_paused = true)]
async fn a_worker_failure_resolves_to_null_rather_than_throwing() {
    let h = Harness::new();
    h.client.push(FakeReply::failing("worker_error"));
    let report = h
        .run("const r = await agent('go'); return {isNull: r === null}")
        .await;
    assert_eq!(report.returned, json!({"isNull": true}));
    assert!(report.script_error.is_none(), "agent() must not throw");
    assert_eq!(report.agents[0].error.as_deref(), Some("worker_error"));
}

#[tokio::test(start_paused = true)]
async fn a_refused_hire_resolves_to_null() {
    let h = Harness::new();
    h.client.push(FakeReply::hire_error(ClientError::Refused(
        "delegation depth exceeded".to_string(),
    )));
    let report = h
        .run("const r = await agent('go'); return {isNull: r === null}")
        .await;
    assert_eq!(report.returned, json!({"isNull": true}));
    assert!(report.script_error.is_none());
    assert_eq!(h.client.hires(), 1, "a refusal is not retried");
    assert!(report.agents[0]
        .error
        .as_deref()
        .expect("recorded")
        .contains("depth"));
}

#[tokio::test(start_paused = true)]
async fn a_script_named_sid_follows_up_instead_of_hiring() {
    let h = Harness::new();
    h.client.push_follow_up(FakeReply::text("continued"));
    let report = h.run("return await agent('carry on', {sid: 's99'})").await;
    assert_eq!(report.returned, json!("continued"));
    assert_eq!(h.client.hires(), 0);
    assert!(h.client.calls().contains(&FakeCall::FollowUp {
        sid: "s99".to_string(),
        task: "carry on".to_string()
    }));
    assert!(
        h.client.stopped().is_empty(),
        "a session the script named is not the runner's to stop"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// parallel() / pipeline()
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn parallel_is_a_barrier_and_a_failure_becomes_a_null_slot() {
    let h = Harness::new();
    h.client.on_task("a", FakeReply::text("A"));
    h.client
        .on_task("b", FakeReply::failing("worker_error"))
        .on_task("c", FakeReply::text("C"));
    let report = h
        .run("return await parallel([() => agent('a'), () => agent('b'), () => agent('c')])")
        .await;
    assert_eq!(report.returned, json!(["A", null, "C"]));
    assert!(report.script_error.is_none(), "parallel() never rejects");
}

#[tokio::test(start_paused = true)]
async fn a_thunk_that_throws_becomes_a_null_slot() {
    let h = Harness::new();
    let report = h
        .run("return await parallel([() => agent('a'), () => { throw new Error('boom') }])")
        .await;
    assert_eq!(report.returned[1], Value::Null);
    assert!(report.script_error.is_none());
}

#[tokio::test(start_paused = true)]
async fn pipeline_streams_items_with_no_barrier_between_stages() {
    let h = Harness::new();
    // Item 2's first stage is slow. With a barrier, no stage-2 call could
    // happen before it finished; with streaming, item 1 runs its whole chain
    // first. The hire ORDER is the proof.
    h.client.on_task(
        "s1-2",
        FakeReply::text("slow").with_delay(Duration::from_secs(30)),
    );
    let report = h
        .run(
            "return await pipeline([1, 2],
               (prev, item) => agent('s1-' + item),
               (prev, item) => agent('s2-' + item),
               (prev, item) => agent('s3-' + item))",
        )
        .await;
    assert!(report.ok(), "{report:?}");
    assert_eq!(
        h.hired_tasks(),
        vec!["s1-1", "s1-2", "s2-1", "s3-1", "s2-2", "s3-2"],
        "item 1 must reach stage 3 while item 2 is still in stage 1"
    );
}

#[tokio::test(start_paused = true)]
async fn pipeline_stages_receive_prev_item_and_index() {
    let h = Harness::new();
    let report = h
        .run(
            "return await pipeline(['x', 'y'],
               (prev, item, idx) => 'first:' + prev + ':' + item + ':' + idx,
               (prev, item, idx) => prev + '|second:' + item + ':' + idx)",
        )
        .await;
    assert_eq!(
        report.returned,
        json!(["first:x:x:0|second:x:0", "first:y:y:1|second:y:1"])
    );
}

#[tokio::test(start_paused = true)]
async fn a_throwing_stage_drops_its_item_and_skips_the_rest_of_its_chain() {
    let h = Harness::new();
    let report = h
        .run(
            "return await pipeline([1, 2],
               (prev, item) => { if (item === 1) throw new Error('nope'); return item },
               (prev, item) => agent('stage2-' + item))",
        )
        .await;
    assert_eq!(report.returned[0], Value::Null, "item 1 dropped");
    assert_eq!(report.returned[1], json!("ok"), "item 2 unaffected");
    assert_eq!(
        h.hired_tasks(),
        vec!["stage2-2"],
        "the dropped item must skip its remaining stages"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// scheduling
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_vendor_pool_bounds_observed_concurrency() {
    let h = Harness::new();
    h.client
        .with_default_reply(FakeReply::text("done").with_delay(Duration::from_secs(5)));
    let cfg = RunConfig {
        scheduler: SchedulerConfig {
            max_parallel: 32,
            pools: VendorPools::default().with(AgentVendor::Claude, 8),
            spawn_rate_per_sec: 100_000.0,
            ..SchedulerConfig::default()
        },
        brakes: Brakes {
            max_agents: 200,
            ..Brakes::default()
        },
        ..h.config()
    };
    let report = h
        .run_with(
            "const thunks = []
             for (let i = 0; i < 100; i++) thunks.push(() => agent('task ' + i, {vendor: 'claude'}))
             const out = await parallel(thunks)
             return out.filter(Boolean).length",
            cfg,
        )
        .await;
    assert_eq!(report.returned, json!(100), "every agent must complete");
    assert_eq!(
        h.client.peak_concurrency(),
        8,
        "the claude pool caps concurrency at its slot count"
    );
    assert_eq!(report.totals.agents, 100);
}

#[tokio::test(start_paused = true)]
async fn a_harness_limit_delays_the_next_hire_by_the_pool_backoff() {
    let h = Harness::new();
    // First hire is limited, the retry (after the backoff) succeeds.
    h.client
        .push(FakeReply::hire_error(ClientError::VendorLimit(
            "429 rate limited".to_string(),
        )))
        .push(FakeReply::text("first"))
        .push(FakeReply::text("second"));

    let start = tokio::time::Instant::now();
    let report = h
        .run("const a = await agent('one'); const b = await agent('two'); return [a, b]")
        .await;
    let elapsed = tokio::time::Instant::now() - start;

    assert_eq!(report.returned, json!(["first", "second"]));
    assert!(
        elapsed >= Duration::from_secs(30),
        "a limited pool must stand down for the initial backoff, took {elapsed:?}"
    );
    assert_eq!(h.client.hires(), 3, "one limited hire plus two good ones");
}

#[tokio::test(start_paused = true)]
async fn a_limit_that_never_clears_resolves_to_null_after_the_attempt_budget() {
    let h = Harness::new();
    h.client
        .with_default_reply(FakeReply::hire_error(ClientError::VendorLimit(
            "still limited".to_string(),
        )));
    let cfg = RunConfig {
        scheduler: SchedulerConfig {
            hire_attempts: 2,
            ..unthrottled()
        },
        ..h.config()
    };
    let report = h
        .run_with(
            "const r = await agent('go'); return {isNull: r === null}",
            cfg,
        )
        .await;
    assert_eq!(report.returned, json!({"isNull": true}));
    assert_eq!(h.client.hires(), 2, "attempts are bounded");
}

// ───────────────────────────────────────────────────────────────────────────
// brakes and budget
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn the_agent_brake_refuses_new_work_while_in_flight_agents_finish() {
    let h = Harness::new();
    h.client
        .with_default_reply(FakeReply::text("ok").with_delay(Duration::from_secs(2)));
    let cfg = RunConfig {
        brakes: Brakes {
            max_agents: 3,
            ..Brakes::default()
        },
        ..h.config()
    };
    let report = h
        .run_with(
            "const thunks = []
             for (let i = 0; i < 5; i++) thunks.push(() => agent('task ' + i))
             return await parallel(thunks)",
            cfg,
        )
        .await;

    let slots = report.returned.as_array().expect("array");
    assert_eq!(slots.len(), 5);
    assert_eq!(
        slots.iter().filter(|v| !v.is_null()).count(),
        3,
        "exactly max_agents may start: {slots:?}"
    );
    let brake = report.brake.expect("the brake must be reported");
    assert!(brake.contains("max_agents=3"), "{brake}");
    assert!(
        h.events()
            .iter()
            .any(|e| matches!(e, ProgressEvent::BrakeTripped { .. })),
        "the brake must be announced once"
    );
    assert_eq!(
        h.client.hires(),
        3,
        "a tripped brake never cancels a running worker, it just stops new ones"
    );
}

#[tokio::test(start_paused = true)]
async fn a_brake_throws_into_a_sequential_script_and_names_itself() {
    let h = Harness::new();
    h.client.with_default_reply(FakeReply::text("ok"));
    let cfg = RunConfig {
        brakes: Brakes {
            max_agents: 2,
            ..Brakes::default()
        },
        ..h.config()
    };
    // Worker failures are null; a BRAKE is a run-level stop condition and
    // must surface as a throw a sequential script can see and name.
    let report = h
        .run_with(
            "await agent('one')
             await agent('two')
             let msg = null
             try { await agent('three') } catch (e) { msg = String(e) }
             return { msg }",
            cfg,
        )
        .await;
    let msg = report.returned["msg"].as_str().expect("caught message");
    assert!(msg.contains("max_agents=2"), "{msg}");
    assert!(report.brake.is_some(), "the brake must be reported");
}

#[tokio::test(start_paused = true)]
async fn idempotency_keys_are_unique_per_call_never_label_keyed() {
    let h = Harness::new();
    h.client.with_default_reply(FakeReply::text("ok"));
    h.run(
        "await agent('first task', { label: 'x' })
         await agent('second task', { label: 'x' })
         return 0",
    )
    .await;
    let keys: Vec<String> = h
        .client
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            crate::fake::FakeCall::Hire {
                idempotency_key, ..
            } => Some(idempotency_key),
            _ => None,
        })
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1], "same label must not collide: {keys:?}");
    assert!(keys[0].starts_with("flow-"), "{keys:?}");
}

#[tokio::test]
async fn the_run_token_persists_on_resume_and_remints_fresh() {
    let dir = tempfile::tempdir().expect("tempdir");
    let open = |resume| {
        crate::journal::Journal::open(dir.path(), resume)
            .expect("open")
            .run_token()
            .to_string()
    };
    let first = open(false);
    assert_eq!(first, open(true), "resume must keep the run identity");
    assert_ne!(first, open(false), "a fresh run into a reused dir is NEW");
}

#[tokio::test(start_paused = true)]
async fn the_budget_target_trips_and_is_visible_to_the_script() {
    let h = Harness::new();
    h.client
        .with_default_reply(FakeReply::text("ok").with_cost(0.6));
    let cfg = h.config().with_budget(1.0);
    let report = h
        .run_with(
            "const seen = [budget.total, budget.spent(), budget.remaining()]
             await agent('one')
             await agent('two')
             seen.push(budget.spent())
             let braked = false
             try { await agent('three') } catch (e) { braked = String(e.message || e) }
             return {seen, braked}",
            cfg,
        )
        .await;

    let seen = &report.returned["seen"];
    assert_eq!(seen[0].as_f64(), Some(1.0), "budget.total is injected");
    assert_eq!(seen[1].as_f64(), Some(0.0));
    assert_eq!(seen[2].as_f64(), Some(1.0));
    assert!(
        (seen[3].as_f64().expect("a number") - 1.2).abs() < 1e-9,
        "budget.spent tracks reported costs, got {}",
        seen[3]
    );
    let braked = report.returned["braked"].as_str().expect("a brake message");
    assert!(braked.contains("budget target"), "{braked}");
    assert!(report.brake.expect("reported").contains("budget target"));
}

#[tokio::test(start_paused = true)]
async fn budget_remaining_is_infinite_without_a_target() {
    let h = Harness::new();
    let report = h
        .run("return {total: budget.total, inf: budget.remaining() === Infinity}")
        .await;
    assert_eq!(report.returned["total"], Value::Null);
    assert_eq!(report.returned["inf"], json!(true));
}

// ───────────────────────────────────────────────────────────────────────────
// session cleanup
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn non_kept_sessions_are_stopped_and_kept_ones_are_not() {
    let h = Harness::new();
    let report = h
        .run("await agent('transient'); await agent('durable', {keep: true}); return 'done'")
        .await;
    assert!(report.ok(), "{report:?}");
    assert_eq!(
        h.client.stopped(),
        vec!["f1".to_string()],
        "only the non-kept session is released"
    );
}

#[tokio::test(start_paused = true)]
async fn every_non_kept_session_is_released_even_when_a_brake_ends_the_run() {
    let h = Harness::new();
    h.client
        .with_default_reply(FakeReply::text("ok").with_delay(Duration::from_secs(3)));
    let cfg = RunConfig {
        brakes: Brakes {
            max_agents: 2,
            ..Brakes::default()
        },
        ..h.config()
    };
    let report = h
        .run_with(
            "const thunks = []
             for (let i = 0; i < 4; i++) thunks.push(() => agent('t' + i))
             return await parallel(thunks)",
            cfg,
        )
        .await;
    assert!(report.brake.is_some());
    let mut stopped = h.client.stopped();
    stopped.sort();
    assert_eq!(
        stopped,
        vec!["f1".to_string(), "f2".to_string()],
        "both hired sessions are released"
    );
}

#[tokio::test(start_paused = true)]
async fn a_thrown_script_still_releases_its_sessions() {
    let h = Harness::new();
    let report = h
        .run("await agent('one'); throw new Error('author bug')")
        .await;
    assert!(report
        .script_error
        .expect("the throw is reported")
        .contains("author bug"));
    assert_eq!(h.client.stopped(), vec!["f1".to_string()]);
    assert_eq!(
        report.totals.agents, 1,
        "records survive a script that threw"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// journal and resume
// ───────────────────────────────────────────────────────────────────────────

const RESUMABLE: &str = "const a = await agent('alpha')
     const b = await agent('beta')
     const c = await agent('gamma')
     return [a, b, c]";

#[tokio::test(start_paused = true)]
async fn a_resumed_run_replays_from_the_journal_without_touching_the_client() {
    let h = Harness::new();
    h.client
        .on_task("alpha", FakeReply::text("A").with_cost(0.1));
    h.client
        .on_task("beta", FakeReply::text("B").with_cost(0.2));
    h.client
        .on_task("gamma", FakeReply::text("C").with_cost(0.3));

    let first = h.run(RESUMABLE).await;
    assert_eq!(first.returned, json!(["A", "B", "C"]));
    let calls_after_first = h.client.calls().len();
    assert!(calls_after_first > 0);

    let second = h.run_with(RESUMABLE, h.config().resuming()).await;
    assert_eq!(
        second.returned, first.returned,
        "same script and args must replay identically"
    );
    assert_eq!(
        h.client.calls().len(),
        calls_after_first,
        "a full cache hit must not touch the client at all"
    );
    assert_eq!(second.cache.hits, 3);
    assert_eq!(second.cache.invalidated_at, None);
    assert!(second.agents.iter().all(|a| a.cached));
    assert!(
        (second.totals.cost_usd - 0.0).abs() < f64::EPSILON,
        "a replayed run spends nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn editing_one_call_keeps_the_prefix_and_goes_live_from_there() {
    let h = Harness::new();
    h.client.on_task("alpha", FakeReply::text("A"));
    h.client.on_task("beta", FakeReply::text("B"));
    h.client.on_task("gamma", FakeReply::text("C"));
    h.client.on_task("BETA", FakeReply::text("B2"));

    h.run(RESUMABLE).await;
    let baseline = h.hired_tasks().len();

    let edited = RESUMABLE.replace("'beta'", "'BETA'");
    let report = h.run_with(&edited, h.config().resuming()).await;

    assert_eq!(report.returned, json!(["A", "B2", "C"]));
    assert_eq!(report.cache.hits, 1, "only the unchanged prefix is reused");
    assert_eq!(report.cache.invalidated_at, Some(1));
    let diagnostic = report
        .cache
        .diagnostic
        .expect("the first mismatch must be explained");
    assert!(diagnostic.contains("call #1"), "{diagnostic}");
    assert!(diagnostic.contains("running live"), "{diagnostic}");

    let live = &h.hired_tasks()[baseline..];
    assert_eq!(
        live,
        ["BETA", "gamma"],
        "the edited call and everything after it must run live"
    );
}

#[tokio::test(start_paused = true)]
async fn an_interrupted_call_reattaches_instead_of_hiring_twice() {
    let h = Harness::new();
    // Hand-write a journal whose second call was dispatched but never
    // finished — exactly what a kill mid-turn leaves behind.
    let key0 = crate::journal::call_key("alpha", &Value::Null);
    let key1 = crate::journal::call_key("beta", &Value::Null);
    std::fs::write(
        h.dir.path().join("journal.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"seq": 0, "key": key0, "sid": "s1", "done": true, "result": "A"}),
            json!({"seq": 1, "key": key1, "sid": "s2", "done": false})
        ),
    )
    .expect("seed journal");
    h.client.with_default_reply(FakeReply::text("re-attached"));

    let report = h
        .run_with(
            "const a = await agent('alpha'); const b = await agent('beta'); return [a, b]",
            h.config().resuming(),
        )
        .await;

    assert_eq!(report.returned, json!(["A", "re-attached"]));
    assert_eq!(h.client.hires(), 0, "the live session must not be re-hired");
    assert!(
        h.client.calls().contains(&FakeCall::Await {
            sid: "s2".to_string()
        }),
        "the runner re-attaches to the dispatched sid: {:?}",
        h.client.calls()
    );
    assert_eq!(report.cache.reattached, 1);
}

#[tokio::test(start_paused = true)]
async fn the_run_directory_describes_itself() {
    let h = Harness::new();
    h.run_with("return 1", h.config().with_args(json!({"target": "src"})))
        .await;
    let script = std::fs::read_to_string(h.dir.path().join("script.js")).expect("script.js");
    assert!(script.starts_with("export const meta"));
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(h.dir.path().join("run.json")).expect("run.json"),
    )
    .expect("json");
    assert_eq!(manifest["meta"]["name"], json!("t"));
    assert_eq!(manifest["args"], json!({"target": "src"}));
}

#[tokio::test(start_paused = true)]
async fn the_journal_records_a_dispatch_line_before_the_completion_line() {
    let h = Harness::new();
    h.run("return await agent('alpha')").await;
    let raw = std::fs::read_to_string(h.dir.path().join("journal.jsonl")).expect("journal");
    let lines: Vec<Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    assert_eq!(lines.len(), 2, "dispatch then completion");
    assert_eq!(lines[0]["done"], json!(false));
    assert_eq!(lines[0]["sid"], json!("f1"));
    assert_eq!(lines[1]["done"], json!(true));
    assert_eq!(lines[1]["result"], json!("ok"));
}

// ───────────────────────────────────────────────────────────────────────────
// schema
// ───────────────────────────────────────────────────────────────────────────

const BUGS_SCHEMA: &str =
    "{type: 'object', required: ['bugs'], properties: {bugs: {type: 'array'}}}";

#[tokio::test(start_paused = true)]
async fn a_schema_reply_is_extracted_from_a_fenced_block() {
    let h = Harness::new();
    h.client.push(FakeReply::text(
        "Sure!\n```json\n{\"bugs\": [\"one\"]}\n```\nHope that helps.",
    ));
    let report = h
        .run(&format!(
            "return await agent('find bugs', {{schema: {BUGS_SCHEMA}}})"
        ))
        .await;
    assert_eq!(report.returned, json!({"bugs": ["one"]}));
    assert_eq!(
        h.client
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::FollowUp { .. }))
            .count(),
        0,
        "a good reply needs no retry"
    );
}

#[tokio::test(start_paused = true)]
async fn a_mismatched_reply_is_retried_in_the_same_session_and_then_succeeds() {
    let h = Harness::new();
    h.client
        .push(FakeReply::text("I think there are three bugs."));
    h.client
        .push_follow_up(FakeReply::text("{\"bugs\": [\"a\", \"b\"]}"));
    let report = h
        .run(&format!(
            "return await agent('find bugs', {{schema: {BUGS_SCHEMA}}})"
        ))
        .await;

    assert_eq!(report.returned, json!({"bugs": ["a", "b"]}));
    let follow_ups: Vec<FakeCall> = h
        .client
        .calls()
        .into_iter()
        .filter(|c| matches!(c, FakeCall::FollowUp { .. }))
        .collect();
    assert_eq!(follow_ups.len(), 1, "exactly one retry");
    match &follow_ups[0] {
        FakeCall::FollowUp { sid, task } => {
            assert_eq!(sid, "f1", "the retry stays in the same session");
            assert_eq!(task, crate::SCHEMA_RETRY_PROMPT);
        }
        other => panic!("unexpected call {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn exhausted_schema_retries_resolve_to_null() {
    let h = Harness::new();
    h.client.with_default_reply(FakeReply::text("no json here"));
    let report = h
        .run(&format!(
            "const r = await agent('find bugs', {{schema: {BUGS_SCHEMA}, retry: {{max: 2}}}})
             return {{isNull: r === null}}"
        ))
        .await;
    assert_eq!(report.returned, json!({"isNull": true}));
    assert_eq!(
        h.client
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeCall::FollowUp { .. }))
            .count(),
        2,
        "retry.max is honoured"
    );
    assert!(report.agents[0]
        .error
        .as_deref()
        .expect("recorded")
        .contains("never matched"));
}

#[tokio::test(start_paused = true)]
async fn a_custom_retry_prompt_is_used_verbatim() {
    let h = Harness::new();
    h.client.with_default_reply(FakeReply::text("prose"));
    h.run(&format!(
        "await agent('x', {{schema: {BUGS_SCHEMA}, retry: {{max: 1, prompt: 'JSON ONLY'}}}})"
    ))
    .await;
    assert!(h.client.calls().contains(&FakeCall::FollowUp {
        sid: "f1".to_string(),
        task: "JSON ONLY".to_string()
    }));
}

#[tokio::test(start_paused = true)]
async fn a_reply_that_violates_the_schema_is_retried_not_accepted() {
    let h = Harness::new();
    // Valid JSON, wrong shape: `bugs` must be an array.
    h.client.push(FakeReply::text("{\"bugs\": 3}"));
    h.client.push_follow_up(FakeReply::text("{\"bugs\": []}"));
    let report = h
        .run(&format!(
            "return await agent('x', {{schema: {BUGS_SCHEMA}}})"
        ))
        .await;
    assert_eq!(report.returned, json!({"bugs": []}));
}

// ───────────────────────────────────────────────────────────────────────────
// determinism, args, usage, progress
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn runtime_traps_catch_indirection_the_static_scan_cannot_see() {
    let h = Harness::new();
    for expr in [
        "Date['now']()",
        "Math['random']()",
        "Reflect.construct(Date, [])",
        "new (Date)",
        "Intl['DateTimeFormat']()",
    ] {
        let report = h
            .run(&format!(
                "try {{ {expr}; return 'NOT TRAPPED' }} catch (e) {{ return String(e.message) }}"
            ))
            .await;
        let message = report.returned.as_str().expect("a string");
        assert!(
            message.contains("args"),
            "{expr} must be trapped at runtime with an actionable message, got {message:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn explicit_dates_still_work_inside_the_trap() {
    let h = Harness::new();
    let report = h
        .run("const d = new Date(2026, 0, 2); return {year: d.getFullYear(), parsed: Date.parse('2026-01-02T00:00:00Z')}")
        .await;
    assert_eq!(report.returned["year"], json!(2026));
    assert!(report.returned["parsed"].as_f64().expect("a number") > 0.0);
}

#[tokio::test(start_paused = true)]
async fn a_date_instance_cannot_smuggle_the_real_constructor_back() {
    let h = Harness::new();
    let report = h
        .run("try { return new Date(0).constructor.now() } catch (e) { return 'trapped' }")
        .await;
    assert_eq!(report.returned, json!("trapped"));
}

#[tokio::test(start_paused = true)]
async fn there_is_no_filesystem_network_or_module_loader_in_the_realm() {
    let h = Harness::new();
    let report = h
        .run(
            "return ['require', 'process', 'fetch', 'import', 'globalThis.__ccteam_agent']
               .map(n => { try { return typeof eval(n) } catch (e) { return 'absent' } })",
        )
        .await;
    for (i, kind) in report
        .returned
        .as_array()
        .expect("array")
        .iter()
        .enumerate()
    {
        assert!(
            kind == "undefined" || kind == "absent",
            "slot {i} should not exist, got {kind}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn args_reach_the_script_verbatim() {
    let h = Harness::new();
    let cfg = h.config().with_args(json!({"files": ["a.rs", "b.rs"]}));
    let report = h
        .run_with(
            "return {count: args.files.length, first: args.files[0]}",
            cfg,
        )
        .await;
    assert_eq!(report.returned, json!({"count": 2, "first": "a.rs"}));
}

#[tokio::test(start_paused = true)]
async fn absent_args_are_undefined_not_null() {
    let h = Harness::new();
    let report = h.run("return typeof args").await;
    assert_eq!(report.returned, json!("undefined"));
}

#[tokio::test(start_paused = true)]
async fn usage_is_passed_through_verbatim() {
    let h = Harness::new();
    h.client
        .set_usage(json!({"accounts": [{"harness": "claude", "used_pct": 61}]}));
    let report = h.run("return await usage()").await;
    assert_eq!(
        report.returned,
        json!({"accounts": [{"harness": "claude", "used_pct": 61}]})
    );
    assert!(h.client.calls().contains(&FakeCall::Usage));
}

#[tokio::test(start_paused = true)]
async fn phase_and_log_emit_progress_and_group_later_agents() {
    let h = Harness::new();
    let report = h
        .run("phase('Scan'); log('looking'); await agent('one'); phase('Fix'); await agent('two')")
        .await;

    let events = h.events();
    assert!(matches!(
        events.first(),
        Some(ProgressEvent::RunStarted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(ProgressEvent::RunFinished { .. })
    ));
    assert!(events
        .iter()
        .any(|e| matches!(e, ProgressEvent::PhaseStarted { title } if title == "Scan")));
    assert!(events
        .iter()
        .any(|e| matches!(e, ProgressEvent::Log { message, phase }
            if message == "looking" && phase.as_deref() == Some("Scan"))));
    assert_eq!(report.agents[0].phase.as_deref(), Some("Scan"));
    assert_eq!(report.agents[1].phase.as_deref(), Some("Fix"));
}

#[tokio::test(start_paused = true)]
async fn an_explicit_phase_option_beats_the_global_cursor() {
    let h = Harness::new();
    let report = h
        .run("phase('Scan'); await agent('one', {phase: 'Verify'})")
        .await;
    assert_eq!(report.agents[0].phase.as_deref(), Some("Verify"));
}

#[tokio::test(start_paused = true)]
async fn progress_events_carry_the_agent_lifecycle() {
    let h = Harness::new();
    h.client.push(FakeReply::text("answer").with_cost(0.75));
    h.run("await agent('one', {label: 'finder', vendor: 'kimi'})")
        .await;
    let events = h.events();
    assert!(events.iter().any(|e| matches!(e,
        ProgressEvent::AgentStarted { seq: 0, label, vendor: Some(AgentVendor::Kimi), cached: false }
            if label == "finder")));
    let finished = events
        .iter()
        .find_map(|e| match e {
            ProgressEvent::AgentFinished {
                outcome, cost_usd, ..
            } => Some((outcome.clone(), *cost_usd)),
            _ => None,
        })
        .expect("an AgentFinished event");
    assert_eq!(finished.0.expect("outcome").text, "answer");
    assert!((finished.1 - 0.75).abs() < f64::EPSILON);
}

#[tokio::test(start_paused = true)]
async fn meta_phases_are_announced_before_the_first_agent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    let sink: ProgressCallback = Arc::new(move |e| {
        sink_events.lock().expect("mutex").push(e);
    });
    let cfg = RunConfig::new(dir.path(), Arc::new(FakeClient::new()) as _).with_progress(sink);
    run_workflow(
        ScriptSource::inline(
            "export const meta = { name: 'x', description: 'd', phases: [{title: 'Scan'}, {title: 'Fix'}] }
             return 1",
        ),
        cfg,
    )
    .await
    .expect("run");

    let first = events.lock().expect("mutex")[0].clone();
    match first {
        ProgressEvent::RunStarted { name, phases, .. } => {
            assert_eq!(name, "x");
            assert_eq!(phases, vec!["Scan".to_string(), "Fix".to_string()]);
        }
        other => panic!("expected RunStarted first, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn the_default_label_is_a_readable_slice_of_the_task() {
    let h = Harness::new();
    let long = "x".repeat(200);
    let report = h.run(&format!("await agent('{long}')")).await;
    assert_eq!(report.agents[0].label.chars().count(), 60);
    assert!(report.agents[0].label.ends_with('…'));
}

// ───────────────────────────────────────────────────────────────────────────
// script-level failures
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn a_syntax_error_fails_the_run_with_a_readable_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = RunConfig::new(dir.path(), Arc::new(FakeClient::new()) as _);
    let err = run_workflow(ScriptSource::inline(format!("{META}const = = =")), cfg)
        .await
        .expect_err("a syntax error must fail the run");
    assert!(err.to_string().contains("did not compile"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_missing_script_file_is_reported_with_its_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("nope.js");
    let cfg = RunConfig::new(dir.path(), Arc::new(FakeClient::new()) as _);
    let err = run_workflow(ScriptSource::path(&missing), cfg)
        .await
        .expect_err("missing file");
    assert!(err.to_string().contains("nope.js"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_script_read_from_disk_runs_the_same_as_an_inline_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("wf.js");
    std::fs::write(&script, format!("{META}return await agent('from disk')")).expect("write");
    let client = Arc::new(FakeClient::new());
    let cfg = RunConfig::new(dir.path().join("run"), Arc::clone(&client) as _);
    let report = run_workflow(ScriptSource::path(&script), cfg)
        .await
        .expect("run");
    assert_eq!(report.returned, json!("ok"));
    assert_eq!(client.hires(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_banned_api_in_the_script_fails_before_anything_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = Arc::new(FakeClient::new());
    let cfg = RunConfig::new(dir.path(), Arc::clone(&client) as _);
    let err = run_workflow(
        ScriptSource::inline(format!("{META}await agent('x'); const t = Date.now()")),
        cfg,
    )
    .await
    .expect_err("rejected");
    assert!(err.to_string().contains("Date.now()"), "{err}");
    assert_eq!(client.hires(), 0, "nothing runs before the guard passes");
}
