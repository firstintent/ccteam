//! Admission control: everything that decides *whether and when* a workflow's
//! next `agent()` call may reach the client.
//!
//! A dynamic workflow is a loop with a credit card attached. The script is
//! deliberately allowed to say `for (const f of 400 files) agent(...)`, so the
//! scheduler — not the author — is what keeps that from becoming 400
//! simultaneous sessions, a vendor ban, or a runaway bill.
//!
//! Five independent limiters, all checked on the way in:
//!
//! * **run cap** — total agents in flight for this run;
//! * **vendor pools** — per-harness slots, because "8 concurrent Claude
//!   sessions" and "32 concurrent dsh sessions" are different physical facts;
//! * **spawn rate** — a token bucket, so a fan-out of 200 ramps instead of
//!   stampeding a daemon that has to spawn 200 processes;
//! * **pool backoff** — when a harness says "limit", that pool stands down
//!   with exponential backoff instead of hammering;
//! * **brakes** — agent count, cost, wall clock, budget target.
//!
//! A tripped brake refuses NEW admissions and never touches work already in
//! flight. That is the ccteam red line "never kill a long session" applied to
//! workflows: the runner may decline to start more, but a worker that is
//! already thinking finishes its turn.
//!
//! Waiting is done on `tokio::sync::Semaphore` (FIFO-fair, so it is the pump)
//! and `tokio::time` (so every ramp, timeout and backoff is virtual under
//! `tokio::time::pause()` and therefore testable in microseconds).

use crate::client::ClientError;
use crate::progress::{ProgressCallback, ProgressEvent};
use ccteam_harness::AgentVendor;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Absolute ceiling on agents per run, independent of configuration. The
/// runaway backstop from the official contract: no workflow, however
/// configured, spawns more than this.
pub const HARD_AGENT_CAP: usize = 1000;

/// Per-harness concurrency slots.
///
/// Defaults encode how expensive a *session* is on each harness rather than
/// how fast the model is: the four subscription-shaped CLIs get 8, the two
/// that are cheap to hold open get 32, and `pi` — which is `LocalOnly` and
/// single-process-ish — gets 4.
#[derive(Debug, Clone)]
pub struct VendorPools {
    slots: HashMap<AgentVendor, usize>,
    /// Used when the script names no harness and the client will pick.
    fallback: usize,
}

impl Default for VendorPools {
    fn default() -> Self {
        let mut slots = HashMap::new();
        slots.insert(AgentVendor::Claude, 8);
        slots.insert(AgentVendor::Codex, 8);
        slots.insert(AgentVendor::Grok, 8);
        slots.insert(AgentVendor::Opencode, 8);
        slots.insert(AgentVendor::Kimi, 32);
        slots.insert(AgentVendor::Dsh, 32);
        slots.insert(AgentVendor::Pi, 4);
        Self { slots, fallback: 8 }
    }
}

impl VendorPools {
    /// Override one harness's slot count. Zero is clamped to one: a pool of
    /// zero would deadlock rather than refuse, which is never what a caller
    /// means.
    pub fn with(mut self, vendor: AgentVendor, slots: usize) -> Self {
        self.slots.insert(vendor, slots.max(1));
        self
    }

    /// Slots for calls that name no harness.
    pub fn with_fallback(mut self, slots: usize) -> Self {
        self.fallback = slots.max(1);
        self
    }

    fn slots_for(&self, vendor: Option<AgentVendor>) -> usize {
        match vendor {
            Some(v) => self.slots.get(&v).copied().unwrap_or(self.fallback).max(1),
            None => self.fallback,
        }
    }
}

/// Shape of the run's concurrency.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Agents in flight across the whole run.
    pub max_parallel: usize,
    pub pools: VendorPools,
    /// Hires per second, smoothed by a token bucket whose burst equals one
    /// second of rate.
    pub spawn_rate_per_sec: f64,
    /// First backoff after a harness reports a limit.
    pub backoff_initial: Duration,
    /// Ceiling for the doubling.
    pub backoff_max: Duration,
    /// How many calls may be *waiting* for admission before the runner starts
    /// refusing. Backpressure has to be visible: a script that queues 50 000
    /// calls should be told, not silently buffered.
    pub max_pending: usize,
    /// Hire attempts when the harness reports a limit (each preceded by the
    /// pool's backoff).
    pub hire_attempts: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_parallel: 32,
            pools: VendorPools::default(),
            spawn_rate_per_sec: 10.0,
            backoff_initial: Duration::from_secs(30),
            backoff_max: Duration::from_secs(600),
            max_pending: 1024,
            hire_attempts: 3,
        }
    }
}

