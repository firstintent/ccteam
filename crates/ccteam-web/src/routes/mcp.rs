//! v0.9 T4 — Streamable HTTP MCP endpoint (`POST /mcp`).
//!
//! One JSON-RPC 2.0 message in → one JSON-RPC response out via
//! [`ccteam_im::mcp::McpDispatch`]. No SSE push (a `GET` is 405, which the
//! transport defines as "this server offers no server-initiated stream").
//!
//! **Auth — self-gated, bearer-only, and there is no admin tier.** This router
//! mounts OUTSIDE the web `auth_layer` (see `lib::router_with_state`): that
//! layer only understands the web-token family (`ccteam:<hex>` + cookies) and
//! would 401 a session bearer before this handler ran — which silently
//! downgraded every managed session's A2A call to an admin fallback and dropped
//! the delegation parent (fixed v0.9.2). [`require_mcp_auth`] is the single
//! gate, and it resolves exactly TWO families — a credential that PARSES as one
//! is answered by that family or 401'd, never retried as another:
//!
//! 1. session principal `ccteam-sid:<sid>:<secret>` → the caller IS that
//!    managed session: Ambient with the FULL caller identity injected (the
//!    delegation-parent edge)
//! 2. enrollment credential `ccteam-enroll:<id>:<secret>` → a hand-started or
//!    external client, whose per-process identity this route issues (see below)
//!
//! **Why the data plane has no admin tier.** The web token used to be a third
//! family here, and it was written into all five vendors' GLOBAL MCP configs so
//! that any hand-started agent could orchestrate. But a vendor's global config
//! is one static file shared by every process that vendor ever starts, so a
//! durable credential written into it cannot say WHICH caller is speaking.
//! Measured consequence: two `codex` runs in different repos authenticated as
//! the same machine-wide caller, so neither could be a delegation parent and
//! their `agent` children mounted as ROOTS in a project nobody had
//! named. A credential a static file can carry must therefore grant nothing by
//! itself — which is why the tier is *deleted* rather than narrowed: an
//! identity-free caller has no correct amount of authority. The web token still
//! authenticates the REST/web control plane (`/api/v1/**`, cookies, the SPA),
//! where it names a browser session rather than a process.
//!
//! A bearer is ALWAYS required — even when `AuthState.enabled == false`
//! (loopback / `--no-auth`): DNS-rebinding / local-script hardening; curated
//! per-session configs and external clients always hold a token. Cookies
//! never authenticate `/mcp`.
//!
//! ## The enrollment family is the one STATEFUL path
//!
//! An enrollment credential says only *whose* the config is
//! ([`ccteam_core::enroll`]) and carries no authority of its own — that is what
//! makes it safe to leave in a static file. The per-PROCESS identity is issued
//! HERE, at `initialize`, as the transport's own `Mcp-Session-Id`:
//!
//! ```text
//! initialize + enroll bearer            -> open a binding, mint its ledger node,
//!                                          answer with Mcp-Session-Id
//! any later request + enroll bearer
//!                  + Mcp-Session-Id     -> that node's identity, injected exactly
//!                                          like a managed session's
//! DELETE, or the idle sweep             -> binding + node closed; the next call 404s
//!                                          and the client re-initializes
//! ```
//!
//! The node is a real sid with a real `meta.json` (`managed_by: external`) that
//! authenticates like any managed session — which is why nothing downstream
//! needed a new code path for it. What it never gets is a project ccteam
//! GUESSED. A binding's workspace comes from exactly one of three rungs, in
//! order:
//!
//! 1. the credential's own pinned scope ([`EnrollScope::Project`]);
//! 2. a project **named on a `session_*` call** whose owner may see it
//!    ([`bind_named_project`]) — the client says the word, ccteam checks it with
//!    the same predicate the REST choke point uses, and the binding keeps it for
//!    life;
//! 3. otherwise refuse, naming the projects this credential could reach.
//!
//! Rung 2 is what makes the machine-wide credential `ccteam config` writes into
//! every vendor's global config usable at all: that credential names no project
//! by construction (it is one file shared by every process), so without a
//! client-driven way to name one, a hand-started agent could only ever discover
//! its tool face. It is still not inference — an unnamed project stays refused,
//! and a named one the owner cannot see is answered exactly like one that does
//! not exist.

use std::sync::Arc;

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use ccteam_core::enroll::{self, EnrollCredential, ENROLL_BEARER_PREFIX};
use ccteam_im::native_bindings::{NativeBinding, NativeBindings};

use crate::auth::Identity;
use crate::state::AppState;

/// The transport's session header, lower-cased as [`HeaderName`] requires.
/// Real-machine verified: claude, codex, grok, opencode and kimi all echo it on
/// `notifications/initialized`, the SSE `GET`, `tools/list` and `tools/call`.
const MCP_SESSION_ID: &str = "mcp-session-id";

/// How often the idle sweep runs.
const BINDING_SWEEP_PERIOD: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a binding may go without a request before that sweep closes it.
///
/// Only codex and grok were observed sending a closing `DELETE`, so idle
/// eviction is the primary reaper. The window is deliberately generous: a
/// human-driven agent can sit idle for hours between tool calls, and reaping one
/// early costs it the ledger node that did its work (its next call 404s, it
/// re-initializes, and its children mount under a NEW node).
const BINDING_MAX_IDLE_SECS: i64 = 2 * 60 * 60;

/// Mount `POST|GET|DELETE /mcp`.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/mcp",
        post(handle_post).get(no_sse_stream).delete(handle_delete),
    )
}

/// No server-initiated stream at this endpoint. 405 is the transport's own
/// answer for that (clients treat it as "poll-only"), so this is a protocol
/// statement rather than a missing feature.
async fn no_sse_stream() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": "method not allowed: this MCP endpoint offers no SSE stream (POST for requests, DELETE to end an enrolled session)"
        })),
    )
        .into_response()
}

