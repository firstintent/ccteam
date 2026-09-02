//! `GET /api/v1/projects/{slug}/flow-runs` — read-only list of the `ccteam
//! flow run` envelopes on this project's ledger.
//!
//! A flow's leaves already have a home in the UI: each `agent()` call is an
//! ordinary delegation with a real sid, drawn in the team topology through
//! `parent_sid`. This endpoint answers the question the topology cannot — which
//! of those hires belonged to one run, what that run was called, whether it is
//! still going, and how it ended.
//!
//! The source is `progress.jsonl`, like every other read-side aggregate here:
//! the runner is a CLI process that submits its three envelope rows through the
//! `flow-run` hook, and this handler folds them back into runs. Nothing is read
//! from a run directory — those live on whichever machine drove the run, which
//! is not necessarily this one.
//!
//! The journal is read for ENVELOPE rows, not for rows: a run is three rows
//! among thousands of chat and tool rows, so the walk is bounded in envelopes
//! kept (plus a byte budget), never in journal rows. The first cut of this
//! endpoint tailed a fixed 5000-row window and reported the window overflowing
//! as "older runs scrolled out" — on any project that had chatted more than
//! 5000 rows that read as a warning about runs that never existed, and a run
//! older than 5000 rows of chat silently vanished (docs-local/issues/#16).

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_core::journal::Pick;
use ccteam_core::progress::{FLOW_BRAKE_TRIPPED, FLOW_RUN_FINISHED, FLOW_RUN_STARTED};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

use super::sessions_api::project_not_visible;

/// How many runs one request lists, at least.
///
/// The bound is in envelope ROWS kept while walking the journal backwards:
/// a run contributes a `started` row, a `finished` row and at most a few
/// `brake` rows, so three rows per run is the floor and the list holds at
/// least [`RUN_LIMIT`] complete runs whenever that many exist. Any run whose
/// rows fall past the window is not listed and `truncated` says so.
const RUN_LIMIT: usize = 50;
const ENVELOPE_ROW_LIMIT: usize = RUN_LIMIT * 3;

/// How much journal one request may read.
///
/// Envelope rows are rare, so a project that never ran a flow is walked to
/// its first byte — the budget is what keeps that walk finite on a very large
/// journal (this is a cold endpoint behind a tab, polled only while the tab
/// is open, not one of the hot status paths). Hitting it reports `truncated`,
/// because a walk that stopped early cannot promise the rest holds no run.
const SCAN_BYTE_BUDGET: u64 = 64 * 1024 * 1024;

/// The envelope kinds, as the bytes a row of that kind must contain. A byte
/// scan for these gates the JSON parse, so the thousands of chat rows between
/// two runs cost a substring search each, not a parse.
const ENVELOPE_KINDS: [&str; 3] = [FLOW_RUN_STARTED, FLOW_RUN_FINISHED, FLOW_BRAKE_TRIPPED];

/// One flow run, folded from its envelope rows.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlowRun {
    /// The run directory's basename — stable across `--resume`, and the join
    /// key to the on-disk journal an evaluation reads.
    pub run_id: String,
    /// `meta.name` from the script. Empty when the script declared none.
    pub name: String,
    /// `meta.description` from the script. Empty when the script declared none.
    pub description: String,
    /// The managed session that launched the run — the same attribution its
    /// hires carry. `null` when a flow was started from a plain shell.
    pub parent_sid: Option<String>,
    /// `running` | `ok` | `error` | `brake`.
    pub status: String,
    /// Agents the run hired (0 until it reports).
    pub agents: u64,
    /// USD spent by the run's hires.
    pub cost_usd: f64,
    /// RFC3339, the run's own clock.
    pub started_at: String,
    /// RFC3339; `null` while the run is still going.
    pub finished_at: Option<String>,
}

