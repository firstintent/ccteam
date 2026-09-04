//! `flow-run` hook — the RUN-LEVEL envelope of `ccteam flow run`.
//!
//! A flow's leaves are already on the ledger: every `agent()` call is an
//! ordinary delegation with a real sid and `parent_sid` attribution. What was
//! missing is the envelope around them — which hires belonged to one run, what
//! the run was called, and how it ended. That fact lived only in the run
//! directory of whichever machine typed `ccteam flow run`, so the daemon and
//! the web UI could not see a run at all.
//!
//! **Why a hook kind rather than a new route.** The runner is a short-lived CLI
//! process, not the daemon, so the row has to arrive over HTTP; the daemon
//! already exposes exactly one door for "an outside process has an event for
//! the ledger" (`POST /internal/hook/{kind}/{action}`), already behind the same
//! auth layer. Adding a route would have been a second door to the same room.
//!
//! **Why not the existing `progress-append` kind.** That handler is a Claude
//! Code hook-shape translator: it derives the project from a `cwd` field and
//! forwards a fixed tool-call vocabulary (`tool_name`, `file_path`, `command`,
//! `exit_code`). Every field of a run envelope would be silently dropped, and
//! `cwd` is the wrong resolver anyway — the flow CLI already knows its project
//! explicitly (`--project`, else the slug in the cwd's `.ccteam/state.json`),
//! and a run may be driven from a directory that is not the project's.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use ccteam_core::progress::{
    append_event, build_flow_brake_tripped_event, build_flow_run_finished_event,
    build_flow_run_started_event,
};
use ccteam_core::CcteamPaths;

/// The run opened.
pub const ACTION_STARTED: &str = "started";
/// The run ended, cleanly or not.
pub const ACTION_FINISHED: &str = "finished";
/// A brake refused new admissions (the run is still finishing in-flight work).
pub const ACTION_BRAKE: &str = "brake";

/// Translate one flow-run envelope submission into a `progress.jsonl` row.
///
/// Every field is validated here rather than in the route: the CLI is the only
/// caller today, but a hook kind is a public wire, and the ledger is the state
/// source of truth for every reader downstream of it.
pub fn flow_run(paths: &CcteamPaths, action: &str, stdin: &Value) -> Result<()> {
    let project = required_str(stdin, "project")?;
    ensure_ledger_slug(project)?;
    let run_id = required_str(stdin, "run_id")?;

    let event = match action {
        ACTION_STARTED => build_flow_run_started_event(
            run_id,
            project,
            stdin.get("parent_sid").and_then(Value::as_str),
            optional_str(stdin, "name"),
            optional_str(stdin, "description"),
            optional_str(stdin, "script_path"),
            required_str(stdin, "started_at")?,
        ),
        ACTION_FINISHED => build_flow_run_finished_event(
            run_id,
            project,
            stdin
                .get("agents")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0),
            stdin.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
            // A submission that forgot to say how it went is not a success:
            // an unreadable `ok` reads as "did not complete cleanly".
            stdin.get("ok").and_then(Value::as_bool).unwrap_or(false),
            stdin.get("brake").and_then(Value::as_str),
            required_str(stdin, "finished_at")?,
        ),
        ACTION_BRAKE => {
            build_flow_brake_tripped_event(run_id, project, optional_str(stdin, "reason"))
        }
        other => bail!("unknown `flow-run` action: {other} (started|finished|brake)"),
    };

    append_event(&paths.progress_jsonl(project), &event)
}

fn required_str<'a>(stdin: &'a Value, field: &str) -> Result<&'a str> {
    stdin
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("`flow-run` payload missing `{field}`"))
}

fn optional_str<'a>(stdin: &'a Value, field: &str) -> &'a str {
    stdin.get(field).and_then(Value::as_str).unwrap_or("")
}

/// The slug arrives over HTTP and is joined into a filesystem path, so it is
/// checked before it can name a file. `slugify` only ever produces `[a-z0-9-]`;
/// anything holding a separator or a parent-directory hop is not a slug this
/// daemon minted and is REFUSED rather than sanitized — quietly rewriting a
/// caller's project name would file a run under the wrong workspace, which is
/// worse than a visible error on an observability path.
///
/// `pub` because the web route's ACL gate must run the SAME shape check
/// BEFORE `can_see_project` resolves the slug into a filesystem path — a
/// traversal-shaped "slug" must never reach path resolution at all (s523 R2).
pub fn ensure_ledger_slug(slug: &str) -> Result<()> {
    let shaped = slug.len() <= 64
        && !slug.contains("..")
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !shaped {
        bail!("`project` is not a project slug: {slug:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::CcteamPaths;
    use serde_json::json;
    use tempfile::TempDir;

    fn fake_paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        }
    }

    fn rows(paths: &CcteamPaths, slug: &str) -> Vec<Value> {
        std::fs::read_to_string(paths.progress_jsonl(slug))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn started_then_finished_land_as_two_rows_in_the_project_ledger() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);

        flow_run(
            &paths,
            ACTION_STARTED,
            &json!({
                "project": "alpha",
                "run_id": "audit-1",
                "parent_sid": "s42",
                "name": "audit-routes",
                "description": "Audit route handlers",
                "script_path": "/w/flow.js",
                "started_at": "2026-09-01T10:15:00Z",
            }),
        )
        .unwrap();
        flow_run(
            &paths,
            ACTION_FINISHED,
            &json!({
                "project": "alpha",
                "run_id": "audit-1",
                "agents": 5,
                "cost_usd": 1.25,
                "ok": true,
                "finished_at": "2026-09-01T10:20:00Z",
            }),
        )
        .unwrap();

        let rows = rows(&paths, "alpha");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["event"], "flow_run_started");
        assert_eq!(rows[0]["parent_sid"], "s42");
        assert_eq!(rows[1]["event"], "flow_run_finished");
        assert_eq!(rows[1]["run_id"], "audit-1");
        assert_eq!(rows[1]["agents"], 5);
    }

    #[test]
    fn a_traversing_project_name_never_names_a_file() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let err = flow_run(
            &paths,
            ACTION_STARTED,
            &json!({
                "project": "../../etc/passwd",
                "run_id": "r1",
                "started_at": "2026-09-01T10:15:00Z",
            }),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not a project slug"),
            "unexpected error: {err}"
        );
        assert!(!paths.progress_dir().exists());
    }

    #[test]
    fn a_payload_without_a_run_id_is_refused_not_filed_anonymously() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let err = flow_run(
            &paths,
            ACTION_STARTED,
            &json!({"project": "alpha", "started_at": "2026-09-01T10:15:00Z"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("run_id"), "unexpected: {err}");
    }

    #[test]
    fn an_unknown_action_is_an_error_rather_than_a_silent_no_op() {
        let tmp = TempDir::new().unwrap();
        let paths = fake_paths(&tmp);
        let err = flow_run(
            &paths,
            "resumed",
            &json!({"project": "alpha", "run_id": "r1"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown `flow-run` action"));
    }
}