/// Hard stops evaluated at admission.
#[derive(Debug, Clone)]
pub struct Brakes {
    /// Agents this run may start. Clamped to [`HARD_AGENT_CAP`].
    pub max_agents: usize,
    pub max_cost_usd: Option<f64>,
    /// Wall clock from run start. Measured on `tokio::time`, so a paused
    /// clock in tests makes this exact.
    pub wall_clock: Option<Duration>,
    /// The script-visible `budget.total`. Reaching it is a brake, matching
    /// the official contract's "HARD ceiling, not advisory".
    pub budget_total: Option<f64>,
}

impl Default for Brakes {
    fn default() -> Self {
        Self {
            max_agents: 100,
            max_cost_usd: None,
            wall_clock: None,
            budget_total: None,
        }
    }
}

/// Host-side run control. Held by whoever started the run; deliberately not
/// reachable from the script — a workflow must not be able to pause itself.
#[derive(Debug, Clone, Default)]
pub struct RunControl {
    inner: Arc<ControlInner>,
}

#[derive(Debug, Default)]
struct ControlInner {
    paused: std::sync::atomic::AtomicBool,
    wake: Notify,
}

impl RunControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop admitting new agents. In-flight agents are untouched.
    pub fn pause(&self) {
        self.inner.paused.store(true, Ordering::SeqCst);
    }

    /// Resume admission and wake everyone waiting at the gate.
    pub fn resume(&self) {
        self.inner.paused.store(false, Ordering::SeqCst);
        self.inner.wake.notify_waiters();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::SeqCst)
    }

    async fn gate(&self) {
        while self.inner.paused.load(Ordering::SeqCst) {
            // `notified()` is registered before the re-check inside the loop's
            // next iteration, so a resume racing with the check cannot be
            // missed indefinitely.
            let notified = self.inner.wake.notified();
            if !self.inner.paused.load(Ordering::SeqCst) {
                break;
            }
            notified.await;
        }
    }
}

/// Why admission was refused. Both variants reach the script as a thrown
/// error, which `parallel()`/`pipeline()` turn into a `null` slot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AdmissionError {
    Brake(String),
    QueueFull(String),
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdmissionError::Brake(m) | AdmissionError::QueueFull(m) => f.write_str(m),
        }
    }
}

/// Held for the lifetime of one agent; dropping it returns both slots.
pub(crate) struct Admission {
    _run: OwnedSemaphorePermit,
    _pool: OwnedSemaphorePermit,
}

struct Pool {
    slots: Arc<Semaphore>,
    backoff: Mutex<Backoff>,
}

struct Backoff {
    until: Option<Instant>,
    next: Duration,
}

struct TokenBucket {
    tokens: f64,
    rate: f64,
    capacity: f64,
    last: Instant,
}

pub(crate) struct Scheduler {
    cfg: SchedulerConfig,
    brakes: Brakes,
    run_slots: Arc<Semaphore>,
    pools: HashMap<Option<AgentVendor>, Pool>,
    rate: Mutex<TokenBucket>,
    started: Instant,
    admitted: AtomicUsize,
    pending: AtomicUsize,
    spent: Mutex<f64>,
    brake: Mutex<Option<String>>,
    control: RunControl,
    progress: Option<ProgressCallback>,
}

