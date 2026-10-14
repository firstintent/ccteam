//! What a Claude transcript says about the session's turn GATING: the goal it
//! is working toward, and how often a Stop hook has refused to let its turn
//! end (GitHub #206). Read from the transcript jsonl because neither is on the
//! stream-json stdout stream nor has a control_request (both verified by live
//! probe) — reading the transcript is the blessed path, not terminal scraping.
//!
//! COST (issue #213): `thread_status` asks for this at every turn boundary and
//! per live session on every team-graph / status poll, and a long session's
//! transcript is tens of MB. The transcript is append-only, so the answer is a
//! fold over its lines: [`read`] keeps the fold per transcript ([`FoldCache`])
//! and feeds it only the lines appended since the last read. The first read of a file is
//! bounded to its trailing [`FIRST_READ_MAX_BYTES`].

use std::path::Path;
use std::sync::LazyLock;

use crate::execution::journal::{FoldCache, LineFold};
use crate::execution::transcript_tail::anthropic_project_dir;

/// The session's current `/goal` + recent Stop-hook refusals. Read together
/// because they are one answer: a goal whose condition is not met yet is
/// exactly what keeps refusing.
pub(super) struct TurnGating {
    pub(super) goal: Option<crate::GoalStatus>,
    /// Refusals inside [`STOP_HOOK_RECENT_WINDOW`].
    pub(super) stop_hook_blocks: u32,
}

/// How far back a Stop-hook refusal still counts as describing the session's
/// state NOW. The transcript has no turn-boundary marker to bound the scan
/// with, and an all-time count would keep accusing a session whose hook was
/// removed hours ago — a wall-clock window is the honest bound a reader can
/// also state out loud ("N times in the last hour").
const STOP_HOOK_RECENT_WINDOW: chrono::Duration = chrono::Duration::hours(1);

/// The marker Claude writes for a Stop hook that denied a stop. Verified
/// against a real transcript (2026-09-11, `excore`): one record per refusal,
/// `{"type":"user","isMeta":true,"message":{"role":"user","content":"Stop hook
/// feedback:…"}}`.
const STOP_HOOK_MARKER: &str = "Stop hook feedback";

/// The first read of a transcript scans at most this much of its tail, so a
/// huge transcript can't stall the statusline.
const FIRST_READ_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Transcripts whose fold is kept (see [`FoldCache`]) — a daemon outlives
/// thousands of sessions, only a handful of which are being looked at.
const MAX_CACHED_TRANSCRIPTS: usize = 256;

/// The fold of every transcript line read so far.
#[derive(Default)]
struct GatingScan {
    /// The LAST `goal_status` record: `None` = none seen, `Some(None)` = the
    /// goal was cleared (a cleared goal carries an empty condition).
    goal: Option<Option<crate::GoalStatus>>,
    /// Refusals in transcript order; `None` = undated. Pruned (see
    /// [`Self::recent_blocks`]) so it holds only what can still count.
    refusals: Vec<Option<chrono::DateTime<chrono::Utc>>>,
}

impl LineFold for GatingScan {
    fn ingest(&mut self, line: &[u8]) {
        self.ingest_line(&String::from_utf8_lossy(line));
    }
}

