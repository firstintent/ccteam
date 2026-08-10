//! v0.9 T4 — Streamable HTTP MCP endpoint (`POST /mcp`).
//!
//! One JSON-RPC 2.0 message in → one JSON-RPC response out via
//! [`ccteam_im::mcp::McpDispatch`]. No SSE push (a `GET` is 405, which the
//! transport defines as "this server offers no server-initiated stream").
//!
//! **Auth — self-gated, bearer-only.** This router mounts OUTSIDE the web
//! `auth_layer` (see `lib::router_with_state`): that layer only understands
//! the web token family (`ccteam:<hex>` + cookies) and would 401 a session
//! bearer before this handler ran — which silently downgraded every managed
//! session's A2A call to an admin fallback and dropped the delegation parent
//! (fixed v0.9.2). [`require_mcp_auth`] is the single gate, and it resolves the
//! bearer families in this order — a credential that PARSES as one family is
//! answered by that family or 401'd, never retried as another:
//!
//! 1. session principal `ccteam-sid:<sid>:<secret>` → Ambient with the FULL
//!    caller identity injected (the delegation-parent edge)
//! 2. enrollment credential `ccteam-enroll:<id>:<secret>` → see below
//! 3. admin web token `ccteam:<hex>` → [`McpAuth::Admin`] (owner front door;
//!    `session_spawn` is a root spawn by design)
//! 4. tenant web token `ccteam:<hex>` → [`McpAuth::User`] (project-scoped root
//!    caller; never promoted to admin or treated as a managed session)
//!
//! A bearer is ALWAYS required — even when `AuthState.enabled == false`
//! (loopback / `--no-auth`): DNS-rebinding / local-script hardening; curated
//! per-session configs and external clients always hold a token. Cookies
//! never authenticate `/mcp`.
//!
//! ## The enrollment family is the one STATEFUL path
//!
//! A hand-started vendor process reads a static global config, so whatever is
//! written there is shared by every process that vendor ever starts. Measured
//! consequence of putting the admin web token there: two `codex` runs in
//! different repos authenticated as the same machine-wide caller, so neither
//! could be a delegation parent and their `session_spawn` children mounted as
//! ROOTS in a project nobody had named. An enrollment credential therefore says
//! only *whose* the config is ([`ccteam_core::enroll`]) and carries no authority
//! of its own; the per-PROCESS identity is issued HERE, at `initialize`, as the
//! transport's own `Mcp-Session-Id`:
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
//! guessed: an unbound binding may discover its tool face and call `status`, and
//! every other tool fails closed naming the projects it could have.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use ccteam_core::enroll::{self, EnrollCredential, ENROLL_BEARER_PREFIX};
use ccteam_im::native_bindings::{NativeBinding, NativeBindings};

use crate::auth::{resolve_identity, Identity, TOKEN_PREFIX};
use crate::state::AppState;
use crate::token::generate_or_load_token;

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
    headers: HeaderMap,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let auth = match require_mcp_auth(&app, &headers).await {
        Ok(a) => a,
        Err(resp) => return resp,
    };

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

    // The enrollment family owns the whole request cycle (open a binding on
    // `initialize`, resolve it on everything after), so it branches before the
    // caller-enum mapping the static families share.
    if let McpAuth::Enroll { credential } = auth {
        return handle_enroll_post(&app, &credential, &headers, req).await;
    }

    // Web-family bearer → Admin or project-scoped User. Session bearer
    // `ccteam-sid:<sid>:<secret>` → Ambient path with the FULL caller identity.
    // The managed-session fields are injected (_caller_sid/_caller_secret/_caller_role/
    // _caller_slug) so session_* principal auth matches the live session
    // (v0.9.0 W1 G4 — previously only role+secret were injected, so session_*
    // over HTTP failed closed with "no project scope").
    log_call_identity(&auth, &req);
    let (caller, req) = match auth {
        McpAuth::Admin => (ccteam_im::mcp::McpCaller::Admin, req),
        McpAuth::User { user_id } => (ccteam_im::mcp::McpCaller::User { user_id }, req),
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
            (ccteam_im::mcp::McpCaller::Ambient, req)
        }
        // Handled above — it needs the request cycle, not just a caller tier.
        // Fail closed rather than panic: a broken invariant in an auth path must
        // not serve the request, and must not take the connection down either.
        McpAuth::Enroll { .. } => {
            tracing::error!("POST /mcp: enrollment bearer reached the static-family match");
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "auth required"})),
            )
                .into_response();
        }
    };
    dispatch_json_rpc(&app, req, caller).await
}

