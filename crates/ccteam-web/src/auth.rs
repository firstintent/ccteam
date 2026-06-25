//! V0.3 M5.3 — token-based auth middleware for the ccteam web layer.
//!
//! ## Decision: whole-app gate (no read/write split)
//!
//! When [`AuthState::enabled`] is true, **every** route on the
//! stateful router (`/`, `/project/<slug>`, `/sse/...`,
//! `/screenshot/<slug>.png`, `/api/<slug>/...`) requires either:
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
//! 3. sets a HttpOnly `ccteam_token` cookie + 302-redirects to the
//!    same path minus the `token` query parameter,
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
    http::{header, HeaderValue, StatusCode, Uri},
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

/// Query-string parameter parsed by the URL shim.
pub const QUERY_PARAM: &str = "token";

/// Auth state attached to [`AppState`].
#[derive(Debug, Clone)]
pub struct AuthState {
    /// If false, the middleware passes every request through (the
    /// loopback default + explicit `--no-auth`).
    pub enabled: bool,
    /// Hex-encoded token (no `ccteam:` prefix). `None` when disabled.
    pub token: Option<String>,
}

impl AuthState {
    /// Auth-off (loopback default / `--no-auth` opt-out).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            token: None,
        }
    }

    /// Auth-on with the supplied hex token.
    pub fn enabled(token: String) -> Self {
        Self {
            enabled: true,
            token: Some(token),
        }
    }

    /// Wire-format token (`ccteam:<hex>`) for templates / docs. Returns
    /// `None` when auth is disabled.
    pub fn wire_token(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("{TOKEN_PREFIX}{t}"))
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
    /// The admin uses the shared `web-api` pool; a per-user tenant owns
    /// `web:<id>`. Mirrors the gateway's `web_owner_chat`.
    pub fn web_owner(&self) -> String {
        if self.is_admin {
            "web:web-api".to_string()
        } else {
            format!("web:{}", self.id)
        }
    }

    /// Whether this identity may see a resource owned by `owner` (a project's
    /// `ProjectState.owner` or a session owner). The admin/owner sees
    /// everything; a per-user tenant sees only what it owns (`web:<id>`).
    pub fn can_see_owner(&self, owner: Option<&str>) -> bool {
        self.is_admin || owner == Some(self.web_owner().as_str())
    }
}

/// `Some(403)` unless the caller is the admin/owner — the shared gate for every
/// admin-only route (user management, IM credentials, hosts, global config).
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

/// Extract `{slug}` from `/api/v1/projects/{slug}` or `…/{slug}/...`. `None` for
/// the bare collection path (`/api/v1/projects`) or any non-project path.
fn project_slug_from_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/v1/projects/")?;
    let slug = rest.split('/').next().unwrap_or(rest);
    if slug.is_empty() {
        None
    } else {
        Some(slug)
    }
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

/// Pull `?token=<value>` out of a query string (no decoding —
/// presented value is already a base set of ASCII hex + `ccteam:`
/// literal).
fn query_token(query: &str) -> Option<&str> {
    for part in query.split('&') {
        if let Some(rest) = part.strip_prefix("token=") {
            return Some(rest);
        }
    }
    None
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
/// 4. Bearer header valid → pass through (fallback for cookieless clients: API
///    callers, iOS-PWA where the cookie is dropped across the standalone switch).
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
    let Some(expected) = auth.token.as_deref() else {
        // Defensive: enabled=true but no token loaded → fail closed.
        return (StatusCode::INTERNAL_SERVER_ERROR, "auth misconfigured").into_response();
    };
    // v0.8.18 档1 — the bootstrap token → admin; per-user tokens → their tenant.
    let tenants = app.paths.tenants_json();

    // 1. URL shim — an explicit `?token=ccteam:<hex>` login link ALWAYS wins: it
    //    (re)establishes the session cookie so a fresh login replaces a stale
    //    one. The cookie stores the BARE hex presented (admin OR per-user), so
    //    the carry-over below resolves to the SAME identity.
    if let Some(q) = req.uri().query() {
        if let Some(presented) = query_token(q) {
            if let Some(bare) = bare_hex(presented) {
                if resolve_identity(bare, expected, &tenants).is_some() {
                    let clean = uri_without_token(req.uri());
                    let cookie = Cookie::build((COOKIE_NAME, bare.to_string()))
                        .http_only(true)
                        .same_site(SameSite::Strict)
                        .path("/")
                        .build();
                    let jar = jar.add(cookie);
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
    //    the admin pool (`web:web-api`) instead of `web:<tenant>`.
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
    if let Some(h) = req.headers().get(header::AUTHORIZATION) {
        if let Some(presented) = parse_bearer(h) {
            if let Some(bare) = bare_hex(presented) {
                if let Some(id) = resolve_identity(bare, expected, &tenants) {
                    // Own the token before the &mut borrow (it borrows req.headers).
                    let presented = presented.to_string();
                    req.extensions_mut().insert(id);
                    req.extensions_mut().insert(PresentedToken(presented));
                    return next.run(req).await;
                }
            }
        }
    }

    (StatusCode::UNAUTHORIZED, "auth required").into_response()
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
        assert_eq!(query_token("token=ccteam:abc"), Some("ccteam:abc"));
        assert_eq!(query_token("other=x&token=ccteam:abc"), Some("ccteam:abc"));
        assert_eq!(query_token("other=x"), None);
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
    fn bare_hex_strips_only_the_wire_prefix() {
        assert_eq!(bare_hex("ccteam:deadbeef"), Some("deadbeef"));
        assert_eq!(bare_hex("deadbeef"), None);
        assert_eq!(bare_hex("Bearer ccteam:x"), None);
    }

    #[test]
    fn can_see_owner_admin_sees_all_tenant_sees_own() {
        let admin = Identity::admin();
        assert_eq!(admin.web_owner(), "web:web-api");
        assert!(admin.can_see_owner(None));
        assert!(admin.can_see_owner(Some("web:u1")));
        assert!(admin.can_see_owner(Some("telegram:123")));

        let t = Identity::tenant("u1".to_string());
        assert_eq!(t.web_owner(), "web:u1");
        assert!(t.can_see_owner(Some("web:u1")), "owns it");
        assert!(!t.can_see_owner(Some("web:u2")), "another tenant's");
        assert!(!t.can_see_owner(None), "legacy/admin-pool project");
        assert!(!t.can_see_owner(Some("telegram:1")), "IM-owned");
    }

    #[test]
    fn resolve_identity_admin_tenant_and_unknown() {
        use ccteam_core::tenants::TenantRegistry;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tenants.json");
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
