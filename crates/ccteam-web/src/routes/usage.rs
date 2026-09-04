//! `GET /api/v1/usage` — where each harness ACCOUNT stands right now.
//!
//! The script-side twin of the MCP `status{detail:"usage"}` body an agent
//! calls: one request answers "which harness still has quota to hire from",
//! so a scheduler never has to shell out per vendor or scrape a dashboard.
//! Both surfaces render through [`ccteam_im::usage_view`] — one fact, one
//! spelling, so a script and an agent can never be told different numbers.
//!
//! **Not a probe.** Nothing here polls, and nothing here logs into a vendor:
//! [`Gateway::account_usage_snapshot`](ccteam_im::gateway::Gateway::account_usage_snapshot)
//! asks live same-vendor adapters for state they already hold in memory (never
//! a turn) and otherwise reads the recorded observation from
//! [`ccteam_harness::usage_catalog`], which drops each window at the vendor's
//! OWN declared reset. Without a live gateway (standalone web) the cached half
//! still answers — the numbers are the daemon's memory, not a session's.
//!
//! **Honest absence.** A vendor with nothing observed is simply not in the map.
//! There is no `unknown` row, because a reader would have to guess whether that
//! meant "plenty left" or "no idea", and the second is what it always means.
//!
//! Distinct from `GET /api/v1/vendors/quota` on purpose: that one is an
//! admin-only NETWORK probe of vendor billing APIs using the daemon user's
//! credential files. This one costs no network and no credentials — it is the
//! daemon reporting what its own sessions already told it — so it sits inside
//! the ordinary web-token gate like [`super::models`], readable by any
//! authenticated identity. It is machine state, not any user's data (the IM
//! `/status` card has always shown the same windows to every chat).

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
    Json,
};
use ccteam_harness::AgentVendor;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use utoipa::IntoParams;

use crate::state::AppState;

/// `?vendor=claude` — narrow the map to one harness.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub(crate) struct UsageQuery {
    /// Harness token (`claude` / `codex` / `grok` / `opencode` / `kimi` / `pi`
    /// / `dsh`). Omit for every harness ccteam has heard from. An unknown
    /// token is not an error — it simply matches nothing, same as a harness
    /// with no observation.
    vendor: Option<String>,
}

/// `GET /api/v1/usage` — per-harness account windows.
#[utoipa::path(
    get,
    path = "/api/v1/usage",
    tag = "usage",
    params(UsageQuery),
    responses(
        (
            status = 200,
            description = "Per-harness account usage \
                           `{usage: {<harness>: {observed, source, subscription?, \
                           windows: [{w: 5h|7d|credits, model?, pct?, resets?, severity?}]}}}`. \
                           A harness with no unexpired observation is absent.",
            body = serde_json::Value
        ),
        (status = 401, description = "No web token presented"),
    ),
)]
pub(crate) async fn handle_usage(
    State(app): State<AppState>,
    Query(query): Query<UsageQuery>,
) -> Response {
    let wanted = query
        .vendor
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    // An unparseable filter matches no harness — an empty map, never every
    // harness (a typo must not silently widen the answer).
    let filter = match wanted {
        Some(raw) => match parse_vendor(raw) {
            Some(vendor) => Some(vendor),
            None => return Json(json!({ "usage": Map::new() })).into_response(),
        },
        None => None,
    };

    let usage: Map<String, Value> = match app.gateway.as_ref() {
        Some(gw) => {
            let guard = ccteam_im::latency::gateway_lock(gw, "web.usage").await;
            guard
                .account_usage_snapshot(filter)
                .await
                .iter()
                .map(|(vendor, entry)| {
                    (
                        vendor.clone(),
                        ccteam_im::usage_view::vendor_usage_value(entry),
                    )
                })
                .collect()
        }
        // Standalone web: no adapter to ask, but the recorded observations are
        // on disk and still describe the account until their own reset.
        None => AgentVendor::ALL
            .iter()
            .copied()
            .filter(|candidate| filter.is_none_or(|want| want == *candidate))
            .filter_map(|candidate| {
                let vendor = vendor_token(candidate);
                let entry = ccteam_harness::usage_catalog::last_known_entry_in(
                    &app.paths.root,
                    vendor,
                    chrono::Utc::now(),
                )?;
                Some((
                    vendor.to_string(),
                    ccteam_im::usage_view::vendor_usage_value(&entry),
                ))
            })
            .collect(),
    };
    Json(json!({ "usage": usage })).into_response()
}

/// Harness token → vendor. Same spelling `POST .../sessions` accepts.
fn parse_vendor(raw: &str) -> Option<AgentVendor> {
    let raw = raw.to_ascii_lowercase();
    AgentVendor::ALL
        .iter()
        .copied()
        .find(|candidate| vendor_token(*candidate) == raw)
}

/// The wire token for a harness — the key of the response map.
fn vendor_token(vendor: AgentVendor) -> &'static str {
    match vendor {
        AgentVendor::Claude => "claude",
        AgentVendor::Codex => "codex",
        AgentVendor::Grok => "grok",
        AgentVendor::Opencode => "opencode",
        AgentVendor::Kimi => "kimi",
        AgentVendor::Pi => "pi",
        AgentVendor::Dsh => "dsh",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_harness_token_round_trips() {
        for vendor in AgentVendor::ALL.iter().copied() {
            assert_eq!(parse_vendor(vendor_token(vendor)), Some(vendor));
            assert_eq!(
                parse_vendor(&vendor_token(vendor).to_uppercase()),
                Some(vendor)
            );
        }
        assert_eq!(parse_vendor("nope"), None);
        assert_eq!(parse_vendor(""), None);
    }
}
