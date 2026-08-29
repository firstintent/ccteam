//! v0.8.9 Phase 2 — ccteam-hub read + install backend (the curated plugin
//! marketplace).
//!
//! `ccteam-hub` (`github.com/firstintent/ccteam-hub`) is a curated catalog of
//! installable **plugins** — agents + skills (workflows deferred). ccteam
//! reads its `index.json` over HTTPS (github-raw) plus a local cache under
//! `~/.ccteam/hub-cache/` (track-upstream: the index stores per-plugin
//! `upstream` URLs, not vendored bodies), and installs a plugin's content into
//! its appropriate target: project-local `.claude/agents/<id>.md` for an
//! agent, or the user-level ccteam skill library for a skill (a single
//! `SKILL.md`, or a whole dir via its `manifest`). `ccteam-web` + the CLI call
//! this module.
//!
//! ## Why here (not `ccteam-core`)
//!
//! The raw-content base URL constant ([`ccteam_core::HUB_RAW_BASE`]) and the
//! cache *directory* path ([`ccteam_core::CcteamPaths::hub_cache_dir`]) live in
//! the primitives leaf, but the **network fetch + sha256 integrity check +
//! install** live here because `ccteam-core` is the topology leaf
//! (`core -> harness -> cost`) and must not take an async HTTP + sha2
//! dependency. `ccteam-im` already depends on `reqwest`, so this is at home
//! next to [`crate::onboarding`] and applies a hardened-fetch posture (30s
//! timeout, redirects refused, 1 MiB cap, Content-Length pre-check + bounded
//! streaming read).
//!
//! ## Test seam
//!
//! [`fetch_index`] / [`load_catalog`] take a `base` parameter (the hub
//! `index.json` host; live wrappers pass [`ccteam_core::HUB_RAW_BASE`],
//! overridable via [`HUB_BASE_ENV`]). Plugin bodies are fetched from each
//! entry's own `upstream` URL (track-upstream model), gated by a host
//! allowlist (github raw + loopback). Tests point both at an in-process
//! loopback fake hub so `cargo test` never touches github.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Upper bound on a fetched body (catalog `index.json` or plugin body). Hub
/// agents are a few KiB; 1 MiB is huge headroom while still capping a
/// misconfigured / hostile endpoint that would otherwise stream an unbounded
/// body into memory.
pub const MAX_HUB_BODY_BYTES: usize = 1024 * 1024;

/// Env var overriding the ccteam-hub raw-content base, for deterministic
/// tests (an in-process fake hub stands in for github). Mirrors the
/// `base`-override test seam, exposed as an env knob so non-test callers
/// (CLI / web) can also repoint the hub without a code change.
pub const HUB_BASE_ENV: &str = "CCTEAM_HUB_BASE";

/// Resolve the active hub raw-content base: [`HUB_BASE_ENV`] when set,
/// otherwise [`ccteam_core::HUB_RAW_BASE`] (`main`).
pub fn hub_base() -> String {
    hub_base_from(std::env::var(HUB_BASE_ENV).ok())
}

/// Pure rule behind [`hub_base`]. Split out so its test does not have to
/// `remove_var`/`set_var` the override under every sibling test in this
/// binary (CLI-ENVTEST-1).
fn hub_base_from(raw: Option<String>) -> String {
    raw.unwrap_or_else(|| ccteam_core::HUB_RAW_BASE.to_string())
}

/// The whole `index.json` catalog. Top-level metadata + the plugin list.
///
/// Schema (live ccteam-hub):
/// `{ version, name, description, generated_at, plugins: [HubPlugin] }`.
/// Unknown top-level keys are tolerated (forward-compat) — only `plugins` is
/// load-bearing for the backend; the metadata is surfaced to the UI as-is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubIndex {
    /// Schema version of the index (currently `1`).
    #[serde(default)]
    pub version: u32,
    /// Human catalog name.
    #[serde(default)]
    pub name: String,
    /// One-line catalog description.
    #[serde(default)]
    pub description: String,
    /// RFC3339 timestamp the index was generated (best-effort; free-form).
    #[serde(default)]
    pub generated_at: String,
    /// The installable plugins.
    #[serde(default)]
    pub plugins: Vec<HubPlugin>,
}

impl HubIndex {
    /// Find a plugin by its exact `id`. `None` when no entry matches.
    pub fn find(&self, id: &str) -> Option<&HubPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// Browse ordering: official ccteam first-party plugins (`source ==
    /// "ccteam"`) sort to the top — the featured / recommended slot — while
    /// every other source keeps its existing (id-sorted) order. Stable, so the
    /// non-ccteam tail is untouched. Applied at parse time so every consumer
    /// (web marketplace browse, CLI `role search`) inherits one ordering.
    pub fn sort_ccteam_first(&mut self) {
        self.plugins.sort_by_key(|p| p.source != "ccteam");
    }
}

/// Advisory hub-wide model catalog (`models.json`, schema
/// `ccteam.models/v1`). It is never consulted by session spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubModelsCatalog {
    /// Exact schema discriminator (`ccteam.models/v1`).
    pub schema: String,
    /// Hub maintainer's RFC3339 update timestamp.
    pub updated_at: String,
    /// Vendor wire name to its advisory catalog.
    pub vendors: std::collections::BTreeMap<String, HubVendorModels>,
}

/// One vendor block in [`HubModelsCatalog`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubVendorModels {
    /// Hub-advertised default model id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Advisory model rows in hub order.
    #[serde(default)]
    pub models: Vec<HubModel>,
}

/// One advisory hub model row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubModel {
    /// Opaque vendor model id.
    pub id: String,
    /// Human-readable vendor/community label, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Small advisory alias set maintained by the hub.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Confirmed context window, when the hub has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// Verified hub snapshot handed to the status panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubModelsSnapshot {
    /// Parsed and schema-validated catalog.
    pub catalog: HubModelsCatalog,
    /// SHA-256 of the exact `models.json` bytes.
    pub revision: String,
    /// True when a verified cache is being used after refresh failed or its
    /// TTL expired.
    pub stale: bool,
}

