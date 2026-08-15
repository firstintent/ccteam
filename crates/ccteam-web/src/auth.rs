//! V0.3 M5.3 — token-based auth middleware for the ccteam web layer.
//!
//! ## Decision: whole-app gate (no read/write split)
//!
//! When [`AuthState::enabled`] is true, **every** route on the
//! stateful router (`/`, `/project/<slug>`, `/sse/...`,
//! `/api/<slug>/...`) requires either:
//!
//! - `Authorization: Bearer ccteam:<token>` on the request, or
//! - the `ccteam_token` cookie set by the URL shim.
//!
//! `/health` is exempt so ops monitoring keeps working without baking
//! the token into a probe. (Body is `{status, version}` — no project
//! state leak.)
//!
//! ## Browser flow
//!
//! On first visit the user pastes
//! `http://<host>:<port>/?token=ccteam:<hex>`. The middleware:
//!
//! 1. extracts `token` from the query string,
//! 2. validates it (constant-time) against the loaded token,
//! 3. sets a HttpOnly `ccteam_token` cookie (persistent — `Max-Age` =
//!    [`COOKIE_MAX_AGE_DAYS`] so it survives a browser restart yet forces a
//!    re-login after at most that long) + 302-redirects to the same path
//!    minus the `token` query parameter,
//! 4. subsequent GETs / SSE include the cookie automatically.
//!
//! ## CSRF defense (SameSite=Strict cookie)
//!
//! CSRF is defeated by the `SameSite=Strict` attribute on the
//! `ccteam_token` cookie (set in [`auth_layer`]). Under `Strict` the
//! browser refuses to attach the cookie to **any** cross-site request —
//! including mutating POST/PUT/DELETE form-submits *and* top-level
//! navigations — so a malicious page on attacker.example cannot ride
//! the victim's cookie. The other auth path is the `Authorization:
//! Bearer ccteam:<hex>` header, which a cross-origin page cannot forge
//! (it is not in the CORS "simple header" allowlist, and a custom
//! header triggers a preflight we never permit). Either way the
//! attacker can produce neither a valid cookie nor a valid header
//! against `/api/<slug>/...`, so no authenticated mutating request is
//! possible.
//!
//! Note: the middleware does **not** require the `Authorization` header
//! on mutating methods. A same-origin request authenticated by the
//! cookie alone is accepted for every method — `SameSite=Strict` (not
//! per-method header enforcement) is what carries the CSRF guarantee.
//! This keeps the cookie carry-over flow (step 3 below) working for the
//! same-origin SPA without forcing it to echo the bearer token.
//!
//! Same-origin clients: the SPA stores the token and injects it via
//! `Authorization: Bearer` on every fetch (see
//! `web/src/lib/fetchInterceptor.ts`); the cookie is the fallback for
//! plain navigations / SSE that cannot set a header.
//!
//! Architecture refs: `docs/versions/v0-3/prd.md` §6.2.4 / §6.2.5 / §9.

use std::net::SocketAddr;
use std::path::Path;

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use axum_extra::extract::{
    cookie::{Cookie, SameSite},
    CookieJar,
};
use serde_json::json;
use subtle::ConstantTimeEq;

use ccteam_core::session_secret;
use ccteam_core::tenants::TenantRegistry;

use crate::state::AppState;

/// Wire-format token prefix. Tokens are written + checked as
/// `ccteam:<hex>` so a leaked token is grep-able by purpose.
pub const TOKEN_PREFIX: &str = "ccteam:";

/// Cookie name set by the URL shim. HttpOnly + SameSite=Strict.
pub const COOKIE_NAME: &str = "ccteam_token";

/// How long a freshly-minted session cookie lives, in days.
/// Set as `Max-Age` so the cookie is **persistent** (written to disk →
/// survives a browser restart) yet self-expiring.
///
/// Steady-state: a valid cookie short-circuits before Bearer re-mint, so the
/// clock does **not** slide on every request — at rest the user is forced to
/// re-login after this many days. Caveat (accepted, 2026-07-29 review): if the
/// cookie dies first while the SPA localStorage Bearer is still valid, the
/// next Bearer REST call re-mints a **new** full Max-Age window, so the
/// effective continuous login can stretch toward ~2× this window until both
/// paths finally expire. That is a heal of the dual-auth desync, not a
/// sliding session. Do not "fix" by re-minting on the valid-cookie path —
/// that would turn this into a true sliding window.
pub const COOKIE_MAX_AGE_DAYS: i64 = 7;

/// Query-string parameter parsed by the URL shim.
pub const QUERY_PARAM: &str = "token";

/// Auth state attached to [`AppState`].
///
/// v0.8.24 — the admin token lives behind a shared `Arc<RwLock<…>>` so the
/// self-serve reset (`POST /api/v1/me/reset-token`) can rotate it LIVE:
/// every `AppState` clone (one per request) shares the same cell, so the
/// new token is accepted immediately, without a daemon restart.
#[derive(Debug, Clone)]
pub struct AuthState {
    /// If false, the middleware passes every request through (the
    /// loopback default + explicit `--no-auth`).
    pub enabled: bool,
    /// Hex-encoded token (no `ccteam:` prefix). `None` when disabled.
    token: std::sync::Arc<std::sync::RwLock<Option<String>>>,
}