/// `POST /mcp` — body = one JSON-RPC 2.0 message.
async fn handle_post(
    State(app): State<AppState>,
    PeerAddr(peer): PeerAddr,
    headers: HeaderMap,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(auth) = require_mcp_auth(&app, &headers) else {
        return unauthorized();
    };
    if let Some(refusal) = refuse_unsupported_protocol_header(&headers) {
        return refusal;
    }

    let mut req = match body {
        Ok(Json(v)) => v,
        Err(err) => {
            // JSON-RPC-over-HTTP: parse errors are JSON-RPC -32700 with HTTP 200
            // (same convention as the mcp.sock line handler).
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!("parse error: {err}"),
                    },
                })
                .to_string(),
            )
                .into_response();
        }
    };

    match auth {
        // The enrollment family owns the whole request cycle (open a binding on
        // `initialize`, resolve it on everything after), so it takes the request
        // rather than just a caller tier.
        McpAuth::Enroll { credential } => {
            handle_enroll_post(&app, &credential, &headers, req, peer).await
        }
        // Session bearer `ccteam-sid:<sid>:<secret>` → Ambient path with the FULL
        // caller identity injected (_caller_sid/_caller_secret/_caller_role/
        // _caller_slug) so session_* principal auth matches the live session
        // (v0.9.0 W1 G4 — previously only role+secret were injected, so
        // session_* over HTTP failed closed with "no project scope").
        McpAuth::Session {
            sid,
            role,
            secret,
            slug,
            may_invoke_tools,
        } => {
            // A session that is still spawning is not a session yet: nothing
            // can be dispatched to it and it must not be able to spawn or stop
            // anybody. It only needs discovery (`initialize` / `tools/list`) to
            // finish building its tool face, so the window where a secret
            // exists for a session that may never come to life carries no
            // authority to act — by construction, not by cleanup timing.
            if !may_invoke_tools && is_tool_call(&req) {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({
                        "jsonrpc": "2.0",
                        "id": req.get("id").cloned().unwrap_or(Value::Null),
                        "error": {
                            "code": -32000,
                            "message": format!(
                                "session {sid} is still starting: its tool face can be listed but not called yet"
                            ),
                        },
                    })
                    .to_string(),
                )
                    .into_response();
            }
            inject_session_caller(&mut req, &sid, &role, &secret, &slug);
            dispatch_json_rpc(
                &app,
                req,
                ccteam_im::mcp::McpCaller::Ambient,
                &format!("session:{sid}"),
            )
            .await
        }
    }
}

/// The transport's own version gate: a client that DECLARES a protocol
/// revision this server does not speak is told so at the HTTP layer, before a
/// body is interpreted under assumptions neither side shares. An absent header
/// passes — `initialize` negotiates instead (and a conforming client sends the
/// header only after that handshake).
fn refuse_unsupported_protocol_header(headers: &HeaderMap) -> Option<Response> {
    // A PRESENT header must name a version this server speaks. Non-UTF8 and
    // empty values are present-but-invalid → 400, not silently "absent"
    // (absence is the one lenient case: the client then negotiates at
    // `initialize`).
    let declared = headers
        .get("mcp-protocol-version")?
        .to_str()
        .map(str::trim)
        .unwrap_or("");
    if ccteam_im::mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&declared) {
        return None;
    }
    Some(
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "unsupported MCP-Protocol-Version {declared}; supported: {}",
                    ccteam_im::mcp::SUPPORTED_PROTOCOL_VERSIONS.join(", ")
                )
            })),
        )
            .into_response(),
    )
}

