//! DSH web first-class companion-port support.
//!
//! This module owns the WEB half of the feature: the companion-port byte proxy
//! (which carries the ccteam auth boundary), the REST response shapes, and the
//! mapping from a web [`Identity`] to a runtime identity. The process half —
//! per-identity `dsh web` children, start claims, attach-if-detected, stderr
//! tails — lives in [`ccteam_harness::DshRuntimeManager`] so the DSH adapter
//! can share the SAME instance map: one identity, one runtime, by construction.
//!
//! These instances are intentionally NOT modelled as ccteam sessions. They are
//! local vendor web servers keyed by the authenticated web identity.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU16, Ordering},
    Arc,
};

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
use ccteam_core::identity::{is_tenant_owned, WEB_OWNER_PREFIX};
use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_harness::{
    DshEnrollmentResolver, DshRestTokenResolver, DshRuntimeIdentity, DshRuntimeManager,
    DshRuntimeState, DshRuntimeStatus,
};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use serde::Serialize;
use tower_http::compression::CompressionLayer;
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DshWebStatusState {
    Disabled,
    Stopped,
    Starting,
    Running,
    Attached,
}

impl From<DshRuntimeState> for DshWebStatusState {
    fn from(state: DshRuntimeState) -> Self {
        match state {
            DshRuntimeState::Disabled => Self::Disabled,
            DshRuntimeState::Stopped => Self::Stopped,
            DshRuntimeState::Starting => Self::Starting,
            DshRuntimeState::Running => Self::Running,
            DshRuntimeState::Attached => Self::Attached,
        }
    }
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

/// Build the DSH runtime manager with ccteam's enrollment resolver injected.
///
/// The ONE construction site: `ccteam-harness` sits below `ccteam-core::enroll`
/// (core depends on harness), so the tenant bearer is resolved through this
/// closure. Returned unconfigured — `serve` calls `configure` once it knows the
/// daemon's own port, which is why `ccteam start` can build the manager in its
/// composition root long before the web listener binds.
pub fn new_runtime_manager(ccteam_home: PathBuf) -> Arc<DshRuntimeManager> {
    Arc::new(DshRuntimeManager::new(
        ccteam_home,
        enrollment_resolver(),
        rest_token_resolver(),
    ))
}

fn enrollment_resolver() -> DshEnrollmentResolver {
    Arc::new(|root, owner| {
        ensure_user_credential_in(root, owner)
            .map(|credential| credential.bearer())
            .with_context(|| format!("ensure enrollment credential for {owner}"))
    })
}

fn rest_token_resolver() -> DshRestTokenResolver {
    Arc::new(|root, owner| {
        rest_token_for_owner(root, owner)
            .with_context(|| format!("resolve ccteam REST token for {owner}"))
    })
}

/// The identity's OWN ccteam REST bearer, in the wire form the API accepts.
///
/// Reuse before mint, through the existing stores — no new token kind and no
/// new ACL surface: a tenant's `web_token` is the very token they signed in
/// with, and the operator's is the admin token file `ccteam start` already
/// manages. Minting only happens where the store itself would mint: an unknown
/// tenant is an error (this must never create a user), and the admin file is
/// get-or-mint exactly as elsewhere.
///
/// The returned value is a credential: it is written only into that identity's
/// own DSH profile and never logged or reported.
fn rest_token_for_owner(root: &Path, owner: &str) -> Result<String> {
    let paths = CcteamPaths {
        root: root.to_path_buf(),
        // Unused by the `secrets/` accessors below and never escapes here.
        projects_root: root.join("projects"),
    };
    let hex = match owner
        .strip_prefix(WEB_OWNER_PREFIX)
        .filter(|_| is_tenant_owned(Some(owner)))
    {
        Some(id) => {
            let dir = paths.users_dir();
            let mut registry = TenantRegistry::load(&dir);
            let existing = registry
                .by_id(id)
                .map(|tenant| tenant.web_token.clone())
                .ok_or_else(|| anyhow!("no ccteam user {id}"))?;
            if existing.trim().is_empty() {
                let minted = registry
                    .rotate_token(id)
                    .ok_or_else(|| anyhow!("no ccteam user {id}"))?;
                registry.save_one(&dir, id)?;
                minted
            } else {
                existing
            }
        }
        None => crate::token::generate_or_load_token(&paths.web_token_path())?,
    };
    Ok(format!("{}{hex}", crate::auth::TOKEN_PREFIX))
}

/// The web face of the DSH runtime: identity mapping, the companion port, and
/// the REST response shapes. Process supervision is delegated wholesale to the
/// shared [`DshRuntimeManager`].
#[derive(Debug)]
pub struct DshWebSupervisor {
    runtime: Arc<DshRuntimeManager>,
    companion_port: AtomicU16,
}

impl DshWebSupervisor {
    pub fn new(runtime: Arc<DshRuntimeManager>) -> Self {
        Self {
            runtime,
            companion_port: AtomicU16::new(0),
        }
    }

    /// The shared process core, for consumers that speak to DSH directly.
    pub fn runtime(&self) -> &Arc<DshRuntimeManager> {
        &self.runtime
    }

    pub fn set_companion_addr(&self, addr: SocketAddr) {
        self.companion_port.store(addr.port(), Ordering::SeqCst);
    }