/// Honest advisory source state. The upstream file is optional, so every
/// fetch/cache failure degrades to `Unavailable` instead of failing `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubModelsState {
    /// A verified fresh or stale snapshot.
    Available(HubModelsSnapshot),
    /// No verified source is available.
    Unavailable,
}

/// Refresh cadence for `models.json`. A fresh verified cache avoids network;
/// an expired one is returned as stale if refresh cannot complete.
pub const HUB_MODELS_TTL_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HubModelsCacheMeta {
    sha256: String,
    fetched_at: String,
}

/// One file of a multi-file skill (PRD §二): a path relative to the skill
/// dir + the sha256 of its body. The engine derives the file's fetch URL from
/// the plugin's `upstream` dir + `relpath`, verifies the sha, and writes it
/// under the user-level skill library's `<id>/<relpath>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Path relative to the skill dir (e.g. `SKILL.md`, `scripts/run.sh`).
    pub relpath: String,
    /// sha256 hex of this file's body (per-file integrity gate).
    pub content_sha: String,
}

/// A vendor-native plugin marketplace pointer carried by a `type:"plugin"`
/// hub entry. ccteam never copies the plugin body; it hands these two facts
/// to Claude Code's own installer via `.claude/settings.local.json` (see
/// [`ccteam_core::enable_marketplace_plugin`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketplaceRef {
    /// The marketplace's DECLARED name (its `.claude-plugin/marketplace.json`
    /// `name`). Becomes the `extraKnownMarketplaces` key + the `@<marketplace>`
    /// half of the `enabledPlugins` key.
    pub name: String,
    /// The vendor `MarketplaceSource` object, stored verbatim and passed
    /// through unmodified (e.g. `{"source":"github","repo":"owner/repo"}`).
    /// Kept as raw JSON so the full vendor union (github/url/git/npm/…) is
    /// honoured without re-modeling it here.
    pub source: serde_json::Value,
}

/// One installable plugin entry in the hub index (track-upstream schema).
///
/// `{ id, type, name, description, upstream, content_sha, source, license,
///   tags[], manifest? }`. `type` is `"agent" | "skill" | "workflow" |
///   "plugin"`; `upstream` is the raw-fetchable URL of the body
/// (`raw.githubusercontent.com/<owner>/<repo>/<sha>/<path>` for an external
/// source, or the hub's own raw tree for first-party content). `content_sha`
/// is the sha256 of that body. A multi-file skill additionally carries a
/// `manifest` of every file (relpath + sha, incl. `SKILL.md`). A `plugin`
/// entry carries `marketplace` + `plugin_id` instead of a body (delegated
/// vendor install — no `upstream` / `content_sha` / `manifest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubPlugin {
    /// Globally-unique plugin id (the install key, and default install stem).
    pub id: String,
    /// `"agent"`, `"skill"`, `"workflow"`, or `"plugin"`. Serde wire name
    /// `type`.
    #[serde(rename = "type")]
    pub type_: String,
    /// Human-friendly name.
    #[serde(default)]
    pub name: String,
    /// One-line description (for the catalog browse view).
    #[serde(default)]
    pub description: String,
    /// Raw-fetchable URL of the body @sha (the install fetch target). For a
    /// multi-file skill this points at `SKILL.md`; the other files are derived
    /// from its dir + each `manifest` `relpath`.
    #[serde(default)]
    pub upstream: String,
    /// sha256 hex of the body at `upstream`. The installer verifies the
    /// fetched body against this before writing (integrity / anti-tamper).
    /// Absent for a `plugin` entry (nothing is fetched / copied).
    #[serde(default)]
    pub content_sha: String,
    /// Provenance: which source ccteam-hub tracked this plugin from
    /// (`"ccteam"` first-party, `"agency-agents"`, …).
    #[serde(default)]
    pub source: String,
    /// SPDX-ish license string (free-form).
    #[serde(default)]
    pub license: String,
    /// Browse / filter tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Multi-file skill manifest (every file incl. `SKILL.md`). Absent for a
    /// single-file agent / SKILL.md-only skill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Vec<ManifestEntry>>,
    /// `type:"plugin"` only — the vendor marketplace pointer ccteam delegates
    /// the install to. Absent for content (agent/skill/workflow) entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<MarketplaceRef>,
    /// `type:"plugin"` only — the plugin's name within its marketplace (the
    /// `<plugin>` half of `enabledPlugins["<plugin>@<marketplace>"]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
}

/// Outcome of a successful [`install_plugin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    /// The plugin id that was installed.
    pub id: String,
    /// The plugin `type` (`"agent"` / `"skill"`).
    pub type_: String,
    /// Absolute path of the file written (project agent definition, global
    /// library `SKILL.md`, or delegated plugin settings).
    pub path: PathBuf,
    /// `true` when an existing file at the target was overwritten (only
    /// possible with `force`).
    pub overwrote: bool,
}

/// Whether a hub plugin is already present at its canonical target, computed
/// on-the-fly from disk vs. the index's `content_sha` (no sidecar file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstalledStatus {
    /// No file at the plugin's target path.
    NotInstalled,
    /// File present and its sha256 equals the index `content_sha`.
    Installed,
    /// File present but its sha256 differs — the hub has a newer body.
    UpdateAvailable,
}