/// Flow runs for one project, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FlowRuns {
    pub runs: Vec<FlowRun>,
    /// True when at least one run exists that is NOT listed: the journal holds
    /// more envelope rows than the window keeps (only the newest runs are
    /// shown), or the read budget ran out before the journal's first byte.
    /// False means the list is complete — including an empty list on a busy
    /// project that simply never ran a flow. A bounded window is fine; a
    /// silently bounded one is not: the reader must be able to tell "no runs"
    /// from "no runs *shown*".
    pub truncated: bool,
}

/// `GET /api/v1/projects/{slug}/flow-runs`
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/flow-runs",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Flow runs, newest first (may be empty)", body = FlowRuns),
        (status = 404, description = "Project not visible / unknown"),
    ),
)]
pub(crate) async fn handle_flow_runs(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(slug): Path<String>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    // A project that has never run a flow — or never run anything — has no
    // journal, and an unreadable one is an operational fault, not this
    // endpoint's to report. Both are the same honest answer: no runs.
    // `has_more` comes from the tail reader's one-extra-row probe on KEPT
    // rows (or its byte budget), never from `len() >= limit` (s523 R2).
    let (events, truncated) = match ccteam_core::collect_recent_events_where(
        &app.paths,
        &slug,
        ENVELOPE_ROW_LIMIT,
        SCAN_BYTE_BUDGET,
        pick_envelope_row,
    ) {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(%slug, error = %err, "flow-runs: read progress journal failed");
            (Vec::new(), false)
        }
    };
    (
        StatusCode::OK,
        Json(FlowRuns {
            runs: fold_runs(&events),
            truncated,
        }),
    )
        .into_response()
}

/// Keep a journal row only when it is a flow envelope.
///
/// Bytes first: a row that does not even contain an envelope kind's name is
/// skipped without parsing. A row that does (a chat row quoting the name, say)
/// is parsed and judged on its `event` field like any other. Rows that are not
/// JSON at all are corrupt — the same verdict every other reader gives them.
fn pick_envelope_row(line: &[u8]) -> Pick<Value> {
    if !ENVELOPE_KINDS
        .iter()
        .any(|kind| contains_bytes(line, kind.as_bytes()))
    {
        return Pick::Skip;
    }
    match serde_json::from_slice::<Value>(line) {
        Ok(event) if is_envelope(&event) => Pick::Keep(event),
        Ok(_) => Pick::Skip,
        Err(_) => Pick::Corrupt,
    }
}