    pub fn companion_port(&self) -> Option<u16> {
        if !self.runtime.enabled() {
            return None;
        }
        match self.companion_port.load(Ordering::SeqCst) {
            0 => None,
            p => Some(p),
        }
    }

    pub async fn status_for(&self, identity: &Identity) -> DshStatusResponse {
        let status = self.runtime.status(&runtime_identity(identity)).await;
        self.respond(identity, status)
    }

    pub async fn start_for(&self, identity: &Identity) -> DshStatusResponse {
        let status = self.runtime.start(&runtime_identity(identity)).await;
        self.respond(identity, status)
    }

    pub async fn stop_for(&self, identity: &Identity) -> DshStatusResponse {
        let status = self.runtime.stop(&runtime_identity(identity)).await;
        self.respond(identity, status)
    }

    pub async fn proxy_target_for(&self, identity: &Identity) -> Result<ProxyTarget> {
        if !self.runtime.enabled() {
            return Err(anyhow!("DSH web companion listener is disabled"));
        }
        let port = self.runtime.port_for(&runtime_identity(identity)).await?;
        Ok(ProxyTarget { port })
    }

    pub async fn shutdown_all(&self) {
        self.runtime.shutdown_all().await;
    }

    fn respond(&self, identity: &Identity, status: DshRuntimeStatus) -> DshStatusResponse {
        DshStatusResponse {
            state: status.state.into(),
            port: status.port,
            companion_port: self.companion_port(),
            home_kind: home_kind(identity),
            dsh_version: status.dsh_version,
            error_tail: status.error_tail,
            // The native (loopback-only) window is an operator affordance: a
            // tenant never learns a URL that bypasses the ccteam auth boundary.
            native_url: if identity.is_admin {
                status.native_url
            } else {
                None
            },
        }
    }
}

/// A web identity as the process core sees it.
fn runtime_identity(identity: &Identity) -> DshRuntimeIdentity {
    DshRuntimeIdentity {
        owner_tag: identity.owner_tag(),
        id: identity.id.clone(),
        operator: identity.is_admin,
    }
}

fn home_kind(identity: &Identity) -> DshHomeKind {
    if identity.is_admin {
        DshHomeKind::Own
    } else {
        DshHomeKind::Managed
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
    let target = match app.dsh_web.proxy_target_for(&identity).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel's token is REUSED, never a second credential: what lands in a
    /// tenant's DSH profile must be the very token they sign in with, in the
    /// wire form the REST API accepts.
    ///
    /// Root-injected, so no env pinning is needed — nothing here can reach the
    /// real `~/.ccteam`.
    #[test]
    fn tenant_rest_token_is_the_one_the_tenant_already_has() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let users = root.join("secrets").join("users");
        let mut registry = TenantRegistry::default();
        let tenant = registry.add("alice");
        registry.save(&users).unwrap();

        let resolved = rest_token_for_owner(root, &format!("user:{}", tenant.id)).unwrap();
        assert_eq!(resolved, format!("ccteam:{}", tenant.web_token));
        assert_eq!(
            rest_token_for_owner(root, &format!("user:{}", tenant.id)).unwrap(),
            resolved,
            "resolving twice must not rotate the tenant's login token"
        );
    }

    /// An identity with no user row is an error, never a silently minted new
    /// user: this resolver runs on a spawn path, not an admin one.
    #[test]
    fn unknown_tenant_is_refused_rather_than_created() {
        let tmp = tempfile::tempdir().unwrap();
        let err = rest_token_for_owner(tmp.path(), "user:u00000000").unwrap_err();
        assert!(err.to_string().contains("u00000000"), "got {err}");
        assert!(
            !tmp.path().join("secrets").join("users").exists(),
            "a failed lookup must not create a user store"
        );
    }

    /// Operator-ish tags all collapse to the admin token file, get-or-mint —
    /// the same value `ccteam start` manages, not a new one.
    #[test]
    fn operator_rest_token_is_the_admin_web_token() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let first = rest_token_for_owner(root, "user:web-api").unwrap();
        let on_disk =
            std::fs::read_to_string(root.join("secrets").join("web-token")).expect("token file");
        assert_eq!(first, format!("ccteam:{}", on_disk.trim()));
        assert_eq!(
            rest_token_for_owner(root, "telegram:42").unwrap(),
            first,
            "every operator-ish tag resolves to the one admin token"
        );
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

    /// The REST shape is the SPA's contract: an unconfigured runtime must still
    /// answer `disabled` with no companion port and the caller's home kind.
    #[tokio::test]
    async fn disabled_runtime_keeps_the_documented_status_shape() {
        let supervisor = DshWebSupervisor::new(new_runtime_manager(PathBuf::from("/nonexistent")));
        let status = supervisor.status_for(&Identity::admin()).await;
        assert_eq!(status.state, DshWebStatusState::Disabled);
        assert_eq!(status.companion_port, None);
        assert_eq!(status.home_kind, DshHomeKind::Own);
        let tenant = supervisor
            .status_for(&Identity::tenant("alice".into()))
            .await;
        assert_eq!(tenant.home_kind, DshHomeKind::Managed);
    }
}
