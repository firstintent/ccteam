//! Incremental in-memory projection of per-project progress journals.
//!
//! Every aggregate is folded from the byte cursor in [`Self::catch_up`]. The
//! append observer and every read-side query only trigger that same path, so an
//! event can never be counted once by a writer and again by a reader.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use anyhow::{Context, Result};
use ccteam_core::{CcteamPaths, CostSummary};
use ccteam_harness::execution::progress_bridge::{
    self, AGENT_DONE, CHAT_TURN_COMPLETED, DELEGATION_COLLECTED, DELEGATION_COMPLETED,
    DELEGATION_DENIED, DELEGATION_DISPATCHED, DELEGATION_NOTIFIED, DELEGATION_SPAWNED,
    DELEGATION_STOPPED,
};
use ccteam_harness::AgentVendor;
use chrono::{DateTime, Utc};
use serde_json::Value;

const TAIL_CAPACITY: usize = 200;
const MINUTES_24H: i64 = 24 * 60;

type Clock = dyn Fn() -> DateTime<Utc> + Send + Sync + 'static;

static PROJECTIONS: OnceLock<Mutex<Vec<Weak<ProgressProjection>>>> = OnceLock::new();
static OBSERVER_INSTALLED: OnceLock<()> = OnceLock::new();

/// Read-amplification counters for structural performance assertions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectionMetrics {
    /// Number of delta reads that reached the journal facade.
    pub catch_up_invocations: u64,
    /// Complete journal bytes consumed across those delta reads.
    pub bytes_ingested: u64,
    /// External truncations/rotations observed by the size guard.
    pub rotations: u64,
}

/// One vendor-specific pricing view of a session's completed turns.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SessionPricing {
    /// Accrued USD, absent until at least one turn can be priced honestly.
    pub cost_usd: Option<f64>,
    /// Turns carrying usage that had no matching price for this vendor.
    pub unpriced_turns: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct PricingAccumulator {
    cost_usd: f64,
    priced_turns: usize,
    unpriced_turns: usize,
}

/// Incremental session facts derived from progress rows carrying this sid.
#[derive(Debug, Clone, Default)]
pub struct SessionProjection {
    pricing: HashMap<AgentVendor, PricingAccumulator>,
    /// Raw tokens accrued from completed turns, absent until non-zero usage.
    pub tokens_total: Option<u64>,
    /// Most recent non-empty canonical model reported by a completed turn.
    pub observed_model: Option<String>,
    /// Most recent valid progress row carrying this sid/session_id.
    pub last_event: Option<Value>,
    /// Parsed timestamp of [`Self::last_event`], when valid.
    pub last_activity_at: Option<DateTime<Utc>>,
}

impl SessionProjection {
    /// Select the pricing accumulator for a session's actual vendor.
    pub fn pricing(&self, vendor: AgentVendor) -> SessionPricing {
        let value = self.pricing.get(&vendor).copied().unwrap_or_default();
        SessionPricing {
            cost_usd: (value.priced_turns > 0).then_some(value.cost_usd),
            unpriced_turns: value.unpriced_turns,
        }
    }
}

/// Delegation lifecycle counts for one project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DelegationProjection {
    /// All recorded delegated spawns.
    pub spawned: u64,
    /// All recorded dispatches.
    pub dispatched: u64,
    /// All recorded completed delegated turns.
    pub completed: u64,
    /// All recorded completion notifications.
    pub notified: u64,
    /// All recorded collection events.
    pub collected: u64,
    /// All recorded delegated stops.
    pub stopped: u64,
    /// All recorded guardrail denials.
    pub denied: u64,
    /// Completion notifications in the trailing 24 hourly minutes.
    pub notified_24h: u32,
    /// Guardrail denials in the trailing 24 hourly minutes.
    pub denied_24h: u32,
}

