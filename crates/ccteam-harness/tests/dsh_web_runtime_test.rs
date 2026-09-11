//! DSH web runtime supervision against the hermetic `dsh web` fake
//! (`fixtures/dsh_web/fake_dsh_web.py`).
//!
//! Gate: no real dsh, no network. The fake reproduces dsh 0.1.5's browser
//! authentication — a tokened readiness URL, 401 for everything without an
//! accepted cookie, and one 303 cookie exchange bound to the request's Host
//! authority — which is what ccteam's runtime has to survive.
//!
//! `CCTEAM_DSH_BIN` points the manager at it, the same test-only override as
//! `CCTEAM_{CLAUDE,CODEX}_BIN`. Both `HOME` and `CCTEAM_HOME` are pinned:
//! `CCTEAM_HOME` wins over `HOME`, so pinning one is not isolation (AGENTS.md
//! §五).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ccteam_harness::{
    DshRuntimeConfig, DshRuntimeIdentity, DshRuntimeManager, DshRuntimeState, DSH_BIN_ENV,
};
use serial_test::serial;
use tempfile::TempDir;

const STATE_ENV: &str = "CCTEAM_FAKE_DSH_STATE";
const NO_AUTH_ENV: &str = "CCTEAM_FAKE_DSH_NO_AUTH";
const READY_STDERR_ENV: &str = "CCTEAM_FAKE_DSH_READY_STDERR";
const ATTACH_URL_ENV: &str = "CCTEAM_DSH_WEB_ATTACH_URL";

