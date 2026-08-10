//! Enrollment credentials over REST — the console's "copy an MCP config an
//! EXTERNAL agent can paste" surface (`routes/enroll.rs`).
//!
//! Proves the four properties that make the copy button safe and useful:
//! 1. a minted bearer really authenticates (`enroll::verify_in`) and its
//!    snippets are the real vendor dialects, pointing at the host the operator
//!    was browsing — NOT `127.0.0.1`, which no other machine can reach;
//! 2. a listing never carries a secret, in any form;
//! 3. a project-scoped mint is gated by the ACL LAYER, not by this route — it
//!    lives at `POST /api/v1/projects/{slug}/enroll` precisely so
//!    `auth::project_acl_layer` covers it by path shape, and the test asserts
//!    the layer's own rejection (identical to a sibling project route's, and
//!    delivered even for a request body the handler could never parse);
//! 4. revoke kills exactly one credential, and only its owner may revoke it.
//!
//! Isolation: every state seam here is `_in(root)`-injected via `CcteamPaths`
//! pointed at a tempdir (`fake_paths`), so nothing derives from `HOME` /
//! `CCTEAM_HOME` and no test can write the real `~/.ccteam`.

use std::net::SocketAddr;

use ccteam_core::enroll;
use ccteam_core::tenants::TenantRegistry;
use ccteam_core::CcteamPaths;
use ccteam_web::{router_with_state, AppState, AuthState};
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
/// The host an operator would actually be browsing — deliberately not
/// loopback, since that is the whole point of the feature.
const BROWSED_HOST: &str = "box.example:7331";

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

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn seed_project(paths: &CcteamPaths, slug: &str, owner: &str) {
    let state_path = paths.project_state(slug);
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let mut st = ccteam_core::ProjectState::initial_for_team(slug.into(), "dev".into());
    st.owner = Some(owner.to_string());
    st.save(&state_path).unwrap();
}

/// POST a mint as `token`, presenting `BROWSED_HOST` as the browsed origin
/// (reqwest would otherwise send the loopback test address). `path` is either
/// the flat user-scoped route or a project-scoped one.
async fn post_mint(
    c: &reqwest::Client,
    addr: SocketAddr,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let res = c
        .post(format!("http://{addr}{path}"))
        .header("Authorization", format!("Bearer ccteam:{token}"))
        .header("Host", BROWSED_HOST)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = res.status();
    let json = res.json::<serde_json::Value>().await.unwrap_or_default();
    (status, json)
}

/// The USER-scoped mint: no project in the path, no scope discriminator in the
/// body — the route says which scope this is.
async fn mint_user(
    c: &reqwest::Client,
    addr: SocketAddr,
    token: &str,
    label: Option<&str>,
) -> (reqwest::StatusCode, serde_json::Value) {
    post_mint(
        c,
        addr,
        token,
        "/api/v1/enroll",
        serde_json::json!({"label": label}),
    )
    .await
}

/// The PROJECT-scoped mint: the slug is in the PATH, which is what the ACL
/// choke point matches on.
async fn mint_project(
    c: &reqwest::Client,
    addr: SocketAddr,
    token: &str,
    slug: &str,
    label: Option<&str>,
) -> (reqwest::StatusCode, serde_json::Value) {
    post_mint(
        c,
        addr,
        token,
        &format!("/api/v1/projects/{slug}/enroll"),
        serde_json::json!({"label": label}),
    )
    .await
}

fn snippet<'a>(minted: &'a serde_json::Value, vendor: &str) -> &'a serde_json::Value {
    minted["snippets"]
        .as_array()
        .expect("snippets[]")
        .iter()
        .find(|s| s["vendor"] == vendor)
        .unwrap_or_else(|| panic!("no {vendor} snippet in {minted:#?}"))
}