/// Immutable read-side view of a single project's progress aggregate.
#[derive(Debug, Clone, Default)]
pub struct ProjectProjectionSnapshot {
    /// Consumed byte cursor after the final complete line.
    pub offset: u64,
    /// Valid rows rejected as corrupt while consuming this file generation.
    pub corrupt_count: usize,
    /// Final parseable event in the journal.
    pub last_valid: Option<Value>,
    /// Up to the latest 200 parseable events, oldest first.
    pub tail: Vec<Value>,
    /// Lifetime and rolling-24h project cost aggregate.
    pub cost: CostSummary,
    /// Per-session incremental accumulators.
    pub sessions: HashMap<String, SessionProjection>,
    /// Delegation lifecycle and rolling counters.
    pub delegations: DelegationProjection,
    /// Low-frequency workflow facts needed by the legacy workflow summary.
    /// Telemetry, chat activity and other high-volume kinds never enter this
    /// vector, so project detail avoids re-reading the full journal.
    pub workflow_events: Vec<Value>,
    last_unscoped: Option<Value>,
}

impl ProjectProjectionSnapshot {
    /// Return at most the newest `n` tail rows in chronological order.
    pub fn recent_events(&self, n: usize) -> Vec<Value> {
        let start = self.tail.len().saturating_sub(n);
        self.tail[start..].to_vec()
    }

    /// Resolve one session's activity without scanning its journal.
    pub fn session_activity(
        &self,
        sid: &str,
        fallback_silent_seconds: u64,
        live: Option<ccteam_core::stall::LiveTurn>,
        now: DateTime<Utc>,
    ) -> ccteam_core::stall::ProgressActivityStatus {
        let selected = self
            .sessions
            .get(sid)
            .and_then(|session| session.last_event.clone())
            .or_else(|| self.last_unscoped.clone());
        let events = selected.into_iter().collect::<Vec<_>>();
        ccteam_core::stall::classify_session_activity(
            &events,
            sid,
            fallback_silent_seconds,
            live,
            now,
        )
    }
}

#[derive(Debug, Clone, Default)]
struct CostBucket {
    total: f64,
    count: u32,
    by_vendor: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct DelegationBucket {
    notified: u32,
    denied: u32,
}

#[derive(Debug, Clone, Default)]
struct SlugState {
    offset: u64,
    corrupt_count: usize,
    last_valid: Option<Value>,
    last_unscoped: Option<Value>,
    tail: VecDeque<Value>,
    lifetime_cost: f64,
    lifetime_by_vendor: BTreeMap<String, f64>,
    minute_cost: BTreeMap<i64, CostBucket>,
    sessions: HashMap<String, SessionProjection>,
    delegations: DelegationProjection,
    minute_delegations: BTreeMap<i64, DelegationBucket>,
    workflow_events: Vec<Value>,
}

#[derive(Default)]
struct SlugProjection {
    ingest: Mutex<()>,
    state: RwLock<SlugState>,
}

/// Shared incremental projection for every progress journal under one home.
pub struct ProgressProjection {
    paths: CcteamPaths,
    slugs: RwLock<HashMap<String, Arc<SlugProjection>>>,
    clock: Arc<Clock>,
    hydration_started: AtomicBool,
    hydration_complete: AtomicBool,
    catch_up_invocations: AtomicU64,
    bytes_ingested: AtomicU64,
    rotations: AtomicU64,
}

impl ProgressProjection {
    /// Return the process-wide projection for this path set, constructing and
    /// registering it on first use. The gateway and web composition roots call
    /// this independently and receive the same [`Arc`].
    pub fn new(paths: CcteamPaths) -> Arc<Self> {
        let mut registry = lock(PROJECTIONS.get_or_init(|| Mutex::new(Vec::new())));
        let mut shared = None;
        registry.retain(|projection| {
            let Some(projection) = projection.upgrade() else {
                return false;
            };
            if projection.paths.root == paths.root
                && projection.paths.projects_root == paths.projects_root
            {
                shared = Some(projection);
            }
            true
        });
        if let Some(projection) = shared {
            return projection;
        }
        let projection = Self::construct(paths, Arc::new(Utc::now));
        registry.push(Arc::downgrade(&projection));
        drop(registry);
        install_observer();
        projection
    }

    #[cfg(test)]
    fn new_with_clock(paths: CcteamPaths, clock: Arc<Clock>) -> Arc<Self> {
        let projection = Self::construct(paths, clock);
        register_projection(&projection);
        projection
    }

