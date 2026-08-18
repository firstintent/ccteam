//! DSH web runtime supervision — the per-identity `dsh web` process core.
//!
//! One identity, one runtime: this manager owns the instance map, the start
//! claims and the child processes, so every consumer shares the SAME `dsh web`
//! process per identity **by construction** rather than by convention. Today
//! the consumer is the ccteam web companion proxy (`ccteam-web::dsh_web`);
//! the DSH adapter connects to the same instances next.
//!
//! Deliberately free of web types (no axum, no `AppState`, no `Identity`):
//! `ccteam-web` depends on this crate, never the other way round. Identities
//! arrive as [`DshRuntimeIdentity`] and REST/JSON shaping stays in the caller.
//!
//! These instances are NOT ccteam sessions: they are local vendor web servers
//! keyed by an authenticated identity, never entries in the gateway live map.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};

use crate::execution::dsh_acp::spawn_spec::DshSpawnSpec;
use crate::execution::dsh_acp::{
    build_web_spawn_spec, is_ccteam_managed_dsh_orphan, tenant_home_segment, DshWebSpawnOptions,
    DSH_NATIVE_WEB_PROFILE, DSH_WEB_PROFILE,
};

const DEFAULT_ATTACH_URL: &str = "http://127.0.0.1:3080";
const ATTACH_URL_ENV: &str = "CCTEAM_DSH_WEB_ATTACH_URL";
const READINESS_PREFIX: &str = "dsh web: http://127.0.0.1:";
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const ORPHAN_STOP_GRACE: Duration = Duration::from_millis(750);
const ERROR_TAIL_LINES: usize = 24;

/// Runtime wiring the manager only learns once the daemon has bound its ports.
/// Handed in through [`DshRuntimeManager::configure`]; until then the manager
/// answers as `disabled`.
#[derive(Debug, Clone)]
pub struct DshRuntimeConfig {
    pub enabled: bool,
    pub daemon_url: String,
    pub attach_url: Option<String>,
}

impl DshRuntimeConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            daemon_url: "http://127.0.0.1:7331".to_string(),
            attach_url: None,
        }
    }
}

/// Resolves the ccteam enrollment bearer a managed tenant DSH web instance
/// authenticates its ccteam tool surface with, from `(ccteam_home, owner_tag)`.
///
/// Injected rather than called directly: `ccteam-core::enroll` sits ABOVE this
/// crate in the dependency graph (core depends on harness), so the assembling
/// layer supplies the resolver.
pub type DshEnrollmentResolver = Arc<dyn Fn(&Path, &str) -> Result<String> + Send + Sync>;

/// Lifecycle of one identity's DSH web runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DshRuntimeState {
    /// No companion listener / unconfigured manager — nothing can run.
    Disabled,
    Stopped,
    Starting,
    Running,
    /// Attached to a DSH web instance ccteam did not spawn (operator's own).
    Attached,
}

/// The authenticated identity a runtime belongs to, de-webbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshRuntimeIdentity {
    /// Ledger owner tag — the per-identity instance key.
    pub owner_tag: String,
    /// Identity id, used for the managed tenant home segment.
    pub id: String,
    /// Operators use their own `~/.dsh` (attach-if-detected); tenants get a
    /// ccteam-managed home under `<ccteam_home>/runtime/dsh/web/<user>/`.
    pub operator: bool,
}

/// Snapshot of one identity's runtime. Response shaping (REST/JSON) belongs to
/// the caller.
#[derive(Debug, Clone)]
pub struct DshRuntimeStatus {
    pub state: DshRuntimeState,
    pub port: Option<u16>,
    pub dsh_version: Option<String>,
    pub error_tail: Option<String>,
    /// Loopback URL of an operator's own instance, when there is one.
    pub native_url: Option<String>,
}

impl DshRuntimeStatus {
    fn disabled() -> Self {
        Self {
            state: DshRuntimeState::Disabled,
            port: None,
            dsh_version: None,
            error_tail: None,
            native_url: None,
        }
    }

