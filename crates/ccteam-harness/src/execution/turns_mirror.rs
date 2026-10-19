//! V0.6.0 F108 / F118 — ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`.
//!
//! The Anthropic transcript at
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` is the wire SoT
//! for one *session*; it disappears when the user runs `/clear` or when
//! Claude rotates session-ids on compaction. ccteam owns a
//! parallel mirror — `<project>/.ccteam/chat/<bot>/turns.jsonl` — that:
//!
//! - never gets rotated by Claude,
//! - records exactly the (user / assistant / usage / tool-call) summary
//!   the F108 dual-track event stream emitted, and
//! - is the input to [`crate::execution::session_recovery`]'s F118
//!   `rebuild_from_turns_jsonl` flow.
//!
//! Schema (one [`TurnRecord`] per line):
//!
//! ```jsonl
//! {"turn_id":"...","ts":"2026-05-17T...","vendor":"claude","role":"<bot>",
//!  "user":"...","assistant":"...","usage":{...},"tool_calls":[...]}
//! ```
//!
//! Append is atomic (POSIX `O_APPEND` + one `write_all`; record bodies
//! fit comfortably under PIPE_BUF). Reads tolerate half-flushed tails
//! (lines that fail to deserialize are skipped).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::journal;
use super::turn_status::TurnStatus;

/// A browser-safe reference to one project-scoped outbound attachment.
///
/// The file bytes stay under `<project>/.ccteam/uploads/`; transcript and
/// web wire shapes carry only this metadata. `id` is the stored basename and
/// is resolved by the project-ACL'd upload read route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub id: String,
    pub name: String,
    pub kind: AttachmentRefKind,
    pub size: u64,
}

/// Display class for an outbound attachment. This is deliberately separate
/// from HTTP inline-safety: the download route independently allowlists safe
/// raster extensions and serves every other type as an attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRefKind {
    Image,
    File,
}

/// One conversation turn the F108 dual-track stream observed. Optional
/// fields default to empty / null so half-completed turns (e.g. the
/// assistant errored before producing text) still round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub turn_id: String,
    /// The ADAPTER's execution-turn id this row belongs to, when the channel
    /// has one. `turn_id` above is ccteam's own per-row key; a vendor turn can
    /// span several rows (interim narration) and a delegation request is bound
    /// to the EXECUTION turn, so a restart reconcile needs the execution id on
    /// disk to rebind an outstanding request to the answer it produced
    /// (issue #201). Absent on rows written by channels that expose none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_turn_id: Option<String>,
    /// The execution turn this one CONTINUES: the vendor opened this turn by
    /// itself to carry on that turn's work (claude answering its own
    /// `<task-notification>`, or re-running a line ccteam injected mid-turn).
    ///
    /// The chain edge, on DISK, in the transcript. A restart used to rebuild a
    /// delegation chain from the request's binding alone, so a rebind that had
    /// not reached `delegation.json` yet made the reconcile hand the parent the
    /// intermediate turn and ignore the one that actually answered — the very
    /// mis-attribution the binding exists to prevent (GitHub #198/#199). The
    /// edge is written with the turn itself, in the same durable append, so the
    /// chain can be walked from the transcript no matter what the request
    /// record managed to persist. Absent on every turn ccteam asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues_exec_turn: Option<String>,
    pub ts: DateTime<Utc>,
    /// Vendor scalar (`"claude"` / `"codex"`). Plain string here so the
    /// jsonl is hand-greppable; the orchestrator never mixes vendors in
    /// one turns.jsonl file in V0.6.0.
    pub vendor: String,
    /// Bot role name (also `workflow.yaml chat.bot_name`).
    pub role: String,
    /// User-side prompt text. Empty when the turn was driven by a
    /// slash directive (e.g. `/compact`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// Assistant-side reply text (concatenation of every `text` block
    /// emitted on this turn).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub assistant: String,
    /// The turn's conclusion — the text after the assistant's last tool
    /// call (Claude's `result.result`), when the vendor marks that boundary
    /// AND it is shorter than `assistant`. A bounded excerpt of the turn
    /// (completion notification, inline result, `agent_read` row) shows this
    /// instead of the head of the narration (issue #196). Absent = the row's
    /// `assistant` is its own conclusion or the vendor drew no boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    /// Token / cost accounting. Free-form `Value` so the wire shape
    /// stays aligned with whatever `UnifiedTokenUsage` evolves to.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub usage: Value,
    /// Post-turn status snapshot, absent on legacy rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TurnStatus>,
    /// Brief summaries of any tool calls the assistant emitted this
    /// turn. Keeps the mirror useful for F118 recovery without bloating
    /// the file with full tool-input bodies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallSummary>,
    /// Project-scoped outbound attachment references. Never contains file
    /// bytes, base64, daemon paths, or browser-provided URLs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentRef>,
    /// Terminal outcome when this row represents a failed vendor turn.
    /// Successful/interim rows omit the field so existing JSONL stays compact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Stable canonical/vendor error kind (`server_overloaded`, `transport`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// Human-readable terminal error returned by the vendor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TurnRecord {
    /// Whether this row is the terminal failure boundary for its vendor turn.
    pub fn failed(&self) -> bool {
        self.outcome.as_deref() == Some("failed")
    }
}

