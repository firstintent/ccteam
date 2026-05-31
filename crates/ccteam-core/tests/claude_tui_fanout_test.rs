//! F177 — squad-mode fan-out acceptance test.
//!
//! Three bots share one project dir → three `tail_loop`s simultaneously.
//! Before F177, each loop called `discover_active_session(cwd)` which
//! picked the most-recently-modified `<sid>.jsonl` under
//! `~/.claude/projects/<encoded-cwd>/`, so all three loops read whichever
//! jsonl bot-A just wrote → fan-out (every `@<bot>` in the group chat
//! triggered three identical replies).
//!
//! With F177 each bot's tail loop is targeted at the sid recorded in
//! `<project>/.ccteam/chat/<role>/active-session-id` (written by the
//! F176 chat-progress hook on SessionStart). When bot-A appends a turn
//! to its own jsonl, only bot-A's event stream should observe it.
//!
//! This test exercises the tail loop end-to-end via
//! `ClaudeTuiAdapter::events`, plants the markers + transcripts that
//! the production hook + Anthropic CLI would have created, then asserts
//! per-stream isolation. It doesn't depend on tmux because the tail
//! loop only reads from disk.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use ccteam_core::execution::claude_tui::ClaudeTuiAdapter;
use ccteam_core::execution::transcript_tail::{active_session_id_path, encode_project_cwd};
use ccteam_harness::{
    AgentVendor, ExecutionMode, HarnessAdapter, ThreadEvent, ThreadHandle, ThreadItemDetails,
};
use futures::StreamExt;
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;

/// Assemble a `ThreadHandle` that drives `ClaudeTuiAdapter::events`
/// straight into a tail loop pointed at our hermetic temp tree.
fn make_handle(project_dir: &Path, cwd: &Path, role: &str) -> ThreadHandle {
    ThreadHandle {
        vendor: AgentVendor::Claude,
        mode: ExecutionMode::Chat,
        identity: format!("ccteam-chat-fanout-{role}"),
        started_at: chrono::Utc::now(),
        raw_extras: json!({
            "tmux_session": format!("ccteam-chat-fanout-{role}"),
            "role": role,
            "project_dir": project_dir.to_string_lossy(),
            "cwd": cwd.to_string_lossy(),
            "slug": "fanout",
        }),
    }
}

/// Plant the active-session-id marker the way the F176 hook would.
fn plant_marker(project_dir: &Path, role: &str, sid: &str) {
    let marker = active_session_id_path(project_dir, role);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, sid).unwrap();
}

/// Path to a bot's Anthropic transcript jsonl under the hermetic HOME
/// we point `dirs::home_dir` at via the `HOME` env var.
fn transcript_path(home: &Path, cwd: &Path, sid: &str) -> PathBuf {
    home.join(".claude")
        .join("projects")
        .join(encode_project_cwd(cwd))
        .join(format!("{sid}.jsonl"))
}

/// Append one assistant-text transcript record to `path`. Models
/// Anthropic's transcript jsonl line shape so `parse_transcript_line`
/// emits an `ItemCompleted(AgentMessage)` event.
fn append_assistant_turn(path: &Path, uuid: &str, text: &str) {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let row = serde_json::to_string(&json!({
        "type": "assistant",
        "uuid": uuid,
        "message": {"content": [{"type": "text", "text": text}]}
    }))
    .unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{row}").unwrap();
    f.sync_all().unwrap();
}

/// Drain a `BoxStream<ThreadEvent>` for up to `max_ms`, collecting
/// every `ItemCompleted::AgentMessage` text seen. Returns when the
/// budget elapses without a new event — short timeouts keep the
/// negative-assertion fast.
async fn collect_agent_messages(
    mut stream: futures::stream::BoxStream<'static, ThreadEvent>,
    max_ms: u64,
) -> Vec<String> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(max_ms);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(ThreadEvent::ItemCompleted { item })) => {
                if let ThreadItemDetails::AgentMessage(text) = item.details {
                    out.push(text);
                }
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => break,
        }
    }
    out
}

/// Three bots in one project dir. Only bot-A's transcript gets a new
/// line; bots B/C must observe nothing on their event streams.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn three_bots_one_project_only_target_bot_observes_turn() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let project_dir = tmp.path().join("proj");
    let cwd = project_dir.clone();
    std::fs::create_dir_all(&project_dir).unwrap();

    // The Anthropic projects dir must exist before tail_loop arms its
    // inotify watcher (otherwise it loops on the existence check).
    let anth_dir = home
        .join(".claude")
        .join("projects")
        .join(encode_project_cwd(&cwd));
    std::fs::create_dir_all(&anth_dir).unwrap();

    // Plant one marker per bot, pointing at a distinct sid.
    let bots: [(&str, &str); 3] = [
        ("alice", "sid-aaaa"),
        ("bob", "sid-bbbb"),
        ("carol", "sid-cccc"),
    ];
    for (role, sid) in bots {
        plant_marker(&project_dir, role, sid);
        // Pre-create each bot's jsonl as empty so `read_marker_target`
        // sees it. Pre-creating also matters because the tail loop's
        // initial sweep would otherwise return None until first MODIFY.
        let path = transcript_path(&home, &cwd, sid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
    }

    let adapter = ClaudeTuiAdapter::new();
    let h_a = make_handle(&project_dir, &cwd, bots[0].0);
    let h_b = make_handle(&project_dir, &cwd, bots[1].0);
    let h_c = make_handle(&project_dir, &cwd, bots[2].0);
    let s_a = adapter.events(&h_a);
    let s_b = adapter.events(&h_b);
    let s_c = adapter.events(&h_c);

    // Let the watchers arm before we trigger the event.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bot-A receives a new turn. B and C jsonls remain unchanged.
    let path_a = transcript_path(&home, &cwd, bots[0].1);
    append_assistant_turn(&path_a, "uuid-a-1", "hello from alice");

    // Concurrent collection — each stream gets the same wall-clock
    // window to surface any cross-fire. The fanout bug would have all
    // three streams emit "hello from alice"; F177 confines it to A.
    let (msgs_a, msgs_b, msgs_c) = tokio::join!(
        collect_agent_messages(s_a, 2000),
        collect_agent_messages(s_b, 2000),
        collect_agent_messages(s_c, 2000),
    );

    assert!(
        msgs_a.iter().any(|t| t == "hello from alice"),
        "bot A must observe its own turn; got: {msgs_a:?}"
    );
    assert!(
        msgs_b.is_empty(),
        "bot B must NOT observe bot A's turn (this is the fan-out bug); got: {msgs_b:?}"
    );
    assert!(
        msgs_c.is_empty(),
        "bot C must NOT observe bot A's turn (this is the fan-out bug); got: {msgs_c:?}"
    );

    std::env::remove_var("HOME");
}
