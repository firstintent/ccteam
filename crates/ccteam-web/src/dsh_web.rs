//! DSH web first-class companion-port support.
//!
//! This module intentionally does not model DSH web instances as ccteam
//! sessions. They are local vendor web servers keyed by the authenticated web
//! identity and reached through a byte proxy that owns the ccteam auth boundary.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU16, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::{
    body::Body,
    extract::{ws::WebSocketUpgrade, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use axum_extra::extract::CookieJar;
use ccteam_core::enroll::ensure_user_credential_in;
use ccteam_harness::{
    build_web_spawn_spec, is_ccteam_managed_dsh_orphan, tenant_home_segment, DshWebSpawnOptions,
    DSH_NATIVE_WEB_PROFILE, DSH_WEB_PROFILE,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};
use tower_http::compression::CompressionLayer;
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

const DEFAULT_ATTACH_URL: &str = "http://127.0.0.1:3080";
const ATTACH_URL_ENV: &str = "CCTEAM_DSH_WEB_ATTACH_URL";
const READINESS_PREFIX: &str = "dsh web: http://127.0.0.1:";
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const ORPHAN_STOP_GRACE: Duration = Duration::from_millis(750);
const ERROR_TAIL_LINES: usize = 24;

#[derive(Debug, Clone)]
pub struct DshWebRuntimeConfig {
    pub enabled: bool,
    pub daemon_url: String,
    pub attach_url: Option<String>,
}

impl DshWebRuntimeConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            daemon_url: "http://127.0.0.1:7331".to_string(),
            attach_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DshWebStatusState {
    Disabled,
    Stopped,
    Starting,
    Running,
    Attached,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DshHomeKind {
    Own,
    Managed,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DshStatusResponse {
    /// `disabled` means `--dsh-web-bind off`; no companion listener exists.
    pub state: DshWebStatusState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companion_port: Option<u16>,
    pub home_kind: DshHomeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dsh_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_url: Option<String>,
}

#[derive(Debug)]
struct DshInstance {
    child: Option<Child>,
    port: Option<u16>,
    _home: PathBuf,
    _started_at: DateTime<Utc>,
    last_activity: DateTime<Utc>,
    kind: DshInstanceKind,
    state: DshWebStatusState,
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

#[derive(Debug)]
pub struct DshWebSupervisor {
    config: DshWebRuntimeConfig,
    companion_port: AtomicU16,
    instances: Mutex<HashMap<String, DshInstance>>,
    /// In-flight start waiters, keyed like `instances`. A `Starting` row
    /// without an entry here is an orphan (the HTTP task that called
    /// `start_for` was cancelled and took the child with it).
    inflight: Mutex<HashMap<String, watch::Receiver<bool>>>,
    client: reqwest::Client,
}

impl Default for DshWebSupervisor {
    fn default() -> Self {
        Self::disabled()
    }
}

impl DshWebSupervisor {
    pub fn disabled() -> Self {
        Self::new(DshWebRuntimeConfig::disabled())
    }

    pub fn new(config: DshWebRuntimeConfig) -> Self {
        Self {
            config,
            companion_port: AtomicU16::new(0),
            instances: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            client: reqwest::Client::new(),
        }
    }

    pub fn set_companion_addr(&self, addr: SocketAddr) {
        self.companion_port.store(addr.port(), Ordering::SeqCst);
    }

    pub fn companion_port(&self) -> Option<u16> {
        if !self.config.enabled {
            return None;
        }
        match self.companion_port.load(Ordering::SeqCst) {
            0 => None,
            p => Some(p),
        }
    }

    pub async fn status_for(&self, identity: &Identity) -> DshStatusResponse {
        let home_kind = home_kind(identity);
        if !self.config.enabled {
            return DshStatusResponse {
                state: DshWebStatusState::Disabled,
                port: None,
                companion_port: None,
                home_kind,
                dsh_version: None,
                error_tail: None,
                native_url: None,
            };
        }

        let key = identity.owner_tag();
        let snapshot = {
            let mut instances = self.instances.lock().await;
            if let Some(instance) = instances.get_mut(&key) {
                if instance.state == DshWebStatusState::Starting {
                    let inflight = self.inflight.lock().await;
                    if !inflight.contains_key(&key) {
                        // Orphan: a cancelled request dropped the spawn future
                        // (and `kill_on_drop` the child) after inserting
                        // Starting. Tell the truth so the UI offers Start
                        // instead of spinning forever.
                        instance.state = DshWebStatusState::Stopped;
                        instance.port = None;
                    }
                }
            }
            instances.get(&key).map(|instance| {
                (
                    instance.state.clone(),
                    instance.port,
                    instance.kind,
                    instance.error_tail.clone(),
                    instance.dsh_version.clone(),
                    instance.native_url.clone(),
                )
            })
        };
        let Some((state, port, kind, tail, dsh_version, native_url)) = snapshot else {
            return DshStatusResponse {
                state: DshWebStatusState::Stopped,
                port: None,
                companion_port: self.companion_port(),
                home_kind,
                dsh_version: None,
                error_tail: None,
                native_url: None,
            };
        };
        let error_tail = read_error_tail(&tail).await;
        DshStatusResponse {
            state,
            port,
            companion_port: self.companion_port(),
            home_kind,
            dsh_version,
            error_tail,
            native_url: if identity.is_admin {
                native_url
            } else {
                let _ = kind;
                None
            },
        }
    }

    pub async fn start_for(&self, app: &AppState, identity: &Identity) -> DshStatusResponse {
        if !self.config.enabled {
            return self.status_for(identity).await;
        }
        let key = identity.owner_tag();

        // Serving, or another start is actually running: do not spawn a
        // second child. `Starting` WITHOUT an inflight waiter is an orphan
        // (cancelled HTTP task) and must be retried — treating it as live
        // is what left the rob tenant spinning on "Starting the DSH web
        // instance…" with no process behind it.
        //
        // `instances` (the MutexGuard) drops at the end of this block —
        // before any `.await` below tries to lock the same
        // `tokio::sync::Mutex` again. Awaiting `status_for` while still
        // holding the guard (the original deadlock) self-deadlocks: the
        // task waits on a lock only it holds, and every later caller of
        // `self.instances.lock()` — including every proxied request and
        // every future `/status` poll — then hangs forever too.
        if let Some(rx) = self.live_or_inflight(&key).await {
            if let Some(mut rx) = rx {
                let _ = rx.wait_for(|done| *done).await;
            }
            return self.status_for(identity).await;
        }

        let kind = if identity.is_admin {
            DshInstanceKind::Operator
        } else {
            DshInstanceKind::Tenant
        };
        let home = match dsh_home_for(app, identity) {
            Ok(path) => path,
            Err(err) => {
                self.record_stopped_error(&key, kind, PathBuf::new(), err.to_string())
                    .await;
                return self.status_for(identity).await;
            }
        };

        let Some(claimed) = self.claim_start(&key, kind, home.clone()).await else {
            return self.status_for(identity).await;
        };

        if let StartClaim::Spawn { tx, .. } = &claimed {
            let tx = tx.clone();
            // The spawn itself lives on a detached task so cancelling the
            // HTTP request (browser abort, iframe timeout, companion-port
            // retry) cannot drop `spawn_until_ready`'s `kill_on_drop` Child
            // and leave the map stuck at Starting. This waiter is the only
            // thing the request owns; dropping it just stops waiting.
            let supervisor = app.dsh_web.clone();
            let app = app.clone();
            let identity = identity.clone();
            let start_key = key.clone();
            tokio::spawn(async move {
                let _done = StartDone(tx);
                let tail = {
                    let instances = supervisor.instances.lock().await;
                    instances
                        .get(&start_key)
                        .map(|i| i.error_tail.clone())
                        .unwrap_or_else(new_error_tail)
                };
                let start_result = if identity.is_admin {
                    supervisor
                        .start_operator(&app, &identity, home.clone(), tail.clone())
                        .await
                } else {
                    supervisor
                        .start_tenant(&app, &identity, home.clone(), tail.clone())
                        .await
                };
                supervisor
                    .finish_start(&start_key, kind, home, tail, start_result)
                    .await;
            });
        }

        let mut rx = claimed.into_rx();
        let _ = rx.wait_for(|done| *done).await;
        self.status_for(identity).await
    }

    /// `None` = caller should start. `Some(None)` = already serving.
    /// `Some(Some(rx))` = wait for the in-flight start.
    async fn live_or_inflight(&self, key: &str) -> Option<Option<watch::Receiver<bool>>> {
        let instances = self.instances.lock().await;
        match instances.get(key).map(|i| &i.state) {
            Some(DshWebStatusState::Running | DshWebStatusState::Attached) => Some(None),
            Some(DshWebStatusState::Starting) => {
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
            Some(DshWebStatusState::Running | DshWebStatusState::Attached) => return None,
            Some(DshWebStatusState::Starting) => {
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
                kind,
                state: DshWebStatusState::Starting,
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
                        Some(DshWebStatusState::Starting)
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
                        Some(DshWebStatusState::Starting)
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

    pub async fn stop_for(&self, identity: &Identity) -> DshStatusResponse {
        if !self.config.enabled {
            return self.status_for(identity).await;
        }
        let key = identity.owner_tag();
        let instance = {
            let mut instances = self.instances.lock().await;
            instances.remove(&key)
        };
        if let Some(instance) = instance {
            terminate_instance(instance).await;
        }
        self.status_for(identity).await
    }

    pub async fn proxy_target_for(
        &self,
        app: &AppState,
        identity: &Identity,
    ) -> Result<ProxyTarget> {
        if !self.config.enabled {
            return Err(anyhow!("DSH web companion listener is disabled"));
        }
        let key = identity.owner_tag();
        self.start_for(app, identity).await;
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(&key)
            .ok_or_else(|| anyhow!("DSH web instance is stopped"))?;
        instance.last_activity = Utc::now();
        let port = instance
            .port
            .ok_or_else(|| anyhow!("DSH web instance is starting"))?;
        Ok(ProxyTarget { port })
    }

    pub async fn shutdown_all(&self) {
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
                kind,
                state: DshWebStatusState::Stopped,
                error_tail: tail,
                dsh_version: None,
                native_url: None,
            },
        );
    }

    async fn start_operator(
        &self,
        app: &AppState,
        identity: &Identity,
        home: PathBuf,
        tail: ErrorTail,
    ) -> Result<DshInstance> {
        let attach_url = self
            .config
            .attach_url
            .clone()
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
                kind: DshInstanceKind::Operator,
                state: DshWebStatusState::Attached,
                error_tail: tail,
                dsh_version: None,
                native_url: Some(normalize_url(&attach_url)),
            });
        }

        let spawn_home = home.clone();
        let spawn = build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: &identity.owner_tag(),
            ccteam_home: app.paths.root.clone(),
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
            kind: DshInstanceKind::Operator,
            state: DshWebStatusState::Running,
            error_tail: tail,
            dsh_version: None,
            native_url: Some(format!("http://127.0.0.1:{port}/")),
        })
    }

    async fn start_tenant(
        &self,
        app: &AppState,
        identity: &Identity,
        home: PathBuf,
        tail: ErrorTail,
    ) -> Result<DshInstance> {
        let owner = identity.owner_tag();
        let credential = ensure_user_credential_in(&app.paths.root, &owner)
            .with_context(|| format!("ensure enrollment credential for {owner}"))?;
        let spawn = build_web_spawn_spec(DshWebSpawnOptions {
            owner_tag: &owner,
            ccteam_home: app.paths.root.clone(),
            dsh_home: home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some(&credential.bearer()),
            daemon_url: Some(&self.config.daemon_url),
        })
        .map_err(|e| anyhow!("{e}"))?;
        let (child, port) = spawn_until_ready(spawn, tail.clone(), &self.client).await?;
        Ok(DshInstance {
            child: Some(child),
            port: Some(port),
            _home: home,
            _started_at: Utc::now(),
            last_activity: Utc::now(),
            kind: DshInstanceKind::Tenant,
            state: DshWebStatusState::Running,
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

#[derive(Debug, Clone, Copy)]
pub struct ProxyTarget {
    pub port: u16,
}

pub fn companion_router() -> Router<AppState> {
    Router::new()
        .route("/", any(handle_companion_request))
        .route("/{*path}", any(handle_companion_request))
        // The origin hop deliberately stays identity-encoded so HTML can be
        // spliced safely (see `should_strip_request_header`). Compression is
        // applied only after that splice, on the outbound browser/WAN hop.
        // CompressionLayer leaves WebSocket 101 upgrades untouched.
        .layer(CompressionLayer::new().gzip(true).br(true))
}

/// Reap DSH processes stranded by daemon versions that predate PDEATHSIG.
///
/// `/proc` is intentionally the authority here: only an init-parented process
/// whose own `DSH_HOME` points inside this ccteam installation's managed DSH
/// runtime can match. Failures are ignored because this startup cleanup is a
/// best-effort compatibility sweep, never a reason to keep the daemon down.
#[cfg(target_os = "linux")]
pub(crate) async fn sweep_legacy_dsh_orphans(ccteam_home: &Path) {
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
pub(crate) async fn sweep_legacy_dsh_orphans(_ccteam_home: &Path) {
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

async fn handle_companion_request(
    State(app): State<AppState>,
    jar: CookieJar,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    req: Request<Body>,
) -> Response {
    let identity = match crate::auth::resolve_strict_web_identity(
        &app.auth,
        &app.paths.root,
        req.headers(),
        &jar,
    ) {
        Ok(Some(identity)) => identity,
        Ok(None) => return (StatusCode::UNAUTHORIZED, "auth required").into_response(),
        Err(message) => return (StatusCode::INTERNAL_SERVER_ERROR, message).into_response(),
    };
    let target = match app.dsh_web.proxy_target_for(&app, &identity).await {
        Ok(target) => target,
        Err(err) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": err.to_string(),
                    "error_code": "dsh_upstream_unready"
                })),
            )
                .into_response();
        }
    };
    if is_ws_upgrade(req.headers()) {
        if let Ok(ws) = ws {
            let uri = req.uri().clone();
            return ws.on_upgrade(move |socket| proxy_websocket(socket, target.port, uri));
        }
    }
    proxy_http(req, target.port, &app.dsh_proxy_client).await
}

async fn proxy_http(req: Request<Body>, port: u16, client: &reqwest::Client) -> Response {
    let (parts, body) = req.into_parts();
    let target = format!(
        "http://127.0.0.1:{port}{}",
        parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/")
    );
    let mut builder = client.request(parts.method, target);
    let headers = proxy_request_headers(&parts.headers, port);
    builder = builder.headers(headers);
    builder = builder.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    match builder.send().await {
        Ok(resp) => response_from_reqwest(resp).await,
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// Restores `crypto.randomUUID` when the browser withheld it.
///
/// `crypto.randomUUID` is a **secure-context-only** API. Native `dsh web`
/// binds loopback, and `http://127.0.0.1` *is* a secure context, so upstream
/// calls it freely — `dsh-client-connection` mints the id of every single
/// `/api` RPC with it. Reaching that same UI through this companion port over
/// plain HTTP on a LAN address is NOT a secure context, so the property is
/// simply absent and every RPC throws `crypto.randomUUID is not a function`
/// before it is sent: no workspace list, no provider catalog, no chat.
///
/// ccteam is what moved the UI off loopback, so ccteam restores the platform
/// API it took away. `crypto.getRandomValues` stays available in insecure
/// contexts, so this is a real RFC 4122 v4 UUID — no downgrade to `Math.random`
/// — and it no-ops when the real thing is present (HTTPS, or a browser on the
/// daemon host), which keeps it inert on the paths that never needed it.
const CRYPTO_RANDOM_UUID_POLYFILL: &str = r#"<script>(function(){var c=window.crypto;if(!c||typeof c.getRandomValues!=="function")return;if(typeof c.randomUUID==="function")return;var h=[];for(var i=0;i<256;i++)h.push((i+256).toString(16).slice(1));Object.defineProperty(c,"randomUUID",{configurable:true,writable:true,value:function(){var b=new Uint8Array(16);c.getRandomValues(b);b[6]=(b[6]&15)|64;b[8]=(b[8]&63)|128;return h[b[0]]+h[b[1]]+h[b[2]]+h[b[3]]+"-"+h[b[4]]+h[b[5]]+"-"+h[b[6]]+h[b[7]]+"-"+h[b[8]]+h[b[9]]+"-"+h[b[10]]+h[b[11]]+h[b[12]]+h[b[13]]+h[b[14]]+h[b[15]]}})})();</script>"#;

/// Splice the polyfill in as the first thing inside `<head>` so it runs before
/// the boot script and every client plugin module. Returns `None` when there
/// is no head to open, leaving the body untouched rather than guessing.
fn inject_polyfill(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let head = lower.find("<head")?;
    let open_end = lower[head..].find('>')? + head + 1;
    let mut out = String::with_capacity(html.len() + CRYPTO_RANDOM_UUID_POLYFILL.len());
    out.push_str(&html[..open_end]);
    out.push_str(CRYPTO_RANDOM_UUID_POLYFILL);
    out.push_str(&html[open_end..]);
    Some(out)
}

fn is_html_response(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_start().to_ascii_lowercase().starts_with("text/html"))
}

async fn response_from_reqwest(resp: reqwest::Response) -> Response {
    let status = resp.status();
    let headers = resp.headers().clone();

    // HTML is buffered (documents are small) so the polyfill can be spliced
    // in; everything else — assets, RPC payloads, downloads — keeps streaming
    // untouched. `accept-encoding` is stripped on the way out (see
    // `should_strip_request_header`), so the body here is always identity and
    // splicing can never corrupt a compressed stream.
    if is_html_response(&headers) {
        match resp.bytes().await {
            Ok(bytes) => {
                let body = match std::str::from_utf8(&bytes) {
                    Ok(html) => match inject_polyfill(html) {
                        Some(patched) => Body::from(patched),
                        None => Body::from(bytes),
                    },
                    // Not UTF-8 → not something to splice; pass it through.
                    Err(_) => Body::from(bytes),
                };
                let mut out = Response::builder().status(status);
                if let Some(out_headers) = out.headers_mut() {
                    for (name, value) in headers.iter() {
                        // CONTENT_LENGTH is dropped: the spliced body is
                        // longer than the origin advertised, and a stale
                        // length truncates the document in the browser.
                        if !is_hop_by_hop(name) && name != header::CONTENT_LENGTH {
                            out_headers.append(name, value.clone());
                        }
                    }
                }
                return out.body(body).unwrap_or_else(|err| {
                    (StatusCode::BAD_GATEWAY, err.to_string()).into_response()
                });
            }
            Err(err) => {
                return (StatusCode::BAD_GATEWAY, err.to_string()).into_response();
            }
        }
    }

    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    let mut out = Response::builder().status(status);
    if let Some(out_headers) = out.headers_mut() {
        for (name, value) in headers.iter() {
            if !is_hop_by_hop(name) {
                out_headers.append(name, value.clone());
            }
        }
    }
    out.body(Body::from_stream(stream))
        .unwrap_or_else(|err| (StatusCode::BAD_GATEWAY, err.to_string()).into_response())
}

fn proxy_request_headers(headers: &HeaderMap, port: u16) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers.iter() {
        if should_strip_request_header(name) {
            continue;
        }
        if name == header::ORIGIN {
            if let Ok(origin) = HeaderValue::from_str(&format!("http://127.0.0.1:{port}")) {
                out.insert(header::ORIGIN, origin);
            }
            continue;
        }
        out.append(name, value.clone());
    }
    out.insert(
        header::HOST,
        HeaderValue::from_str(&format!("127.0.0.1:{port}")).expect("loopback host header"),
    );
    out
}

fn should_strip_request_header(name: &HeaderName) -> bool {
    is_hop_by_hop(name)
        || name == header::COOKIE
        || name == header::AUTHORIZATION
        // This hop is loopback, so compression buys nothing — and dropping it
        // guarantees the HTML that comes back is identity-encoded, which is
        // what lets `response_from_reqwest` splice the secure-context polyfill
        // into it without ever risking a corrupted compressed stream.
        || name == header::ACCEPT_ENCODING
        || name.as_str().starts_with("sec-fetch-")
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn is_ws_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && headers
            .get(header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
}

async fn proxy_websocket(socket: axum::extract::ws::WebSocket, port: u16, uri: axum::http::Uri) {
    use axum::extract::ws::Message as AxMessage;
    use tokio_tungstenite::tungstenite::Message as TMessage;

    let target = format!(
        "ws://127.0.0.1:{port}{}",
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
    );
    let Ok((upstream, _)) = tokio_tungstenite::connect_async(target).await else {
        return;
    };
    let (mut down_tx, mut down_rx) = socket.split();
    let (mut up_tx, mut up_rx) = upstream.split();
    loop {
        tokio::select! {
            msg = down_rx.next() => match msg {
                Some(Ok(msg)) => {
                    let mapped = match msg {
                        AxMessage::Text(t) => Some(TMessage::Text(t.as_str().to_string())),
                        AxMessage::Binary(b) => Some(TMessage::Binary(b.to_vec())),
                        AxMessage::Ping(b) => Some(TMessage::Ping(b.to_vec())),
                        AxMessage::Pong(b) => Some(TMessage::Pong(b.to_vec())),
                        AxMessage::Close(frame) => {
                            let _ = up_tx.send(TMessage::Close(frame.map(|f| tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                code: f.code.into(),
                                reason: f.reason.to_string().into(),
                            }))).await;
                            break;
                        }
                    };
                    if let Some(mapped) = mapped {
                        if up_tx.send(mapped).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Err(_)) | None => break,
            },
            msg = up_rx.next() => match msg {
                Some(Ok(msg)) => {
                    let mapped = match msg {
                        TMessage::Text(t) => Some(AxMessage::Text(t.into())),
                        TMessage::Binary(b) => Some(AxMessage::Binary(b.into())),
                        TMessage::Ping(b) => Some(AxMessage::Ping(b.into())),
                        TMessage::Pong(b) => Some(AxMessage::Pong(b.into())),
                        TMessage::Close(_) => {
                            let _ = down_tx.send(AxMessage::Close(None)).await;
                            break;
                        }
                        TMessage::Frame(_) => None,
                    };
                    if let Some(mapped) = mapped {
                        if down_tx.send(mapped).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Err(_)) | None => break,
            }
        }
    }
}

async fn spawn_until_ready(
    spawn: ccteam_harness::execution::dsh_acp::spawn_spec::DshSpawnSpec,
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

fn dsh_home_for(app: &AppState, identity: &Identity) -> Result<PathBuf> {
    if identity.is_admin {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("HOME is unknown"))?;
        Ok(home.join(".dsh"))
    } else {
        Ok(app
            .paths
            .root
            .join("runtime")
            .join("dsh")
            .join("web")
            .join(tenant_home_segment(&identity.id)))
    }
}

fn home_kind(identity: &Identity) -> DshHomeKind {
    if identity.is_admin {
        DshHomeKind::Own
    } else {
        DshHomeKind::Managed
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

    #[test]
    fn strips_sensitive_and_hop_by_hop_headers() {
        assert!(should_strip_request_header(&header::COOKIE));
        assert!(should_strip_request_header(&header::AUTHORIZATION));
        assert!(should_strip_request_header(&HeaderName::from_static(
            "sec-fetch-site"
        )));
        assert!(should_strip_request_header(&header::CONNECTION));
        assert!(!should_strip_request_header(&header::CONTENT_TYPE));
    }

    /// Compression must stay off on the loopback hop: the polyfill splice
    /// assumes an identity-encoded body, and a gzipped document spliced as
    /// text would reach the browser corrupted.
    #[test]
    fn accept_encoding_is_stripped_so_html_comes_back_spliceable() {
        assert!(should_strip_request_header(&header::ACCEPT_ENCODING));
    }

    #[test]
    fn polyfill_lands_before_the_boot_script_and_module_imports() {
        let html = "<!doctype html>\n<html lang=\"zh-CN\">\n  <head><script>window.__DSH_BOOT__ = {}</script>\n<script type=\"module\" src=\"/assets/index.js\"></script></head>\n<body></body></html>";
        let patched = inject_polyfill(html).expect("head present");
        let at = patched.find("randomUUID").expect("polyfill present");
        // Ordering is the whole point: DSH mints RPC ids from the very first
        // client module, so a polyfill placed after them is a polyfill that
        // never ran in time.
        assert!(at < patched.find("__DSH_BOOT__").unwrap());
        assert!(at < patched.find("/assets/index.js").unwrap());
        assert!(patched.starts_with("<!doctype html>"));
    }

    #[test]
    fn polyfill_injection_is_inert_without_a_head() {
        assert!(inject_polyfill("{\"json\":true}").is_none());
        assert!(inject_polyfill("plain text").is_none());
    }

    #[tokio::test]
    async fn orphaned_starting_status_heals_to_stopped() {
        let supervisor = DshWebSupervisor::new(DshWebRuntimeConfig {
            enabled: true,
            daemon_url: "http://127.0.0.1:7331".to_string(),
            attach_url: None,
        });
        let identity = Identity::admin();
        let key = identity.owner_tag();
        {
            let mut instances = supervisor.instances.lock().await;
            instances.insert(
                key,
                DshInstance {
                    child: None,
                    port: None,
                    _home: PathBuf::new(),
                    _started_at: Utc::now(),
                    last_activity: Utc::now(),
                    kind: DshInstanceKind::Operator,
                    state: DshWebStatusState::Starting,
                    error_tail: new_error_tail(),
                    dsh_version: None,
                    native_url: None,
                },
            );
        }
        let status = supervisor.status_for(&identity).await;
        assert_eq!(
            status.state,
            DshWebStatusState::Stopped,
            "Starting with no inflight task is an orphan, not a live boot"
        );
    }

    #[test]
    fn only_html_responses_are_buffered_for_injection() {
        let html_type = |v: &'static str| {
            let mut h = HeaderMap::new();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(v));
            is_html_response(&h)
        };
        assert!(html_type("text/html; charset=utf-8"));
        assert!(html_type("text/html"));
        // Assets and RPC payloads must keep streaming untouched.
        assert!(!html_type("application/json"));
        assert!(!html_type("text/javascript"));
        assert!(!html_type("application/octet-stream"));
        assert!(!is_html_response(&HeaderMap::new()));
    }
}
