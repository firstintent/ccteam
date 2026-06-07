//! v0.8.9 Phase 2 — deterministic tests for the ccteam-hub backend
//! (`ccteam_im::hub`).
//!
//! The backend fetches `index.json` + plugin bodies from the hub raw host and
//! installs them into a project's `.claude/`. Like `role_import_test.rs`,
//! these tests stand a tiny in-process HTTP/1.1 responder in for
//! `raw.githubusercontent.com` (the `base` parameter is the seam) so the
//! round-trip is real HTTP but never touches the network. The project dir is a
//! `TempDir`; `CCTEAM_HOME` is pointed at another `TempDir` so the hub cache
//! writes under a throwaway root, not the real `~/.ccteam`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use ccteam_im::hub::{
    fetch_index, install_plugin, installed_status, load_catalog, HubError, HubPlugin,
    InstalledStatus, MAX_HUB_BODY_BYTES,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Single-shot HTTP/1.1 responder (one connection, then exit). Identical in
/// spirit to `role_import_test::spawn_oneshot_http` — used as the FAKE HUB for
/// single-request cases (the body fetch, or a 404/empty index). Returns
/// `http://127.0.0.1:<port>` to pass as the hub `base`.
fn spawn_oneshot_http(status_line: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "{status_line}\r\nContent-Type: text/plain\r\n\
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

/// One-shot responder with a runtime-built body + arbitrary extra headers
/// (e.g. a `Location:` for the redirect test). Mirrors
/// `role_import_test::spawn_oneshot_http_dyn`.
fn spawn_oneshot_http_dyn(status_line: &str, extra_headers: &str, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let status_line = status_line.to_string();
    let extra_headers = extra_headers.to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "{status_line}\r\nContent-Type: text/plain\r\n{extra_headers}\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

/// Multi-route fake hub: serves a fixed `index.json` and a per-path body map,
/// for `N` sequential connections (the round-trip needs two: GET /index.json
/// then GET /agents/<id>.md). Routes on the request path parsed from the first
/// request line. Unknown paths get a 404. Returns the `base` URL.
fn spawn_fake_hub(index_json: String, bodies: Vec<(String, String)>, connections: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            // First line: "GET /path HTTP/1.1"
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let body: Option<&str> = if path == "/index.json" {
                Some(index_json.as_str())
            } else {
                bodies
                    .iter()
                    .find(|(p, _)| format!("/{p}") == path)
                    .map(|(_, b)| b.as_str())
            };
            let resp = match body {
                Some(b) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    b.len(),
                    b
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            };
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

const AGENT_BODY: &str =
    "---\nname: helper\ndescription: A curated helper\n---\nYou are a helpful agent.\n";

/// Build an index JSON with a single agent plugin whose `content_sha` is the
/// real sha256 of `AGENT_BODY` (so the integrity check passes).
fn good_index_json(id: &str) -> String {
    let sha = sha256_hex(AGENT_BODY.as_bytes());
    format!(
        r#"{{
          "version": 1,
          "name": "ccteam-hub",
          "description": "curated",
          "generated_at": "2026-01-01T00:00:00Z",
          "plugins": [
            {{
              "id": "{id}",
              "type": "agent",
              "name": "Helper",
              "description": "A curated helper",
              "path": "agents/{id}.md",
              "content_sha": "{sha}",
              "source": "agency-agents",
              "upstream": "https://github.com/example/x",
              "license": "MIT",
              "tags": ["util"]
            }}
          ]
        }}"#
    )
}

/// A standalone HubPlugin pointing at `agents/<id>.md` with the given sha.
fn plugin(id: &str, sha: &str) -> HubPlugin {
    HubPlugin {
        id: id.to_string(),
        type_: "agent".to_string(),
        name: "Helper".to_string(),
        description: "A curated helper".to_string(),
        path: format!("agents/{id}.md"),
        content_sha: sha.to_string(),
        source: String::new(),
        upstream: String::new(),
        license: String::new(),
        tags: vec![],
    }
}

#[tokio::test]
async fn round_trip_fetch_parse_install_and_list() {
    let proj = TempDir::new().unwrap();
    let id = "helper";
    let base = spawn_fake_hub(
        good_index_json(id),
        vec![(format!("agents/{id}.md"), AGENT_BODY.to_string())],
        2, // GET /index.json + GET /agents/helper.md
    );

    // fetch_index parses the catalog.
    let index = fetch_index(&base).await.expect("index fetch+parse");
    assert_eq!(index.plugins.len(), 1);
    let p = index.find(id).expect("plugin in index").clone();
    assert_eq!(p.type_, "agent");

    // install_plugin writes the body verbatim under .claude/agents/.
    let res = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect("install must succeed");
    assert_eq!(res.id, id);
    assert_eq!(res.type_, "agent");
    assert!(!res.overwrote);
    let dest = proj.path().join(".claude/agents/helper.md");
    assert_eq!(res.path, dest);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), AGENT_BODY);

    // ccteam_core::list_roles surfaces the freshly-installed agent.
    let roles = ccteam_core::list_roles(proj.path()).unwrap();
    assert!(
        roles.iter().any(|r| r.role == "helper"),
        "installed agent must appear in list_roles; got {roles:?}"
    );
}

