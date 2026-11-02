use std::net::SocketAddr;

use ccteam_core::{CcteamPaths, ProjectState};
use ccteam_web::{router_with_state, AppState, AuthState};
use serde_json::{json, Value};
use tempfile::TempDir;

const TOKEN: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

#[tokio::test]
async fn ownership_changes_revoke_previews_and_remote_projects_never_read_local_files() {
    use ccteam_core::{
        config::{self, CcteamConfig, ProjectEntry},
        tenants::TenantRegistry,
    };
    let (_tmp, paths, addr, client) = fixture().await;
    let mut registry = TenantRegistry::default();
    let alice = registry.add("alice");
    let alice_token = alice.web_token.clone();
    registry.save(&paths.users_dir()).unwrap();
    let root = format!("http://{addr}/api/v1/projects/demo");
    assert_eq!(
        client
            .get(format!("{root}/files"))
            .bearer_auth(format!("ccteam:{alice_token}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let mut state = ProjectState::load(&paths.project_state("demo")).unwrap();
    state.owner = Some(format!("user:{}", alice.id));
    state.save(&paths.project_state("demo")).unwrap();
    let preview: Value = client
        .post(format!("{root}/files/preview"))
        .bearer_auth(format!("ccteam:{alice_token}"))
        .json(&json!({"path":"web/index.html"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = format!("http://{addr}{}", preview["url"].as_str().unwrap());
    assert_eq!(client.get(&url).send().await.unwrap().status(), 200);
    state.owner = Some("user:web-api".into());
    state.save(&paths.project_state("demo")).unwrap();
    assert_eq!(client.get(url).send().await.unwrap().status(), 404);
    let config = CcteamConfig {
        projects: vec![ProjectEntry {
            slug: "demo".into(),
            path: paths.project_dir("demo"),
            host: "satellite".into(),
            remote_slug: Some("demo".into()),
            remote_path: None,
            team: "dev".into(),
            installed_at: chrono::Utc::now(),
        }],
        ..Default::default()
    };
    config::save(&paths.root, &config).unwrap();
    assert_eq!(
        client
            .get(format!("{root}/file?path=web/index.html"))
            .bearer_auth(format!("ccteam:{TOKEN}"))
            .send()
            .await
            .unwrap()
            .status(),
        501
    );
    std::fs::write(config::config_path(&paths.root), "projects: [not valid").unwrap();
    assert_eq!(
        client
            .get(format!("{root}/files"))
            .bearer_auth(format!("ccteam:{TOKEN}"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
}

async fn fixture() -> (TempDir, CcteamPaths, SocketAddr, reqwest::Client) {
    let tmp = TempDir::new().unwrap();
    let paths = CcteamPaths {
        root: tmp.path().join("home"),
        projects_root: tmp.path().join("projects"),
    };
    for slug in ["demo", "other"] {
        ProjectState::initial(slug.to_string())
            .save(&paths.project_state(slug))
            .unwrap();
    }
    let root = paths.project_dir("demo");
    std::fs::create_dir_all(root.join("web/assets")).unwrap();
    std::fs::write(root.join("web/index.html"), "<link rel=stylesheet href=assets/style.css><h1>Hello</h1><img alt=dot src=assets/dot.svg><script type=module src=assets/app.js></script>").unwrap();
    std::fs::write(root.join("web/assets/style.css"), "h1 { color: red }").unwrap();
    std::fs::write(
        root.join("web/assets/app.js"),
        "import { loaded } from './helper.js'; document.body.dataset.loaded = loaded",
    )
    .unwrap();
    std::fs::write(
        root.join("web/assets/helper.js"),
        "export const loaded = 'yes'",
    )
    .unwrap();
    std::fs::write(root.join("web/assets/dot.svg"), "<svg xmlns='http://www.w3.org/2000/svg' width='3' height='3'><rect width='3' height='3' fill='red'/></svg>").unwrap();
    std::fs::write(root.join(".env"), "PRIVATE_TOKEN=secret").unwrap();
    std::fs::create_dir(root.join("node_modules")).unwrap();
    std::fs::write(root.join("node_modules/huge.js"), "ignored").unwrap();
    std::fs::write(root.join("binary.bin"), b"a\0b").unwrap();
    std::fs::write(root.join("web/private.rs"), "server-only source").unwrap();
    std::fs::write(root.join("outside.html"), "outside preview directory").unwrap();
    std::fs::write(root.join("large.txt"), vec![b'a'; 2 * 1024 * 1024 + 1]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_state(AppState::with_auth(
        paths.clone(),
        AuthState::enabled(TOKEN.into()),
    ));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        tmp,
        paths,
        addr,
        reqwest::Client::builder().no_proxy().build().unwrap(),
    )
}

#[tokio::test]
async fn bounded_read_only_files_hide_internal_paths_and_reject_escapes() {
    let (_tmp, paths, addr, client) = fixture().await;
    let base = format!("http://{addr}/api/v1/projects/demo");
    assert_eq!(
        client
            .get(format!("{base}/files"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response: Value = client
        .get(format!("{base}/files"))
        .bearer_auth(format!("ccteam:{TOKEN}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names = response["paths"].as_array().unwrap();
    assert!(names.contains(&json!("web/index.html")));
    assert!(!names.iter().any(|p| p.as_str().unwrap().contains(".env")
        || p.as_str().unwrap().contains("node_modules")
        || p.as_str().unwrap().contains(".ccteam")));
    for (path, status) in [
        ("web/index.html", 200),
        ("../secret", 403),
        (".env", 403),
        ("node_modules/huge.js", 403),
        ("large.txt", 413),
    ] {
        let response = client
            .get(format!("{base}/file"))
            .query(&[("path", path)])
            .bearer_auth(format!("ccteam:{TOKEN}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
    let binary: Value = client
        .get(format!("{base}/file?path=binary.bin"))
        .bearer_auth(format!("ccteam:{TOKEN}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(binary["binary"], true);
    assert!(binary["content"].is_null());
    #[cfg(unix)]
    {
        nix::unistd::mkfifo(
            &paths.project_dir("demo").join("pipe.txt"),
            nix::sys::stat::Mode::S_IRUSR,
        )
        .unwrap();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client
                .get(format!("{base}/file?path=pipe.txt"))
                .bearer_auth(format!("ccteam:{TOKEN}"))
                .send(),
        )
        .await
        .expect("FIFO must not hang")
        .unwrap();
        assert_eq!(response.status(), 404);
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", paths.project_dir("demo").join("escape.txt"))
            .unwrap();
        assert_eq!(
            client
                .get(format!("{base}/file?path=escape.txt"))
                .bearer_auth(format!("ccteam:{TOKEN}"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Chromium integration: make test-files-browser"]
async fn preview_runs_relative_modules_and_css_inside_an_opaque_browser_sandbox() {
    let (_tmp, _paths, addr, client) = fixture().await;
    let link: Value = client
        .post(format!("http://{addr}/api/v1/projects/demo/files/preview"))
        .bearer_auth(format!("ccteam:{TOKEN}"))
        .json(&json!({"path":"web/index.html"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/tests/file-preview-browser.mjs");
    let status = tokio::process::Command::new("node")
        .arg(script)
        .arg(format!("http://{addr}"))
        .arg(link["url"].as_str().unwrap())
        .status()
        .await
        .unwrap();
    assert!(status.success(), "Chromium preview verification failed");
}

#[tokio::test]
async fn preview_capability_loads_relative_assets_without_login_and_cannot_cross_projects() {
    let (_tmp, _paths, addr, client) = fixture().await;
    let base = format!("http://{addr}");
    let link: Value = client
        .post(format!("{base}/api/v1/projects/demo/files/preview"))
        .bearer_auth(format!("ccteam:{TOKEN}"))
        .json(&json!({"path":"web/index.html"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = link["url"].as_str().unwrap();
    let response = client.get(format!("{base}{url}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("sandbox allow-scripts"));
    assert!(!csp.contains("allow-same-origin"));
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert!(response.headers().get("set-cookie").is_none());
    let asset = url.replace("index.html", "assets/app.js");
    let response = client
        .get(format!("{base}{asset}"))
        .header("Origin", "null")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["access-control-allow-origin"], "*");
    assert!(response.text().await.unwrap().contains("dataset.loaded"));
    let wrong_project = url.replace("/projects/demo/", "/projects/other/");
    assert_eq!(
        client
            .get(format!("{base}{wrong_project}"))
            .bearer_auth(format!("ccteam:{TOKEN}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let private = url.replace("web/index.html", ".env");
    assert_eq!(
        client
            .get(format!("{base}{private}"))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for path in [
        url.replace("web/index.html", "outside.html"),
        url.replace("index.html", "private.rs"),
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/projects/demo/files/preview/bad/web/index.html"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}