/// Run one JSON-RPC message through the dispatcher and shape the HTTP answer.
/// One home for the transport convention (200 + JSON body for a request, 202 for
/// a notification) so every credential family answers identically.
async fn dispatch_json_rpc(
    app: &AppState,
    req: Value,
    caller: ccteam_im::mcp::McpCaller,
    tier: &str,
) -> Response {
    let dispatch = app.mcp_dispatch();
    // Log AFTER the dispatch: the one fact worth recording about a
    // `tools/list` is how big the face it answered with was, and that only
    // exists once the daemon has composed it.
    let answer = dispatch.dispatch_as(req.clone(), caller).await;
    log_tier_call(tier, &req, answer.as_ref());
    match answer {
        Some(response) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            response.to_string(),
        )
            .into_response(),
        // Notifications (e.g. notifications/initialized) → 202 empty.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Record WHICH identity a tool call arrived with — the one `POST /mcp`
/// tool-call log line.
///
/// The identity a request authenticates as is the single fact that decides its
/// delegation parent, its project scope and what it may reach, and it used to be
/// the one fact nothing wrote down: a managed session whose vendor loaded the
/// wrong `ccteam` MCP entry silently served its calls as the machine-wide admin
/// bearer, and the only evidence was a delegation tree missing an edge. That
/// specific downgrade is now impossible (no admin tier exists to fall back to),
/// but the question it hid is not — a session that reaches the wrong config now
/// shows up as `enroll:<id>` or 401s, and either way the log answers "who was
/// this?" by being read rather than reconstructed.
///
/// Callers pass the tier they resolved (`session:<sid>` for a managed session or
/// an enrolled client's ledger node, `enroll:<id>` for a binding that has none)
/// — an enrolled client is never logged as the identity that wrote its config
/// file, which is the whole point of that family.
///
/// Discovery is logged too. A tool face is now composed PER CALLER, so "which
/// tools did this session actually get" is a question with a different answer
/// per session — and the only moment it is decided is `initialize` /
/// `tools/list`. Unlogged, a leaf that came up with the wrong face would leave
/// no evidence at all; `tools = N` is that evidence.
fn log_tier_call(tier: &str, req: &Value, response: Option<&Value>) {
    if is_tool_call(req) {
        let tool = called_tool(req).unwrap_or("?");
        tracing::info!(%tier, %tool, "ccteam-web: POST /mcp tool call");
        return;
    }
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    if !matches!(method, "initialize" | "tools/list") {
        return;
    }
    let tools = response
        .and_then(|response| response.pointer("/result/tools"))
        .and_then(Value::as_array)
        .map(Vec::len);
    tracing::info!(%tier, %method, ?tools, "ccteam-web: POST /mcp discovery");
}

/// Who authenticated against `POST /mcp`. Two families, no admin tier — see the
/// module doc for why a credential a static config file can carry must grant
/// nothing by itself.
enum McpAuth {
    Session {
        sid: String,
        role: String,
        secret: String,
        slug: String,
        /// `false` while the session is still spawning — it may DISCOVER its
        /// tool face (`initialize`, `tools/list`) but not call anything with
        /// it. See [`ccteam_im::principals`].
        may_invoke_tools: bool,
    },
    /// A hand-started client's config credential. It names an identity and
    /// (optionally) one project; the per-process identity is issued at
    /// `initialize` and carried by `Mcp-Session-Id`.
    Enroll { credential: EnrollCredential },
}

/// Enforce bearer always (this route mounts outside `auth_layer`, so this is
/// the ONLY gate). Accepts exactly two families:
/// - session principal `ccteam-sid:<sid>:<secret>` (curated per-session MCP → Ambient)
/// - enrollment `ccteam-enroll:<id>:<secret>` (a hand-started client's config)
///
/// Anything else — including a valid web-console token — is 401. Each family is
/// CLAIMED by its prefix: a bearer that claims one and fails it is 401, never
/// retried as another. A downgrade between families is exactly how a managed
/// session silently became an admin caller (v0.9.2), so the families must not be
/// able to cover for each other.
///
/// **Synchronous on purpose.** Both families are verified from a registry lock or
/// one credential file read: no gateway lock, no await. A vendor's `initialize`
/// handshake therefore can never queue behind a spawn (the Pi deadlock,
/// 2026-08-09), and the gate cannot grow a slow path by accident.
///
/// `None` = refuse; the caller answers [`unauthorized`]. Returning the identity
/// rather than a response keeps the refusal's wording in ONE place instead of
/// travelling as an error value through every caller.
fn require_mcp_auth(app: &AppState, headers: &HeaderMap) -> Option<McpAuth> {
    let tok = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_bearer_value)?;

    // Session-scoped bearer first (works regardless of AuthState.enabled).
    if let Some((sid, secret)) = parse_session_bearer(tok) {
        return verify_session_bearer(app, &sid, &secret);
    }
    // Enrollment credential: verified against the on-disk record, whose owner +
    // scope are the only identity facts that matter.
    if tok.starts_with(ENROLL_BEARER_PREFIX) {
        return enroll::verify_in(&app.paths.root, tok)
            .map(|credential| McpAuth::Enroll { credential });
    }
    None
}

/// The one 401 this endpoint ever answers. It names both accepted families and
/// where a credential comes from: an operator who reaches here is holding the
/// wrong KIND of credential, and "auth required" alone would send them looking
/// for a longer token instead of a different one.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "auth required: POST /mcp accepts two credential families — \
                      `Authorization: Bearer ccteam-sid:<sid>:<secret>` (a ccteam-managed \
                      session's own principal, written into its curated MCP config at spawn) \
                      or `Authorization: Bearer ccteam-enroll:<id>:<secret>` plus the \
                      `Mcp-Session-Id` from `initialize` (any other client: `ccteam config` \
                      writes one into each vendor's global config, and the web console's \
                      project page mints a project-scoped one via \
                      `POST /api/v1/projects/{slug}/enroll`). A web-console token \
                      (`ccteam:<hex>`) authenticates `/api/v1/**`, never this endpoint."
        })),
    )
        .into_response()
}

/// Is this request an attempt to CALL a tool (as opposed to discovering the
/// tool face)? Deliberately positive: anything that is not a recognized
/// `tools/call` counts as discovery, and the only methods a spawning session
/// needs are `initialize`, `notifications/initialized` and `tools/list`.
fn is_tool_call(req: &Value) -> bool {
    req.get("method").and_then(Value::as_str) == Some("tools/call")
}

/// The tool a `tools/call` names, `None` for anything else.
fn called_tool(req: &Value) -> Option<&str> {
    if !is_tool_call(req) {
        return None;
    }
    req.pointer("/params/name").and_then(Value::as_str)
}

/// Parse `ccteam-sid:<sid>:<secret>` → (sid, secret).
fn parse_session_bearer(tok: &str) -> Option<(String, String)> {
    let rest = tok.strip_prefix("ccteam-sid:")?;
    let (sid, secret) = rest.split_once(':')?;
    if sid.is_empty() || secret.is_empty() {
        return None;
    }
    Some((sid.to_string(), secret.to_string()))
}

/// Resolve `ccteam-sid:<sid>:<secret>` against the principal REGISTRY.
///
/// v0.9.0 W1 (F1/G4) — role/slug come from the matched principal, never the
/// client; an empty secret / unknown sid → `None`, which the gate answers as 401.
///
/// **No gateway lock.** This used to lock the gateway to read a session's
/// secret, which made a credential check wait on whatever else held that lock
/// — including the very spawn whose vendor was making this call. Pi deadlocked
/// exactly there (2026-08-09): IM `/new` held the lock across `start_thread`,
/// the bridge's `session_start` blocked on this request, and the child never
/// reached the point of reading its stdin. The registry has its own lock, so
/// no spawn path can starve the handshake it is waiting for.
fn verify_session_bearer(app: &AppState, sid: &str, secret: &str) -> Option<McpAuth> {
    let matched = app.session_principals.as_ref()?.verify(sid, secret)?;
    Some(McpAuth::Session {
        sid: matched.sid,
        role: matched.role,
        secret: secret.to_string(),
        slug: matched.slug,
        may_invoke_tools: ccteam_im::principals::may_invoke_tools(matched.state),
    })
}

