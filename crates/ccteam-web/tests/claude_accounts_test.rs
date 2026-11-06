//! Discovery never turns project access into permission to spend every local
//! subscription. This integration process owns its temporary HOME and store.
#![cfg(unix)]

use ccteam_core::{config::CcteamConfig, tenants::TenantRegistry, CcteamPaths};
use ccteam_harness::{execution::claude_accounts::account_id, ClaudeStreamJsonAdapter};
use ccteam_im::gateway::Gateway;
use ccteam_web::{router_with_state, AppState, AuthState};
use std::{os::unix::fs::PermissionsExt, sync::Arc};

#[tokio::test]
async fn account_discovery_filters_grants_before_spawning_the_optional_tool() {
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
    let mut config = CcteamConfig::default();
    config
        .cswap_account_grants
        .insert(format!("user:{}", alice.id), vec![first.clone()]);
    ccteam_core::config::save(&paths.root, &config).unwrap();
    let calls = tmp.path().join("calls");
    let cswap = tmp.path().join("cswap");
    std::fs::write(&cswap, format!("#!/bin/sh\necho x >> '{}'\nif [ \"$1\" = run ]; then echo '--no-share-history --require-session'; exit 0; fi\nif [ \"$1\" = config ]; then echo '{{\"schemaVersion\":1,\"settings\":[]}}'; exit 0; fi\nprintf '%s\\n' '{{\"schemaVersion\":1,\"accounts\":[{{\"number\":1,\"email\":\"one@example.test\"}},{{\"number\":2,\"email\":\"two@example.test\"}}]}}'\n", calls.display())).unwrap();
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
    let denied: serde_json::Value = client
        .get(&url)
        .bearer_auth(format!("ccteam:{}", bob.web_token))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(denied["available"], false);
    assert_eq!(
        denied["reason"], "not_granted",
        "a missing grant is not reported as a missing tool"
    );
    assert_eq!(denied["accounts"], serde_json::json!([]));
    assert!(
        !calls.exists(),
        "an ungranted tenant must not even invoke account discovery"
    );
    let allowed: serde_json::Value = client
        .get(&url)
        .bearer_auth(format!("ccteam:{}", alice.web_token))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(allowed["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(allowed["accounts"][0]["id"], first);
    assert!(!allowed.to_string().contains(&second));
    assert!(!allowed.to_string().contains("one@"));
    let operator: serde_json::Value = client
        .get(&url)
        .bearer_auth(format!("ccteam:{admin}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(operator["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap().lines().count(),
        3,
        "discovery is coalesced and cached"
    );
    config.cswap_account_grants.clear();
    ccteam_core::config::save(&paths.root, &config).unwrap();
    let revoked: serde_json::Value = client
        .get(&url)
        .bearer_auth(format!("ccteam:{}", alice.web_token))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        revoked["accounts"],
        serde_json::json!([]),
        "cache must not retain a revoked grant"
    );

    // The operator grants from the web; tenants cannot grant themselves.
    let grants = |user: &str| format!("http://{address}/api/v1/users/{user}/cswap-accounts");
    let status = |response: reqwest::Response| response.status().as_u16();
    assert_eq!(
        status(
            client
                .put(grants(&bob.id))
                .bearer_auth(format!("ccteam:{}", bob.web_token))
                .json(&serde_json::json!({"accounts": [second]}))
                .send()
                .await
                .unwrap()
        ),
        403
    );
    for bad in ["2", "*", "two@example.test"] {
        assert_eq!(
            status(
                client
                    .put(grants(&bob.id))
                    .bearer_auth(format!("ccteam:{admin}"))
                    .json(&serde_json::json!({"accounts": [bad]}))
                    .send()
                    .await
                    .unwrap()
            ),
            400,
            "{bad} is not a stable account id"
        );
    }
    assert_eq!(
        status(
            client
                .put(grants("u-missing"))
                .bearer_auth(format!("ccteam:{admin}"))
                .json(&serde_json::json!({"accounts": []}))
                .send()
                .await
                .unwrap()
        ),
        404
    );
    let saved: serde_json::Value = client
        .put(grants(&bob.id))
        .bearer_auth(format!("ccteam:{admin}"))
        .json(&serde_json::json!({"accounts": [second, second]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["accounts"], serde_json::json!([second]));
    let listed: serde_json::Value = client
        .get(grants(&bob.id))
        .bearer_auth(format!("ccteam:{admin}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["accounts"], serde_json::json!([second]));
    let granted: serde_json::Value = client
        .get(&url)
        .bearer_auth(format!("ccteam:{}", bob.web_token))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(granted["available"], true);
    assert_eq!(granted["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(granted["accounts"][0]["id"], second);

    // Deleting the tenant drops its grant instead of leaving it dangling.
    assert_eq!(
        status(
            client
                .delete(format!("http://{address}/api/v1/users/{}", bob.id))
                .bearer_auth(format!("ccteam:{admin}"))
                .send()
                .await
                .unwrap()
        ),
        200
    );
    assert!(ccteam_core::config::load(&paths.root)
        .unwrap()
        .cswap_account_grants
        .is_empty());
}
