//! v0.8.6 W5b ResDisk — resource API route integration tests.
//!
//! Each case fixtures a project under a tempdir-backed `CcteamPaths`,
//! spins a real axum listener (so the full `stateful_router` — including
//! the api_v1 GETs sharing paths with the new POST/DELETE — builds
//! without a route collision), fires the relevant request, and asserts
//! the status + body shape + disk side effect.
//!
//! These cover the disk/config-backed verbs only (the gateway is left
//! `None`, the standalone "internal web" path); the session endpoints +
//! their gateway-stop behaviour land in the next stage's smoke tests.
//! Auth is disabled (loopback default) so these focus on route logic.

use std::net::SocketAddr;

use ccteam_core::{
    bootstrap_project, disable_tool_surface_bootstrap_for_tests, write_role, CcteamPaths,
};
use ccteam_web::{router_with_state, AppState};
use tempfile::TempDir;
use tokio::net::TcpListener;

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

fn fixture_project(paths: &CcteamPaths, slug: &str) {
    disable_tool_surface_bootstrap_for_tests();
    bootstrap_project(paths, slug, "demo request", "dev").unwrap();
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

// ----------------------------- roles -----------------------------

#[tokio::test]
async fn get_roles_lists_agent_md_files() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    // v0.9.0 (engine neutralization): bootstrap seeds no role; author one with frontmatter.
    write_role(
        &paths.project_dir("demo"),
        "reviewer",
        "---\nname: reviewer\ndescription: Reviews diffs\nmodel: sonnet\n---\nbody\n",
    )
    .unwrap();

    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/demo/roles"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let arr: serde_json::Value = resp.json().await.unwrap();
    let roles = arr.as_array().unwrap();
    // only the authored `reviewer` role is listed (v0.9.0: no seeded cto).
    let names: Vec<&str> = roles
        .iter()
        .map(|r| r.get("role").unwrap().as_str().unwrap())
        .collect();
    assert!(names.contains(&"reviewer"), "got names: {names:?}");
    let reviewer = roles
        .iter()
        .find(|r| r.get("role").unwrap() == "reviewer")
        .unwrap();
    assert_eq!(reviewer.get("description").unwrap(), "Reviews diffs");
    assert_eq!(reviewer.get("model").unwrap(), "sonnet");
}

#[tokio::test]
async fn get_roles_unknown_project_404() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/ghost/roles"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn get_single_role_returns_frontmatter_and_body() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    write_role(
        &paths.project_dir("demo"),
        "reviewer",
        "---\nmodel: opus\n---\nYou review.\n",
    )
    .unwrap();
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/demo/roles/reviewer"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v.get("role").unwrap(), "reviewer");
    assert_eq!(v.get("frontmatter").unwrap().get("model").unwrap(), "opus");
    assert_eq!(v.get("body").unwrap(), "You review.\n");
}

