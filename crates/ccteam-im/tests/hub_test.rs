//! Deterministic tests for the ccteam-hub backend (`ccteam_im::hub`),
//! track-upstream model.
//!
//! The backend fetches `index.json` from the hub base and plugin bodies from
//! each entry's `upstream` URL, then installs them into a project's
//! `.claude/`. These tests stand a tiny in-process HTTP/1.1 responder in for
//! `raw.githubusercontent.com` (loopback — on the fetch host-allowlist) so the
//! round-trip is real HTTP but never touches the network. The project dir is a
//! `TempDir`; `CCTEAM_HOME` is pointed at another `TempDir` so the hub cache
//! writes under a throwaway root, not the real `~/.ccteam`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

use ccteam_im::hub::{
    fetch_index, install_plugin, installed_status, load_catalog, HubError, HubPlugin,
    InstalledStatus, ManifestEntry, MAX_HUB_BODY_BYTES,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// Single-shot HTTP/1.1 responder (one connection, then exit) — the fake body
/// host for single-fetch behavior cases (sha mismatch / 404 / oversize / …).
/// Returns `http://127.0.0.1:<port>` (loopback → host-allowlisted).
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
/// (e.g. a `Location:` for the redirect test).
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

/// Bind a loopback listener and return `(listener, base_url)` so the caller can
/// build an index whose `upstream` URLs point back at `base` BEFORE handing the
/// listener to [`serve_routes`].
fn bind_loopback() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

/// Serve a `path → body` map (path without the leading `/`) for every
/// connection until the listener drops. Unknown paths 404. The multi-route
/// fake hub used by the round-trip + multi-file install tests.
fn serve_routes(listener: TcpListener, routes: Vec<(String, String)>) {
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let body: Option<&str> = routes
            .iter()
            .find(|(p, _)| format!("/{p}") == path)
            .map(|(_, b)| b.as_str());
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
    });
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

/// An `index.json` with one agent plugin whose `content_sha` is the real
/// sha256 of `AGENT_BODY` and whose `upstream` points at `{base}/agents/<id>.md`
/// (so the engine's upstream-fetch hits the fake hub, not github).
fn good_index_json(id: &str, base: &str) -> String {
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
              "upstream": "{base}/agents/{id}.md",
              "content_sha": "{sha}",
              "source": "agency-agents",
              "license": "MIT",
              "tags": ["util"]
            }}
          ]
        }}"#
    )
}

/// A standalone single-file agent HubPlugin whose `upstream` is
/// `{base}/agents/<id>.md` (the body host); `base` is a loopback fake or a
/// dead/disallowed host depending on what the test exercises.
fn plugin(base: &str, id: &str, sha: &str) -> HubPlugin {
    HubPlugin {
        id: id.to_string(),
        type_: "agent".to_string(),
        name: "Helper".to_string(),
        description: "A curated helper".to_string(),
        upstream: format!("{base}/agents/{id}.md"),
        content_sha: sha.to_string(),
        source: String::new(),
        license: String::new(),
        tags: vec![],
        manifest: None,
    }
}