/// Errors from [`fetch_index`] / [`load_catalog`] / [`fetch_plugin_body`] /
/// [`install_plugin`].
#[derive(Debug, Error)]
pub enum HubError {
    /// No plugin with the given id in the (fetched / cached) index.
    #[error("no plugin `{0}` in the ccteam-hub catalog")]
    UnknownId(String),
    /// A file already exists at the install target and `force` was not set.
    /// The `String` is the install stem.
    #[error("plugin `{0}` is already installed — pass force to overwrite")]
    Exists(String),
    /// The underlying `reqwest` call failed (DNS / TLS / connect / read).
    #[error("hub fetch failed for `{what}`: {source}")]
    Http {
        /// What was being fetched (`"index.json"` or a plugin id).
        what: String,
        /// The underlying transport error.
        #[source]
        source: reqwest::Error,
    },
    /// The hub returned a non-success HTTP status (e.g. 404). Carries the
    /// status + URL for an honest message. A non-followed redirect (3xx)
    /// surfaces here too (redirects are refused).
    #[error("hub fetch for `{what}` returned HTTP {status} ({url})")]
    BadStatus {
        /// What was being fetched.
        what: String,
        /// The HTTP status code.
        status: u16,
        /// The URL that was fetched.
        url: String,
    },
    /// The fetch URL's host is not a registered source host (only github raw
    /// and loopback are permitted). Refused before any network I/O — the
    /// host-allowlist gate.
    #[error("fetch for `{what}` refused: host `{host}` is not an allowed source host ({url})")]
    HostNotAllowed {
        /// What was being fetched.
        what: String,
        /// The disallowed host.
        host: String,
        /// The URL that was refused.
        url: String,
    },
    /// The fetched / cached index failed to parse as the expected schema.
    #[error("hub index is malformed: {0}")]
    BadIndex(String),
    /// The optional `models.json` exists but is not valid
    /// `ccteam.models/v1`.
    #[error("hub models catalog is malformed: {0}")]
    BadModels(String),
    /// Cached `models.json` bytes do not match the sidecar SHA written when
    /// they were fetched; the cache is refused rather than displayed.
    #[error("hub models cache integrity check failed: expected sha256 {expected}, got {actual}")]
    ModelsCacheShaMismatch {
        /// SHA stored alongside the last successful fetch.
        expected: String,
        /// SHA computed over the cached bytes.
        actual: String,
    },
    /// The fetched body was empty — refuse to install a content-less file.
    #[error("fetched plugin `{0}` is empty — refusing to install a content-less file")]
    EmptyBody(String),
    /// The fetched body exceeded [`MAX_HUB_BODY_BYTES`].
    #[error("fetched `{what}` exceeds the {max} byte cap — refusing")]
    TooLarge {
        /// What was being fetched.
        what: String,
        /// The byte cap that was exceeded.
        max: usize,
    },
    /// The fetched body's sha256 did not match the index `content_sha`. The
    /// integrity gate: a tampered / MITM'd body is refused, not written.
    #[error("integrity check failed for plugin `{id}`: expected sha256 {expected}, got {actual}")]
    ShaMismatch {
        /// The plugin id.
        id: String,
        /// The `content_sha` from the index.
        expected: String,
        /// The sha256 actually computed over the fetched body.
        actual: String,
    },
    /// The plugin `type` is recognised but not yet installable (workflow).
    #[error("plugin type `{0}` is not yet supported for install")]
    UnsupportedType(String),
    /// The install stem (from the plugin id or a caller `--as` override)
    /// sanitizes to an invalid/empty filename — a client error (bad input),
    /// distinct from a disk write failure.
    #[error("{0}")]
    BadStem(String),
    /// A local disk write / cache read/write failed (our side). Wraps the
    /// underlying message.
    #[error("{0}")]
    Write(String),
    /// A `type:"plugin"` entry is missing its `marketplace` / `plugin_id`
    /// pointer (a malformed catalog row), so it can't be delegated to the
    /// vendor installer.
    #[error("plugin `{0}` is missing its marketplace pointer (marketplace + plugin_id)")]
    InvalidPlugin(String),
}

/// Build the hardened reqwest client used for every hub fetch: 30s timeout
/// and **redirects refused** (`Policy::none()`). A 3xx to an arbitrary host
/// (open-redirect / typo-squat) becomes a non-success status the caller
/// rejects as [`HubError::BadStatus`] rather than silently fetching attacker
/// content.
fn hardened_client(what: &str, url: &str) -> Result<reqwest::Client, HubError> {
    let builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none());
    let builder = if url_is_loopback(url) {
        builder.no_proxy()
    } else {
        builder
    };
    builder.build().map_err(|source| HubError::Http {
        what: what.to_string(),
        source,
    })
}