#[tokio::test]
async fn get_single_role_unknown_role_404() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/projects/demo/roles/nope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// v0.8.6 (review-fix #1) — full HTTP-level traversal smoke for the
/// single-role GET. A percent-encoded `..%2f..%2f...` path param must
/// NOT escape `<project>/.claude/agents/` and leak an out-of-tree file;
/// it must come back 400 (handler guard) or 404 (axum path-normalization
/// at the router), never 200 carrying the target file's bytes. A normal
/// role on the same server still returns 200, proving the guard isn't a
/// blanket reject. Mirrors the `read_role` + `role_name_is_valid` unit
/// guards but exercises the real axum stack end to end.
#[tokio::test]
async fn get_single_role_rejects_path_traversal() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_dir = paths.project_dir("demo");
    // v0.9.0 (engine neutralization): bootstrap seeds no role. Author a real
    // `cto` role as a USER file so the positive control below (a normal role
    // reads 200) and the no-clobber existence check have a real file.
    write_role(&project_dir, "cto", "---\nname: cto\n---\nGuide.\n").unwrap();
    // Plant a real secret OUTSIDE the project's agents/ dir that a working
    // traversal would surface (sibling .md so even `<evil>.md` resolves).
    let secret_dir = tmp.path().join("outside");
    std::fs::create_dir_all(&secret_dir).unwrap();
    std::fs::write(secret_dir.join("secret.md"), "TOP-SECRET-CANARY\n").unwrap();
    let addr = spawn(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    // A normal role still reads fine (guard is not a blanket deny).
    let ok = client
        .get(format!("http://{addr}/api/v1/projects/demo/roles/cto"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200, "normal role must still 200");

    // Each of these is a percent-encoded traversal aimed at a file the
    // server can actually read; none may return 200, and none may echo
    // the canary or any out-of-tree file content.
    let evil_paths = [
        // up to /etc/passwd
        "..%2f..%2f..%2f..%2f..%2fetc%2fpasswd",
        // up to the user's ~/.claude/CLAUDE.md (the requirement's case)
        "..%2f..%2f.claude%2fCLAUDE.md",
        // at the planted sibling canary (relative to .claude/agents/)
        "..%2f..%2f..%2foutside%2fsecret",
        // backslash + leading-dot variants
        "..%5c..%5csecret",
        ".hidden",
    ];
    for evil in evil_paths {
        let url = format!("http://{addr}/api/v1/projects/demo/roles/{evil}");
        let resp = client.get(&url).send().await.unwrap();
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        assert!(
            status == 400 || status == 404,
            "traversal `{evil}` must be 400/404, got {status}; body={body}"
        );
        assert!(
            !body.contains("TOP-SECRET-CANARY") && !body.contains("root:"),
            "traversal `{evil}` leaked out-of-tree file content: {body}"
        );
    }
    // Ensure the project's real agents/ dir is intact (no clobber).
    assert!(project_dir.join(".claude/agents/cto.md").exists());
}

#[tokio::test]
async fn put_role_json_writes_file() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_dir = paths.project_dir("demo");
    let addr = spawn(AppState::new(paths)).await;

    let content = "---\nname: newbot\nmodel: haiku\n---\nfresh persona\n";
    let resp = reqwest::Client::new()
        .put(format!("http://{addr}/api/v1/projects/demo/roles/newbot"))
        .json(&serde_json::json!({ "content": content }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v.get("ok").unwrap(), true);
    let written = std::fs::read_to_string(project_dir.join(".claude/agents/newbot.md")).unwrap();
    assert_eq!(written, content);
}

#[tokio::test]
async fn put_role_raw_body_writes_file() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let project_dir = paths.project_dir("demo");
    let addr = spawn(AppState::new(paths)).await;

    let content = "---\nname: rawbot\n---\nraw markdown body\n";
    let resp = reqwest::Client::new()
        .put(format!("http://{addr}/api/v1/projects/demo/roles/rawbot"))
        .header("content-type", "text/markdown")
        .body(content)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let written = std::fs::read_to_string(project_dir.join(".claude/agents/rawbot.md")).unwrap();
    assert_eq!(written, content);
}

