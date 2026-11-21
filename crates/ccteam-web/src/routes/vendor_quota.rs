//! VENDOR-QUOTA-1 — `GET /api/v1/vendors/quota`: the local machine's vendor
//! subscription-quota snapshot for the Ops & Hosts page.
//!
//! Standalone endpoint, deliberately NOT folded into the host detail: quota
//! probing is network I/O (two vendor APIs) and must not slow the binary
//! probe path. Claude is read from the usage ccteam recorded from its own
//! rate-limit headers instead (#244, [`QuotaProbeKind::ClaudeRecordedUsage`]). Admin-only ([`deny_non_admin`]) — it reads the daemon user's
//! vendor credential files. Those reads are strictly read-only and there are
//! NO OAuth refresh flows: an expired token simply 401s into `unavailable`.
//!
//! Per-vendor failure isolation: one vendor's timeout / 401 / shape drift
//! maps to ITS `unavailable` row and never stalls or fails the others (all
//! probes run concurrently; each has an 8s budget). Results cache per-vendor
//! for 5 minutes (process-lifetime, the `probe_bin_cached` idiom). The
//! normalized model + pure parsers live in `ccteam_core::vendor_quota`; this
//! module is the thin HTTP + credential-file + cache layer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_core::host_registry::AgentProbeSpec;
use ccteam_core::vendor_quota::{
    claude_plan_from_credentials, codex_chatgpt_from_auth, kimi_managed_token_from_credentials,
    parse_codex_wham_usage, parse_kimi_usages, probed_specs, QuotaProbeKind, QuotaVerdict,
    QuotaWindow, QuotaWindowKind, VendorQuota,
};

use crate::auth::{deny_non_admin, Identity};
use crate::state::AppState;

/// Per-vendor result cache TTL — quota windows move on a 5h/weekly clock;
/// 5 minutes is far fresher than anything the bars can show.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Per-request budget for one vendor's usage API.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

const CODEX_WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const KIMI_USAGES_URL: &str = "https://api.kimi.com/coding/v1/usages";

/// Process-lifetime, per-vendor TTL cache (mirrors `probe_bin_cached`'s
/// shape: a Mutex map keyed by vendor token).
#[derive(Debug)]
struct QuotaCache {
    ttl: Duration,
    map: Mutex<BTreeMap<&'static str, (Instant, VendorQuota)>>,
}

impl QuotaCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            map: Mutex::new(BTreeMap::new()),
        }
    }

    /// The cached row when it is younger than the TTL.
    fn fresh(&self, vendor: &str) -> Option<VendorQuota> {
        let map = self.map.lock().ok()?;
        let (at, quota) = map.get(vendor)?;
        if at.elapsed() < self.ttl {
            Some(quota.clone())
        } else {
            None
        }
    }

    fn store(&self, vendor: &'static str, quota: VendorQuota) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(vendor, (Instant::now(), quota));
        }
    }
}

/// The quota probe service held on `AppState`.
#[derive(Debug)]
pub struct VendorQuotaService {
    client: reqwest::Client,
    cache: QuotaCache,
}

impl Default for VendorQuotaService {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            cache: QuotaCache::new(CACHE_TTL),
        }
    }
}

impl VendorQuotaService {
    /// Every probe-bearing vendor's quota row, registry-ordered. Cached rows
    /// serve from memory; misses probe concurrently and fill the cache. The
    /// claude row reads a local record under `root` (the ccteam home) every
    /// time — nothing to cache.
    pub async fn quotas(&self, root: &Path) -> Vec<VendorQuota> {
        let mut by_vendor: BTreeMap<&'static str, VendorQuota> = BTreeMap::new();
        let mut misses = Vec::new();
        for (spec, kind) in probed_specs() {
            if kind == QuotaProbeKind::ClaudeRecordedUsage {
                by_vendor.insert(spec.vendor, claude_recorded_quota(spec.vendor, root));
            } else if let Some(hit) = self.cache.fresh(spec.vendor) {
                by_vendor.insert(spec.vendor, hit);
            } else {
                misses.push((spec, kind));
            }
        }
        let probed = futures::future::join_all(misses.iter().map(|(spec, kind)| {
            let client = &self.client;
            async move { (spec.vendor, probe_one(client, spec, *kind).await) }
        }))
        .await;
        for (vendor, quota) in probed {
            self.cache.store(vendor, quota.clone());
            by_vendor.insert(vendor, quota);
        }
        // Emit in REGISTRY order (the probe axis), not alphabetical.
        probed_specs()
            .filter_map(|(spec, _)| by_vendor.remove(spec.vendor))
            .collect()
    }
}

