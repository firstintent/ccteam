//! v0.8.9 Phase 2 — ccteam-hub coordinates (the curated plugin marketplace).
//!
//! `ccteam-hub` (`github.com/firstintent/ccteam-hub`) is a small, curated
//! catalog of installable **plugins** — agents (and, in the future, skills /
//! workflows). ccteam reads its `index.json` over HTTPS (github-raw) plus a
//! local cache under `~/.ccteam/hub-cache/`, and installs a plugin's content
//! into a user project.
//!
//! This module is the **leaf-crate** part: the raw-content base URL constant
//! plus the two pure path/filename utilities the installer reuses
//! ([`raw_url`], [`sanitize_role_stem`]). The actual fetch + integrity-check +
//! install backend lives in `ccteam-im` (`ccteam_im::hub`) because
//! `ccteam-core` is the primitives leaf (`core -> harness -> cost`) and must
//! not take an async HTTP + sha2 dependency. The cache **directory** path is a
//! `CcteamPaths` method ([`crate::CcteamPaths::hub_cache_dir`]) so it honours
//! `CCTEAM_HOME` like every other `~/.ccteam/` subdir.
//!
//! ## Why `main`, not a pinned commit
//!
//! The hub is the **curated source of truth itself** — there is no second
//! baked manifest to keep in lockstep with an upstream, so [`HUB_RAW_BASE`]
//! tracks the `main` branch (ccteam always wants the hub's latest curated
//! `index.json`). Each plugin entry carries its own `content_sha`, and the
//! installer verifies the fetched body against it, so "track main" does not
//! weaken integrity: a body that doesn't match the index it came from is
//! refused.

/// Raw-content base for the curated ccteam-hub catalog, tracking `main`.
///
/// `{HUB_RAW_BASE}/index.json` is the catalog; `{HUB_RAW_BASE}/{plugin.path}`
/// (e.g. `agents/<id>.md`) is a plugin body. The `ccteam_im::hub` backend
/// joins these via [`raw_url`] and may override the base (env
/// `CCTEAM_HUB_BASE`) so deterministic tests point at an in-process fake hub
/// instead of github.
pub const HUB_RAW_BASE: &str = "https://raw.githubusercontent.com/firstintent/ccteam-hub/main";

/// Join a raw-content `base` with a repo-relative `path`, tolerating a
/// trailing slash on `base` (no double slash). The `ccteam_im::hub` backend
/// uses this for both `index.json` and a plugin body (`agents/<id>.md`) so the
/// `base` override works uniformly for the live host and an in-process fake
/// hub. Re-exported from `ccteam-core` as `catalog_raw_url`.
pub fn raw_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}

/// Lower-case + collapse any run of characters outside `[a-z0-9_-]` to a
/// single `-`, then strip leading/trailing `-`. Produces a stem that
/// satisfies `admin_actions::validate_bot_name` (the [`crate::write_role`]
/// gate, which **rejects** rather than transforms). The hub installer uses
/// this on the chosen install stem (the `target_stem` override or the plugin
/// `id`) **before** writing. Returns an error if the input sanitizes to the
/// empty string (e.g. all-punctuation), since `write_role` would reject an
/// empty name anyway and a clearer message helps the user.
pub fn sanitize_role_stem(raw: &str) -> anyhow::Result<String> {
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
    fn raw_url_joins_cleanly() {
        assert_eq!(
            raw_url("https://host/base", "agents/y.md"),
            "https://host/base/agents/y.md"
        );
        // Trailing slash on base is tolerated (no double slash).
        assert_eq!(
            raw_url("https://host/base/", "agents/y.md"),
            "https://host/base/agents/y.md"
        );
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

    /// The sanitizer's output MUST satisfy the same `[a-z0-9_-]` set the write
    /// side (`write_role` → `validate_bot_name`) enforces, or install will
    /// sanitize a stem that `write_role` then rejects. We can't call the
    /// private validator here, so re-assert the character class.
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