/// The `params.arguments` object, created if the message has none.
///
/// Every method gets one now, not just `tools/call`: `initialize` and
/// `tools/list` are where the daemon composes this caller's TOOL FACE, and it
/// cannot do that without knowing who is asking. A message that carries no
/// `params` at all (which `initialize` and `tools/list` legitimately may)
/// would otherwise arrive anonymous and be served the full surface.
fn caller_args(req: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    if !req.get("params").is_some_and(Value::is_object) {
        req.as_object_mut()?.insert("params".into(), json!({}));
    }
    let params = req.get_mut("params")?.as_object_mut()?;
    if !params.get("arguments").is_some_and(Value::is_object) {
        params.insert("arguments".into(), json!({}));
    }
    params.get_mut("arguments")?.as_object_mut()
}

/// Inject the FULL caller identity (`_caller_sid` / `_caller_secret` /
/// `_caller_role` / `_caller_slug`) so the Ambient PRINCIPAL gate sees the
/// curated session's identity. All four are OVERWRITTEN (never trust a
/// caller-supplied value); the daemon re-verifies `(sid, secret)` and
/// re-derives slug/role from CallerCtx.
fn inject_session_caller(req: &mut Value, sid: &str, role: &str, secret: &str, slug: &str) {
    let Some(map) = caller_args(req) else {
        return;
    };
    map.insert("_caller_sid".into(), json!(sid));
    map.insert("_caller_secret".into(), json!(secret));
    map.insert("_caller_role".into(), json!(role));
    map.insert("_caller_slug".into(), json!(slug));
    // The local-socket admin fallback arg must never ride in over HTTP —
    // this transport has no admin tier at all (see the module doc); it
    // authenticates a session principal or an enrolled client's node.
    map.remove("_caller_admin_token");
    map.remove("_enroll_reachable");
}

/// Tell the daemon which projects an enrolled binding with NO ledger node yet
/// could name, so `initialize.instructions` can list them instead of leaving
/// the agent to discover its first refusal.
fn inject_enroll_reachable(req: &mut Value, reachable: Vec<String>) {
    let Some(map) = caller_args(req) else {
        return;
    };
    map.remove("_caller_sid");
    map.remove("_caller_secret");
    map.remove("_caller_role");
    map.remove("_caller_slug");
    map.remove("_caller_admin_token");
    map.insert("_enroll_reachable".into(), json!(reachable));
}

/// The connecting peer's address, when the serving stack was built with
/// `into_make_service_with_connect_info` (the production `serve()` path is).
/// A router mounted without it — unit tests, embedded uses — yields `None`,
/// and provenance is simply skipped: absence of the fact must degrade to the
/// enrollment ladder, never to a 500 on every `/mcp` request.
struct PeerAddr(Option<SocketAddr>);

impl<S> axum::extract::FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|ci| ci.0),
        ))
    }
}

/// Chomp `Bearer ` → the wire token (`ccteam:<hex>`).
fn parse_bearer_value(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("Bearer ")?;
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// =====================================================================
// Enrollment family — one identity per PROCESS, issued at `initialize`
// =====================================================================

/// `POST /mcp` under an enrollment credential.
///
/// `initialize` mints the identity; every later method must present it. There is
/// no third case on purpose: a request that carries no `Mcp-Session-Id` has no
/// process identity, and the alternatives (infer from a client-supplied path, the
/// peer address, the most recent project) are precisely the guessing this
/// mechanism exists to delete.
async fn handle_enroll_post(
    app: &AppState,
    credential: &EnrollCredential,
    headers: &HeaderMap,
    mut req: Value,
    peer: Option<SocketAddr>,
) -> Response {
    if req.get("method").and_then(Value::as_str) == Some("initialize") {
        return open_binding(app, credential, req, peer).await;
    }
    let Some(binding) = resolve_binding(app, credential, headers) else {
        return no_such_mcp_session(&req);
    };
    // Provenance retry for a binding still on the ladder: the eager attempt at
    // `initialize` can miss (pid recorded a beat late, `/proc` transiently
    // unreadable), and an unbound binding is the only state worth re-checking.
    let binding = if binding.principal().is_none()
        && try_provenance_attach(app, peer, credential, &binding.mcp_session_id).await
    {
        match resolve_binding(app, credential, headers) {
            Some(rebound) => rebound,
            None => return no_such_mcp_session(&req),
        }
    } else {
        binding
    };
    // Rung 2 of the project ladder, resolved BEFORE the identity is read: a
    // `session_*` call that NAMES a project may turn a nodeless binding into a
    // ledger node, and on a binding that already has one the naming is checked
    // against it rather than silently ignored.
    let binding = match bind_named_project(app, credential, binding, &req).await {
        Ok(binding) => binding,
        Err(refusal) => return *refusal,
    };
    match binding.principal() {
        // The binding speaks for a session — a node it minted, or a managed
        // session provenance attached. Verify against the live registry on
        // every call: slug/role are the PRINCIPAL's (a provenance target's
        // project is the session's own, not the credential's), a spawning
        // session is held to discovery-only exactly like the session-bearer
        // path, and a principal that stopped verifying means the session
        // behind it ended — the binding is stale, and the transport's own
        // re-initialize signal is the recovery.
        Some((sid, secret)) => {
            let verified = app
                .session_principals
                .as_ref()
                .and_then(|principals| principals.verify(sid, secret));
            let Some(matched) = verified else {
                close_binding(app, &binding.mcp_session_id).await;
                return no_such_mcp_session(&req);
            };
            if !ccteam_im::principals::may_invoke_tools(matched.state) && is_tool_call(&req) {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({
                        "jsonrpc": "2.0",
                        "id": req.get("id").cloned().unwrap_or(Value::Null),
                        "error": {
                            "code": -32000,
                            "message": format!(
                                "session {sid} is still starting: its tool face can be listed but not called yet"
                            ),
                        },
                    })
                    .to_string(),
                )
                    .into_response();
            }
            inject_session_caller(&mut req, sid, &matched.role, secret, &matched.slug);
            dispatch_json_rpc(
                app,
                req,
                ccteam_im::mcp::McpCaller::Ambient,
                &format!("session:{sid}"),
            )
            .await
        }
        None => {
            if let Some(refusal) = refuse_projectless_call(app, credential, &binding, &req) {
                return refusal;
            }
            // No ledger node yet: the daemon still needs to know WHO is asking
            // so `initialize` can tell this agent which projects it may name.
            inject_enroll_reachable(&mut req, addressable_projects(app, &credential.owner));
            dispatch_json_rpc(
                app,
                req,
                ccteam_im::mcp::McpCaller::Ambient,
                &format!("enroll:{}", credential.id),
            )
            .await
        }
    }
}

