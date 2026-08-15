//! DSH web first-class companion-port support.
//!
//! This module intentionally does not model DSH web instances as ccteam
//! sessions. They are local vendor web servers keyed by the authenticated web
//! identity and reached through a byte proxy that owns the ccteam auth boundary.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
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
    build_web_spawn_spec, DshWebSpawnOptions, DSH_NATIVE_WEB_PROFILE, DSH_WEB_PROFILE,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

const DEFAULT_ATTACH_URL: &str = "http://127.0.0.1:3080";
const ATTACH_URL_ENV: &str = "CCTEAM_DSH_WEB_ATTACH_URL";
const READINESS_PREFIX: &str = "dsh web: http://127.0.0.1:";
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
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

type ErrorTail = Arc<Mutex<VecDeque<String>>>;

#[derive(Debug)]
pub struct DshWebSupervisor {
    config: DshWebRuntimeConfig,
    companion_port: AtomicU16,
    instances: Mutex<HashMap<String, DshInstance>>,
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
            let instances = self.instances.lock().await;
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
        {
            let instances = self.instances.lock().await;
            if matches!(
                instances.get(&key).map(|i| &i.state),
                Some(
                    DshWebStatusState::Starting
                        | DshWebStatusState::Running
                        | DshWebStatusState::Attached
                )
            ) {
                return self.status_for(identity).await;
            }
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
        let tail = new_error_tail();
        {
            let mut instances = self.instances.lock().await;
            instances.insert(
                key.clone(),
                DshInstance {
                    child: None,
                    port: None,
                    _home: home.clone(),
                    _started_at: Utc::now(),
                    last_activity: Utc::now(),
                    kind,
                    state: DshWebStatusState::Starting,
                    error_tail: tail.clone(),
                    dsh_version: None,
                    native_url: None,
                },
            );
        }

        let start_result = if identity.is_admin {
            self.start_operator(home.clone(), tail.clone()).await
        } else {
            self.start_tenant(app, identity, home.clone(), tail.clone())
                .await
        };

        match start_result {
            Ok(mut instance) => {
                instance.error_tail = tail;
                let mut instances = self.instances.lock().await;
                instances.insert(key, instance);
            }
            Err(err) => {
                self.record_stopped_error(&key, kind, home, err.to_string())
                    .await;
            }
        }
        self.status_for(identity).await
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

    async fn start_operator(&self, home: PathBuf, tail: ErrorTail) -> Result<DshInstance> {
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
            dsh_home: spawn_home,
            profile: DSH_NATIVE_WEB_PROFILE,
            materialize_profile: false,
            enrollment: None,
            daemon_url: None,
            scrub_provider_env: false,
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
            dsh_home: home.clone(),
            profile: DSH_WEB_PROFILE,
            materialize_profile: true,
            enrollment: Some(&credential.bearer()),
            daemon_url: Some(&self.config.daemon_url),
            scrub_provider_env: true,
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
                Json(serde_json::json!({ "error": err.to_string() })),
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

async fn response_from_reqwest(resp: reqwest::Response) -> Response {
    let status = resp.status();
    let headers = resp.headers().clone();
    let stream = resp
        .bytes_stream()
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err));
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

fn tenant_home_segment(id: &str) -> String {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return id.to_string();
    }
    let hash = Sha256::digest(id.as_bytes());
    let suffix = hash[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!("tenant-{suffix}")
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
}
