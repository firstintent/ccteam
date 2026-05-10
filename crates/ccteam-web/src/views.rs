//! V0.3 M5.1 — askama template structs.
//!
//! Each handler builds one of these, then axum renders it via the
//! generic [`HtmlTemplate`] wrapper (`IntoResponse` impl below). We
//! keep the wrapper rather than depending on the deprecated
//! `askama_axum` crate (which never tracked axum 0.8).

use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

/// Wrap an askama template so axum renders it as `text/html` with a
/// graceful 500 fallback if rendering fails (askama 0.12 returns
/// `Result<String, askama::Error>`; we map any render error to a
/// plain-text 500 instead of panicking).
pub struct HtmlTemplate<T>(pub T);

impl<T: Template> IntoResponse for HtmlTemplate<T> {
    fn into_response(self) -> Response {
        match self.0.render() {
            Ok(body) => Html(body).into_response(),
            Err(err) => {
                tracing::error!(?err, "askama render failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("template render error: {err}"),
                )
                    .into_response()
            }
        }
    }
}

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate {
    pub version: &'static str,
    pub projects_root: String,
    pub projects: Vec<DashboardRow>,
}

pub struct DashboardRow {
    pub slug: String,
    pub team: String,
    pub current_phase: String,
    pub last_event_label: String,
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
}

#[derive(Template)]
#[template(path = "project.html")]
pub struct ProjectTemplate {
    pub version: &'static str,
    pub slug: String,
    pub team: String,
    pub current_phase: String,
    pub badge_class: &'static str,
    pub badge_label: &'static str,
    pub cost_label: String,
    pub state_json_pretty: String,
    pub events: Vec<EventRow>,
    pub outbox: Vec<OutboxRow>,
}

pub struct EventRow {
    pub ts: String,
    pub event: String,
    pub detail: String,
}

pub struct OutboxRow {
    pub filename: String,
    pub kind: String,
    pub created_at: String,
    pub preview: String,
}
