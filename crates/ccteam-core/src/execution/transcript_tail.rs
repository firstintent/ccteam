//! V0.6.0 F108 — byte-offset incremental tail of Claude Code's
//! transcript jsonl at `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`.
//!
//! ## Why we tail this file
//!
//! Anthropic's `claude` CLI writes one JSONL line per message to a
//! per-session file under `~/.claude/projects/`. The file is internal
//! but **production-stable** (ccgram has shipped against it for >6
//! months). We tail it because the `chat-progress` hook stream gives us
//! *boundary* events (`SessionStart`, `Stop`, etc.) but not the
//! per-item content (assistant text, tool-use args, thinking blocks);
//! the transcript JSONL fills that gap.
//!
//! ## Cursor file
//!
//! Cursor state lives at
//! `<project>/.ccteam/chat/<bot>/transcript-cursor.json` (see
//! [`cursor_path`]). It carries the byte offset we last successfully
//! parsed up to, plus the discovered session-id (`<sid>.jsonl`) name so
//! we don't have to re-scan ~/.claude/projects/ every poll.
//!
//! ## Gotchas (ccgram + OMC ground truth)
//!
//! 1. **UTF-8 boundary safety** — if `last_byte_offset > 0`, peek one
//!    byte; if it isn't `{` (line-start), discard one full line so we
//!    re-sync to a JSON record boundary.
//! 2. **Half-flushed tail** — non-empty lines that fail JSON parse mean
//!    a writer is mid-flush. **Do not advance the cursor**; retry next
//!    tick.
//! 3. **Truncation / `/clear`** — when `file_size < cursor_offset`, the
//!    file was rotated. Reset cursor to 0 and discover a fresh sid.
//! 4. **Tool pairing across cycles** — `tool_use` may arrive in tick N
//!    and matching `tool_result` in tick N+1. Carry a pending-tools map
//!    in the loop closure so we can correlate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};

use ccteam_harness::{ThreadEvent, ThreadItem, ThreadItemDetails};

/// Persistent cursor written to
/// `<project>/.ccteam/chat/<bot>/transcript-cursor.json`. Stored as
/// JSON (not jsonl) — small, single-record state.
///
/// **Multi-session aware** (NAS flood fix): the cursor tracks a
/// per-session-id `byte_offset` map (`prior_offsets`) plus a `current`
/// `(session_id, byte_offset)` pointer. This closes a duplicate-emit
/// bug that surfaces when the user's main claude session spawns
/// Task-tool subagents — each subagent writes its own jsonl into the
/// same `~/.claude/projects/<encoded-cwd>/` dir, so
/// [`discover_active_session`] (picks most-recently-modified jsonl)
/// oscillates between the main session and subagent jsonls. With the
/// old single-cursor design, every oscillation reset
/// `byte_offset = 0` and re-emitted all events of the newly-picked
/// session → 15× duplicate Telegram sends per round-trip.
///
/// With per-sid tracking, switching to a known sid resumes from its
/// persisted offset (no re-emit), and a genuinely-new sid starts at
/// 0 once (the legitimate `/clear` or `/compact` rotation case).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptCursor {
    /// Encoded cwd path component (slashes → dashes; matches Anthropic
    /// `~/.claude/projects/<encoded>/` subdir).
    #[serde(default)]
    pub project_encoded: String,
    /// Current session-id (the `<sid>.jsonl` basename). Empty when no
    /// session has been associated yet.
    #[serde(default)]
    pub session_id: String,
    /// Byte offset we've parsed up to inside the **current**
    /// `<session_id>.jsonl`. Use [`switch_session`](Self::switch_session)
    /// when transitioning to a different sid — never set this to 0
    /// directly on a switch, or duplicates will surface for any sid
    /// we've already drained.
    #[serde(default)]
    pub byte_offset: u64,
    /// Last event uuid we saw. Defensive — lets a future migration
    /// detect a duplicate-emit bug without scanning the whole tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<String>,
    /// Per-sid byte offsets for sessions we've previously tailed.
    /// `switch_session` reads / writes this map so we resume on
    /// re-entry instead of restarting from byte 0.
    ///
    /// Bounded growth: `~/.claude/projects/<encoded>/` accumulates
    /// jsonls over a project's lifetime (one per `claude` invocation +
    /// one per Task-tool subagent). Realistically O(100s) entries per
    /// active project; entries are small (sid string + u64) so the
    /// JSON file stays well under a few KB.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub prior_offsets: HashMap<String, u64>,
}