/// Compact tool-call entry: `name`, optional file-path / arg excerpt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallSummary {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Default subdirectory under `<project>/.ccteam/chat/<sid>/`.
///
/// v0.8.8 F1 — 这一层的键由 bot_role 改为 **sid**(`s<N>`):同
/// `(project, role)` 可有多个独立会话,turns / cursor / marker 一律按
/// sid 隔离,所以两个同 role 会话的历史不互相污染。函数签名(`&str`)不变
/// —— 调用方负责传 sid 而非 role。
const CHAT_BASE: &str = ".ccteam/chat";

/// Resolve `<project>/.ccteam/chat/<sid>/turns.jsonl`.
pub fn turns_jsonl_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir.join(CHAT_BASE).join(sid).join("turns.jsonl")
}

/// Resolve `<project>/.ccteam/chat/<sid>/`. Created by [`ensure_dir`].
pub fn chat_dir(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir.join(CHAT_BASE).join(sid)
}

/// `mkdir -p <project>/.ccteam/chat/<sid>/`. Idempotent.
pub fn ensure_dir(project_dir: &Path, sid: &str) -> Result<()> {
    let p = chat_dir(project_dir, sid);
    fs::create_dir_all(&p).with_context(|| format!("create {}", p.display()))?;
    Ok(())
}

/// Append `record` as one JSONL line. Creates parent dir + file when
/// missing. Returns the absolute path written for caller logging.
pub fn append_turn(project_dir: &Path, sid: &str, record: &TurnRecord) -> Result<PathBuf> {
    ensure_dir(project_dir, sid)?;
    let path = turns_jsonl_path(project_dir, sid);
    let line = serde_json::to_string(record)? + "\n";
    let mut f = crate::execution::fs_atomic::open_jsonl_append(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(path)
}

/// Read every parseable record from the session's turns.jsonl. Returns an
/// empty Vec when the file is absent (V0.6.0 F108 first-turn case).
pub fn read_all_turns(project_dir: &Path, sid: &str) -> Result<Vec<TurnRecord>> {
    let path = turns_jsonl_path(project_dir, sid);
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Skip half-flushed / older-shape / torn rows defensively — F118 recovery
    // has to work on whatever survived, so damage must cost one LINE, not the
    // whole transcript ([`super::fs_atomic::read_jsonl`]).
    super::fs_atomic::read_jsonl(&path)
}

/// Counts the gateway asks of a session's turns.jsonl at every turn boundary
/// (pump start, meta refresh, final answer, `agent` wait settle). Kept as an
/// incremental fold ([`journal::FoldCache`]) so each ask reads only the rows
/// appended since the last one, not the whole session history (issue #213).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnsDigest {
    /// Rows with a non-empty `user` — the session's user turns.
    pub user_turns: u64,
    /// Rows carrying a `status` — one per completed execution turn (only the
    /// final answer of a turn carries it).
    pub status_turns: u64,
    /// Highest `N` among `turn_id`s shaped `<sid>-<N>` — where a new pump
    /// resumes numbering. COUNTING rows was a proxy that held only while
    /// every such row had text: a closing row (empty `assistant`, #209) is
    /// not counted, so the next pump re-issued its id — and a turn id already
    /// in `notified_turns` silently swallows the completion that reuses it.
    pub max_answer_seq: u64,
    /// `turn_id` of the newest row.
    pub last_turn_id: Option<String>,
}