// ── credential files (read-only) ────────────────────────────────────────────

/// `$CLAUDE_CONFIG_HOME/.credentials.json` (default `~/.claude/…`).
fn claude_credentials_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CLAUDE_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")))?;
    Some(dir.join(".credentials.json"))
}

/// `$CODEX_HOME/auth.json` (default `~/.codex/…`) — mirrors
/// `mcp_register::resolve_codex_config_path`'s home resolution.
fn codex_auth_path() -> Option<PathBuf> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
    Some(dir.join("auth.json"))
}

/// `$KIMI_CODE_HOME/credentials/kimi-code.json` (default `~/.kimi-code/…`).
fn kimi_credentials_path() -> Option<PathBuf> {
    let dir = std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")))?;
    Some(dir.join("credentials").join("kimi-code.json"))
}

/// Read + extract one credential file. A missing/unreadable file or a
/// credential of the wrong KIND (API-key account) reads as `None` → the
/// caller maps it to `not_subscription` — never an error, never a guess.
fn read_credential<T>(path: Option<PathBuf>, extract: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    let body = std::fs::read_to_string(path?).ok()?;
    extract(&body)
}

/// The claude row: the plan from the credential file, the windows from the
/// usage ccteam last recorded from claude's own rate-limit headers (each
/// window dropped once its reset has passed). No subscription login →
/// `not_subscription`; nothing recorded that still holds → `unavailable`.
fn claude_recorded_quota(vendor: &str, root: &Path) -> VendorQuota {
    claude_quota_from(
        vendor,
        read_credential(claude_credentials_path(), claude_plan_from_credentials),
        ccteam_harness::usage_catalog::last_known_entry_in(root, vendor, chrono::Utc::now()),
    )
}

/// [`claude_recorded_quota`] without the file reads: `plan` is the credential
/// verdict, `entry` the recorded observation (already stripped of reset
/// windows).
fn claude_quota_from(
    vendor: &str,
    plan: Option<Option<String>>,
    entry: Option<ccteam_harness::usage_catalog::VendorAccountUsage>,
) -> VendorQuota {
    let Some(plan) = plan else {
        return VendorQuota::not_subscription(vendor);
    };
    let Some(entry) = entry else {
        return VendorQuota::unavailable(vendor);
    };
    let usage = &entry.usage;
    let window = |kind, pct: Option<u8>, resets_at: Option<&str>| {
        pct.map(|pct| QuotaWindow {
            kind,
            used_percent: f64::from(pct).min(100.0),
            resets_at: resets_at
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&chrono::Utc)),
        })
    };
    let windows: Vec<QuotaWindow> = [
        window(
            QuotaWindowKind::FiveHour,
            usage.five_hour_pct,
            usage.five_hour_resets_at.as_deref(),
        ),
        window(
            QuotaWindowKind::Weekly,
            usage.weekly_pct,
            usage.weekly_resets_at.as_deref(),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    if windows.is_empty() {
        return VendorQuota::unavailable(vendor);
    }
    VendorQuota::available(vendor, plan, windows)
}

// ── HTTP layer (thin; every failure → None → `unavailable`) ─────────────────

fn codex_usage_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    account_id: &str,
) -> Option<reqwest::Request> {
    client
        .get(url)
        .bearer_auth(token)
        .header("ChatGPT-Account-Id", account_id)
        .build()
        .ok()
}