/// Run one JSON-RPC message through the dispatcher and shape the HTTP answer.
/// One home for the transport convention (200 + JSON body for a request, 202 for
/// a notification) so every credential family answers identically.
async fn dispatch_json_rpc(
    app: &AppState,
    req: Value,
    caller: ccteam_im::mcp::McpCaller,
) -> Response {
    let dispatch = app.mcp_dispatch();
    match dispatch.dispatch_as(req, caller).await {
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

/// Record WHICH identity a tool call arrived with.
///
/// The tier a request authenticates as is the single fact that decides its
/// delegation parent, its project scope and what it may reach — and until now
/// it was the one fact nothing wrote down. A managed session whose vendor
/// loaded the wrong `ccteam` MCP entry silently served its calls as the
/// machine-wide admin bearer instead, and the only evidence was a delegation
/// tree missing an edge. One line per tool call makes that answerable by
/// reading the log rather than by reconstructing it.
///
/// Tool calls only: `initialize` / `tools/list` are discovery noise.
fn log_call_identity(auth: &McpAuth, req: &Value) {
    let tier = match auth {
        McpAuth::Admin => "admin".to_string(),
        McpAuth::User { user_id } => format!("user:{user_id}"),
        McpAuth::Session { sid, .. } => format!("session:{sid}"),
        // An enrolled client logs as the NODE it resolved to (or as the
        // credential while it has none) — never as the identity that wrote the
        // config file. That is the whole point of the family, so its log line is
        // emitted where the binding is known.
        McpAuth::Enroll { credential } => format!("enroll:{}", credential.id),
    };
    log_tier_call(&tier, req);
}

/// The one `POST /mcp` tool-call log line. See [`log_call_identity`] for why the
/// authenticated tier is worth a line of its own.
fn log_tier_call(tier: &str, req: &Value) {
    if !is_tool_call(req) {
        return;
    }
    let tool = called_tool(req).unwrap_or("?");
    tracing::info!(%tier, %tool, "ccteam-web: POST /mcp tool call");
}

/// Who authenticated against `POST /mcp`.
enum McpAuth {
    Admin,
    User {
        user_id: String,
    },
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
    Enroll {
        credential: EnrollCredential,
    },
}

/// Enforce bearer always (this route mounts outside `auth_layer`, so this is
/// the ONLY gate). Accepts, in this order:
/// - session-scoped `ccteam-sid:<sid>:<secret>` (curated per-session MCP → Ambient)
/// - enrollment `ccteam-enroll:<id>:<secret>` (a hand-started client's config)
/// - admin/tenant web token `ccteam:<hex>` (resolved by the shared web family)
///
/// Each family is CLAIMED by its prefix: a bearer that claims one and fails it
/// is 401, never retried as another. A downgrade between families is exactly how
/// a managed session silently became an admin caller (v0.9.2), so the families
/// must not be able to cover for each other.
async fn require_mcp_auth(app: &AppState, headers: &HeaderMap) -> Result<McpAuth, Response> {
    let unauthorized = || {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "auth required: Authorization: Bearer ccteam:<hex> | ccteam-sid:<sid>:<secret> | ccteam-enroll:<id>:<secret>"
            })),
        )
            .into_response()
    };

    let raw = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_bearer_value);

    // Session-scoped bearer first (works regardless of AuthState.enabled).
    if let Some(tok) = raw {
        if let Some((sid, secret)) = parse_session_bearer(tok) {
            return verify_session_bearer(app, &sid, &secret).map_err(|_| unauthorized());
        }
        // Enrollment credential: verified against the on-disk record, whose
        // owner + scope are the only identity facts that matter. Reading it
        // needs no lock and no gateway, so a vendor's `initialize` handshake
        // can never queue behind a spawn (the Pi deadlock, 2026-08-09).
        if tok.starts_with(ENROLL_BEARER_PREFIX) {
            return match enroll::verify_in(&app.paths.root, tok) {
                Some(credential) => Ok(McpAuth::Enroll { credential }),
                None => Err(unauthorized()),
            };
        }
    }

    // Web token family — the LIVE admin token when the web gate is enabled
    // (single source with REST, including rotation) plus the tenant registry;
    // load the admin token from disk on loopback / --no-auth where AuthState
    // holds none.
    let expected = match app.auth.current_token() {
        Some(hex) => hex,
        None => match generate_or_load_token(&app.paths.web_token_path()) {
            Ok(hex) => hex,
            Err(err) => {
                tracing::error!(error = %err, "POST /mcp: failed to load web token");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "auth misconfigured: cannot load web token"})),
                )
                    .into_response());
            }
        },
    };
    let Some(bare) = raw.and_then(|presented| presented.strip_prefix(TOKEN_PREFIX)) else {
        return Err(unauthorized());
    };
    match resolve_identity(bare, &expected, &app.paths.users_dir()) {
        Some(identity) if identity.is_admin => Ok(McpAuth::Admin),
        Some(identity) => Ok(McpAuth::User {
            user_id: identity.id,
        }),
        None => Err(unauthorized()),
    }
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
/// client; an empty secret / unknown sid → 401.
///
/// **No gateway lock.** This used to lock the gateway to read a session's
/// secret, which made a credential check wait on whatever else held that lock
/// — including the very spawn whose vendor was making this call. Pi deadlocked
/// exactly there (2026-08-09): IM `/new` held the lock across `start_thread`,
/// the bridge's `session_start` blocked on this request, and the child never
/// reached the point of reading its stdin. The registry has its own lock, so
/// no spawn path can starve the handshake it is waiting for.
fn verify_session_bearer(app: &AppState, sid: &str, secret: &str) -> Result<McpAuth, ()> {
    let Some(principals) = app.session_principals.as_ref() else {
        return Err(());
    };
    match principals.verify(sid, secret) {
        Some(matched) => Ok(McpAuth::Session {
            sid: matched.sid,
            role: matched.role,
            secret: secret.to_string(),
            slug: matched.slug,
            may_invoke_tools: ccteam_im::principals::may_invoke_tools(matched.state),
        }),
        None => Err(()),
    }
}

