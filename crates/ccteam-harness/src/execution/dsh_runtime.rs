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

use crate::execution::dsh_acp::spawn_spec::{
    dsh_home_for_identity, tenant_id_from_owner_tag, DshSpawnSpec,
};
use crate::execution::dsh_acp::{
    build_web_spawn_spec, socket_path_for_identity, DshWebSpawnOptions, DSH_NATIVE_WEB_PROFILE,
    DSH_WEB_PROFILE,
};

const DEFAULT_ATTACH_URL: &str = "http://127.0.0.1:3080";
const ATTACH_URL_ENV: &str = "CCTEAM_DSH_WEB_ATTACH_URL";
/// Everything `dsh web` prints after this prefix is ONE whitespace-delimited
/// URL, taken verbatim — see [`parse_readiness`]. The port used to be scraped
/// out of a longer hardcoded prefix, which is why 0.1.5 adding `?token=` to
/// that same line broke startup: the port survived and the credential beside
/// it was thrown away.
const READINESS_PREFIX: &str = "dsh web: ";
/// What ccteam tells an operator whose OWN `dsh web` challenges it.
///
/// That instance's browser credential exists only in the terminal that printed
/// its URL, so there is nothing for ccteam to hold and nothing to forge. Both
/// ways out are the operator's, and attaching anyway is still right: a second
/// process over the same DSH home and ACP socket is the worse outcome.
const ATTACHED_AUTH_REQUIRED: &str = "This `dsh web` was started outside ccteam and asks for its own \
     browser credential, which exists only in the terminal that printed its URL. ccteam attached to \
     it instead of starting a second process over the same DSH home, but cannot authenticate to it. \
     Open that printed URL directly, or stop that instance and press Start so ccteam manages one.";
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

/// Resolves the identity's own ccteam REST bearer (`ccteam:<hex>`) — the one
/// the team panel calls `/api/v1` with — from `(ccteam_home, owner_tag)`.
///
/// Injected for the same reason as [`DshEnrollmentResolver`]: the token stores
/// (`ccteam-core::tenants`, `ccteam-web::token`) sit ABOVE this crate. Reuse
/// before mint is the resolver's job, not this manager's.
pub type DshRestTokenResolver = Arc<dyn Fn(&Path, &str) -> Result<String> + Send + Sync>;

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

/// Owner tag every operator-ish identity collapses to.
///
/// The web console's admin already arrives as `user:web-api`; IM chats
/// (`telegram:123`, …) and a blank owner are the same human at the same
/// `~/.dsh`. Mapping them to ONE key is what keeps two `dsh web` processes from
/// writing that home — and from fighting over one ACP socket.
const OPERATOR_OWNER_TAG: &str = "user:web-api";

impl DshRuntimeIdentity {
    /// The identity a ledger owner tag belongs to, matching
    /// [`crate::execution::dsh_acp::identity_dsh_home`]'s lineage exactly: a
    /// `user:<id>` tenant gets its own managed runtime, everyone else shares the
    /// operator's.
    pub fn for_owner_tag(owner_tag: &str) -> Self {
        match tenant_id_from_owner_tag(owner_tag) {
            Some(id) => Self {
                owner_tag: owner_tag.to_string(),
                id: id.to_string(),
                operator: false,
            },
            None => Self {
                owner_tag: OPERATOR_OWNER_TAG.to_string(),
                id: "web-api".to_string(),
                operator: true,
            },
        }
    }
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

/// Where an identity's `dsh web` listens, plus whatever ccteam must send to be
/// let in.
///
/// The credential is a ready-to-send `Cookie` header value and is OPAQUE to
/// ccteam: it is whatever the vendor set on the readiness exchange, kept
/// name=value and never parsed, assumed or logged. `None` is a first-class
/// answer — a DSH without browser auth, or an instance ccteam did not start.
#[derive(Debug, Clone)]
pub struct DshEndpoint {
    pub port: u16,
    pub credential: Option<String>,
}

/// What answered when ccteam probed a `dsh web` address.
///
/// The distinction the 0.1.5 breakage turned on: **liveness is not
/// authorization**. Since that release every unauthenticated request is
/// answered 401, so a status code says nothing about whether the server
/// started — only a transport error does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebProbe {
    /// Nothing answered: no listener at that address.
    Absent,
    /// A listener answered and served this request.
    Serving,
    /// A listener answered its browser-auth challenge: alive, and this request
    /// carried no credential it accepts.
    NeedsCredential,
}

