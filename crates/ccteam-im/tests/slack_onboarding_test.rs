//! Slack onboarding (`slack_setup_with_base`) — deterministic credential
//! validation tests.
//!
//! Setup proves the bot token with `auth.test` and the app-level token with
//! `apps.connections.open` (the two calls the live Socket Mode channel makes).
//! A small std `TcpListener` responder routed by request path stands in for
//! the Slack Web API, so each round-trip is real HTTP but never leaves the
//! box — mirrors `lark_onboarding_test.rs`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use ccteam_im::onboarding::{slack_setup_with_base, OnboardingError};

/// Requests seen by the mock: `(path, authorization header)`.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// Serve `routes` (path → JSON body) on `127.0.0.1:0` for every connection
/// until the test ends. Returns the `api_base` and the request log.
fn spawn_slack_mock(routes: &[(&str, &'static str)]) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let routes: HashMap<String, &'static str> =
        routes.iter().map(|(p, b)| (p.to_string(), *b)).collect();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        req.extend_from_slice(&buf[..n]);
                        if header_end.is_none() {
                            if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                                header_end = Some(pos + 4);
                                let headers = String::from_utf8_lossy(&req[..pos]);
                                content_length = headers
                                    .lines()
                                    .find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("content-length")
                                            .then(|| value.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                            }
                        }
                        if let Some(end) = header_end {
                            if req.len().saturating_sub(end) >= content_length {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let head = String::from_utf8_lossy(&req).into_owned();
            let path = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let auth = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_string())
                })
                .unwrap_or_default();
            log.lock().unwrap().push((path.clone(), auth));
            let body = routes
                .get(&path)
                .copied()
                .unwrap_or(r#"{"ok":false,"error":"unknown_method"}"#);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}"), seen)
}

const AUTH_OK: &str =
    r#"{"ok":true,"team":"Acme","user":"ccteam","user_id":"UBOT","bot_id":"BBOT"}"#;
const SOCKET_OK: &str = r#"{"ok":true,"url":"wss://wss-primary.slack.com/link/?ticket=t"}"#;

#[tokio::test]
async fn slack_setup_ok_validates_both_tokens_and_returns_creds() {
    let (base, seen) = spawn_slack_mock(&[
        ("/auth.test", AUTH_OK),
        ("/apps.connections.open", SOCKET_OK),
    ]);

    let result = slack_setup_with_base(
        "xoxb-1-bot",
        "xapp-1-app",
        vec!["U0ALICE".into(), "U0BOB".into()],
        &base,
    )
    .await
    .expect("valid tokens must validate");

    assert_eq!(result.creds.bot_token, "xoxb-1-bot");
    assert_eq!(result.creds.app_token, "xapp-1-app");
    assert_eq!(result.creds.allowed_user_ids, vec!["U0ALICE", "U0BOB"]);
    assert_eq!(result.team, "Acme");
    assert_eq!(result.bot_user, "ccteam");
    assert_eq!(result.bot_user_id, "UBOT");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            ("/auth.test".to_string(), "Bearer xoxb-1-bot".to_string()),
            (
                "/apps.connections.open".to_string(),
                "Bearer xapp-1-app".to_string()
            ),
        ],
        "the bot token proves the Web API, the app token proves Socket Mode"
    );
}

#[tokio::test]
async fn slack_setup_bad_bot_token_is_api_not_ok() {
    let (base, seen) = spawn_slack_mock(&[
        ("/auth.test", r#"{"ok":false,"error":"invalid_auth"}"#),
        ("/apps.connections.open", SOCKET_OK),
    ]);
    let err = slack_setup_with_base("xoxb-bad", "xapp-1", vec![], &base)
        .await
        .expect_err("ok:false on auth.test must fail");
    match err {
        OnboardingError::ApiNotOk(msg) => {
            assert!(
                msg.contains("auth.test") && msg.contains("invalid_auth"),
                "{msg}"
            );
        }
        other => panic!("expected ApiNotOk, got {other:?}"),
    }
    assert_eq!(seen.lock().unwrap().len(), 1, "stops at the first failure");
}

#[tokio::test]
async fn slack_setup_bad_app_token_is_api_not_ok() {
    let (base, _) = spawn_slack_mock(&[
        ("/auth.test", AUTH_OK),
        (
            "/apps.connections.open",
            r#"{"ok":false,"error":"not_allowed_token_type"}"#,
        ),
    ]);
    let err = slack_setup_with_base("xoxb-1", "xoxb-wrong-kind", vec![], &base)
        .await
        .expect_err("a bot token where the app token belongs must fail");
    match err {
        OnboardingError::ApiNotOk(msg) => {
            assert!(
                msg.contains("apps.connections.open") && msg.contains("not_allowed_token_type"),
                "{msg}"
            );
        }
        other => panic!("expected ApiNotOk, got {other:?}"),
    }
}

#[tokio::test]
async fn slack_setup_missing_bot_user_is_bad_response() {
    let (base, _) = spawn_slack_mock(&[
        ("/auth.test", r#"{"ok":true,"team":"Acme"}"#),
        ("/apps.connections.open", SOCKET_OK),
    ]);
    let err = slack_setup_with_base("xoxb-1", "xapp-1", vec![], &base)
        .await
        .expect_err("auth.test without user_id is malformed");
    assert!(matches!(err, OnboardingError::BadResponse(_)), "{err:?}");
}