    fn construct(paths: CcteamPaths, clock: Arc<Clock>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            slugs: RwLock::new(HashMap::new()),
            clock,
            hydration_started: AtomicBool::new(false),
            hydration_complete: AtomicBool::new(false),
            catch_up_invocations: AtomicU64::new(0),
            bytes_ingested: AtomicU64::new(0),
            rotations: AtomicU64::new(0),
        })
    }

    /// Start a one-shot, non-blocking hydration pass over registered projects.
    /// Each project journal is consumed in its own blocking task.
    pub fn start_hydration(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self
            .hydration_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let projection = Arc::clone(self);
        handle.spawn(async move {
            let paths = projection.paths.clone();
            let slugs = tokio::task::spawn_blocking(move || {
                ccteam_core::collect_projects(&paths)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|project| project.state.slug)
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            for slug in slugs {
                let projection = Arc::clone(&projection);
                let result = tokio::task::spawn_blocking(move || projection.catch_up(&slug)).await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "progress projection hydration failed")
                    }
                    Err(error) => {
                        tracing::warn!(%error, "progress projection hydration task failed")
                    }
                }
            }
            projection.hydration_complete.store(true, Ordering::Release);
        });
    }

    /// Synchronously hydrate specific slugs. Intended for deterministic tests
    /// and composition roots that are already running on a blocking thread.
    pub fn hydrate_now(&self, slugs: &[String]) -> Result<()> {
        self.hydration_started.store(true, Ordering::Release);
        for slug in slugs {
            self.catch_up(slug)?;
        }
        self.hydration_complete.store(true, Ordering::Release);
        Ok(())
    }

    /// Whether the startup hydration pass has not completed yet.
    pub fn warming_up(&self) -> bool {
        !self.hydration_complete.load(Ordering::Acquire)
    }

    /// Query one project. A metadata check first catches hook fallback writes
    /// and external truncation before cloning the in-memory aggregate.
    pub fn project_snapshot(&self, slug: &str) -> ProjectProjectionSnapshot {
        if let Err(error) = self.catch_up_for_query(slug) {
            tracing::warn!(slug, %error, "progress projection query catch-up failed");
        }
        let state = self.slug(slug);
        let state = read_lock(&state.state);
        snapshot(&state, (self.clock)())
    }

    /// Query one session accumulator without cloning the project's tail or
    /// sibling session map.
    pub fn session_snapshot(&self, slug: &str, sid: &str) -> Option<SessionProjection> {
        if let Err(error) = self.catch_up_for_query(slug) {
            tracing::warn!(slug, %error, "progress projection session catch-up failed");
        }
        read_lock(&self.slug(slug).state).sessions.get(sid).cloned()
    }

    /// Snapshot projection ingestion metrics.
    pub fn metrics(&self) -> ProjectionMetrics {
        ProjectionMetrics {
            catch_up_invocations: self.catch_up_invocations.load(Ordering::Relaxed),
            bytes_ingested: self.bytes_ingested.load(Ordering::Relaxed),
            rotations: self.rotations.load(Ordering::Relaxed),
        }
    }

    fn slug(&self, slug: &str) -> Arc<SlugProjection> {
        if let Some(existing) = read_lock(&self.slugs).get(slug).cloned() {
            return existing;
        }
        write_lock(&self.slugs)
            .entry(slug.to_string())
            .or_default()
            .clone()
    }

    fn progress_path(&self, slug: &str) -> PathBuf {
        self.paths.progress_jsonl(slug)
    }

    fn observed_path(&self, path: &Path) -> Option<String> {
        if path.parent()? != self.paths.progress_dir() {
            return None;
        }
        path.file_name()?
            .to_str()?
            .strip_suffix(".jsonl")
            .filter(|slug| !slug.is_empty())
            .map(str::to_string)
    }

    fn catch_up_path(&self, path: &Path) {
        let Some(slug) = self.observed_path(path) else {
            return;
        };
        if let Err(error) = self.catch_up(&slug) {
            tracing::warn!(slug, %error, "progress projection observer catch-up failed");
        }
    }

    fn catch_up(&self, slug: &str) -> Result<()> {
        let projection = self.slug(slug);
        let _ingest = lock(&projection.ingest);
        self.catch_up_locked(slug, &projection)
    }

    fn catch_up_for_query(&self, slug: &str) -> Result<()> {
        let projection = self.slug(slug);
        let path = self.progress_path(slug);
        let offset = read_lock(&projection.state).offset;
        let size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
        };
        if size == offset {
            return Ok(());
        }
        let _ingest = match projection.ingest.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
        self.catch_up_locked(slug, &projection)
    }

    fn catch_up_locked(&self, slug: &str, projection: &SlugProjection) -> Result<()> {
        let path = self.progress_path(slug);
        let mut offset = read_lock(&projection.state).offset;
        let size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
        };

        if size < offset {
            *write_lock(&projection.state) = SlugState::default();
            self.rotations.fetch_add(1, Ordering::Relaxed);
            offset = 0;
        }
        if size == offset {
            return Ok(());
        }

        self.catch_up_invocations.fetch_add(1, Ordering::Relaxed);
        let delta = ccteam_core::journal::read_delta(&path, offset)?;
        let consumed = delta.next_offset.saturating_sub(offset);
        let now = (self.clock)();
        let mut state = write_lock(&projection.state);
        for event in delta.events {
            fold_event(&mut state, event, now);
        }
        state.corrupt_count = state.corrupt_count.saturating_add(delta.corrupt_count);
        state.offset = delta.next_offset;
        drop(state);
        self.bytes_ingested.fetch_add(consumed, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
fn register_projection(projection: &Arc<ProgressProjection>) {
    lock(PROJECTIONS.get_or_init(|| Mutex::new(Vec::new()))).push(Arc::downgrade(projection));
    install_observer();
}

fn install_observer() {
    OBSERVER_INSTALLED.get_or_init(|| {
        let _ = progress_bridge::set_persist_observer(Box::new(notify_persisted));
    });
}

fn notify_persisted(path: &Path) {
    let projections = {
        let mut registry = lock(PROJECTIONS.get_or_init(|| Mutex::new(Vec::new())));
        let mut live = Vec::with_capacity(registry.len());
        registry.retain(|projection| {
            let Some(projection) = projection.upgrade() else {
                return false;
            };
            live.push(projection);
            true
        });
        live
    };
    for projection in projections {
        projection.catch_up_path(path);
    }
}

fn fold_event(state: &mut SlugState, event: Value, now: DateTime<Utc>) {
    state.last_valid = Some(event.clone());
    if state.tail.len() == TAIL_CAPACITY {
        state.tail.pop_front();
    }
    state.tail.push_back(event.clone());

    let sid = ccteam_core::progress::event_sid(&event).map(str::to_string);
    if let Some(sid) = sid.as_ref() {
        let session = state.sessions.entry(sid.clone()).or_default();
        session.last_activity_at = event_timestamp(&event);
        session.last_event = Some(event.clone());
    } else {
        state.last_unscoped = Some(event.clone());
    }

    let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
    if matches!(
        kind,
        "agent_spawn" | AGENT_DONE | "escalation" | "gate_triggered"
    ) {
        state.workflow_events.push(event.clone());
    }
    if kind == AGENT_DONE {
        fold_cost(state, &event, now);
    }
    if kind == CHAT_TURN_COMPLETED {
        if let Some(sid) = sid {
            fold_session_turn(state.sessions.entry(sid).or_default(), &event);
        }
    }
    fold_delegation(state, kind, &event, now);
}

fn fold_cost(state: &mut SlugState, event: &Value, now: DateTime<Utc>) {
    let cost = event.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    state.lifetime_cost += cost;
    let vendor = event.get("vendor").and_then(Value::as_str);
    if let Some(vendor) = vendor {
        *state
            .lifetime_by_vendor
            .entry(vendor.to_string())
            .or_insert(0.0) += cost;
    }

    let now_minute = minute(now);
    let event_minute = bucket_minute(event, now).min(now_minute);
    let oldest = now_minute.saturating_sub(MINUTES_24H);
    state.minute_cost.retain(|minute, _| *minute >= oldest);
    if event_minute < oldest {
        return;
    }
    let bucket = state.minute_cost.entry(event_minute).or_default();
    bucket.total += cost;
    bucket.count = bucket.count.saturating_add(1);
    if let Some(vendor) = vendor {
        *bucket.by_vendor.entry(vendor.to_string()).or_insert(0.0) += cost;
    }
}

fn fold_session_turn(session: &mut SessionProjection, event: &Value) {
    if let Some(model) = event
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        session.observed_model = Some(model.to_string());
    }
    let Some(usage) = event.get("usage").and_then(|usage| {
        serde_json::from_value::<ccteam_cost::UnifiedTokenUsage>(usage.clone()).ok()
    }) else {
        return;
    };
    if usage.total() > 0 {
        session.tokens_total = Some(
            session
                .tokens_total
                .unwrap_or(0)
                .saturating_add(usage.total()),
        );
    }
    let model = event.get("model").and_then(Value::as_str).unwrap_or("");
    for vendor in AgentVendor::ALL {
        let accumulator = session.pricing.entry(*vendor).or_default();
        match ccteam_cost::resolve_turn_cost(&usage, vendor.cost_vendor(), model) {
            Some(cost) => {
                accumulator.cost_usd += cost;
                accumulator.priced_turns += 1;
            }
            None => accumulator.unpriced_turns += 1,
        }
    }
}

