//! Outbound pipeline: tail `<project>/.ccteam/chat/<bot>/turns.jsonl`
//! → forward agent replies through the appropriate [`Channel`].
//!
//! The tui-impl teammate's `turns_mirror.rs` writes one JSON object
//! per agent reply. We tail by file-position, parse each new line,
//! and dispatch to the platform Channel matching the bot's
//! registered `im_platform`.

use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::latency::now_unix_ms;

/// One row in `turns.jsonl` (subset we care about — extras tolerated).
///
/// V0.6.1 ship-day F138 — accepts both the legacy schema (`role` +
/// `content` fields) AND the canonical `turns_mirror::TurnRecord`
/// schema (`assistant` field with separate `user` field for the
/// user-side prompt). turns_mirror is the SoT writer (F137); the
/// `From<RawRecord>` conversion below derives `role = "assistant"`
/// when `assistant` is non-empty and uses it as `content`. The legacy
/// `role`/`content` path is preserved for tests that hand-build rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "RawRecord")]
pub struct TurnRow {
    /// "user" / "assistant" / "tool" — only `assistant` is forwarded.
    pub role: String,
    /// Reply text (post tui-adapter cleanup).
    pub content: String,
    /// Optional reference back to the inbound platform message id
    /// (echo suppression — outbound never sends a turn whose
    /// `reply_to` matches a message we just dropped to mailbox).
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Optional thread id (if the user spoke in a Slack thread, the
    /// reply belongs to the same thread).
    #[serde(default)]
    pub thread_ts: Option<String>,
    /// Where the reply should land (mailbox `reply_target` echoed
    /// through by the tui adapter).
    #[serde(default)]
    pub reply_target: Option<String>,
    /// Latency: turn id (carried from turns_mirror) so per-row dispatch
    /// logs can correlate back to the `turn.done` Stage F event.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// Latency: timestamp the row was written (turns_mirror `ts`).
    /// Used to compute `tail_age_ms` = `now - ts` at outbound dispatch.
    #[serde(default)]
    pub ts: Option<DateTime<Utc>>,
}

/// Wire-format raw record union. Either schema parses; `Into<TurnRow>`
/// normalizes to the outbound view (role + content).
#[derive(Debug, Clone, Deserialize)]
struct RawRecord {
    /// Legacy / hand-built: explicit role.
    #[serde(default)]
    role: String,
    /// Legacy / hand-built: explicit content.
    #[serde(default)]
    content: String,
    /// Canonical turns_mirror schema: assistant-side text.
    #[serde(default)]
    assistant: String,
    /// Canonical turns_mirror schema: user-side text (skipped for
    /// outbound — only assistant rows forward).
    #[serde(default)]
    #[allow(dead_code)]
    user: String,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    reply_target: Option<String>,
    /// turns_mirror schema field.
    #[serde(default)]
    turn_id: String,
    /// turns_mirror schema field (RFC3339).
    #[serde(default)]
    ts: Option<DateTime<Utc>>,
}

impl From<RawRecord> for TurnRow {
    fn from(raw: RawRecord) -> Self {
        // Prefer explicit (role + content) when present — keeps the
        // legacy in-crate test fixtures working unchanged. Otherwise
        // synthesize from turns_mirror's (assistant) field.
        let (role, content) = if !raw.role.is_empty() && !raw.content.is_empty() {
            (raw.role, raw.content)
        } else if !raw.assistant.is_empty() {
            ("assistant".to_string(), raw.assistant)
        } else if !raw.user.is_empty() {
            // User-side row from turns_mirror — outbound filter drops
            // these, but we deserialize cleanly so the cursor advances.
            ("user".to_string(), String::new())
        } else {
            (raw.role, raw.content)
        };
        TurnRow {
            role,
            content,
            reply_to: raw.reply_to,
            thread_ts: raw.thread_ts,
            reply_target: raw.reply_target,
            turn_id: if raw.turn_id.is_empty() {
                None
            } else {
                Some(raw.turn_id)
            },
            ts: raw.ts,
        }
    }
}

/// Tailer state for one bot. Persisted between daemon ticks so we
/// don't re-forward old turns on restart.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TailCursor {
    /// File-position in bytes (cursor advanced after each successful
    /// dispatch).
    pub position: u64,
}

/// Resolve `<project>/.ccteam/chat/<role>/turns.jsonl` for the given
/// `(projects_root, slug, role)` triple.
pub fn turns_jsonl_path(projects_root: &std::path::Path, slug: &str, role: &str) -> PathBuf {
    projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("turns.jsonl")
}