fn url_is_loopback(url: &str) -> bool {
    let Some(host) = reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Hosts ccteam will fetch plugin/index content from — the curation gate. A
/// body is only pulled from a registered source's host (today every source,
/// incl. the hub's own first-party tree, is github raw) at an immutable
/// pinned-sha path. Loopback is additionally allowed for the in-process test
/// hub. A future GitLab / self-hosted source extends this list.
const ALLOWED_FETCH_HOSTS: &[&str] = &["raw.githubusercontent.com"];

/// True when `url`'s host is on the fetch allowlist (or loopback, for tests).
fn host_is_allowed(url: &str) -> bool {
    if url_is_loopback(url) {
        return true;
    }
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .map(|host| {
            ALLOWED_FETCH_HOSTS
                .iter()
                .any(|h| host.eq_ignore_ascii_case(h))
        })
        .unwrap_or(false)
}

/// GET `url` with the hardened client and return the body bytes, enforcing
/// the status / size guards. `what` labels the fetch for error messages.
async fn fetch_bytes(client: &reqwest::Client, url: &str, what: &str) -> Result<Vec<u8>, HubError> {
    // Host-allowlist gate (PRD §四): refuse any host that isn't a registered
    // source host before a single byte leaves the process.
    if !host_is_allowed(url) {
        let host = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        return Err(HubError::HostNotAllowed {
            what: what.to_string(),
            host,
            url: url.to_string(),
        });
    }
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|source| HubError::Http {
            what: what.to_string(),
            source,
        })?;
    if !resp.status().is_success() {
        return Err(HubError::BadStatus {
            what: what.to_string(),
            status: resp.status().as_u16(),
            url: url.to_string(),
        });
    }
    // Bounded read: reject before reading a byte if Content-Length is over the
    // cap; otherwise accumulate and bail the instant we cross it (covers a
    // lying / absent Content-Length).
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_HUB_BODY_BYTES)
    {
        return Err(HubError::TooLarge {
            what: what.to_string(),
            max: MAX_HUB_BODY_BYTES,
        });
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|source| HubError::Http {
        what: what.to_string(),
        source,
    })? {
        if buf.len() + chunk.len() > MAX_HUB_BODY_BYTES {
            return Err(HubError::TooLarge {
                what: what.to_string(),
                max: MAX_HUB_BODY_BYTES,
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Fetch + parse the hub `index.json` from `{base}/index.json`. No cache —
/// always hits the network. [`load_catalog`] wraps this with the on-disk
/// cache.
pub async fn fetch_index(base: &str) -> Result<HubIndex, HubError> {
    let url = ccteam_core::catalog_raw_url(base, "index.json");
    let client = hardened_client("index.json", &url)?;
    let bytes = fetch_bytes(&client, &url, "index.json").await?;
    parse_index(&bytes)
}

/// Parse index bytes into a [`HubIndex`], mapping a parse failure to
/// [`HubError::BadIndex`].
fn parse_index(bytes: &[u8]) -> Result<HubIndex, HubError> {
    let mut index: HubIndex =
        serde_json::from_slice(bytes).map_err(|e| HubError::BadIndex(format!("{e}")))?;
    // Feature official ccteam plugins at the top of every browse/list.
    index.sort_ccteam_first();
    Ok(index)
}

/// Load the hub catalog with a local cache under `~/.ccteam/hub-cache/`.
///
/// - `refresh == true` (or no cache file yet): fetch from `{base}/index.json`,
///   write it to `<hub_cache_dir>/index.json` atomically (tmp + rename), and
///   return the parsed index.
/// - `refresh == false` and a cache file exists: read + parse the cache (no
///   network — offline browse). A corrupt cache surfaces as
///   [`HubError::BadIndex`] (the caller can retry with `refresh = true`).
///
/// On a refresh, the parse happens **before** the cache write, so a malformed
/// fetched index never clobbers a good cache.
pub async fn load_catalog(
    base: &str,
    paths: &ccteam_core::CcteamPaths,
    refresh: bool,
) -> Result<HubIndex, HubError> {
    let cache_path = paths.hub_cache_dir().join("index.json");
    if !refresh && cache_path.exists() {
        let bytes = std::fs::read(&cache_path).map_err(|e| {
            HubError::Write(format!("read hub cache {}: {e}", cache_path.display()))
        })?;
        return parse_index(&bytes);
    }
    // Refresh path: fetch + parse, then persist atomically.
    let url = ccteam_core::catalog_raw_url(base, "index.json");
    let client = hardened_client("index.json", &url)?;
    let bytes = fetch_bytes(&client, &url, "index.json").await?;
    let index = parse_index(&bytes)?;
    write_cache_atomic(&cache_path, &bytes)?;
    Ok(index)
}

/// Parse and validate `models.json`. Unknown fields are tolerated for forward
/// compatibility, but the schema tag and every vendor/model id are required.
fn parse_models_catalog(bytes: &[u8]) -> Result<HubModelsCatalog, HubError> {
    let catalog: HubModelsCatalog =
        serde_json::from_slice(bytes).map_err(|err| HubError::BadModels(err.to_string()))?;
    if catalog.schema != "ccteam.models/v1" {
        return Err(HubError::BadModels(format!(
            "unsupported schema `{}`",
            catalog.schema
        )));
    }
    for (vendor, entry) in &catalog.vendors {
        if vendor.trim().is_empty() {
            return Err(HubError::BadModels("vendor key is empty".to_string()));
        }
        if entry.models.iter().any(|model| model.id.trim().is_empty()) {
            return Err(HubError::BadModels(format!(
                "vendor `{vendor}` contains an empty model id"
            )));
        }
    }
    Ok(catalog)
}

fn models_cache_paths(paths: &ccteam_core::CcteamPaths) -> (PathBuf, PathBuf) {
    let dir = paths.hub_cache_dir();
    (dir.join("models.json"), dir.join("models.meta.json"))
}

fn read_models_cache(
    paths: &ccteam_core::CcteamPaths,
) -> Result<Option<(HubModelsSnapshot, chrono::DateTime<chrono::Utc>)>, HubError> {
    let (body_path, meta_path) = models_cache_paths(paths);
    if !body_path.exists() && !meta_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&body_path).map_err(|err| {
        HubError::Write(format!(
            "read hub models cache {}: {err}",
            body_path.display()
        ))
    })?;
    let meta_bytes = std::fs::read(&meta_path).map_err(|err| {
        HubError::Write(format!(
            "read hub models cache metadata {}: {err}",
            meta_path.display()
        ))
    })?;
    let meta: HubModelsCacheMeta = serde_json::from_slice(&meta_bytes)
        .map_err(|err| HubError::BadModels(format!("cache metadata: {err}")))?;
    let actual = sha256_hex(&bytes);
    if !sha_eq(&actual, &meta.sha256) {
        return Err(HubError::ModelsCacheShaMismatch {
            expected: meta.sha256,
            actual,
        });
    }
    let fetched_at = chrono::DateTime::parse_from_rfc3339(&meta.fetched_at)
        .map_err(|err| HubError::BadModels(format!("cache fetched_at: {err}")))?
        .with_timezone(&chrono::Utc);
    Ok(Some((
        HubModelsSnapshot {
            catalog: parse_models_catalog(&bytes)?,
            revision: actual,
            stale: false,
        },
        fetched_at,
    )))
}

/// Load advisory hub `models.json` with a verified TTL cache.
///
/// A fresh cache is returned without network. Otherwise `{base}/models.json`
/// is fetched through the same host allowlist, redirect refusal, timeout, and
/// byte cap as `index.json`; valid bytes replace the cache atomically. A 404,
/// offline host, malformed body, or cache integrity failure never fails the
/// caller: a previously verified cache is returned with `stale=true`, else the
/// state is honestly [`HubModelsState::Unavailable`].
pub async fn load_models_catalog(
    base: &str,
    paths: &ccteam_core::CcteamPaths,
    force_refresh: bool,
) -> HubModelsState {
    let now = chrono::Utc::now();
    let cached = read_models_cache(paths).ok().flatten();
    if !force_refresh {
        if let Some((snapshot, fetched_at)) = &cached {
            let age = now.signed_duration_since(*fetched_at).num_seconds();
            if (0..=HUB_MODELS_TTL_SECS).contains(&age) {
                return HubModelsState::Available(snapshot.clone());
            }
        }
    }

    let url = ccteam_core::catalog_raw_url(base, "models.json");
    let fetched = match hardened_client("models.json", &url) {
        Ok(client) => fetch_bytes(&client, &url, "models.json").await,
        Err(err) => Err(err),
    };
    if let Ok(bytes) = fetched {
        if let Ok(catalog) = parse_models_catalog(&bytes) {
            let revision = sha256_hex(&bytes);
            let (body_path, meta_path) = models_cache_paths(paths);
            let meta = HubModelsCacheMeta {
                sha256: revision.clone(),
                fetched_at: now.to_rfc3339(),
            };
            // Cache writes decorate a successful advisory fetch. A local disk
            // failure must not turn a usable in-memory catalog into unavailable.
            if write_cache_atomic(&body_path, &bytes).is_ok() {
                if let Ok(meta_bytes) = serde_json::to_vec_pretty(&meta) {
                    let _ = write_cache_atomic(&meta_path, &meta_bytes);
                }
            }
            return HubModelsState::Available(HubModelsSnapshot {
                catalog,
                revision,
                stale: false,
            });
        }
    }

    match cached {
        Some((mut snapshot, _)) => {
            snapshot.stale = true;
            HubModelsState::Available(snapshot)
        }
        None => HubModelsState::Unavailable,
    }
}

/// Atomic tmp + rename write of the index cache (mirrors
/// `ccteam_core::config::save`): ensure the parent dir, write `.tmp`, rename
/// over the final path so a crash mid-write can't leave a torn cache.
fn write_cache_atomic(cache_path: &Path, bytes: &[u8]) -> Result<(), HubError> {
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            HubError::Write(format!("create hub cache dir {}: {e}", parent.display()))
        })?;
    }
    let tmp = cache_path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|e| HubError::Write(format!("write hub cache tmp {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, cache_path).map_err(|e| {
        HubError::Write(format!(
            "rename hub cache {} -> {}: {e}",
            tmp.display(),
            cache_path.display()
        ))
    })?;
    Ok(())
}

