//! Lark/Feishu onboarding (`lark_setup_with_base`) — deterministic
//! credential-validate tests.
//!
//! The validate fetches a `tenant_access_token` from the app id/secret
//! (the same `auth/v3/tenant_access_token/internal` call the live channel
//! makes). A one-shot std `TcpListener` stands in for the Feishu endpoint
//! so the round-trip is real HTTP but never touches the network — no
//! `wiremock`/axum dep needed, and the base URL is parameterized exactly
//! for this (mirrors `telegram_setup_with_base`).

use std::io::{Read, Write};
use std::net::TcpListener;

use ccteam_im::onboarding::{lark_setup_with_base, OnboardingError};

/// Spawn a single-shot HTTP/1.1 responder on `127.0.0.1:0` that replies to
/// the first connection with `body` (status 200, JSON) and exits. Returns
/// `http://127.0.0.1:<port>` — a Lark `api_base` override.
fn spawn_oneshot_http(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn lark_setup_ok_returns_creds() {
    let base =
        spawn_oneshot_http(r#"{"code":0,"msg":"ok","tenant_access_token":"t-tok","expire":7200}"#);

    let result = lark_setup_with_base(
        "cli_app_1",
        "secret_1",
        vec!["ou_alice".into(), "ou_bob".into()],
        true, // Feishu / CN
        &base,
    )
    .await
    .expect("valid credentials (code=0 + token) must validate");

    assert_eq!(result.creds.app_id, "cli_app_1");
    assert_eq!(result.creds.app_secret, "secret_1");
    assert_eq!(result.creds.allowed_user_ids, vec!["ou_alice", "ou_bob"]);
    assert!(result.creds.use_feishu, "use_feishu must round-trip true");
}

#[tokio::test]
async fn lark_setup_bad_creds_is_api_not_ok() {
    // Feishu signals invalid app credentials as a 200 with a non-zero
    // `code` — the validate must turn that into an error (honest failure),
    // not a silently-persisted dead token.
    let base = spawn_oneshot_http(r#"{"code":10003,"msg":"invalid app_secret"}"#);

    let err = lark_setup_with_base("cli_bad", "wrong", vec![], false, &base)
        .await
        .expect_err("a non-zero Feishu code must surface as an error");

    match err {
        OnboardingError::ApiNotOk(msg) => {
            assert!(
                msg.contains("invalid app_secret") || msg.contains("10003"),
                "ApiNotOk must carry the upstream reason; got: {msg}"
            );
        }
        other => panic!("expected ApiNotOk, got {other:?}"),
    }
}

#[tokio::test]
async fn lark_setup_missing_token_is_bad_response() {
    // code=0 but no `tenant_access_token` field → malformed response.
    let base = spawn_oneshot_http(r#"{"code":0,"msg":"ok"}"#);

    let err = lark_setup_with_base("cli_x", "secret_x", vec![], true, &base)
        .await
        .expect_err("a code=0 response with no token must be rejected");

    assert!(
        matches!(err, OnboardingError::BadResponse(_)),
        "expected BadResponse, got {err:?}"
    );
}