#[tokio::test]
async fn minted_bearer_verifies_and_snippets_carry_the_browsed_host() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let (status, minted) = mint_user(&c, addr, ADMIN_HEX, Some("rob's laptop")).await;
    assert_eq!(status, 201, "{minted:#?}");

    // 1. The bearer the console hands out is the one the daemon verifies.
    let bearer = minted["bearer"].as_str().expect("bearer").to_string();
    let cred = enroll::verify_in(&paths.root, &bearer).expect("minted bearer must verify");
    assert_eq!(cred.owner, "user:web-api", "admin mints into its own pool");
    assert_eq!(cred.scope, enroll::EnrollScope::User);
    assert_eq!(cred.label.as_deref(), Some("rob's laptop"));
    assert_eq!(minted["credential"]["id"], cred.id);
    assert_eq!(minted["credential"]["scope"], "user");

    // 2. The URL is the host the operator was browsing. An external machine
    //    cannot reach the daemon's own loopback bind, so a snippet naming it
    //    would be silently useless.
    assert_eq!(minted["url"], format!("http://{BROWSED_HOST}/mcp"));
    assert_eq!(
        minted["insecure_transport"], true,
        "plain HTTP off loopback"
    );

    // 3. Every dialect parses in its own format and carries url + credential.
    let claude = snippet(&minted, "claude");
    assert_eq!(claude["format"], "json");
    let claude_body: serde_json::Value = serde_json::from_str(claude["body"].as_str().unwrap())
        .expect("the Claude/Kimi dialect is JSON");
    assert_eq!(
        claude_body["mcpServers"]["ccteam"]["url"],
        format!("http://{BROWSED_HOST}/mcp")
    );
    assert_eq!(
        claude_body["mcpServers"]["ccteam"]["headers"]["Authorization"],
        format!("Bearer {bearer}")
    );
    // Kimi shares the `mcpServers` family and gets its own exact body (its file
    // schema has no `type` key), so a paste works for either vendor.
    let kimi_body: serde_json::Value =
        serde_json::from_str(snippet(&minted, "kimi")["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        kimi_body["mcpServers"]["ccteam"]["headers"]["Authorization"],
        format!("Bearer {bearer}")
    );

    let codex = snippet(&minted, "codex");
    assert_eq!(codex["format"], "toml");
    let codex_body = codex["body"].as_str().unwrap();
    // `codex_mcp_registered` PARSES the TOML and validates the shape ccteam
    // itself demands (`[mcp_servers.ccteam]` with `url` + `http_headers.
    // Authorization` in the current credential family, no legacy stdio
    // `command`). Parsing plus shape in one assertion, from the module that
    // owns the dialect — a snippet that fails this would not have worked.
    let codex_file = tmp.path().join("codex-snippet.toml");
    std::fs::write(&codex_file, codex_body).unwrap();
    assert!(
        ccteam_core::mcp_register::codex_mcp_registered(&codex_file),
        "codex snippet must be a valid registration:\n{codex_body}"
    );
    assert!(codex_body.contains("http_headers"), "{codex_body}");

    // Whatever the vendor, the credential + the reachable host are in there —
    // and the body is a config ccteam itself would recognise as its own
    // registration. That last check is what makes "the shapes live in ONE
    // place" enforceable: each predicate parses its own dialect and demands
    // the vendor's own header key, so a hand-rolled snippet would fail here.
    let vendors: Vec<String> = minted["snippets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["vendor"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        vendors,
        ["claude", "codex", "grok", "opencode", "kimi"],
        "every vendor whose config ccteam writes gets a snippet (pi's tool \
         surface is the managed-session bridge, so it has no config)"
    );
    for s in minted["snippets"].as_array().unwrap() {
        let vendor = s["vendor"].as_str().unwrap();
        let body = s["body"].as_str().unwrap();
        assert!(body.contains(&bearer), "{vendor} lost the bearer");
        assert!(body.contains(BROWSED_HOST), "{vendor} lost the host");
        assert!(
            !body.contains("127.0.0.1"),
            "{vendor} points at loopback, which no external agent can reach:\n{body}"
        );
        let file = tmp.path().join(format!("{vendor}-snippet"));
        std::fs::write(&file, body).unwrap();
        use ccteam_core::mcp_register as reg;
        let accepted = match vendor {
            "claude" => reg::claude_mcp_registered(&file),
            "codex" => reg::codex_mcp_registered(&file),
            "grok" => reg::grok_mcp_registered(&file),
            "opencode" => reg::opencode_mcp_registered(&file),
            "kimi" => reg::kimi_mcp_registered(&file),
            other => panic!("unknown dialect {other}"),
        };
        assert!(
            accepted,
            "{vendor} snippet is not a valid ccteam registration:\n{body}"
        );
    }

    // (Scratch-file hygiene — rendering writes real, credential-bearing config
    // files — is asserted deterministically by the unit test
    // `render_snippets_leaves_no_credential_bearing_file_behind`; counting a
    // shared /tmp from here would race every other concurrent test.)
}

