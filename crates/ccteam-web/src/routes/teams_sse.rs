//! V0.5.0 F96 — `GET /api/v1/teams/<name>/events` SSE channel.
//!
//! Forwards 5 team event variants from F95's
//! `~/.ccteam/teams-progress.jsonl` (or whatever
//! `state.teams_progress_path` points at — tests override). Each
//! progress line that parses as JSON with a matching
//! `team_name == <name>` is shipped as an `event: progress` SSE
//! frame.
//!
//! Wire format mirrors `routes::sse` for consistency. Polling
//! cadence: 1s (small files; the file is append-only). Tests
//! exercising this with high message counts will get coverage from
//! integration tests in `api_v1_teams_test.rs`.
//!
//! No broadcast bus — the file is read fresh on each connection
//! (initial dump + tail-from-tail). Multiple concurrent SSE
//! subscribers each open the file independently; for V0.5.0 traffic
//! volumes (a few events per minute) this is fine. A future
//! optimisation can hoist into a `tokio::sync::broadcast` like
//! `routes::sse` if traffic grows.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use serde_json::Value;

use crate::state::AppState;

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `GET /api/v1/teams/{name}/events`
///
/// OpenAPI note: a **Server-Sent Events** stream (`text/event-stream`),
/// which OpenAPI cannot fully model. Each `event: progress` frame's
/// `data` is a JSON team-progress line filtered to `team_name == {name}`.
#[utoipa::path(
    get,
    path = "/api/v1/teams/{name}/events",
    tag = "teams",
    params(("name" = String, Path, description = "Team name")),
    responses(
        (status = 200, description = "SSE stream (text/event-stream) of team-progress events for this team.", content_type = "text/event-stream"),
    ),
)]
pub(crate) async fn handle_team_events(
    State(app): State<AppState>,
    Path(name): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let path = (*app.teams_progress_path).clone();
    let stream = futures::stream::unfold(StreamState::start(path, name), |mut st| async move {
        // Loop until we have at least one event to ship (or a
        // keep-alive opportunity passes).
        loop {
            if let Some(ev) = st.next_event() {
                return Some((Ok::<_, Infallible>(ev), st));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default().interval(KEEPALIVE_INTERVAL))
}

struct StreamState {
    path: std::path::PathBuf,
    team_name: String,
    /// Byte offset we've already shipped up to. Initialised to `0` so
    /// callers see the full file on connect (acts as a snapshot dump
    /// — the SPA reducer dedupes by `ts` if needed).
    cursor: u64,
    /// Buffered events parsed from the last read but not yet emitted.
    pending: std::collections::VecDeque<Event>,
}

impl StreamState {
    fn start(path: std::path::PathBuf, team_name: String) -> Self {
        Self {
            path,
            team_name,
            cursor: 0,
            pending: Default::default(),
        }
    }

    fn next_event(&mut self) -> Option<Event> {
        if let Some(ev) = self.pending.pop_front() {
            return Some(ev);
        }
        self.refresh();
        self.pending.pop_front()
    }

    fn refresh(&mut self) {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return;
        };
        let size = metadata.len();
        if size <= self.cursor {
            return;
        }
        let body = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, path = %self.path.display(), "teams-progress.jsonl read failed");
                return;
            }
        };
        // Walk every line starting at `self.cursor`. We use byte
        // offsets because chrono/UTF-8 boundaries don't matter — the
        // file is JSON-Lines, the lines are valid UTF-8.
        if body.len() <= self.cursor as usize {
            return;
        }
        let tail = &body[self.cursor as usize..];
        // Track how many bytes we'll consume; only advance the cursor
        // once we've processed every line so a mid-line crash retries
        // cleanly.
        let mut consumed = 0u64;
        for line in tail.split_inclusive('\n') {
            consumed += line.len() as u64;
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(err) => {
                    tracing::debug!(error = %err, "teams-progress.jsonl skip non-JSON line");
                    continue;
                }
            };
            let line_team = value
                .get("team_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if line_team != self.team_name {
                continue;
            }
            self.pending
                .push_back(Event::default().event("progress").data(value.to_string()));
        }
        self.cursor += consumed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_emits_only_matching_team_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("teams-progress.jsonl");
        std::fs::write(
            &path,
            r#"{"event":"team_member_joined","team_name":"roblog","teammate_name":"x"}
{"event":"team_message_sent","team_name":"other","from":"a","to":"b"}
{"event":"team_task_created","team_name":"roblog","task_id":"1"}
"#,
        )
        .unwrap();
        let mut st = StreamState::start(path.clone(), "roblog".into());
        st.refresh();
        assert_eq!(st.pending.len(), 2);
        // Cursor advanced to EOF — second refresh emits nothing.
        let cursor_after_first = st.cursor;
        st.pending.clear();
        st.refresh();
        assert_eq!(st.pending.len(), 0);
        assert_eq!(st.cursor, cursor_after_first);
    }

    #[test]
    fn refresh_is_resilient_to_garbage_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("teams-progress.jsonl");
        std::fs::write(
            &path,
            "not-json\n{\"event\":\"team_member_joined\",\"team_name\":\"roblog\"}\nbroken{\n",
        )
        .unwrap();
        let mut st = StreamState::start(path, "roblog".into());
        st.refresh();
        assert_eq!(st.pending.len(), 1);
    }
}
