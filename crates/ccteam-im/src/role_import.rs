//! v0.8.7 W3 (DC.2) — one-shot import of an agency-agents role into a
//! project's `.claude/agents/`.
//!
//! The **offline** catalog (browse/search/resolve) lives in `ccteam-core`
//! (`role_catalog`); the **network** fetch lives here because `ccteam-core`
//! is the primitives leaf (`core -> harness -> cost`) and must not take an
//! async HTTP dependency. `ccteam-im` already depends on `reqwest` (the IM
//! onboarding flows), so the importer is at home next to
//! [`crate::onboarding`], and follows the same `*_with_base` test seam
//! (a deterministic mock server stands in for `raw.githubusercontent.com`).
//!
//! Flow:
//! 1. resolve `catalog_id` → [`ccteam_core::CatalogEntry`] (offline),
//! 2. derive the target role stem: caller `target` override, else the
//!    entry's `display_name`; sanitize it to `[a-z0-9_-]` (the catalog `.md`
//!    is already Claude-native — **no frontmatter conversion**),
//! 3. refuse if `<project>/.claude/agents/<role>.md` exists and `!force`,
//! 4. `reqwest` GET the role `.md` from `{base_url}/{raw_path}` (honest
//!    error on a 404 / network failure),
//! 5. `ccteam_core::write_role` the body **verbatim**.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Outcome of a successful import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRoleResult {
    /// The sanitized role stem written (`.claude/agents/<role>.md`).
    pub role: String,
    /// Absolute path of the written role file.
    pub path: PathBuf,
    /// The catalog id that was imported.
    pub catalog_id: String,
    /// `true` when an existing role file was overwritten (only possible
    /// with `force`).
    pub overwrote: bool,
}

/// Errors from [`import_role_from_catalog`].
#[derive(Debug, Error)]
pub enum ImportError {
    /// No catalog entry with the given id (offline lookup miss). The
    /// `String` is the id the user asked for.
    #[error("no role `{0}` in the agency-agents catalog — try `ccteam role search <q>`")]
    UnknownId(String),
    /// A role file already exists at the target path and `force` was not
    /// set. The `String` is the target role stem.
    #[error("role `{0}` already exists — pass --force to overwrite")]
    Exists(String),
    /// The underlying `reqwest` HTTP call failed (DNS / TLS / connect /
    /// read timeout) — an honest network failure, not a saved partial file.
    #[error("fetch failed for role `{role}`: {source}")]
    Http {
        /// The catalog id being fetched.
        role: String,
        /// The underlying transport error.
        #[source]
        source: reqwest::Error,
    },
    /// The upstream returned a non-success HTTP status (e.g. 404 for a
    /// stale `raw_path`). Carries the status + the URL for a honest message.
    #[error("fetch for role `{role}` returned HTTP {status} ({url})")]
    BadStatus {
        /// The catalog id being fetched.
        role: String,
        /// The HTTP status code returned.
        status: u16,
        /// The URL that was fetched.
        url: String,
    },
    /// The fetched body was empty — refuse to write a content-less role
    /// file (mirrors `write_role`'s empty-body rejection, but with the
    /// fetch context).
    #[error("fetched role `{0}` is empty — refusing to write a content-less role file")]
    EmptyBody(String),
    /// The target role name (after sanitizing the override / display_name)
    /// is invalid, or `write_role` failed. Wraps the underlying message.
    #[error("{0}")]
    Write(String),
}

/// Import a catalog role into `project_dir`'s `.claude/agents/`, fetching
/// from the live upstream raw base ([`ccteam_core::AGENCY_RAW_BASE`]).
///
/// `target` (the `--as <role>` override) renames the imported role; when
/// `None` the catalog entry's `display_name` is used. Either way the stem is
/// sanitized to `[a-z0-9_-]`. `force` permits overwriting an existing role
/// file (otherwise [`ImportError::Exists`]).
pub async fn import_role_from_catalog(
    project_dir: &Path,
    catalog_id: &str,
    target: Option<&str>,
    force: bool,
) -> Result<ImportRoleResult, ImportError> {
    import_role_from_catalog_with_base(
        project_dir,
        catalog_id,
        target,
        force,
        ccteam_core::AGENCY_RAW_BASE,
    )
    .await
}

/// Test-friendly variant that overrides the raw base URL so a deterministic
/// mock server can stand in for `raw.githubusercontent.com` (no real
/// network call required for `cargo test`). Mirrors
/// [`crate::onboarding::telegram_setup_with_base`].
pub async fn import_role_from_catalog_with_base(
    project_dir: &Path,
    catalog_id: &str,
    target: Option<&str>,
    force: bool,
    base_url: &str,
) -> Result<ImportRoleResult, ImportError> {
    // 1. Resolve id → entry (offline). A corrupt vendored manifest is a
    //    programming/build error; surface it as Write (it isn't a network
    //    issue and shouldn't masquerade as UnknownId).
    let entry = ccteam_core::catalog_find_by_id(catalog_id)
        .map_err(|e| ImportError::Write(format!("vendored catalog is corrupt: {e:#}")))?
        .ok_or_else(|| ImportError::UnknownId(catalog_id.to_string()))?;

    // 2. Derive + sanitize the target role stem. The catalog `.md` is
    //    already Claude-native — we do ZERO frontmatter conversion; only the
    //    *filename* stem is normalized to the write_role charset.
    let raw_stem = target.unwrap_or(&entry.display_name);
    let role = ccteam_core::sanitize_role_stem(raw_stem)
        .map_err(|e| ImportError::Write(format!("{e:#}")))?;

    // 3. Refuse to clobber an existing role unless forced.
    let dest = ccteam_core::agent_md_path(project_dir, &role);
    let exists = dest.exists();
    if exists && !force {
        return Err(ImportError::Exists(role));
    }

    // 4. Fetch the role .md over HTTP (honest error on 404 / transport).
    let url = ccteam_core::catalog_raw_url(base_url, &entry.raw_path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|source| ImportError::Http {
            role: catalog_id.to_string(),
            source,
        })?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|source| ImportError::Http {
            role: catalog_id.to_string(),
            source,
        })?;
    if !resp.status().is_success() {
        return Err(ImportError::BadStatus {
            role: catalog_id.to_string(),
            status: resp.status().as_u16(),
            url,
        });
    }
    let body = resp.text().await.map_err(|source| ImportError::Http {
        role: catalog_id.to_string(),
        source,
    })?;
    if body.trim().is_empty() {
        return Err(ImportError::EmptyBody(catalog_id.to_string()));
    }

    // 5. Write verbatim (write_role re-validates the stem + atomic write).
    let path = ccteam_core::write_role(project_dir, &role, &body)
        .map_err(|e| ImportError::Write(format!("{e:#}")))?;

    Ok(ImportRoleResult {
        role,
        path,
        catalog_id: catalog_id.to_string(),
        overwrote: exists,
    })
}