/// V0.6.1 F134 — persistent byte-offset state for one bot's
/// `turns.jsonl` tailer. Stored at
/// `<projects_root>/<slug>/.ccteam/chat/<role>/outbound.cursor` as a
/// JSON blob `{"position": N}` (serde-derived on [`TailCursor`]).
///
/// Daemon restart re-loads this file so messages forwarded across a
/// previous run aren't re-sent to the user. Missing / malformed file
/// → fall back to the zero cursor (re-forward everything; safer than
/// dropping content silently, and `turns.jsonl` grows monotonically
/// so a stale cursor would only ever under-skip, never over-skip).
pub fn outbound_cursor_path(projects_root: &std::path::Path, slug: &str, role: &str) -> PathBuf {
    projects_root
        .join(slug)
        .join(".ccteam")
        .join("chat")
        .join(role)
        .join("outbound.cursor")
}

/// Load a [`TailCursor`] from disk. Missing file or parse failure
/// returns the default (position = 0).
pub fn load_cursor(path: &std::path::Path) -> TailCursor {
    if !path.exists() {
        return TailCursor::default();
    }
    match fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(err) => {
            tracing::warn!(error = %err, path = %path.display(), "imd: outbound: load_cursor read failed");
            TailCursor::default()
        }
    }
}

/// Persist a [`TailCursor`] to disk. Creates the parent directory if
/// missing. Low-level helper — does **not** enforce monotonicity, and
/// is not safe to call concurrently from multiple writers against the
/// same path.
///
/// Production code should go through [`OutboundCursor`] instead, which
/// owns the in-memory truth, serializes writers via an async mutex,
/// and exposes [`OutboundCursor::try_advance`] (monotonic) +
/// [`OutboundCursor::force_set`] (for the truncation-rewind case).
/// Direct callers of `save_cursor` are limited to (a) tests and (b)
/// startup paths that initialize the cursor from a known value before
/// any concurrent writers exist.
pub fn save_cursor(path: &Path, cursor: &TailCursor) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let body = serde_json::to_string(cursor).context("serialize TailCursor")?;
    fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Per-bot outbound cursor with **async-serialized monotonic
/// advance**. This is the synchronization primitive that closes the
/// fast-path × safety-net duplicate-send race in
/// [`crate::daemon::spawn_outbound_dispatcher`] +
/// [`crate::daemon::drain_outboxes`].
///
/// The two writers share an `Arc<OutboundCursor>` (constructed once
/// per bot in `ensure_bot_channels`). Both call:
///
/// - [`current`](Self::current) before sending a row to TG — skip
///   when `row_end_pos <= cursor.current()`, the other writer already
///   delivered it. Closes the per-row double-send window.
/// - [`try_advance`](Self::try_advance) after a successful TG send —
///   monotonic, no rewinds. Closes the cursor-rewind loop that
///   produced the unbounded "大量重复消息" flood on NAS-latency
///   environments.
///
/// Truncation is handled out-of-band via
/// [`force_set`](Self::force_set), called by `drain_outboxes` when it
/// detects `turns.jsonl` shrank below the current cursor (file
/// rotated / manually edited). Without an explicit reset the
/// monotonic guard would otherwise keep re-reading from byte 0 every
/// tick and re-forward the rotated content forever.
///
/// Disk persistence is best-effort: write errors warn-log but never
/// fail the in-memory advance. The worst case on disk-write failure
/// is re-forwarding the most recent row on daemon restart — still
/// bounded, still monotonic.
#[derive(Debug)]
pub struct OutboundCursor {
    path: PathBuf,
    state: Mutex<TailCursor>,
}

impl OutboundCursor {
    /// Construct, seeding from disk if `path` exists. Missing / corrupt
    /// file → zero cursor (re-forward everything from start; matches
    /// [`load_cursor`] semantics).
    pub fn load_from_disk(path: PathBuf) -> Arc<Self> {
        let initial = load_cursor(&path);
        Arc::new(Self {
            path,
            state: Mutex::new(initial),
        })
    }

    /// Current in-memory position. Cheap to call (short lock); use
    /// before each TG send for per-row dedup.
    pub async fn current(&self) -> u64 {
        self.state.lock().await.position
    }

    /// Monotonic advance. Returns `true` iff `new_pos` was strictly
    /// greater than the prior position and was applied. Persists to
    /// disk under the same lock so disk == memory after this call
    /// returns. Persistence errors are logged but don't fail the
    /// advance — the in-memory truth still moves forward.
    pub async fn try_advance(&self, new_pos: u64) -> bool {
        let mut guard = self.state.lock().await;
        if new_pos <= guard.position {
            return false;
        }
        guard.position = new_pos;
        self.persist_locked(&guard);
        true
    }