fn fold_delegation(state: &mut SlugState, kind: &str, event: &Value, now: DateTime<Utc>) {
    let counter = match kind {
        DELEGATION_SPAWNED => &mut state.delegations.spawned,
        DELEGATION_DISPATCHED => &mut state.delegations.dispatched,
        DELEGATION_COMPLETED => &mut state.delegations.completed,
        DELEGATION_NOTIFIED => &mut state.delegations.notified,
        DELEGATION_COLLECTED => &mut state.delegations.collected,
        DELEGATION_STOPPED => &mut state.delegations.stopped,
        DELEGATION_DENIED => &mut state.delegations.denied,
        _ => return,
    };
    *counter = counter.saturating_add(1);
    if kind != DELEGATION_NOTIFIED && kind != DELEGATION_DENIED {
        return;
    }

    let now_minute = minute(now);
    let event_minute = bucket_minute(event, now).min(now_minute);
    let oldest = now_minute.saturating_sub(MINUTES_24H);
    state
        .minute_delegations
        .retain(|minute, _| *minute >= oldest);
    if event_minute < oldest {
        return;
    }
    let bucket = state.minute_delegations.entry(event_minute).or_default();
    if kind == DELEGATION_NOTIFIED {
        bucket.notified = bucket.notified.saturating_add(1);
    } else {
        bucket.denied = bucket.denied.saturating_add(1);
    }
}

