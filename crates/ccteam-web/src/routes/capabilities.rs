//! v0.8.6 W5b ResDisk — `GET /api/v1/capabilities`.
//!
//! Reports the harness vendors ccteam can drive, each annotated with an
//! `available` flag from a PATH probe of the vendor binary. The SPA uses
//! this to grey out a "create session with vendor X" affordance when the
//! binary isn't installed.
//!
//! The probe is the SINGLE implementation in
//! [`ccteam_core::host_registry`] (`probe_bin_cached`, path-keyed and cached,
//! over the shared `AGENT_PROBE_SPECS` registry), shared with the host-keyed
//! `GET /api/v1/hosts` report, the satellite report loop, and the MCP
//! `status` panel, so none of them can drift apart. The wire shape here is
//! unchanged (`harnesses` of `id, vendor, available, providers`), and
//! `providers` stays reserved (empty).
//!
//! Auth: merged into [`super::stateful_router`] via the `/api/v1`
//! `OpenApiRouter`, so the existing `auth_layer` gate applies for free.

use axum::{response::IntoResponse, Json};
use ccteam_core::host_registry::{probe_bin_cached, resolve_bin, AGENT_PROBE_SPECS};
use serde::Serialize;
use utoipa::ToSchema;

/// One harness entry in the capabilities response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HarnessCapability {
    /// Stable harness id (`claude-code` / `codex`).
    pub id: &'static str,
    /// Vendor token matching [`ccteam_harness::AgentVendor`]'s lowercase
    /// serde form (`claude` / `codex`) — the same string
    /// `POST .../sessions` accepts.
    pub vendor: &'static str,
    /// Whether the vendor binary is on PATH (or its `CCTEAM_*_BIN`
    /// override resolves to an executable).
    pub available: bool,
    /// Reserved for per-vendor provider/model enumeration; empty for now.
    pub providers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CapabilitiesResponse {
    pub harnesses: Vec<HarnessCapability>,
}

/// List the harness vendors ccteam can drive, each flagged `available`
/// from a PATH probe of the vendor binary. `providers` is reserved.
#[utoipa::path(
    get,
    path = "/api/v1/capabilities",
    tag = "capabilities",
    responses((status = 200, description = "Harness capability matrix", body = CapabilitiesResponse)),
)]
pub(crate) async fn handle_capabilities() -> impl IntoResponse {
    // Probe each spec off the async runtime (each shells out; cached, so the
    // common case never spawns a child). Reuses the SINGLE `hosts::probe_bin`.
    let harnesses = tokio::task::spawn_blocking(|| {
        AGENT_PROBE_SPECS
            .iter()
            .map(|spec| HarnessCapability {
                id: spec.harness_id,
                vendor: spec.vendor,
                available: probe_bin_cached(&resolve_bin(spec), false).0,
                providers: Vec::new(),
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|err| {
        // Worker panicked/cancelled — degrade to "all unavailable" rather
        // than a 500; the SPA just shows both vendors disabled.
        tracing::warn!(?err, "capabilities probe worker failed");
        Vec::new()
    });
    Json(CapabilitiesResponse { harnesses })
}