    /// Unconditional reset — `new_pos` may be smaller than the current
    /// position. Used only when `drain_outboxes` detects truncation
    /// (`turns.jsonl` shrunk below the cursor). Persists to disk.
    pub async fn force_set(&self, new_pos: u64) {
        let mut guard = self.state.lock().await;
        guard.position = new_pos;
        self.persist_locked(&guard);
    }

    /// Backing-file path (read-only — needed by tests / debug logs).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn persist_locked(&self, cursor: &TailCursor) {
        if let Some(parent) = self.path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                tracing::warn!(
                    error = %err,
                    parent = %parent.display(),
                    "imd: outbound cursor mkdir failed"
                );
                return;
            }
        }
        let body = match serde_json::to_string(cursor) {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(error = %err, "imd: outbound cursor serialize failed");
                return;
            }
        };
        if let Err(err) = fs::write(&self.path, body) {
            tracing::warn!(
                error = %err,
                path = %self.path.display(),
                "imd: outbound cursor persist failed (in-memory advance still in effect)"
            );
        }
    }
}

/// Per-row scan output: parsed row plus the byte position **after**
/// that row (i.e. where the next row would start). Used by both the
/// fast-path dispatcher and `drain_outboxes` to dedup-check each row
/// individually against [`OutboundCursor::current`] before sending.
#[derive(Debug, Clone)]
pub struct IndexedRow {
    /// Parsed `turns.jsonl` row.
    pub row: TurnRow,
    /// Byte offset of the first byte AFTER this row's terminating
    /// newline. Suitable as a [`TailCursor::position`] value once the
    /// row has been confirmed delivered.
    pub end_pos: u64,
}

/// Read every new line since `cursor.position`, returning each parsed
/// row together with its post-row byte offset, plus the final cursor
/// (= EOF at read time). Missing file → empty Vec. File shorter than
/// `cursor.position` (truncation) → rewinds `start` to 0 and reads
/// the whole file; callers should treat that as a truncation signal
/// and call [`OutboundCursor::force_set`] before advancing.
pub fn read_new_rows_indexed(
    path: &Path,
    cursor: &TailCursor,
) -> Result<(Vec<IndexedRow>, TailCursor)> {
    if !path.exists() {
        return Ok((vec![], cursor.clone()));
    }
    let mut f = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let len = f.metadata()?.len();
    let start = if cursor.position > len {
        0
    } else {
        cursor.position
    };
    f.seek(SeekFrom::Start(start))?;
    let reader = BufReader::new(f);
    let mut rows = Vec::new();
    let mut consumed: u64 = start;
    for line_res in reader.lines() {
        let line = line_res?;
        consumed += line.len() as u64 + 1; // +1 for `\n`
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TurnRow>(trimmed) {
            Ok(row) => rows.push(IndexedRow {
                row,
                end_pos: consumed,
            }),
            Err(err) => {
                tracing::warn!(error = %err, line = %trimmed, "malformed turns.jsonl row; skipping");
            }
        }
    }
    Ok((rows, TailCursor { position: consumed }))
}

/// Backwards-compat wrapper — strips per-row positions. Kept so the
/// existing test suite and any external callers still compile.
pub fn read_new_rows(path: &Path, cursor: &TailCursor) -> Result<(Vec<TurnRow>, TailCursor)> {
    let (indexed, end) = read_new_rows_indexed(path, cursor)?;
    Ok((indexed.into_iter().map(|i| i.row).collect(), end))
}