/// Inject the FULL caller identity (`_caller_sid` / `_caller_secret` /
/// `_caller_role` / `_caller_slug`) into a tools/call arguments object so the
/// Ambient session_* PRINCIPAL gate sees the curated session's identity. All
/// four are OVERWRITTEN (never trust a caller-supplied value); the daemon
/// re-verifies `(sid, secret)` and re-derives slug/role from CallerCtx.
fn inject_session_caller(req: &mut Value, sid: &str, role: &str, secret: &str, slug: &str) {
    let Some(params) = req.get_mut("params") else {
        return;
    };
    let args = params.as_object_mut().and_then(|m| m.get_mut("arguments"));
    let args = match args {
        Some(a) => a,
        None => {
            if let Some(obj) = params.as_object_mut() {
                obj.insert("arguments".into(), json!({}));
                obj.get_mut("arguments").unwrap()
            } else {
                return;
            }
        }
    };
    if let Some(map) = args.as_object_mut() {
        map.insert("_caller_sid".into(), json!(sid));
        map.insert("_caller_secret".into(), json!(secret));
        map.insert("_caller_role".into(), json!(role));
        map.insert("_caller_slug".into(), json!(slug));
        // The local-socket admin fallback arg must never ride in over HTTP —
        // this transport authenticates via bearer only (admin or sid).
        map.remove("_caller_admin_token");
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
) -> Response {
    if req.get("method").and_then(Value::as_str) == Some("initialize") {
        return open_binding(app, credential, req).await;
    }
    let Some(binding) = resolve_binding(app, credential, headers) else {
        return no_such_mcp_session(&req);
    };
    match binding.principal() {
        // The binding has a ledger node, so this call IS that node: inject its
        // identity exactly as a managed session's and let the EXISTING Ambient
        // path do the rest (principal gate, project clamp, delegation parent).
        Some((sid, secret)) => {
            log_tier_call(&format!("session:{sid}"), &req);
            let slug = binding.project.as_deref().unwrap_or_default();
            inject_session_caller(&mut req, sid, "", secret, slug);
            dispatch_json_rpc(app, req, ccteam_im::mcp::McpCaller::Ambient).await
        }
        None => {
            log_tier_call(&format!("enroll:{}", credential.id), &req);
            if let Some(refusal) = refuse_projectless_call(app, credential, &binding, &req) {
                return refusal;
            }
            dispatch_json_rpc(app, req, ccteam_im::mcp::McpCaller::Ambient).await
        }
    }
}

/// `initialize` — issue this process its own identity.
///
/// The binding is opened first and unconditionally: even when no ledger node can
/// be created, the client gets an id, so its later calls arrive with something
/// resolvable and are refused with a REASON instead of being indistinguishable
/// from a client that never initialized.
async fn open_binding(app: &AppState, credential: &EnrollCredential, req: Value) -> Response {
    let client = client_label(&req);
    let id = app.native_bindings.open(
        &credential.id,
        &credential.owner,
        credential.scope.project().map(str::to_string),
        &client,
    );
    if let Some(slug) = credential.scope.project() {
        match mint_ledger_node(app, credential, slug, &client).await {
            Ok((sid, secret)) => {
                // The secret never leaves this process: the client authenticates
                // with its enroll bearer + id, and the daemon speaks for it
                // internally with the principal it minted here.
                app.native_bindings.attach_session(&id, &sid, &secret);
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
    let mut response = dispatch_json_rpc(app, req, ccteam_im::mcp::McpCaller::Ambient).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(MCP_SESSION_ID), value);
    }
    response
}

/// Create the ledger node for a project-scoped credential and mint the principal
/// it authenticates with.
///
/// Fail-closed on visibility through the SAME predicate the REST ACL choke point
/// uses: the credential is long-lived and pasted into config files, so "the
/// operator could see this project when they minted it" is not enough — it has to
/// still be true now.
async fn mint_ledger_node(
    app: &AppState,
    credential: &EnrollCredential,
    slug: &str,
    client: &str,
) -> Result<(String, String), String> {
    let identity = identity_for_owner(&credential.owner);
    if !app.paths.project_state(slug).exists()
        || !crate::routes::api_v1::can_see_project(app, &identity, slug)
    {
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
    match addressable_projects(app, &credential.owner) {
        projects if projects.is_empty() => message.push_str(
            " No project is registered for this credential's owner yet; create one in the web \
             console first.",
        ),
        projects => message.push_str(&format!(
            " Ask for a project-scoped enrollment snippet (web console → the project → external \
             agent) for one of: {}.",
            projects.join(", ")
        )),
    }
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
    let auth = match require_mcp_auth(&app, &headers).await {
        Ok(a) => a,
        Err(resp) => return resp,
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
