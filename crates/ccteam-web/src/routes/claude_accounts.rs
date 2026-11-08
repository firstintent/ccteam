//! Optional local cswap discovery. Every identity that can start a Claude
//! session may use any local account (owner decision 2026-11-06, #229): the
//! default login is already the box owner's subscription, so a per-account
//! grant would gate nothing the default path does not already expose.

use crate::state::AppState;
use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Json,
};

#[utoipa::path(
    get, path = "/api/v1/vendors/claude/accounts", tag = "sessions",
    responses((status = 200, description = "Optional cswap availability and local subscription accounts", body = serde_json::Value))
)]
pub(crate) async fn handle_claude_accounts(State(app): State<AppState>) -> Response {
    match app.cswap_accounts.accounts().await {
        Ok(accounts) => {
            Json(serde_json::json!({"available": true, "accounts": accounts})).into_response()
        }
        Err(error) => Json(serde_json::json!({
            "available": false,
            "accounts": [],
            "message": error.to_string(),
        }))
        .into_response(),
    }
}