impl GatingScan {
    fn ingest_line(&mut self, line: &str) {
        // Cheap reject before any parse: a goal record and a refusal are
        // rare, and substring scanning is orders of magnitude cheaper than
        // `serde_json`.
        let goal = line.contains("goal_status");
        let refusal = line.contains(STOP_HOOK_MARKER);
        if !goal && !refusal {
            return;
        }
        // A half-flushed / non-JSON line is skipped, not fatal.
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        if goal {
            if let Some(att) = v
                .get("attachment")
                .filter(|att| att.get("type").and_then(|t| t.as_str()) == Some("goal_status"))
            {
                let condition = att
                    .get("condition")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                let met = att.get("met").and_then(|m| m.as_bool()).unwrap_or(false);
                // The LAST record decides on its own — nothing earlier can
                // revive a cleared goal. Claude's goal has no state axis
                // beyond `met` — its "why is this goal not advancing" fact is
                // `stop_hook_blocks`.
                self.goal = Some((!condition.trim().is_empty()).then_some(crate::GoalStatus {
                    condition,
                    met,
                    state: None,
                }));
            }
        }
        if refusal {
            // The marker has to be the message's OWN text, not a quotation of
            // it inside some other record (an assistant explaining the hook, a
            // tool result echoing the transcript).
            let text = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or_default();
            if text.trim_start().starts_with(STOP_HOOK_MARKER) {
                self.refusals.push(
                    v.get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|t| t.with_timezone(&chrono::Utc)),
                );
            }
        }
    }

    fn goal(&self) -> Option<crate::GoalStatus> {
        self.goal.clone().flatten()
    }

    /// The refusals in the last [`STOP_HOOK_RECENT_WINDOW`]: walking back from
    /// the newest, stop at the first refusal older than the window (the
    /// transcript is append-ordered, so everything before it is older still).
    /// A refusal with no readable timestamp is counted — it is a refusal that
    /// happened; only a DATED one can be ruled out of the window. What lies at
    /// or before that stop can never count again (the window only moves
    /// forward), so it is dropped.
    fn recent_blocks(&mut self, now: chrono::DateTime<chrono::Utc>) -> u32 {
        let floor = now - STOP_HOOK_RECENT_WINDOW;
        let stale = self
            .refusals
            .iter()
            .rposition(|at| at.is_some_and(|at| at < floor));
        if let Some(stale) = stale {
            self.refusals.drain(..=stale);
        }
        u32::try_from(self.refusals.len()).unwrap_or(u32::MAX)
    }
}

/// The session's current `/goal` + Stop-hook refusals, read from the Claude
/// transcript (cwd + the vendor uuid name the file). Claude writes the active
/// goal as a `{type:"attachment", attachment:{type: "goal_status", condition,
/// met}}` record on the user turn. `None` when the transcript is absent or
/// unreadable, which is what makes "cannot tell" distinguishable from
/// "nothing refused".
pub(super) fn read(cwd: &Path, uuid: &str) -> Option<TurnGating> {
    let path = anthropic_project_dir(cwd)?.join(format!("{uuid}.jsonl"));
    read_path(&path, chrono::Utc::now())
}

