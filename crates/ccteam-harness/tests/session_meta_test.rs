//! v0.8.21 — per-session `meta.json` write/read/list + external Claude session
//! discovery. The discovery tests mutate `$HOME` (the function reads
//! `~/.claude/projects/`), so they are `#[serial]` and live here in an
//! integration binary, never in a lib `#[cfg(test)]` module.

use std::collections::HashSet;
use std::path::Path;

use ccteam_harness::{
    discover_external_claude_sessions, list_session_metas, read_session_meta, write_session_meta,
    AgentVendor, PermissionMode, SessionMeta, SessionOrigin, SessionProtocol,
};
use serial_test::serial;
use tempfile::TempDir;

fn sample_meta(sid: &str, slug: &str, last_active: &str, uuid: &str) -> SessionMeta {
    SessionMeta {
        awaiting_observation: false,
        mode: None,
        tool_face: None,
        managed_by: Default::default(),
        stopped_at: None,
        sid: sid.into(),
        slug: slug.into(),
        vendor: AgentVendor::Claude,
        protocol: SessionProtocol::StreamJson,
        role: "cto".into(),
        permission_mode: PermissionMode::Skip,
        owner: "user:web-api".into(),
        vendor_uuid: uuid.into(),
        model: None,
        observed_model: None,
        effort: None,
        host: "local".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        last_active: last_active.into(),
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
    }
}

#[test]
fn meta_write_is_atomic_overwrite() {
    // A second write replaces the file in place (tmp + rename), leaving exactly
    // one parseable meta.json — no stale `.tmp` companion is read back.
    let proj = TempDir::new().unwrap();
    write_session_meta(
        proj.path(),
        &sample_meta("s1", "demo", "2026-06-01T00:00:00Z", "u1"),
    )
    .unwrap();
    write_session_meta(
        proj.path(),
        &sample_meta("s1", "demo", "2026-06-29T00:00:00Z", "u1-new"),
    )
    .unwrap();
    let got = read_session_meta(proj.path(), "s1").unwrap();
    assert_eq!(got.vendor_uuid, "u1-new", "second write wins");
    assert_eq!(got.last_active, "2026-06-29T00:00:00Z");
}

#[test]
fn list_session_metas_sorted_by_last_active_desc() {
    let proj = TempDir::new().unwrap();
    write_session_meta(
        proj.path(),
        &sample_meta("s1", "demo", "2026-06-01T00:00:00Z", "u1"),
    )
    .unwrap();
    write_session_meta(
        proj.path(),
        &sample_meta("s2", "demo", "2026-06-29T00:00:00Z", "u2"),
    )
    .unwrap();
    write_session_meta(
        proj.path(),
        &sample_meta("s3", "demo", "2026-06-15T00:00:00Z", "u3"),
    )
    .unwrap();

    let metas = list_session_metas(proj.path());
    assert_eq!(metas.len(), 3);
    assert_eq!(metas[0].sid, "s2", "newest last_active first");
    assert_eq!(metas[1].sid, "s3");
    assert_eq!(metas[2].sid, "s1");
}

#[test]
fn list_session_metas_empty_when_no_chat_dir() {
    let proj = TempDir::new().unwrap();
    assert!(list_session_metas(proj.path()).is_empty());
}

/// Build a fake `~/.claude/projects/<enc>/<uuid>.jsonl` with the given lines.
fn write_claude_jsonl(home: &Path, enc: &str, uuid: &str, lines: &[String]) {
    let dir = home.join(".claude").join("projects").join(enc);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{uuid}.jsonl")), lines.join("\n")).unwrap();
}

