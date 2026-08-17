//! Bounded, allocation-light HTTP latency diagnostics.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;

const SLOW_REQUEST: Duration = Duration::from_millis(500);
const MAX_ROUTES: usize = 256;
const SAMPLES_PER_ROUTE: usize = 512;
const FALLBACK_ROUTE: &str = "<unmatched>";
const OVERFLOW_METHOD: &str = "*";
const OVERFLOW_ROUTE: &str = "<overflow>";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteKey {
    method: String,
    route: String,
}

#[derive(Debug, Default)]
struct RouteSamples {
    count: u64,
    elapsed_us: VecDeque<u64>,
}

/// One bounded per-route latency summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteLatencyMetric {
    pub method: String,
    pub route: String,
    pub count: u64,
    pub p50_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

static ROUTES: OnceLock<Mutex<BTreeMap<RouteKey, RouteSamples>>> = OnceLock::new();

/// Axum middleware that records route-template latency and warns on a slow
/// response. Dynamic path values never become metric keys.
pub async fn record_request_latency(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| FALLBACK_ROUTE.to_string());
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed();
    let status = response.status().as_u16();

    record(&method, &route, elapsed);
    if elapsed > SLOW_REQUEST {
        tracing::warn!(
            route,
            method,
            status,
            elapsed_ms = elapsed.as_millis() as u64,
            "slow HTTP request"
        );
    }
    response
}

/// Snapshot all route summaries in stable `(method, route)` order.
pub fn route_latency_metrics() -> Vec<RouteLatencyMetric> {
    lock_routes()
        .iter()
        .map(|(key, samples)| {
            let mut elapsed = samples.elapsed_us.iter().copied().collect::<Vec<_>>();
            elapsed.sort_unstable();
            RouteLatencyMetric {
                method: key.method.clone(),
                route: key.route.clone(),
                count: samples.count,
                p50_us: percentile(&elapsed, 50),
                p99_us: percentile(&elapsed, 99),
                max_us: elapsed.last().copied().unwrap_or(0),
            }
        })
        .collect()
}

/// Return the heaviest progress kinds by appended bytes for internal
/// diagnostics. This is intentionally a getter, not a new REST surface.
pub fn top_progress_kinds(
    limit: usize,
) -> Vec<ccteam_harness::execution::progress_bridge::KindStat> {
    let mut stats = ccteam_harness::execution::progress_bridge::kind_stats();
    stats.sort_unstable_by(|left, right| {
        right
            .appended_bytes
            .cmp(&left.appended_bytes)
            .then_with(|| right.appended_count.cmp(&left.appended_count))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    stats.truncate(limit);
    stats
}

fn record(method: &str, route: &str, elapsed: Duration) {
    let mut routes = lock_routes();
    let mut key = RouteKey {
        method: method.to_string(),
        route: route.to_string(),
    };
    if !routes.contains_key(&key) && routes.len() >= MAX_ROUTES.saturating_sub(1) {
        key.method = OVERFLOW_METHOD.to_string();
        key.route = OVERFLOW_ROUTE.to_string();
    }
    let samples = routes.entry(key).or_default();
    samples.count = samples.count.saturating_add(1);
    if samples.elapsed_us.len() == SAMPLES_PER_ROUTE {
        samples.elapsed_us.pop_front();
    }
    samples
        .elapsed_us
        .push_back(elapsed.as_micros().min(u64::MAX as u128) as u64);
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = percentile
        .saturating_mul(sorted.len())
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn lock_routes() -> MutexGuard<'static, BTreeMap<RouteKey, RouteSamples>> {
    ROUTES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
