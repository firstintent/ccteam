//! v0.8.9 Phase 2 — ccteam-hub coordinates (the curated plugin marketplace).
//!
//! `ccteam-hub` (`github.com/firstintent/ccteam-hub`) is a small, curated
//! catalog of installable **plugins** — agents (and, in the future, skills /
//! workflows). ccteam reads its `index.json` over HTTPS (github-raw) plus a
//! local cache under `~/.ccteam/hub-cache/`, and installs a plugin's content
//! into a user project.
//!
//! This module is the **leaf-crate** part: just the raw-content base URL
//! constant. The actual fetch + integrity-check + install backend lives in
//! `ccteam-im` (`ccteam_im::hub`) because `ccteam-core` is the primitives
//! leaf (`core -> harness -> cost`) and must not take an async HTTP +
//! sha2 dependency — the same split the agency-agents catalog/importer
//! uses ([`crate::role_catalog`] is offline data; the HTTP importer is
//! [`ccteam_im::role_import`]). The cache **directory** path is a
//! `CcteamPaths` method ([`crate::CcteamPaths::hub_cache_dir`]) so it
//! honours `CCTEAM_HOME` like every other `~/.ccteam/` subdir.
//!
//! ## Why `main`, not a pinned commit
//!
//! Unlike [`crate::role_catalog::AGENCY_RAW_BASE`] (pinned to a commit sha so
//! a *vendored* manifest stays in lockstep with the upstream it was swept
//! from), the hub is the **curated source of truth itself** — there is no
//! second baked manifest to keep in sync. ccteam always wants the hub's
//! latest curated `index.json`, so the base tracks the `main` branch. Each
//! plugin entry carries its own `content_sha`, and the installer verifies the
//! fetched body against it, so "track main" does not weaken integrity: a body
//! that doesn't match the index it came from is refused.

/// Raw-content base for the curated ccteam-hub catalog, tracking `main`.
///
/// `{HUB_RAW_BASE}/index.json` is the catalog; `{HUB_RAW_BASE}/{plugin.path}`
/// (e.g. `agents/<id>.md`) is a plugin body. The `ccteam_im::hub` backend
/// joins these via [`crate::raw_url`] and may override the base (env
/// `CCTEAM_HUB_BASE`) so deterministic tests point at an in-process fake hub
/// instead of github.
pub const HUB_RAW_BASE: &str = "https://raw.githubusercontent.com/firstintent/ccteam-hub/main";
