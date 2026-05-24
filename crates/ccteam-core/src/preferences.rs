//! V0.6.0 Wave 3 F112 §C — user-level fallback preferences.
//!
//! Lives at `~/.ccteam/preferences.toml` (sibling of `config.yaml`).
//! Kept separate from `config.yaml` because:
//!
//! 1. **Scope** — `config.yaml` is the SoT for project registry +
//!    daemon-managed state; `preferences.toml` is purely user opt-in
//!    knobs (vendor fallback today; more knobs in V0.7+).
//! 2. **Format** — TOML matches how Cargo / rustfmt / cargo-mutants
//!    serialize their user configs; YAML is reserved for ccteam
//!    schema bodies the daemon edits.
//! 3. **Opt-in semantics** — defaults are *off*. Missing file = full
//!    defaults; corrupt file = warn + use defaults (never block the
//!    daemon on a busted preferences edit).
//!
//! ## Shape
//!
//! ```toml
//! [fallback]
//! on_claude_quota = "codex"        # "codex" | "off"  (default "off")
//!
//! [fallback.codex]
//! enabled_for_roles = ["main", "fixer", "explorer"]   # empty = all roles
//! ```
//!
//! ## Atomic save
//!
//! `save()` writes to `preferences.toml.tmp`, then renames into
//! place — same convention as `config.yaml` (no `.bak` because the
//! prefs file is small + lossless reproducible from prompts).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// File name relative to `paths.root` (`~/.ccteam/`).
pub const PREFERENCES_FILENAME: &str = "preferences.toml";

/// Top-level user preferences schema.
///
/// Every section is optional; `Preferences::default()` yields a
/// fully-defaulted (all-off) shape. Adding a new section in V0.7+
/// follows the same `#[serde(default)]` pattern so older files keep
/// parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Preferences {
    #[serde(default)]
    pub fallback: FallbackPrefs,
}

/// Cross-vendor fallback knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FallbackPrefs {
    /// What to do when a Claude `budget_exceeded` event lands.
    /// Default `Off` — no fallback (matches V0.5.x behaviour).
    #[serde(default)]
    pub on_claude_quota: OnClaudeQuota,

    /// Codex-specific fallback tuning (only consulted when
    /// `on_claude_quota == Codex`).
    #[serde(default)]
    pub codex: CodexFallbackPrefs,
}

/// What to do when Claude trips its 24h budget cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OnClaudeQuota {
    /// Hard-stop (V0.5.x default) — the orchestrator's existing
    /// F84 `auto_disable_workflow` path fires.
    #[default]
    Off,
    /// Try to keep the workflow alive by swapping the affected role's
    /// adapter to Codex on the next spawn. Requires `codex` on PATH
    /// + a `Vendor::Codex` adapter registered with the orchestrator.
    Codex,
}

/// Codex-specific fallback knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CodexFallbackPrefs {
    /// Roles eligible for Codex fallback. Empty list = every role is
    /// eligible (the simple default). Useful when the user only
    /// trusts Codex for, say, `critic`/`reviewer` and wants `main`
    /// to hard-stop instead.
    #[serde(default)]
    pub enabled_for_roles: Vec<String>,
}

/// Resolve `<paths.root>/preferences.toml`.
pub fn preferences_path(root: &Path) -> PathBuf {
    root.join(PREFERENCES_FILENAME)
}

