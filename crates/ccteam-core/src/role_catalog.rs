//! v0.8.7 W3 (DC.1) — vendored, **offline** catalog of the open-source
//! agency-agents role library (the MIT, Claude-native subagent collection
//! at `github.com/wshobson/agents`).
//!
//! This module is pure data + search: it parses a manifest baked into the
//! binary via `include_str!` and answers `search` / `find_by_id` queries
//! with **no network I/O**. The actual import (fetch the `.md` over HTTP and
//! `write_role` it into `<project>/.claude/agents/`) lives **outside** this
//! leaf crate — `ccteam-core` is the primitives leaf (topology
//! `core -> harness -> cost`) and must not take an async HTTP dependency.
//! The network importer (`import_role_from_catalog`) lives in `ccteam-im`
//! (which already depends on `reqwest`) and composes this catalog's lookup
//! with a `reqwest` fetch + [`crate::write_role`].
//!
//! ## Manifest shape
//!
//! [`AGENCY_AGENTS_CATALOG`] is a JSON array of [`CatalogEntry`]:
//! `{ id, division, display_name, description, raw_path }` — **no body**.
//! The body is fetched on demand at import time. `raw_path` is the
//! repo-relative path of the role `.md`; the full raw URL is
//! `{AGENCY_RAW_BASE}/{raw_path}` (see [`raw_url`]).
//!
//! ## How the manifest was generated (refresh = chore)
//!
//! A dev-time sweep over the upstream repo's git tree:
//! `gh api 'repos/wshobson/agents/git/trees/HEAD?recursive=1'` filtered to
//! `^plugins/[^/]+/agents/[^/]+\.md$` (the role files; the repo also holds
//! skills/commands/templates/docs which are **not** roles), then each file's
//! YAML frontmatter (`name` / `description`) was read for the human fields.
//! As of the snapshot baked here that is **192 roles across 78 divisions**
//! (upstream tree sha `cf6059d0…`, branch `main`, license MIT). The upstream
//! has no semver tags, so `raw_path` resolves against `HEAD`; pin a commit
//! sha in [`AGENCY_RAW_BASE`] if reproducible imports are needed. Refreshing
//! the manifest is a chore: re-run the sweep and overwrite
//! `agency_agents_catalog.json` (mirrors the `workflow_templates/`
//! maintenance pattern).
//!
//! `id` is **globally unique**: file stems collide across divisions (e.g.
//! `backend-architect` appears in 6 divisions), so `id` is the frontmatter
//! `name` (already division-prefixed) sanitized to `[a-z0-9_-]`, falling
//! back to `<division>-<stem>`. The bare stem is **not** a safe key.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The vendored agency-agents manifest (JSON array, no role bodies).
pub const AGENCY_AGENTS_CATALOG: &str = include_str!("templates/agency_agents_catalog.json");

/// Raw-content base for resolving a [`CatalogEntry::raw_path`] to a full
/// URL. `HEAD` tracks the upstream default branch; swap in a commit sha
/// here for reproducible imports. The importer (`ccteam-im`) may override
/// this for deterministic tests.
pub const AGENCY_RAW_BASE: &str = "https://raw.githubusercontent.com/wshobson/agents/HEAD";

/// One catalog entry. Carries only the metadata needed to browse + resolve
/// a role for import; the markdown body is fetched on demand at import time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Globally-unique catalog id (`backend-development-backend-architect`).
    /// This is the `<id>` argument to `ccteam role add`.
    pub id: String,
    /// The upstream division (plugin) the role lives under
    /// (`backend-development`).
    pub division: String,
    /// Human-friendly short name (the file stem, e.g. `backend-architect`).
    pub display_name: String,
    /// One-line description (from frontmatter `description`, best-effort).
    pub description: String,
    /// Repo-relative path of the role `.md`
    /// (`plugins/backend-development/agents/backend-architect.md`).
    pub raw_path: String,
}

impl CatalogEntry {
    /// Full raw URL for this entry against the default upstream base.
    pub fn raw_url(&self) -> String {
        raw_url(AGENCY_RAW_BASE, &self.raw_path)
    }
}

/// Join a raw base with a repo-relative `raw_path`, tolerating a trailing
/// slash on `base`. Used by the importer so the `base_url` override works
/// uniformly for the live base and a test mock server.
pub fn raw_url(base: &str, raw_path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), raw_path)
}

/// Parse the vendored manifest. Cheap enough to call per-invocation (the
/// CLI is a one-shot process); callers that need it repeatedly can cache.
pub fn all() -> Result<Vec<CatalogEntry>> {
    serde_json::from_str(AGENCY_AGENTS_CATALOG).context("parse vendored agency_agents_catalog.json")
}

/// Find an entry by its exact catalog `id`. Returns `Ok(None)` when no
/// entry matches (the caller maps that to a "no such role in catalog"
/// user error), `Err` only on a corrupt vendored manifest.
pub fn find_by_id(id: &str) -> Result<Option<CatalogEntry>> {
    Ok(all()?.into_iter().find(|e| e.id == id))
}

/// **Offline** substring search over the vendored manifest. Matches the
/// (case-insensitive) query against `id`, `division`, `display_name`, and
/// `description`. An empty / whitespace query returns the full catalog
/// (sorted by id) so `ccteam role search ""` is a "list everything" browse.
/// Results are sorted by id for stable output.
pub fn search(query: &str) -> Result<Vec<CatalogEntry>> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<CatalogEntry> = all()?
        .into_iter()
        .filter(|e| {
            if q.is_empty() {
                return true;
            }
            e.id.to_lowercase().contains(&q)
                || e.division.to_lowercase().contains(&q)
                || e.display_name.to_lowercase().contains(&q)
                || e.description.to_lowercase().contains(&q)
        })
        .collect();
    hits.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(hits)
}

