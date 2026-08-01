//! `GET /api/v1/models` — the spawn-tuning discovery surface.
//!
//! `POST .../sessions` takes `model` + `effort` and forwards both to the
//! vendor verbatim (see [`super::sessions_api::CreateSessionForm`]). That
//! contract is only usable if a caller can find out what a given vendor
//! actually takes, so this route publishes the two axes side by side:
//!
//! - **models** — the ids a vendor reported in its own last handshake
//!   ([`ccteam_core::model_catalog`]), with `observed_at` + `source` so the
//!   reader can judge staleness. Never observed → `models: []` and both
//!   provenance fields `null`: an empty list is an honest "ccteam has not
//!   heard from this vendor yet", never a claim that it has no models.
//! - **efforts** — the reasoning-effort ladder, vendor-declared when the
//!   handshake said so, else ccteam's CLI-verified pinned fallback.
//!
//! **Advisory, never a gate.** ccteam does not validate `model`/`effort`
//! against this response, and a value missing here still rides to the
//! vendor, which owns the verdict on its own value set. Filtering a spawn
//! against a cached table is exactly how an explicit caller choice used to
//! turn into a silent vendor default.
//!
//! Every known vendor gets a row even when nothing was ever observed, so a
//! client can render a complete picker without cross-referencing
//! `/api/v1/capabilities` for the vendor list.
//!
//! Auth: machine capability data, not project data — merged into the
//! `/api/v1` [`OpenApiRouter`](utoipa_axum::router::OpenApiRouter) like
//! [`super::capabilities`], so the shared web-token gate applies and any
//! logged-in identity may read it. No project ACL is involved because no
//! project is named.

use axum::{extract::State, response::IntoResponse, Json};
use ccteam_core::host_registry::AGENT_PROBE_SPECS;
use ccteam_core::model_catalog::{load_model_catalog_in, supported_efforts_in};
use serde::Serialize;
use utoipa::ToSchema;

use crate::state::AppState;

/// One model a vendor reported for itself.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelEntry {
    /// Opaque vendor model id — pass this back as `model` verbatim.
    pub id: String,
    /// Vendor display label; `null` when the handshake carried none.
    pub display_name: Option<String>,
    /// Per-model effort tokens when the vendor declared them per model;
    /// empty when it declares the ladder once for the whole vendor (see
    /// [`VendorModels::efforts`]).
    pub efforts: Vec<String>,
}

/// One vendor's tuning axes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VendorModels {
    /// Vendor token (`claude` / `codex` / `grok` / `opencode` / `kimi`) —
    /// the same string `POST .../sessions` accepts.
    pub vendor: String,
    /// RFC3339 capture time of the handshake behind [`Self::models`];
    /// `null` when this vendor has never been observed.
    pub observed_at: Option<String>,
    /// Which vendor protocol surface supplied [`Self::models`] (e.g.
    /// `"ACP session availableModels"`); `null` when never observed.
    pub source: Option<String>,
    /// Observed model ids; `[]` when never observed (NOT "has no models").
    pub models: Vec<ModelEntry>,
    /// Reasoning-effort ladder offered for this vendor: vendor-declared when
    /// its handshake said so, else the CLI-verified fallback. Empty means
    /// ccteam knows of no effort axis for this vendor (OpenCode today).
    pub efforts: Vec<String>,
}

/// `GET /api/v1/models` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelsResponse {
    /// One row per known vendor, in the registry's order.
    pub vendors: Vec<VendorModels>,
}

/// List each vendor's observed models + reasoning-effort ladder, for
/// populating a spawn composer. Advisory: ccteam never validates a spawn
/// against this list, so a value absent here still reaches the vendor.
#[utoipa::path(
    get,
    path = "/api/v1/models",
    tag = "models",
    responses((status = 200, description = "Per-vendor model catalog + effort ladders", body = ModelsResponse)),
)]
pub(crate) async fn handle_models(State(app): State<AppState>) -> impl IntoResponse {
    let root = app.paths.root.clone();
    let catalog = load_model_catalog_in(&root);
    let vendors = AGENT_PROBE_SPECS
        .iter()
        .map(|spec| {
            let entry = catalog.0.get(spec.vendor);
            VendorModels {
                vendor: spec.vendor.to_string(),
                observed_at: entry.map(|e| e.observed_at.clone()),
                source: entry.map(|e| e.source.clone()),
                models: entry
                    .map(|e| {
                        e.models
                            .iter()
                            .map(|m| ModelEntry {
                                id: m.id.clone(),
                                display_name: m.display_name.clone(),
                                efforts: m.efforts.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                // Deliberately the harness helper rather than a local fold
                // over `entry.models`: the "vendor-declared, else pinned
                // fallback" precedence has ONE home, and a web-local copy
                // would be the second place to drift.
                efforts: supported_efforts_in(&root, spec.vendor),
            }
        })
        .collect();
    Json(ModelsResponse { vendors })
}