impl TranscriptCursor {
    /// Load cursor file or return a fresh default.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let body =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let c: Self =
            serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))?;
        Ok(c)
    }

    /// Atomic-ish save: write to `<path>.tmp` + rename. Caller owns
    /// the parent dir.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Transition the cursor to `new_sid`. Saves the current sid's
    /// `byte_offset` into [`prior_offsets`](Self::prior_offsets) so a
    /// later return to the same sid resumes — never re-reads.
    ///
    /// - Same `new_sid` as current: no-op (returns `false`).
    /// - Known `new_sid` in `prior_offsets`: resumes at that offset
    ///   (no re-emit).
    /// - Unknown `new_sid`: starts at 0 (legitimate fresh session).
    ///
    /// Always clears [`last_event_id`](Self::last_event_id) on a real
    /// switch since per-session uuids don't carry over. Caller still
    /// owns persisting via [`save`](Self::save).
    pub fn switch_session(&mut self, new_sid: &str, project_encoded: String) -> bool {
        if self.session_id == new_sid {
            return false;
        }
        if !self.session_id.is_empty() {
            self.prior_offsets
                .insert(self.session_id.clone(), self.byte_offset);
        }
        let resume_offset = self.prior_offsets.get(new_sid).copied().unwrap_or(0);
        self.session_id = new_sid.to_string();
        self.byte_offset = resume_offset;
        self.last_event_id = None;
        self.project_encoded = project_encoded;
        true
    }
}

/// Encode a cwd path the way Anthropic does for
/// `~/.claude/projects/<encoded>/`: replace every `/` with `-`. Leading
/// `/` becomes a leading `-` (matches the on-disk layout — see
/// `~/.claude/projects/-home-rob-workplace-agents-ccteam/`).
pub fn encode_project_cwd(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    s.replace('/', "-")
}

/// Resolve `~/.claude/projects/<encoded-cwd>/`.
pub fn anthropic_project_dir(cwd: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".claude")
            .join("projects")
            .join(encode_project_cwd(cwd)),
    )
}

/// Resolve the cursor file path.
pub fn cursor_path(project_dir: &Path, bot_role: &str) -> PathBuf {
    super::turns_mirror::chat_dir(project_dir, bot_role).join("transcript-cursor.json")
}

/// Path to the marker file holding the bot's currently-active Anthropic
/// session_id (the `<sid>.jsonl` basename under
/// `~/.claude/projects/<encoded-cwd>/`). The `chat-progress` hook
/// rewrites this on every `SessionStart` and clears it on
/// `SessionEnd { reason: "clear" }`. The chat-mode tail loop reads it
/// to target the correct jsonl deterministically — three bots in one
/// project dir each get their own marker, so the tail loops can't
/// cross-fire.
pub fn active_session_id_path(project_dir: &Path, bot_role: &str) -> PathBuf {
    super::turns_mirror::chat_dir(project_dir, bot_role).join("active-session-id")
}