fn snapshot(state: &SlugState, now: DateTime<Utc>) -> ProjectProjectionSnapshot {
    let oldest = minute(now).saturating_sub(MINUTES_24H);
    let mut cost = CostSummary {
        cost_total_usd: state.lifetime_cost,
        cost_total_by_vendor: state.lifetime_by_vendor.clone(),
        ..CostSummary::default()
    };
    for bucket in state.minute_cost.range(oldest..).map(|(_, bucket)| bucket) {
        cost.cost_24h_usd += bucket.total;
        cost.session_count_24h = cost.session_count_24h.saturating_add(bucket.count);
        for (vendor, value) in &bucket.by_vendor {
            *cost.cost_24h_by_vendor.entry(vendor.clone()).or_insert(0.0) += value;
        }
    }
    let mut delegations = state.delegations;
    for bucket in state
        .minute_delegations
        .range(oldest..)
        .map(|(_, bucket)| bucket)
    {
        delegations.notified_24h = delegations.notified_24h.saturating_add(bucket.notified);
        delegations.denied_24h = delegations.denied_24h.saturating_add(bucket.denied);
    }
    ProjectProjectionSnapshot {
        offset: state.offset,
        corrupt_count: state.corrupt_count,
        last_valid: state.last_valid.clone(),
        tail: state.tail.iter().cloned().collect(),
        cost,
        sessions: state.sessions.clone(),
        delegations,
        workflow_events: state.workflow_events.clone(),
        last_unscoped: state.last_unscoped.clone(),
    }
}

