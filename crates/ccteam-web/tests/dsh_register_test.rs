//! `POST /api/v1/hosts/local/register-mcp?vendor=dsh` — the Hosts-page
//! "register the ccteam DSH plugin" action (v0.10.3 gate ①). Own process:
//! this test pins `HOME`/`CCTEAM_HOME`, so it must stay the only test here
//! (env mutation is per-process, AGENTS §五).

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
    assert!(
        names.contains(&"@ccteam/ccteam-ui"),
        "ccteam's own bundle — tools, transport and panel in one — is added by \
         the click: {names:?}"
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

    let ours = row_of("ccteam-ui");
    assert!(
        ours.get("transportSocket")
            .and_then(serde_yaml::Value::as_str)
            .is_some_and(|socket| !socket.is_empty()),
        "row carries the socket path: {patch}"
    );
    let team = ours;
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

    assert!(
        profile
            .join("node_modules")
            .join("@ccteam")
            .join("ccteam-ui")
            .join("package.json")
            .exists(),
        "ccteam-ui materialized into the profile"
    );

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
    assert_eq!(
        rows_again
            .iter()
            .filter(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some("ccteam-ui"))
            .count(),
        1,
        "re-registration must not duplicate ccteam-ui: {patch_again}"
    );
    let manifest_again: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest_again["dsh"]["profile"]["bundles"], manifest["dsh"]["profile"]["bundles"],
        "re-registration must not grow the bundle list"
    );

    // The operator installs the plugin themselves (`dsh plugin add`): pnpm's
    // dependency line plus a real package directory where ccteam's link was.
    // The next click must arm THAT copy and install nothing — a second copy of
    // the same plugin id aborts the whole Cordis boot.
    let package_dir = profile
        .join("node_modules")
        .join("@ccteam")
        .join("ccteam-ui");
    std::fs::remove_file(&package_dir).expect("ccteam materialized a symlink");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::write(
        package_dir.join("package.json"),
        serde_json::json!({"name": "@ccteam/ccteam-ui", "version": "9.9.9-theirs"}).to_string(),
    )
    .unwrap();
    let mut theirs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    theirs["dependencies"] = serde_json::json!({"@ccteam/ccteam-ui": "^0.10.4"});
    std::fs::write(
        profile.join("package.json"),
        serde_json::to_string_pretty(&theirs).unwrap(),
    )
    .unwrap();

    let resp = client
        .post(&url)
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "registering onto their install still works"
    );

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(
        after["dsh"]["profile"]["bundles"], manifest["dsh"]["profile"]["bundles"],
        "no second bundle entry for a plugin the operator installed"
    );
    assert!(
        !std::fs::symlink_metadata(&package_dir)
            .unwrap()
            .file_type()
            .is_symlink(),
        "their package directory is left in place"
    );
    let installed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(package_dir.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(
        installed["version"], "9.9.9-theirs",
        "ccteam does not overwrite the version they pinned"
    );
    let rows: Vec<serde_yaml::Value> =
        serde_yaml::from_str(&std::fs::read_to_string(profile.join("cordis.patch.yml")).unwrap())
            .unwrap();
    let ours = rows
        .iter()
        .find(|row| row.get("id").and_then(serde_yaml::Value::as_str) == Some("ccteam-ui"))
        .expect("their copy still gets its config row");
    let config = ours
        .get("config")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("flat plugin config");
    assert_eq!(
        config[&serde_yaml::Value::String("restToken".into())],
        serde_yaml::Value::String(format!("ccteam:{ADMIN_HEX}")),
        "credentials still reach the operator's own install: {config:?}"
    );

    // Same profile, read back through the surface the Hosts panel calls.
    let findings = ccteam_web::dsh_web::operator_dsh_plugin_findings(&ccteam_root);
    assert_eq!(findings.len(), 1, "the drift is reported: {findings:?}");
    assert_eq!(findings[0].code, "plugin_version_mismatch");
    assert_eq!(findings[0].installed.as_deref(), Some("9.9.9-theirs"));
    assert_eq!(findings[0].bundle, "@ccteam/ccteam-ui");
    assert!(
        findings[0]
            .remedy
            .contains("`dsh plugin --profile web update @ccteam/ccteam-ui`"),
        "the panel prints the command that fixes it: {}",
        findings[0].remedy
    );

    // They list their own plugin twice. Cordis aborts the whole boot on a
    // duplicate loader entry id — but removing a row they wrote is not
    // ccteam's call, so the next click leaves both and reports.
    let mut doubled: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    doubled["dsh"]["profile"]["bundles"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("@ccteam/ccteam-ui"));
    std::fs::write(
        profile.join("package.json"),
        serde_json::to_string_pretty(&doubled).unwrap(),
    )
    .unwrap();

    let resp = client
        .post(&url)
        .header("Authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let doubled_after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(profile.join("package.json")).unwrap())
            .unwrap();
    assert_eq!(
        doubled_after["dsh"]["profile"]["bundles"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|bundle| bundle.as_str() == Some("@ccteam/ccteam-ui"))
            .count(),
        2,
        "both rows are theirs and both stay: {doubled_after}"
    );
    let findings = ccteam_web::dsh_web::operator_dsh_plugin_findings(&ccteam_root);
    let duplicate = findings
        .iter()
        .find(|finding| finding.code == "duplicate_bundle_id")
        .unwrap_or_else(|| panic!("the panel reports the duplicate: {findings:?}"));
    assert_eq!(duplicate.count, Some(2));
    assert_eq!(duplicate.bundle, "@ccteam/ccteam-ui");
    assert!(
        duplicate
            .remedy
            .contains("`dsh plugin --profile web remove @ccteam/ccteam-ui`"),
        "with the command that fixes it: {}",
        duplicate.remedy
    );
}
