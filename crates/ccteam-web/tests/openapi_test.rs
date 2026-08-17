//! v0.8.7 W5 (Item E) — OpenAPI auto-docs tests.
//!
//! Two guarantees:
//!
//! 1. **Route ↔ spec drift** ([`spec_covers_every_api_v1_route`]): the
//!    generated spec is built from the SAME `OpenApiRouter` registrations
//!    that serve traffic (`routes::openapi::openapi_spec()`), so this test
//!    asserts the exact operation COUNT and every expected `(method, path)`
//!    pair. Adding a `/api/v1` route without a `#[utoipa::path]` (or
//!    forgetting to register it in `openapi::build_api_v1`) drops the count
//!    and fails here; adding one to the spec without wiring the route does
//!    the same.
//! 2. **Serve + auth** ([`openapi_json_and_docs_served_under_auth`]): with
//!    the same web-token auth gate as every other `/api/v1` route (DE.3),
//!    `GET /api/v1/openapi.json` returns a valid OpenAPI 3.x document with
//!    a non-empty `paths`, and `GET /api/docs` returns 200 HTML (Scalar).
//!    Both 401 without the token.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use ccteam_core::CcteamPaths;
use ccteam_web::routes::openapi::openapi_spec;
use ccteam_web::{router_with_state, AppState, AuthState};
use tempfile::TempDir;
use tokio::net::TcpListener;

const TOKEN_HEX: &str = "deadbeefcafef00ddeadbeefcafef00d";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