fn event_timestamp(event: &Value) -> Option<DateTime<Utc>> {
    event
        .get("ts")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn bucket_minute(event: &Value, fallback: DateTime<Utc>) -> i64 {
    event_timestamp(event)
        .map(minute)
        .unwrap_or_else(|| minute(fallback))
}

fn minute(value: DateTime<Utc>) -> i64 {
    value.timestamp().div_euclid(60)
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_lock<T>(value: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    value
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_lock<T>(value: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    value
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use chrono::{Duration, TimeZone};
    use serde_json::json;

    fn test_paths(root: &Path) -> CcteamPaths {
        CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 16, 12, 34, 0)
            .single()
            .unwrap()
    }

    fn projection(paths: CcteamPaths) -> Arc<ProgressProjection> {
        let now = fixed_now();
        ProgressProjection::new_with_clock(paths, Arc::new(move || now))
    }

    fn write_lines(path: &Path, rows: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(path).unwrap();
        for row in rows {
            serde_json::to_writer(&mut file, row).unwrap();
            file.write_all(b"\n").unwrap();
        }
    }

    fn append_raw(path: &Path, row: &Value) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        serde_json::to_writer(&mut file, row).unwrap();
        file.write_all(b"\n").unwrap();
    }

    fn agent_done(sid: &str, vendor: &str, cost: f64, ts: DateTime<Utc>) -> Value {
        json!({
            "event": AGENT_DONE,
            "session_id": sid,
            "vendor": vendor,
            "cost_usd": cost,
            "ts": ts.to_rfc3339(),
        })
    }

    #[test]
    fn mixed_fixture_projects_cost_sessions_delegations_tail_and_corruption() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let path = paths.progress_jsonl("demo");
        let now = fixed_now();
        let usage = ccteam_cost::UnifiedTokenUsage {
            output_tokens: 1_000_000,
            ..Default::default()
        };
        let mut rows = vec![
            agent_done("s1", "claude", 2.0, now - Duration::hours(25)),
            agent_done("s4", "claude", 0.5, now - Duration::hours(24)),
            agent_done("s2", "codex", 3.0, now - Duration::minutes(10)),
            json!({
                "event": CHAT_TURN_COMPLETED,
                "sid": "s1",
                "model": "claude-sonnet-4-6",
                "usage": usage,
                "ts": (now - Duration::minutes(5)).to_rfc3339(),
            }),
            json!({
                "event": DELEGATION_SPAWNED,
                "sid": "s3",
                "ts": (now - Duration::minutes(4)).to_rfc3339(),
            }),
            json!({
                "event": DELEGATION_NOTIFIED,
                "sid": "s3",
                "ts": (now - Duration::minutes(3)).to_rfc3339(),
            }),
            json!({
                "event": DELEGATION_DENIED,
                "sid": "s3",
                "ts": (now - Duration::hours(25)).to_rfc3339(),
            }),
        ];
        rows.extend((0..205).map(|seq| {
            json!({
                "event": "gate_triggered",
                "seq": seq,
                "ts": (now - Duration::seconds(204 - seq)).to_rfc3339(),
            })
        }));
        write_lines(&path, &rows[..7]);
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b"{broken json}\n").unwrap();
            for row in &rows[7..] {
                serde_json::to_writer(&mut file, row).unwrap();
                file.write_all(b"\n").unwrap();
            }
        }

        let projection = projection(paths);
        projection.hydrate_now(&["demo".to_string()]).unwrap();
        let snapshot = projection.project_snapshot("demo");

        assert_eq!(snapshot.offset, std::fs::metadata(&path).unwrap().len());
        assert_eq!(snapshot.corrupt_count, 1);
        assert_eq!(snapshot.cost.cost_total_usd, 5.5);
        assert_eq!(snapshot.cost.cost_24h_usd, 3.5);
        assert_eq!(snapshot.cost.session_count_24h, 2);
        assert_eq!(snapshot.cost.cost_total_by_vendor["claude"], 2.5);
        assert_eq!(snapshot.cost.cost_total_by_vendor["codex"], 3.0);
        assert_eq!(snapshot.cost.cost_24h_by_vendor["codex"], 3.0);
        assert_eq!(snapshot.cost.cost_24h_by_vendor["claude"], 0.5);

        let session = snapshot.sessions.get("s1").unwrap();
        assert_eq!(session.tokens_total, Some(1_000_000));
        assert_eq!(session.observed_model.as_deref(), Some("claude-sonnet-4-6"));
        assert!((session.pricing(AgentVendor::Claude).cost_usd.unwrap() - 15.0).abs() < 0.001);
        assert_eq!(session.last_activity_at, Some(now - Duration::minutes(5)));

        assert_eq!(snapshot.delegations.spawned, 1);
        assert_eq!(snapshot.delegations.notified, 1);
        assert_eq!(snapshot.delegations.notified_24h, 1);
        assert_eq!(snapshot.delegations.denied, 1);
        assert_eq!(snapshot.delegations.denied_24h, 0);

        assert_eq!(snapshot.tail.len(), TAIL_CAPACITY);
        assert_eq!(snapshot.tail.first().unwrap()["seq"], 5);
        assert_eq!(snapshot.tail.last().unwrap()["seq"], 204);
        assert_eq!(snapshot.last_valid.as_ref().unwrap()["seq"], 204);
    }

    #[test]
    fn observer_and_hook_fallback_share_one_byte_cursor_without_double_counting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let path = paths.progress_jsonl("observer");
        let now = fixed_now();
        write_lines(&path, &[agent_done("s1", "claude", 1.0, now)]);
        let projection = projection(paths);
        projection.hydrate_now(&["observer".to_string()]).unwrap();
        let hydrated_metrics = projection.metrics();

        progress_bridge::append_event(
            &path,
            &agent_done("s2", "claude", 2.0, now - Duration::minutes(1)),
        )
        .unwrap();
        let observer_metrics = projection.metrics();
        assert!(observer_metrics.bytes_ingested > hydrated_metrics.bytes_ingested);

        append_raw(
            &path,
            &agent_done("s3", "codex", 4.0, now - Duration::minutes(2)),
        );
        assert_eq!(projection.metrics(), observer_metrics);
        let snapshot = projection.project_snapshot("observer");
        assert_eq!(snapshot.cost.cost_total_usd, 7.0);
        assert_eq!(snapshot.offset, std::fs::metadata(&path).unwrap().len());

        let caught_up_metrics = projection.metrics();
        for _ in 0..5 {
            assert_eq!(
                projection.project_snapshot("observer").cost.cost_total_usd,
                7.0
            );
        }
        assert_eq!(projection.metrics(), caught_up_metrics);
    }

    #[test]
    fn no_new_data_queries_do_not_ingest_more_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let path = paths.progress_jsonl("stable");
        write_lines(&path, &[agent_done("s1", "claude", 1.0, fixed_now())]);
        let projection = projection(paths);
        projection.hydrate_now(&["stable".to_string()]).unwrap();
        let metrics = projection.metrics();

        for _ in 0..10 {
            let snapshot = projection.project_snapshot("stable");
            assert_eq!(snapshot.cost.cost_total_usd, 1.0);
        }
        assert_eq!(projection.metrics().bytes_ingested, metrics.bytes_ingested);
        assert_eq!(
            projection.metrics().catch_up_invocations,
            metrics.catch_up_invocations
        );
    }

    #[test]
    fn smaller_file_resets_and_rehydrates_the_slug() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let path = paths.progress_jsonl("rotated");
        let mut initial = agent_done("s1", "claude", 9.0, fixed_now());
        initial["padding"] = Value::String("x".repeat(512));
        write_lines(&path, &[initial]);
        let projection = projection(paths);
        projection.hydrate_now(&["rotated".to_string()]).unwrap();
        assert_eq!(
            projection.project_snapshot("rotated").cost.cost_total_usd,
            9.0
        );

        write_lines(
            &path,
            &[agent_done(
                "s2",
                "codex",
                1.5,
                fixed_now() - Duration::minutes(1),
            )],
        );
        let snapshot = projection.project_snapshot("rotated");
        assert_eq!(snapshot.cost.cost_total_usd, 1.5);
        assert_eq!(snapshot.sessions.len(), 1);
        assert!(snapshot.sessions.contains_key("s2"));
        assert_eq!(snapshot.offset, std::fs::metadata(&path).unwrap().len());
        assert_eq!(projection.metrics().rotations, 1);
    }

    #[test]
    fn warming_flag_clears_after_hydration() {
        let tmp = tempfile::TempDir::new().unwrap();
        let projection = projection(test_paths(tmp.path()));
        assert!(projection.warming_up());
        projection.hydrate_now(&["empty".to_string()]).unwrap();
        assert!(!projection.warming_up());
    }
}