    fn stopped() -> Self {
        Self {
            state: DshRuntimeState::Stopped,
            port: None,
            dsh_version: None,
            error_tail: None,
            native_url: None,
        }
    }
}

#[derive(Debug)]
struct DshInstance {
    child: Option<Child>,
    port: Option<u16>,
    _home: PathBuf,
    _started_at: DateTime<Utc>,
    last_activity: DateTime<Utc>,
    /// Retained for diagnostics: operator-attached vs ccteam-managed tenant.
    _kind: DshInstanceKind,
    state: DshRuntimeState,
    error_tail: ErrorTail,
    dsh_version: Option<String>,
    native_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DshInstanceKind {
    Operator,
    Tenant,
}

enum StartClaim {
    Wait {
        rx: watch::Receiver<bool>,
    },
    Spawn {
        rx: watch::Receiver<bool>,
        tx: watch::Sender<bool>,
    },
}

impl StartClaim {
    fn into_rx(self) -> watch::Receiver<bool> {
        match self {
            Self::Wait { rx } | Self::Spawn { rx, .. } => rx,
        }
    }
}

/// Marks the inflight start as done on drop (success, error, or panic).
struct StartDone(watch::Sender<bool>);

impl Drop for StartDone {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

type ErrorTail = Arc<Mutex<VecDeque<String>>>;

/// The single owner of this daemon's `dsh web` child processes.
///
/// Cheap to clone (shared inner state), so the composition root can build ONE
/// and hand it to every consumer.
#[derive(Clone)]
pub struct DshRuntimeManager {
    inner: Arc<Inner>,
}

struct Inner {
    ccteam_home: PathBuf,
    enrollment: DshEnrollmentResolver,
    /// Set once by `configure`, after the daemon knows its own ports.
    config: OnceLock<DshRuntimeConfig>,
    instances: Mutex<HashMap<String, DshInstance>>,
    /// In-flight start waiters, keyed like `instances`. A `Starting` row
    /// without an entry here is an orphan (the task that called `start` was
    /// cancelled and took the child with it).
    inflight: Mutex<HashMap<String, watch::Receiver<bool>>>,
    client: reqwest::Client,
}

impl std::fmt::Debug for DshRuntimeManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DshRuntimeManager")
            .field("ccteam_home", &self.inner.ccteam_home)
            .field("config", &self.inner.config.get())
            .finish_non_exhaustive()
    }
}

impl DshRuntimeManager {
    /// Unconfigured manager: answers `disabled` and spawns nothing until
    /// [`configure`](Self::configure) runs. Two-phase on purpose — the daemon
    /// builds ONE manager in its composition root, before any port is bound.
    pub fn new(ccteam_home: PathBuf, enrollment: DshEnrollmentResolver) -> Self {
        Self {
            inner: Arc::new(Inner {
                ccteam_home,
                enrollment,
                config: OnceLock::new(),
                instances: Mutex::new(HashMap::new()),
                inflight: Mutex::new(HashMap::new()),
                client: reqwest::Client::new(),
            }),
        }
    }

    /// `new` + [`configure`](Self::configure) in one step, for callers that
    /// already know the wiring (standalone serve paths, tests).
    pub fn configured(
        ccteam_home: PathBuf,
        enrollment: DshEnrollmentResolver,
        config: DshRuntimeConfig,
    ) -> Self {
        let manager = Self::new(ccteam_home, enrollment);
        manager.configure(config);
        manager
    }

    /// Install the runtime wiring. First call wins; later calls are ignored so
    /// a second consumer cannot re-point a live runtime.
    pub fn configure(&self, config: DshRuntimeConfig) {
        let _ = self.inner.config.set(config);
    }

    /// `false` until `configure` ran with `enabled: true`.
    pub fn enabled(&self) -> bool {
        self.inner.enabled()
    }

    /// The DSH home this identity's runtime uses.
    pub fn home_for(&self, identity: &DshRuntimeIdentity) -> Result<PathBuf> {
        self.inner.home_for(identity)
    }