impl AuthState {
    /// Auth-off (loopback default / `--no-auth` opt-out).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            token: std::sync::Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Auth-on with the supplied hex token.
    pub fn enabled(token: String) -> Self {
        Self {
            enabled: true,
            token: std::sync::Arc::new(std::sync::RwLock::new(Some(token))),
        }
    }

    /// The current bare-hex admin token; `None` when disabled.
    pub fn current_token(&self) -> Option<String> {
        self.token.read().ok().and_then(|t| t.clone())
    }

    /// v0.8.24 — rotate the in-memory admin token (the caller has already
    /// persisted the new value to `~/.ccteam/secrets/web-token`). Takes
    /// effect for the NEXT request on every AppState clone (shared cell).
    pub fn rotate(&self, new_hex: String) {
        if let Ok(mut guard) = self.token.write() {
            *guard = Some(new_hex);
        }
    }

    /// Wire-format token (`ccteam:<hex>`) for templates / docs. Returns
    /// `None` when auth is disabled.
    pub fn wire_token(&self) -> Option<String> {
        self.current_token().map(|t| format!("{TOKEN_PREFIX}{t}"))
    }
}

/// v0.8.18 档1 — the resolved identity of an authenticated request, injected
/// into the request extensions by [`auth_layer`] so handlers can scope to the
/// caller. The bootstrap (owner) web token from `ccteam config` resolves to
/// `admin`; per-user tokens from the tenant registry resolve to that tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `"admin"` for the bootstrap token (the owner), else the tenant id.
    pub id: String,
    /// Whether this is the admin/owner (manages users; sees everything).
    pub is_admin: bool,
}

impl Identity {
    /// The owner / bootstrap-token identity (full access, manages users).
    pub fn admin() -> Self {
        Self {
            id: "admin".to_string(),
            is_admin: true,
        }
    }

    /// A per-user tenant identity.
    pub fn tenant(id: String) -> Self {
        Self {
            id,
            is_admin: false,
        }
    }

    /// This identity's owner tag for resources it creates on the web
    /// (a project's `ProjectState.owner` / a session owner, `"channel:chat_id"`).
    /// The `user:` namespace is the SYNTHETIC identity channel (NOT a delivery
    /// channel — see the gateway's `canonical_owner`): the admin uses the shared
    /// `user:web-api` pool; a per-user tenant owns `user:<id>`.
    pub fn owner_tag(&self) -> String {
        ccteam_core::identity::owner_tag(&self.id, self.is_admin)
    }

    /// v0.8.20 — the bare chat_id of this identity's WEB FRONTEND (the delivery
    /// channel `"web"`): `"web-api"` for the admin, else the tenant id. Threaded
    /// into the gateway's web session creation as the `reply_to` seed; the gateway
    /// then derives the OWNER as `user:<id>` via `canonical_owner` (web↔IM
    /// convergence — the tenant's own IM bot then sees it too).
    pub fn web_chat_id(&self) -> String {
        ccteam_core::identity::web_chat_id(&self.id, self.is_admin)
    }

    /// Whether this identity may see a resource owned by `owner` (a project's
    /// `ProjectState.owner` or a session owner). Own resources are always
    /// visible (the admin's `user:web-api` pool, or a tenant's `user:<id>`). The
    /// admin/operator additionally sees every NON-tenant resource — unowned
    /// (legacy CLI projects, `None`) and IM-owned (`telegram:*`) — but NOT a
    /// per-user tenant's resources (`user:<id>`), which stay private to that
    /// tenant (档1 owner ruling: admin does not peek into tenants' projects). A
    /// tenant sees ONLY its own.
    pub fn can_see_owner(&self, owner: Option<&str>) -> bool {
        ccteam_core::identity::can_see_owner(&self.id, self.is_admin, owner)
    }
}

/// `Some(403)` unless the caller is the admin/owner — the shared gate for every
/// admin-only route (user management and global IM credentials).
pub fn deny_non_admin(identity: &Identity) -> Option<Response> {
    if identity.is_admin {
        None
    } else {
        Some(
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "admin only: this surface is owner-gated"})),
            )
                .into_response(),
        )
    }
}

/// v0.8.18 档1 — the caller's OWN presented wire token (`ccteam:<hex>`), stashed
/// by [`auth_layer`] so `GET /api/v1/auth/token` returns the CALLER's token, not
/// the admin's (a tenant must never receive the bootstrap token = escalation).
/// Absent on the no-auth/loopback path → the endpoint reports auth-not-required.
#[derive(Debug, Clone)]
pub struct PresentedToken(pub String);