/// `initialize` — issue this process its own identity.
///
/// The binding is opened first and unconditionally: even when no ledger node can
/// be created, the client gets an id, so its later calls arrive with something
/// resolvable and are refused with a REASON instead of being indistinguishable
/// from a client that never initialized.
async fn open_binding(
    app: &AppState,
    credential: &EnrollCredential,
    mut req: Value,
    peer: Option<SocketAddr>,
) -> Response {
    let client = client_label(&req);
    let id = app.native_bindings.open(
        &credential.id,
        &credential.owner,
        credential.scope.project().map(str::to_string),
        &client,
    );
    // Provenance first: when the connecting process IS a managed session's
    // own, the binding is that session and nothing may mint a second identity
    // for it — that second identity is exactly the ghost node the measured
    // grok collision used to leave behind.
    let attached = try_provenance_attach(app, peer, credential, &id).await;
    if attached {
        // fall through to answer `initialize` with the id
    } else if let Some(slug) = credential.scope.project() {
        match mint_ledger_node(app, credential, slug, &client).await {
            Ok((sid, secret)) => {
                // The secret never leaves this process: the client authenticates
                // with its enroll bearer + id, and the daemon speaks for it
                // internally with the principal it minted here. Nothing else can
                // hold this id yet (it was issued three statements ago), so the
                // attach cannot lose a race and the in-force principal is ours.
                let _ = app.native_bindings.attach_session(&id, &sid, &secret, true);
                tracing::info!(
                    enroll = %credential.id, mcp_session = %id, sid = %sid,
                    project = %slug, %client,
                    "POST /mcp: enrolled client is ledger node"
                );
            }
            Err(err) => tracing::warn!(
                enroll = %credential.id, mcp_session = %id, project = %slug,
                %client, reason = %err,
                "POST /mcp: enrolled client has no ledger node (session_* will fail closed)"
            ),
        }
    }
    // Whichever rung the binding landed on, `initialize` answers with a face
    // and instructions composed for it: a node speaks as its own principal, a
    // still-unbound binding is told which projects it may name.
    let tier = match app
        .native_bindings
        .resolve(&id, &credential.id)
        .and_then(|binding| {
            binding
                .principal()
                .map(|(sid, secret)| (sid.to_string(), secret.to_string(), binding.clone()))
        }) {
        Some((sid, secret, binding)) => {
            let matched = app
                .session_principals
                .as_ref()
                .and_then(|principals| principals.verify(&sid, &secret));
            let (role, slug) = matched
                .map(|matched| (matched.role, matched.slug))
                .unwrap_or_else(|| (String::new(), binding.project.clone().unwrap_or_default()));
            inject_session_caller(&mut req, &sid, &role, &secret, &slug);
            format!("session:{sid}")
        }
        None => {
            inject_enroll_reachable(&mut req, addressable_projects(app, &credential.owner));
            format!("enroll:{}", credential.id)
        }
    };
    let mut response = dispatch_json_rpc(app, req, ccteam_im::mcp::McpCaller::Ambient, &tier).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(MCP_SESSION_ID), value);
    }
    response
}

/// Bind an enrolled binding to the MANAGED session whose process subtree the
/// connecting peer belongs to — identity by provenance.
///
/// The ACP dialect gives ccteam no lever to make a vendor prefer the
/// per-session MCP entry it was handed over a same-named machine-credential
/// entry in the vendor's own global config (measured: grok 1.0.0 resolves the
/// collision toward its config — `ccteam_harness::execution::mcp_config`).
/// The daemon SPAWNED that process, though. So when a loopback peer
/// authenticating with an enrollment credential turns out to be a managed
/// session's vendor process (or its descendant —
/// [`ccteam_harness::execution::vendor_pids`], recorded before the first
/// handshake byte, resolved through `/proc`), the binding is attached to that
/// session's own principal and the vendor's config choice stops deciding
/// identity. No ghost node is minted, children mount under the real session,
/// and the identity-degraded warning has nothing to say.
///
/// Refusals are quiet and fail open to the enrollment ladder: a non-loopback
/// peer, a non-linux host, a hand-started process, a stopped session — and a
/// credential PINNED to a different project, because an operator who scoped a
/// snippet said something and provenance must not overrule it into another
/// workspace.
async fn try_provenance_attach(
    app: &AppState,
    peer: Option<SocketAddr>,
    credential: &EnrollCredential,
    mcp_session_id: &str,
) -> bool {
    let Some(peer) = peer else {
        return false;
    };
    if !peer.ip().is_loopback() {
        return false;
    }
    let Some(principals) = app.session_principals.as_ref() else {
        return false;
    };
    let Some(sid) = ccteam_harness::execution::vendor_pids::owner_of_local_peer(peer.port()) else {
        return false;
    };
    let Some((secret, slug)) = principals.credential_for_managed_attach(&sid) else {
        return false;
    };
    if let Err(err) = app.native_bindings.bind_project(mcp_session_id, &slug) {
        tracing::warn!(
            mcp_session = %mcp_session_id, %sid, session_project = %slug, reason = %err,
            "POST /mcp: peer is a managed session's process but this credential is pinned elsewhere — leaving the binding on the enrollment ladder"
        );
        return false;
    }
    let in_force = app
        .native_bindings
        .attach_session(mcp_session_id, &sid, &secret, false);
    let attached = in_force.as_ref().map(|(held, _)| held.as_str()) == Some(sid.as_str());
    if attached {
        principals.mark_used(&sid);
        tracing::info!(
            enroll = %credential.id, mcp_session = %mcp_session_id, %sid, project = %slug,
            "POST /mcp: peer process is a managed session's own — binding attached to the session's principal, whichever config entry its vendor loaded"
        );
    }
    attached
}