#[tokio::test]
async fn install_sha_mismatch_is_refused() {
    let proj = TempDir::new().unwrap();
    // Plugin advertises a wrong sha → integrity check must fail, nothing written.
    let p = plugin("helper", &sha256_hex(b"some other content"));
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", AGENT_BODY);

    let err = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect_err("sha mismatch must be refused");
    assert!(
        matches!(err, HubError::ShaMismatch { .. }),
        "expected ShaMismatch, got {err:?}"
    );
    assert!(
        !proj.path().join(".claude/agents/helper.md").exists(),
        "no file should be written on a sha mismatch"
    );
}

#[tokio::test]
async fn install_404_is_bad_status() {
    let proj = TempDir::new().unwrap();
    let p = plugin("helper", &sha256_hex(AGENT_BODY.as_bytes()));
    let base = spawn_oneshot_http("HTTP/1.1 404 Not Found", "nope");

    let err = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect_err("a 404 body must be an honest error");
    match err {
        HubError::BadStatus { status, .. } => assert_eq!(status, 404),
        other => panic!("expected BadStatus 404, got {other:?}"),
    }
    assert!(!proj.path().join(".claude/agents/helper.md").exists());
}

#[tokio::test]
async fn install_oversize_body_is_refused() {
    let proj = TempDir::new().unwrap();
    let big = vec![b'x'; MAX_HUB_BODY_BYTES + 1];
    // sha is irrelevant — the size cap fires before the integrity check.
    let p = plugin("helper", &sha256_hex(&big));
    let base = spawn_oneshot_http_dyn("HTTP/1.1 200 OK", "", big);

    let err = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect_err("an over-cap body must be refused");
    match err {
        HubError::TooLarge { max, .. } => assert_eq!(max, MAX_HUB_BODY_BYTES),
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert!(!proj.path().join(".claude/agents/helper.md").exists());
}

#[tokio::test]
async fn install_does_not_follow_redirects() {
    let proj = TempDir::new().unwrap();
    let p = plugin("helper", &sha256_hex(AGENT_BODY.as_bytes()));
    // 302 → another host; with redirects disabled the 302 surfaces as BadStatus.
    let base = spawn_oneshot_http_dyn(
        "HTTP/1.1 302 Found",
        "Location: http://evil.example/agents/helper.md\r\n",
        b"redirecting".to_vec(),
    );

    let err = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect_err("a redirect must NOT be followed");
    match err {
        HubError::BadStatus { status, .. } => assert!(
            (300..400).contains(&status),
            "expected a 3xx surfaced as BadStatus, got {status}"
        ),
        other => panic!("expected BadStatus for an un-followed redirect, got {other:?}"),
    }
    assert!(!proj.path().join(".claude/agents/helper.md").exists());
}

#[tokio::test]
async fn install_empty_body_is_refused() {
    let proj = TempDir::new().unwrap();
    // content_sha must match the (whitespace) body so we reach the empty check,
    // which fires *after* the integrity gate.
    let blank = "   \n  ";
    let p = plugin("helper", &sha256_hex(blank.as_bytes()));
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", blank);

    let err = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect_err("an empty body must be refused");
    assert!(
        matches!(err, HubError::EmptyBody(_)),
        "expected EmptyBody, got {err:?}"
    );
}

#[tokio::test]
async fn install_refuses_existing_then_force_overwrites() {
    let proj = TempDir::new().unwrap();
    let id = "helper";
    let p = plugin(id, &sha256_hex(AGENT_BODY.as_bytes()));

    // First install (one connection for the body).
    let base1 = spawn_oneshot_http("HTTP/1.1 200 OK", AGENT_BODY);
    let first = install_plugin(proj.path(), &p, None, false, &base1)
        .await
        .expect("first install succeeds");
    assert!(!first.overwrote);

    // Second install without force → refused before any fetch (so the base is
    // never contacted; point it at a dead port to prove it).
    let err = install_plugin(proj.path(), &p, None, false, "http://127.0.0.1:1")
        .await
        .expect_err("re-install without force must be refused");
    assert!(
        matches!(err, HubError::Exists(ref s) if s == id),
        "expected Exists(\"{id}\"), got {err:?}"
    );

    // With force it overwrites.
    let base2 = spawn_oneshot_http("HTTP/1.1 200 OK", AGENT_BODY);
    let forced = install_plugin(proj.path(), &p, None, true, &base2)
        .await
        .expect("forced re-install succeeds");
    assert!(
        forced.overwrote,
        "force over an existing file sets overwrote"
    );
}

#[tokio::test]
async fn install_workflow_type_is_unsupported() {
    let proj = TempDir::new().unwrap();
    let mut p = plugin("flow", &sha256_hex(AGENT_BODY.as_bytes()));
    p.type_ = "workflow".to_string();
    p.path = "workflows/flow.yaml".to_string();
    // UnsupportedType fires before any fetch → dead port is fine.
    let err = install_plugin(proj.path(), &p, None, false, "http://127.0.0.1:1")
        .await
        .expect_err("workflow type is not yet installable");
    assert!(
        matches!(err, HubError::UnsupportedType(ref t) if t == "workflow"),
        "expected UnsupportedType(workflow), got {err:?}"
    );
}

#[tokio::test]
async fn install_skill_writes_nested_skill_md() {
    let proj = TempDir::new().unwrap();
    let skill_body = "---\nname: do-thing\ndescription: does a thing\n---\nbody\n";
    let mut p = plugin("do-thing", &sha256_hex(skill_body.as_bytes()));
    p.type_ = "skill".to_string();
    p.path = "skills/do-thing/SKILL.md".to_string();
    let base = spawn_oneshot_http_dyn("HTTP/1.1 200 OK", "", skill_body.as_bytes().to_vec());

    let res = install_plugin(proj.path(), &p, None, false, &base)
        .await
        .expect("skill install succeeds");
    assert_eq!(res.type_, "skill");
    let dest = proj.path().join(".claude/skills/do-thing/SKILL.md");
    assert_eq!(res.path, dest);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), skill_body);
}

