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
        // projects
        ("GET", "/api/v1/projects"),
        ("POST", "/api/v1/projects"),
        ("GET", "/api/v1/projects/{slug}"),
        ("DELETE", "/api/v1/projects/{slug}"),
        ("GET", "/api/v1/projects/{slug}/sessions/{sid}"),
        ("GET", "/api/v1/auth/token"),
        // workflow panels
        ("GET", "/api/v1/projects/{slug}/artifact_queue"),
        ("GET", "/api/v1/projects/{slug}/artifact_status"),
        ("GET", "/api/v1/projects/{slug}/cost_history"),
        ("GET", "/api/v1/projects/{slug}/sessions/active"),
        ("GET", "/api/v1/projects/{slug}/jobs/{job_id}/log"),
        ("GET", "/api/v1/sessions/active"),
        // roles
        ("GET", "/api/v1/projects/{slug}/roles"),
        ("GET", "/api/v1/projects/{slug}/roles/{role}"),
        ("PUT", "/api/v1/projects/{slug}/roles/{role}"),
        // sessions (gateway spine)
        ("GET", "/api/v1/projects/{slug}/sessions"),
        ("POST", "/api/v1/projects/{slug}/sessions"),
        ("GET", "/api/v1/sessions/{sid}"),
        ("POST", "/api/v1/sessions/{sid}/turn"),
        ("GET", "/api/v1/sessions/{sid}/events"),
        ("POST", "/api/v1/sessions/{sid}/stop"),
        // teams
        ("GET", "/api/v1/teams"),
        ("GET", "/api/v1/teams/{name}"),
        ("GET", "/api/v1/teams/{name}/tasks"),
        ("GET", "/api/v1/teams/{name}/inbox"),
        ("GET", "/api/v1/teams/{name}/member/{teammate}/definition"),
        ("GET", "/api/v1/teams/{name}/events"),
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
    // DE.4 — the two SSE endpoints can't be modeled as JSON; they must be
    // declared `text/event-stream` so an integrator knows not to expect a
    // JSON body.
    let spec = openapi_spec();
    for sse_path in [
        "/api/v1/sessions/{sid}/events",
        "/api/v1/teams/{name}/events",
    ] {
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
        "HarnessCapability",
        "DashboardRow",
        "AuthToken",
        "JobLogResponse",
        "CreateProjectForm",
        "CreatedProject",
        "CreateSessionForm",
        "TurnForm",
        "RoleContentForm",
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
}
