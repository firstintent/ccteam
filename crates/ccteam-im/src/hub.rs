//! v0.8.9 Phase 2 — ccteam-hub read + install backend (the curated plugin
//! marketplace).
//!
//! `ccteam-hub` (`github.com/firstintent/ccteam-hub`) is a curated catalog of
//! installable **plugins** — agents + skills (workflows deferred). ccteam
//! reads its `index.json` over HTTPS (github-raw) plus a local cache under
//! `~/.ccteam/hub-cache/` (track-upstream: the index stores per-plugin
//! `upstream` URLs, not vendored bodies), and installs a plugin's content into
//! a user project (`.claude/agents/<id>.md` for an agent;
//! `.claude/skills/<id>/…` for a skill — a single `SKILL.md`, or a whole dir
//! for a multi-file skill via its `manifest`). `ccteam-web` + the CLI call
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
    std::env::var(HUB_BASE_ENV).unwrap_or_else(|_| ccteam_core::HUB_RAW_BASE.to_string())
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

/// One file of a multi-file skill (PRD §二): a path relative to the skill
/// dir + the sha256 of its body. The engine derives the file's fetch URL from
/// the plugin's `upstream` dir + `relpath`, verifies the sha, and writes it
/// under `.claude/skills/<id>/<relpath>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Path relative to the skill dir (e.g. `SKILL.md`, `scripts/run.sh`).
    pub relpath: String,
    /// sha256 hex of this file's body (per-file integrity gate).
    pub content_sha: String,
}

/// One installable plugin entry in the hub index (track-upstream schema).
///
/// `{ id, type, name, description, upstream, content_sha, source, license,
///   tags[], manifest? }`. `type` is `"agent" | "skill" | "workflow"`;
/// `upstream` is the raw-fetchable URL of the body
/// (`raw.githubusercontent.com/<owner>/<repo>/<sha>/<path>` for an external
/// source, or the hub's own raw tree for first-party content). `content_sha`
/// is the sha256 of that body. A multi-file skill additionally carries a
/// `manifest` of every file (relpath + sha, incl. `SKILL.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HubPlugin {
    /// Globally-unique plugin id (the install key, and default install stem).
    pub id: String,
    /// `"agent"`, `"skill"`, or `"workflow"`. Serde wire name `type`.
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
}

/// Outcome of a successful [`install_plugin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallResult {
    /// The plugin id that was installed.
    pub id: String,
    /// The plugin `type` (`"agent"` / `"skill"`).
    pub type_: String,
    /// Absolute path of the file written
    /// (`.claude/agents/<stem>.md` or `.claude/skills/<stem>/SKILL.md`).
    pub path: PathBuf,
    /// `true` when an existing file at the target was overwritten (only
    /// possible with `force`).
    pub overwrote: bool,
}

/// Whether a hub plugin is already present in a project, computed on-the-fly
/// from the file on disk vs. the index's `content_sha` (no sidecar file — the
/// `.ccteam` layout red-line forbids a new per-project state file).
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
    if plugin.upstream.trim().is_empty() {
        return Err(HubError::Write(format!(
            "plugin `{}` has no upstream URL to fetch",
            plugin.id
        )));
    }
    fetch_text_verified(&plugin.upstream, &plugin.content_sha, &plugin.id).await
}

