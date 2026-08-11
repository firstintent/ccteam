//! `POST /mcp` refuses the WEB-TOKEN family outright — the admin token and a
//! tenant's token alike.
//!
//! This file used to assert the opposite (a tenant's console bearer reaching the
//! same MCP ingress without being promoted to admin). What changed is not one
//! family's strictness but the shape of the data plane: ccteam wrote a durable
//! web token into all five vendors' GLOBAL MCP configs so any hand-started agent
//! could orchestrate — and a vendor's global config is ONE static file shared by
//! every process that vendor ever starts, so what it carries cannot say which
//! caller is speaking. Measured: two `codex` runs in different repos
//! authenticated as the same machine-wide caller, neither could be a delegation
//! parent, and their `session_spawn` children mounted as ROOTS in a project
//! nobody had named. A credential a static file can carry must therefore grant
//! nothing by itself, so the tier it bought is deleted rather than narrowed. The
//! console token keeps its own job: `/api/v1/**`, cookies, the SPA.
//!
//! The fixture stays here because it is the one that can prove this for BOTH web
//! identities at the real route boundary: a LIVE admin token out of `AuthState`
//! (not a guessed hex, so the 401 is about the family and not about the token
//! being unknown) and a real tenant from the registry.

use std::net::SocketAddr;

use ccteam_core::enroll::{self, EnrollScope};
use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use tempfile::TempDir;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn(state: AppState) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router_with_state(state))
            .await
            .unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn post_mcp(addr: SocketAddr, bearer: &str, body: serde_json::Value) -> reqwest::Response {
    client()
        .post(format!("http://{addr}/mcp"))
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// The two web identities this endpoint must refuse: a valid admin token and a
/// registered tenant's token. No env pinning is needed — every state seam here
/// (`users_dir`, project state, the enrollment record) is `_in(root)`-injected
/// under the tempdir, so nothing can reach the real `~/.ccteam`.
struct Fixture {
    addr: SocketAddr,
    paths: CcteamPaths,
    admin: String,
    tenant: String,
}

async fn fixture(tmp: &TempDir) -> Fixture {
    let paths = fake_paths(tmp.path());
    let mut tenants = TenantRegistry::default();
    let tenant = tenants.add("alice");
    tenants.save(&paths.users_dir()).unwrap();
    let admin_project = paths.project_state("admin-project");
    std::fs::create_dir_all(admin_project.parent().unwrap()).unwrap();
    let mut project = ccteam_core::ProjectState::initial("admin-project".into());
    project.owner = Some("user:web-api".into());
    project.save(&admin_project).unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    Fixture {
        addr,
        paths,
        admin: format!("ccteam:{ADMIN_HEX}"),
        tenant: format!("ccteam:{}", tenant.web_token),
    }
}

#[tokio::test]
async fn the_web_token_family_is_refused_for_both_identities_and_every_method() {
    let tmp = TempDir::new().unwrap();
    let fx = fixture(&tmp).await;

    // Method-independent on purpose: this is a FAMILY refusal, not a per-method
    // permission. `permission/ask` is in the list because it is the internal HITL
    // bus — a human's console credential must not be able to speak on it at all,
    // which the 401 now guarantees more strongly than the old "not exposed on the
    // front door" answer did.
    for (who, bearer) in [("admin", &fx.admin), ("tenant", &fx.tenant)] {
        for body in [
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                               "params":{"name":"session_list","arguments":{}}}),
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"permission/ask","params":{}}),
        ] {
            let method = body["method"].as_str().unwrap().to_string();
            let resp = post_mcp(fx.addr, bearer, body).await;
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "{who} web token must be refused at /mcp for {method}"
            );
        }
    }

    // …and the fixture is not simply broken: a credential of an ACCEPTED family,
    // minted under the same root, gets through.
    let cred = enroll::mint_in(&fx.paths.root, EnrollScope::User, "user:web-api", None).unwrap();
    let ok = post_mcp(
        fx.addr,
        &cred.bearer(),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await;
    assert_eq!(
        ok.status(),
        reqwest::StatusCode::OK,
        "an enrollment credential must still authenticate"
    );
}

/// A 401 that only says "auth required" costs the operator a debugging session:
/// the endpoint's whole point is that two specific credential families work, so
/// the refusal has to name them and say where one comes from.
#[tokio::test]
async fn the_401_names_both_accepted_families_and_where_to_get_one() {
    let tmp = TempDir::new().unwrap();
    let fx = fixture(&tmp).await;

    let resp = post_mcp(
        fx.addr,
        &fx.admin,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json().await.unwrap();
    let message = body["error"].as_str().unwrap_or_default().to_string();
    for expected in [
        // the two accepted families, in their exact wire form
        "ccteam-sid:<sid>:<secret>",
        "ccteam-enroll:<id>:<secret>",
        // the header an enrolled client must echo after `initialize`
        "Mcp-Session-Id",
        // where an enrollment credential comes from: the CLI writes one per
        // vendor, the console mints a project-scoped one
        "ccteam config",
        "POST /api/v1/projects/{slug}/enroll",
        // and what the credential in hand IS good for, so the operator stops
        // trying it here
        "/api/v1/**",
    ] {
        assert!(
            message.contains(expected),
            "the 401 must mention `{expected}`; got: {message}"
        );
    }
}

/// `DELETE /mcp` runs the same gate before it looks at anything else, so a web
/// token gets the family refusal rather than the "only an enrolled client can end
/// a session" 405 — which would have implied the credential was otherwise fine.
#[tokio::test]
async fn delete_with_a_web_token_is_401_not_405() {
    let tmp = TempDir::new().unwrap();
    let fx = fixture(&tmp).await;

    for bearer in [&fx.admin, &fx.tenant] {
        let resp = client()
            .delete(format!("http://{}/mcp", fx.addr))
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
