//! Vendor-side session-title push — the WRITE half of the session-title
//! system ([`super::session_meta::apply_title`] owns the ccteam-side SoT).
//!
//! ccteam already READS a vendor's own title (`custom-title` / `ai-title` in
//! Claude's transcript) when it imports or resumes a session. This module is
//! the symmetric direction: an explicit user rename in ccteam lands on the
//! vendor's own title surface, so the session reads the same way in
//! `claude --resume`'s picker as it does in the IM `/sessions` list and the
//! web rail.
//!
//! **Claude's surface is a file, on purpose**: the SDK's documented
//! `renameSession(sessionId, title)` "appends a custom-title entry to the
//! session's JSONL file", and the CLI absorbs any fresher externally-written
//! title from the transcript tail before re-appending its own cached one — so
//! an external writer is a first-class path, live session or not. That also
//! makes a rename work on a STOPPED session, which no RPC could.
//!
//! Two hard rules keep this honest:
//! 1. **Never create the transcript.** An empty `<uuid>.jsonl` would flip
//!    `session_jsonl_exists` and make the next spawn `--resume` a transcript
//!    with no messages. No file yet ⇒ [`TitleSync::Deferred`], never a lie.
//! 2. **Metadata only.** The appended entry is session metadata Claude reads
//!    for its picker; nothing enters the model's conversation (red line: no
//!    prompt injection).

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{SessionTitleTarget, TitleSync};

use super::transcript_tail::{active_session_id_path, anthropic_project_dir_in};

/// The exact entry Claude Code appends for a user rename:
/// `{"type":"custom-title","customTitle":…,"sessionId":…}`.
///
/// Field ORDER is load-bearing — Claude's tail scan matches lines with
/// `startsWith('{"type":"custom-title"')`, so `type` must serialize first
/// (serde preserves declaration order).
#[derive(Serialize)]
struct CustomTitleEntry<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(rename = "customTitle")]
    custom_title: &'a str,
    #[serde(rename = "sessionId")]
    session_id: &'a str,
}

/// Resolve the Anthropic session UUID whose transcript carries this session's
/// title: the live marker file first (the terminal protocol rewrites
/// `active-session-id` on every `SessionStart`, so `meta.json`'s uuid can be
/// stale after a `/clear`), else the recorded `vendor_uuid` (the stream-json
/// path, whose uuid is derived deterministically from `(slug, sid)` and never
/// drifts). `None` when neither is known.
fn resolve_claude_uuid(target: &SessionTitleTarget) -> Option<String> {
    let marker = active_session_id_path(&target.project_dir, &target.sid);
    if let Ok(raw) = std::fs::read_to_string(&marker) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let uuid = target.vendor_uuid.trim();
    (!uuid.is_empty()).then(|| uuid.to_string())
}

/// Push `title` onto the Claude transcript's `custom-title` entry, resolving
/// `~/.claude/projects/` from the user home. Shared by BOTH Claude adapters
/// (stream-json and the frozen terminal protocol) — the title surface is a
/// property of the vendor, not of the wire protocol.
pub fn push_claude_custom_title(target: &SessionTitleTarget, title: &str) -> TitleSync {
    let Some(home) = dirs::home_dir() else {
        return TitleSync::Deferred("no home dir to resolve ~/.claude/projects".into());
    };
    push_claude_custom_title_in(&home.join(".claude").join("projects"), target, title)
}