/// Lower-case + collapse any run of characters outside `[a-z0-9_-]` to a
/// single `-`, then strip leading/trailing `-`. Produces a stem that
/// satisfies `admin_actions::validate_bot_name` (the `write_role` gate,
/// which **rejects** rather than transforms). The importer uses this on the
/// chosen role stem (`--as <role>` override or the catalog `display_name`)
/// **before** calling `write_role`. Returns an error if the input sanitizes
/// to the empty string (e.g. all-punctuation), since `write_role` would
/// reject an empty name anyway and a clearer message helps the user.
pub fn sanitize_role_stem(raw: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
            last_dash = false;
        } else {
            // Any other char (space, '/', uppercase already lowered above,
            // punctuation) collapses to a single '-'.
            if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        anyhow::bail!("role name `{raw}` sanitizes to empty (needs at least one [a-z0-9_] char)");
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_valid_and_nonempty() {
        let all = all().expect("vendored manifest must be valid JSON");
        assert!(
            all.len() >= 100,
            "expected a substantial vendored catalog, got {}",
            all.len()
        );
        // Every entry carries the mandatory id/division/raw_path, and
        // raw_path looks like the real upstream layout.
        for e in &all {
            assert!(!e.id.is_empty(), "entry id must be non-empty");
            assert!(!e.division.is_empty(), "entry division must be non-empty");
            assert!(
                e.raw_path.starts_with("plugins/") && e.raw_path.ends_with(".md"),
                "raw_path must match the upstream plugins/.../agents/*.md layout, got `{}`",
                e.raw_path
            );
        }
    }

    #[test]
    fn ids_are_globally_unique() {
        let all = all().unwrap();
        let mut ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(
            before,
            ids.len(),
            "catalog ids must be globally unique (bare stems collide across divisions)"
        );
    }

    #[test]
    fn search_is_offline_and_matches() {
        // A query we know is in the catalog (backend roles exist across
        // several divisions). This runs with no network — pure manifest.
        let hits = search("backend").unwrap();
        assert!(
            !hits.is_empty(),
            "`backend` should match catalog entries offline"
        );
        assert!(
            hits.iter().all(|e| e.id.to_lowercase().contains("backend")
                || e.division.to_lowercase().contains("backend")
                || e.display_name.to_lowercase().contains("backend")
                || e.description.to_lowercase().contains("backend")),
            "every hit must actually contain the query in a searched field"
        );
    }

    #[test]
    fn search_is_case_insensitive() {
        let lower = search("backend").unwrap();
        let upper = search("BACKEND").unwrap();
        assert_eq!(lower.len(), upper.len(), "search must be case-insensitive");
    }

    #[test]
    fn empty_query_lists_everything() {
        let all_n = all().unwrap().len();
        assert_eq!(search("   ").unwrap().len(), all_n);
    }

    #[test]
    fn find_by_id_roundtrips_a_real_entry() {
        let first = all().unwrap().into_iter().next().unwrap();
        let found = find_by_id(&first.id).unwrap();
        assert_eq!(found.as_ref(), Some(&first));
        assert!(
            find_by_id("definitely-not-a-real-catalog-id-xyz")
                .unwrap()
                .is_none(),
            "unknown id must yield None, not Err"
        );
    }

    #[test]
    fn raw_url_joins_cleanly() {
        assert_eq!(
            raw_url("https://host/base", "plugins/x/agents/y.md"),
            "https://host/base/plugins/x/agents/y.md"
        );
        // Trailing slash on base is tolerated (no double slash).
        assert_eq!(
            raw_url("https://host/base/", "plugins/x/agents/y.md"),
            "https://host/base/plugins/x/agents/y.md"
        );
    }

    #[test]
    fn entry_raw_url_uses_default_base() {
        let e = all().unwrap().into_iter().next().unwrap();
        assert_eq!(e.raw_url(), format!("{AGENCY_RAW_BASE}/{}", e.raw_path));
    }

    #[test]
    fn sanitize_stem_handles_uppercase_and_spaces() {
        assert_eq!(sanitize_role_stem("My Agent").unwrap(), "my-agent");
        assert_eq!(
            sanitize_role_stem("Backend Architect Pro").unwrap(),
            "backend-architect-pro"
        );
        // Already-clean stem is unchanged.
        assert_eq!(
            sanitize_role_stem("backend-architect").unwrap(),
            "backend-architect"
        );
        // Collapsing + trimming of leading/trailing junk.
        assert_eq!(sanitize_role_stem("  --Foo__Bar!!  ").unwrap(), "foo__bar");
        // Underscores are preserved (valid in [a-z0-9_-]).
        assert_eq!(
            sanitize_role_stem("data_scientist").unwrap(),
            "data_scientist"
        );
    }

    #[test]
    fn sanitize_stem_rejects_empty_result() {
        assert!(sanitize_role_stem("").is_err());
        assert!(
            sanitize_role_stem("!!!").is_err(),
            "all-punctuation → empty → Err"
        );
    }

    /// The sanitizer's output MUST satisfy the same `[a-z0-9_-]` set the
    /// write side (`write_role` → `validate_bot_name`) enforces, or import
    /// will sanitize a stem that `write_role` then rejects. We can't call
    /// the private validator here, so re-assert the character class.
    #[test]
    fn sanitized_stems_are_within_write_role_charset() {
        for raw in ["My Agent", "Backend/Architect", "FOO BAR", "a..b", "x/y/z"] {
            let s = sanitize_role_stem(raw).unwrap();
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'),
                "sanitized `{s}` (from `{raw}`) escapes [a-z0-9_-]"
            );
        }
    }
}
