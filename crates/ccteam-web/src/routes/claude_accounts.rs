//! Optional local cswap discovery. Account use is authorized again at the
//! gateway's common spawn boundary; project visibility is not a spend grant.

use crate::{auth::Identity, state::AppState};
use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Extension, Json,
};

#[utoipa::path(
    get, path = "/api/v1/vendors/claude/accounts", tag = "sessions",
    responses((status = 200, description = "Optional cswap availability and authorized subscription accounts", body = serde_json::Value))
)]
pub(crate) async fn handle_claude_accounts(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    let unavailable = |message: String| {
        Json(serde_json::json!({"available": false, "accounts": [], "message": message}))
            .into_response()
    };
    let Some(gateway) = &app.gateway else {
        return unavailable("Gateway is unavailable; default login remains available".into());
    };
    let access = match gateway
        .lock()
        .await
        .cswap_account_access(&identity.web_chat_id())
    {
        Ok(access) => access,
        Err(_) => return unavailable("Account permissions are unavailable".into()),
    };
    if access.as_ref().is_some_and(Vec::is_empty) {
        // Distinct from "cswap missing": the tool may be fine, this user just
        // has no grant. The operator grants from Settings → Admin → Users.
        return Json(serde_json::json!({
            "available": false,
            "reason": "not_granted",
            "accounts": [],
            "message": "No cswap accounts are granted to this user; an administrator can grant them in Settings → Admin → Users",
        }))
        .into_response();
    }
    match app.cswap_accounts.accounts().await {
        Ok(accounts) => {
            let accounts: Vec<_> = accounts
                .into_iter()
                .filter(|account| {
                    ccteam_core::identity::can_use_cswap_account(
                        access.is_none(),
                        access.as_deref().unwrap_or_default(),
                        &account.id,
                    )
                })
                .collect();
            Json(serde_json::json!({"available": true, "accounts": accounts})).into_response()
        }
        Err(error) => unavailable(error.to_string()),
    }
}