fn is_envelope(event: &Value) -> bool {
    event
        .get("event")
        .and_then(Value::as_str)
        .is_some_and(|kind| ENVELOPE_KINDS.contains(&kind))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// What the envelope rows seen so far say about one run.
#[derive(Default)]
struct RunAcc {
    name: String,
    description: String,
    parent_sid: Option<String>,
    started_at: String,
    finished_at: Option<String>,
    agents: u64,
    cost_usd: f64,
    /// The runner's own verdict, once it reported one.
    ok: Option<bool>,
    /// Set by either a `flow_brake_tripped` row or a `brake` on the terminal
    /// row — a brake is visible from the moment it trips, not only at the end.
    brake: Option<String>,
}

/// Fold chronological progress rows into runs, newest first.
///
/// Kept as a pure function over the rows so the derivation — especially the
/// status ladder — is testable without a journal or a server.
fn fold_runs(events: &[Value]) -> Vec<FlowRun> {
    let mut acc: HashMap<String, RunAcc> = HashMap::new();

    for event in events {
        let kind = event
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !matches!(
            kind,
            FLOW_RUN_STARTED | FLOW_RUN_FINISHED | FLOW_BRAKE_TRIPPED
        ) {
            continue;
        }
        let Some(run_id) = event
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let run = acc.entry(run_id.to_string()).or_default();
        match kind {
            FLOW_RUN_STARTED => {
                run.name = string_field(event, "name");
                run.description = string_field(event, "description");
                run.parent_sid = event
                    .get("parent_sid")
                    .and_then(Value::as_str)
                    .filter(|sid| !sid.is_empty())
                    .map(str::to_owned);
                run.started_at = string_field(event, "started_at");
            }
            FLOW_RUN_FINISHED => {
                run.finished_at = Some(string_field(event, "finished_at"));
                run.agents = event.get("agents").and_then(Value::as_u64).unwrap_or(0);
                run.cost_usd = event.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                run.ok = event.get("ok").and_then(Value::as_bool);
                if let Some(brake) = event.get("brake").and_then(Value::as_str) {
                    if !brake.is_empty() {
                        run.brake = Some(brake.to_string());
                    }
                }
            }
            FLOW_BRAKE_TRIPPED => {
                run.brake = Some(string_field(event, "reason"));
            }
            _ => {}
        }
    }

    let mut runs: Vec<FlowRun> = acc
        .into_iter()
        // A run whose opening row has already scrolled out of the scan window
        // is dropped rather than shown with an invented start time: this list
        // is "recent runs", and a row that cannot say when or what it was is
        // not a run anyone can act on.
        .filter(|(_, run)| !run.started_at.is_empty())
        .map(|(run_id, run)| FlowRun {
            run_id,
            status: status_of(&run).to_string(),
            name: run.name,
            description: run.description,
            parent_sid: run.parent_sid,
            agents: run.agents,
            cost_usd: run.cost_usd,
            started_at: run.started_at,
            finished_at: run.finished_at,
        })
        .collect();
    // Newest first. RFC3339 UTC timestamps order lexicographically, and the
    // run id breaks ties so the list never reshuffles between requests.
    runs.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    runs
}

/// The status ladder. A brake outranks the runner's `ok:false`, because every
/// braked run also reports `ok:false` and "hit its ceiling" is a different
/// thing for a reader to do something about than "the script broke".
fn status_of(run: &RunAcc) -> &'static str {
    if run.brake.is_some() {
        return "brake";
    }
    match (run.finished_at.is_some(), run.ok) {
        (false, _) => "running",
        (true, Some(true)) => "ok",
        (true, _) => "error",
    }
}