/// Fetch the bytes at `url` with the hardened client, **verify
/// `sha256 == expected_sha`** (the integrity gate), and return the body as
/// non-empty UTF-8. `what` labels the fetch (plugin id or a manifest relpath)
/// for error messages. Shared by [`fetch_plugin_body`] and the multi-file
/// manifest install so every fetched byte passes the same host-allowlist +
/// size + sha + UTF-8 gauntlet.
async fn fetch_text_verified(
    url: &str,
    expected_sha: &str,
    what: &str,
) -> Result<String, HubError> {
    let client = hardened_client(what, url)?;
    let buf = fetch_bytes(&client, url, what).await?;
    let actual = sha256_hex(&buf);
    if !sha_eq(&actual, expected_sha) {
        return Err(HubError::ShaMismatch {
            id: what.to_string(),
            expected: expected_sha.to_string(),
            actual,
        });
    }
    let body = String::from_utf8(buf)
        .map_err(|_| HubError::Write(format!("fetched `{what}` is not valid UTF-8")))?;
    if body.trim().is_empty() {
        return Err(HubError::EmptyBody(what.to_string()));
    }
    Ok(body)
}

/// The directory portion of an `upstream` URL (everything before the last
/// `/`). A multi-file skill's per-file URLs are `dir + "/" + relpath`.
fn upstream_dir(upstream: &str) -> Result<&str, HubError> {
    upstream
        .rfind('/')
        .map(|i| &upstream[..i])
        .ok_or_else(|| HubError::Write(format!("malformed upstream URL: {upstream}")))
}

/// Fetch a plugin's body from its `upstream` URL and **verify
/// `sha256(body) == plugin.content_sha`** before returning it (the integrity
/// gate). The body must be non-empty UTF-8 within the size cap, fetched from
/// an allowlisted host. For a multi-file skill this returns the `SKILL.md`
/// body; the sibling files are fetched by [`install_plugin`] via the manifest.
pub async fn fetch_plugin_body(plugin: &HubPlugin) -> Result<String, HubError> {
    // Vendor-native plugin: there is no body to copy. The "preview" honestly
    // describes the delegated install (which marketplace/source + plugin), so
    // the reviewer sees exactly what enabling will hand to Claude Code.
    if plugin.type_ == "plugin" {
        let market = plugin
            .marketplace
            .as_ref()
            .ok_or_else(|| HubError::InvalidPlugin(plugin.id.clone()))?;
        let plugin_name = plugin.plugin_id.as_deref().unwrap_or(&plugin.id);
        let source = serde_json::to_string_pretty(&market.source)
            .unwrap_or_else(|_| market.source.to_string());
        return Ok(format!(
            "Vendor-native Claude Code plugin (delegated install — ccteam copies/executes \
             nothing).\n\nEnabling this writes two keys into the project's \
             .claude/settings.local.json:\n  extraKnownMarketplaces[\"{name}\"].source = \
             {source}\n  enabledPlugins[\"{plugin_name}@{name}\"] = true\n\nClaude Code fetches \
             and installs the plugin itself on its next launch in this project.",
            name = market.name,
        ));
    }
    if plugin.upstream.trim().is_empty() {
        return Err(HubError::Write(format!(
            "plugin `{}` has no upstream URL to fetch",
            plugin.id
        )));
    }
    fetch_text_verified(&plugin.upstream, &plugin.content_sha, &plugin.id).await
}