/// v0.8.18 档1 — the project-ownership gate for **every** `/api/v1/projects/
/// {slug}/...` route (the single choke point: no project-scoped route can leak,
/// and new ones are covered automatically — vs. gating each handler). Runs
/// AFTER [`auth_layer`] (so [`Identity`] is in the request extensions). If the
/// caller can't see the addressed project, 404 before the handler runs. The
/// bare `/api/v1/projects` collection (list/create) has no slug → not gated
/// here (the list is filtered per-identity in `build_projects`).
pub async fn project_acl_layer(State(app): State<AppState>, req: Request, next: Next) -> Response {
    if let Some(slug) = project_slug_from_path(req.uri().path()) {
        let identity = req
            .extensions()
            .get::<Identity>()
            .cloned()
            .unwrap_or_else(Identity::admin);
        if !crate::routes::api_v1::can_see_project(&app, &identity, slug) {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("project not found: {slug}")})),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// Extract the addressed project `{slug}` from ANY project-scoped path, so the
/// one middleware covers every route family that names a project:
///
/// - `/api/v1/projects/{slug}[/...]` — the REST resource tree,
/// - `/api/{slug}/...` — the legacy per-project action routes (`btw`, `pause`,
///   `resume`, `inject_decision`) and the pane snapshots,
/// - `/ws/{slug}/pty`, `/ws/{slug}/{sid}/pty` — the live terminal sockets.
///
/// Cross-user fix (2026-07-28) — the last two families used to sit OUTSIDE the choke point with no
/// identity check at all, so any authenticated tenant could snapshot or attach
/// a PTY to another user's project (and POST its pause/resume/btw actions).
/// `None` for the bare collection path (`/api/v1/projects`) and for the
/// non-project routes that share those prefixes (`/api/v1/...`, `/api/docs`,
/// `/ws/chat`).
fn project_slug_from_path(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("/api/v1/projects/") {
        // Collection action: creates a new catalog entry and stamps the caller
        // as owner, so it has the same ACL posture as POST /projects (there is
        // no existing project slug to authorize yet).
        if rest == "import" {
            return None;
        }
        return non_empty(first_segment(rest));
    }
    if let Some(rest) = path.strip_prefix("/api/") {
        let head = first_segment(rest);
        // `/api/v1/...` is the versioned tree (handled above); `/api/docs...`
        // is the Scalar UI. Neither names a project.
        if head == "v1" || head == "docs" {
            return None;
        }
        return non_empty(head);
    }
    if let Some(rest) = path.strip_prefix("/ws/") {
        let head = first_segment(rest);
        // `/ws/chat` is the browser chat socket — identity-scoped, not project-
        // scoped (it binds to the caller's own identity, see `chat_ws`).
        if head == "chat" {
            return None;
        }
        return non_empty(head);
    }
    None
}

fn first_segment(rest: &str) -> &str {
    rest.split('/').next().unwrap_or(rest)
}

fn non_empty(slug: &str) -> Option<&str> {
    (!slug.is_empty()).then_some(slug)
}

/// Strip the `ccteam:` wire prefix from a presented token → the bare hex (the
/// value compared / stored in the cookie). `None` when the prefix is absent.
fn bare_hex(presented: &str) -> Option<&str> {
    presented.strip_prefix(TOKEN_PREFIX)
}

/// Resolve a bare-hex token to an [`Identity`]: the admin token (the bootstrap
/// web token) → admin; else a per-user tenant token from the registry → that
/// tenant; else `None`. Constant-time compares throughout.
pub fn resolve_identity(bare: &str, admin_hex: &str, tenants_path: &Path) -> Option<Identity> {
    if session_secret::ct_eq(bare, admin_hex) {
        return Some(Identity::admin());
    }
    TenantRegistry::load(tenants_path)
        .by_token(bare)
        .map(|t| Identity::tenant(t.id.clone()))
}

/// Resolve a web request's cookie/Bearer credentials without the SPA login
/// query shim and without public-shell bypasses. Used by the DSH companion
/// port, whose entire surface is a reverse proxy into a local DSH process and
/// must therefore fail closed on anonymous requests.
pub fn resolve_strict_web_identity(
    auth: &AuthState,
    ccteam_root: &Path,
    headers: &HeaderMap,
    jar: &CookieJar,
) -> Result<Option<Identity>, &'static str> {
    if !auth.enabled {
        return Ok(Some(Identity::admin()));
    }
    let Some(expected) = auth.current_token() else {
        return Err("auth misconfigured");
    };
    let tenants = ccteam_root.join("secrets").join("users");
    if let Some(cookie_val) = cookie_token(jar) {
        if let Some(id) = resolve_identity(cookie_val, &expected, &tenants) {
            return Ok(Some(id));
        }
    }
    if let Some(h) = headers.get(header::AUTHORIZATION) {
        if let Some(presented) = parse_bearer(h) {
            if let Some(bare) = bare_hex(presented) {
                if let Some(id) = resolve_identity(bare, &expected, &tenants) {
                    return Ok(Some(id));
                }
            }
        }
    }
    Ok(None)
}