    pub async fn status(&self, identity: &DshRuntimeIdentity) -> DshRuntimeStatus {
        self.inner.status(identity).await
    }

    /// Idempotently start (or attach to) this identity's runtime and report the
    /// resulting status. Concurrent callers share one start.
    pub async fn start(&self, identity: &DshRuntimeIdentity) -> DshRuntimeStatus {
        Arc::clone(&self.inner).start(identity).await
    }

    /// Stop and forget this identity's runtime. Attached (operator-owned)
    /// instances are detached, never killed.
    pub async fn stop(&self, identity: &DshRuntimeIdentity) -> DshRuntimeStatus {
        self.inner.stop(identity).await
    }

    /// Loopback port of this identity's serving runtime, starting it first.
    pub async fn port_for(&self, identity: &DshRuntimeIdentity) -> Result<u16> {
        Arc::clone(&self.inner).port_for(identity).await
    }

    /// Terminate every instance this manager owns (daemon shutdown).
    pub async fn shutdown_all(&self) {
        self.inner.shutdown_all().await;
    }
}

impl Inner {
    fn config(&self) -> Option<&DshRuntimeConfig> {
        self.config.get()
    }

    fn enabled(&self) -> bool {
        self.config().is_some_and(|config| config.enabled)
    }

    fn home_for(&self, identity: &DshRuntimeIdentity) -> Result<PathBuf> {
        if identity.operator {
            let home = dirs::home_dir().ok_or_else(|| anyhow!("HOME is unknown"))?;
            Ok(home.join(".dsh"))
        } else {
            Ok(self
                .ccteam_home
                .join("runtime")
                .join("dsh")
                .join("web")
                .join(tenant_home_segment(&identity.id)))
        }
    }

    async fn status(&self, identity: &DshRuntimeIdentity) -> DshRuntimeStatus {
        if !self.enabled() {
            return DshRuntimeStatus::disabled();
        }

        let key = identity.owner_tag.clone();
        let snapshot = {
            let mut instances = self.instances.lock().await;
            if let Some(instance) = instances.get_mut(&key) {
                if instance.state == DshRuntimeState::Starting {
                    let inflight = self.inflight.lock().await;
                    if !inflight.contains_key(&key) {
                        // Orphan: a cancelled request dropped the spawn future
                        // (and `kill_on_drop` the child) after inserting
                        // Starting. Tell the truth so the UI offers Start
                        // instead of spinning forever.
                        instance.state = DshRuntimeState::Stopped;
                        instance.port = None;
                    }
                }
            }
            instances.get(&key).map(|instance| {
                (
                    instance.state,
                    instance.port,
                    instance.error_tail.clone(),
                    instance.dsh_version.clone(),
                    instance.native_url.clone(),
                )
            })
        };
        let Some((state, port, tail, dsh_version, native_url)) = snapshot else {
            return DshRuntimeStatus::stopped();
        };
        let error_tail = read_error_tail(&tail).await;
        DshRuntimeStatus {
            state,
            port,
            dsh_version,
            error_tail,
            native_url,
        }
    }