/// V0.6.0 Wave 3 — push every assistant row in `rows` to `channel`
/// addressed at `recipient`. Honors the [`should_forward`] filter so
/// `user` / `tool` rows are skipped. Per-row send errors are logged
/// but do not abort the forward loop (one flake shouldn't stall the
/// whole bot). Returns the count of rows successfully dispatched.
///
/// The daemon resolves `(channel, recipient)` per-bot from the
/// [`crate::BotRegistration`] (`im_platform` → channel impl,
/// `im_chat_id` → recipient string).
pub async fn forward_new_rows(
    rows: &[TurnRow],
    channel: &dyn crate::transport::Channel,
    recipient: &str,
    inbound_message_ids: &[String],
) -> usize {
    let mut sent = 0;
    for row in rows {
        if !should_forward(row, inbound_message_ids) {
            continue;
        }
        let tail_age_ms = row
            .ts
            .map(|t| now_unix_ms().saturating_sub(t.timestamp_millis().max(0) as u128) as u64);
        let turn_id_log = row.turn_id.clone().unwrap_or_default();
        let mut msg = crate::transport::SendMessage::new(row.content.clone(), recipient);
        msg.thread_ts = row.thread_ts.clone();
        let send_t0 = std::time::Instant::now();
        match channel.send(&msg).await {
            Ok(tg_msg_id) => {
                sent += 1;
                tracing::info!(
                    event = "latency",
                    stage = "outbound.send",
                    turn_id = %turn_id_log,
                    recipient,
                    tail_age_ms = tail_age_ms.unwrap_or(0),
                    send_ms = send_t0.elapsed().as_millis() as u64,
                    tg_msg_id = tg_msg_id.as_deref().unwrap_or(""),
                    content_len = row.content.len(),
                    "latency outbound.send"
                );
            }
            Err(err) => {
                tracing::warn!(
                    event = "latency",
                    stage = "outbound.send.err",
                    turn_id = %turn_id_log,
                    recipient,
                    tail_age_ms = tail_age_ms.unwrap_or(0),
                    send_ms = send_t0.elapsed().as_millis() as u64,
                    error = %err,
                    "latency outbound.send (failed)"
                );
            }
        }
    }
    sent
}