/// Pick the most recently-modified **main-session** `<sid>.jsonl`
/// under `~/.claude/projects/<encoded>/`. Returns `None` when the dir
/// is missing, empty, or contains only subagent jsonls.
///
/// **Subagent filter**: claude's Task tool spawns subagents into the
/// same project dir, each writing its own `<sid>.jsonl`. Without the
/// filter, `discover_active_session` returned whichever was most
/// recently modified — including subagents — and the tail loop
/// alternated between main and subagent files, re-emitting events
/// each time. The filter skips any jsonl whose first line declares
/// `"type":"agent-setting"` (the marker Anthropic writes for
/// subagent sessions). Main-session jsonls carry `"type":"last-prompt"`
/// or `"type":"summary"` and pass through.
pub fn discover_active_session(cwd: &Path) -> Option<(String, PathBuf)> {
    let dir = anthropic_project_dir(cwd)?;
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut best: Option<(std::time::SystemTime, String, PathBuf)> = None;
    for ent in entries.flatten() {
        let path = ent.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(meta) = ent.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if is_subagent_jsonl(&path) {
            continue;
        }
        match &best {
            Some((t, _, _)) if *t >= mtime => {}
            _ => best = Some((mtime, stem.to_string(), path)),
        }
    }
    best.map(|(_, sid, p)| (sid, p))
}

/// Check whether a jsonl file is a subagent transcript (Task-tool
/// spawn) rather than a main claude chat session. Subagents are
/// internal — their assistant messages must NOT be forwarded to the
/// user-facing IM channel.
///
/// Heuristic: subagent jsonls open with a `"type":"agent-setting"`
/// record (Anthropic-internal marker). Main-session jsonls open with
/// `"type":"last-prompt"`, `"type":"summary"`, or a user/assistant
/// record. We read the first line only and look for the subagent
/// marker as a substring — a parse-free check that survives schema
/// drift on other fields and tolerates very long lines.
///
/// Files that error on read are treated as **not** subagents
/// (fail-safe — better to tail a possibly-irrelevant file once than
/// to silently drop a real session).
pub fn is_subagent_jsonl(path: &Path) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut first_line = String::new();
    if BufReader::new(file).read_line(&mut first_line).is_err() {
        return false;
    }
    first_line.contains("\"type\":\"agent-setting\"")
        || first_line.contains("\"type\": \"agent-setting\"")
}

/// In-memory carry-over for `tool_use` ↔ `tool_result` pairing across
/// poll cycles. `tool_use_id` → tool name. Keep it small (the pending
/// set only spans one active assistant turn).
pub type PendingTools = HashMap<String, String>;

/// Result of one `read_new` pass.
#[derive(Debug, Default)]
pub struct TailDelta {
    /// New events to forward into the [`ThreadEvent`] stream.
    pub events: Vec<ThreadEvent>,
    /// New byte offset to persist (only advances past complete lines).
    pub new_offset: u64,
    /// Updated pending-tools map for the next tick.
    pub pending_tools: PendingTools,
    /// Last event uuid observed (advances `last_event_id` on cursor).
    pub last_event_id: Option<String>,
}

