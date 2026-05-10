//! V0.3 M5.2 — `GET /screenshot/<slug>.png` on-demand pane render.
//!
//! Wraps `ccteam_core::render_screenshot` (V0.2.2 F38), which:
//!
//! 1. Captures the active tmux pane for `<slug>` (ANSI escapes preserved)
//! 2. Runs it through the `vt100` state machine
//! 3. Renders the cell grid into an `RgbImage` via `imageproc`
//! 4. Saves the PNG under `<project>/.ccteam/screenshots/<utc>.png`
//! 5. Returns `Ok(Some(path))` on success, `Ok(None)` on graceful degrade
//!    (tmux missing / session not found / font failed / IO failure)
//!
//! This handler reads the saved PNG bytes back and serves them as
//! `image/png`. Cache-Control is `no-cache, must-revalidate` because
//! the user clicks "refresh" to capture *now*; a cached previous
//! capture would be misleading.
//!
//! Architectural red lines:
//!
//! - **No polling** (PRD §5.2.5 + dev-plan §8 grep) — F38 takes
//!   ~200-500 ms per render and the PNG is 100-500 KB; auto-polling
//!   would burn CPU and bandwidth. The handler runs only when the
//!   browser explicitly requests the URL.
//! - **No `unwrap` / `expect`** (dev-plan §8 grep) — every fallible
//!   step degrades to a 504 + plain-text reason so the
//!   project-detail page never 500s on a misbehaving tmux.
//! - **`spawn_blocking`** — `render_screenshot` shells out to
//!   `tmux capture-pane` and runs the imageproc render synchronously;
//!   wrap it in `spawn_blocking` so the axum runtime stays
//!   responsive.
//! - **F38 graceful degrade** — `Ok(None)` from `render_screenshot`
//!   maps to **504 Gateway Timeout** + plain-text reason (PRD §5.2.5),
//!   not a 404. The session might just not be running yet; the user
//!   should retry, not assume the slug is unknown.

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use ccteam_core::CcteamPaths;

use crate::state::AppState;

/// Number of pane lines to capture. F38's MCP tool defaults to 50;
/// the web view shows the same so feedback matches the
/// `ccteam-mcp screenshot` smoke output.
const SCREENSHOT_LINES: usize = 50;

pub fn router() -> Router<AppState> {
    Router::new().route("/screenshot/{file}", get(handle_screenshot))
}

async fn handle_screenshot(State(app): State<AppState>, Path(file): Path<String>) -> Response {
    // Strip the `.png` suffix to recover the slug. Anything else is a
    // 404 — the path stays narrow so a future `.jpg` or thumbnail
    // doesn't accidentally land here.
    let stem = match file.strip_suffix(".png") {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                "screenshot URL must be /screenshot/<slug>.png".to_string(),
            )
                .into_response();
        }
    };
    let (slug, sid) = resolve_screenshot_target(&app, &stem);

    let paths: CcteamPaths = (*app.paths).clone();
    let render = tokio::task::spawn_blocking(move || {
        ccteam_core::render_screenshot(&paths, &slug, sid.as_deref(), SCREENSHOT_LINES)
    })
    .await;

    let path_opt = match render {
        Ok(Ok(opt)) => opt,
        Ok(Err(err)) => {
            tracing::warn!(?err, "render_screenshot returned hard Err");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("screenshot rendering failed: {err}"),
            )
                .into_response();
        }
        Err(err) => {
            // spawn_blocking panicked or was cancelled — the renderer
            // has its own catch_unwind around vt100/imageproc, so
            // hitting this branch means tokio itself disagrees.
            tracing::warn!(?err, "render_screenshot spawn_blocking failed");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("screenshot worker failed: {err}"),
            )
                .into_response();
        }
    };

    let png_path = match path_opt {
        Some(p) => p,
        None => {
            // F38 graceful-degrade path. PRD §5.2.5 says 504 with a
            // plain-text body so the alt-text "screenshot unavailable"
            // shows in the browser instead of 500-ing the page.
            return (
                StatusCode::GATEWAY_TIMEOUT,
                "screenshot unavailable: tmux session not found or render degraded\n",
            )
                .into_response();
        }
    };

    // Read the PNG bytes back. F38 just wrote them; `read` is cheap
    // and avoids streaming a `tokio::fs::File` (no benefit at
    // ~500 KB per asset).
    let bytes = match tokio::fs::read(&png_path).await {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(file = %png_path.display(), error = %err, "read PNG failed");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                format!("read PNG failed: {err}"),
            )
                .into_response();
        }
    };

    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache, must-revalidate"),
            ),
        ],
        bytes,
    )
        .into_response()
}

fn resolve_screenshot_target(app: &AppState, stem: &str) -> (String, Option<String>) {
    if app.paths.project_state(stem).exists() {
        return (stem.to_string(), None);
    }
    if let Some((slug, sid)) = split_slug_sid(stem) {
        if ccteam_core::ProjectState::load(&app.paths.project_state(slug))
            .map(|state| state.sessions.contains_key(sid))
            .unwrap_or(false)
        {
            return (slug.to_string(), Some(sid.to_string()));
        }
    }
    (stem.to_string(), None)
}

fn split_slug_sid(stem: &str) -> Option<(&str, &str)> {
    for marker in ["-claude-", "-codex-"] {
        if let Some(idx) = stem.rfind(marker) {
            let slug = &stem[..idx];
            let sid = &stem[idx + 1..];
            let digits = sid.rsplit_once('-')?.1;
            if !slug.is_empty() && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            {
                return Some((slug, sid));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::{HarnessKind, ProjectState, SessionRecord, TeamKind};
    use chrono::Utc;

    fn fake_app() -> (tempfile::TempDir, AppState) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        };
        (tmp, AppState::new(paths))
    }

    #[test]
    fn split_slug_sid_handles_hyphenated_slug() {
        assert_eq!(
            split_slug_sid("research-bigquery-tax-claude-12"),
            Some(("research-bigquery-tax", "claude-12")),
        );
    }

    #[test]
    fn resolve_screenshot_target_uses_registered_flex_sid() {
        let (_tmp, app) = fake_app();
        let mut state = ProjectState::initial_for_team("dev-flex".into(), "dev".into());
        state.team_kind = TeamKind::Flex;
        state.sessions.insert(
            "claude-1".into(),
            SessionRecord {
                harness: HarnessKind::Claude,
                tmux_session: "ccteam-dev-flex-claude-1".into(),
                started_at: Utc::now(),
                pid: None,
            },
        );
        state.save(&app.paths.project_state("dev-flex")).unwrap();

        assert_eq!(
            resolve_screenshot_target(&app, "dev-flex-claude-1"),
            ("dev-flex".into(), Some("claude-1".into())),
        );
    }

    #[test]
    fn resolve_screenshot_target_prefers_exact_slug_over_sid_suffix() {
        let (_tmp, app) = fake_app();
        let exact = ProjectState::initial("dev-flex-claude-1".into());
        exact
            .save(&app.paths.project_state("dev-flex-claude-1"))
            .unwrap();

        assert_eq!(
            resolve_screenshot_target(&app, "dev-flex-claude-1"),
            ("dev-flex-claude-1".into(), None),
        );
    }
}