/// Install a hub plugin into its type-specific target. Both roots are explicit
/// so callers and tests control all filesystem effects without ambient env
/// reads.
///
/// - **agent**: sanitize the install stem, refuse to clobber without `force`,
///   fetch + verify, then write `.claude/agents/<stem>.md` in `project_dir`.
/// - **single-file skill**: sanitize with the nested library-id rules, refuse
///   an existing `<library_root>/<stem>/` without `force`, fetch + verify, then
///   write `<library_root>/<stem>/SKILL.md`.
/// - **multi-file skill** (`manifest` present): the install target is the
///   library skill dir; refuse to clobber it unless `force`;
///   fetch + verify EVERY manifest file (URL = the `upstream` dir + each
///   `relpath`) BEFORE writing any — so a mid-list integrity failure leaves
///   nothing partial — then write each under `<library_root>/<stem>/<relpath>`.
/// - **workflow / unknown type**: [`HubError::UnsupportedType`] (deferred).
///
/// `InstallResult.path` is the primary file (`SKILL.md` / the agent `.md`).
pub async fn install_plugin(
    project_dir: &Path,
    library_root: &Path,
    plugin: &HubPlugin,
    target_stem: Option<&str>,
    force: bool,
) -> Result<InstallResult, HubError> {
    // Vendor-native plugin: delegated install (no body fetch / copy / exec) —
    // ccteam writes the marketplace pointer + enable flag into
    // settings.local.json and Claude Code does the rest on next launch.
    if plugin.type_ == "plugin" {
        return install_marketplace_plugin(project_dir, plugin);
    }

    let raw_stem = target_stem.unwrap_or(&plugin.id);
    let stem = if plugin.type_ == "skill" {
        ccteam_core::sanitize_skill_library_id(raw_stem)
    } else {
        ccteam_core::sanitize_role_stem(raw_stem)
    }
    .map_err(|e| HubError::BadStem(format!("{e:#}")))?;

    // Multi-file skill (manifest present): a directory install.
    if let Some(manifest) = plugin.manifest.as_ref().filter(|m| !m.is_empty()) {
        if plugin.type_ != "skill" {
            // A manifest only makes sense for a skill directory.
            return Err(HubError::UnsupportedType(plugin.type_.clone()));
        }
        let skill_dir = library_root.join(&stem);
        let exists = skill_dir.exists();
        if exists && !force {
            return Err(HubError::Exists(stem));
        }
        let mut relpaths = std::collections::HashSet::with_capacity(manifest.len());
        for entry in manifest {
            ccteam_core::validate_skill_library_file_relpath(&entry.relpath)
                .map_err(|e| HubError::Write(format!("{e:#}")))?;
            if !relpaths.insert(entry.relpath.as_str()) {
                return Err(HubError::Write(format!(
                    "skill manifest contains duplicate relpath `{}`",
                    entry.relpath
                )));
            }
        }
        let base_url = upstream_dir(&plugin.upstream)?;
        // Pass 1: fetch + verify EVERY file (no writes → atomic on failure).
        let mut files: Vec<(String, String)> = Vec::with_capacity(manifest.len());
        for entry in manifest {
            let url = format!("{base_url}/{}", entry.relpath);
            let body = fetch_text_verified(&url, &entry.content_sha, &entry.relpath).await?;
            files.push((entry.relpath.clone(), body));
        }
        // Pass 2: write each under the global library skill dir.
        let mut primary: Option<PathBuf> = None;
        for (relpath, body) in &files {
            let written = ccteam_core::write_library_skill_file(
                library_root,
                &stem,
                relpath,
                body.as_bytes(),
                force,
            )
            .map_err(|e| HubError::Write(format!("{e:#}")))?;
            if relpath == "SKILL.md" {
                primary = Some(written);
            }
        }
        let path = primary.unwrap_or_else(|| skill_dir.join("SKILL.md"));
        return Ok(InstallResult {
            id: plugin.id.clone(),
            type_: plugin.type_.clone(),
            path,
            overwrote: exists,
        });
    }

    // Single-file agent / SKILL.md-only skill. Resolve the target path by type
    // up front so the clobber check and the write agree, and an unsupported
    // type fails before any network I/O.
    let dest = match plugin.type_.as_str() {
        "agent" => ccteam_core::agent_md_path(project_dir, &stem),
        "skill" => library_root.join(&stem).join("SKILL.md"),
        "workflow" => return Err(HubError::UnsupportedType(plugin.type_.clone())),
        other => return Err(HubError::UnsupportedType(other.to_string())),
    };
    let exists = if plugin.type_ == "skill" {
        library_root.join(&stem).exists()
    } else {
        dest.exists()
    };
    if exists && !force {
        return Err(HubError::Exists(stem));
    }
    let body = fetch_plugin_body(plugin).await?;
    let path = match plugin.type_.as_str() {
        "agent" => ccteam_core::write_role(project_dir, &stem, &body),
        "skill" => ccteam_core::write_library_skill(library_root, &stem, &body, force),
        // The match above already returned for every other type.
        _ => unreachable!("unsupported type handled before fetch"),
    }
    .map_err(|e| HubError::Write(format!("{e:#}")))?;

    Ok(InstallResult {
        id: plugin.id.clone(),
        type_: plugin.type_.clone(),
        path,
        overwrote: exists,
    })
}

/// Delegated install of a `type:"plugin"` entry — writes the marketplace
/// pointer + enable flag into `<project>/.claude/settings.local.json` and
/// returns; Claude Code does the actual fetch / native-dep / install on its
/// next launch in the project. ccteam fetches nothing and executes nothing,
/// so there is no body, no sha gate, and no clobber check (re-enabling is an
/// idempotent settings rewrite — `force` is irrelevant and ignored).
///
/// `InstallResult.path` is the `settings.local.json` that was written;
/// `overwrote` is always `false` (config merge, never a content overwrite).
fn install_marketplace_plugin(
    project_dir: &Path,
    plugin: &HubPlugin,
) -> Result<InstallResult, HubError> {
    let market = plugin
        .marketplace
        .as_ref()
        .ok_or_else(|| HubError::InvalidPlugin(plugin.id.clone()))?;
    let plugin_name = plugin
        .plugin_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| HubError::InvalidPlugin(plugin.id.clone()))?;

    ccteam_core::enable_marketplace_plugin(project_dir, &market.name, &market.source, plugin_name)
        .map_err(|e| HubError::Write(format!("{e:#}")))?;
    let path = project_dir.join(".claude").join("settings.local.json");

    Ok(InstallResult {
        id: plugin.id.clone(),
        type_: plugin.type_.clone(),
        path,
        overwrote: false,
    })
}

