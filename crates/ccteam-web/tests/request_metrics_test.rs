use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{middleware::from_fn, routing::get, Router};
use ccteam_web::metrics::{record_request_latency, route_latency_metrics, top_progress_kinds};
use tokio::net::TcpListener;

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn route_templates_are_recorded_and_only_slow_requests_warn() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let captured = Arc::clone(&captured);
        move || CaptureWriter(Arc::clone(&captured))
    };
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(writer)
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);

    let app = Router::new()
        .route("/fast/{id}", get(|| async { "fast" }))
        .route(
            "/slow/{id}",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(525)).await;
                "slow"
            }),
        )
        .layer(from_fn(record_request_latency));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    assert_eq!(
        client
            .get(format!("http://{addr}/fast/dynamic-value"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("http://{addr}/slow/another-value"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    server.abort();

    let metrics = route_latency_metrics();
    assert!(metrics
        .iter()
        .any(|metric| metric.method == "GET" && metric.route == "/fast/{id}"));
    assert!(metrics.iter().any(|metric| {
        metric.method == "GET" && metric.route == "/slow/{id}" && metric.max_us >= 500_000
    }));
    assert!(metrics
        .iter()
        .all(|metric| !metric.route.contains("dynamic-value")));
    assert!(top_progress_kinds(5).len() <= 5);

    let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("slow HTTP request"), "logs: {logs}");
    assert!(logs.contains("/slow/{id}"), "logs: {logs}");
    assert!(logs.contains("elapsed_ms"), "logs: {logs}");
    assert!(!logs.contains("/fast/{id}"), "logs: {logs}");
}