impl WebProbe {
    fn alive(self) -> bool {
        !matches!(self, Self::Absent)
    }
}

/// Classify one HTTP answer. Only the transport layer can say `Absent`, which
/// is why that arm is not reachable from a status code.
fn classify_probe(status: reqwest::StatusCode) -> WebProbe {
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            WebProbe::NeedsCredential
        }
        _ => WebProbe::Serving,
    }
}

/// What one `dsh web` output line says about where the server is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DshReadiness {
    port: u16,
    /// Path + query of the printed URL, to be replayed ONCE against loopback.
    /// `None` when the line carried no query: an older `dsh web`, or a future
    /// one that drops browser auth, and then there is nothing to exchange.
    exchange: Option<String>,
}

#[derive(Debug)]
struct DshInstance {
    child: Option<Child>,
    port: Option<u16>,
    /// Opaque vendor credential for THIS instance; see [`DshEndpoint`].
    credential: Option<String>,
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
    rest_token: DshRestTokenResolver,
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
    pub fn new(
        ccteam_home: PathBuf,
        enrollment: DshEnrollmentResolver,
        rest_token: DshRestTokenResolver,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                ccteam_home,
                enrollment,
                rest_token,
                config: OnceLock::new(),
                instances: Mutex::new(HashMap::new()),
                inflight: Mutex::new(HashMap::new()),
                client: probe_client(),
            }),
        }
    }

    /// `new` + [`configure`](Self::configure) in one step, for callers that
    /// already know the wiring (standalone serve paths, tests).
    pub fn configured(
        ccteam_home: PathBuf,
        enrollment: DshEnrollmentResolver,
        rest_token: DshRestTokenResolver,
        config: DshRuntimeConfig,
    ) -> Self {
        let manager = Self::new(ccteam_home, enrollment, rest_token);
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

    /// Loopback endpoint of this identity's serving runtime, starting it first.
    ///
    /// The port alone is not enough to reach a `dsh web` since 0.1.5: the
    /// credential minted at startup belongs to the same answer, so callers
    /// cannot forget to ask for it.
    pub async fn endpoint_for(&self, identity: &DshRuntimeIdentity) -> Result<DshEndpoint> {
        Arc::clone(&self.inner).endpoint_for(identity).await
    }

    /// Terminate every instance this manager owns (daemon shutdown).
    pub async fn shutdown_all(&self) {
        self.inner.shutdown_all().await;
    }

    /// Hosts-page "register the ccteam DSH plugin" action (gate ①): merge
    /// ccteam's OWN bundle entry and patch row — `ccteam-ui` carrying this
    /// daemon's URL, `transportSocket` and the operator's REST token — into
    /// the operator's real `~/.dsh` web profile, WITHOUT starting or
    /// attaching a runtime. This is how an operator whose hand-started `dsh
    /// web` lacks the plugin gets it: register here, then restart that
    /// instance themselves. Idempotent and merge-only; touches only ccteam's
    /// own row. That row carries the operator's own REST token (owner
    /// decision 2026-08-28); enrollment stays the human's to paste.
    pub async fn register_operator_profile(&self) -> Result<PathBuf> {
        let daemon_url = self
            .inner
            .config()
            .map(|config| config.daemon_url.clone())
            .ok_or_else(|| anyhow!("DSH web runtime is not configured"))?;
        let identity = DshRuntimeIdentity::for_owner_tag(OPERATOR_OWNER_TAG);
        let home = self.inner.home_for(&identity)?;
        let socket = self.inner.socket_for(&identity);
        let ccteam_home = self.inner.ccteam_home.clone();
        let rest_token = self.inner.rest_token_for(OPERATOR_OWNER_TAG);
        tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            crate::execution::dsh_acp::spawn_spec::ensure_socket_dir(&socket)
                .map_err(|e| anyhow!("{e}"))?;
            let materialized =
                crate::execution::dsh_acp::materialize::register_ccteam_plugins_into_profile(
                    &ccteam_home,
                    &home,
                    DSH_NATIVE_WEB_PROFILE,
                    crate::execution::dsh_acp::materialize::DshPluginConfig {
                        daemon_url: Some(&daemon_url),
                        enrollment: None,
                        transport_socket: Some(&socket.to_string_lossy()),
                        rest_token: rest_token.as_deref(),
                    },
                )
                .map_err(|e| anyhow!("{e}"))?;
            Ok(materialized.profile_dir)
        })
        .await
        .context("join DSH plugin registration task")?
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
        // One resolver for both key shapes: the adapter asks by owner tag, the
        // manager by `operator` + id, and they must never disagree.
        dsh_home_for_identity(identity.operator, &identity.id, &self.ccteam_home)
            .map_err(|e| anyhow!("{e}"))
    }

    fn socket_for(&self, identity: &DshRuntimeIdentity) -> PathBuf {
        socket_path_for_identity(identity.operator, &identity.id, &self.ccteam_home)
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
                credential: None,
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

    async fn endpoint_for(self: Arc<Self>, identity: &DshRuntimeIdentity) -> Result<DshEndpoint> {
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
        let port = instance
            .port
            .ok_or_else(|| anyhow!("DSH web instance is starting"))?;
        Ok(DshEndpoint {
            port,
            credential: instance.credential.clone(),
        })
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
                credential: None,
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
        // ANYTHING that answers HTTP there is present. Requiring a 2xx here is
        // what made an operator's own 0.1.5 instance read as absent, and ccteam
        // then started a SECOND `dsh web` over the same home and ACP socket.
        let probe = self.probe_attached_dsh(&attach_url).await;
        if probe.alive() {
            let port = port_from_url(&attach_url).unwrap_or(3080);
            if probe == WebProbe::NeedsCredential {
                // ccteam holds no credential for a process it did not start,
                // and will not invent one; say so instead of proxying a bare
                // 401 the panel cannot explain.
                push_tail(&tail, ATTACHED_AUTH_REQUIRED.to_string()).await;
            }
            return Ok(DshInstance {
                child: None,
                port: Some(port),
                credential: None,
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

        // ccteam is about to start this instance in the operator's own home, so
        // it may register its own plugin row there (gate ①) — merge-only, and
        // only on this branch: an ATTACHED instance is the human's process, and
        // ccteam does not edit the home of a `dsh web` it did not start.
        let socket = self.socket_for(identity);
        // The panel's REST bearer travels with ccteam's own rows into the
        // operator's profile too (owner decision 2026-08-28: pasting a token
        // is for a hand-started `dsh web` only). It is the operator's own
        // admin web token, written into the operator's own home, 0600. A
        // resolver failure is not fatal: the panel just starts unconfigured.
        let rest_token = self.rest_token_for(&identity.owner_tag);
        let spawn = build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: &identity.owner_tag,
            ccteam_home: self.ccteam_home.clone(),
            dsh_home: home.clone(),
            profile: DSH_NATIVE_WEB_PROFILE,
            materialize_profile: false,
            // Enrollment stays the human's to paste: it is a session-level
            // MCP principal for hand-driven DSH agents, not the panel's token.
            enrollment: None,
            daemon_url: self.config().map(|config| config.daemon_url.as_str()),
            transport_socket: Some(&socket),
            rest_token: rest_token.as_deref(),
        })
        .map_err(|e| anyhow!("{e}"))?;
        let started = spawn_until_ready(spawn, tail.clone(), &self.client).await?;
        let port = started.port;
        Ok(DshInstance {
            child: Some(started.child),
            port: Some(port),
            credential: started.credential,
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

    /// This identity's own ccteam REST bearer for the panel row, or `None`
    /// when it cannot be resolved. A convenience, not a precondition: a
    /// runtime that cannot resolve one still serves DSH, with the panel
    /// asking for a token. Never logs the value.
    fn rest_token_for(&self, owner: &str) -> Option<String> {
        match (self.rest_token)(&self.ccteam_home, owner) {
            Ok(token) => Some(token),
            Err(error) => {
                tracing::warn!(
                    owner = %owner,
                    "no ccteam REST token for this identity; DSH team panel starts unconfigured: {error:#}"
                );
                None
            }
        }
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
        let rest_token = self.rest_token_for(owner);
        let socket = self.socket_for(identity);
        let spawn = build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: owner,
            ccteam_home: self.ccteam_home.clone(),
            dsh_home: home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some(&bearer),
            daemon_url: Some(&config.daemon_url),
            transport_socket: Some(&socket),
            rest_token: rest_token.as_deref(),
        })
        .map_err(|e| anyhow!("{e}"))?;
        let started = spawn_until_ready(spawn, tail.clone(), &self.client).await?;
        Ok(DshInstance {
            child: Some(started.child),
            port: Some(started.port),
            credential: started.credential,
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

    /// Is an operator's own `dsh web` already there — and will it talk to us?
    ///
    /// The "is this really dsh" sniff still decides `Absent`, because some
    /// other server on 3080 must not become ccteam's DSH panel. The status
    /// code only says whether a credential is missing: 0.1.5's own challenge
    /// body names dsh, so an authenticating instance still identifies itself.
    async fn probe_attached_dsh(&self, attach_url: &str) -> WebProbe {
        let url = normalize_url(attach_url);
        let Ok(resp) = self.client.get(&url).timeout(HEALTH_TIMEOUT).send().await else {
            return WebProbe::Absent;
        };
        let probe = classify_probe(resp.status());
        if resp.headers().contains_key("x-dsh-web") {
            return probe;
        }
        let identified = resp
            .text()
            .await
            .map(|body| {
                let lower = body.to_ascii_lowercase();
                lower.contains("dsh") || lower.contains("deepseek")
            })
            .unwrap_or(false);
        if identified {
            probe
        } else {
            WebProbe::Absent
        }
    }
}

/// Whether an OS process is an unusable orphan from ccteam's managed DSH
/// runtime. Kept pure so startup cleanup can be tested without signaling a
/// real process.
pub fn is_ccteam_managed_dsh_orphan(dsh_home: &Path, ppid: u32, ccteam_home: &Path) -> bool {
    if ppid != 1
        || dsh_home
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return false;
    }
    let managed_root = ccteam_home.join("runtime").join("dsh");
    dsh_home != managed_root && dsh_home.starts_with(managed_root)
}

/// Reap DSH processes stranded by daemon versions that predate PDEATHSIG, then
/// delete the per-session DSH homes those versions created.
///
/// `/proc` is intentionally the authority here: only an init-parented process
/// whose own `DSH_HOME` points inside this ccteam installation's managed DSH
/// runtime can match. Failures are ignored because this startup cleanup is a
/// best-effort compatibility sweep, never a reason to keep the daemon down.
#[cfg(target_os = "linux")]
pub async fn sweep_legacy_dsh_orphans(ccteam_home: &Path) {
    let victims = legacy_dsh_orphans(ccteam_home);
    remove_legacy_per_sid_homes(ccteam_home);
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
pub async fn sweep_legacy_dsh_orphans(ccteam_home: &Path) {
    // macOS has neither /proc nor PDEATHSIG; retain the existing graceful
    // kill-on-drop behavior there. The stale per-session homes are plain
    // directories, so those go either way.
    remove_legacy_per_sid_homes(ccteam_home);
}

/// Delete `<ccteam_home>/runtime/dsh/s<N>/` — the per-hire DSH homes ccteam
/// stopped creating in v0.10.3, when hires became connections to the identity's
/// one runtime. Only `s<digits>` names match, so the live layout
/// (`web/`, `client/`, `acp/`) and anything an operator parked there survive.
fn remove_legacy_per_sid_homes(ccteam_home: &Path) {
    let Ok(entries) = std::fs::read_dir(ccteam_home.join("runtime").join("dsh")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_sid = name
            .strip_prefix('s')
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()));
        if !is_sid || !entry.path().is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => tracing::info!(home = ?entry.path(), "removed legacy per-session DSH home"),
            Err(err) => {
                tracing::warn!(home = ?entry.path(), error = %err, "could not remove legacy per-session DSH home")
            }
        }
    }
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

/// A `dsh web` this manager started: the child, where it listens, and the
/// credential ccteam exchanged for at startup.
struct StartedDsh {
    child: Child,
    port: u16,
    credential: Option<String>,
}

async fn spawn_until_ready(
    spawn: DshSpawnSpec,
    tail: ErrorTail,
    client: &reqwest::Client,
) -> Result<StartedDsh> {
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
    // BOTH pipes are scanned for the readiness line: which one a vendor writes
    // it to is the vendor's choice, not a contract, and stderr keeps feeding
    // the error tail either way.
    let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
    spawn_line_reader(stdout, None, lines_tx.clone());
    if let Some(stderr) = child.stderr.take() {
        spawn_line_reader(stderr, Some(tail.clone()), lines_tx.clone());
    }
    drop(lines_tx);
    let readiness = tokio::time::timeout(READINESS_TIMEOUT, async {
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some(line) = line else {
                        return Err(anyhow!("DSH web exited before readiness"));
                    };
                    if let Some(readiness) = parse_readiness(&line) {
                        return Ok(readiness);
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
    probe_listener(client, readiness.port).await?;
    let credential = match &readiness.exchange {
        Some(path_and_query) => exchange_credential(client, readiness.port, path_and_query).await,
        None => None,
    };
    Ok(StartedDsh {
        child,
        port: readiness.port,
        credential,
    })
}

/// Assert the listener answers — LIVENESS ONLY.
///
/// This gate used to demand a 2xx, which is the whole 0.1.5 breakage: a
/// perfectly healthy `dsh web` answers 401 to a request without its browser
/// cookie, and ccteam reported "DSH web health probe returned 401
/// Unauthorized" for a server that had started fine. Only a transport error
/// means "did not start".
async fn probe_listener(client: &reqwest::Client, port: u16) -> Result<WebProbe> {
    let resp = client
        .get(format!("http://127.0.0.1:{port}/"))
        .timeout(HEALTH_TIMEOUT)
        .send()
        .await
        .context("probe DSH web readiness")?;
    Ok(classify_probe(resp.status()))
}

/// Trade the readiness URL's query for whatever the vendor mints, exactly once.
///
/// Opaque on purpose: ccteam replays the printed query without reading it,
/// keeps every returned cookie as `name=value`, and never parses, interprets
/// or logs either. That is what survives the next release — a renamed cookie,
/// a second one, a different token format all pass straight through.
///
/// The request is rebuilt against `127.0.0.1:<port>` rather than the printed
/// authority because the cookie is bound to the Host of THIS exchange, and
/// `127.0.0.1:<port>` is exactly what the companion proxy rewrites Host to. A
/// cookie minted for any other name is one the proxy can never present.
///
/// A failure here is not fatal: the instance is up, and the panel says 401
/// instead of nothing.
async fn exchange_credential(
    client: &reqwest::Client,
    port: u16,
    path_and_query: &str,
) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}{path_and_query}");
    match client.get(&url).timeout(HEALTH_TIMEOUT).send().await {
        Ok(resp) => {
            let status = resp.status();
            let credential = credential_from_set_cookie(
                resp.headers()
                    .get_all(reqwest::header::SET_COOKIE)
                    .iter()
                    .filter_map(|value| value.to_str().ok()),
            );
            if credential.is_none() {
                tracing::warn!(
                    %status,
                    "DSH web readiness URL returned no cookie; the panel will not be able to reach it"
                );
            }
            credential
        }
        Err(err) => {
            // `without_url` matters: the URL IS the credential here, and a
            // reqwest error prints the URL it failed on.
            tracing::warn!(
                "DSH web credential exchange failed: {:#}",
                err.without_url()
            );
            None
        }
    }
}

/// Every `Set-Cookie` reduced to the `Cookie` header value that replays them:
/// the leading `name=value` of each, joined. Attributes (`Path`, `HttpOnly`,
/// `Max-Age`, …) are the browser's business and are dropped.
fn credential_from_set_cookie<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let pairs: Vec<&str> = values
        .filter_map(|value| value.split(';').next())
        .map(str::trim)
        .filter(|pair| pair.contains('=') && !pair.starts_with('='))
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

/// Drain one child pipe for the life of the process: every line is offered to
/// the readiness scan, and stderr additionally feeds the error tail.
///
/// Draining past readiness matters — a reader dropped the moment the URL
/// appears closes the pipe, and the next thing `dsh web` prints kills it.
fn spawn_line_reader(
    pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    tail: Option<ErrorTail>,
    lines: tokio::sync::mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(tail) = &tail {
                push_tail(tail, line.clone()).await;
            }
            // The receiver is gone once readiness is decided; keep draining.
            let _ = lines.send(line);
        }
    });
}

/// The readiness line, read as the URL `dsh web` actually printed.
///
/// Whatever follows the prefix is ONE whitespace-delimited URL and is parsed
/// as such — never scraped. A LAN URL may follow it in parentheses, and other
/// `dsh web:` lines (the browser-handoff notice) are not URLs at all and are
/// simply skipped, not fatal.
fn parse_readiness(line: &str) -> Option<DshReadiness> {
    let printed = line
        .split_once(READINESS_PREFIX)?
        .1
        .split_whitespace()
        .next()?;
    let url = reqwest::Url::parse(printed).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let port = url.port_or_known_default()?;
    let exchange = url
        .query()
        .filter(|query| !query.is_empty())
        .map(|query| format!("{}?{query}", url.path()));
    Some(DshReadiness { port, exchange })
}

/// The client every DSH probe and the credential exchange share.
///
/// Redirects are NEVER followed: the credential exchange answers 303 to a
/// clean `/`, and following it would land on an unauthenticated page and throw
/// the `Set-Cookie` away. Nothing else here wants a redirect followed either.
/// A builder that cannot start (TLS init; nothing here is TLS) degrades to a
/// default client, which costs the credential and logs it, never a wrong one.
fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
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
            Arc::new(|_root, owner| Ok(format!("ccteam:token-for-{owner}"))),
            config,
        )
    }

    /// The readiness line is a URL, so ccteam reads it as one: the port AND
    /// the credential beside it come out of the same parse. Scraping digits
    /// off a hardcoded prefix is what silently dropped `?token=` when dsh
    /// 0.1.5 added it.
    #[test]
    fn readiness_line_is_read_as_the_url_dsh_printed() {
        assert_eq!(
            parse_readiness("noise dsh web: http://127.0.0.1:35479/?token=abc"),
            Some(DshReadiness {
                port: 35479,
                exchange: Some("/?token=abc".to_string()),
            })
        );
        // No query = nothing to exchange, and that is not an error: an older
        // dsh, or a future one that drops browser auth.
        assert_eq!(
            parse_readiness("dsh web: http://127.0.0.1:35479/"),
            Some(DshReadiness {
                port: 35479,
                exchange: None,
            })
        );
        // A LAN URL follows in parentheses; the first token is the loopback one.
        assert_eq!(
            parse_readiness(
                "dsh web: http://127.0.0.1:4567/?token=t (LAN: http://192.168.1.5:4567/?token=t)"
            ),
            Some(DshReadiness {
                port: 4567,
                exchange: Some("/?token=t".to_string()),
            })
        );
        // A host other than loopback still yields a port: the printed line is
        // the authority on where dsh listens, not ccteam's assumptions.
        assert_eq!(
            parse_readiness("dsh web: http://localhost:35479/"),
            Some(DshReadiness {
                port: 35479,
                exchange: None,
            })
        );
        // Same prefix, not a URL: skipped, never fatal.
        assert_eq!(
            parse_readiness("dsh web: opening the default browser; pass --no-open to disable"),
            None
        );
        assert_eq!(parse_readiness("dsh listening on 3080"), None);
    }

    /// Liveness is not authorization. A 401 is a running server saying "not
    /// you", which is exactly what every unauthenticated dsh 0.1.5 request
    /// gets — treating it as "did not start" is the bug this fixes.
    #[test]
    fn an_authentication_challenge_still_proves_the_listener_is_alive() {
        assert_eq!(
            classify_probe(reqwest::StatusCode::UNAUTHORIZED),
            WebProbe::NeedsCredential
        );
        assert_eq!(
            classify_probe(reqwest::StatusCode::FORBIDDEN),
            WebProbe::NeedsCredential
        );
        assert_eq!(classify_probe(reqwest::StatusCode::OK), WebProbe::Serving);
        assert_eq!(
            classify_probe(reqwest::StatusCode::SEE_OTHER),
            WebProbe::Serving
        );
        for probe in [WebProbe::Serving, WebProbe::NeedsCredential] {
            assert!(probe.alive(), "{probe:?} answered, so it is up");
        }
        assert!(!WebProbe::Absent.alive());
    }

    /// The credential stays opaque: every cookie the vendor sets is replayed
    /// as `name=value`, whatever it is called and however many there are.
    #[test]
    fn set_cookie_headers_become_one_replayable_cookie_value() {
        assert_eq!(
            credential_from_set_cookie(
                ["dsh-auth-avJ5=v1.body.sig; Max-Age=2592000; Path=/; HttpOnly; SameSite=Strict"]
                    .into_iter()
            ),
            Some("dsh-auth-avJ5=v1.body.sig".to_string())
        );
        assert_eq!(
            credential_from_set_cookie(
                ["first=one; Path=/", " second=two; HttpOnly", "third=three"].into_iter()
            ),
            Some("first=one; second=two; third=three".to_string())
        );
        assert_eq!(credential_from_set_cookie(std::iter::empty()), None);
        assert_eq!(
            credential_from_set_cookie(["", "=novalue", "novalue"].into_iter()),
            None
        );
    }

    /// Every non-tenant front door (admin web, IM chats, a blank owner) is the
    /// same human at the same `~/.dsh`, so they must collapse to ONE instance
    /// key. Two keys would mean two `dsh web` processes writing that home and
    /// racing for one ACP socket.
    #[test]
    fn operator_shaped_owner_tags_collapse_to_one_instance_key() {
        let tenant = DshRuntimeIdentity::for_owner_tag("user:alice");
        assert_eq!(
            tenant,
            DshRuntimeIdentity {
                owner_tag: "user:alice".to_string(),
                id: "alice".to_string(),
                operator: false,
            }
        );
        for tag in ["user:web-api", "user:", "telegram:123", "slack:T1/U2", ""] {
            let identity = DshRuntimeIdentity::for_owner_tag(tag);
            assert!(identity.operator, "`{tag}` is the operator");
            assert_eq!(
                identity.owner_tag, OPERATOR_OWNER_TAG,
                "`{tag}` must share the operator's single instance"
            );
        }
    }

    #[test]
    fn orphan_reaping_only_matches_our_init_parented_runtime_homes() {
        let ccteam_home = Path::new("/srv/ccteam-home");
        let managed = ccteam_home.join("runtime/dsh/web/user-alice");
        assert!(is_ccteam_managed_dsh_orphan(&managed, 1, ccteam_home));
        assert!(!is_ccteam_managed_dsh_orphan(&managed, 4242, ccteam_home));
        assert!(!is_ccteam_managed_dsh_orphan(
            Path::new("/home/alice/.dsh"),
            1,
            ccteam_home
        ));
        assert!(!is_ccteam_managed_dsh_orphan(
            Path::new("/srv/unrelated/dsh"),
            1,
            ccteam_home
        ));
        assert!(!is_ccteam_managed_dsh_orphan(
            Path::new("/srv/ccteam-home/runtime/dsh/../../alice/.dsh"),
            1,
            ccteam_home
        ));
    }

    #[test]
    fn legacy_sweep_removes_per_sid_homes_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let dsh_root = tmp.path().join("runtime").join("dsh");
        for name in ["s1", "s42", "web", "client", "acp", "spare", "s"] {
            std::fs::create_dir_all(dsh_root.join(name)).unwrap();
        }
        std::fs::write(dsh_root.join("s1").join("marker"), b"x").unwrap();

        remove_legacy_per_sid_homes(tmp.path());

        assert!(!dsh_root.join("s1").exists());
        assert!(!dsh_root.join("s42").exists());
        for keep in ["web", "client", "acp", "spare", "s"] {
            assert!(
                dsh_root.join(keep).is_dir(),
                "`{keep}` is not a per-session home and must survive"
            );
        }
    }

    #[test]
    fn tenant_home_segment_keeps_safe_ids_and_hashes_unsafe_ids() {
        assert_eq!(
            crate::execution::dsh_acp::tenant_home_segment("alice-1"),
            "alice-1"
        );
        assert!(crate::execution::dsh_acp::tenant_home_segment("bad/id").starts_with("tenant-"));
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
        assert!(manager.endpoint_for(&identity).await.is_err());
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
                    credential: None,
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
