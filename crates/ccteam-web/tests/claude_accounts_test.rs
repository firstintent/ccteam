//! Discovery is open to every authenticated identity (owner decision
//! 2026-11-06, #229) yet still never leaks raw emails and runs the optional
//! tool once per cache window. This integration process owns its temporary
//! HOME and store.
#![cfg(unix)]

use ccteam_core::{tenants::TenantRegistry, CcteamPaths};
use ccteam_harness::{execution::claude_accounts::account_id, ClaudeStreamJsonAdapter};
use ccteam_im::gateway::Gateway;
use ccteam_web::{router_with_state, AppState, AuthState};
use std::{os::unix::fs::PermissionsExt, sync::Arc};

#[tokio::test]
async fn every_identity_discovers_every_local_account_without_a_grant() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path().join("home"));
    std::env::set_var("CCTEAM_HOME", tmp.path().join("state"));
    let paths = CcteamPaths {
        root: tmp.path().join("state"),
        projects_root: tmp.path().join("projects"),
    };
    std::fs::create_dir_all(paths.users_dir()).unwrap();
    let mut tenants = TenantRegistry::default();
    let alice = tenants.add("alice");
    let bob = tenants.add("bob");
    tenants.save(&paths.users_dir()).unwrap();
    let first = account_id("one@example.test", "");
    let second = account_id("two@example.test", "");
    let calls = tmp.path().join("calls");
    let cswap = tmp.path().join("cswap");
    std::fs::write(&cswap, format!("#!/bin/sh\necho x >> '{}'\nif [ \"$1\" = run ]; then echo '--share-history --no-share-history --require-session'; exit 0; fi\nif [ \"$1\" = config ]; then echo '{{\"schemaVersion\":1,\"settings\":[]}}'; exit 0; fi\nprintf '%s\\n' '{{\"schemaVersion\":1,\"accounts\":[{{\"number\":1,\"email\":\"one@example.test\"}},{{\"number\":2,\"email\":\"two@example.test\"}}]}}'\n", calls.display())).unwrap();
    std::fs::set_permissions(&cswap, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("CCTEAM_CSWAP_BIN", &cswap);
    let mut gateway = Gateway::new(
        Arc::new(ClaudeStreamJsonAdapter::new()),
        "project",
        tmp.path(),
    );
    gateway.enable_project_creation(paths.clone());
    let admin = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let mut state = AppState::with_auth(paths.clone(), AuthState::enabled(admin.into()));
    state.gateway = Some(Arc::new(tokio::sync::Mutex::new(gateway)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router_with_state(state))
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{address}/api/v1/vendors/claude/accounts");

    for token in [alice.web_token.clone(), bob.web_token.clone(), admin.into()] {
        let listed: serde_json::Value = client
            .get(&url)
            .bearer_auth(format!("ccteam:{token}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed["available"], true);
        let ids: std::collections::BTreeSet<_> = listed["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|account| account["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            ids,
            [first.clone(), second.clone()].into(),
            "tenants and the admin see the same local accounts"
        );
        assert!(
            !listed.to_string().contains("example.test"),
            "labels carry the local part only; the email domain never leaves the daemon"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap().lines().count(),
        3,
        "discovery is coalesced and cached across identities"
    );
    let anonymous = client.get(&url).send().await.unwrap();
    assert_eq!(
        anonymous.status().as_u16(),
        401,
        "discovery still sits behind the web-token gate"
    );

    // #236 — switching a session's account. A released Claude session only
    // rebinds (nothing spawns); its next message resumes under the account.
    use ccteam_harness::execution::session_meta::{
        read_session_meta, write_session_meta, SessionMeta, SessionOrigin,
    };
    use ccteam_harness::{AgentVendor, PermissionMode, SessionProtocol};
    let state = paths.project_state("project");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    ccteam_core::ProjectState::initial("project".into())
        .save(&state)
        .unwrap();
    write_session_meta(
        tmp.path(),
        &SessionMeta {
            claude_account: None,
            model_pinned_generation: None,
            mode: None,
            tool_face: None,
            managed_by: Default::default(),
            stopped_at: None,
            sid: "s7".into(),
            slug: "project".into(),
            vendor: AgentVendor::Claude,
            protocol: SessionProtocol::StreamJson,
            role: String::new(),
            permission_mode: PermissionMode::Skip,
            owner: "user:web-api".into(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: "local".into(),
            created_at: "2026-11-08T00:00:00Z".into(),
            last_active: "2026-11-08T00:00:00Z".into(),
            origin: SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: None,
            spawned_by_role: None,
            delegation_depth: 0,
        },
    )
    .unwrap();
    let switch = |sid: &str, account: serde_json::Value, token: Option<&str>| {
        let request = client
            .put(format!(
                "http://{address}/api/v1/sessions/{sid}/claude-account"
            ))
            .json(&serde_json::json!({ "claude_account": account }));
        match token {
            Some(token) => request.bearer_auth(format!("ccteam:{token}")),
            None => request,
        }
        .send()
    };
    let status = |response: reqwest::Response| response.status().as_u16();
    assert_eq!(
        status(switch("s7", serde_json::Value::Null, None).await.unwrap()),
        401
    );
    assert_eq!(
        status(
            switch("s404", serde_json::Value::Null, Some(admin))
                .await
                .unwrap()
        ),
        404
    );
    assert_eq!(
        status(
            switch("s7", "f".repeat(64).into(), Some(admin))
                .await
                .unwrap()
        ),
        422,
        "only an account cswap lists can be chosen"
    );
    let switched: serde_json::Value = switch("s7", second.clone().into(), Some(admin))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(switched["changed"], true);
    assert_eq!(
        switched["released"], false,
        "a released session only rebinds"
    );
    assert_eq!(switched["claude_account"]["slot"], "2");
    assert_eq!(switched["claude_account"]["label"], "two");
    let stored = read_session_meta(tmp.path(), "s7")
        .unwrap()
        .claude_account
        .unwrap();
    assert_eq!(
        (stored.id.as_str(), stored.config_dir),
        (second.as_str(), None)
    );
    let default: serde_json::Value = switch("s7", serde_json::Value::Null, Some(admin))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(default["changed"], true);
    assert!(default["claude_account"].is_null());
    assert!(read_session_meta(tmp.path(), "s7")
        .unwrap()
        .claude_account
        .is_none());
}
