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
//! ## CSRF defense (option a, per advisor)
//!
//! POST `/api/...` requires the `Authorization` header *always* (even
//! when a valid cookie is present). The browser-side template inlines
//! the token into htmx `hx-headers` only when `auth.enabled` so
//! same-origin htmx form-submits authenticate; cross-origin form-submit
//! cannot inject `Authorization` (header-allowlisted by the browser),
//! and cross-origin `fetch`/`XHR` triggers a CORS preflight which we
//! never allow. Result: a malicious page on attacker.example **cannot**
//! produce an authenticated POST against `/api/<slug>/btw`.
//!
//! XSS tradeoff: the token appears in an HTML attribute, not just an
//! HttpOnly cookie. If the dashboard is XSS'd the attacker can read
//! the token *and* fire same-origin fetches with the cookie, so
//! cookie-only would not save us. Inlining doesn't materially worsen
//! the threat model.
//!
//! Architecture refs: `docs/versions/v0-3/prd.md` §6.2.4 / §6.2.5 / §9.

use std::net::SocketAddr;

use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode, Uri},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::{
    cookie::{Cookie, SameSite},
    CookieJar,
};
use subtle::ConstantTimeEq;

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
/// Order of checks:
///
/// 1. If `auth.enabled = false` → pass through.
/// 2. Bearer header valid → pass through.
/// 3. Query string `?token=` present + valid → set HttpOnly cookie +
///    302 redirect to URI minus the `token` parameter.
/// 4. Cookie value valid → pass through.
/// 5. Else → 401 plain-text "auth required".
pub async fn auth_layer(
    State(app): State<AppState>,
    jar: CookieJar,
    req: Request,
    next: Next,
) -> Response {
    let auth = app.auth.clone();
    if !auth.enabled {
        return next.run(req).await;
    }
    let Some(expected) = auth.token.as_deref() else {
        // Defensive: enabled=true but no token loaded → fail closed.
        return (StatusCode::INTERNAL_SERVER_ERROR, "auth misconfigured").into_response();
    };

    // 1. Authorization header.
    if let Some(h) = req.headers().get(header::AUTHORIZATION) {
        if let Some(presented) = parse_bearer(h) {
            if validate_bearer(presented, expected) {
                return next.run(req).await;
            }
        }
    }

    // 2. URL shim — `?token=ccteam:<hex>` query → cookie + redirect.
    if let Some(q) = req.uri().query() {
        if let Some(presented) = query_token(q) {
            if validate_bearer(presented, expected) {
                let clean = uri_without_token(req.uri());
                let cookie = Cookie::build((COOKIE_NAME, expected.to_string()))
                    .http_only(true)
                    .same_site(SameSite::Strict)
                    .path("/")
                    .build();
                let jar = jar.add(cookie);
                return (jar, Redirect::to(&clean)).into_response();
            }
        }
    }

    // 3. Cookie carry-over for subsequent GETs / SSE.
    if let Some(cookie_val) = cookie_token(&jar) {
        if cookie_val.as_bytes().ct_eq(expected.as_bytes()).into() {
            return next.run(req).await;
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
}