/// Read from `[cursor.byte_offset, EOF)` and translate each complete
/// JSONL line into one or more [`ThreadEvent`]s. Pure-ish: no cursor
/// write (caller persists on success), no event emit (caller forwards
/// to the channel).
///
/// Returns `Ok(None)` when the transcript file is absent (first-spawn /
/// pre-rotation case). Returns `Ok(Some(delta))` otherwise; the
/// delta's `new_offset` may equal the input offset (no new content) or
/// stop short of file end (half-flushed last line — retry next tick).
pub async fn read_new(
    transcript_path: &Path,
    cursor: &TranscriptCursor,
    mut pending: PendingTools,
) -> Result<Option<TailDelta>> {
    if !transcript_path.exists() {
        return Ok(None);
    }
    let meta = tokio::fs::metadata(transcript_path)
        .await
        .with_context(|| format!("stat {}", transcript_path.display()))?;
    let file_size = meta.len();

    let mut start_offset = cursor.byte_offset;
    if file_size < start_offset {
        // Truncated (e.g. /clear rewrote the file). Reset.
        start_offset = 0;
    }
    if file_size == start_offset {
        return Ok(Some(TailDelta {
            events: Vec::new(),
            new_offset: start_offset,
            pending_tools: pending,
            last_event_id: cursor.last_event_id.clone(),
        }));
    }

    let mut file = tokio::fs::File::open(transcript_path)
        .await
        .with_context(|| format!("open {}", transcript_path.display()))?;
    file.seek(std::io::SeekFrom::Start(start_offset))
        .await
        .with_context(|| format!("seek {} -> {}", transcript_path.display(), start_offset))?;

    // UTF-8 boundary safety: if we're seeking mid-file, peek one byte;
    // if it isn't a `{`, drop forward to the next line.
    if start_offset > 0 {
        let mut probe = [0u8; 1];
        let _ = file.read_exact(&mut probe).await;
        if probe[0] != b'{' {
            // Re-open + readline-discard.
            let mut reader = BufReader::new(file);
            let mut throwaway = String::new();
            let _ = reader.read_line(&mut throwaway).await;
            // Position now sits at the start of a fresh line.
            file = reader.into_inner();
        } else {
            // Re-seek back so we re-read the `{` byte we just peeked.
            file.seek(std::io::SeekFrom::Start(start_offset))
                .await
                .context("re-seek after probe")?;
        }
    }

    let pos_after_resync = file
        .stream_position()
        .await
        .context("stream_position after UTF-8 resync")?;

    let mut reader = BufReader::new(file);
    let mut safe_offset = pos_after_resync;
    let mut events: Vec<ThreadEvent> = Vec::new();
    let mut last_event_id = cursor.last_event_id.clone();

    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .context("read_line on transcript")?;
        if n == 0 {
            break;
        }
        // Only fully-terminated lines (ending in '\n') are safe to commit.
        if !line.ends_with('\n') {
            // Half-flushed tail; stop without advancing safe_offset.
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            safe_offset += n as u64;
            continue;
        }
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                // Malformed line — likely a writer mid-flush wrote a
                // partial record despite the newline (very rare). Don't
                // advance safe_offset; retry next tick.
                break;
            }
        };
        let line_events = parse_transcript_line(&parsed, &mut pending);
        for ev in &line_events {
            if let Some(id) = thread_event_uuid(ev) {
                last_event_id = Some(id);
            }
        }
        events.extend(line_events);
        safe_offset += n as u64;
    }

    Ok(Some(TailDelta {
        events,
        new_offset: safe_offset,
        pending_tools: pending,
        last_event_id,
    }))
}