fn read_path(path: &Path, now: chrono::DateTime<chrono::Utc>) -> Option<TurnGating> {
    static CACHE: LazyLock<FoldCache<GatingScan>> =
        LazyLock::new(|| FoldCache::new(MAX_CACHED_TRANSCRIPTS, Some(FIRST_READ_MAX_BYTES)));
    CACHE.read(path, GatingScan::default, |scan| TurnGating {
        goal: scan.goal(),
        stop_hook_blocks: scan.recent_blocks(now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest_goal(body: &str) -> Option<crate::GoalStatus> {
        let mut scan = GatingScan::default();
        body.lines().for_each(|line| scan.ingest_line(line));
        scan.goal()
    }

    fn recent_blocks(body: &str, now: chrono::DateTime<chrono::Utc>) -> u32 {
        let mut scan = GatingScan::default();
        body.lines().for_each(|line| scan.ingest_line(line));
        scan.recent_blocks(now)
    }

    fn refusal(ts: &str) -> String {
        format!(
            r#"{{"type":"user","isMeta":true,"timestamp":"{ts}","message":{{"role":"user","content":"Stop hook feedback:\n[keep going]"}}}}"#
        )
    }

    fn at(ts: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn latest_goal_takes_the_last_record_and_honors_clear() {
        // No goal record → None.
        assert!(latest_goal(r#"{"type":"user"}"#).is_none());
        // One active goal.
        let one = r#"{"type":"assistant"}
{"type":"attachment","attachment":{"type":"goal_status","met":false,"condition":"ship payments"}}"#;
        let g = latest_goal(one).unwrap();
        assert_eq!(g.condition, "ship payments");
        assert!(!g.met);
        // The LAST record wins (met flips true).
        let two = format!(
            "{one}\n{}",
            r#"{"type":"attachment","attachment":{"type":"goal_status","met":true,"condition":"ship payments"}}"#
        );
        assert!(latest_goal(&two).unwrap().met);
        // A trailing clear (empty condition) → no active goal.
        let cleared = format!(
            "{one}\n{}",
            r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"condition":""}}"#
        );
        assert!(latest_goal(&cleared).is_none());
        // Half-flushed / non-JSON lines are skipped, not fatal.
        assert!(latest_goal("{partial\n{\"x\":1}").is_none());
    }

    /// GitHub #206 (P2) — a `/goal` Stop hook that refuses every stop is why a
    /// session can be silent for hours, and the transcript records each
    /// refusal. Count the recent ones so `/status` can say so; a refusal from
    /// a hook that is long gone must not keep accusing the session.
    #[test]
    fn stop_hook_refusals_are_counted_within_the_recent_window() {
        let now = at("2026-09-11T05:08:00Z");
        // Nothing to count.
        assert_eq!(recent_blocks("", now), 0);
        assert_eq!(recent_blocks(r#"{"type":"assistant"}"#, now), 0);
        // Two refusals inside the window, one hours old → the old one stops
        // the scan instead of inflating the count.
        let body = [
            refusal("2026-09-10T17:29:00Z"),
            r#"{"type":"assistant"}"#.to_string(),
            refusal("2026-09-11T04:24:00Z"),
            // The millisecond form a real transcript writes (probed
            // 2026-09-11: 123 refusals in `excore`'s live session).
            refusal("2026-09-11T05:07:30.470Z"),
        ]
        .join("\n");
        assert_eq!(recent_blocks(&body, now), 2);
        // A quotation of the marker is not a refusal — only a message whose
        // OWN text is the hook's feedback counts.
        let quoted = r#"{"type":"assistant","timestamp":"2026-09-11T05:07:40Z","message":{"role":"assistant","content":"the Stop hook feedback says to keep going"}}"#;
        assert_eq!(recent_blocks(quoted, now), 0);
        // An undated refusal happened; only a DATED one can be ruled out.
        let undated = r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Stop hook feedback:\nnot yet"}}"#;
        assert_eq!(recent_blocks(undated, now), 1);
    }

    /// Issue #213: a later read folds in only what was appended — the answer
    /// tracks the file (new goal, new refusals, the window sliding forward)
    /// exactly as a full re-read would, and a half-flushed line waits for its
    /// newline instead of being lost.
    #[test]
    fn a_later_read_folds_in_only_the_appended_lines() {
        use std::io::Write;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("t.jsonl");
        let goal = |cond: &str| {
            format!(
                r#"{{"type":"attachment","attachment":{{"type":"goal_status","met":false,"condition":"{cond}"}}}}"#
            )
        };
        std::fs::write(
            &path,
            format!("{}\n{}\n", goal("first"), refusal("2026-09-11T05:00:00Z")),
        )
        .unwrap();
        let g = read_path(&path, at("2026-09-11T05:10:00Z")).unwrap();
        assert_eq!(g.goal.unwrap().condition, "first");
        assert_eq!(g.stop_hook_blocks, 1);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let second = goal("second");
        let (head, tail) = second.split_at(20);
        write!(f, "{}\n{head}", refusal("2026-09-11T05:20:00Z")).unwrap();
        let g = read_path(&path, at("2026-09-11T05:30:00Z")).unwrap();
        assert_eq!(
            g.goal.unwrap().condition,
            "first",
            "half a line is not read"
        );
        assert_eq!(g.stop_hook_blocks, 2);

        writeln!(f, "{tail}").unwrap();
        // 06:10 — the 05:00 refusal has left the window.
        let g = read_path(&path, at("2026-09-11T06:10:00Z")).unwrap();
        assert_eq!(g.goal.unwrap().condition, "second");
        assert_eq!(g.stop_hook_blocks, 1);

        // The path now names a different (shorter) file → the fold restarts.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, format!("{}\n", goal(""))).unwrap();
        let g = read_path(&path, at("2026-09-11T06:10:00Z")).unwrap();
        assert!(g.goal.is_none());
        assert_eq!(g.stop_hook_blocks, 0);
    }
}