/// v0.8.24 Track D — resolve a join-token or satellite agent-token to a
/// non-admin identity. Join tokens map to `host-join` (only useful for
/// `POST /hosts/join`); agent tokens map to `host:<id>` (heartbeat).
pub fn resolve_host_token(bare: &str, ccteam_root: &Path) -> Option<Identity> {
    use ccteam_core::host_registry::{
        join_tokens_path_in, registry_path_in, HostRegistry, JoinTokenStore,
    };
    let tok_path = join_tokens_path_in(ccteam_root);
    if let Ok(store) = JoinTokenStore::load(&tok_path) {
        if store.contains_valid(bare) {
            return Some(Identity {
                id: "host-join".to_string(),
                is_admin: false,
            });
        }
    }
    let reg_path = registry_path_in(ccteam_root);
    if let Ok(reg) = HostRegistry::load(&reg_path) {
        if let Some(h) = reg.by_agent_token(bare) {
            return Some(Identity {
                id: format!("host:{}", h.id),
                is_admin: false,
            });
        }
    }
    None
}

/// Decide whether a `SocketAddr` is loopback (auth defaults off) or
/// non-loopback (auth defaults on).
pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Constant-time check that `presented` (full `ccteam:<hex>` string)
/// matches the configured token. The `subtle::ConstantTimeEq` impl on
/// byte slices already short-circuits on differing lengths in a way
/// that doesn't leak content; the upstream contract is that *content*
/// timing must not vary, which is what we need to defeat the
/// LAN-attacker threat model in PRD §9.1.
pub fn validate_bearer(presented: &str, expected_hex: &str) -> bool {
    let prefix = TOKEN_PREFIX.as_bytes();
    let bytes = presented.as_bytes();
    if bytes.len() != prefix.len() + expected_hex.len() {
        return false;
    }
    if !bytes.starts_with(prefix) {
        return false;
    }
    let hex = &bytes[prefix.len()..];
    hex.ct_eq(expected_hex.as_bytes()).into()
}

/// Extract the bearer token from `Authorization: Bearer ccteam:<hex>`,
/// returning the full `ccteam:<hex>` slice (post-`Bearer ` chomp).
fn parse_bearer(value: &HeaderValue) -> Option<&str> {
    let s = value.to_str().ok()?;
    let rest = s.strip_prefix("Bearer ")?;
    Some(rest.trim())
}

/// Pull the token from the cookie jar, if present. Returns the bare
/// hex value (no prefix) since the cookie payload omits the prefix —
/// keeps the cookie body short + machine-checkable.
pub fn cookie_token(jar: &CookieJar) -> Option<&str> {
    jar.get(COOKIE_NAME).map(|c| c.value())
}

/// Strip the `token` parameter from a request URI's query string.
/// Used when the URL shim 302-redirects to a clean URL after setting
/// the cookie.
fn uri_without_token(uri: &Uri) -> String {
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let key = p.split_once('=').map(|(k, _)| k).unwrap_or(p);
            key != QUERY_PARAM
        })
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{}?{}", path, kept.join("&"))
    }
}

/// Pull `?token=<value>` out of a query string.
///
/// Values are **percent-decoded**. The SPA login form navigates to
/// `/?token=${encodeURIComponent("ccteam:<hex>")}`, which yields
/// `token=ccteam%3A…` on the wire. Without decoding, [`bare_hex`] fails
/// (it looks for a literal `ccteam:` prefix), the URL shim never sets the
/// cookie, and the request falls through to the public-shell 301
/// `/` → `/app/` — leaving the user on the login page. Unencoded
/// `ccteam:<hex>` (CLI-printed links, hand-pasted) still works.
fn query_token(query: &str) -> Option<String> {
    for part in query.split('&') {
        if let Some(rest) = part.strip_prefix("token=") {
            return percent_decode_token(rest);
        }
    }
    None
}

/// Percent-decode a `token` query value into a UTF-8 string.
///
/// Accepts both the SPA form (`ccteam%3A` + hex) and the unencoded CLI
/// form (`ccteam:` + hex). Invalid percent sequences / non-UTF-8 → `None`
/// (fail closed; a garbage token must not authenticate).
fn percent_decode_token(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                // Incomplete or non-hex escape → fail closed (do not treat
                // a dangling `%` as a literal — callers must present a
                // well-formed wire token).
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = from_hex_nibble(bytes[i + 1])?;
                let lo = from_hex_nibble(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            // application/x-www-form-urlencoded spaces (not used by our
            // SPA, but harmless if a proxy rewrites the query that way).
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn from_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Extract the bare hex from a presented `?token=` value.
/// Accepts wire form `ccteam:<hex>` OR bare hex (the login UI asks for hex
/// only; CLI personal links use the wire form).
fn presented_token_hex(presented: &str) -> Option<&str> {
    if let Some(bare) = bare_hex(presented) {
        return Some(bare);
    }
    if !presented.is_empty() && presented.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(presented);
    }
    None
}

/// Where the URL shim should send the browser after setting the cookie.
/// `/` is rewritten to `/app/` so login lands on the SPA in one hop
/// (avoids `/` → 301 `/app/` losing cookies on some proxies).
fn login_redirect_path(uri: &Uri) -> String {
    let clean = uri_without_token(uri);
    if clean == "/" {
        "/app/".to_string()
    } else {
        clean
    }
}

/// Build the persistent HttpOnly session cookie for a validated bare-hex
/// token. Shared by the `?token=` URL shim and the Bearer re-mint path so
/// both keep identical attributes (SameSite=Strict, 7-day Max-Age, Path=/).
fn mint_session_cookie(bare: &str) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, bare.to_string()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        // Persistent (survives a browser restart) + self-expiring after
        // COOKIE_MAX_AGE_DAYS — the "stay logged in ≤ 7d" contract.
        .max_age(cookie::time::Duration::days(COOKIE_MAX_AGE_DAYS))
        .build()
}