/// Rung 2 of the project ladder: **the caller names its workspace.**
///
/// A `session_*` call carrying an explicit `project` is the client choosing where
/// it works, which is the one rung ccteam can honour without guessing: it is the
/// caller's own word, and the answer is [`may_bind_project`] — the same
/// visibility predicate rung 1 and the REST choke point use. Without it the
/// machine-wide credential in every vendor's global config could do nothing but
/// discovery, because a file shared by every process cannot name a project.
///
/// Runs for EVERY `session_*` call, bound or not, so the naming is honoured once
/// and enforced forever: [`NativeBindings::bind_project`] refuses a later switch,
/// which is what keeps one MCP session to one workspace for its whole life. A
/// call that names nothing, or is not a `session_*` call, passes through
/// untouched — discovery and `status` must keep working unbound.
///
/// `Ok` = the binding as it stands afterwards (freshly noded, or unchanged);
/// `Err` = the refusal to answer with (boxed: the `Err` variant must stay
/// small — `clippy::result_large_err`).
async fn bind_named_project(
    app: &AppState,
    credential: &EnrollCredential,
    binding: NativeBinding,
    req: &Value,
) -> Result<NativeBinding, Box<Response>> {
    let Some(tool) = called_tool(req) else {
        return Ok(binding);
    };
    // The session face is the only one that takes a workspace argument;
    // `status`/discovery name a project nowhere and must not bind one.
    if !ccteam_im::mcp::is_session_tool(tool) {
        return Ok(binding);
    }
    let Some(slug) = named_project(req) else {
        return Ok(binding);
    };
    if !may_bind_project(app, credential, &slug) {
        return Err(Box::new(mcp_tool_error(
            req,
            unaddressable_project(app, credential, tool, &slug),
        )));
    }
    // The registry is the arbiter of "one session, one workspace": a binding
    // already pinned elsewhere refuses HERE, before anything is minted.
    if let Err(err) = app
        .native_bindings
        .bind_project(&binding.mcp_session_id, &slug)
    {
        return Err(Box::new(mcp_tool_error(req, format!("{tool}: {err}"))));
    }
    if binding.principal().is_some() {
        // Already the node for this very project (that is what `bind_project`
        // just confirmed) — nothing to mint.
        return Ok(binding);
    }
    let (sid, secret) = mint_ledger_node(app, credential, &slug, &binding.client)
        .await
        .map_err(|err| {
            Box::new(mcp_tool_error(
                req,
                format!("{tool}: cannot work in `{slug}`: {err}"),
            ))
        })?;
    // Attach ONCE: a client may fire two tool calls in parallel and both would
    // see a nodeless binding. The registry decides which node the binding keeps;
    // the loser retires the one it minted instead of overwriting an identity that
    // children may already hang off.
    let in_force = app
        .native_bindings
        .attach_session(&binding.mcp_session_id, &sid, &secret, true);
    if in_force.as_ref().map(|(held, _)| held.as_str()) != Some(sid.as_str()) {
        retire_node(
            app.gateway.as_ref(),
            app.session_principals.as_ref(),
            std::slice::from_ref(&sid),
        )
        .await;
    }
    // Re-read rather than patch a local copy: the registry now holds the truth
    // (including a parallel caller's node), and a binding that vanished mid-flight
    // gets the transport's own re-initialize signal.
    let bound = app
        .native_bindings
        .resolve(&binding.mcp_session_id, &credential.id)
        .ok_or_else(|| Box::new(no_such_mcp_session(req)))?;
    tracing::info!(
        enroll = %credential.id, mcp_session = %binding.mcp_session_id,
        sid = bound.sid.as_deref().unwrap_or("-"), project = %slug,
        client = %binding.client, %tool,
        "POST /mcp: enrolled client named its project and is now a ledger node"
    );
    Ok(bound)
}