fn kimi_usage_request(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Option<reqwest::Request> {
    client.get(url).bearer_auth(token).build().ok()
}

/// Execute one probe request and parse the 2xx body. ANY transport or HTTP
/// failure — connect, timeout, 401/403/5xx — is `None` (→ `unavailable`):
/// the UI hides the zone and nothing is error-styled.
async fn execute_and_parse(
    client: &reqwest::Client,
    request: Option<reqwest::Request>,
    parse: impl FnOnce(&str) -> QuotaVerdict,
) -> Option<QuotaVerdict> {
    let resp = client.execute(request?).await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().await.ok()?;
    Some(parse(&body))
}

/// Probe one vendor. Never fails: every unhappy path is a `not_subscription`
/// / `unavailable` row.
async fn probe_one(
    client: &reqwest::Client,
    spec: &'static AgentProbeSpec,
    kind: QuotaProbeKind,
) -> VendorQuota {
    let vendor = spec.vendor;
    match kind {
        // The four-header billing coupling cannot be derived cleanly from
        // `~/.grok/` (refresh-keyed token, internal proxy base) — stubbed
        // unavailable by construction; see QuotaProbeKind's doc comment.
        QuotaProbeKind::GrokBillingUnavailable => VendorQuota::unavailable(vendor),
        // Answered from the local record in `quotas`, never probed.
        QuotaProbeKind::ClaudeRecordedUsage => VendorQuota::unavailable(vendor),
        QuotaProbeKind::CodexWhamUsage => {
            let Some((token, account_id)) =
                read_credential(codex_auth_path(), codex_chatgpt_from_auth)
            else {
                return VendorQuota::not_subscription(vendor);
            };
            let request = codex_usage_request(client, CODEX_WHAM_USAGE_URL, &token, &account_id);
            execute_and_parse(client, request, parse_codex_wham_usage)
                .await
                .map(|verdict| verdict.into_quota(vendor))
                .unwrap_or_else(|| VendorQuota::unavailable(vendor))
        }
        QuotaProbeKind::KimiManagedUsages => {
            let Some(token) =
                read_credential(kimi_credentials_path(), kimi_managed_token_from_credentials)
            else {
                return VendorQuota::not_subscription(vendor);
            };
            let request = kimi_usage_request(client, KIMI_USAGES_URL, &token);
            execute_and_parse(client, request, parse_kimi_usages)
                .await
                .map(|verdict| verdict.into_quota(vendor))
                .unwrap_or_else(|| VendorQuota::unavailable(vendor))
        }
    }
}

/// `GET /api/v1/vendors/quota` — the local machine's per-vendor quota rows.
/// Vendors with no probe surface (opencode/pi/dsh) are absent from the list
/// — the UI renders nothing for them, exactly like `not_subscription` /
/// `unavailable` rows.
#[utoipa::path(
    get,
    path = "/api/v1/vendors/quota",
    tag = "hosts",
    responses(
        (status = 200, description = "Per-vendor quota rows `{quotas: [{vendor, state, plan?, windows?}]}`", body = serde_json::Value),
        (status = 403, description = "Not the admin/owner"),
    ),
)]
pub(crate) async fn handle_vendor_quota(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
) -> Response {
    // Reads the daemon user's vendor credential files — owner-scoped.
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    let quotas = app.vendor_quotas.quotas(&app.paths.root).await;
    Json(serde_json::json!({ "quotas": quotas })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_core::vendor_quota::{QuotaState, QuotaWindowKind};

    // ── request builders (the thin wiring the parsers rely on) ───────────

    // ── claude: the recorded observation, no request (#244) ───────────

    fn recorded(
        usage: ccteam_harness::AccountUsage,
    ) -> ccteam_harness::usage_catalog::VendorAccountUsage {
        ccteam_harness::usage_catalog::VendorAccountUsage {
            observed_at: "2026-11-14T18:00:00Z".into(),
            source: "status card".into(),
            usage,
        }
    }

    #[test]
    fn claude_row_is_the_recorded_windows_with_the_credential_plan() {
        let quota = claude_quota_from(
            "claude",
            Some(Some("team".into())),
            Some(recorded(ccteam_harness::AccountUsage {
                five_hour_pct: Some(33),
                five_hour_resets_at: Some("2026-11-14T22:50:00+00:00".into()),
                weekly_pct: Some(62),
                weekly_resets_at: Some("2026-11-17T13:00:00+00:00".into()),
                ..Default::default()
            })),
        );
        assert_eq!(quota.state, QuotaState::Available);
        assert_eq!(quota.plan.as_deref(), Some("team"));
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].kind, QuotaWindowKind::FiveHour);
        assert_eq!(quota.windows[0].used_percent, 33.0);
        assert_eq!(
            quota.windows[0].resets_at.unwrap().to_rfc3339(),
            "2026-11-14T22:50:00+00:00"
        );
        assert_eq!(quota.windows[1].kind, QuotaWindowKind::Weekly);
        assert_eq!(quota.windows[1].used_percent, 62.0);
    }

    #[test]
    fn claude_row_without_a_login_or_a_record() {
        let weekly = ccteam_harness::AccountUsage {
            weekly_pct: Some(10),
            ..Default::default()
        };
        assert_eq!(
            claude_quota_from("claude", None, Some(recorded(weekly))).state,
            QuotaState::NotSubscription
        );
        assert_eq!(
            claude_quota_from("claude", Some(None), None).state,
            QuotaState::Unavailable
        );
    }

    #[test]
    fn codex_request_carries_bearer_and_account_id() {
        let client = reqwest::Client::new();
        let req = codex_usage_request(
            &client,
            "https://chatgpt.com/backend-api/wham/usage",
            "tok",
            "acc-1",
        )
        .unwrap();
        assert_eq!(req.headers().get("authorization").unwrap(), "Bearer tok");
        assert_eq!(req.headers().get("chatgpt-account-id").unwrap(), "acc-1");
    }

    // ── TTL cache ────────────────────────────────────────────────────────

    #[test]
    fn cache_serves_within_ttl_and_misses_past_it() {
        let cache = QuotaCache::new(Duration::from_secs(300));
        assert!(cache.fresh("claude").is_none());
        cache.store("claude", VendorQuota::unavailable("claude"));
        assert_eq!(
            cache.fresh("claude").unwrap().state,
            QuotaState::Unavailable
        );
        // A zero TTL never hits — every call re-probes.
        let zero = QuotaCache::new(Duration::ZERO);
        zero.store("codex", VendorQuota::not_subscription("codex"));
        assert!(zero.fresh("codex").is_none());
    }

    // ── execute_and_parse against a local mock (no real network) ─────────

    async fn spawn_mock(
        status: axum::http::StatusCode,
        body: &'static str,
    ) -> std::net::SocketAddr {
        let app = axum::Router::new().route(
            "/usage",
            axum::routing::get(move || async move { (status, body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        addr
    }

    #[tokio::test]
    async fn http_401_maps_to_unavailable() {
        let addr = spawn_mock(axum::http::StatusCode::UNAUTHORIZED, "{\"error\":\"nope\"}").await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{addr}/usage");
        let req = codex_usage_request(&client, &url, "tok", "acc-1");
        let verdict = execute_and_parse(&client, req, parse_codex_wham_usage).await;
        assert!(verdict.is_none(), "401 → None → unavailable row");
    }

    #[tokio::test]
    async fn timeout_maps_to_unavailable() {
        let app = axum::Router::new().route(
            "/usage",
            axum::routing::get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "never"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let url = format!("http://{addr}/usage");
        let req = kimi_usage_request(&client, &url, "tok");
        let verdict = execute_and_parse(&client, req, parse_kimi_usages).await;
        assert!(verdict.is_none(), "timeout → None → unavailable row");
    }

    #[tokio::test]
    async fn ok_body_parses_through_the_fixture_path() {
        let body = r#"{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":42,"reset_at":1789000000}}}"#;
        let addr = spawn_mock(axum::http::StatusCode::OK, body).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{addr}/usage");
        let req = codex_usage_request(&client, &url, "tok", "acc-1");
        let verdict = execute_and_parse(&client, req, parse_codex_wham_usage)
            .await
            .unwrap();
        let QuotaVerdict::Available { windows, .. } = verdict else {
            panic!("expected available");
        };
        assert_eq!(windows[0].kind, QuotaWindowKind::FiveHour);
        assert_eq!(windows[0].used_percent, 42.0);
    }
}