#[tokio::test]
async fn round_trip_fetch_parse_install_and_list() {
    let proj = TempDir::new().unwrap();
    let id = "helper";
    // Bind first so the index `upstream` can point back at this base.
    let (listener, base) = bind_loopback();
    let index_json = good_index_json(id, &base);
    serve_routes(
        listener,
        vec![
            ("index.json".to_string(), index_json),
            (format!("agents/{id}.md"), AGENT_BODY.to_string()),
        ],
    );

    // fetch_index parses the catalog.
    let index = fetch_index(&base).await.expect("index fetch+parse");
    assert_eq!(index.plugins.len(), 1);
    let p = index.find(id).expect("plugin in index").clone();
    assert_eq!(p.type_, "agent");

    // install_plugin writes the body verbatim under .claude/agents/.
    let res = install_plugin(proj.path(), &p, None, false)
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
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", AGENT_BODY);
    let p = plugin(&base, "helper", &sha256_hex(b"some other content"));

    let err = install_plugin(proj.path(), &p, None, false)
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
    let base = spawn_oneshot_http("HTTP/1.1 404 Not Found", "nope");
    let p = plugin(&base, "helper", &sha256_hex(AGENT_BODY.as_bytes()));

    let err = install_plugin(proj.path(), &p, None, false)
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
    let base = spawn_oneshot_http_dyn("HTTP/1.1 200 OK", "", big.clone());
    // sha is irrelevant — the size cap fires before the integrity check.
    let p = plugin(&base, "helper", &sha256_hex(&big));

    let err = install_plugin(proj.path(), &p, None, false)
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
    // 302 → another host; with redirects disabled the 302 surfaces as BadStatus.
    let base = spawn_oneshot_http_dyn(
        "HTTP/1.1 302 Found",
        "Location: http://evil.example/agents/helper.md\r\n",
        b"redirecting".to_vec(),
    );
    let p = plugin(&base, "helper", &sha256_hex(AGENT_BODY.as_bytes()));

    let err = install_plugin(proj.path(), &p, None, false)
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
    let base = spawn_oneshot_http("HTTP/1.1 200 OK", blank);
    let p = plugin(&base, "helper", &sha256_hex(blank.as_bytes()));

    let err = install_plugin(proj.path(), &p, None, false)
        .await
        .expect_err("an empty body must be refused");
    assert!(
        matches!(err, HubError::EmptyBody(_)),
        "expected EmptyBody, got {err:?}"
    );
}

#[tokio::test]
async fn fetch_from_disallowed_host_is_refused() {
    let proj = TempDir::new().unwrap();
    // upstream host = github.com (not raw.githubusercontent.com, not loopback)
    // → refused before any network I/O.
    let p = plugin(
        "https://github.com",
        "helper",
        &sha256_hex(AGENT_BODY.as_bytes()),
    );
    let err = install_plugin(proj.path(), &p, None, false)
        .await
        .expect_err("a non-allowlisted host must be refused");
    assert!(
        matches!(err, HubError::HostNotAllowed { .. }),
        "expected HostNotAllowed, got {err:?}"
    );
    assert!(!proj.path().join(".claude/agents/helper.md").exists());
}

#[tokio::test]
async fn install_refuses_existing_then_force_overwrites() {
    let proj = TempDir::new().unwrap();
    let id = "helper";
    let sha = sha256_hex(AGENT_BODY.as_bytes());

    // First install (one connection for the body).
    let base1 = spawn_oneshot_http("HTTP/1.1 200 OK", AGENT_BODY);
    let p1 = plugin(&base1, id, &sha);
    let first = install_plugin(proj.path(), &p1, None, false)
        .await
        .expect("first install succeeds");
    assert!(!first.overwrote);

    // Second install without force → refused before any fetch (so the host is
    // never contacted; point upstream at a dead port to prove it).
    let p_dead = plugin("http://127.0.0.1:1", id, &sha);
    let err = install_plugin(proj.path(), &p_dead, None, false)
        .await
        .expect_err("re-install without force must be refused");
    assert!(
        matches!(err, HubError::Exists(ref s) if s == id),
        "expected Exists(\"{id}\"), got {err:?}"
    );

    // With force it overwrites.
    let base2 = spawn_oneshot_http("HTTP/1.1 200 OK", AGENT_BODY);
    let p2 = plugin(&base2, id, &sha);
    let forced = install_plugin(proj.path(), &p2, None, true)
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
    let mut p = plugin(
        "http://127.0.0.1:1",
        "flow",
        &sha256_hex(AGENT_BODY.as_bytes()),
    );
    p.type_ = "workflow".to_string();
    // UnsupportedType fires before any fetch → dead port is fine.
    let err = install_plugin(proj.path(), &p, None, false)
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
    let base = spawn_oneshot_http_dyn("HTTP/1.1 200 OK", "", skill_body.as_bytes().to_vec());
    let mut p = plugin(&base, "do-thing", &sha256_hex(skill_body.as_bytes()));
    p.type_ = "skill".to_string();

    let res = install_plugin(proj.path(), &p, None, false)
        .await
        .expect("skill install succeeds");
    assert_eq!(res.type_, "skill");
    let dest = proj.path().join(".claude/skills/do-thing/SKILL.md");
    assert_eq!(res.path, dest);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), skill_body);
}