/// Load preferences, returning defaults when the file is absent.
///
/// Parse / IO failures bubble up — the daemon decides whether to
/// surface the warning or fall back to defaults. The
/// `load_or_default` helper is the production entry point that
/// swallows errors with a `tracing::warn!` and returns defaults.
pub fn load(root: &Path) -> Result<Preferences> {
    let path = preferences_path(root);
    if !path.exists() {
        return Ok(Preferences::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read preferences at {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parse preferences at {}", path.display()))
}

/// Production entry: load preferences; on any error, log + return
/// defaults so a broken prefs file never wedges the orchestrator.
pub fn load_or_default(root: &Path) -> Preferences {
    match load(root) {
        Ok(prefs) => prefs,
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %preferences_path(root).display(),
                "preferences.toml unreadable; using defaults"
            );
            Preferences::default()
        }
    }
}

/// Atomically write preferences to `<root>/preferences.toml`.
pub fn save(root: &Path, prefs: &Preferences) -> Result<()> {
    std::fs::create_dir_all(root).with_context(|| format!("mkdir {}", root.display()))?;
    let raw = toml::to_string_pretty(prefs).context("serialize preferences")?;
    let path = preferences_path(root);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, raw.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

impl Preferences {
    /// True iff Codex fallback is globally enabled AND this role is on
    /// the eligibility list (or the list is empty = all roles).
    pub fn codex_fallback_enabled_for(&self, role: &str) -> bool {
        if !matches!(self.fallback.on_claude_quota, OnClaudeQuota::Codex) {
            return false;
        }
        let allowed = &self.fallback.codex.enabled_for_roles;
        allowed.is_empty() || allowed.iter().any(|r| r == role)
    }
}

/// V0.6.0 Wave 3 F112 §C — outcome of the orchestrator's quota
/// fallback decision. Combines the three orthogonal inputs (budget
/// tripped? claude executor? user opt-in?) into the action the
/// `try_spawn` path takes. Lives in `preferences.rs` (not
/// `orchestrator.rs`) so unit tests can drive it without bringing
/// up the full Orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaFallbackDecision {
    /// Budget not tripped — proceed with the spawn as normal.
    Proceed,
    /// Budget tripped + prefs opt-in + claude executor + role
    /// eligible — swap this spawn's adapter to Codex (workflow
    /// continues; vendor=claude tagged in the audit event).
    SwapToCodex,
    /// Budget tripped + no fallback option applies — hard-stop
    /// (V0.5.x default behaviour); orchestrator writes
    /// `budget_exceeded` + sends a `btw` escalation + returns
    /// without spawning.
    HardStop,
}

/// Pure helper for the V0.6.0 Wave 3 F112 §C decision table.
/// Inputs:
///
/// - `budget_tripped` — `cost_so_far >= budget_limit` (caller has
///   already done the comparison).
/// - `current_executor_is_claude` — `agent.executor == Executor::Claude`.
/// - `role` — agent role (used for prefs eligibility check).
/// - `prefs` — the user's `~/.ccteam/preferences.toml` body.
///
/// The orchestrator additionally requires a Codex adapter to be
/// registered before honouring `SwapToCodex`; that check stays in
/// `orchestrator.rs` because it touches the live adapter map.
pub fn quota_fallback_decision(
    budget_tripped: bool,
    current_executor_is_claude: bool,
    role: &str,
    prefs: &Preferences,
) -> QuotaFallbackDecision {
    if !budget_tripped {
        return QuotaFallbackDecision::Proceed;
    }
    if !current_executor_is_claude {
        // Budget tripped on a Codex-vendored agent — no Claude
        // fallback exists, so hard-stop.
        return QuotaFallbackDecision::HardStop;
    }
    if prefs.codex_fallback_enabled_for(role) {
        QuotaFallbackDecision::SwapToCodex
    } else {
        QuotaFallbackDecision::HardStop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_is_off() {
        let p = Preferences::default();
        assert_eq!(p.fallback.on_claude_quota, OnClaudeQuota::Off);
        assert!(p.fallback.codex.enabled_for_roles.is_empty());
        assert!(!p.codex_fallback_enabled_for("main"));
    }

    #[test]
    fn codex_fallback_eligibility_gates_on_role_list() {
        let mut p = Preferences::default();
        p.fallback.on_claude_quota = OnClaudeQuota::Codex;

        // Empty list = every role eligible.
        assert!(p.codex_fallback_enabled_for("main"));
        assert!(p.codex_fallback_enabled_for("anything"));

        // Restricted list = only listed roles.
        p.fallback.codex.enabled_for_roles = vec!["critic".into()];
        assert!(p.codex_fallback_enabled_for("critic"));
        assert!(!p.codex_fallback_enabled_for("main"));
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let mut p = Preferences::default();
        p.fallback.on_claude_quota = OnClaudeQuota::Codex;
        p.fallback.codex.enabled_for_roles = vec!["main".into(), "fixer".into()];

        save(tmp.path(), &p).unwrap();
        let back = load(tmp.path()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let tmp = TempDir::new().unwrap();
        let p = load(tmp.path()).unwrap();
        assert_eq!(p, Preferences::default());
    }

    #[test]
    fn invalid_toml_load_errors() {
        let tmp = TempDir::new().unwrap();
        let path = preferences_path(tmp.path());
        std::fs::write(&path, b"this is not valid = = toml").unwrap();
        assert!(load(tmp.path()).is_err());
    }

    #[test]
    fn invalid_toml_load_or_default_returns_defaults() {
        let tmp = TempDir::new().unwrap();
        let path = preferences_path(tmp.path());
        std::fs::write(&path, b"= invalid").unwrap();
        let p = load_or_default(tmp.path());
        assert_eq!(p, Preferences::default());
    }
}