/// Install a hub plugin's content into `project_dir`.
///
/// - **agent / single-file skill**: derive + sanitize the install stem
///   (`target_stem` override, else `plugin.id`); refuse to clobber an existing
///   target unless `force`; fetch the body from `plugin.upstream` + verify its
///   sha; write `.claude/agents/<stem>.md` (agent) or
///   `.claude/skills/<stem>/SKILL.md` (skill).
/// - **multi-file skill** (`manifest` present): the install target is the
///   skill DIR (`.claude/skills/<stem>/`); refuse to clobber it unless `force`;
///   fetch + verify EVERY manifest file (URL = the `upstream` dir + each
///   `relpath`) BEFORE writing any — so a mid-list integrity failure leaves
///   nothing partial — then write each under `.claude/skills/<stem>/<relpath>`.
/// - **workflow / unknown type**: [`HubError::UnsupportedType`] (deferred).
///
/// `InstallResult.path` is the primary file (`SKILL.md` / the agent `.md`).
pub async fn install_plugin(
    project_dir: &Path,
    plugin: &HubPlugin,
    target_stem: Option<&str>,
    force: bool,
) -> Result<InstallResult, HubError> {
    let raw_stem = target_stem.unwrap_or(&plugin.id);
    let stem = ccteam_core::sanitize_role_stem(raw_stem)
        .map_err(|e| HubError::BadStem(format!("{e:#}")))?;

    // Multi-file skill (manifest present): a directory install.
    if let Some(manifest) = plugin.manifest.as_ref().filter(|m| !m.is_empty()) {
        if plugin.type_ != "skill" {
            // A manifest only makes sense for a skill directory.
            return Err(HubError::UnsupportedType(plugin.type_.clone()));
        }
        let skill_dir = ccteam_core::skill_dir_path(project_dir, &stem);
        let exists = skill_dir.exists();
        if exists && !force {
            return Err(HubError::Exists(stem));
        }
        let base_url = upstream_dir(&plugin.upstream)?;
        // Pass 1: fetch + verify EVERY file (no writes → atomic on failure).
        let mut files: Vec<(String, String)> = Vec::with_capacity(manifest.len());
        for entry in manifest {
            let url = format!("{base_url}/{}", entry.relpath);
            let body = fetch_text_verified(&url, &entry.content_sha, &entry.relpath).await?;
            files.push((entry.relpath.clone(), body));
        }
        // Pass 2: write each under .claude/skills/<stem>/<relpath>.
        let mut primary: Option<PathBuf> = None;
        for (relpath, body) in &files {
            let written = ccteam_core::write_skill_file(project_dir, &stem, relpath, body)
                .map_err(|e| HubError::Write(format!("{e:#}")))?;
            if relpath == "SKILL.md" {
                primary = Some(written);
            }
        }
        let path = primary.unwrap_or_else(|| ccteam_core::skill_md_path(project_dir, &stem));
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
        "skill" => ccteam_core::skill_md_path(project_dir, &stem),
        "workflow" => return Err(HubError::UnsupportedType(plugin.type_.clone())),
        other => return Err(HubError::UnsupportedType(other.to_string())),
    };
    let exists = dest.exists();
    if exists && !force {
        return Err(HubError::Exists(stem));
    }
    let body = fetch_plugin_body(plugin).await?;
    let path = match plugin.type_.as_str() {
        "agent" => ccteam_core::write_role(project_dir, &stem, &body),
        "skill" => ccteam_core::write_skill(project_dir, &stem, &body),
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

/// Compute the on-the-fly installed status of `plugin` in `project_dir` — no
/// sidecar file. Keyed on `plugin.id` (the no-override install stem):
///
/// - target absent → [`InstalledStatus::NotInstalled`],
/// - present + sha matches → [`InstalledStatus::Installed`],
/// - present + sha differs → [`InstalledStatus::UpdateAvailable`].
///
/// For a **multi-file skill** (`manifest` present) the comparison is over the
/// whole dir: every manifest file present + sha-matching → `Installed`; none
/// present → `NotInstalled`; any file missing or stale → `UpdateAvailable`.
///
/// An unreadable file (race / permissions) or an unsupported / unsanitizable
/// type is treated as `NotInstalled` (best-effort decoration of the catalog;
/// never errors). The web layer uses this to badge each catalog row.
pub fn installed_status(project_dir: &Path, plugin: &HubPlugin) -> InstalledStatus {
    // Default stem == sanitized plugin id (the no-override install path).
    let Ok(stem) = ccteam_core::sanitize_role_stem(&plugin.id) else {
        return InstalledStatus::NotInstalled;
    };

    // Multi-file skill: compare every manifest file under the skill dir.
    if let Some(manifest) = plugin.manifest.as_ref().filter(|m| !m.is_empty()) {
        let dir = ccteam_core::skill_dir_path(project_dir, &stem);
        let mut present = 0usize;
        let mut matched = 0usize;
        for entry in manifest {
            if let Ok(bytes) = std::fs::read(dir.join(&entry.relpath)) {
                present += 1;
                if sha_eq(&sha256_hex(&bytes), &entry.content_sha) {
                    matched += 1;
                }
            }
        }
        return if present == 0 {
            InstalledStatus::NotInstalled
        } else if matched == manifest.len() {
            InstalledStatus::Installed
        } else {
            InstalledStatus::UpdateAvailable
        };
    }

    // Single-file agent / SKILL.md-only skill.
    let path = match plugin.type_.as_str() {
        "agent" => ccteam_core::agent_md_path(project_dir, &stem),
        "skill" => ccteam_core::skill_md_path(project_dir, &stem),
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
        // Sanity: with the env unset we get the core default.
        std::env::remove_var(HUB_BASE_ENV);
        assert_eq!(hub_base(), ccteam_core::HUB_RAW_BASE);
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

    #[test]
    fn sort_ccteam_first_features_ccteam_source() {
        fn p(id: &str, source: &str) -> HubPlugin {
            HubPlugin {
                id: id.into(),
                type_: "skill".into(),
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
}