struct DigestFold {
    prefix: String,
    digest: TurnsDigest,
}

impl journal::LineFold for DigestFold {
    fn ingest(&mut self, line: &[u8]) {
        // Same tolerance as `read_all_turns`: an unparseable row is skipped.
        let Ok(turn) = serde_json::from_slice::<TurnRecord>(line) else {
            return;
        };
        let d = &mut self.digest;
        if !turn.user.is_empty() {
            d.user_turns += 1;
        }
        if turn.status.is_some() {
            d.status_turns += 1;
        }
        if let Some(seq) = turn
            .turn_id
            .strip_prefix(&self.prefix)
            .and_then(|n| n.parse::<u64>().ok())
        {
            d.max_answer_seq = d.max_answer_seq.max(seq);
        }
        d.last_turn_id = Some(turn.turn_id);
    }
}

/// The session's [`TurnsDigest`] — empty when it has no turns.jsonl yet,
/// `None` when the file exists but cannot be read (callers keep what they
/// knew rather than reporting zero turns).
pub fn digest(project_dir: &Path, sid: &str) -> Option<TurnsDigest> {
    /// Sessions whose digest is kept: live ones are asked at every turn; a
    /// dropped digest is just rebuilt from the file.
    const MAX_CACHED_DIGESTS: usize = 512;
    static CACHE: std::sync::LazyLock<journal::FoldCache<DigestFold>> =
        std::sync::LazyLock::new(|| journal::FoldCache::new(MAX_CACHED_DIGESTS, None));
    let path = turns_jsonl_path(project_dir, sid);
    let digest = CACHE.read(
        &path,
        || DigestFold {
            prefix: format!("{sid}-"),
            digest: TurnsDigest::default(),
        },
        |fold| fold.digest.clone(),
    );
    match digest {
        Some(digest) => Some(digest),
        None if !path.exists() => Some(TurnsDigest::default()),
        None => None,
    }
}

/// Return the last `n` parseable turns, in chronological order. F118
/// `rebuild_from_turns_jsonl` uses this to bound the conversation
/// history it injects into a fresh tmux session.
pub fn last_n_turns(project_dir: &Path, sid: &str, n: usize) -> Result<Vec<TurnRecord>> {
    let path = turns_jsonl_path(project_dir, sid);
    Ok(journal::tail_filter_map(&path, n, None, |line| {
        serde_json::from_slice::<TurnRecord>(line).ok()
    })?
    .events)
}

/// The most one field of a [`StepRecord`] keeps (bytes, cut at a char
/// boundary). A tool can return megabytes; the inspector needs enough to read,
/// and an unbounded line would make every lookup scan pay for it.
pub const STEP_FIELD_MAX_BYTES: usize = 256 * 1024;

/// One finished step of a turn — a tool call with its input and output, or a
/// reasoning block — kept in `<project>/.ccteam/chat/<sid>/steps.jsonl` for
/// the web step inspector. Observability only: nothing replays or bills from
/// it, and losing it loses no conversation (that is `turns.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    /// Unique across the session's life — the live activity frame carries it,
    /// and it is what the inspector asks for.
    pub step_id: String,
    pub ts: DateTime<Utc>,
    /// `tool_call` | `command_exec` | `file_change` | `web_search` | `thinking`.
    pub kind: String,
    /// Tool / category name; empty for thinking.
    #[serde(default)]
    pub name: String,
    /// The call's input, pretty JSON or the bare command / query / path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// What the step returned — a tool's output, or the reasoning text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Whether a field was cut at [`STEP_FIELD_MAX_BYTES`].
    #[serde(default)]
    pub truncated: bool,
}

impl StepRecord {
    /// Cap every field at [`STEP_FIELD_MAX_BYTES`], flagging the cut.
    pub fn bounded(mut self) -> Self {
        for field in [&mut self.input, &mut self.output].into_iter().flatten() {
            if field.len() > STEP_FIELD_MAX_BYTES {
                let mut cut = STEP_FIELD_MAX_BYTES;
                while !field.is_char_boundary(cut) {
                    cut -= 1;
                }
                field.truncate(cut);
                self.truncated = true;
            }
        }
        self
    }
}

/// Resolve `<project>/.ccteam/chat/<sid>/steps.jsonl`.
pub fn steps_jsonl_path(project_dir: &Path, sid: &str) -> PathBuf {
    chat_dir(project_dir, sid).join("steps.jsonl")
}

