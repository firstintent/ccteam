//! `GET /health` — liveness probe.
//!
//! Returns 200 + `{ "status": "ok", "version": "<crate>" }`. M5.0
//! verification scripts (and the integration-test subprocess harness)
//! rely on this endpoint to confirm the server is up before issuing
//! further requests.

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

/// Build the `GET /health` router.
pub fn router() -> Router {
    Router::new().route("/health", get(handle_health))
}

async fn handle_health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_health_body_shape() {
        // Direct handler call — the in-process axum + reqwest path is
        // exercised by `crate::tests::serve_health_endpoint_returns_ok_json`
        // (lib.rs); here we just assert the body shape so a churn in
        // the JSON contract is loud.
        let Json(body) = handle_health().await;
        assert_eq!(body["status"], "ok");
        assert!(body["version"].as_str().is_some_and(|s| !s.is_empty()));
    }
}