impl Scheduler {
    pub(crate) fn new(
        mut cfg: SchedulerConfig,
        mut brakes: Brakes,
        control: RunControl,
        progress: Option<ProgressCallback>,
    ) -> Self {
        cfg.max_parallel = cfg.max_parallel.clamp(1, HARD_AGENT_CAP);
        cfg.spawn_rate_per_sec = cfg.spawn_rate_per_sec.max(0.01);
        cfg.max_pending = cfg.max_pending.max(1);
        cfg.hire_attempts = cfg.hire_attempts.max(1);
        brakes.max_agents = brakes.max_agents.clamp(1, HARD_AGENT_CAP);

        let mut pools = HashMap::new();
        for vendor in AgentVendor::ALL.iter().copied().map(Some).chain([None]) {
            pools.insert(
                vendor,
                Pool {
                    slots: Arc::new(Semaphore::new(cfg.pools.slots_for(vendor))),
                    backoff: Mutex::new(Backoff {
                        until: None,
                        next: cfg.backoff_initial,
                    }),
                },
            );
        }

        let now = Instant::now();
        let rate = cfg.spawn_rate_per_sec;
        Self {
            run_slots: Arc::new(Semaphore::new(cfg.max_parallel)),
            pools,
            rate: Mutex::new(TokenBucket {
                tokens: rate,
                rate,
                capacity: rate,
                last: now,
            }),
            started: now,
            admitted: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            spent: Mutex::new(0.0),
            brake: Mutex::new(None),
            control,
            progress,
            cfg,
            brakes,
        }
    }

    pub(crate) fn budget_total(&self) -> Option<f64> {
        self.brakes.budget_total
    }

    pub(crate) fn spent(&self) -> f64 {
        *self.spent.lock().expect("spent mutex poisoned")
    }

    pub(crate) fn add_cost(&self, cost: f64) {
        if cost.is_finite() && cost > 0.0 {
            *self.spent.lock().expect("spent mutex poisoned") += cost;
        }
    }

    /// The brake that ended (or is ending) the run, if any.
    pub(crate) fn tripped(&self) -> Option<String> {
        self.brake.lock().expect("brake mutex poisoned").clone()
    }

    /// Wait for a slot in every limiter, or refuse with a readable reason.
    pub(crate) async fn admit(
        &self,
        vendor: Option<AgentVendor>,
    ) -> Result<Admission, AdmissionError> {
        self.check_limits()?;
        // Claimed BEFORE any waiting. A `parallel()` of 50 thunks enters this
        // function 50 times in the same microtask drain, so a
        // check-then-increment would let all 50 past a max_agents of 3: the
        // count has to move atomically with the decision.
        self.reserve_agent()?;

        let waiting = self.pending.fetch_add(1, Ordering::SeqCst) + 1;
        if waiting > self.cfg.max_pending {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            self.admitted.fetch_sub(1, Ordering::SeqCst);
            return Err(AdmissionError::QueueFull(format!(
                "workflow queue is full ({} calls already waiting for a slot); \
                 slow the fan-out or raise scheduler.max_pending",
                self.cfg.max_pending
            )));
        }
        let admission = self.acquire_slots(vendor).await;
        self.pending.fetch_sub(1, Ordering::SeqCst);

        // A brake may have tripped while this call queued. Refuse rather than
        // sneak one more agent past a ceiling the user asked for — and give
        // the reservation back, because this agent never started.
        if let Err(err) = self.check_limits() {
            self.admitted.fetch_sub(1, Ordering::SeqCst);
            return Err(err);
        }
        Ok(admission)
    }

