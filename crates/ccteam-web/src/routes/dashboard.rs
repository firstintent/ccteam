//! `GET /` — read-only project list.
//!
//! Calls `ccteam_core::collect_projects` (promoted from
//! `ccteam-cli::commands` in this PR — see
//! `docs/dev-coupling-audit.md` F45). Renders the askama
//! `dashboard.html` template with one row per project.
//!
//! For each project we additionally tail the last 200 progress events
//! and run them through the V0.2.2 F35 silence classifier to label a
//! status badge. **Read-only** — even if classification is
//! `PostStopLimbo` we never invoke the matching `LimboAction`. CLAUDE.md
//! §三 read-only red line.

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use ccteam_core::collect_projects;

use crate::queries::{recent_event_summary, slug_recent_events};
use crate::state::AppState;
use crate::status::status_badge;
use crate::views::{DashboardRow, DashboardTemplate, HtmlTemplate};

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(handle_index))
}

async fn handle_index(State(app): State<AppState>) -> impl IntoResponse {
    match build_template(&app) {
        Ok(tpl) => HtmlTemplate(tpl).into_response(),
        Err(err) => {
            tracing::error!(?err, "dashboard render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("dashboard error: {err}"),
            )
                .into_response()
        }
    }
}

fn build_template(app: &AppState) -> anyhow::Result<DashboardTemplate> {
    let summaries = collect_projects(&app.paths)?;
    let mut rows = Vec::with_capacity(summaries.len());
    for s in summaries {
        // Per project: read 200 progress events for the badge taxonomy.
        // For dashboards with <20 projects this is well under 1ms each
        // (typical progress.jsonl < 20 KB). Documented in
        // docs/v0-3/prd.md §4.2.2 + tech-design §5.5 (progress.jsonl
        // is the SoT, never tmux output).
        let events = slug_recent_events(&app.paths, &s.state.slug, 200);
        let badge = status_badge(&s.state, &events, s.stall_silent_seconds);
        let last_event_label = match s.state.last_progress_event_at {
            Some(ts) => recent_event_summary(ts, s.stall_silent_seconds),
            None => "—".to_string(),
        };
        rows.push(DashboardRow {
            slug: s.state.slug.clone(),
            team: s.state.team.clone(),
            current_phase: s.state.current_phase.clone(),
            last_event_label,
            badge_class: badge.css_class(),
            badge_label: badge.label(),
            cost_label: format!("{:.2}", s.state.cost_used_usd),
        });
    }

    Ok(DashboardTemplate {
        version: env!("CARGO_PKG_VERSION"),
        projects_root: app.paths.projects_root.display().to_string(),
        projects: rows,
    })
}