const ENV_KEYS: &[&str] = &[
    DSH_BIN_ENV,
    STATE_ENV,
    NO_AUTH_ENV,
    READY_STDERR_ENV,
    ATTACH_URL_ENV,
    "HOME",
    "CCTEAM_HOME",
    "DEEPSEEK_API_KEY",
];

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn capture() -> Self {
        Self {
            saved: ENV_KEYS
                .iter()
                .copied()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.iter().rev() {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn fake_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dsh_web/fake_dsh_web.py")
}

/// Pin the whole home surface, then hand the manager the fake as `dsh`.
fn isolate(tmp: &TempDir) -> EnvGuard {
    let guard = EnvGuard::capture();
    let home = tmp.path().join("home");
    let ccteam_home = tmp.path().join(".ccteam-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&ccteam_home).unwrap();
    let bin = fake_bin();
    assert!(bin.is_file(), "missing fake at {}", bin.display());
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("CCTEAM_HOME", &ccteam_home);
        std::env::set_var(DSH_BIN_ENV, &bin);
        std::env::set_var(STATE_ENV, tmp.path().join("fake-dsh.json"));
        std::env::remove_var(NO_AUTH_ENV);
        std::env::remove_var(READY_STDERR_ENV);
        std::env::remove_var(ATTACH_URL_ENV);
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    guard
}

/// What the fake handed out: the port it bound, the launch token it printed,
/// and the exact cookie the loopback authority mints.
#[derive(Debug, serde::Deserialize)]
struct FakeState {
    port: u16,
    #[allow(dead_code)]
    token: String,
    cookie: String,
}

fn fake_state(tmp: &TempDir) -> FakeState {
    let path = tmp.path().join("fake-dsh.json");
    serde_json::from_slice(&std::fs::read(&path).expect("the fake wrote its state")).unwrap()
}

fn manager(tmp: &TempDir, attach_url: Option<String>) -> DshRuntimeManager {
    DshRuntimeManager::configured(
        tmp.path().join(".ccteam-home"),
        std::sync::Arc::new(|_root, owner| Ok(format!("ccteam-enroll:test:{owner}"))),
        std::sync::Arc::new(|_root, owner| Ok(format!("ccteam:token-for-{owner}"))),
        DshRuntimeConfig {
            enabled: true,
            daemon_url: "http://127.0.0.1:7331".to_string(),
            attach_url,
        },
    )
}

fn tenant() -> DshRuntimeIdentity {
    DshRuntimeIdentity {
        owner_tag: "user:alice".to_string(),
        id: "alice".to_string(),
        operator: false,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap()
}

/// A `dsh web` the OPERATOR started: ccteam neither spawned it nor holds its
/// token. Killed on drop.
struct OperatorDsh {
    child: Child,
    port: u16,
}

impl OperatorDsh {
    fn start(tmp: &TempDir) -> Self {
        let state = tmp.path().join("operator-dsh.json");
        let mut child = Command::new("python3")
            .arg(fake_bin())
            .env(STATE_ENV, &state)
            .env_remove(NO_AUTH_ENV)
            .env_remove(READY_STDERR_ENV)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the operator's own dsh web");
        let mut stdout = std::io::BufReader::new(child.stdout.take().expect("fake stdout"));
        let mut line = String::new();
        for _ in 0..2 {
            use std::io::BufRead;
            line.clear();
            stdout.read_line(&mut line).expect("fake readiness line");
        }
        assert!(line.contains("dsh web: http"), "got {line:?}");
        let parsed: FakeState =
            serde_json::from_slice(&std::fs::read(&state).expect("state file")).unwrap();
        Self {
            child,
            port: parsed.port,
        }
    }
}

impl Drop for OperatorDsh {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The whole bug in one test: a `dsh web` that answers 401 until it is given
/// its own cookie must still reach `Running`, and ccteam must come out holding
/// the credential that instance actually minted.
#[tokio::test]
#[serial(dsh_env)]
async fn a_managed_runtime_starts_against_an_authenticating_dsh_and_keeps_its_credential() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let manager = manager(&tmp, None);
    let identity = tenant();

    let status = manager.start(&identity).await;
    assert_eq!(
        status.state,
        DshRuntimeState::Running,
        "401 is authorization, not liveness: {:?}",
        status.error_tail
    );

    let fake = fake_state(&tmp);
    assert_eq!(status.port, Some(fake.port));
    let endpoint = manager.endpoint_for(&identity).await.expect("endpoint");
    assert_eq!(endpoint.port, fake.port);
    assert_eq!(
        endpoint.credential.as_deref(),
        Some(fake.cookie.as_str()),
        "ccteam must hold the cookie this instance minted for the loopback \
         authority the proxy sends upstream"
    );

    // And it is a credential that WORKS: the same value the companion port
    // injects is accepted where a request without it is refused.
    let http = client();
    let url = format!("http://127.0.0.1:{}/api/settings/describe", fake.port);
    let accepted = http
        .post(&url)
        .header(reqwest::header::COOKIE, endpoint.credential.unwrap())
        .send()
        .await
        .expect("authenticated request");
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);
    let refused = http.post(&url).send().await.expect("bare request");
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

    manager.stop(&identity).await;
}

/// Which pipe the readiness line lands on is the vendor's choice, not a
/// contract — and stderr keeps feeding the panel's error tail either way.
#[tokio::test]
#[serial(dsh_env)]
async fn readiness_is_found_on_stderr_and_stderr_still_feeds_the_error_tail() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::set_var(READY_STDERR_ENV, "1");
    }
    let manager = manager(&tmp, None);
    let identity = tenant();

    let status = manager.start(&identity).await;
    assert_eq!(
        status.state,
        DshRuntimeState::Running,
        "error tail: {:?}",
        status.error_tail
    );
    assert_eq!(status.port, Some(fake_state(&tmp).port));
    assert!(
        status
            .error_tail
            .as_deref()
            .is_some_and(|tail| tail.contains("dsh web:")),
        "stderr must still reach the tail: {:?}",
        status.error_tail
    );

    manager.stop(&identity).await;
}

/// No query in the readiness URL — an older dsh, or a future release that
/// drops browser auth — means no exchange and no credential, and everything
/// downstream stays correct.
#[tokio::test]
#[serial(dsh_env)]
async fn a_dsh_without_browser_auth_starts_with_no_credential() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    unsafe {
        std::env::set_var(NO_AUTH_ENV, "1");
    }
    let manager = manager(&tmp, None);
    let identity = tenant();

    let status = manager.start(&identity).await;
    assert_eq!(
        status.state,
        DshRuntimeState::Running,
        "error tail: {:?}",
        status.error_tail
    );
    let endpoint = manager.endpoint_for(&identity).await.expect("endpoint");
    assert_eq!(endpoint.port, fake_state(&tmp).port);
    assert_eq!(
        endpoint.credential, None,
        "nothing was offered, so nothing is invented"
    );

    manager.stop(&identity).await;
}

/// An instance ccteam did not start is PRESENT even while it is challenging.
/// Reading its 401 as absence is what makes ccteam start a rival `dsh web`
/// over the same DSH home and ACP socket — so it attaches, and says plainly
/// what it cannot do rather than proxying a bare 401.
#[tokio::test]
#[serial(dsh_env)]
async fn an_authenticating_operator_instance_is_attached_and_explained_not_duplicated() {
    let tmp = TempDir::new().unwrap();
    let _guard = isolate(&tmp);
    let operator = OperatorDsh::start(&tmp);
    unsafe {
        // Nothing may be spawned on this path: a spawn would fail loudly.
        std::env::set_var(DSH_BIN_ENV, "/nonexistent/ccteam-test-dsh");
    }
    let manager = manager(&tmp, Some(format!("http://127.0.0.1:{}", operator.port)));
    let identity = DshRuntimeIdentity::for_owner_tag("user:web-api");

    let status = tokio::time::timeout(Duration::from_secs(20), manager.start(&identity))
        .await
        .expect("attach must not hang");

    assert_eq!(
        status.state,
        DshRuntimeState::Attached,
        "a challenging dsh is present: {:?}",
        status.error_tail
    );
    assert_eq!(status.port, Some(operator.port));
    let tail = status.error_tail.unwrap_or_default();
    assert!(
        tail.contains("started outside ccteam") && tail.contains("printed"),
        "the panel must name the remedy: {tail}"
    );

    manager.stop(&identity).await;
}