#[tokio::test]
async fn put_role_rejects_bad_name() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    fixture_project(&paths, "demo");
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::Client::new()
        .put(format!(
            "http://{addr}/api/v1/projects/demo/roles/Bad%20Name"
        ))
        .json(&serde_json::json!({ "content": "---\nx\n---\nbody\n" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --------------------------- capabilities ---------------------------

#[tokio::test]
async fn get_capabilities_lists_both_vendors() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::get(format!("http://{addr}/api/v1/capabilities"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    let harnesses = v.get("harnesses").unwrap().as_array().unwrap();
    // v0.8.24 — 4-way (claude / codex / grok / opencode); availability is
    // PATH-probe dynamic, but the list always surfaces every known harness.
    assert!(
        harnesses.len() >= 4,
        "expected ≥4 harnesses (claude/codex/grok/opencode), got {}",
        harnesses.len()
    );
    let ids: Vec<&str> = harnesses
        .iter()
        .map(|h| h.get("id").unwrap().as_str().unwrap())
        .collect();
    assert!(ids.contains(&"claude-code"));
    assert!(ids.contains(&"codex"));
    assert!(ids.iter().any(|id| id.contains("grok")));
    assert!(ids.iter().any(|id| id.contains("opencode")));
    // `available` is a bool; `providers` is an array (empty for now).
    for h in harnesses {
        assert!(h.get("available").unwrap().is_boolean());
        assert!(h.get("providers").unwrap().as_array().unwrap().is_empty());
        assert!(h.get("vendor").unwrap().is_string());
    }
}

// ------------------------------ models ------------------------------

/// `GET /api/v1/models` is the affordance side of `POST .../sessions`
/// accepting `model` + `effort`. Two properties matter to a client building
/// a spawn composer: an OBSERVED vendor reports its own ids with dated
/// provenance, and an UNOBSERVED vendor still gets a row — otherwise the
/// picker silently loses vendors that simply have not run yet.
#[tokio::test]
async fn get_models_reports_every_vendor_with_observed_and_fallback_rows() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    ccteam_core::model_catalog::record_vendor_models_in(
        &paths.root,
        "kimi",
        "ACP session availableModels",
        vec![ccteam_core::model_catalog::CatalogModel {
            id: "kimi-code/k3".to_string(),
            display_name: Some("K3".to_string()),
            efforts: vec!["low".to_string(), "high".to_string(), "max".to_string()],
        }],
    )
    .unwrap();
    let addr = spawn(AppState::new(paths)).await;

    let resp = reqwest::get(format!("http://{addr}/api/v1/models"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    let vendors = v["vendors"].as_array().expect("vendors array");
    let names: Vec<&str> = vendors
        .iter()
        .map(|e| e["vendor"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["claude", "codex", "grok", "opencode", "kimi"]);

    let kimi = vendors.iter().find(|e| e["vendor"] == "kimi").unwrap();
    assert_eq!(kimi["source"], "ACP session availableModels");
    assert!(
        kimi["observed_at"].as_str().unwrap().starts_with("20"),
        "observed row carries an RFC3339 capture time: {kimi}"
    );
    assert_eq!(kimi["models"][0]["id"], "kimi-code/k3");
    assert_eq!(kimi["models"][0]["display_name"], "K3");
    assert_eq!(
        kimi["models"][0]["efforts"],
        serde_json::json!(["low", "high", "max"])
    );
    assert_eq!(
        kimi["efforts"],
        serde_json::json!(["low", "high", "max"]),
        "vendor-level ladder is the union its own handshake declared"
    );

    // Never observed: an honest empty model list + null provenance, but the
    // CLI-verified effort ladder is still offered so the axis is discoverable
    // before the vendor's first session.
    let claude = vendors.iter().find(|e| e["vendor"] == "claude").unwrap();
    assert_eq!(claude["models"], serde_json::json!([]));
    assert!(claude["observed_at"].is_null(), "{claude}");
    assert!(claude["source"].is_null(), "{claude}");
    assert_eq!(
        claude["efforts"],
        serde_json::json!(["low", "medium", "high", "xhigh", "max"])
    );

    // OpenCode declares no effort axis: an empty ladder, never an invented one.
    let opencode = vendors.iter().find(|e| e["vendor"] == "opencode").unwrap();
    assert_eq!(opencode["efforts"], serde_json::json!([]));
}

// ----------------------------- projects -----------------------------

#[tokio::test]
async fn post_project_creates_and_registers() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let root = paths.root.clone();
    let target = tmp.path().join("adopted-repo");
    let addr = spawn(AppState::new(paths)).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/projects"))
        .json(&serde_json::json!({
            "slug": "myapp",
            "path": target.display().to_string(),
            "team": "dev",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v.get("slug").unwrap(), "myapp");
    assert_eq!(v.get("host").unwrap(), "local");
    assert_eq!(v.get("path").unwrap(), &target.display().to_string());

    // Side effects: state.json on disk + registered in config.yaml.
    assert!(target.join(".ccteam/state.json").exists());
    let entry = ccteam_core::lookup_project_in_config(&root, "myapp")
        .unwrap()
        .expect("myapp registered");
    assert_eq!(entry.path, target);
    assert_eq!(entry.host, "local");
    assert_eq!(entry.team, "dev");
}

#[tokio::test]
async fn post_project_rejects_bad_slug() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/projects"))
        .json(&serde_json::json!({ "slug": "Bad Slug", "path": "/tmp/x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn post_project_rejects_relative_path() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/v1/projects"))
        .json(&serde_json::json!({ "slug": "myapp", "path": "rel/dir" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn post_project_auto_appends_on_duplicate_slug() {
    // A duplicate slug does NOT 409 — `handle_create_project` AUTO-APPENDS a
    // numeric suffix (dup → dup2), the same rule `ccteam init` uses (red line:
    // "新建项目 slug = 目录名 + 数字累加"). The 201 body carries the slug actually
    // used. (409 is reserved for the pathological >999-collisions case.)
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let target = tmp.path().join("repo1");
    let addr = spawn(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("http://{addr}/api/v1/projects"))
        .json(&serde_json::json!({ "slug": "dup", "path": target.display().to_string() }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 201);

    let second = client
        .post(format!("http://{addr}/api/v1/projects"))
        .json(&serde_json::json!({ "slug": "dup", "path": target.display().to_string() }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 201);
    let created: serde_json::Value = second.json().await.unwrap();
    assert_eq!(
        created["slug"], "dup2",
        "duplicate slug auto-appends: {created}"
    );
}

#[tokio::test]
async fn import_remote_project_is_idempotent_and_surfaces_binding() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let mut registry = ccteam_core::HostRegistry::default();
    registry.upsert(ccteam_core::HostRecord {
        id: "sat-a".into(),
        hostname: "sat-a".into(),
        os: "linux".into(),
        arch: "aarch64".into(),
        ccteam_version: "0.9.2".into(),
        agent_token: "token".into(),
        last_heartbeat_unix: ccteam_core::now_unix(),
        agents: vec![],
        projects: vec![ccteam_core::HostProjectReport {
            slug: "wire-demo".into(),
            path: "/srv/work/demo".into(),
        }],
        joined_at: chrono::Utc::now().to_rfc3339(),
    });
    registry.save(&paths.host_registry_path()).unwrap();
    let root = paths.root.clone();
    let data_home = paths.projects_root.join("catalog-demo");
    let addr = spawn(AppState::new(paths)).await;
    let client = reqwest::Client::new();
    let request = serde_json::json!({
        "host": "sat-a",
        "remote_slug": "wire-demo",
        "slug": "catalog-demo",
    });
    let first = client
        .post(format!("http://{addr}/api/v1/projects/import"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 201);
    let created: serde_json::Value = first.json().await.unwrap();
    assert_eq!(created["slug"], "catalog-demo");
    assert_eq!(created["host"], "sat-a");
    assert_eq!(created["path"], data_home.display().to_string());
    let entry = ccteam_core::lookup_project_in_config(&root, "catalog-demo")
        .unwrap()
        .unwrap();
    assert_eq!(entry.remote_slug.as_deref(), Some("wire-demo"));
    assert_eq!(
        entry.remote_path.as_deref(),
        Some(std::path::Path::new("/srv/work/demo"))
    );
    assert!(data_home.join(".ccteam/state.json").is_file());
    assert!(!data_home.join("AGENTS.md").exists());

    let second = client
        .post(format!("http://{addr}/api/v1/projects/import"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    assert_eq!(
        ccteam_core::load_ccteam_config(&root)
            .unwrap()
            .projects
            .len(),
        1
    );

    let projects: serde_json::Value = client
        .get(format!("http://{addr}/api/v1/projects"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(projects[0]["host"], "sat-a");
    assert_eq!(projects[0]["host_online"], true);
    let host: serde_json::Value = client
        .get(format!("http://{addr}/api/v1/hosts/sat-a"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(host["projects"][0]["cataloged"], true);
    assert_eq!(host["projects"][0]["catalog_slug"], "catalog-demo");
}

#[tokio::test]
async fn delete_project_deregisters_without_gateway() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let root = paths.root.clone();
    let target = tmp.path().join("torm");
    let addr = spawn(AppState::new(paths)).await;
    let client = reqwest::Client::new();

    // Create then delete.
    client
        .post(format!("http://{addr}/api/v1/projects"))
        .json(&serde_json::json!({ "slug": "torm", "path": target.display().to_string() }))
        .send()
        .await
        .unwrap();

    let resp = client
        .delete(format!("http://{addr}/api/v1/projects/torm"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v.get("removed").unwrap(), true);
    // sessions_stopped is empty (no gateway attached).
    assert!(v
        .get("sessions_stopped")
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());

    // Deregistered from config.yaml; working tree NOT purged.
    assert!(ccteam_core::lookup_project_in_config(&root, "torm")
        .unwrap()
        .is_none());
    assert!(
        target.exists(),
        "DELETE must NOT file-purge the working tree"
    );
    assert!(target.join(".ccteam/state.json").exists());
}

#[tokio::test]
async fn delete_unknown_project_404() {
    let tmp = TempDir::new().unwrap();
    let paths = fake_paths(tmp.path());
    let addr = spawn(AppState::new(paths)).await;
    let resp = reqwest::Client::new()
        .delete(format!("http://{addr}/api/v1/projects/ghost"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