    async fn start(self: Arc<Self>, identity: &DshRuntimeIdentity) -> DshRuntimeStatus {
        if !self.enabled() {
            return self.status(identity).await;
        }
        let key = identity.owner_tag.clone();

        // Serving, or another start is actually running: do not spawn a
        // second child. `Starting` WITHOUT an inflight waiter is an orphan
        // (cancelled caller task) and must be retried — treating it as live
        // is what left the rob tenant spinning on "Starting the DSH web
        // instance…" with no process behind it.
        //
        // `instances` (the MutexGuard) drops at the end of `live_or_inflight`
        // — before any `.await` below tries to lock the same
        // `tokio::sync::Mutex` again. Awaiting `status` while still holding
        // the guard (the original deadlock) self-deadlocks: the task waits on
        // a lock only it holds, and every later caller of
        // `self.instances.lock()` — including every proxied request and every
        // future status poll — then hangs forever too.
        if let Some(rx) = self.live_or_inflight(&key).await {
            if let Some(mut rx) = rx {
                let _ = rx.wait_for(|done| *done).await;
            }
            return self.status(identity).await;
        }

        let kind = if identity.operator {
            DshInstanceKind::Operator
        } else {
            DshInstanceKind::Tenant
        };
        let home = match self.home_for(identity) {
            Ok(path) => path,
            Err(err) => {
                self.record_stopped_error(&key, kind, PathBuf::new(), err.to_string())
                    .await;
                return self.status(identity).await;
            }
        };

        let Some(claimed) = self.claim_start(&key, kind, home.clone()).await else {
            return self.status(identity).await;
        };

        if let StartClaim::Spawn { tx, .. } = &claimed {
            let tx = tx.clone();
            // The spawn itself lives on a detached task so cancelling the
            // caller (browser abort, iframe timeout, companion-port retry)
            // cannot drop `spawn_until_ready`'s `kill_on_drop` Child and
            // leave the map stuck at Starting. This waiter is the only thing
            // the caller owns; dropping it just stops waiting.
            let runtime = Arc::clone(&self);
            let identity = identity.clone();
            let start_key = key.clone();
            tokio::spawn(async move {
                let _done = StartDone(tx);
                let tail = {
                    let instances = runtime.instances.lock().await;
                    instances
                        .get(&start_key)
                        .map(|i| i.error_tail.clone())
                        .unwrap_or_else(new_error_tail)
                };
                let start_result = if identity.operator {
                    runtime
                        .start_operator(&identity, home.clone(), tail.clone())
                        .await
                } else {
                    runtime
                        .start_tenant(&identity, home.clone(), tail.clone())
                        .await
                };
                runtime
                    .finish_start(&start_key, kind, home, tail, start_result)
                    .await;
            });
        }

        let mut rx = claimed.into_rx();
        let _ = rx.wait_for(|done| *done).await;
        self.status(identity).await
    }

    /// `None` = caller should start. `Some(None)` = already serving.
    /// `Some(Some(rx))` = wait for the in-flight start.
    async fn live_or_inflight(&self, key: &str) -> Option<Option<watch::Receiver<bool>>> {
        let instances = self.instances.lock().await;
        match instances.get(key).map(|i| &i.state) {
            Some(DshRuntimeState::Running | DshRuntimeState::Attached) => Some(None),
            Some(DshRuntimeState::Starting) => {
                let inflight = self.inflight.lock().await;
                inflight.get(key).cloned().map(Some)
            }
            _ => None,
        }
    }

    /// Insert the Starting row + inflight waiter. Returns `None` if the
    /// instance is already serving; otherwise a claim the caller either
    /// waits on (another start is running) or uses to spawn the work.
    async fn claim_start(
        &self,
        key: &str,
        kind: DshInstanceKind,
        home: PathBuf,
    ) -> Option<StartClaim> {
        let mut instances = self.instances.lock().await;
        match instances.get(key).map(|i| &i.state) {
            Some(DshRuntimeState::Running | DshRuntimeState::Attached) => return None,
            Some(DshRuntimeState::Starting) => {
                let inflight = self.inflight.lock().await;
                if let Some(rx) = inflight.get(key) {
                    return Some(StartClaim::Wait { rx: rx.clone() });
                }
            }
            _ => {}
        }
        let (tx, rx) = watch::channel(false);
        instances.insert(
            key.to_string(),
            DshInstance {
                child: None,
                port: None,
                _home: home,
                _started_at: Utc::now(),
                last_activity: Utc::now(),
                _kind: kind,
                state: DshRuntimeState::Starting,
                error_tail: new_error_tail(),
                dsh_version: None,
                native_url: None,
            },
        );
        drop(instances);
        let mut inflight = self.inflight.lock().await;
        inflight.insert(key.to_string(), rx.clone());
        Some(StartClaim::Spawn { rx, tx })
    }