/// The `project` argument of a `tools/call`, trimmed, `None` when absent/empty.
/// Read verbatim from the caller: it is a NAME, never authority — what it may
/// mean is decided by [`may_bind_project`].
fn named_project(req: &Value) -> Option<String> {
    let slug = req
        .pointer("/params/arguments/project")
        .and_then(Value::as_str)?
        .trim();
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Whether this credential's owner may work in `slug` at all: the project exists
/// AND the REST ACL's own visibility predicate says yes.
///
/// One home for both rungs of the ladder. They MUST answer identically —
/// otherwise naming a project on a call would reach a workspace the credential
/// could never have been minted for, which is the same defect as an inferred
/// project wearing different clothes.
fn may_bind_project(app: &AppState, credential: &EnrollCredential, slug: &str) -> bool {
    app.paths.project_state(slug).exists()
        && crate::routes::api_v1::can_see_project(app, &identity_for_owner(&credential.owner), slug)
}

/// The ONE answer for a named project this credential cannot work in: unknown
/// here and invisible to its owner are deliberately the same text, so a caller
/// cannot probe for the existence of somebody else's workspace. Both halves
/// depend only on the OWNER, never on the probed slug's fate.
fn unaddressable_project(
    app: &AppState,
    credential: &EnrollCredential,
    tool: &str,
    slug: &str,
) -> String {
    let mut message = format!(
        "{tool}: project `{slug}` is not registered here, or not this credential owner's — \
         one answer for both on purpose."
    );
    match addressable_projects(app, &credential.owner) {
        projects if projects.is_empty() => message.push_str(
            " No project is registered for this credential's owner yet; create one in the web \
             console first.",
        ),
        projects => message.push_str(&format!(" It can name: {}.", projects.join(", "))),
    }
    message
}

/// Create the ledger node for a project a binding may work in, and mint the
/// principal it authenticates with.
///
/// Fail-closed on visibility through [`may_bind_project`] — the same predicate
/// the REST ACL choke point uses: the credential is long-lived and pasted into
/// config files, so "the operator could see this project when they minted it" is
/// not enough — it has to still be true now.
async fn mint_ledger_node(
    app: &AppState,
    credential: &EnrollCredential,
    slug: &str,
    client: &str,
) -> Result<(String, String), String> {
    if !may_bind_project(app, credential, slug) {
        return Err(format!(
            "project `{slug}` is not registered here (or not this credential owner's)"
        ));
    }
    let Some(gateway) = app.gateway.as_ref() else {
        return Err("no live gateway (standalone web has no session ledger)".to_string());
    };
    let Some(principals) = app.session_principals.as_ref() else {
        return Err("no principal registry (standalone web)".to_string());
    };
    // The one gateway-lock hold on this path, for one durable write. Waiting on
    // it is honest latency — a delayed but correct identity beats a fast wrong
    // one, which is what a rootless spawn into a guessed project was.
    let sid = {
        let mut gw = gateway.lock().await;
        gw.register_external_node(slug, &credential.owner, client)
            .map_err(|err| err.to_string())?
    };
    let secret = ccteam_core::session_secret::mint();
    // Live immediately: the node exists the moment it is registered, and its very
    // next request is the one that needs to authenticate as it. Roleless (`""`)
    // and depth 0 match the node's own `meta.json`.
    principals.promote(&sid, &secret, slug, "", 0);
    Ok((sid, secret))
}

/// Resolve the `Mcp-Session-Id` header against THIS credential.
///
/// A missing header, an unknown id and another credential's id are one outcome:
/// the id alone is not a credential, and answering them differently would let a
/// leaked id be probed for existence.
fn resolve_binding(
    app: &AppState,
    credential: &EnrollCredential,
    headers: &HeaderMap,
) -> Option<NativeBinding> {
    let id = headers.get(MCP_SESSION_ID)?.to_str().ok()?.trim();
    app.native_bindings.resolve(id, &credential.id)
}

/// 404 + JSON-RPC `-32001`: the transport's own recovery signal, which a
/// conforming client answers by running `initialize` again. Not a new protocol —
/// deliberately the one every MCP client already implements.
fn no_such_mcp_session(req: &Value) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "jsonrpc": "2.0",
            "id": req.get("id").cloned().unwrap_or(Value::Null),
            "error": {
                "code": -32001,
                "message": "no such MCP session: send `initialize` with your enrollment bearer and echo the `Mcp-Session-Id` it answers with on every later request",
            },
        })
        .to_string(),
    )
        .into_response()
}

/// Refuse a tool call from a binding that has no project, naming the ones it
/// could have. `None` = let it through.
///
/// Discovery (`initialize` / `notifications/initialized` / `tools/list`) and
/// `status` pass: a client must be able to see what exists and where it stands.
/// Everything else is withheld, because acting needs a workspace and ccteam will
/// not pick one — an inferred project is how a hand-started agent's children
/// ended up in a scratch repo nobody had named.
fn refuse_projectless_call(
    app: &AppState,
    credential: &EnrollCredential,
    binding: &NativeBinding,
    req: &Value,
) -> Option<Response> {
    let tool = called_tool(req)?;
    if matches!(tool, "status" | ccteam_im::mcp::STATUS_BEACON_TOOL_NAME) {
        return None;
    }
    let cause = match binding.project.as_deref() {
        // Pinned but unusable: the node could not be created (unregistered slug,
        // not this owner's, no gateway) and the reason is already in the log.
        Some(slug) => format!(
            "its enrollment credential names project `{slug}`, which this daemon cannot bind \
             (not registered here, or not yours)"
        ),
        None => "its enrollment credential is user-scoped, so it names no project".to_string(),
    };
    let mut message = format!(
        "{tool}: this MCP session has no project — {cause}. ccteam never infers one from your \
         working directory, your address or the most recent project."
    );
    let projects = addressable_projects(app, &credential.owner);
    if projects.is_empty() {
        message.push_str(
            " No project is registered for this credential's owner yet; create one in the web \
             console first.",
        );
        return Some(mcp_tool_error(req, message));
    }
    // Rung 2 is only open to a binding that has not committed yet: a pinned one
    // cannot move ([`NativeBindings::bind_project`]), so offering it a project to
    // name would be advice that gets refused.
    if binding.project.is_none() {
        message.push_str(&format!(
            " Name one on the call (`project: \"{}\"`) — the first project a session names is its \
             workspace for the rest of its life.",
            projects[0]
        ));
    }
    message.push_str(&format!(
        " Or ask for a project-scoped enrollment snippet (web console → the project → external \
         agent) for one of: {}.",
        projects.join(", ")
    ));
    Some(mcp_tool_error(req, message))
}

/// A tool-level failure, in the shape the AGENT reads: `isError: true` content on
/// an HTTP 200, exactly like every `session_*` refusal the dispatcher produces. A
/// JSON-RPC error envelope would be a transport fault, which this is not.
fn mcp_tool_error(req: &Value, message: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "jsonrpc": "2.0",
            "id": req.get("id").cloned().unwrap_or(Value::Null),
            "result": {
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            },
        })
        .to_string(),
    )
        .into_response()
}