/// [`push_claude_custom_title`] against an explicit Claude projects root — the
/// injection seam used by tests (never touches the real `~/.claude`).
pub fn push_claude_custom_title_in(
    claude_projects_root: &Path,
    target: &SessionTitleTarget,
    title: &str,
) -> TitleSync {
    let Some(uuid) = resolve_claude_uuid(target) else {
        return TitleSync::Deferred("no claude session uuid recorded yet".into());
    };
    let transcript: PathBuf = anthropic_project_dir_in(claude_projects_root, &target.project_dir)
        .join(format!("{uuid}.jsonl"));
    if !transcript.exists() {
        // Rule 1: never create it. The title still stands ccteam-side.
        return TitleSync::Deferred(
            "claude has not filed a transcript for this session yet".into(),
        );
    }
    let entry = CustomTitleEntry {
        kind: "custom-title",
        custom_title: title,
        session_id: &uuid,
    };
    let Ok(mut line) = serde_json::to_string(&entry) else {
        return TitleSync::Deferred("could not encode the custom-title entry".into());
    };
    line.push('\n');
    match std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .and_then(|mut f| f.write_all(line.as_bytes()))
    {
        Ok(()) => TitleSync::Pushed,
        Err(err) => TitleSync::Deferred(format!("transcript append failed: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::transcript_tail::encode_project_cwd;
    use crate::{AgentVendor, ExecutionMode, ThreadHandle};

    fn target(project_dir: &Path, uuid: &str) -> SessionTitleTarget {
        SessionTitleTarget {
            sid: "s7".into(),
            vendor_uuid: uuid.into(),
            project_dir: project_dir.to_path_buf(),
            thread: None,
        }
    }

    /// Build `<root>/<encoded cwd>/<uuid>.jsonl` with one pre-existing line, so
    /// the append path has a real transcript to extend.
    fn seed_transcript(root: &Path, cwd: &Path, uuid: &str) -> PathBuf {
        let dir = root.join(encode_project_cwd(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{uuid}.jsonl"));
        std::fs::write(&path, "{\"type\":\"summary\",\"summary\":\"prior\"}\n").unwrap();
        path
    }

    #[test]
    fn appends_a_custom_title_entry_claude_can_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude-projects");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let uuid = "11111111-2222-4333-8444-555555555555";
        let transcript = seed_transcript(&root, &cwd, uuid);

        let sync = push_claude_custom_title_in(&root, &target(&cwd, uuid), "ship the rename");
        assert_eq!(sync, TitleSync::Pushed);

        let raw = std::fs::read_to_string(&transcript).unwrap();
        let last = raw.lines().next_back().unwrap();
        // Claude's tail scan matches on this exact prefix — key order matters.
        assert!(
            last.starts_with("{\"type\":\"custom-title\""),
            "custom-title must lead the line: {last}"
        );
        let parsed: serde_json::Value = serde_json::from_str(last).unwrap();
        assert_eq!(parsed["customTitle"], "ship the rename");
        assert_eq!(parsed["sessionId"], uuid);
        // The prior content is intact (append, never rewrite).
        assert!(raw.contains("\"prior\""));
    }

    #[test]
    fn never_creates_a_transcript_that_does_not_exist_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude-projects");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let uuid = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

        let sync = push_claude_custom_title_in(&root, &target(&cwd, uuid), "too early");
        assert!(
            matches!(sync, TitleSync::Deferred(ref why) if why.contains("transcript")),
            "expected an honest Deferred, got {sync:?}"
        );
        // A phantom jsonl here would make the next spawn `--resume` an empty
        // transcript (`session_jsonl_exists`).
        assert!(!root
            .join(encode_project_cwd(&cwd))
            .join(format!("{uuid}.jsonl"))
            .exists());
    }

    #[test]
    fn live_marker_uuid_wins_over_a_stale_recorded_uuid() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude-projects");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let stale = "00000000-1111-4222-8333-444444444444";
        let live = "99999999-8888-4777-8666-555555555555";
        let stale_path = seed_transcript(&root, &cwd, stale);
        let live_path = seed_transcript(&root, &cwd, live);
        // The terminal protocol's hook rewrites this on every SessionStart.
        let marker = active_session_id_path(&cwd, "s7");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, format!("{live}\n")).unwrap();

        let sync = push_claude_custom_title_in(&root, &target(&cwd, stale), "after /clear");
        assert_eq!(sync, TitleSync::Pushed);
        assert!(std::fs::read_to_string(&live_path)
            .unwrap()
            .contains("after /clear"));
        assert!(
            !std::fs::read_to_string(&stale_path)
                .unwrap()
                .contains("after /clear"),
            "the stale meta uuid must not receive the title"
        );
    }

    #[test]
    fn no_uuid_at_all_is_deferred_not_a_silent_success() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude-projects");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let sync = push_claude_custom_title_in(&root, &target(&cwd, "  "), "nowhere to go");
        assert!(matches!(sync, TitleSync::Deferred(ref why) if why.contains("uuid")));
    }

    #[test]
    fn a_live_thread_handle_does_not_change_the_file_path() {
        // The claude surface is the transcript either way — `thread` presence
        // is irrelevant, which is what makes rename-while-stopped work.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude-projects");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let uuid = "12121212-3434-4565-8787-909090909090";
        let transcript = seed_transcript(&root, &cwd, uuid);
        let mut t = target(&cwd, uuid);
        t.thread = Some(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: uuid.into(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        });

        assert_eq!(
            push_claude_custom_title_in(&root, &t, "live rename"),
            TitleSync::Pushed
        );
        assert!(std::fs::read_to_string(&transcript)
            .unwrap()
            .contains("live rename"));
    }
}