#[test]
#[serial]
fn discover_external_filters_cwd_excludes_known_and_subagents() {
    let home = TempDir::new().unwrap();
    let proj = TempDir::new().unwrap();
    let cwd = proj.path().to_string_lossy().to_string();

    // A: cwd matches → discoverable, carries a custom title.
    write_claude_jsonl(
        home.path(),
        "enc",
        "11111111-1111-1111-1111-111111111111",
        &[
            format!(r#"{{"type":"user","cwd":"{cwd}"}}"#),
            r#"{"type":"custom-title","customTitle":"refactor gateway"}"#.to_string(),
        ],
    );
    // B: a DIFFERENT cwd → excluded (content-based filter, not path encoding).
    write_claude_jsonl(
        home.path(),
        "enc",
        "22222222-2222-2222-2222-222222222222",
        &[r#"{"type":"user","cwd":"/some/other/project"}"#.to_string()],
    );
    // C: cwd matches but uuid is already adopted (known) → excluded.
    write_claude_jsonl(
        home.path(),
        "enc",
        "33333333-3333-3333-3333-333333333333",
        &[format!(r#"{{"type":"user","cwd":"{cwd}"}}"#)],
    );
    // D: cwd matches but it's a subagent transcript (first line agent-setting).
    write_claude_jsonl(
        home.path(),
        "enc",
        "44444444-4444-4444-4444-444444444444",
        &[
            r#"{"type":"agent-setting"}"#.to_string(),
            format!(r#"{{"type":"user","cwd":"{cwd}"}}"#),
        ],
    );

    std::env::set_var("HOME", home.path());
    let known: HashSet<String> = ["33333333-3333-3333-3333-333333333333".to_string()]
        .into_iter()
        .collect();
    let found = discover_external_claude_sessions(proj.path(), &known);
    let uuids: Vec<&str> = found.iter().map(|s| s.vendor_uuid.as_str()).collect();

    assert!(
        uuids.contains(&"11111111-1111-1111-1111-111111111111"),
        "A: matching cwd"
    );
    assert!(
        !uuids.contains(&"22222222-2222-2222-2222-222222222222"),
        "B: other cwd excluded"
    );
    assert!(
        !uuids.contains(&"33333333-3333-3333-3333-333333333333"),
        "C: known uuid excluded"
    );
    assert!(
        !uuids.contains(&"44444444-4444-4444-4444-444444444444"),
        "D: subagent excluded"
    );

    let a = found
        .iter()
        .find(|s| s.vendor_uuid == "11111111-1111-1111-1111-111111111111")
        .unwrap();
    assert_eq!(a.title, "refactor gateway", "custom title extracted");
    assert_eq!(a.cwd.trim_end_matches('/'), cwd.trim_end_matches('/'));
}

#[test]
#[serial]
fn discover_external_survives_cjk_tail_cut() {
    // Fix 5 regression — read_tail reads the last 16 KiB; a CJK-heavy transcript
    // makes that boundary land mid-UTF-8. A strict from_utf8 would discard the
    // WHOLE tail and the session would be undiscoverable; from_utf8_lossy keeps
    // it: the partial first line fails to parse (harmless), the intact cwd line
    // on the tail still resolves.
    let home = TempDir::new().unwrap();
    let proj = TempDir::new().unwrap();
    let cwd = proj.path().to_string_lossy().to_string();

    // ~36 KB of CJK on the first line so the 16 KiB tail starts mid-character.
    let big_cjk = "観".repeat(12_000);
    write_claude_jsonl(
        home.path(),
        "enc",
        "55555555-5555-5555-5555-555555555555",
        &[
            format!(r#"{{"type":"user","text":"{big_cjk}"}}"#),
            format!(r#"{{"type":"user","cwd":"{cwd}"}}"#),
        ],
    );

    std::env::set_var("HOME", home.path());
    let found = discover_external_claude_sessions(proj.path(), &HashSet::new());
    assert!(
        found
            .iter()
            .any(|s| s.vendor_uuid == "55555555-5555-5555-5555-555555555555"),
        "a CJK-heavy transcript whose 16 KiB tail cuts mid-char must still be discoverable"
    );
}

#[test]
#[serial]
fn discover_external_none_when_home_has_no_claude_projects() {
    let home = TempDir::new().unwrap();
    let proj = TempDir::new().unwrap();
    std::env::set_var("HOME", home.path());
    assert!(discover_external_claude_sessions(proj.path(), &HashSet::new()).is_empty());
}