/// The two transports that do NOT put the credential on the wire in clear
/// text: loopback (never leaves the box) and a TLS-terminating proxy in front
/// of the daemon. Both are read off the request, like the host itself.
#[tokio::test]
async fn transport_honesty_follows_the_request() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // Browsing over loopback: the operator IS on this machine.
    let (status, local) = mint_user(&c, addr, ADMIN_HEX, None).await;
    assert_eq!(status, 201);
    assert_eq!(
        local["insecure_transport"], true,
        "the baseline is plain HTTP"
    );

    let loopback: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/enroll"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .header("Host", "127.0.0.1:7331")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(loopback["url"], "http://127.0.0.1:7331/mcp");
    assert_eq!(
        loopback["insecure_transport"], false,
        "loopback bytes never leave the machine"
    );

    // Behind a TLS-terminating reverse proxy the snippet must dial https, or
    // the pasting agent would downgrade a connection that actually works.
    let proxied: serde_json::Value = c
        .post(format!("http://{addr}/api/v1/enroll"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .header("Host", "team.example")
        .header("X-Forwarded-Proto", "https")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(proxied["url"], "https://team.example/mcp");
    assert_eq!(proxied["insecure_transport"], false);
    for s in proxied["snippets"].as_array().unwrap() {
        assert!(
            s["body"]
                .as_str()
                .unwrap()
                .contains("https://team.example/mcp"),
            "{} kept the wrong scheme: {}",
            s["vendor"],
            s["body"]
        );
    }
}

#[tokio::test]
async fn listing_never_returns_a_secret() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    seed_project(&paths, "alpha", "user:web-api");
    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let (_, first) = mint_user(&c, addr, ADMIN_HEX, None).await;
    let (_, second) = mint_project(&c, addr, ADMIN_HEX, "alpha", Some("reviewer")).await;
    let secrets: Vec<String> = [&first, &second]
        .iter()
        .map(|m| {
            let bearer = m["bearer"].as_str().unwrap();
            enroll::verify_in(&paths.root, bearer).unwrap().secret
        })
        .collect();

    let res = c
        .get(format!("http://{addr}/api/v1/enroll"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let raw = res.text().await.unwrap();

    for secret in &secrets {
        assert!(
            !raw.contains(secret.as_str()),
            "a listing leaked a secret:\n{raw}"
        );
    }
    let listed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let rows = listed["credentials"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listed:#?}");
    // The ops-visible half IS there (that is what makes revoke usable), and the
    // project-scoped row still says which workspace it pins.
    assert!(rows.iter().any(|r| r["id"] == first["credential"]["id"]));
    let scoped = rows.iter().find(|r| r["scope"] == "project").unwrap();
    assert_eq!(scoped["project"], "alpha");
    assert_eq!(scoped["label"], "reviewer");
    assert_eq!(
        scoped["bearer_prefix"],
        format!("ccteam-enroll:{}:", scoped["id"].as_str().unwrap()),
        "the prefix stops before any secret byte"
    );
}

/// The project-scoped mint is gated by `auth::project_acl_layer`, not by this
/// route. That is asserted three ways, because "the handler happens to return
/// the same 404" is exactly the illusion this route shape exists to remove:
/// (a) the rejection is byte-identical to a SIBLING project-addressed route's,
/// (b) it arrives even for a body the handler's `Json<…>` extractor could never
///     parse — so the request never reached the handler at all,
/// (c) it applies in both directions (tenant → admin's project, admin → a
///     tenant's project), which is `can_see_owner`, not anything local here.
#[tokio::test]
async fn project_scoped_mint_is_gated_by_the_project_acl_layer() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();

    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();
    let tenant_owner = format!("user:{}", tenant.id);

    seed_project(&paths, "adminproj", "user:web-api");
    seed_project(&paths, "aliceproj", &tenant_owner);

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    // (a) Same rejection as a sibling `/api/v1/projects/{slug}/*` route. If the
    //     layer's shape ever changes, BOTH move together — nothing here to keep
    //     in sync by hand.
    let sibling = c
        .get(format!(
            "http://{addr}/api/v1/projects/adminproj/mcp-servers"
        ))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .send()
        .await
        .unwrap();
    let sibling_status = sibling.status();
    let sibling_body: serde_json::Value = sibling.json().await.unwrap();
    let (status, body) = mint_project(&c, addr, &tenant_tok, "adminproj", None).await;
    assert_eq!(status, sibling_status, "{body:#?}");
    assert_eq!(body, sibling_body, "the LAYER's rejection shape wins");
    assert_eq!(status, 404, "404, never 403 — existence is not revealed");
    assert_eq!(body["error"], "project not found: adminproj");

    // (b) An unparseable body still gets the layer's 404, not a 400/415 from the
    //     extractor: proof the gate ran BEFORE the handler.
    let garbage = c
        .post(format!("http://{addr}/api/v1/projects/adminproj/enroll"))
        .header("Authorization", format!("Bearer ccteam:{tenant_tok}"))
        .header("Content-Type", "application/json")
        .body("this is not json")
        .send()
        .await
        .unwrap();
    assert_eq!(
        garbage.status(),
        404,
        "the layer answers before the handler"
    );
    assert_eq!(
        garbage.json::<serde_json::Value>().await.unwrap(),
        sibling_body
    );

    // (c) Both directions, plus an unknown slug.
    let (status, body) = mint_project(&c, addr, ADMIN_HEX, "aliceproj", None).await;
    assert_eq!(
        status, 404,
        "a tenant's workspace is private from admin: {body:#?}"
    );
    let (status, _) = mint_project(&c, addr, ADMIN_HEX, "ghost", None).await;
    assert_eq!(status, 404);
    // Nothing was minted along the way.
    assert!(enroll::list_in(&paths.root).is_empty());

    // Its owner may. The credential pins that ONE workspace, which is what
    // makes the snippet safe to hand out.
    let (status, minted) = mint_project(&c, addr, &tenant_tok, "aliceproj", None).await;
    assert_eq!(status, 201, "{minted:#?}");
    let cred = enroll::verify_in(&paths.root, minted["bearer"].as_str().unwrap()).unwrap();
    assert_eq!(cred.scope.project(), Some("aliceproj"));
    assert_eq!(cred.owner, tenant_owner, "sessions inherit the minter");
    assert_eq!(minted["credential"]["project"], "aliceproj");
    assert_eq!(minted["credential"]["scope"], "project");
    assert_eq!(enroll::list_in(&paths.root).len(), 1);

    // The tenant sees only its own credential in the listing.
    let listed: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/enroll"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        listed["credentials"].as_array().unwrap().len(),
        0,
        "a tenant's credential is private from the admin: {listed:#?}"
    );
}

#[tokio::test]
async fn revoke_kills_one_bearer_and_only_for_its_owner() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let mut reg = TenantRegistry::default();
    let tenant = reg.add("alice");
    reg.save(&paths.users_dir()).unwrap();
    let tenant_tok = tenant.web_token.clone();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let (_, doomed) = mint_user(&c, addr, ADMIN_HEX, None).await;
    let (_, keeper) = mint_user(&c, addr, ADMIN_HEX, Some("keep")).await;
    let doomed_bearer = doomed["bearer"].as_str().unwrap().to_string();
    let keeper_bearer = keeper["bearer"].as_str().unwrap().to_string();
    let doomed_id = doomed["credential"]["id"].as_str().unwrap().to_string();

    let del = |token: String, id: String| {
        let c = c.clone();
        async move {
            c.delete(format!("http://{addr}/api/v1/enroll/{id}"))
                .header("Authorization", format!("Bearer ccteam:{token}"))
                .send()
                .await
                .unwrap()
                .status()
        }
    };

    // Another identity's credential is indistinguishable from a missing one,
    // and stays alive.
    assert_eq!(del(tenant_tok.clone(), doomed_id.clone()).await, 404);
    assert!(enroll::verify_in(&paths.root, &doomed_bearer).is_some());

    assert_eq!(del(ADMIN_HEX.to_string(), doomed_id.clone()).await, 200);
    assert!(
        enroll::verify_in(&paths.root, &doomed_bearer).is_none(),
        "a revoked bearer must stop verifying"
    );
    assert!(
        enroll::verify_in(&paths.root, &keeper_bearer).is_some(),
        "revoking one must not disturb another"
    );
    // Idempotent from the caller's side: gone is gone.
    assert_eq!(del(ADMIN_HEX.to_string(), doomed_id).await, 404);

    let listed: serde_json::Value = c
        .get(format!("http://{addr}/api/v1/enroll"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = listed["credentials"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["label"], "keep");
}

/// A request whose `Host` cannot be spliced into a URL (crafted, or absent)
/// falls back to the bind the daemon recorded instead of embedding garbage in a
/// config the operator is about to paste.
#[tokio::test]
async fn a_crafted_host_falls_back_to_the_recorded_bind() {
    let tmp = tempfile::TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    std::fs::create_dir_all(&paths.root).unwrap();
    let run_dir = paths.root.join("run");
    ccteam_harness::execution::mcp_config::record_daemon_mcp_url(&run_dir, "0.0.0.0:9099").unwrap();

    let state = AppState::with_auth(paths.clone(), AuthState::enabled(ADMIN_HEX.into()));
    let addr = spawn(state).await;
    let c = client();

    let res = c
        .post(format!("http://{addr}/api/v1/enroll"))
        .header("Authorization", format!("Bearer ccteam:{ADMIN_HEX}"))
        .header("Host", "evil.example/mcp?x=1")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let minted: serde_json::Value = res.json().await.unwrap();
    // Same resolution chain the daemon uses everywhere else (computed here so
    // the assertion holds whatever the ambient env override is).
    assert_eq!(
        minted["url"],
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&run_dir)
    );
    assert!(
        !minted["url"].as_str().unwrap().contains("evil.example"),
        "a crafted Host must never reach the snippet: {minted:#?}"
    );
}