    async fn finish_start(
        &self,
        key: &str,
        kind: DshInstanceKind,
        home: PathBuf,
        tail: ErrorTail,
        start_result: Result<DshInstance>,
    ) {
        match start_result {
            Ok(mut instance) => {
                instance.error_tail = tail;
                let leftover = {
                    let mut instances = self.instances.lock().await;
                    if matches!(
                        instances.get(key).map(|i| &i.state),
                        Some(DshRuntimeState::Starting)
                    ) {
                        instances.insert(key.to_string(), instance);
                        None
                    } else {
                        Some(instance)
                    }
                };
                if let Some(instance) = leftover {
                    terminate_instance(instance).await;
                }
            }
            Err(err) => {
                let still_starting = {
                    let instances = self.instances.lock().await;
                    matches!(
                        instances.get(key).map(|i| &i.state),
                        Some(DshRuntimeState::Starting)
                    )
                };
                if still_starting {
                    self.record_stopped_error(key, kind, home, err.to_string())
                        .await;
                }
            }
        }
        self.inflight.lock().await.remove(key);
    }

    async fn stop(&self, identity: &DshRuntimeIdentity) -> DshRuntimeStatus {
        if !self.enabled() {
            return self.status(identity).await;
        }
        let instance = {
            let mut instances = self.instances.lock().await;
            instances.remove(&identity.owner_tag)
        };
        if let Some(instance) = instance {
            terminate_instance(instance).await;
        }
        self.status(identity).await
    }

    async fn port_for(self: Arc<Self>, identity: &DshRuntimeIdentity) -> Result<u16> {
        if !self.enabled() {
            return Err(anyhow!("DSH web runtime is disabled"));
        }
        let key = identity.owner_tag.clone();
        Arc::clone(&self).start(identity).await;
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(&key)
            .ok_or_else(|| anyhow!("DSH web instance is stopped"))?;
        instance.last_activity = Utc::now();
        instance
            .port
            .ok_or_else(|| anyhow!("DSH web instance is starting"))
    }

    async fn shutdown_all(&self) {
        let instances = {
            let mut locked = self.instances.lock().await;
            std::mem::take(&mut *locked)
        };
        for (_, instance) in instances {
            terminate_instance(instance).await;
        }
    }

    async fn record_stopped_error(
        &self,
        key: &str,
        kind: DshInstanceKind,
        home: PathBuf,
        error: String,
    ) {
        let tail = new_error_tail();
        push_tail(&tail, error).await;
        let mut instances = self.instances.lock().await;
        instances.insert(
            key.to_string(),
            DshInstance {
                child: None,
                port: None,
                _home: home,
                _started_at: Utc::now(),
                last_activity: Utc::now(),
                _kind: kind,
                state: DshRuntimeState::Stopped,
                error_tail: tail,
                dsh_version: None,
                native_url: None,
            },
        );
    }

