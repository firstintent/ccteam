//! Recover what a `claude` body said while no daemon was reading it.
//!
//! After a daemon restart, a stream-json body that was mid-turn keeps working
//! on its own and exits when idle (measured: stdin EOF → the in-flight turn
//! and any self-continuation run to completion). Its stdout had no reader, so
//! ccteam's live path saw none of it — but claude's own transcript jsonl
//! (`~/.claude/projects/<encoded cwd>/<uuid>.jsonl`, one JSON record per
//! message, `timestamp` per record) did. This module reads the records newer
//! than the last moment ccteam observed the session and rebuilds the assistant
//! text + token usage the live pump would have mirrored.
//!
//! What counts as "the answer" mirrors the live translator
//! (`translate.rs`): one answer per vendor turn, taken from the message that
//! ENDS the turn (`stop_reason: end_turn` — the transcript's equivalent of a
//! `result` record; tool-use-only and mid-turn narration messages are
//! progress, not answers). Usage is summed over every assistant message after
//! the cut, which is what a turn's `result.usage` adds up to.
//!
//! Same sanctioned path as the terminal protocol's transcript track (read the
//! vendor's file; never a pane scrape, never a prompt). Anything that does not
//! parse is skipped, never guessed.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::RecoveredTurn;

/// Read `path` and fold every assistant record with `timestamp >
/// observed_until` into one [`RecoveredTurn`]. `None` when the file is
/// missing/unreadable or nothing newer than `observed_until` carries
/// assistant text. `last_observed_assistant` dedups the one race at the cut:
/// if the FIRST recovered text equals the last text ccteam already recorded,
/// it was observed after all and is skipped.
pub fn recover_after(
    path: &Path,
    observed_until: DateTime<Utc>,
    last_observed_assistant: Option<&str>,
) -> Option<RecoveredTurn> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut texts: Vec<String> = Vec::new();
    let mut ended_at: Option<DateTime<Utc>> = None;
    let mut input: u64 = 0;
    let mut output: u64 = 0;
    let mut cache_create: u64 = 0;
    let mut cache_read: u64 = 0;
    let mut usage_seen = false;

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A half-flushed tail (the body was still writing) is not an error:
        // skip the record, keep what parsed.
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(ts) = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
        else {
            continue;
        };
        if ts <= observed_until {
            continue;
        }
        let message = row.get("message");
        let text = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if let Some(usage) = message.and_then(|m| m.get("usage")) {
            usage_seen = true;
            input += usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            output += usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            cache_create += usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            cache_read += usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
        ended_at = Some(ended_at.map_or(ts, |prev: DateTime<Utc>| prev.max(ts)));
        // Only the message that ends a vendor turn carries the turn's answer
        // (the live path mirrors `result.result`, never mid-turn narration).
        if message
            .and_then(|m| m.get("stop_reason"))
            .and_then(Value::as_str)
            != Some("end_turn")
        {
            continue;
        }
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if texts.is_empty() && last_observed_assistant.is_some_and(|last| last.trim() == text) {
            // The cut landed between the transcript write and ccteam's own
            // mirror write of the same message: already recorded.
            continue;
        }
        texts.push(text.to_string());
    }

    if texts.is_empty() {
        return None;
    }
    let usage = if usage_seen {
        serde_json::json!({
            "input_tokens": input,
            "output_tokens": output,
            "cache_creation_input_tokens": cache_create,
            "cache_read_input_tokens": cache_read,
        })
    } else {
        Value::Null
    };
    Some(RecoveredTurn {
        assistant: texts.join("\n\n"),
        usage,
        ended_at: ended_at.unwrap_or(observed_until),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn assistant(ts: &str, blocks: Vec<Value>, usage: Option<Value>) -> String {
        assistant_stop(ts, blocks, usage, "end_turn")
    }

    fn assistant_stop(ts: &str, blocks: Vec<Value>, usage: Option<Value>, stop: &str) -> String {
        let mut message =
            serde_json::json!({"role": "assistant", "content": blocks, "stop_reason": stop});
        if let Some(u) = usage {
            message["usage"] = u;
        }
        serde_json::json!({
            "type": "assistant",
            "timestamp": ts,
            "message": message,
        })
        .to_string()
    }

    fn user(ts: &str, text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": ts,
            "message": {"role": "user", "content": text},
        })
        .to_string()
    }

    #[test]
    fn recovers_text_and_usage_newer_than_the_cut_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("u.jsonl");
        let lines = [
            user("2026-08-19T02:06:30.000Z", "do the task"),
            assistant(
                "2026-08-19T02:07:00.000Z",
                vec![serde_json::json!({"type":"text","text":"observed answer"})],
                Some(serde_json::json!({"input_tokens": 5, "output_tokens": 7})),
            ),
            // Tool-use only record: counts usage, contributes no text.
            assistant_stop(
                "2026-08-19T02:30:00.000Z",
                vec![serde_json::json!({"type":"tool_use","name":"Bash","input":{}})],
                Some(serde_json::json!({"input_tokens": 10, "output_tokens": 20,
                    "cache_read_input_tokens": 100})),
                "tool_use",
            ),
            // Mid-turn narration (text + a tool call in one message, stop =
            // tool_use): progress, not the answer — counts usage only.
            assistant_stop(
                "2026-08-19T02:30:30.000Z",
                vec![
                    serde_json::json!({"type":"text","text":"Let me check the file first."}),
                    serde_json::json!({"type":"tool_use","name":"Read","input":{}}),
                ],
                Some(serde_json::json!({"input_tokens": 2, "output_tokens": 3})),
                "tool_use",
            ),
            assistant(
                "2026-08-19T02:31:00.000Z",
                vec![
                    serde_json::json!({"type":"thinking","thinking":"…"}),
                    serde_json::json!({"type":"text","text":"first unobserved"}),
                ],
                Some(serde_json::json!({"input_tokens": 1, "output_tokens": 2})),
            ),
            // Subagent sidechain rows never count.
            serde_json::json!({"type":"assistant","isSidechain":true,
                "timestamp":"2026-08-19T02:32:00.000Z",
                "message":{"content":[{"type":"text","text":"sidechain"}]}})
            .to_string(),
            assistant(
                "2026-08-19T02:40:00.000Z",
                vec![serde_json::json!({"type":"text","text":"DONE"})],
                Some(serde_json::json!({"input_tokens": 3, "output_tokens": 4})),
            ),
            // A half-flushed tail is skipped, not fatal.
            "{\"type\":\"assistant\",\"timesta".to_string(),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let cut = Utc.with_ymd_and_hms(2026, 8, 19, 2, 9, 0).unwrap();
        let recovered = recover_after(&path, cut, Some("observed answer")).expect("recovered");
        assert_eq!(recovered.assistant, "first unobserved\n\nDONE");
        assert_eq!(recovered.usage["input_tokens"], 16);
        assert_eq!(recovered.usage["output_tokens"], 29);
        assert_eq!(recovered.usage["cache_read_input_tokens"], 100);
        assert_eq!(recovered.usage["cache_creation_input_tokens"], 0);
        assert_eq!(
            recovered.ended_at,
            Utc.with_ymd_and_hms(2026, 8, 19, 2, 40, 0).unwrap()
        );
    }

    #[test]
    fn dedups_the_last_observed_answer_at_the_cut() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("u.jsonl");
        // The mirror row was written at 02:08:59.900 but the transcript stamped
        // the same message 02:09:00.050 — newer than the cut, yet observed.
        let lines = [
            assistant(
                "2026-08-19T02:09:00.050Z",
                vec![serde_json::json!({"type":"text","text":"same answer"})],
                None,
            ),
            assistant(
                "2026-08-19T02:12:00.000Z",
                vec![serde_json::json!({"type":"text","text":"later"})],
                None,
            ),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let cut = Utc.with_ymd_and_hms(2026, 8, 19, 2, 9, 0).unwrap();
        let recovered = recover_after(&path, cut, Some("same answer")).unwrap();
        assert_eq!(recovered.assistant, "later");
        assert_eq!(
            recovered.usage,
            Value::Null,
            "no usage blocks → Null, never 0"
        );
    }

    #[test]
    fn nothing_newer_or_missing_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("u.jsonl");
        assert!(recover_after(&path, Utc::now(), None).is_none());
        std::fs::write(
            &path,
            assistant(
                "2026-08-19T02:00:00.000Z",
                vec![serde_json::json!({"type":"text","text":"old"})],
                None,
            ),
        )
        .unwrap();
        let cut = Utc.with_ymd_and_hms(2026, 8, 19, 2, 9, 0).unwrap();
        assert!(recover_after(&path, cut, None).is_none());
    }
}
