mod support {
    pub mod perf_fixture;
}

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ccteam_harness::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, ExecutionMode,
    HarnessAdapter, HarnessError, PermissionMode, SessionProtocol, SpawnCtx, ThreadEvent,
    ThreadHandle, ThreadStatus, ToolSurfaceRebuild, TurnId, TurnInput, TurnRouting, TurnSubmission,
};
use ccteam_im::gateway::Gateway;
use ccteam_im::latency::gateway_lock_metrics;
use ccteam_web::{router_with_state, AppState};
use futures::stream::{self, BoxStream};
use reqwest::StatusCode;
use support::perf_fixture::{
    assert_known_corruption, generate, HISTORY_TURNS, LIVE_SESSIONS, PROGRESS_SOURCE_ROWS, SLUG,
    STOPPED_SESSIONS,
};
use tokio::net::TcpListener;

const MIB: u64 = 1024 * 1024;

struct PerfAdapter;

#[async_trait::async_trait]
impl HarnessAdapter for PerfAdapter {
    fn name(&self) -> &'static str {
        "perf-gate"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: format!("{}-{}", ctx.slug, ctx.sid),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        })
    }

    async fn submit_turn(
        &self,
        _handle: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Ok(TurnId::new("perf-turn"))
    }

    async fn submit_turn_routed(
        &self,
        handle: &ThreadHandle,
        input: TurnInput,
        _routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        self.submit_turn(handle, input)
            .await
            .map(TurnSubmission::started)
    }

    fn event_attachment(&self) -> EventAttachment {
        EventAttachment::OneShot
    }

    async fn rebuild_tool_surface(
        &self,
        _handle: &ThreadHandle,
    ) -> Result<ToolSurfaceRebuild, HarnessError> {
        Ok(ToolSurfaceRebuild::RespawnRequired {
            reason: "perf test double".to_string(),
        })
    }

    fn events(&self, _handle: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        Box::pin(stream::pending())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "perf test double".to_string(),
        })
    }

    async fn close_thread(&self, _handle: &ThreadHandle) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn handle_directive(
        &self,
        _handle: &ThreadHandle,
        directive: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        Ok(DirectiveOutcome::Done {
            receipt: directive.name,
        })
    }

    async fn thread_status(&self, _handle: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn perf_gate() {
    if std::env::var("CCTEAM_PERF_GATE").as_deref() != Ok("1") {
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let isolated_home = root.path().join("isolated-home");
    std::fs::create_dir_all(&isolated_home).unwrap();
    std::env::set_var("HOME", &isolated_home);
    std::env::set_var("CCTEAM_HOME", isolated_home.join(".ccteam"));
    std::env::set_var("CCTEAM_CLAUDE_HOME", isolated_home.join(".claude"));
    std::env::set_var("CCTEAM_PROJECTS_ROOT", isolated_home.join("projects"));
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");

    let fixture = generate(&root.path().join("fixture-a"), 0x5eed_cafe_1234_5678);
    assert!(fixture.stats.generation_time < Duration::from_secs(60));
    assert!(
        (150 * MIB..=200 * MIB).contains(&fixture.stats.progress_bytes),
        "progress fixture is {:.1}MiB",
        fixture.stats.progress_bytes as f64 / MIB as f64
    );
    assert_eq!(fixture.stats.progress_lines, PROGRESS_SOURCE_ROWS + 1);
    assert!((840_000..=860_000).contains(&fixture.stats.flood_rows));
    assert_known_corruption(&fixture);

    let second_root = tempfile::tempdir().unwrap();
    let second = generate(second_root.path(), 0x5eed_cafe_1234_5678);
    assert!(second.stats.generation_time < Duration::from_secs(60));
    assert_eq!(fixture.stats.fixture_bytes, second.stats.fixture_bytes);
    assert_eq!(fixture.stats.progress_bytes, second.stats.progress_bytes);
    assert_eq!(fixture.stats.turn_bytes, second.stats.turn_bytes);
    drop(second);
    drop(second_root);
    println!(
        "perf-gate fixture: target=150-200MiB/~1M rows measured={:.1}MiB/{} rows gen={:.2}s live={} stopped={} turns={}",
        fixture.stats.progress_bytes as f64 / MIB as f64,
        fixture.stats.progress_lines,
        fixture.stats.generation_time.as_secs_f64(),
        LIVE_SESSIONS,
        STOPPED_SESSIONS,
        HISTORY_TURNS
    );

    let factory = Arc::new(|_vendor, _protocol| {
        Arc::new(PerfAdapter) as Arc<dyn HarnessAdapter + Send + Sync>
    });
    let mut gateway = Gateway::new_with_factory(factory, SLUG, fixture.project_dir.clone());
    gateway.set_sessions_config(ccteam_core::SessionsConfig {
        max_live: LIVE_SESSIONS as u32,
    });
    for _ in 0..LIVE_SESSIONS {
        gateway
            .create_session_api_proto(
                SLUG.to_string(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
                SessionProtocol::StreamJson,
                "web-api".to_string(),
            )
            .await
            .unwrap();
    }

    let app = AppState::new(fixture.paths.clone());
    let hydration_started = Instant::now();
    app.progress_projection
        .hydrate_now(&[SLUG.to_string()])
        .unwrap();
    let hydration_time = hydration_started.elapsed();
    let projection = Arc::clone(&app.progress_projection);
    let app = app.with_gateway_owned(gateway);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server =
        tokio::spawn(async move { axum::serve(listener, router_with_state(app)).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{addr}");

    for _ in 0..5 {
        assert_ok(&client, &format!("{base}/api/v1/status")).await;
    }
    let projection_before = projection.metrics();
    let journal_before = ccteam_core::journal::metrics();
    let status_samples = measure_gets(&client, &format!("{base}/api/v1/status"), 25).await;
    let projection_after = projection.metrics();
    let journal_after = ccteam_core::journal::metrics();
    let status_p95 = percentile(&status_samples, 95);
    let projection_bytes = projection_after
        .bytes_ingested
        .saturating_sub(projection_before.bytes_ingested);
    let journal_bytes = journal_after
        .bytes_read
        .saturating_sub(journal_before.bytes_read);
    let read_per_call = journal_bytes / status_samples.len() as u64;
    println!(
        "perf-gate status: target p95<100ms/ingest≈0/read<10MiB measured={:.2}ms/{}B/{:.3}MiB hydration={:.2}s",
        status_p95.as_secs_f64() * 1000.0,
        projection_bytes,
        read_per_call as f64 / MIB as f64,
        hydration_time.as_secs_f64()
    );
    assert_release(status_p95 < Duration::from_millis(100), "status p95");
    assert_eq!(projection_bytes, 0);
    assert!(read_per_call < 10 * MIB);

    let keep_status_running = Arc::new(AtomicBool::new(true));
    let status_started = Arc::new(AtomicU64::new(0));
    let mut status_workers = Vec::new();
    for _ in 0..4 {
        let client = client.clone();
        let url = format!("{base}/api/v1/status");
        let keep_running = Arc::clone(&keep_status_running);
        let started = Arc::clone(&status_started);
        status_workers.push(tokio::spawn(async move {
            while keep_running.load(Ordering::Relaxed) {
                started.fetch_add(1, Ordering::Relaxed);
                assert_ok(&client, &url).await;
            }
        }));
    }
    while status_started.load(Ordering::Relaxed) < 8 {
        tokio::task::yield_now().await;
    }
    let health_samples = measure_gets(&client, &format!("{base}/health"), 100).await;
    keep_status_running.store(false, Ordering::Relaxed);
    for worker in status_workers {
        worker.await.unwrap();
    }
    let health_p99 = percentile(&health_samples, 99);
    println!(
        "perf-gate health-during-status: target p99<10ms measured={:.2}ms probes={}",
        health_p99.as_secs_f64() * 1000.0,
        health_samples.len()
    );
    assert_release(health_p99 < Duration::from_millis(10), "health p99");

    let list_url = format!("{base}/api/v1/projects/{SLUG}/sessions");
    for _ in 0..5 {
        assert_ok(&client, &list_url).await;
    }
    let list_samples = measure_gets(&client, &list_url, 50).await;
    let list_p95 = percentile(&list_samples, 95);
    println!(
        "perf-gate session-list: target p95<50ms measured={:.2}ms sessions={}",
        list_p95.as_secs_f64() * 1000.0,
        LIVE_SESSIONS
    );
    assert_release(list_p95 < Duration::from_millis(50), "session-list p95");

    let _ = ccteam_core::journal::tail_valid(&fixture.progress_path, 200).unwrap();
    let tail_started = Instant::now();
    let tail = ccteam_core::journal::tail_valid(&fixture.progress_path, 200).unwrap();
    let tail_time = tail_started.elapsed();
    println!(
        "perf-gate journal-tail: target <50ms measured={:.2}ms rows={}",
        tail_time.as_secs_f64() * 1000.0,
        tail.events.len()
    );
    assert_eq!(tail.events.len(), 200);
    assert_release(tail_time < Duration::from_millis(50), "journal tail");

    let history_url = format!("{base}/api/v1/sessions/{}", fixture.history_sid);
    assert_ok(&client, &history_url).await;
    let history_samples = measure_gets(&client, &history_url, 20).await;
    let history_p95 = percentile(&history_samples, 95);
    println!(
        "perf-gate 10k-history: target p95<100ms measured={:.2}ms source_turns={}",
        history_p95.as_secs_f64() * 1000.0,
        HISTORY_TURNS
    );
    assert_release(
        history_p95 < Duration::from_millis(100),
        "session-history p95",
    );

    let lock = gateway_lock_metrics();
    println!(
        "perf-gate gateway-lock: target hold-p99<5ms measured={:.3}ms samples={} wait-p99={:.3}ms",
        lock.hold.p99_us as f64 / 1000.0,
        lock.hold.count,
        lock.wait.p99_us as f64 / 1000.0
    );
    assert!(lock.hold.count > 0);
    assert_release(lock.hold.p99_us < 5_000, "gateway lock hold p99");

    server.abort();
}

async fn measure_gets(client: &reqwest::Client, url: &str, count: usize) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let started = Instant::now();
        assert_ok(client, url).await;
        samples.push(started.elapsed());
    }
    samples
}

async fn assert_ok(client: &reqwest::Client, url: &str) {
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {url}");
    let _ = response.bytes().await.unwrap();
}

fn percentile(samples: &[Duration], percentile: usize) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = percentile
        .saturating_mul(sorted.len())
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn assert_release(condition: bool, label: &str) {
    if !cfg!(debug_assertions) {
        assert!(condition, "release performance target failed: {label}");
    }
}