/// The complete, FINAL `/api/v1` operation set as of v0.8.7 (the route
/// table is frozen for this version). Kept as `(METHOD, path)` pairs with
/// utoipa's `{param}` path syntax. If a route is added/removed, update
/// BOTH this list and the registration in `routes::openapi` — that dual
/// edit is the point (it makes silent drift impossible).
fn expected_operations() -> BTreeSet<(&'static str, &'static str)> {
    [
        ("GET", "/api/v1/capabilities"),
        // Spawn-tuning discovery: per-vendor model ids + effort ladders.
        ("GET", "/api/v1/models"),
        // v0.8.18 柱1 — host-keyed agent report (list / detail / register-mcp).
        ("GET", "/api/v1/hosts"),
        ("GET", "/api/v1/hosts/{host}"),
        ("POST", "/api/v1/hosts/{host}/register-mcp"),
        // TEAM-5 — deregister a satellite (drop its registry record).
        ("DELETE", "/api/v1/hosts/{host}"),
        // VENDOR-INSTALL-1 — admin one-click vendor install/update jobs.
        ("POST", "/api/v1/hosts/{host}/vendors/{vendor}/install"),
        (
            "GET",
            "/api/v1/hosts/{host}/vendors/{vendor}/install/{job_id}",
        ),
        ("POST", "/api/v1/hosts/join-token"),
        ("GET", "/api/v1/hosts/join-token"),
        ("POST", "/api/v1/hosts/join"),
        // v0.9.0 reverse-connection: the satellite keepalive is a WS control
        // channel (`GET /api/v1/hosts/channel` + `…/hosts/exec/{nonce}`),
        // mounted as plain routes — WS upgrades are not OpenAPI operations,
        // so they are intentionally absent here.
        // v0.8.18 档1 — per-user web tenant management (admin-gated).
        ("POST", "/api/v1/users"),
        ("GET", "/api/v1/users"),
        ("DELETE", "/api/v1/users/{id}"),
        // v0.8.20 F3 — admin re-reveals a tenant's personal login link.
        ("GET", "/api/v1/users/{id}/link"),
        // v0.8.20 F2 — per-user IM bot config (self-serve + admin).
        ("PUT", "/api/v1/me/im"),
        ("GET", "/api/v1/me/im/lark/open-id-candidates"),
        ("PUT", "/api/v1/me/im/lark/allowed-users"),
        // Telegram twin — a per-tenant bot is fail-closed, so the tenant needs
        // a way to discover and bind its own chat id.
        ("GET", "/api/v1/me/im/telegram/chat-id-candidates"),
        ("PUT", "/api/v1/me/im/telegram/allowed-chats"),
        ("PUT", "/api/v1/users/{id}/im"),
        // v0.8.9 Phase 4 — daemon-wide status snapshot (cost pill + Status view).
        ("GET", "/api/v1/status"),
        // v0.9.15 DSHWEB — DSH web companion lifecycle.
        ("GET", "/api/v1/dsh/status"),
        ("POST", "/api/v1/dsh/start"),
        ("POST", "/api/v1/dsh/stop"),
        // v0.8.18 档1 — caller identity (SPA branches admin-only surfaces).
        ("GET", "/api/v1/me"),
        // v0.8.24 — admin self-serve web-token rotation.
        ("POST", "/api/v1/me/reset-token"),
        // projects
        ("GET", "/api/v1/projects"),
        ("POST", "/api/v1/projects"),
        ("POST", "/api/v1/projects/import"),
        ("GET", "/api/v1/projects/{slug}"),
        ("DELETE", "/api/v1/projects/{slug}"),
        ("GET", "/api/v1/projects/{slug}/sessions/{sid}"),
        ("GET", "/api/v1/auth/token"),
        // workflow panels
        ("GET", "/api/v1/projects/{slug}/artifact_queue"),
        ("GET", "/api/v1/projects/{slug}/artifact_status"),
        ("GET", "/api/v1/projects/{slug}/cost_history"),
        ("GET", "/api/v1/projects/{slug}/evolution"),
        ("GET", "/api/v1/projects/{slug}/sessions/active"),
        ("GET", "/api/v1/projects/{slug}/jobs/{job_id}/log"),
        ("GET", "/api/v1/sessions/active"),
        // v0.8.24 F1.12 — project third-party MCP servers.
        ("GET", "/api/v1/projects/{slug}/mcp-servers"),
        ("POST", "/api/v1/projects/{slug}/mcp-servers"),
        // roles
        ("GET", "/api/v1/projects/{slug}/roles"),
        ("GET", "/api/v1/projects/{slug}/roles/{role}"),
        ("PUT", "/api/v1/projects/{slug}/roles/{role}"),
        // v0.9.11 TEAM-2 — division-of-labor charter (routing.md)
        ("GET", "/api/v1/projects/{slug}/routing"),
        ("PUT", "/api/v1/projects/{slug}/routing"),
        // composer attachments — project uploads + installed-skill picker
        ("POST", "/api/v1/projects/{slug}/uploads"),
        ("GET", "/api/v1/projects/{slug}/uploads/{name}"),
        ("GET", "/api/v1/projects/{slug}/skills"),
        ("GET", "/api/v1/skills"),
        // sessions (gateway spine)
        ("GET", "/api/v1/projects/{slug}/sessions"),
        ("POST", "/api/v1/projects/{slug}/sessions"),
        ("GET", "/api/v1/sessions/{sid}"),
        // v0.8.22 P1 — session-title system: rename a session's title.
        ("PATCH", "/api/v1/sessions/{sid}"),
        // live statusline (model + context-window usage) for the SPA top bar.
        ("GET", "/api/v1/sessions/{sid}/status"),
        ("POST", "/api/v1/sessions/{sid}/turn"),
        // v0.8.7 review-fix (R-H1) — web HITL token-resolve endpoint.
        ("POST", "/api/v1/sessions/{sid}/resolve"),
        ("GET", "/api/v1/sessions/{sid}/events"),
        ("POST", "/api/v1/sessions/{sid}/stop"),
        ("GET", "/api/v1/sessions/{sid}/scheduled"),
        ("POST", "/api/v1/sessions/{sid}/scheduled"),
        ("DELETE", "/api/v1/sessions/{sid}/scheduled/{id}"),
        // v0.8.19 — interrupt the running turn (non-destructive; keeps session).
        ("POST", "/api/v1/sessions/{sid}/interrupt"),
        // v0.8.21 — history resume + external session import.
        ("GET", "/api/v1/projects/{slug}/sessions/history"),
        ("POST", "/api/v1/projects/{slug}/sessions/{sid}/resume"),
        ("GET", "/api/v1/projects/{slug}/external-sessions"),
        ("POST", "/api/v1/projects/{slug}/sessions/import"),
        // v0.8.8 F4 — IM credential config (masked read + validate-before-persist).
        ("GET", "/api/v1/config/im"),
        ("PUT", "/api/v1/config/im/telegram"),
        ("POST", "/api/v1/config/im/telegram/chat-id/start"),
        ("GET", "/api/v1/config/im/telegram/chat-id"),
        ("PUT", "/api/v1/config/im/lark"),
        // v0.8.9 Phase 2 — ccteam-hub plugin marketplace.
        ("GET", "/api/v1/marketplace"),
        ("GET", "/api/v1/marketplace/{id}/body"),
        ("GET", "/api/v1/projects/{slug}/marketplace"),
        ("POST", "/api/v1/projects/{slug}/marketplace/install"),
        // v0.9.0 W4 — team visualization: graph snapshot + global SSE.
        ("GET", "/api/v1/agents/graph"),
        ("GET", "/api/v1/agents/events"),
        // Enrollment credentials for external / hand-started agents (mint +
        // ready-to-paste vendor snippets, redacted list, revoke). The
        // project-scoped mint sits under `/projects/{slug}/` on purpose: that
        // path shape is what `auth::project_acl_layer` gates.
        ("GET", "/api/v1/enroll"),
        ("POST", "/api/v1/enroll"),
        ("POST", "/api/v1/projects/{slug}/enroll"),
        ("DELETE", "/api/v1/enroll/{id}"),
    ]
    .into_iter()
    .collect()
}

