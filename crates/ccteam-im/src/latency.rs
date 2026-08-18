//! Latency-analysis logging helpers (V0.6.1 chat-mode timing instrumentation).
//!
//! The TG-mode-3 chat reply path crosses 7 stages (TG ingress → router →
//! mailbox → inbox drain → tmux send-keys → claude turn → hooks →
//! turns_mirror → outbound tail → TG egress). Each stage now emits one
//! `tracing::info!` with the fields below so a single grep can
//! reconstruct a full per-message timeline.
//!
//! Field conventions (kept stable across crates so log post-processing
//! doesn't need stage-specific parsers):
//!
//! - `event = "latency"` — common marker so `journalctl | grep latency`
//!   yields the timing rows and nothing else.
//! - `cid` — correlation id. For TG: `"tg-{message_id}"` (synthesized
//!   in `TelegramChannel::listen`). Flows through `ChannelMessage::id`
//!   and is preserved verbatim into the
//!   downstream logs that can see it (stages A–D, G). Stages F + parts
//!   of E correlate via `turn_id` instead — the Claude session never
//!   sees the cid.
//! - `stage` — short tag (`"tg.ingress"`, `"imd.route"`, ...).
//! - `elapsed_ms` — within-stage wall-clock duration.
//! - `queue_age_ms` / `tail_age_ms` — cross-stage wait time (e.g. how
//!   long an envelope sat in the inbox before the 5s drain tick picked
//!   it up).

use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GATEWAY_LOCK_SAMPLES: usize = 4096;
const SLOW_GATEWAY_HOLD: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
struct LatencyRing {
    count: u64,
    samples_us: VecDeque<u64>,
}

#[derive(Debug, Default)]
struct GatewayLockState {
    wait: LatencyRing,
    hold: LatencyRing,
}

/// Bounded latency distribution for one phase of the gateway mutex.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencySummary {
    /// Lifetime number of completed samples.
    pub count: u64,
    /// Median of the bounded recent sample ring, in microseconds.
    pub p50_us: u64,
    /// 99th percentile of the bounded recent sample ring, in microseconds.
    pub p99_us: u64,
    /// Maximum of the bounded recent sample ring, in microseconds.
    pub max_us: u64,
}

/// Snapshot of gateway-mutex wait and hold durations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GatewayLockMetrics {
    /// Request-to-acquisition latency.
    pub wait: LatencySummary,
    /// Acquisition-to-drop latency.
    pub hold: LatencySummary,
}

static GATEWAY_LOCK_STATE: OnceLock<Mutex<GatewayLockState>> = OnceLock::new();

/// A gateway guard that records its hold duration when dropped.
pub struct GatewayLockGuard<'a> {
    guard: tokio::sync::MutexGuard<'a, crate::gateway::Gateway>,
    acquired_at: Instant,
    site: &'static str,
}

impl Deref for GatewayLockGuard<'_> {
    type Target = crate::gateway::Gateway;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for GatewayLockGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for GatewayLockGuard<'_> {
    fn drop(&mut self) {
        let elapsed = self.acquired_at.elapsed();
        record_hold(elapsed);
        if elapsed > SLOW_GATEWAY_HOLD {
            tracing::warn!(
                site = self.site,
                hold_ms = elapsed.as_millis() as u64,
                "gateway mutex held too long"
            );
        }
    }
}

/// Acquire and instrument the daemon-wide gateway mutex.
pub async fn gateway_lock<'a>(
    gateway: &'a Arc<tokio::sync::Mutex<crate::gateway::Gateway>>,
    site: &'static str,
) -> GatewayLockGuard<'a> {
    let requested_at = Instant::now();
    let guard = gateway.lock().await;
    instrument_gateway_guard(guard, requested_at.elapsed(), site)
}

/// Blocking-pool counterpart used by synchronous status aggregation.
pub fn gateway_blocking_lock<'a>(
    gateway: &'a Arc<tokio::sync::Mutex<crate::gateway::Gateway>>,
    site: &'static str,
) -> GatewayLockGuard<'a> {
    let requested_at = Instant::now();
    let guard = gateway.blocking_lock();
    instrument_gateway_guard(guard, requested_at.elapsed(), site)
}

/// Wrap a guard acquired through a deadline-aware wait.
pub(crate) fn instrument_gateway_guard<'a>(
    guard: tokio::sync::MutexGuard<'a, crate::gateway::Gateway>,
    wait: Duration,
    site: &'static str,
) -> GatewayLockGuard<'a> {
    record_wait(wait);
    GatewayLockGuard {
        guard,
        acquired_at: Instant::now(),
        site,
    }
}

/// Snapshot the process-global bounded gateway-lock distributions.
pub fn gateway_lock_metrics() -> GatewayLockMetrics {
    let state = lock_gateway_state();
    GatewayLockMetrics {
        wait: summarize(&state.wait),
        hold: summarize(&state.hold),
    }
}

fn record_wait(elapsed: Duration) {
    record_ring(&mut lock_gateway_state().wait, elapsed);
}

fn record_hold(elapsed: Duration) {
    record_ring(&mut lock_gateway_state().hold, elapsed);
}

fn record_ring(ring: &mut LatencyRing, elapsed: Duration) {
    ring.count = ring.count.saturating_add(1);
    if ring.samples_us.len() == GATEWAY_LOCK_SAMPLES {
        ring.samples_us.pop_front();
    }
    ring.samples_us
        .push_back(elapsed.as_micros().min(u64::MAX as u128) as u64);
}

fn summarize(ring: &LatencyRing) -> LatencySummary {
    let mut samples = ring.samples_us.iter().copied().collect::<Vec<_>>();
    samples.sort_unstable();
    LatencySummary {
        count: ring.count,
        p50_us: percentile(&samples, 50),
        p99_us: percentile(&samples, 99),
        max_us: samples.last().copied().unwrap_or(0),
    }
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

fn lock_gateway_state() -> MutexGuard<'static, GatewayLockState> {
    GATEWAY_LOCK_STATE
        .get_or_init(|| Mutex::new(GatewayLockState::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wall-clock millis since UNIX epoch. Used by latency logs to compute
/// cross-stage age deltas (a downstream stage diffs this against an
/// upstream-recorded `ts` to get queue/tail wait time).
pub fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