/// Projects this credential's owner could be handed a scoped snippet for.
/// Ownership-filtered so the hint cannot enumerate another tenant's workspaces.
fn addressable_projects(app: &AppState, owner: &str) -> Vec<String> {
    let identity = identity_for_owner(owner);
    ccteam_core::collect_projects(&app.paths)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| identity.can_see_owner(p.state.owner.as_deref()))
        .map(|p| p.state.slug)
        .collect()
}

/// The web identity an enrollment credential speaks for.
///
/// Not a second policy: the stored owner tag is exactly what
/// [`Identity::owner_tag`] produces, so mapping it back yields the identity the
/// REST ACL would have used for that operator — the shared admin console pool or
/// one tenant. Anything else (an IM-owned tag, an empty one) is not a web
/// identity and gets a tenant that owns nothing, which fails closed everywhere.
fn identity_for_owner(owner: &str) -> Identity {
    match owner.strip_prefix(ccteam_core::identity::WEB_OWNER_PREFIX) {
        Some(id) if id == ccteam_core::identity::ADMIN_WEB_ID => Identity::admin(),
        Some(id) => Identity::tenant(id.to_string()),
        None => Identity::tenant(owner.to_string()),
    }
}

/// `clientInfo` from `initialize` → the node's label (`name/version`). Whatever
/// the client called itself is the only honest name for a process ccteam did not
/// start, so it is recorded verbatim and never used to decide anything.
fn client_label(req: &Value) -> String {
    let info = req.pointer("/params/clientInfo");
    let name = info
        .and_then(|i| i.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let version = info
        .and_then(|i| i.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    match (name.is_empty(), version.is_empty()) {
        (true, _) => String::new(),
        (false, true) => name.to_string(),
        (false, false) => format!("{name}/{version}"),
    }
}

/// `DELETE /mcp` — the client says it is done.
///
/// Only the enrollment family owns anything terminable here, so every other
/// credential gets the transport's "this server does not let clients end
/// sessions" answer instead of a misleading success.
async fn handle_delete(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let Some(auth) = require_mcp_auth(&app, &headers) else {
        return unauthorized();
    };
    let McpAuth::Enroll { credential } = auth else {
        return delete_not_supported();
    };
    // Resolve BEFORE closing so another credential's id cannot be terminated.
    let Some(binding) = resolve_binding(&app, &credential, &headers) else {
        return no_such_mcp_session(&Value::Null);
    };
    close_binding(&app, &binding.mcp_session_id).await;
    tracing::info!(
        enroll = %credential.id, mcp_session = %binding.mcp_session_id,
        sid = binding.sid.as_deref().unwrap_or("-"),
        "DELETE /mcp: enrolled client ended its session"
    );
    StatusCode::NO_CONTENT.into_response()
}

/// A `DELETE` from a credential family that holds no binding.
fn delete_not_supported() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({
            "error": "method not allowed: only an enrolled client (Authorization: Bearer ccteam-enroll:<id>:<secret> + Mcp-Session-Id) can end an MCP session here"
        })),
    )
        .into_response()
}

/// Drop a binding and everything it authorised.
async fn close_binding(app: &AppState, mcp_session_id: &str) {
    let Some(sid) = app.native_bindings.close(mcp_session_id) else {
        return;
    };
    retire_node(
        app.gateway.as_ref(),
        app.session_principals.as_ref(),
        &[sid],
    )
    .await;
}

/// Retire the ledger nodes of bindings that just ended: their principals stop
/// verifying and they leave the live views. Their `meta.json` deliberately stays,
/// like any stopped session's, so whatever they spawned keeps resolving to a real
/// parent.
///
/// Shared by the client's `DELETE` and the idle sweep on purpose — a binding must
/// end the same way whether the client said goodbye or simply vanished, which is
/// the far more common case.
async fn retire_node(
    gateway: Option<&Arc<tokio::sync::Mutex<ccteam_im::gateway::Gateway>>>,
    principals: Option<&Arc<ccteam_im::principals::SessionPrincipals>>,
    sids: &[String],
) {
    if sids.is_empty() {
        return;
    }
    if let Some(principals) = principals {
        for sid in sids {
            principals.forget(sid);
        }
    }
    if let Some(gateway) = gateway {
        let mut gw = gateway.lock().await;
        for sid in sids {
            gw.close_external_node(sid);
        }
    }
}

/// Spawn the ONE idle-sweep task for this gateway — called from
/// [`crate::state::AppState::with_gateway`], the same composition root that
/// spawns the ring feeders (see [`crate::ring::spawn_ring_feeder`] for the
/// "one persistent task per gateway" rationale).
///
/// This is the primary reaper, not a safety net: a hand-started agent usually
/// exits without sending `DELETE` (only codex and grok were observed sending one)
/// and nothing else would ever notice. There is no daemon tick to hang it on by
/// design — the daemon responds to messages and schedules, it does not poll — so
/// the sweep owns its timer, and it only ever reclaims resources.
pub(crate) fn spawn_binding_reaper(
    gateway: Arc<tokio::sync::Mutex<ccteam_im::gateway::Gateway>>,
    principals: Arc<ccteam_im::principals::SessionPrincipals>,
    bindings: Arc<NativeBindings>,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(BINDING_SWEEP_PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let closed = bindings.sweep_idle(chrono::Duration::seconds(BINDING_MAX_IDLE_SECS));
            for sid in &closed {
                tracing::info!(
                    %sid, idle_secs = BINDING_MAX_IDLE_SECS,
                    "ccteam-web: reaping an idle enrolled client's ledger node"
                );
            }
            retire_node(Some(&gateway), Some(&principals), &closed).await;
        }
    });
}