    async fn start_operator(
        &self,
        identity: &DshRuntimeIdentity,
        home: PathBuf,
        tail: ErrorTail,
    ) -> Result<DshInstance> {
        let attach_url = self
            .config()
            .and_then(|config| config.attach_url.clone())
            .or_else(|| std::env::var(ATTACH_URL_ENV).ok())
            .unwrap_or_else(|| DEFAULT_ATTACH_URL.to_string());
        if self.probe_attached_dsh(&attach_url).await {
            let port = port_from_url(&attach_url).unwrap_or(3080);
            return Ok(DshInstance {
                child: None,
                port: Some(port),
                _home: home,
                _started_at: Utc::now(),
                last_activity: Utc::now(),
                _kind: DshInstanceKind::Operator,
                state: DshRuntimeState::Attached,
                error_tail: tail,
                dsh_version: None,
                native_url: Some(normalize_url(&attach_url)),
            });
        }

        let spawn_home = home.clone();
        let spawn = build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: &identity.owner_tag,
            ccteam_home: self.ccteam_home.clone(),
            dsh_home: spawn_home,
            profile: DSH_NATIVE_WEB_PROFILE,
            materialize_profile: false,
            enrollment: None,
            daemon_url: None,
        })
        .map_err(|e| anyhow!("{e}"))?;
        let (child, port) = spawn_until_ready(spawn, tail.clone(), &self.client).await?;
        Ok(DshInstance {
            child: Some(child),
            port: Some(port),
            _home: home,
            _started_at: Utc::now(),
            last_activity: Utc::now(),
            _kind: DshInstanceKind::Operator,
            state: DshRuntimeState::Running,
            error_tail: tail,
            dsh_version: None,
            native_url: Some(format!("http://127.0.0.1:{port}/")),
        })
    }

    async fn start_tenant(
        &self,
        identity: &DshRuntimeIdentity,
        home: PathBuf,
        tail: ErrorTail,
    ) -> Result<DshInstance> {
        let owner = &identity.owner_tag;
        let config = self
            .config()
            .ok_or_else(|| anyhow!("DSH web runtime is not configured"))?;
        let bearer = (self.enrollment)(&self.ccteam_home, owner)
            .with_context(|| format!("ensure enrollment credential for {owner}"))?;
        let spawn = build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: owner,
            ccteam_home: self.ccteam_home.clone(),
            dsh_home: home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some(&bearer),
            daemon_url: Some(&config.daemon_url),
        })
        .map_err(|e| anyhow!("{e}"))?;
        let (child, port) = spawn_until_ready(spawn, tail.clone(), &self.client).await?;
        Ok(DshInstance {
            child: Some(child),
            port: Some(port),
            _home: home,
            _started_at: Utc::now(),
            last_activity: Utc::now(),
            _kind: DshInstanceKind::Tenant,
            state: DshRuntimeState::Running,
            error_tail: tail,
            dsh_version: None,
            native_url: None,
        })
    }

    async fn probe_attached_dsh(&self, attach_url: &str) -> bool {
        let url = normalize_url(attach_url);
        let Ok(resp) = self.client.get(&url).timeout(HEALTH_TIMEOUT).send().await else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }
        if resp.headers().contains_key("x-dsh-web") {
            return true;
        }
        resp.text()
            .await
            .map(|body| {
                let lower = body.to_ascii_lowercase();
                lower.contains("dsh") || lower.contains("deepseek")
            })
            .unwrap_or(false)
    }
}

