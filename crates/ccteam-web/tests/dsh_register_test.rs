//! `POST /api/v1/hosts/local/register-mcp?vendor=dsh` — the Hosts-page
//! "register the ccteam DSH plugin" action (v0.10.3 gate ①). Own process:
//! this test pins `HOME`/`CCTEAM_HOME`, so it must stay the only test here
//! (env mutation is per-process, AGENTS §六).

use std::net::SocketAddr;
use std::sync::Arc;

use ccteam_core::CcteamPaths;
use ccteam_harness::DshRuntimeConfig;
use ccteam_web::dsh_web::DshWebSupervisor;
use ccteam_web::{dsh_web, router_with_state, AppState, AuthState};
use tempfile::TempDir;
use tokio::net::TcpListener;

const ADMIN_HEX: &str = "deadbeefcafef00ddeadbeefcafef00d";

fn fake_paths(root: &std::path::Path) -> CcteamPaths {
    CcteamPaths {
        root: root.join(".ccteam"),
        projects_root: root.join("projects"),
    }
}

async fn spawn_app(app: axum::Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::task::yield_now().await;
    addr
}

#[tokio::test]
async fn register_dsh_writes_only_ccteam_rows_into_the_operator_profile() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let ccteam_root = tmp.path().join(".ccteam");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("CCTEAM_HOME", &ccteam_root);
    // The admin web token as `ccteam start` keeps it: the same file the
    // runtime's REST token resolver reads for the operator.
    std::fs::create_dir_all(ccteam_root.join("secrets")).unwrap();
    std::fs::write(ccteam_root.join("secrets").join("web-token"), ADMIN_HEX).unwrap();

    // A pre-existing user profile with the user's OWN bundle and patch row:
    // registration must merge around them, never clobber.
    let profile = home.join(".dsh").join("profiles").join("web");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("package.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "name": "dsh-profile-web",
            "private": true,
            "dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-base", "@user/my-plugin"]}}
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        profile.join("cordis.patch.yml"),
        "- id: my-plugin\n  config:\n    keep: true\n",
    )
    .unwrap();

    let runtime = dsh_web::new_runtime_manager(ccteam_root.clone());
    runtime.configure(DshRuntimeConfig {
        enabled: true,
        daemon_url: "http://127.0.0.1:7331".to_string(),
        attach_url: None,
    });
    let state = AppState::with_auth(fake_paths(tmp.path()), AuthState::enabled(ADMIN_HEX.into()))
        .with_dsh_web(Arc::new(DshWebSupervisor::new(runtime)));
    let addr = spawn_app(router_with_state(state)).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();

    let url = format!("http://{addr}/api/v1/hosts/local/register-mcp?vendor=dsh");
    let auth = format!("Bearer ccteam:{ADMIN_HEX}");
    let resp = client
        .post(&url)
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "admin registration must succeed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["registered"][0], "dsh");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    let bundles = manifest["dsh"]["profile"]["bundles"].as_array().unwrap();
    let names: Vec<&str> = bundles.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"@ccteam/ccteam-client"), "own bundle added");
    assert!(
        names.contains(&"@ccteam/ccteam-ui"),
        "the team panel is registered by the same click: {names:?}"
    );
    assert!(names.contains(&"@user/my-plugin"), "user bundle preserved");

    let patch = std::fs::read_to_string(profile.join("cordis.patch.yml")).unwrap();
    assert!(patch.contains("my-plugin"), "user patch row preserved");
    let rows: Vec<serde_yaml::Value> = serde_yaml::from_str(&patch).unwrap();
    let row_of = |id: &str| -> serde_yaml::Mapping {
        let row = rows
            .iter()
            .find(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some(id))
            .unwrap_or_else(|| panic!("no {id} row in {patch}"));
        assert!(
            row.get("insert").is_none(),
            "ccteam rows OVERRIDE the bundle-inserted entry; a duplicate id \
             aborts the whole Cordis boot: {patch}"
        );
        row.get("config")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("flat plugin config")
            .clone()
    };

    let client_row = row_of("ccteam-client");
    assert!(
        client_row
            .get("transportSocket")
            .and_then(serde_yaml::Value::as_str)
            .is_some_and(|socket| !socket.is_empty()),
        "row carries the socket path: {patch}"
    );
    let team = row_of("ccteam-ui");
    assert_eq!(
        team["daemonUrl"],
        serde_yaml::Value::String("http://127.0.0.1:7331".into()),
        "the panel is pointed at this daemon"
    );
    // The operator's OWN admin web token rides the panel row (owner decision
    // 2026-08-28: pasting a token is for a hand-started `dsh web` only), and a
    // patch carrying a credential is private to the OS user.
    assert_eq!(
        team["restToken"],
        serde_yaml::Value::String(format!("ccteam:{ADMIN_HEX}")),
        "the panel row carries the operator's own REST token: {patch}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(profile.join("cordis.patch.yml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the patch file is 0600");
    }

    for package in ["ccteam-client", "ccteam-ui"] {
        assert!(
            profile
                .join("node_modules")
                .join("@ccteam")
                .join(package)
                .join("package.json")
                .exists(),
            "{package} materialized into the profile"
        );
    }

    // Idempotent: a second click neither errors nor duplicates the row.
    let resp = client
        .post(&url)
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let patch_again = std::fs::read_to_string(profile.join("cordis.patch.yml")).unwrap();
    let rows_again: Vec<serde_yaml::Value> = serde_yaml::from_str(&patch_again).unwrap();
    for id in ["ccteam-client", "ccteam-ui"] {
        assert_eq!(
            rows_again
                .iter()
                .filter(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some(id))
                .count(),
            1,
            "re-registration must not duplicate {id}: {patch_again}"
        );
    }
    let manifest_again: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest_again["dsh"]["profile"]["bundles"], manifest["dsh"]["profile"]["bundles"],
        "re-registration must not grow the bundle list"
    );
}