/// Append one step (bounded first). The session's event pump is the only
/// writer, so a line is never interleaved with another.
pub fn append_step(project_dir: &Path, sid: &str, record: StepRecord) -> Result<()> {
    ensure_dir(project_dir, sid)?;
    let path = steps_jsonl_path(project_dir, sid);
    let line = serde_json::to_string(&record.bounded())? + "\n";
    let mut f = crate::execution::fs_atomic::open_jsonl_append(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", path.display()))?;
    Ok(())
}

/// The newest record for `step_id`, or `None` when the step never finished
/// (or the file is absent). Lines are matched as bytes before being parsed,
/// so a lookup parses one record, not the session's whole history; a torn or
/// otherwise unreadable line is skipped, never an error for every lookup.
pub fn find_step(project_dir: &Path, sid: &str, step_id: &str) -> Result<Option<StepRecord>> {
    use std::io::BufRead;
    let path = steps_jsonl_path(project_dir, sid);
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("open {}", path.display())),
    };
    let needle = format!("\"step_id\":{}", serde_json::to_string(step_id)?);
    let needle = needle.as_bytes();
    let mut found = None;
    for line in std::io::BufReader::new(file).split(b'\n') {
        let line = line.with_context(|| format!("read {}", path.display()))?;
        if !line.windows(needle.len()).any(|w| w == needle) {
            continue;
        }
        if let Ok(record) = serde_json::from_slice::<StepRecord>(&line) {
            if record.step_id == step_id {
                found = Some(record);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mk_turn(id: &str, role: &str, user: &str, assistant: &str) -> TurnRecord {
        TurnRecord {
            exec_turn_id: None,
            turn_id: id.to_string(),
            ts: Utc::now(),
            vendor: "claude".to_string(),
            role: role.to_string(),
            user: user.to_string(),
            assistant: assistant.to_string(),
            usage: Value::Null,
            status: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            outcome: None,
            error_kind: None,
            error: None,
            conclusion: None,
            continues_exec_turn: None,
        }
    }

    #[test]
    fn path_helpers_produce_expected_layout() {
        let p = Path::new("/p");
        assert_eq!(
            turns_jsonl_path(p, "alice"),
            PathBuf::from("/p/.ccteam/chat/alice/turns.jsonl")
        );
        assert_eq!(chat_dir(p, "alice"), PathBuf::from("/p/.ccteam/chat/alice"));
    }

    #[test]
    fn append_and_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let t1 = mk_turn("t1", "alice", "hi", "hello");
        append_turn(tmp.path(), "alice", &t1).unwrap();
        let t2 = mk_turn("t2", "alice", "again", "yo");
        append_turn(tmp.path(), "alice", &t2).unwrap();

        let read = read_all_turns(tmp.path(), "alice").unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].turn_id, "t1");
        assert_eq!(read[0].assistant, "hello");
        assert_eq!(read[1].user, "again");
    }

    #[test]
    fn last_n_returns_chronological_tail() {
        let tmp = TempDir::new().unwrap();
        for i in 0..5 {
            let r = mk_turn(&format!("t{i}"), "bob", "u", "a");
            append_turn(tmp.path(), "bob", &r).unwrap();
        }
        let tail = last_n_turns(tmp.path(), "bob", 2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].turn_id, "t3");
        assert_eq!(tail[1].turn_id, "t4");
    }

    #[test]
    fn last_n_skips_corrupt_rows_while_reading_backwards() {
        let tmp = TempDir::new().unwrap();
        for i in 0..2 {
            append_turn(
                tmp.path(),
                "bob",
                &mk_turn(&format!("t{i}"), "bob", "u", "a"),
            )
            .unwrap();
        }
        let path = turns_jsonl_path(tmp.path(), "bob");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "not-json").unwrap();
        drop(file);
        for i in 2..4 {
            append_turn(
                tmp.path(),
                "bob",
                &mk_turn(&format!("t{i}"), "bob", "u", "a"),
            )
            .unwrap();
        }
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        write!(file, "{{torn").unwrap();

        let tail = last_n_turns(tmp.path(), "bob", 2).unwrap();
        assert_eq!(
            tail.iter()
                .map(|turn| turn.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["t2", "t3"]
        );
    }

    /// Issue #213: the incremental digest answers exactly what a full re-read
    /// would, across appends between asks, a corrupt row, and a torn tail.
    #[test]
    fn digest_tracks_appends_like_a_full_reread() {
        let tmp = TempDir::new().unwrap();
        let full = |dir: &Path| {
            let turns = read_all_turns(dir, "s9").unwrap();
            TurnsDigest {
                user_turns: turns.iter().filter(|t| !t.user.is_empty()).count() as u64,
                status_turns: turns.iter().filter(|t| t.status.is_some()).count() as u64,
                max_answer_seq: turns
                    .iter()
                    .filter_map(|t| t.turn_id.strip_prefix("s9-")?.parse().ok())
                    .max()
                    .unwrap_or(0),
                last_turn_id: turns.last().map(|t| t.turn_id.clone()),
            }
        };
        assert_eq!(digest(tmp.path(), "s9").unwrap(), TurnsDigest::default());
        append_turn(tmp.path(), "s9", &mk_turn("s9-3", "r", "hi", "a")).unwrap();
        assert_eq!(digest(tmp.path(), "s9").unwrap(), full(tmp.path()));
        let mut done = mk_turn("s9-1", "r", "", "b");
        done.status = Some(TurnStatus {
            model: None,
            context: None,
            turn: 1,
            cost_usd: None,
            tokens_total: None,
        });
        append_turn(tmp.path(), "s9", &done).unwrap();
        let path = turns_jsonl_path(tmp.path(), "s9");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "not-json").unwrap();
        write!(file, "{{\"turn_id\":\"torn").unwrap();
        drop(file);
        let d = digest(tmp.path(), "s9").unwrap();
        assert_eq!(d, full(tmp.path()));
        assert_eq!((d.user_turns, d.status_turns, d.max_answer_seq), (1, 1, 3));
        append_turn(tmp.path(), "s9", &mk_turn("x-7", "r", "again", "c")).unwrap();
        let d = digest(tmp.path(), "s9").unwrap();
        assert_eq!(d, full(tmp.path()));
        assert_eq!(d.last_turn_id.as_deref(), Some("x-7"));
        // Present but unreadable is not "no turns".
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |m| fs::set_permissions(&path, fs::Permissions::from_mode(m)).unwrap();
            mode(0o000);
            let unreadable = fs::File::open(&path).is_err(); // root reads anyway
            if unreadable {
                assert_eq!(digest(tmp.path(), "s9"), None);
            }
            mode(0o644);
        }
    }

    /// Issue #210: after an ENOSPC-torn tail the next appended turn lands on
    /// its own line instead of being glued to the fragment (both lost).
    #[test]
    fn append_after_a_torn_tail_keeps_the_new_turn() {
        let tmp = TempDir::new().unwrap();
        append_turn(tmp.path(), "eve", &mk_turn("t0", "eve", "u", "a")).unwrap();
        let path = turns_jsonl_path(tmp.path(), "eve");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"turn_id\":\"torn").unwrap();
        drop(file);
        append_turn(tmp.path(), "eve", &mk_turn("t1", "eve", "u", "a")).unwrap();
        append_turn(tmp.path(), "eve", &mk_turn("t2", "eve", "u", "a")).unwrap();

        let ids: Vec<String> = read_all_turns(tmp.path(), "eve")
            .unwrap()
            .into_iter()
            .map(|turn| turn.turn_id)
            .collect();
        assert_eq!(ids, vec!["t0", "t1", "t2"]);
    }

    #[test]
    fn last_n_zero_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let r = mk_turn("t0", "x", "u", "a");
        append_turn(tmp.path(), "x", &r).unwrap();
        let tail = last_n_turns(tmp.path(), "x", 0).unwrap();
        assert!(tail.is_empty());
    }

    #[test]
    fn read_all_missing_file_is_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let out = read_all_turns(tmp.path(), "ghost").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_skips_corrupt_lines() {
        let tmp = TempDir::new().unwrap();
        ensure_dir(tmp.path(), "carol").unwrap();
        let path = turns_jsonl_path(tmp.path(), "carol");
        let good = serde_json::to_string(&mk_turn("g", "carol", "u", "a")).unwrap();
        fs::write(&path, format!("{good}\n{{not-json\n{good}\n   \n")).unwrap();
        let read = read_all_turns(tmp.path(), "carol").unwrap();
        assert_eq!(read.len(), 2);
    }

    #[test]
    fn failure_outcome_survives_turn_record_round_trip() {
        let raw = serde_json::json!({
            "turn_id": "t-failed",
            "ts": "2026-07-28T08:11:01Z",
            "vendor": "codex",
            "role": "reviewer",
            "assistant": "Selected model is at capacity.",
            "outcome": "failed",
            "error_kind": "server_overloaded",
            "error": "Selected model is at capacity."
        });
        let record: TurnRecord = serde_json::from_value(raw).unwrap();
        let encoded = serde_json::to_value(record).unwrap();
        assert_eq!(encoded["outcome"], "failed");
        assert_eq!(encoded["error_kind"], "server_overloaded");
        assert_eq!(encoded["error"], "Selected model is at capacity.");
    }

    #[test]
    fn attachment_references_survive_turn_record_round_trip() {
        let mut record = mk_turn("t-file", "reviewer", "", "chart attached");
        record.attachments.push(AttachmentRef {
            id: "1780000000000-chart.png".into(),
            name: "chart.png".into(),
            kind: AttachmentRefKind::Image,
            size: 12_345,
        });

        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("base64"));
        let decoded: TurnRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, record);
    }

    #[test]
    fn pre_outcome_turn_record_remains_compatible() {
        let raw = serde_json::json!({
            "turn_id": "t-old",
            "ts": "2026-07-28T08:00:00Z",
            "vendor": "claude",
            "role": "",
            "assistant": "done"
        });
        let record: TurnRecord = serde_json::from_value(raw).unwrap();
        assert!(record.attachments.is_empty());
        let encoded = serde_json::to_value(record).unwrap();
        assert!(encoded.get("attachments").is_none());
        assert!(encoded.get("outcome").is_none());
        assert!(encoded.get("error_kind").is_none());
        assert!(encoded.get("error").is_none());
    }

    fn step(id: &str, output: &str) -> StepRecord {
        StepRecord {
            step_id: id.to_string(),
            ts: Utc::now(),
            kind: "tool_call".to_string(),
            name: "Bash".to_string(),
            input: Some("{\"command\":\"ls\"}".to_string()),
            output: Some(output.to_string()),
            truncated: false,
        }
    }

    #[test]
    fn a_step_is_found_by_id_and_the_newest_record_wins() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_step(dir.path(), "s1", "p-1-a").unwrap(), None);
        append_step(dir.path(), "s1", step("p-1-a", "first")).unwrap();
        append_step(dir.path(), "s1", step("p-1-ab", "prefix sibling")).unwrap();
        append_step(dir.path(), "s1", step("p-1-a", "second")).unwrap();
        let got = find_step(dir.path(), "s1", "p-1-a").unwrap().unwrap();
        assert_eq!(got.output.as_deref(), Some("second"));
        assert_eq!(
            find_step(dir.path(), "s1", "p-1-ab")
                .unwrap()
                .unwrap()
                .output
                .as_deref(),
            Some("prefix sibling")
        );
        assert_eq!(find_step(dir.path(), "s2", "p-1-a").unwrap(), None);
    }

    #[test]
    fn a_torn_line_with_broken_utf8_does_not_fail_every_lookup() {
        let dir = tempfile::tempdir().unwrap();
        append_step(dir.path(), "s1", step("p-1-a", "kept")).unwrap();
        let path = steps_jsonl_path(dir.path(), "s1");
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"step_id\":\"p-1-b\",\"output\":\"\xe4\xb8")
            .unwrap();
        drop(f);
        let got = find_step(dir.path(), "s1", "p-1-a").unwrap().unwrap();
        assert_eq!(got.output.as_deref(), Some("kept"));
        assert_eq!(find_step(dir.path(), "s1", "p-1-b").unwrap(), None);
    }

    #[test]
    fn an_oversized_field_is_cut_at_a_char_boundary_and_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let big = "é".repeat(STEP_FIELD_MAX_BYTES);
        append_step(dir.path(), "s1", step("p-1-big", &big)).unwrap();
        let got = find_step(dir.path(), "s1", "p-1-big").unwrap().unwrap();
        assert!(got.truncated);
        assert!(got.output.unwrap().len() <= STEP_FIELD_MAX_BYTES);
    }
}