/// Reap DSH processes stranded by daemon versions that predate PDEATHSIG.
///
/// `/proc` is intentionally the authority here: only an init-parented process
/// whose own `DSH_HOME` points inside this ccteam installation's managed DSH
/// runtime can match. Failures are ignored because this startup cleanup is a
/// best-effort compatibility sweep, never a reason to keep the daemon down.
#[cfg(target_os = "linux")]
pub async fn sweep_legacy_dsh_orphans(ccteam_home: &Path) {
    let victims = legacy_dsh_orphans(ccteam_home);
    if victims.is_empty() {
        return;
    }

    for victim in &victims {
        // SAFETY: kill is an async-signal-safe syscall. The predicate already
        // restricted the target to an init-parented ccteam-managed DSH home.
        let sent = unsafe { libc::kill(victim.pid, libc::SIGTERM) } == 0;
        if sent {
            tracing::info!(pid = victim.pid, "terminating legacy orphaned DSH process");
        }
    }

    tokio::time::sleep(ORPHAN_STOP_GRACE).await;
    for victim in victims {
        // Re-read both the immutable process start time and the predicate
        // inputs before escalation. This avoids signaling an unrelated process
        // if Linux reused the pid during the grace window.
        let Some(current) = legacy_dsh_process(victim.pid) else {
            continue;
        };
        if current.start_time != victim.start_time
            || !is_ccteam_managed_dsh_orphan(&current.dsh_home, current.ppid, ccteam_home)
        {
            continue;
        }
        // SAFETY: same constrained target as above, revalidated after grace.
        if unsafe { libc::kill(victim.pid, libc::SIGKILL) } == 0 {
            tracing::warn!(
                pid = victim.pid,
                "killed unresponsive legacy orphaned DSH process"
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn sweep_legacy_dsh_orphans(_ccteam_home: &Path) {
    // macOS has neither /proc nor PDEATHSIG; retain the existing graceful
    // kill-on-drop behavior there.
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LegacyDshProcess {
    pid: i32,
    ppid: u32,
    start_time: u64,
    dsh_home: PathBuf,
}

#[cfg(target_os = "linux")]
fn legacy_dsh_orphans(ccteam_home: &Path) -> Vec<LegacyDshProcess> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(legacy_dsh_process)
        .filter(|process| {
            is_ccteam_managed_dsh_orphan(&process.dsh_home, process.ppid, ccteam_home)
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn legacy_dsh_process(pid: i32) -> Option<LegacyDshProcess> {
    use std::os::unix::ffi::OsStringExt;

    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` may contain spaces and parentheses, so split after its final ')'.
    // The remaining fields begin at state (field 3): ppid is index 1 and
    // starttime (field 22) is index 19.
    let (_, fields) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = fields.split_whitespace().collect();
    let ppid = fields.get(1)?.parse().ok()?;
    let start_time = fields.get(19)?.parse().ok()?;

    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let value = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"DSH_HOME="))?;
    if value.is_empty() {
        return None;
    }
    Some(LegacyDshProcess {
        pid,
        ppid,
        start_time,
        dsh_home: PathBuf::from(std::ffi::OsString::from_vec(value.to_vec())),
    })
}

async fn spawn_until_ready(
    spawn: DshSpawnSpec,
    tail: ErrorTail,
    client: &reqwest::Client,
) -> Result<(Child, u16)> {
    let mut command = Command::new(&spawn.bin);
    command
        .args(&spawn.args)
        .current_dir(&spawn.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for key in &spawn.env_remove {
        command.env_remove(key);
    }
    for (key, value) in &spawn.env {
        command.env(key, value);
    }
    // `kill_on_drop` cannot run when the daemon itself is SIGKILLed. Bind the
    // DSH web child to the spawning thread in the Linux kernel; macOS has no
    // PDEATHSIG and keeps the existing graceful-teardown behavior.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getpid is an argument-free syscall.
        let expected_parent = unsafe { libc::getpid() };
        // SAFETY: only async-signal-safe libc calls run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != expected_parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn DSH web `{}` {:?}", spawn.bin, spawn.args))?;
    let stdout = child.stdout.take().context("DSH web stdout unavailable")?;
    if let Some(stderr) = child.stderr.take() {
        spawn_tail_reader(stderr, tail.clone());
    }
    let mut lines = BufReader::new(stdout).lines();
    let port = tokio::time::timeout(READINESS_TIMEOUT, async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let line = line.context("read DSH web stdout")?;
                    let Some(line) = line else {
                        return Err(anyhow!("DSH web exited before readiness"));
                    };
                    if let Some(port) = parse_readiness_port(&line) {
                        return Ok(port);
                    }
                }
                status = child.wait() => {
                    return Err(anyhow!("DSH web exited before readiness: {}", status?));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        anyhow!(
            "DSH web did not print readiness within {:?}",
            READINESS_TIMEOUT
        )
    })??;
    health_probe(client, port).await?;
    Ok((child, port))
}

async fn health_probe(client: &reqwest::Client, port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/");
    let resp = client
        .get(url)
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .context("probe DSH web readiness")?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(anyhow!("DSH web health probe returned {}", resp.status()))
    }
}

fn spawn_tail_reader(stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static, tail: ErrorTail) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            push_tail(&tail, line).await;
        }
    });
}

fn parse_readiness_port(line: &str) -> Option<u16> {
    let start = line.find(READINESS_PREFIX)? + READINESS_PREFIX.len();
    let digits: String = line[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

async fn terminate_instance(mut instance: DshInstance) {
    let Some(mut child) = instance.child.take() else {
        return;
    };
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    #[cfg(not(unix))]
    {
        let _ = child.start_kill();
    }
    match tokio::time::timeout(STOP_TIMEOUT, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            let _ = child.kill().await;
        }
    }
}

fn new_error_tail() -> ErrorTail {
    Arc::new(Mutex::new(VecDeque::with_capacity(ERROR_TAIL_LINES)))
}

async fn push_tail(tail: &ErrorTail, line: String) {
    let mut tail = tail.lock().await;
    if tail.len() == ERROR_TAIL_LINES {
        tail.pop_front();
    }
    tail.push_back(line);
}

async fn read_error_tail(tail: &ErrorTail) -> Option<String> {
    let tail = tail.lock().await;
    if tail.is_empty() {
        None
    } else {
        Some(tail.iter().cloned().collect::<Vec<_>>().join("\n"))
    }
}

fn normalize_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    format!("{trimmed}/")
}

fn port_from_url(url: &str) -> Option<u16> {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    authority.rsplit_once(':')?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager(config: DshRuntimeConfig) -> DshRuntimeManager {
        DshRuntimeManager::configured(
            PathBuf::from("/nonexistent/ccteam-home"),
            Arc::new(|_root, owner| Ok(format!("ccteam-enroll:test:{owner}"))),
            config,
        )
    }

    #[test]
    fn readiness_line_parser_extracts_port() {
        assert_eq!(
            parse_readiness_port("noise dsh web: http://127.0.0.1:35479"),
            Some(35479)
        );
        assert_eq!(
            parse_readiness_port("dsh web: http://localhost:35479"),
            None
        );
    }

    #[test]
    fn tenant_home_segment_keeps_safe_ids_and_hashes_unsafe_ids() {
        assert_eq!(tenant_home_segment("alice-1"), "alice-1");
        assert!(tenant_home_segment("bad/id").starts_with("tenant-"));
    }

    /// The tenant home layout is a contract with the operator (backups, resets)
    /// and with the orphan predicate: `<ccteam_home>/runtime/dsh/web/<user>/`.
    #[test]
    fn tenant_home_lives_under_the_managed_dsh_web_root() {
        let manager = test_manager(DshRuntimeConfig::disabled());
        let home = manager
            .home_for(&DshRuntimeIdentity {
                owner_tag: "user:alice".to_string(),
                id: "alice".to_string(),
                operator: false,
            })
            .expect("tenant home resolves");
        assert!(home.ends_with("runtime/dsh/web/alice"), "got {home:?}");
    }

    #[tokio::test]
    async fn unconfigured_manager_reports_disabled_and_starts_nothing() {
        let manager = DshRuntimeManager::new(
            PathBuf::from("/nonexistent/ccteam-home"),
            Arc::new(|_root, _owner| Ok(String::new())),
        );
        let identity = DshRuntimeIdentity {
            owner_tag: "user:web-api".to_string(),
            id: "admin".to_string(),
            operator: true,
        };
        assert!(!manager.enabled());
        assert_eq!(
            manager.start(&identity).await.state,
            DshRuntimeState::Disabled,
            "a manager the daemon has not configured yet must never spawn"
        );
        assert_eq!(
            manager.status(&identity).await.state,
            DshRuntimeState::Disabled
        );
        assert!(manager.port_for(&identity).await.is_err());
    }

    #[tokio::test]
    async fn orphaned_starting_status_heals_to_stopped() {
        let manager = test_manager(DshRuntimeConfig {
            enabled: true,
            daemon_url: "http://127.0.0.1:7331".to_string(),
            attach_url: None,
        });
        let identity = DshRuntimeIdentity {
            owner_tag: "user:web-api".to_string(),
            id: "admin".to_string(),
            operator: true,
        };
        {
            let mut instances = manager.inner.instances.lock().await;
            instances.insert(
                identity.owner_tag.clone(),
                DshInstance {
                    child: None,
                    port: None,
                    _home: PathBuf::new(),
                    _started_at: Utc::now(),
                    last_activity: Utc::now(),
                    _kind: DshInstanceKind::Operator,
                    state: DshRuntimeState::Starting,
                    error_tail: new_error_tail(),
                    dsh_version: None,
                    native_url: None,
                },
            );
        }
        let status = manager.status(&identity).await;
        assert_eq!(
            status.state,
            DshRuntimeState::Stopped,
            "Starting with no inflight task is an orphan, not a live boot"
        );
    }
}