fn string_field(event: &Value, field: &str) -> String {
    event
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn started(run_id: &str, started_at: &str) -> Value {
        json!({
            "event": FLOW_RUN_STARTED,
            "run_id": run_id,
            "project": "alpha",
            "name": "audit-routes",
            "description": "Audit route handlers",
            "parent_sid": "s42",
            "started_at": started_at,
        })
    }

    fn finished(run_id: &str, ok: bool, brake: Option<&str>) -> Value {
        let mut event = json!({
            "event": FLOW_RUN_FINISHED,
            "run_id": run_id,
            "project": "alpha",
            "agents": 5,
            "cost_usd": 1.25,
            "ok": ok,
            "finished_at": "2026-09-01T10:20:00Z",
        });
        if let Some(brake) = brake {
            event["brake"] = json!(brake);
        }
        event
    }

    #[test]
    fn a_started_row_without_its_partner_is_still_running() {
        let runs = fold_runs(&[started("r1", "2026-09-01T10:15:00Z")]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "running");
        assert_eq!(runs[0].run_id, "r1");
        assert_eq!(runs[0].name, "audit-routes");
        assert_eq!(runs[0].parent_sid.as_deref(), Some("s42"));
        assert!(runs[0].finished_at.is_none());
        // Totals belong to the terminal row; a live run does not fake them.
        assert_eq!(runs[0].agents, 0);
    }

    #[test]
    fn the_terminal_row_decides_ok_versus_error() {
        let ok = fold_runs(&[
            started("r1", "2026-09-01T10:15:00Z"),
            finished("r1", true, None),
        ]);
        assert_eq!(ok.len(), 1, "a run is one entry, not one per row");
        assert_eq!(ok[0].status, "ok");
        assert_eq!(ok[0].agents, 5);
        assert_eq!(ok[0].cost_usd, 1.25);
        assert_eq!(ok[0].finished_at.as_deref(), Some("2026-09-01T10:20:00Z"));

        let errored = fold_runs(&[
            started("r1", "2026-09-01T10:15:00Z"),
            finished("r1", false, None),
        ]);
        assert_eq!(errored[0].status, "error");
    }

    #[test]
    fn a_brake_outranks_the_not_ok_it_always_comes_with() {
        // Both shapes a brake can arrive in must read as `brake`, never as the
        // `error` that `ok:false` alone would give.
        let on_the_terminal_row = fold_runs(&[
            started("r1", "2026-09-01T10:15:00Z"),
            finished("r1", false, Some("max_agents")),
        ]);
        assert_eq!(on_the_terminal_row[0].status, "brake");

        let as_its_own_row = fold_runs(&[
            started("r1", "2026-09-01T10:15:00Z"),
            json!({"event": FLOW_BRAKE_TRIPPED, "run_id": "r1", "reason": "max_cost"}),
            finished("r1", false, None),
        ]);
        assert_eq!(as_its_own_row[0].status, "brake");

        // A brake refuses NEW admissions; in-flight work keeps going, so a run
        // that has only tripped a brake is still shown as braked, not finished.
        let still_draining = fold_runs(&[
            started("r1", "2026-09-01T10:15:00Z"),
            json!({"event": FLOW_BRAKE_TRIPPED, "run_id": "r1", "reason": "max_cost"}),
        ]);
        assert_eq!(still_draining[0].status, "brake");
        assert!(still_draining[0].finished_at.is_none());
    }

    #[test]
    fn runs_come_back_newest_first() {
        let runs = fold_runs(&[
            started("older", "2026-09-01T09:00:00Z"),
            started("newer", "2026-09-01T11:00:00Z"),
            started("middle", "2026-09-01T10:00:00Z"),
        ]);
        let order: Vec<&str> = runs.iter().map(|run| run.run_id.as_str()).collect();
        assert_eq!(order, ["newer", "middle", "older"]);
    }

    #[test]
    fn the_row_picker_keeps_envelopes_and_skips_everything_else_unparsed() {
        let started = json!({"event": FLOW_RUN_STARTED, "run_id": "r1"}).to_string();
        assert!(matches!(
            pick_envelope_row(started.as_bytes()),
            Pick::Keep(_)
        ));

        // A chat row merely QUOTING an envelope kind is parsed and then
        // skipped on its real `event` — never kept, never corrupt.
        let quoting =
            json!({"event": "chat_turn_completed", "text": "see flow_run_started"}).to_string();
        assert_eq!(pick_envelope_row(quoting.as_bytes()), Pick::Skip);

        // A row without any kind name is skipped before parsing — so even a
        // torn one is not this reader's corruption to count.
        assert_eq!(
            pick_envelope_row(br#"{"event":"chat_tool_call_started""#),
            Pick::Skip
        );
        assert_eq!(
            pick_envelope_row(br#"{"event":"chat_tool_call_started"}"#),
            Pick::Skip
        );

        // A torn envelope row IS corrupt: it was meant for us and is unreadable.
        assert_eq!(
            pick_envelope_row(br#"{"event":"flow_run_finished","run_id":"#),
            Pick::Corrupt
        );
    }

    #[test]
    fn unrelated_and_unattributable_rows_are_ignored() {
        let runs = fold_runs(&[
            json!({"event": "chat_turn_completed", "sid": "s1"}),
            json!({"event": FLOW_RUN_STARTED, "project": "alpha"}), // no run_id
            json!({"not even an event": true}),
            finished("orphan", true, None), // start scrolled out of the window
            started("r1", "2026-09-01T10:15:00Z"),
        ]);
        let ids: Vec<&str> = runs.iter().map(|run| run.run_id.as_str()).collect();
        assert_eq!(ids, ["r1"]);
    }
}