#[tokio::test]
async fn install_multi_file_skill_lands_every_file() {
    let proj = TempDir::new().unwrap();
    let skill_md = "---\nname: tdd\ndescription: test-driven\n---\nbody\n";
    let script = "#!/bin/sh\necho hi\n";
    let (listener, base) = bind_loopback();
    // `upstream` points at SKILL.md; the manifest's other files are derived
    // from its dir (`{base}/skills/eng/tdd/<relpath>`).
    let p = HubPlugin {
        id: "tdd".to_string(),
        type_: "skill".to_string(),
        name: "tdd".to_string(),
        description: "test-driven".to_string(),
        upstream: format!("{base}/skills/eng/tdd/SKILL.md"),
        content_sha: sha256_hex(skill_md.as_bytes()),
        source: "mattpocock-skills".to_string(),
        license: "MIT".to_string(),
        tags: vec!["engineering".to_string()],
        manifest: Some(vec![
            ManifestEntry {
                relpath: "SKILL.md".to_string(),
                content_sha: sha256_hex(skill_md.as_bytes()),
            },
            ManifestEntry {
                relpath: "scripts/run.sh".to_string(),
                content_sha: sha256_hex(script.as_bytes()),
            },
        ]),
    };
    serve_routes(
        listener,
        vec![
            ("skills/eng/tdd/SKILL.md".to_string(), skill_md.to_string()),
            (
                "skills/eng/tdd/scripts/run.sh".to_string(),
                script.to_string(),
            ),
        ],
    );

    let res = install_plugin(proj.path(), &p, None, false)
        .await
        .expect("multi-file skill install succeeds");
    assert_eq!(res.type_, "skill");
    let skill_dir = proj.path().join(".claude/skills/tdd");
    // InstallResult.path is the primary SKILL.md.
    assert_eq!(res.path, skill_dir.join("SKILL.md"));
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap(),
        skill_md
    );
    // The sibling resource landed under scripts/ verbatim.
    assert_eq!(
        std::fs::read_to_string(skill_dir.join("scripts/run.sh")).unwrap(),
        script
    );
}

#[tokio::test]
async fn multi_file_skill_sha_mismatch_writes_nothing() {
    let proj = TempDir::new().unwrap();
    let skill_md = "---\nname: tdd\n---\nbody\n";
    let script = "echo hi\n";
    let (listener, base) = bind_loopback();
    // The script's advertised sha is WRONG → the (pass-1) verify fails before
    // any file is written (atomic multi-file install).
    let p = HubPlugin {
        id: "tdd".to_string(),
        type_: "skill".to_string(),
        name: "tdd".to_string(),
        description: String::new(),
        upstream: format!("{base}/skills/eng/tdd/SKILL.md"),
        content_sha: sha256_hex(skill_md.as_bytes()),
        source: String::new(),
        license: String::new(),
        tags: vec![],
        manifest: Some(vec![
            ManifestEntry {
                relpath: "SKILL.md".to_string(),
                content_sha: sha256_hex(skill_md.as_bytes()),
            },
            ManifestEntry {
                relpath: "scripts/run.sh".to_string(),
                content_sha: sha256_hex(b"a totally different script"),
            },
        ]),
    };
    serve_routes(
        listener,
        vec![
            ("skills/eng/tdd/SKILL.md".to_string(), skill_md.to_string()),
            (
                "skills/eng/tdd/scripts/run.sh".to_string(),
                script.to_string(),
            ),
        ],
    );

    let err = install_plugin(proj.path(), &p, None, false)
        .await
        .expect_err("a manifest sha mismatch must abort the whole install");
    assert!(
        matches!(err, HubError::ShaMismatch { .. }),
        "expected ShaMismatch, got {err:?}"
    );
    assert!(
        !proj.path().join(".claude/skills/tdd").exists(),
        "no skill dir should be created when a manifest file fails its sha"
    );
}