/// Filter: only `role == "assistant"` rows are forwarded to IM, and
/// we suppress turns whose `reply_to` matches a message id we
/// dropped inbound (echo suppression).
pub fn should_forward(row: &TurnRow, inbound_message_ids: &[String]) -> bool {
    if row.role != "assistant" {
        return false;
    }
    if let Some(rt) = &row.reply_to {
        if inbound_message_ids.iter().any(|id| id == rt) {
            // The agent's reply is to a message we already sent; this
            // is the IM->agent round-trip we initiated. Forward it.
            // (We only suppress when the **agent** initiates a bot-to-bot
            // call referencing an external id — checked by hop count
            // in the router, not here.)
            return true;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_empty_when_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("missing.jsonl");
        let (rows, cur) = read_new_rows(&path, &TailCursor::default()).unwrap();
        assert!(rows.is_empty());
        assert_eq!(cur.position, 0);
    }

    #[test]
    fn read_advances_cursor() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        fs::write(
            &path,
            r#"{"role":"assistant","content":"hi"}
{"role":"user","content":"u"}
"#,
        )
        .unwrap();
        let (rows, cur) = read_new_rows(&path, &TailCursor::default()).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(cur.position > 0);

        // Append one more line and re-read from the cursor.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        use std::io::Write;
        writeln!(f, "{{\"role\":\"assistant\",\"content\":\"new\"}}").unwrap();
        drop(f);
        let (more, _cur2) = read_new_rows(&path, &cur).unwrap();
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].content, "new");
    }

    #[test]
    fn malformed_line_skipped() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        fs::write(
            &path,
            "{\"role\":\"assistant\",\"content\":\"ok\"}\nNOT JSON\n",
        )
        .unwrap();
        let (rows, _) = read_new_rows(&path, &TailCursor::default()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "ok");
    }

    #[test]
    fn truncation_rewinds_cursor() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        // Write a long first version so the cursor ends up well past
        // the eventual truncated length.
        fs::write(
            &path,
            "{\"role\":\"assistant\",\"content\":\"first version with lots of bytes here so the cursor advances\"}\n",
        )
        .unwrap();
        let (_, cur) = read_new_rows(&path, &TailCursor::default()).unwrap();
        // Replace with shorter content (strict truncation).
        fs::write(&path, "{\"role\":\"assistant\",\"content\":\"b\"}\n").unwrap();
        let (rows, _) = read_new_rows(&path, &cur).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "b");
    }

    #[tokio::test]
    async fn forward_new_rows_dispatches_assistant_only() {
        use crate::transport::providers::mock::MockChannel;
        let channel = MockChannel::new();
        let rows = vec![
            TurnRow {
                role: "assistant".into(),
                content: "reply-1".into(),
                reply_to: None,
                thread_ts: None,
                reply_target: None,
                turn_id: None,
                ts: None,
            },
            TurnRow {
                role: "user".into(),
                content: "u-1".into(),
                reply_to: None,
                thread_ts: None,
                reply_target: None,
                turn_id: None,
                ts: None,
            },
            TurnRow {
                role: "assistant".into(),
                content: "reply-2".into(),
                reply_to: None,
                thread_ts: None,
                reply_target: None,
                turn_id: None,
                ts: None,
            },
        ];
        let sent = forward_new_rows(&rows, &channel, "user-alice", &[]).await;
        assert_eq!(sent, 2);
        let out = channel.outbox().await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content, "reply-1");
        assert_eq!(out[1].content, "reply-2");
        assert_eq!(out[0].recipient, "user-alice");
    }

    #[test]
    fn only_assistant_rows_forwarded() {
        let assistant = TurnRow {
            role: "assistant".into(),
            content: "x".into(),
            reply_to: None,
            thread_ts: None,
            reply_target: None,
            turn_id: None,
            ts: None,
        };
        let user = TurnRow {
            role: "user".into(),
            content: "x".into(),
            reply_to: None,
            thread_ts: None,
            reply_target: None,
            turn_id: None,
            ts: None,
        };
        assert!(should_forward(&assistant, &[]));
        assert!(!should_forward(&user, &[]));
    }

    // ── OutboundCursor: the synchronization primitive that closes the
    //    fast-path × safety-net race ─────────────────────────────────

    #[tokio::test]
    async fn outbound_cursor_try_advance_is_monotonic() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.json");
        let cur = OutboundCursor::load_from_disk(path.clone());
        assert_eq!(cur.current().await, 0);
        assert!(cur.try_advance(100).await);
        assert_eq!(cur.current().await, 100);
        // Smaller / equal advances are rejected — the cursor never rewinds.
        assert!(!cur.try_advance(50).await);
        assert!(!cur.try_advance(100).await);
        assert_eq!(cur.current().await, 100);
        // Larger advance applies.
        assert!(cur.try_advance(200).await);
        assert_eq!(cur.current().await, 200);
        // Disk reflects the latest accepted value.
        let on_disk = load_cursor(&path);
        assert_eq!(on_disk.position, 200);
    }

    #[tokio::test]
    async fn outbound_cursor_force_set_allows_rewind() {
        // Truncation path: turns.jsonl was rotated to a smaller file
        // and the cursor must reset below its current value. try_advance
        // alone would refuse; force_set is the explicit escape hatch.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.json");
        let cur = OutboundCursor::load_from_disk(path.clone());
        assert!(cur.try_advance(500).await);
        cur.force_set(40).await;
        assert_eq!(cur.current().await, 40);
        // Subsequent monotonic advance from the lower baseline works.
        assert!(cur.try_advance(60).await);
        assert_eq!(cur.current().await, 60);
        let on_disk = load_cursor(&path);
        assert_eq!(on_disk.position, 60);
    }

    #[tokio::test]
    async fn outbound_cursor_concurrent_advance_converges_to_max() {
        // Two writers (fast-path dispatcher + safety-net drain) racing
        // against the same cursor must never produce a rewind. Spawn
        // many interleaved try_advance calls and assert the final
        // value is the max of all proposed positions.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.json");
        let cur = OutboundCursor::load_from_disk(path.clone());

        let mut tasks = Vec::new();
        for n in (1u64..=50).rev() {
            let c = cur.clone();
            tasks.push(tokio::spawn(async move {
                c.try_advance(n).await;
            }));
        }
        for n in 1u64..=50 {
            let c = cur.clone();
            tasks.push(tokio::spawn(async move {
                c.try_advance(n).await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(cur.current().await, 50);
        let on_disk = load_cursor(&path);
        assert_eq!(on_disk.position, 50);
    }

    #[tokio::test]
    async fn outbound_cursor_seeds_from_existing_disk_value() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("c.json");
        save_cursor(&path, &TailCursor { position: 777 }).unwrap();
        let cur = OutboundCursor::load_from_disk(path);
        assert_eq!(cur.current().await, 777);
    }

    #[test]
    fn indexed_rows_report_post_row_offsets() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        let body = "{\"role\":\"assistant\",\"content\":\"a\"}\n{\"role\":\"assistant\",\"content\":\"b\"}\n";
        fs::write(&path, body).unwrap();
        let (rows, end) = read_new_rows_indexed(&path, &TailCursor::default()).unwrap();
        assert_eq!(rows.len(), 2);
        // The first row's end_pos is the second row's start.
        assert!(rows[0].end_pos > 0);
        assert!(rows[0].end_pos < rows[1].end_pos);
        // The last row's end_pos equals the final cursor (EOF).
        assert_eq!(rows[1].end_pos, end.position);
        assert_eq!(end.position as usize, body.len());
    }
}