    /// Atomically take one of the run's agent slots.
    fn reserve_agent(&self) -> Result<(), AdmissionError> {
        let max = self.brakes.max_agents;
        let mut current = self.admitted.load(Ordering::SeqCst);
        loop {
            if current >= max {
                return Err(self.trip(format!(
                    "agent brake: this run already started {current} agents (max_agents={max})"
                )));
            }
            match self.admitted.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(seen) => current = seen,
            }
        }
    }

    /// Wait for a run slot, a pool slot, the pool's backoff and a spawn
    /// token — in that order, so a caller that is going to wait anyway waits
    /// while holding as few resources as possible.
    async fn acquire_slots(&self, vendor: Option<AgentVendor>) -> Admission {
        self.control.gate().await;

        let run = Arc::clone(&self.run_slots)
            .acquire_owned()
            .await
            .expect("run semaphore is never closed");
        let pool = self.pool(vendor);
        let pool_permit = Arc::clone(&pool.slots)
            .acquire_owned()
            .await
            .expect("pool semaphore is never closed");

        // Backoff is waited out while holding the pool slot: that caps how
        // many callers can pile onto a limited harness the instant it
        // recovers, instead of releasing a thundering herd.
        self.wait_backoff(vendor).await;
        self.wait_spawn_token().await;

        Admission {
            _run: run,
            _pool: pool_permit,
        }
    }

    fn pool(&self, vendor: Option<AgentVendor>) -> &Pool {
        self.pools
            .get(&vendor)
            .or_else(|| self.pools.get(&None))
            .expect("the fallback pool always exists")
    }

    /// Put a harness's pool into (or deeper into) backoff. Called when the
    /// client reports [`ClientError::VendorLimit`].
    pub(crate) fn note_vendor_limit(&self, vendor: Option<AgentVendor>) -> Duration {
        let pool = self.pool(vendor);
        let mut b = pool.backoff.lock().expect("backoff mutex poisoned");
        let wait = b.next;
        b.until = Some(Instant::now() + wait);
        b.next = (b.next * 2).min(self.cfg.backoff_max);
        wait
    }

    /// A successful hire proves the harness is healthy again.
    pub(crate) fn note_vendor_ok(&self, vendor: Option<AgentVendor>) {
        let pool = self.pool(vendor);
        let mut b = pool.backoff.lock().expect("backoff mutex poisoned");
        b.until = None;
        b.next = self.cfg.backoff_initial;
    }

    pub(crate) async fn wait_backoff(&self, vendor: Option<AgentVendor>) {
        loop {
            let wait = {
                let pool = self.pool(vendor);
                let mut b = pool.backoff.lock().expect("backoff mutex poisoned");
                match b.until {
                    Some(until) => {
                        let now = Instant::now();
                        if now >= until {
                            b.until = None;
                            None
                        } else {
                            Some(until - now)
                        }
                    }
                    None => None,
                }
            };
            match wait {
                Some(d) => tokio::time::sleep(d).await,
                None => return,
            }
        }
    }

    async fn wait_spawn_token(&self) {
        loop {
            let wait = {
                let mut bucket = self.rate.lock().expect("rate mutex poisoned");
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
                bucket.tokens = (bucket.tokens + elapsed * bucket.rate).min(bucket.capacity);
                bucket.last = now;
                if bucket.tokens >= 1.0 {
                    bucket.tokens -= 1.0;
                    None
                } else {
                    Some(Duration::from_secs_f64((1.0 - bucket.tokens) / bucket.rate))
                }
            };
            match wait {
                Some(d) => tokio::time::sleep(d).await,
                None => return,
            }
        }
    }

    /// Everything except the agent count, which is claimed atomically in
    /// [`Scheduler::reserve_agent`].
    fn check_limits(&self) -> Result<(), AdmissionError> {
        if let Some(reason) = self.tripped() {
            return Err(AdmissionError::Brake(reason));
        }
        let spent = self.spent();
        if let Some(max) = self.brakes.max_cost_usd {
            if spent >= max {
                return Err(self.trip(format!(
                    "cost brake: {spent:.4} USD spent (max_cost_usd={max:.4})"
                )));
            }
        }
        if let Some(total) = self.brakes.budget_total {
            if spent >= total {
                return Err(self.trip(format!(
                    "budget target reached: {spent:.4} of {total:.4} USD spent — \
                     no further agents will be hired"
                )));
            }
        }
        if let Some(limit) = self.brakes.wall_clock {
            let elapsed = Instant::now().saturating_duration_since(self.started);
            if elapsed >= limit {
                return Err(self.trip(format!(
                    "time brake: run has been going for {}s (wall_clock={}s)",
                    elapsed.as_secs(),
                    limit.as_secs()
                )));
            }
        }
        Ok(())
    }

    /// Record the first brake and announce it exactly once.
    fn trip(&self, reason: String) -> AdmissionError {
        let mut guard = self.brake.lock().expect("brake mutex poisoned");
        if guard.is_none() {
            *guard = Some(reason.clone());
            if let Some(cb) = &self.progress {
                cb(ProgressEvent::BrakeTripped {
                    reason: reason.clone(),
                });
            }
        }
        AdmissionError::Brake(guard.clone().unwrap_or(reason))
    }

    /// How long to stand down after `err`, or `None` if it is not a limit.
    pub(crate) fn backoff_for(
        &self,
        vendor: Option<AgentVendor>,
        err: &ClientError,
    ) -> Option<Duration> {
        err.is_vendor_limit()
            .then(|| self.note_vendor_limit(vendor))
    }

    pub(crate) fn hire_attempts(&self) -> u32 {
        self.cfg.hire_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler(cfg: SchedulerConfig, brakes: Brakes) -> Scheduler {
        Scheduler::new(cfg, brakes, RunControl::new(), None)
    }

    #[test]
    fn pool_defaults_reflect_how_expensive_each_harness_session_is() {
        let pools = VendorPools::default();
        assert_eq!(pools.slots_for(Some(AgentVendor::Claude)), 8);
        assert_eq!(pools.slots_for(Some(AgentVendor::Codex)), 8);
        assert_eq!(pools.slots_for(Some(AgentVendor::Grok)), 8);
        assert_eq!(pools.slots_for(Some(AgentVendor::Opencode)), 8);
        assert_eq!(pools.slots_for(Some(AgentVendor::Kimi)), 32);
        assert_eq!(pools.slots_for(Some(AgentVendor::Dsh)), 32);
        assert_eq!(pools.slots_for(Some(AgentVendor::Pi)), 4);
        assert_eq!(pools.slots_for(None), 8);
    }

    #[test]
    fn pool_overrides_never_produce_a_deadlocking_zero() {
        let pools = VendorPools::default()
            .with(AgentVendor::Claude, 0)
            .with_fallback(0);
        assert_eq!(pools.slots_for(Some(AgentVendor::Claude)), 1);
        assert_eq!(pools.slots_for(None), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn agent_brake_is_clamped_to_the_hard_cap() {
        let s = scheduler(
            SchedulerConfig::default(),
            Brakes {
                max_agents: 1_000_000,
                ..Brakes::default()
            },
        );
        assert_eq!(s.brakes.max_agents, HARD_AGENT_CAP);
    }

    #[tokio::test(start_paused = true)]
    async fn spawn_ramp_is_honoured() {
        // 10/s with a one-second burst: the first 10 are free, the next 20
        // take two seconds.
        let s = scheduler(
            SchedulerConfig {
                spawn_rate_per_sec: 10.0,
                ..SchedulerConfig::default()
            },
            Brakes {
                max_agents: 100,
                ..Brakes::default()
            },
        );
        let start = Instant::now();
        for _ in 0..30 {
            drop(s.admit(None).await.expect("admitted"));
        }
        let elapsed = Instant::now() - start;
        assert!(
            elapsed >= Duration::from_secs(2),
            "30 spawns at 10/s must take at least 2s of ramp, took {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(2500),
            "the ramp must not over-throttle, took {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_harness_limit_backs_the_pool_off_exponentially() {
        let s = scheduler(SchedulerConfig::default(), Brakes::default());
        assert_eq!(
            s.note_vendor_limit(Some(AgentVendor::Codex)),
            Duration::from_secs(30)
        );

        let start = Instant::now();
        s.wait_backoff(Some(AgentVendor::Codex)).await;
        assert!(
            Instant::now() - start >= Duration::from_secs(30),
            "the pool must stand down for the whole backoff"
        );

        assert_eq!(
            s.note_vendor_limit(Some(AgentVendor::Codex)),
            Duration::from_secs(60),
            "a second limit doubles the wait"
        );
        // Another harness is unaffected: pools back off independently.
        s.wait_backoff(Some(AgentVendor::Claude)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_is_capped_and_reset_by_a_success() {
        let s = scheduler(
            SchedulerConfig {
                backoff_initial: Duration::from_secs(30),
                backoff_max: Duration::from_secs(120),
                ..SchedulerConfig::default()
            },
            Brakes::default(),
        );
        let mut last = Duration::ZERO;
        for _ in 0..8 {
            last = s.note_vendor_limit(Some(AgentVendor::Kimi));
        }
        assert_eq!(last, Duration::from_secs(120), "backoff must be capped");
        s.note_vendor_ok(Some(AgentVendor::Kimi));
        assert_eq!(
            s.note_vendor_limit(Some(AgentVendor::Kimi)),
            Duration::from_secs(30),
            "a healthy hire resets the ladder"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pause_stops_admission_and_resume_releases_it() {
        let control = RunControl::new();
        let s = Arc::new(Scheduler::new(
            SchedulerConfig::default(),
            Brakes::default(),
            control.clone(),
            None,
        ));
        control.pause();
        assert!(control.is_paused());

        let blocked = tokio::time::timeout(Duration::from_secs(5), s.admit(None)).await;
        assert!(blocked.is_err(), "a paused run must not admit new agents");

        control.resume();
        s.admit(None).await.expect("admission resumes");
    }

    #[tokio::test(start_paused = true)]
    async fn the_agent_brake_refuses_new_admissions_with_a_readable_reason() {
        let s = scheduler(
            SchedulerConfig::default(),
            Brakes {
                max_agents: 2,
                ..Brakes::default()
            },
        );
        drop(s.admit(None).await.expect("first"));
        drop(s.admit(None).await.expect("second"));
        let err = s.admit(None).await.err().expect("third must be refused");
        let text = err.to_string();
        assert!(text.contains("max_agents=2"), "{text}");
        assert_eq!(s.tripped().as_deref(), Some(text.as_str()));
    }

    #[tokio::test(start_paused = true)]
    async fn the_cost_and_budget_brakes_trip_on_spend() {
        let s = scheduler(
            SchedulerConfig::default(),
            Brakes {
                max_cost_usd: Some(1.0),
                ..Brakes::default()
            },
        );
        drop(s.admit(None).await.expect("first"));
        s.add_cost(1.25);
        let err = s.admit(None).await.err().expect("refused");
        assert!(err.to_string().contains("cost brake"), "{err}");

        let s = scheduler(
            SchedulerConfig::default(),
            Brakes {
                budget_total: Some(2.0),
                ..Brakes::default()
            },
        );
        s.add_cost(2.0);
        let err = s.admit(None).await.err().expect("refused");
        assert!(err.to_string().contains("budget target"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_wall_clock_brake_trips_on_elapsed_time() {
        let s = scheduler(
            SchedulerConfig::default(),
            Brakes {
                wall_clock: Some(Duration::from_secs(60)),
                ..Brakes::default()
            },
        );
        drop(s.admit(None).await.expect("first"));
        tokio::time::sleep(Duration::from_secs(61)).await;
        let err = s.admit(None).await.err().expect("refused");
        assert!(err.to_string().contains("time brake"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_pending_queue_refuses_instead_of_buffering() {
        let s = Arc::new(scheduler(
            SchedulerConfig {
                max_parallel: 1,
                max_pending: 2,
                ..SchedulerConfig::default()
            },
            Brakes::default(),
        ));
        // Hold the only run slot.
        let held = s.admit(None).await.expect("first");
        let mut waiters = Vec::new();
        for _ in 0..2 {
            let s2 = Arc::clone(&s);
            waiters.push(tokio::spawn(async move { s2.admit(None).await.is_ok() }));
        }
        // Let both waiters reach the gate.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let err = s.admit(None).await.err().expect("queue is full");
        assert!(matches!(err, AdmissionError::QueueFull(_)), "{err}");
        assert!(err.to_string().contains("max_pending"), "{err}");

        drop(held);
        for w in waiters {
            assert!(w.await.expect("join"), "queued callers still get in");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_vendor_pool_caps_concurrency() {
        let s = Arc::new(scheduler(
            SchedulerConfig {
                max_parallel: 32,
                pools: VendorPools::default().with(AgentVendor::Claude, 3),
                spawn_rate_per_sec: 1000.0,
                ..SchedulerConfig::default()
            },
            Brakes::default(),
        ));
        let held: Vec<_> = futures_join(&s, 3).await;
        assert_eq!(held.len(), 3);
        let blocked =
            tokio::time::timeout(Duration::from_secs(5), s.admit(Some(AgentVendor::Claude))).await;
        assert!(blocked.is_err(), "a fourth claude agent must wait");
        drop(held);
        s.admit(Some(AgentVendor::Claude))
            .await
            .expect("a freed slot admits the next one");
    }

    async fn futures_join(s: &Arc<Scheduler>, n: usize) -> Vec<Admission> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(
                s.admit(Some(AgentVendor::Claude))
                    .await
                    .expect("within the pool"),
            );
        }
        out
    }
}