/// Attach a freshly-minted session cookie to an already-built response.
/// Used when a request authenticated via `Authorization: Bearer` (no valid
/// cookie) so cookie-only clients — browser `EventSource` and WebSocket
/// upgrades, which cannot set an Authorization header — heal on the next
/// request after any successful Bearer REST call.
///
/// ## Pitfalls for future handlers
///
/// - **`POST /me/reset-token`**: the middleware re-mints using the *request's*
///   (now-dead) Bearer hex. The handler returns the NEW token in JSON only —
///   the Set-Cookie on that response may still hold the old hex until the SPA
///   saves the new Bearer and the next REST call re-mints. No privilege
///   issue (old hex no longer resolves), just a dirty intermediate cookie.
///   Prefer the handler minting the new cookie itself if that UX matters.
/// - **Future `/logout` that clears the cookie**: if the SPA still injects a
///   valid Bearer, this function will re-attach a cookie after the handler's
///   clear (later Set-Cookie wins). Clear the token client-side first, or
///   skip re-mint when the response already deletes the session cookie.
fn with_session_cookie(bare: &str, response: Response) -> Response {
    let jar = CookieJar::new().add(mint_session_cookie(bare));
    (jar, response).into_response()
}

/// The middleware itself, plumbed via `from_fn_with_state`.
///
/// Order of checks (v0.8.20 — cookie BEFORE header, the ownership-leak fix):
///
/// 1. If `auth.enabled = false` → pass through (loopback = owner = admin).
/// 2. Query string `?token=` present + valid → set HttpOnly cookie + 302
///    redirect to URI minus the `token` parameter (an explicit login link
///    always wins — it (re)establishes the cookie).
/// 3. Cookie value valid → pass through (the CURRENT login; checked before the
///    Authorization header so a stale `Bearer` the SPA fetch shim still injects
///    can't shadow a freshly-set tenant cookie).
/// 4. Bearer header valid → pass through + **re-mint the session cookie** when
///    the request reached this branch (cookie missing/invalid). Cookieless
///    clients (curl, iOS-PWA localStorage Bearer) keep working; after one
///    successful REST call the cookie is restored for EventSource / WS.
/// 5. Else → 401 plain-text "auth required".
pub async fn auth_layer(
    State(app): State<AppState>,
    jar: CookieJar,
    mut req: Request,
    next: Next,
) -> Response {
    let auth = app.auth.clone();
    if !auth.enabled {
        // Loopback / --no-auth: the local operator IS the owner → admin identity.
        req.extensions_mut().insert(Identity::admin());
        return next.run(req).await;
    }
    let Some(expected) = auth.current_token() else {
        // Defensive: enabled=true but no token loaded → fail closed.
        return (StatusCode::INTERNAL_SERVER_ERROR, "auth misconfigured").into_response();
    };
    let expected = expected.as_str();
    // v0.8.18 档1 — the bootstrap token → admin; per-user tokens → their tenant.
    let tenants = app.paths.users_dir();

    // 1. URL shim — an explicit `?token=ccteam:<hex>` (or bare hex) login link
    //    ALWAYS wins: it (re)establishes the session cookie so a fresh login
    //    replaces a stale one. The cookie stores the BARE hex presented (admin
    //    OR per-user), so the carry-over below resolves to the SAME identity.
    //    `query_token` percent-decodes so SPA `encodeURIComponent` (`ccteam%3A…`)
    //    and CLI unencoded links both resolve. Bare hex is accepted too (the
    //    login form asks for hex only; a hand-pasted `?token=<hex>` must work).
    if let Some(q) = req.uri().query() {
        if let Some(presented) = query_token(q) {
            if let Some(bare) = presented_token_hex(&presented) {
                if resolve_identity(bare, expected, &tenants).is_some() {
                    // Prefer landing on the SPA shell after login: bare `/`
                    // would 301 → `/app/` as a second hop, which some
                    // browser/proxy stacks drop Set-Cookie across.
                    let clean = login_redirect_path(req.uri());
                    let jar = jar.add(mint_session_cookie(bare));
                    return (jar, Redirect::to(&clean)).into_response();
                }
            }
        }
    }

    // 2. Session cookie — the CURRENT login. Checked BEFORE the Authorization
    //    header so a freshly-set tenant cookie is NOT shadowed by a stale admin
    //    `Bearer` the SPA fetch shim still injects from a prior login. This is
    //    the v0.8.20 ownership-leak fix: header-first let a cached admin token
    //    outrank the fresh tenant cookie → a tenant's new project landed under
    //    the admin pool (`user:web-api`) instead of `user:<tenant>`.
    if let Some(cookie_val) = cookie_token(&jar) {
        if let Some(id) = resolve_identity(cookie_val, expected, &tenants) {
            let wire = format!("{TOKEN_PREFIX}{cookie_val}");
            req.extensions_mut().insert(id);
            req.extensions_mut().insert(PresentedToken(wire));
            return next.run(req).await;
        }
    }

    // 3. Authorization header — fallback for cookieless clients: API callers
    //    (curl) and iOS-PWA where the standalone switch drops the cookie and the
    //    token survives only in localStorage (injected as Bearer by the SPA).
    //    Reaching here means the cookie was missing or invalid → mint it from
    //    the Bearer so EventSource / WebSocket (header-less) heal after any
    //    successful REST call. Host join/agent tokens are NOT web session
    //    principals and must not receive the SPA cookie.
    if let Some(h) = req.headers().get(header::AUTHORIZATION) {
        if let Some(presented) = parse_bearer(h) {
            if let Some(bare) = bare_hex(presented) {
                if let Some(id) = resolve_identity(bare, expected, &tenants) {
                    // Own the token before the &mut borrow (it borrows req.headers).
                    let bare = bare.to_string();
                    let presented = presented.to_string();
                    req.extensions_mut().insert(id);
                    req.extensions_mut().insert(PresentedToken(presented));
                    let response = next.run(req).await;
                    return with_session_cookie(&bare, response);
                }
                // v0.8.24 Track D — join-token / satellite agent-token may
                // authenticate only the host-join + heartbeat surfaces (not
                // admin). Identity is NOT admin so deny_non_admin still gates
                // list/mint/register-mcp.
                if let Some(id) = resolve_host_token(bare, &app.paths.root) {
                    let presented = presented.to_string();
                    req.extensions_mut().insert(id);
                    req.extensions_mut().insert(PresentedToken(presented));
                    return next.run(req).await;
                }
            }
        }
    }

    // 5. Unauthenticated request for the SPA shell (index.html + hashed bundle +
    //    the `/` → `/app/` redirect): SERVE it instead of 401. The bundle carries
    //    no secrets, and the browser MUST load it so the client-side token flow
    //    (TokenEntryPage) can prompt for a token — otherwise the browser renders
    //    a raw plain-text "auth required" and the login UI never appears. Every
    //    `/api/*` route stays gated (not a shell path → falls through to 401
    //    below), and the `?token=` URL shim (step 1) still runs first, so a login
    //    link continues to establish the session cookie. GET-only: the shell is
    //    never mutated.
    if req.method() == Method::GET && is_public_shell_path(req.uri().path()) {
        return next.run(req).await;
    }

    (StatusCode::UNAUTHORIZED, "auth required").into_response()
}

