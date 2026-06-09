//! v0.8.9 Phase 2 — ccteam-hub read + install backend (the curated plugin
//! marketplace).
//!
//! `ccteam-hub` (`github.com/firstintent/ccteam-hub`) is a curated catalog of
//! installable **plugins** — agents today, skills / workflows later. ccteam
//! reads its `index.json` over HTTPS (github-raw) plus a local cache under
//! `~/.ccteam/hub-cache/`, and installs a plugin's content into a user
//! project (`.claude/agents/<id>.md` for an agent, `.claude/skills/<id>/SKILL.md`
//! for a skill). `ccteam-web` + the CLI call this module.
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
//! Every public fn takes a `base` parameter and the live wrappers pass
//! [`ccteam_core::HUB_RAW_BASE`] (overridable via the [`HUB_BASE_ENV`] env
//! var). Tests point `base` at an in-process fake hub (`spawn_oneshot_http`)
//! so `cargo test` never touches github.

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
}

/// One installable plugin entry in the hub index.
///
/// Matches the live schema:
/// `{ id, type, name, description, path, content_sha, source, upstream,
///   license, tags[] }`. `type` is `"agent" | "skill" | "workflow"`;
/// `content_sha` is the sha256 hex of the body at `path` (relative to the hub
/// raw base, e.g. `agents/<id>.md`).
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
    /// Repo-relative path of the plugin body (`agents/<id>.md`).
    pub path: String,
    /// sha256 hex of the body at `path`. The installer verifies the fetched
    /// body against this before writing (integrity / anti-tamper).
    pub content_sha: String,
    /// Provenance: where ccteam-hub sourced this plugin (free-form).
    #[serde(default)]
    pub source: String,
    /// Upstream repo / URL the plugin was curated from (free-form).
    #[serde(default)]
    pub upstream: String,
    /// SPDX-ish license string (free-form).
    #[serde(default)]
    pub license: String,
    /// Browse / filter tags.
    #[serde(default)]
    pub tags: Vec<String>,
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

/// GET `url` with the hardened client and return the body bytes, enforcing
/// the status / size guards. `what` labels the fetch for error messages.
async fn fetch_bytes(client: &reqwest::Client, url: &str, what: &str) -> Result<Vec<u8>, HubError> {
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
    serde_json::from_slice(bytes).map_err(|e| HubError::BadIndex(format!("{e}")))
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

/// Fetch a plugin body from `{base}/{plugin.path}` with the hardened client,
/// then **verify `sha256(body) == plugin.content_sha`** before returning it.
/// A mismatch is [`HubError::ShaMismatch`] (the integrity gate). The body must
/// also be non-empty UTF-8 within the size cap.
pub async fn fetch_plugin_body(base: &str, plugin: &HubPlugin) -> Result<String, HubError> {
    let url = ccteam_core::catalog_raw_url(base, &plugin.path);
    let client = hardened_client(&plugin.id, &url)?;
    let buf = fetch_bytes(&client, &url, &plugin.id).await?;

    // Integrity check is over the raw bytes (sha256 of the file content).
    let actual = sha256_hex(&buf);
    if !sha_eq(&actual, &plugin.content_sha) {
        return Err(HubError::ShaMismatch {
            id: plugin.id.clone(),
            expected: plugin.content_sha.clone(),
            actual,
        });
    }

    // A plugin body must be valid UTF-8 (Claude-native markdown).
    let body = String::from_utf8(buf).map_err(|_| {
        HubError::Write(format!("fetched plugin `{}` is not valid UTF-8", plugin.id))
    })?;
    if body.trim().is_empty() {
        return Err(HubError::EmptyBody(plugin.id.clone()));
    }
    Ok(body)
}

/// Install a hub plugin's content into `project_dir`.
///
/// 5-step:
/// 1. derive + sanitize the install stem — `target_stem` override, else
///    `plugin.id` — to `[a-z0-9_-]`,
/// 2. refuse to clobber an existing file at the target unless `force`,
/// 3. fetch the body + **verify its sha256** ([`fetch_plugin_body`]),
/// 4. write by type: `"agent"` → [`ccteam_core::write_role`]
///    (`.claude/agents/<stem>.md`); `"skill"` → [`ccteam_core::write_skill`]
///    (`.claude/skills/<stem>/SKILL.md`); `"workflow"` →
///    [`HubError::UnsupportedType`] (deferred); any other → `UnsupportedType`,
/// 5. return the [`InstallResult`].
///
/// `base` is the hub raw-content base (the test seam). Note step 2's
/// existence check is keyed on the **post-sanitize** stem + the resolved type
/// path, so an agent and a skill with the same stem don't collide.
pub async fn install_plugin(
    project_dir: &Path,
    plugin: &HubPlugin,
    target_stem: Option<&str>,
    force: bool,
    base: &str,
) -> Result<InstallResult, HubError> {
    // 1. Derive + sanitize the install stem (filename normalization only — the
    //    body is Claude-native and written verbatim).
    let raw_stem = target_stem.unwrap_or(&plugin.id);
    let stem = ccteam_core::sanitize_role_stem(raw_stem)
        .map_err(|e| HubError::BadStem(format!("{e:#}")))?;

    // Resolve the target path by type up front so the clobber check and the
    // write agree, and an unsupported type fails before any network I/O.
    let dest = match plugin.type_.as_str() {
        "agent" => ccteam_core::agent_md_path(project_dir, &stem),
        "skill" => ccteam_core::skill_md_path(project_dir, &stem),
        "workflow" => return Err(HubError::UnsupportedType(plugin.type_.clone())),
        other => return Err(HubError::UnsupportedType(other.to_string())),
    };

    // 2. Refuse to clobber unless forced.
    let exists = dest.exists();
    if exists && !force {
        return Err(HubError::Exists(stem));
    }

    // 3. Fetch + verify the body (sha256 gate inside).
    let body = fetch_plugin_body(base, plugin).await?;

    // 4. Write verbatim by type.
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
/// sidecar file. Reads the bytes at the plugin's **default** target path
/// (keyed on `plugin.id`, the same stem `install_plugin` uses when no override
/// is given) and compares their sha256 to `plugin.content_sha`:
///
/// - target absent → [`InstalledStatus::NotInstalled`],
/// - present + sha matches → [`InstalledStatus::Installed`],
/// - present + sha differs → [`InstalledStatus::UpdateAvailable`].
///
/// An unreadable file (race / permissions) or an unsupported / unsanitizable
/// type is treated as `NotInstalled` (best-effort decoration of the catalog;
/// never errors). The web layer uses this to badge each catalog row.
pub fn installed_status(project_dir: &Path, plugin: &HubPlugin) -> InstalledStatus {
    // Default stem == sanitized plugin id (the no-override install path).
    let Ok(stem) = ccteam_core::sanitize_role_stem(&plugin.id) else {
        return InstalledStatus::NotInstalled;
    };
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
                path: "agents/foo.md".into(),
                content_sha: "0".into(),
                source: String::new(),
                upstream: String::new(),
                license: String::new(),
                tags: vec![],
            }],
        };
        assert!(idx.find("foo").is_some());
        assert!(idx.find("bar").is_none());
    }
}