/// Enumerate every `(METHOD, path)` operation present in a generated spec.
/// utoipa 5's `PathItem` carries one `Option<Operation>` per HTTP method.
fn spec_operations(spec: &utoipa::openapi::OpenApi) -> BTreeSet<(&'static str, String)> {
    let mut out = BTreeSet::new();
    for (path, item) in &spec.paths.paths {
        for (method, present) in [
            ("GET", item.get.is_some()),
            ("POST", item.post.is_some()),
            ("PUT", item.put.is_some()),
            ("DELETE", item.delete.is_some()),
            ("PATCH", item.patch.is_some()),
            ("HEAD", item.head.is_some()),
            ("OPTIONS", item.options.is_some()),
            ("TRACE", item.trace.is_some()),
        ] {
            if present {
                out.insert((method, path.clone()));
            }
        }
    }
    out
}

#[test]
fn spec_covers_every_api_v1_route() {
    let spec = openapi_spec();
    let got: BTreeSet<(&'static str, String)> = spec_operations(&spec);
    let expected = expected_operations();

    // Exact count — a new route without a spec entry (or vice versa)
    // changes this and fails immediately.
    assert_eq!(
        got.len(),
        expected.len(),
        "operation count drift: spec has {}, expected {}.\n  spec: {:#?}",
        got.len(),
        expected.len(),
        got,
    );

    // Every expected (method, path) present.
    let got_pairs: BTreeSet<(&str, &str)> = got.iter().map(|(m, p)| (*m, p.as_str())).collect();
    for (method, path) in &expected {
        assert!(
            got_pairs.contains(&(*method, *path)),
            "spec is missing operation {method} {path}\n  spec has: {got_pairs:#?}",
        );
    }
    // And nothing extra leaked in (e.g. a non-/api/v1 path or a typo).
    for (method, path) in &got_pairs {
        assert!(
            path.starts_with("/api/v1"),
            "spec carries a non-/api/v1 operation {method} {path}",
        );
        assert!(
            expected.contains(&(*method, *path)),
            "spec carries an UNEXPECTED operation {method} {path} — \
             update expected_operations() AND routes::openapi if intentional",
        );
    }
}

#[test]
fn sse_events_are_text_event_stream() {
    // DE.4 — every SSE endpoint (per-session + v0.9.0 W4's global team-view
    // feed) can't be modeled as JSON; each must be declared
    // `text/event-stream` so an integrator knows not to expect a JSON body.
    let spec = openapi_spec();
    for sse_path in ["/api/v1/sessions/{sid}/events", "/api/v1/agents/events"] {
        let item = spec
            .paths
            .paths
            .get(sse_path)
            .unwrap_or_else(|| panic!("spec missing {sse_path}"));
        let op = item
            .get
            .as_ref()
            .unwrap_or_else(|| panic!("{sse_path} has no GET operation"));
        let resp = op
            .responses
            .responses
            .get("200")
            .unwrap_or_else(|| panic!("{sse_path} missing 200 response"));
        let resp = match resp {
            utoipa::openapi::RefOr::T(r) => r,
            utoipa::openapi::RefOr::Ref(_) => panic!("{sse_path} 200 is a $ref"),
        };
        assert!(
            resp.content.contains_key("text/event-stream"),
            "{sse_path} 200 must declare text/event-stream; got {:?}",
            resp.content.keys().collect::<Vec<_>>(),
        );
    }
}

#[tokio::test]
async fn openapi_json_and_docs_served_under_auth() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    // Auth ENABLED — same gate as every other /api/v1 route (DE.3).
    let state = AppState::with_auth(paths, AuthState::enabled(TOKEN_HEX.into()));
    let addr = spawn(state).await;
    let client = reqwest::Client::new();

    // ---- spec: 401 without token, valid OpenAPI 3.x with token ----
    let unauth = client
        .get(format!("http://{addr}/api/v1/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401, "spec must be behind the auth gate");

    let resp = client
        .get(format!("http://{addr}/api/v1/openapi.json"))
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let doc: serde_json::Value = resp.json().await.expect("openapi.json is valid JSON");
    let version = doc["openapi"].as_str().expect("openapi version field");
    assert!(
        version.starts_with("3."),
        "expected OpenAPI 3.x, got {version}",
    );
    let paths_obj = doc["paths"].as_object().expect("paths object");
    assert!(!paths_obj.is_empty(), "spec paths must be non-empty");
    assert!(
        paths_obj.contains_key("/api/v1/capabilities"),
        "spec should document /api/v1/capabilities",
    );

    // The web-local request/response structs that carry `#[derive(ToSchema)]`
    // must materialize into components.schemas — this proves the derive path
    // works for the shapes the recon flagged (`&'static str` fields on
    // DashboardRow/HarnessCapability, `Option<String>` on AuthToken, etc.).
    let schemas = doc["components"]["schemas"]
        .as_object()
        .expect("components.schemas object");
    for name in [
        "CapabilitiesResponse",
        // v0.8.9 Phase 4 — daemon-wide status snapshot response body.
        "StatusResponse",
        "HarnessCapability",
        "DashboardRow",
        "AuthToken",
        "JobLogResponse",
        "CreateProjectForm",
        "CreatedProject",
        "CreateSessionForm",
        "TurnForm",
        "ResolveForm",
        "RoleContentForm",
        // v0.8.9 Phase 2 — marketplace install request body.
        "InstallForm",
    ] {
        assert!(
            schemas.contains_key(name),
            "components.schemas should contain {name}; got {:?}",
            schemas.keys().collect::<Vec<_>>(),
        );
    }

    // ---- docs UI: 401 without token, 200 HTML with token ----
    let unauth_docs = client
        .get(format!("http://{addr}/api/docs"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        unauth_docs.status(),
        401,
        "docs UI must be behind the auth gate"
    );

    let docs = client
        .get(format!("http://{addr}/api/docs"))
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(docs.status(), 200);
    let ct = docs
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/html"), "docs UI must be HTML; got {ct}");
    let body = docs.text().await.unwrap();
    assert!(
        body.to_lowercase().contains("<!doctype html") || body.contains("<html"),
        "docs UI body should be an HTML document",
    );

    // v0.8.7 review-fix (R-M5) — the docs page must be SELF-HOSTED: no external
    // CDN <script>. Assert there is no `cdn.jsdelivr.net` (the old loader) and
    // no scheme-relative/absolute external script host at all — the only loader
    // is the same-origin vendored route.
    assert!(
        !body.contains("cdn.jsdelivr.net"),
        "docs UI must NOT load Scalar from cdn.jsdelivr.net (R-M5); body:\n{body}",
    );
    assert!(
        !body.contains("https://") && !body.contains("http://") && !body.contains("src=\"//"),
        "docs UI must have NO external/absolute script host (air-gapped); body:\n{body}",
    );
    assert!(
        body.contains("/api/docs/scalar-standalone.js"),
        "docs UI must load the vendored same-origin Scalar JS; body:\n{body}",
    );

    // The vendored JS itself: behind auth (401 without token), 200 JS with it,
    // and it's the real Scalar standalone build (self-init marker present).
    let unauth_js = client
        .get(format!("http://{addr}/api/docs/scalar-standalone.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        unauth_js.status(),
        401,
        "vendored JS is behind the auth gate"
    );

    let js = client
        .get(format!("http://{addr}/api/docs/scalar-standalone.js"))
        .header("Authorization", format!("Bearer ccteam:{TOKEN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(js.status(), 200);
    let js_ct = js
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        js_ct.contains("javascript"),
        "vendored Scalar must be served as javascript; got {js_ct}",
    );
    let js_body = js.text().await.unwrap();
    assert!(
        js_body.len() > 100_000,
        "vendored Scalar standalone build should be the full bundle (got {} bytes)",
        js_body.len(),
    );
    assert!(
        js_body.contains("createApiReference"),
        "vendored bundle must be the real Scalar standalone build",
    );
}

/// v0.8.7 review-fix (R-M5) — pure check that the generated docs HTML carries
/// the local loader and the `$spec` placeholder Scalar substitutes, and that
/// the pinned vendored version constant is wired (so the refresh chore has a
/// single SoT to bump). No server needed.
#[test]
fn docs_html_is_self_hosted_and_version_pinned() {
    use ccteam_web::routes::openapi::{SCALAR_JS, SCALAR_JS_PATH, SCALAR_VERSION};
    // The vendored bundle is non-empty + the real standalone build.
    assert!(
        SCALAR_JS.len() > 1_000_000,
        "vendored Scalar bundle looks truncated ({} bytes)",
        SCALAR_JS.len(),
    );
    assert!(SCALAR_JS.contains("createApiReference"));
    assert_eq!(SCALAR_VERSION, "1.58.0", "pinned Scalar version constant");
    assert_eq!(SCALAR_JS_PATH, "/api/docs/scalar-standalone.js");
}