/// Paths that make up the public SPA shell — the SPA `index.html` (any
/// `/app` route, resolved client-side by react-router), the vite hashed bundle
/// (`/assets/spa/...`), and the bare-domain `/` → `/app/` redirect. These are
/// served to unauthenticated visitors (see [`auth_layer`] step 5) so the
/// in-browser token flow can run; they hold no project state or secrets. Every
/// other path — notably all of `/api/...` — stays behind the token gate.
fn is_public_shell_path(path: &str) -> bool {
    path == "/"
        || path == "/app"
        || path.starts_with("/app/")
        || path.starts_with("/assets/spa/")
        // The PWA manifest + icons: the BROWSER fetches these, anonymously
        // (a manifest request carries no cookie and no Bearer), so gating them
        // means a phone can never install the app. They carry no secrets —
        // the same content the public shell already serves under `/app/`.
        || crate::routes::assets::is_root_pwa_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_bearer_accepts_correct_token() {
        assert!(validate_bearer("ccteam:deadbeef", "deadbeef"));
    }

    #[test]
    fn validate_bearer_rejects_wrong_hex() {
        assert!(!validate_bearer("ccteam:deadbeef", "deadbeee"));
    }

    #[test]
    fn validate_bearer_rejects_missing_prefix() {
        assert!(!validate_bearer("Bearer deadbeef", "deadbeef"));
        assert!(!validate_bearer("deadbeef", "deadbeef"));
    }

    #[test]
    fn validate_bearer_rejects_length_mismatch() {
        assert!(!validate_bearer("ccteam:dead", "deadbeef"));
        assert!(!validate_bearer("ccteam:deadbeefdeadbeef", "deadbeef"));
    }

    #[test]
    fn auth_state_wire_token_round_trip() {
        let s = AuthState::enabled("abc123".into());
        assert_eq!(s.wire_token().unwrap(), "ccteam:abc123");
        let off = AuthState::disabled();
        assert!(off.wire_token().is_none());
    }

    #[test]
    fn uri_without_token_strips_only_token_param() {
        let u: Uri = "/foo?token=ccteam:abc&other=1".parse().unwrap();
        assert_eq!(uri_without_token(&u), "/foo?other=1");
        let u2: Uri = "/foo?token=ccteam:abc".parse().unwrap();
        assert_eq!(uri_without_token(&u2), "/foo");
        let u3: Uri = "/foo".parse().unwrap();
        assert_eq!(uri_without_token(&u3), "/foo");
        let u4: Uri = "/foo?other=1&token=abc&another=2".parse().unwrap();
        assert_eq!(uri_without_token(&u4), "/foo?other=1&another=2");
    }

    #[test]
    fn query_token_extracts_value() {
        assert_eq!(
            query_token("token=ccteam:abc").as_deref(),
            Some("ccteam:abc")
        );
        assert_eq!(
            query_token("other=x&token=ccteam:abc").as_deref(),
            Some("ccteam:abc")
        );
        assert_eq!(query_token("other=x"), None);
    }

    #[test]
    fn query_token_percent_decodes_spa_encode_uri_component() {
        // TokenEntryPage: `/?token=${encodeURIComponent("ccteam:<hex>")}`
        // → wire query `token=ccteam%3A…`. Must become wire-format again.
        assert_eq!(
            query_token("token=ccteam%3Adeadbeef").as_deref(),
            Some("ccteam:deadbeef")
        );
        assert_eq!(
            query_token("token=ccteam%3adeadbeef").as_deref(),
            Some("ccteam:deadbeef"),
            "hex digits in percent-escapes are case-insensitive"
        );
        assert_eq!(
            query_token("other=1&token=ccteam%3Aabc&x=y").as_deref(),
            Some("ccteam:abc")
        );
    }

    #[test]
    fn percent_decode_token_rejects_garbage() {
        assert!(percent_decode_token("ccteam%ZZ").is_none());
        assert!(percent_decode_token("ccteam%").is_none());
        assert!(percent_decode_token("ccteam%3").is_none());
    }

    #[test]
    fn presented_token_hex_accepts_wire_and_bare() {
        assert_eq!(presented_token_hex("ccteam:deadbeef"), Some("deadbeef"));
        assert_eq!(presented_token_hex("deadbeef"), Some("deadbeef"));
        assert_eq!(presented_token_hex("Bearer deadbeef"), None);
        assert_eq!(presented_token_hex(""), None);
        assert_eq!(presented_token_hex("not-hex!"), None);
    }

    #[test]
    fn login_redirect_path_lands_on_spa_from_root() {
        let root: Uri = "/?token=ccteam:abc".parse().unwrap();
        assert_eq!(login_redirect_path(&root), "/app/");
        let nested: Uri = "/project/demo?token=ccteam:abc".parse().unwrap();
        assert_eq!(login_redirect_path(&nested), "/project/demo");
        let app: Uri = "/app/?token=ccteam:abc".parse().unwrap();
        assert_eq!(login_redirect_path(&app), "/app/");
    }

    #[test]
    fn parse_bearer_strips_prefix() {
        let h = HeaderValue::from_static("Bearer ccteam:abc");
        assert_eq!(parse_bearer(&h), Some("ccteam:abc"));
        let h2 = HeaderValue::from_static("Basic ccteam:abc");
        assert_eq!(parse_bearer(&h2), None);
    }

    #[test]
    fn is_loopback_recognizes_127_and_v6() {
        let v4: SocketAddr = "127.0.0.1:7331".parse().unwrap();
        assert!(is_loopback(&v4));
        let v6: SocketAddr = "[::1]:7331".parse().unwrap();
        assert!(is_loopback(&v6));
        let lan: SocketAddr = "192.168.1.5:7331".parse().unwrap();
        assert!(!is_loopback(&lan));
        let any: SocketAddr = "0.0.0.0:7331".parse().unwrap();
        assert!(!is_loopback(&any));
    }

    #[test]
    fn is_public_shell_path_covers_spa_shell_but_never_the_api() {
        // SPA shell + redirect root are public.
        assert!(is_public_shell_path("/"));
        assert!(is_public_shell_path("/app"));
        assert!(is_public_shell_path("/app/"));
        assert!(is_public_shell_path("/app/chat/s/s1"));
        assert!(is_public_shell_path("/assets/spa/index-abc123.js"));
        // PWA install files are fetched anonymously by the browser.
        assert!(is_public_shell_path("/manifest.json"));
        assert!(is_public_shell_path("/icon-192.png"));
        assert!(is_public_shell_path("/sw.js"));
        // The API and every other gated surface are NOT.
        assert!(!is_public_shell_path("/api/v1/auth/token"));
        assert!(!is_public_shell_path("/api/v1/projects/demo/sessions"));
        assert!(!is_public_shell_path("/health"));
        // No prefix-confusion: a path that merely starts with "/app" but is not
        // under the shell must not slip through.
        assert!(!is_public_shell_path("/apple"));
    }

    /// Cross-user fix (2026-07-28) — the ACL choke point must recognise EVERY project-addressed
    /// route family. The regression it guards: `/api/{slug}/…` (actions +
    /// pane snapshots) and `/ws/{slug}/…` (PTY) named a project but were not
    /// matched here, so they ran with no ownership check at all — a tenant
    /// could snapshot or attach a terminal to another user's project.
    #[test]
    fn project_slug_from_path_covers_every_project_addressed_family() {
        // REST resource tree.
        assert_eq!(
            project_slug_from_path("/api/v1/projects/demo"),
            Some("demo")
        );
        assert_eq!(
            project_slug_from_path("/api/v1/projects/demo/sessions"),
            Some("demo")
        );
        // Legacy per-project actions + pane snapshots.
        assert_eq!(project_slug_from_path("/api/demo/pause"), Some("demo"));
        assert_eq!(
            project_slug_from_path("/api/demo/s7/inject_decision"),
            Some("demo")
        );
        assert_eq!(
            project_slug_from_path("/api/demo/pane-snapshot.ansi"),
            Some("demo")
        );
        assert_eq!(
            project_slug_from_path("/api/demo/s7/pane-snapshot.ansi"),
            Some("demo")
        );
        // Live terminal sockets.
        assert_eq!(project_slug_from_path("/ws/demo/pty"), Some("demo"));
        assert_eq!(project_slug_from_path("/ws/demo/s7/pty"), Some("demo"));

        // NOT project-addressed: the collection, the import action, the
        // versioned tree, the docs UI, the identity-scoped chat socket, and
        // anything outside these prefixes.
        assert_eq!(project_slug_from_path("/api/v1/projects"), None);
        assert_eq!(project_slug_from_path("/api/v1/projects/"), None);
        assert_eq!(project_slug_from_path("/api/v1/projects/import"), None);
        assert_eq!(project_slug_from_path("/api/v1/status"), None);
        assert_eq!(project_slug_from_path("/api/v1/sessions/s1/turn"), None);
        assert_eq!(project_slug_from_path("/api/docs"), None);
        assert_eq!(
            project_slug_from_path("/api/docs/scalar-standalone.js"),
            None
        );
        assert_eq!(project_slug_from_path("/ws/chat"), None);
        assert_eq!(project_slug_from_path("/app/chat/s/s1"), None);
        assert_eq!(project_slug_from_path("/health"), None);
    }

    #[test]
    fn bare_hex_strips_only_the_wire_prefix() {
        assert_eq!(bare_hex("ccteam:deadbeef"), Some("deadbeef"));
        assert_eq!(bare_hex("deadbeef"), None);
        assert_eq!(bare_hex("Bearer ccteam:x"), None);
    }

    #[test]
    fn can_see_owner_admin_sees_non_tenant_tenant_sees_own() {
        // 档1 owner ruling: the admin/operator does NOT see a per-user tenant's
        // projects (`user:<id>`), but DOES see its own pool, unowned, + IM-owned.
        let admin = Identity::admin();
        assert_eq!(admin.owner_tag(), "user:web-api");
        assert!(admin.can_see_owner(Some("user:web-api")), "own admin pool");
        assert!(admin.can_see_owner(None), "unowned legacy/CLI project");
        assert!(admin.can_see_owner(Some("telegram:123")), "IM-owned");
        assert!(
            !admin.can_see_owner(Some("user:u1")),
            "a tenant's project is PRIVATE from admin"
        );

        let t = Identity::tenant("u1".to_string());
        assert_eq!(t.owner_tag(), "user:u1");
        assert!(t.can_see_owner(Some("user:u1")), "owns it");
        assert!(!t.can_see_owner(Some("user:u2")), "another tenant's");
        assert!(!t.can_see_owner(None), "legacy/admin-pool project");
        assert!(!t.can_see_owner(Some("telegram:1")), "IM-owned");
        assert!(
            !t.can_see_owner(Some("user:web-api")),
            "the admin pool is not the tenant's"
        );
    }

    #[test]
    fn is_tenant_owned_only_for_per_user_tags() {
        assert!(ccteam_core::identity::is_tenant_owned(Some("user:u1")));
        assert!(ccteam_core::identity::is_tenant_owned(Some("user:abc123")));
        assert!(
            !ccteam_core::identity::is_tenant_owned(Some("user:web-api")),
            "shared admin pool"
        );
        assert!(
            !ccteam_core::identity::is_tenant_owned(Some("telegram:1")),
            "IM-owned"
        );
        assert!(!ccteam_core::identity::is_tenant_owned(None), "unowned");
    }

    #[test]
    fn resolve_identity_admin_tenant_and_unknown() {
        use ccteam_core::tenants::TenantRegistry;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("users");
        let mut reg = TenantRegistry::default();
        let alice = reg.add("alice");
        reg.save(&path).unwrap();

        // The bootstrap (admin) hex → the admin/owner identity.
        let admin = resolve_identity("adminhex", "adminhex", &path).unwrap();
        assert!(admin.is_admin);
        assert_eq!(admin.id, "admin");
        // A per-user token → that tenant (not admin).
        let t = resolve_identity(&alice.web_token, "adminhex", &path).unwrap();
        assert!(!t.is_admin);
        assert_eq!(t.id, alice.id);
        // Neither → None.
        assert!(resolve_identity("nope", "adminhex", &path).is_none());
    }
}