#[tokio::test]
async fn load_catalog_caches_then_reads_offline() {
    // Isolate the hub cache under a throwaway CCTEAM_HOME.
    let home = TempDir::new().unwrap();
    std::env::set_var("CCTEAM_HOME", home.path());
    // (No CCTEAM_PROJECTS_ROOT needed — CcteamPaths::from_env only reads HOME
    //  for the cache dir here.)
    let paths = ccteam_core::CcteamPaths::from_env().unwrap();

    // First call with a live (one-shot) hub: refresh writes the cache.
    let base = spawn_oneshot_http_dyn(
        "HTTP/1.1 200 OK",
        "",
        good_index_json("helper").into_bytes(),
    );
    let idx = load_catalog(&base, &paths, true)
        .await
        .expect("refresh fetches + caches");
    assert_eq!(idx.plugins.len(), 1);
    let cache = paths.hub_cache_dir().join("index.json");
    assert!(cache.is_file(), "refresh must write the cache file");

    // Second call with refresh=false reads the cache — the server is gone
    // (one-shot already consumed), so a network read would fail. Point base at
    // a dead port to prove the cache path is taken (no network).
    let offline = load_catalog("http://127.0.0.1:1", &paths, false)
        .await
        .expect("offline read from cache must succeed");
    assert_eq!(offline.plugins.len(), 1);
    assert_eq!(offline.find("helper").unwrap().type_, "agent");

    std::env::remove_var("CCTEAM_HOME");
}

#[test]
fn installed_status_reflects_disk() {
    let proj = TempDir::new().unwrap();
    let sha = sha256_hex(AGENT_BODY.as_bytes());
    let p = plugin("helper", &sha);

    // Absent → NotInstalled.
    assert_eq!(
        installed_status(proj.path(), &p),
        InstalledStatus::NotInstalled
    );

    // Write the exact body → Installed (sha matches).
    write_agent(proj.path(), "helper", AGENT_BODY);
    assert_eq!(
        installed_status(proj.path(), &p),
        InstalledStatus::Installed
    );

    // A plugin whose index sha differs from the on-disk body → UpdateAvailable.
    let stale = plugin("helper", &sha256_hex(b"newer hub content"));
    assert_eq!(
        installed_status(proj.path(), &stale),
        InstalledStatus::UpdateAvailable
    );
}

fn write_agent(project_dir: &Path, id: &str, body: &str) {
    let dir = project_dir.join(".claude").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.md")), body).unwrap();
}