/// Translate one transcript-jsonl row into 0..N `ThreadEvent`s.
///
/// Recognised row shapes (Anthropic transcript v1, ccgram-verified):
///
/// - `{"type":"assistant","message":{"content":[{"type":"text","text":...}]}}`
///   → `ItemCompleted(ThreadItem::AgentMessage)`
/// - `{"type":"assistant","message":{"content":[{"type":"tool_use",...}]}}`
///   → `ItemStarted(ThreadItem::ToolCall)` + stash in `pending`
/// - `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":...,"content":...}]}}`
///   → `ItemCompleted(ThreadItem::ToolCall { ..., "result": ... })` (if a
///   matching `tool_use_id` is pending) and pop it; otherwise emit a
///   standalone `ItemCompleted` with `tool_name = "unknown"`.
/// - `{"type":"assistant","message":{"content":[{"type":"thinking",...}]}}`
///   → `ItemUpdated(ThreadItem::Reasoning)`
/// - Any other `type` → no event.
pub fn parse_transcript_line(row: &Value, pending: &mut PendingTools) -> Vec<ThreadEvent> {
    let row_uuid = row
        .get("uuid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let typ = row.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let content = row
        .pointer("/message/content")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out: Vec<ThreadEvent> = Vec::new();
    for (idx, block) in content.iter().enumerate() {
        let btype = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let item_id = if row_uuid.is_empty() {
            format!("blk-{idx}")
        } else {
            format!("{row_uuid}-{idx}")
        };
        match (typ, btype) {
            ("assistant", "text") => {
                let text = block
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() {
                    out.push(ThreadEvent::ItemCompleted {
                        item: ThreadItem {
                            id: item_id,
                            details: ThreadItemDetails::AgentMessage(text),
                        },
                    });
                }
            }
            ("assistant", "thinking") => {
                let text = block
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() {
                    out.push(ThreadEvent::ItemUpdated {
                        item: ThreadItem {
                            id: item_id,
                            details: ThreadItemDetails::Reasoning(text),
                        },
                    });
                }
            }
            ("assistant", "tool_use") => {
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let tool_use_id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = block.get("input").cloned().unwrap_or(Value::Null);
                if !tool_use_id.is_empty() {
                    pending.insert(tool_use_id.clone(), name.clone());
                }
                out.push(ThreadEvent::ItemStarted {
                    item: ThreadItem {
                        id: if tool_use_id.is_empty() {
                            item_id
                        } else {
                            tool_use_id
                        },
                        details: ThreadItemDetails::ToolCall { name, args },
                    },
                });
            }
            ("user", "tool_result") => {
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = pending
                    .remove(&tool_use_id)
                    .unwrap_or_else(|| "unknown".to_string());
                let result_content = block.get("content").cloned().unwrap_or(Value::Null);
                out.push(ThreadEvent::ItemCompleted {
                    item: ThreadItem {
                        id: if tool_use_id.is_empty() {
                            item_id
                        } else {
                            tool_use_id
                        },
                        details: ThreadItemDetails::ToolCall {
                            name,
                            args: result_content,
                        },
                    },
                });
            }
            _ => {}
        }
    }
    out
}

fn thread_event_uuid(ev: &ThreadEvent) -> Option<String> {
    match ev {
        ThreadEvent::ItemStarted { item }
        | ThreadEvent::ItemUpdated { item }
        | ThreadEvent::ItemCompleted { item } => Some(item.id.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;
    use tempfile::TempDir;

    #[test]
    fn encode_project_cwd_uses_dashes() {
        let p = Path::new("/home/rob/workplace/agents/ccteam");
        assert_eq!(encode_project_cwd(p), "-home-rob-workplace-agents-ccteam");
    }

    #[test]
    fn cursor_round_trips_through_disk() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.json");
        let mut prior = HashMap::new();
        prior.insert("old-sid-1".to_string(), 100u64);
        prior.insert("old-sid-2".to_string(), 250u64);
        let c = TranscriptCursor {
            project_encoded: "-foo".into(),
            session_id: "abc".into(),
            byte_offset: 4242,
            last_event_id: Some("ev-1".into()),
            prior_offsets: prior,
        };
        c.save(&path).unwrap();
        let back = TranscriptCursor::load(&path).unwrap();
        assert_eq!(back.byte_offset, 4242);
        assert_eq!(back.session_id, "abc");
        assert_eq!(back.last_event_id.as_deref(), Some("ev-1"));
        assert_eq!(back.prior_offsets.get("old-sid-1"), Some(&100));
        assert_eq!(back.prior_offsets.get("old-sid-2"), Some(&250));
    }

    /// Regression: switching between a known sid and a different sid
    /// must NOT reset byte_offset to 0 on re-entry. This is the bug
    /// that caused the NAS Telegram duplicate flood — `discover_active_session`
    /// returns the most-recently-modified jsonl, which oscillates
    /// between the main claude session and Task-tool subagent jsonls.
    /// Each oscillation previously triggered `byte_offset = 0` and a
    /// full re-read of the newly-picked session.
    #[test]
    fn switch_session_resumes_known_sid() {
        let mut c = TranscriptCursor::default();
        // Bind to session A at offset 500.
        assert!(c.switch_session("session-A", "-proj".into()));
        c.byte_offset = 500;

        // Switch to session B (subagent jsonl). A's offset is persisted.
        assert!(c.switch_session("session-B", "-proj".into()));
        assert_eq!(c.byte_offset, 0, "unseen sid starts at 0");
        assert_eq!(c.prior_offsets.get("session-A"), Some(&500));
        c.byte_offset = 300;

        // Switch back to A — must resume at 500, NOT restart at 0.
        assert!(c.switch_session("session-A", "-proj".into()));
        assert_eq!(
            c.byte_offset, 500,
            "returning to a known sid must resume — restarting would re-emit all events"
        );
        assert_eq!(c.prior_offsets.get("session-B"), Some(&300));

        // Same-sid call is a no-op.
        assert!(!c.switch_session("session-A", "-proj".into()));
        assert_eq!(c.byte_offset, 500);
    }

    /// The architectural primary defense against the NAS duplicate flood:
    /// subagent jsonls (`"type":"agent-setting"` first line) must be
    /// filtered out so `discover_active_session` never picks them.
    /// Without this filter, `tail_loop` oscillates between main and
    /// subagent sessions and re-emits each on every switch.
    #[test]
    fn is_subagent_jsonl_detects_agent_setting_marker() {
        let tmp = TempDir::new().unwrap();
        let subagent = tmp.path().join("sa.jsonl");
        std::fs::write(
            &subagent,
            r#"{"type":"agent-setting","sessionId":"sa-1","cwd":"/x"}
{"type":"user","sessionId":"sa-1"}
"#,
        )
        .unwrap();
        assert!(is_subagent_jsonl(&subagent));

        let main = tmp.path().join("main.jsonl");
        std::fs::write(
            &main,
            r#"{"type":"last-prompt","sessionId":"main-1"}
{"type":"user","sessionId":"main-1"}
"#,
        )
        .unwrap();
        assert!(!is_subagent_jsonl(&main));

        // Tolerate the spaced JSON variant some serializers produce.
        let spaced = tmp.path().join("spaced.jsonl");
        std::fs::write(&spaced, r#"{"type": "agent-setting","sessionId":"sa-2"}"#).unwrap();
        assert!(is_subagent_jsonl(&spaced));

        // Missing / empty file → fail-safe to not-a-subagent.
        let missing = tmp.path().join("missing.jsonl");
        assert!(!is_subagent_jsonl(&missing));
    }

    /// Regression: `discover_active_session` must skip subagent jsonls
    /// even when they are the most-recently-modified file in the dir.
    /// This is the actual NAS-flood root cause condition — subagents
    /// write more frequently than the main session during a busy team
    /// orchestration.
    #[test]
    fn discover_skips_subagent_jsonls_even_when_newest() {
        let tmp_home = TempDir::new().unwrap();
        std::env::set_var("HOME", tmp_home.path());
        let cwd = Path::new("/home/test/proj");
        let dir = anthropic_project_dir(cwd).unwrap();
        std::fs::create_dir_all(&dir).unwrap();

        // Write the main session FIRST (so its mtime is older).
        let main_path = dir.join("main-sid.jsonl");
        std::fs::write(
            &main_path,
            r#"{"type":"last-prompt","sessionId":"main-sid"}"#,
        )
        .unwrap();

        // Then write a subagent jsonl — newer mtime.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let sa_path = dir.join("subagent-sid.jsonl");
        std::fs::write(
            &sa_path,
            r#"{"type":"agent-setting","sessionId":"subagent-sid"}"#,
        )
        .unwrap();

        let (picked_sid, picked_path) =
            discover_active_session(cwd).expect("should pick the main session, not the subagent");
        assert_eq!(picked_sid, "main-sid");
        assert_eq!(picked_path, main_path);
    }

    #[test]
    fn switch_session_clears_last_event_id_only_on_real_switch() {
        let mut c = TranscriptCursor {
            session_id: "sid".into(),
            byte_offset: 42,
            last_event_id: Some("ev".into()),
            ..Default::default()
        };
        assert!(!c.switch_session("sid", "-proj".into()));
        assert_eq!(
            c.last_event_id.as_deref(),
            Some("ev"),
            "no-op switch keeps last_event_id intact"
        );
        assert!(c.switch_session("other", "-proj".into()));
        assert!(
            c.last_event_id.is_none(),
            "real switch clears last_event_id — uuids don't cross sessions"
        );
    }

    #[test]
    fn cursor_load_missing_returns_default() {
        let c = TranscriptCursor::load(Path::new("/nope/missing.json")).unwrap();
        assert_eq!(c.byte_offset, 0);
        assert!(c.session_id.is_empty());
    }

    #[test]
    fn parse_assistant_text_emits_agent_message() {
        let row = json!({
            "type": "assistant",
            "uuid": "u1",
            "message": {"content": [{"type": "text", "text": "hello"}]}
        });
        let mut pending = PendingTools::new();
        let events = parse_transcript_line(&row, &mut pending);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ThreadEvent::ItemCompleted { item } => match &item.details {
                ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hello"),
                _ => panic!("wrong detail variant"),
            },
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn parse_tool_use_then_result_pairs_by_id() {
        let mut pending = PendingTools::new();
        let use_row = json!({
            "type": "assistant",
            "uuid": "u2",
            "message": {"content": [{
                "type": "tool_use",
                "id": "toolu-42",
                "name": "Read",
                "input": {"file_path": "/x"}
            }]}
        });
        let res_row = json!({
            "type": "user",
            "uuid": "u3",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": "toolu-42",
                "content": "ok"
            }]}
        });
        let e1 = parse_transcript_line(&use_row, &mut pending);
        assert_eq!(e1.len(), 1);
        assert!(matches!(e1[0], ThreadEvent::ItemStarted { .. }));
        assert!(pending.contains_key("toolu-42"));

        let e2 = parse_transcript_line(&res_row, &mut pending);
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            ThreadEvent::ItemCompleted { item } => match &item.details {
                ThreadItemDetails::ToolCall { name, .. } => assert_eq!(name, "Read"),
                _ => panic!("wrong detail"),
            },
            _ => panic!("wrong event"),
        }
        assert!(!pending.contains_key("toolu-42"));
    }

    #[test]
    fn parse_thinking_block_emits_reasoning() {
        let row = json!({
            "type": "assistant",
            "uuid": "u4",
            "message": {"content": [{"type": "thinking", "thinking": "let me see"}]}
        });
        let mut pending = PendingTools::new();
        let events = parse_transcript_line(&row, &mut pending);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ThreadEvent::ItemUpdated { item } => match &item.details {
                ThreadItemDetails::Reasoning(s) => assert_eq!(s, "let me see"),
                _ => panic!("wrong detail"),
            },
            _ => panic!("wrong event"),
        }
    }

    #[tokio::test]
    async fn read_new_returns_none_on_missing_file() {
        let tmp = TempDir::new().unwrap();
        let cursor = TranscriptCursor::default();
        let delta = read_new(&tmp.path().join("nope.jsonl"), &cursor, PendingTools::new())
            .await
            .unwrap();
        assert!(delta.is_none());
    }

    #[tokio::test]
    async fn read_new_stops_at_half_flushed_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sess.jsonl");
        let good = serde_json::to_string(&json!({
            "type":"assistant","uuid":"u1",
            "message":{"content":[{"type":"text","text":"hi"}]}
        }))
        .unwrap();
        // Good line + half-flushed tail (no trailing newline).
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(good.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        f.write_all(b"{\"type\":\"assistant\",\"message\":{\"content\":[{")
            .unwrap();
        let cursor = TranscriptCursor::default();
        let delta = read_new(&path, &cursor, PendingTools::new())
            .await
            .unwrap()
            .unwrap();
        // Only the first (complete) line emits an event.
        assert_eq!(delta.events.len(), 1);
        // Offset advanced past the good line but NOT past the partial.
        assert_eq!(delta.new_offset, (good.len() + 1) as u64);
    }

    #[tokio::test]
    async fn read_new_handles_truncation_reset() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sess.jsonl");
        let row = serde_json::to_string(&json!({
            "type":"assistant","uuid":"u1",
            "message":{"content":[{"type":"text","text":"after-clear"}]}
        }))
        .unwrap();
        std::fs::write(&path, format!("{row}\n")).unwrap();
        // Cursor claims we've already read 10_000 bytes (file is much
        // shorter) → truncation path.
        let cursor = TranscriptCursor {
            byte_offset: 10_000,
            ..Default::default()
        };
        let delta = read_new(&path, &cursor, PendingTools::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delta.events.len(), 1);
        assert_eq!(delta.new_offset, (row.len() + 1) as u64);
    }
}