/// Compute the on-the-fly installed status of `plugin` at its type-specific
/// target — no sidecar file. Keyed on `plugin.id` (the no-override stem):
///
/// - target absent → [`InstalledStatus::NotInstalled`],
/// - present + sha matches → [`InstalledStatus::Installed`],
/// - present + sha differs → [`InstalledStatus::UpdateAvailable`].
///
/// For a **multi-file skill** (`manifest` present) the comparison is over the
/// whole dir: every manifest file present + sha-matching → `Installed`; any
/// missing file → `NotInstalled`; a present but stale file →
/// `UpdateAvailable`.
///
/// An unreadable file (race / permissions) or an unsupported / unsanitizable
/// type is treated as `NotInstalled` (best-effort decoration of the catalog;
/// never errors). The web layer uses this to badge each catalog row.
pub fn installed_status(
    project_dir: &Path,
    library_root: &Path,
    plugin: &HubPlugin,
) -> InstalledStatus {
    // Vendor-native plugin: "installed" == enabled in settings.local.json.
    // There is no `UpdateAvailable` (Claude tracks the marketplace's live ref,
    // not a hub-pinned sha) — the status is binary.
    if plugin.type_ == "plugin" {
        let enabled = matches!(
            (plugin.marketplace.as_ref(), plugin.plugin_id.as_deref()),
            (Some(m), Some(p))
                if ccteam_core::marketplace_plugin_enabled(project_dir, p, &m.name)
        );
        return if enabled {
            InstalledStatus::Installed
        } else {
            InstalledStatus::NotInstalled
        };
    }

    // Default stem == sanitized plugin id (the no-override install path).
    let stem = if plugin.type_ == "skill" {
        ccteam_core::sanitize_skill_library_id(&plugin.id)
    } else {
        ccteam_core::sanitize_role_stem(&plugin.id)
    };
    let Ok(stem) = stem else {
        return InstalledStatus::NotInstalled;
    };

    // Multi-file skill: every manifest entry must be present; sha mismatches
    // are update-available only after that completeness gate passes.
    if let Some(manifest) = plugin.manifest.as_ref().filter(|m| !m.is_empty()) {
        if plugin.type_ != "skill" {
            return InstalledStatus::NotInstalled;
        }
        let dir = library_root.join(&stem);
        let mut mismatch = false;
        for entry in manifest {
            let Ok(relpath) = ccteam_core::validate_skill_library_file_relpath(&entry.relpath)
            else {
                return InstalledStatus::NotInstalled;
            };
            let Ok(bytes) = std::fs::read(dir.join(relpath)) else {
                return InstalledStatus::NotInstalled;
            };
            if !sha_eq(&sha256_hex(&bytes), &entry.content_sha) {
                mismatch = true;
            }
        }
        return if mismatch {
            InstalledStatus::UpdateAvailable
        } else {
            InstalledStatus::Installed
        };
    }

    // Single-file agent / SKILL.md-only skill.
    let path = match plugin.type_.as_str() {
        "agent" => ccteam_core::agent_md_path(project_dir, &stem),
        "skill" => library_root.join(&stem).join("SKILL.md"),
        // Not-yet-installable types can never be "installed".
        _ => return InstalledStatus::NotInstalled,
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return InstalledStatus::NotInstalled;
    };
    if sha_eq(&sha256_hex(&bytes), &plugin.content_sha) {
        InstalledStatus::Installed
    } else {
        InstalledStatus::UpdateAvailable
    }
}

/// Lower-hex sha256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        // Two lowercase hex nibbles per byte (no `hex` crate dependency).
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    out
}