#[tokio::test]
async fn load_catalog_caches_then_reads_offline() {
    // Isolate the hub cache under a throwaway CCTEAM_HOME.
    let home = TempDir::new().unwrap();
    std::env::set_var("CCTEAM_HOME", home.path());
    let paths = ccteam_core::CcteamPaths::from_env().unwrap();

    // First call with a live (one-shot) hub: refresh writes the cache. The
    // index's `upstream` is irrelevant here (no body fetch) — placeholder host.
    let base = spawn_oneshot_http_dyn(
        "HTTP/1.1 200 OK",
        "",
        good_index_json("helper", "http://127.0.0.1:1").into_bytes(),
    );
    let idx = load_catalog(&base, &paths, true)
        .await
        .expect("refresh fetches + caches");
    assert_eq!(idx.plugins.len(), 1);
    let cache = paths.hub_cache_dir().join("index.json");
    assert!(cache.is_file(), "refresh must write the cache file");

    // Second call with refresh=false reads the cache — the server is gone
    // (one-shot already consumed). Point base at a dead port to prove the cache
    // path is taken (no network).
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
    // upstream host is irrelevant for installed_status (no fetch).
    let p = plugin("http://127.0.0.1:1", "helper", &sha);

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
    let stale = plugin(
        "http://127.0.0.1:1",
        "helper",
        &sha256_hex(b"newer hub content"),
    );
    assert_eq!(
        installed_status(proj.path(), &stale),
        InstalledStatus::UpdateAvailable
    );
}

#[test]
fn installed_status_multi_file_skill_compares_whole_dir() {
    let proj = TempDir::new().unwrap();
    let skill_md = "---\nname: tdd\n---\nbody\n";
    let script = "echo hi\n";
    let p = HubPlugin {
        id: "tdd".to_string(),
        type_: "skill".to_string(),
        name: "tdd".to_string(),
        description: String::new(),
        upstream: "https://raw.githubusercontent.com/x/y/sha/skills/eng/tdd/SKILL.md".to_string(),
        content_sha: sha256_hex(skill_md.as_bytes()),
        source: String::new(),
        license: String::new(),
        tags: vec![],
        manifest: Some(vec![
            ManifestEntry {
                relpath: "SKILL.md".to_string(),
                content_sha: sha256_hex(skill_md.as_bytes()),
            },
            ManifestEntry {
                relpath: "scripts/run.sh".to_string(),
                content_sha: sha256_hex(script.as_bytes()),
            },
        ]),
    };
    let dir = proj.path().join(".claude/skills/tdd");

    // None present → NotInstalled.
    assert_eq!(
        installed_status(proj.path(), &p),
        InstalledStatus::NotInstalled
    );

    // Only SKILL.md present → partial → UpdateAvailable.
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("SKILL.md"), skill_md).unwrap();
    assert_eq!(
        installed_status(proj.path(), &p),
        InstalledStatus::UpdateAvailable
    );

    // All files present + matching → Installed.
    std::fs::write(dir.join("scripts/run.sh"), script).unwrap();
    assert_eq!(
        installed_status(proj.path(), &p),
        InstalledStatus::Installed
    );

    // Tamper a file → UpdateAvailable.
    std::fs::write(dir.join("scripts/run.sh"), "different\n").unwrap();
    assert_eq!(
        installed_status(proj.path(), &p),
        InstalledStatus::UpdateAvailable
    );
}

fn write_agent(project_dir: &Path, id: &str, body: &str) {
    let dir = project_dir.join(".claude").join("agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.md")), body).unwrap();
}