/// Case-insensitive hex compare so an UPPERCASE `content_sha` in the index
/// still matches our lowercase digest. (Both sides are short fixed strings;
/// constant-time isn't required — this is integrity, not a secret compare.)
fn sha_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_is_lowercase_and_known() {
        // sha256("") is the well-known empty-string digest.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha_eq_is_case_insensitive() {
        assert!(sha_eq("ABC123", "abc123"));
        assert!(!sha_eq("abc", "abd"));
    }

    #[test]
    fn hub_base_honours_env_override() {
        // Unset → the core default; set → verbatim (the override is a base URL,
        // never merged or validated here).
        assert_eq!(hub_base_from(None), ccteam_core::HUB_RAW_BASE);
        assert_eq!(
            hub_base_from(Some("https://example.test/raw".to_string())),
            "https://example.test/raw"
        );
    }

    #[test]
    fn index_find_resolves_by_id() {
        let idx = HubIndex {
            version: 1,
            name: "h".into(),
            description: String::new(),
            generated_at: String::new(),
            plugins: vec![HubPlugin {
                id: "foo".into(),
                type_: "agent".into(),
                marketplace: None,
                plugin_id: None,
                name: "Foo".into(),
                description: String::new(),
                upstream: String::new(),
                content_sha: "0".into(),
                source: String::new(),
                license: String::new(),
                tags: vec![],
                manifest: None,
            }],
        };
        assert!(idx.find("foo").is_some());
        assert!(idx.find("bar").is_none());
    }

    fn plugin_entry() -> HubPlugin {
        HubPlugin {
            id: "understand-anything".into(),
            type_: "plugin".into(),
            name: "Understand Anything".into(),
            description: "Codebase knowledge graphs".into(),
            upstream: String::new(),
            content_sha: String::new(),
            source: "external".into(),
            license: "see upstream".into(),
            tags: vec!["understand".into()],
            manifest: None,
            marketplace: Some(MarketplaceRef {
                name: "understand-anything".into(),
                source: serde_json::json!({"source":"github","repo":"Egonex-AI/Understand-Anything"}),
            }),
            plugin_id: Some("understand-anything".into()),
        }
    }

    #[test]
    fn plugin_index_entry_deserializes_without_body_fields() {
        // The exact wire shape sync.py emits: NO upstream / content_sha /
        // manifest; marketplace + plugin_id present.
        let wire = serde_json::json!({
            "id": "understand-anything",
            "type": "plugin",
            "name": "Understand Anything",
            "description": "...",
            "source": "external",
            "license": "see upstream repo",
            "tags": ["understand"],
            "marketplace": {
                "name": "understand-anything",
                "source": { "source": "github", "repo": "Egonex-AI/Understand-Anything" }
            },
            "plugin_id": "understand-anything"
        });
        let p: HubPlugin = serde_json::from_value(wire).unwrap();
        assert_eq!(p.type_, "plugin");
        assert_eq!(p.content_sha, ""); // defaulted — absent on the wire
        assert!(p.upstream.is_empty());
        assert!(p.manifest.is_none());
        assert_eq!(p.marketplace.unwrap().name, "understand-anything");
        assert_eq!(p.plugin_id.as_deref(), Some("understand-anything"));
    }

    #[tokio::test]
    async fn plugin_install_writes_settings_and_status_flips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let library_root = dir.join("skills");
        let p = plugin_entry();

        assert_eq!(
            installed_status(dir, &library_root, &p),
            InstalledStatus::NotInstalled
        );

        let res = install_plugin(dir, &library_root, &p, None, false)
            .await
            .unwrap();
        assert_eq!(res.type_, "plugin");
        assert!(!res.overwrote);
        assert!(res.path.ends_with(".claude/settings.local.json"));

        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            v["enabledPlugins"]["understand-anything@understand-anything"],
            serde_json::json!(true)
        );
        assert_eq!(
            v["extraKnownMarketplaces"]["understand-anything"]["source"]["repo"],
            "Egonex-AI/Understand-Anything"
        );

        // Binary status — never UpdateAvailable for a plugin.
        assert_eq!(
            installed_status(dir, &library_root, &p),
            InstalledStatus::Installed
        );
    }

    #[tokio::test]
    async fn plugin_body_preview_describes_delegation_and_missing_pointer_errors() {
        let preview = fetch_plugin_body(&plugin_entry()).await.unwrap();
        assert!(preview.contains("extraKnownMarketplaces"));
        assert!(preview.contains("enabledPlugins[\"understand-anything@understand-anything\"]"));

        let mut bad = plugin_entry();
        bad.marketplace = None;
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            install_plugin(tmp.path(), &tmp.path().join("skills"), &bad, None, false).await,
            Err(HubError::InvalidPlugin(_))
        ));
    }

    #[test]
    fn sort_ccteam_first_features_ccteam_source() {
        fn p(id: &str, source: &str) -> HubPlugin {
            HubPlugin {
                id: id.into(),
                type_: "skill".into(),
                marketplace: None,
                plugin_id: None,
                name: id.into(),
                description: String::new(),
                upstream: format!("https://raw.githubusercontent.com/x/y/sha/skills/{id}.md"),
                content_sha: "0".into(),
                source: source.into(),
                license: String::new(),
                tags: vec![],
                manifest: None,
            }
        }
        let mut idx = HubIndex {
            version: 1,
            name: "h".into(),
            description: String::new(),
            generated_at: String::new(),
            plugins: vec![
                p("a-agency", "agency-agents"),
                p("x-ccteam", "ccteam"),
                p("b-agency", "agency-agents"),
                p("y-ccteam", "ccteam"),
            ],
        };
        idx.sort_ccteam_first();
        let order: Vec<&str> = idx.plugins.iter().map(|p| p.id.as_str()).collect();
        // `source == "ccteam"` first (original relative order preserved), then
        // every other source (also order-preserving) — a stable sort.
        assert_eq!(order, vec!["x-ccteam", "y-ccteam", "a-agency", "b-agency"]);
    }

    const MODELS_FIXTURE: &[u8] = include_bytes!("../tests/fixtures/hub_models_v1.json");
    const BAD_SHA_META_FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/hub_models_bad_sha.meta.json");

    fn test_paths(root: &std::path::Path) -> ccteam_core::CcteamPaths {
        ccteam_core::CcteamPaths {
            root: root.to_path_buf(),
            projects_root: root.join("projects"),
        }
    }

    #[test]
    fn models_fixture_parses_v1_schema() {
        let catalog = parse_models_catalog(MODELS_FIXTURE).unwrap();
        assert_eq!(catalog.schema, "ccteam.models/v1");
        assert_eq!(catalog.vendors["claude"].default.as_deref(), Some("sonnet"));
        assert_eq!(
            catalog.vendors["claude"].models[0].aliases,
            ["deep", "refactor"]
        );
        assert_eq!(
            catalog.vendors["codex"].models[0].display_name.as_deref(),
            Some("GPT 5.6 Sol Medium")
        );
    }

    #[tokio::test]
    async fn models_absent_degrades_to_unavailable_without_network() {
        let root = tempfile::tempdir().unwrap();
        let state = load_models_catalog(
            "https://example.invalid/ccteam-hub",
            &test_paths(root.path()),
            false,
        )
        .await;
        assert_eq!(state, HubModelsState::Unavailable);
    }

    #[tokio::test]
    async fn models_bad_sha_cache_is_refused_without_network() {
        let root = tempfile::tempdir().unwrap();
        let paths = test_paths(root.path());
        let (body_path, meta_path) = models_cache_paths(&paths);
        std::fs::create_dir_all(body_path.parent().unwrap()).unwrap();
        std::fs::write(body_path, MODELS_FIXTURE).unwrap();
        std::fs::write(meta_path, BAD_SHA_META_FIXTURE).unwrap();

        let state = load_models_catalog("https://example.invalid/ccteam-hub", &paths, false).await;
        assert_eq!(state, HubModelsState::Unavailable);
    }

    #[tokio::test]
    async fn expired_verified_models_cache_survives_refresh_as_stale() {
        let root = tempfile::tempdir().unwrap();
        let paths = test_paths(root.path());
        let (body_path, meta_path) = models_cache_paths(&paths);
        std::fs::create_dir_all(body_path.parent().unwrap()).unwrap();
        std::fs::write(&body_path, MODELS_FIXTURE).unwrap();
        let meta = HubModelsCacheMeta {
            sha256: sha256_hex(MODELS_FIXTURE),
            fetched_at: "2020-01-01T00:00:00Z".to_string(),
        };
        std::fs::write(meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

        let state = load_models_catalog("https://example.invalid/ccteam-hub", &paths, false).await;
        let HubModelsState::Available(snapshot) = state else {
            panic!("verified stale cache should render");
        };
        assert!(snapshot.stale);
        assert_eq!(snapshot.catalog.vendors["claude"].models[0].id, "opus");
    }
}
