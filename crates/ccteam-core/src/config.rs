//! V0.4.2 F73 — global ccteam configuration file `~/.ccteam/config.yaml`.
//!
//! Single source of truth for user-level preferences AND the project
//! registry. Replaces (and consolidates) the V0.4.1 layout where
//! `projects_root` came only from env vars and project discovery
//! relied on walking the filesystem.
//!
//! ## Shape
//!
//! ```yaml
//! projects_root: ~/projects        # optional; default ~/projects
//! projects:                         # canonical SoT for daemon roster
//!   - slug: myapp
//!     path: /home/rob/code/my-fastapi-app
//!     team: dev
//!     installed_at: 2026-05-15T14:00:00Z
//! ```
//!
//! ## Read priority (CcteamPaths::from_env)
//!
//! 1. `CCTEAM_PROJECTS_ROOT` env (ad-hoc / test override)
//! 2. `~/.ccteam/config.yaml::projects_root`
//! 3. `~/projects` (hardcoded default)
//!
//! ## Atomic save
//!
//! `save()` writes to `config.yaml.tmp`, renames into place, and copies
//! the prior contents to `config.yaml.bak` first — same shape as
//! `ProjectState::save` so a crash mid-write doesn't corrupt the SoT.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// File name relative to `paths.root` (`~/.ccteam/`).
pub const CONFIG_FILENAME: &str = "config.yaml";

/// Top-level config schema. Future fields plug in as their own
/// optional sections without breaking existing files — `serde(default)`
/// on every collection guarantees an older config.yaml still parses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CcteamConfig {
    /// Canonical base for `ccteam init --in <slug>`. When absent,
    /// `CcteamPaths::from_env` falls back to `$HOME/projects`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_root: Option<PathBuf>,

    /// Every project under ccteam management — daemon roster reads
    /// this list instead of walking the filesystem. Empty on a fresh
    /// install; `ccteam init` appends one entry per successful install.
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,

    /// V0.4.2 F74: watchdog tunables, folded in from the legacy
    /// `~/.ccteam/watchdog.yaml`. When absent, watchdog uses defaults
    /// (or, on V0.4.1 systems pre-migration, falls back to reading
    /// `watchdog.yaml` directly).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<crate::watchdog::WatchdogConfig>,

    /// V0.4.6 F85: how many days a terminated `~/.claude/jobs/<id>/`
    /// directory may live before the daemon's startup GC sweep (or
    /// `ccteam doctor --gc-claude-jobs --apply`) reclaims it. Default
    /// 7 days. Setting `0` disables GC entirely (every entry is
    /// preserved), which is useful for forensic captures or shared
    /// hosts where ccteam shouldn't touch sibling tools' state.
    #[serde(default = "default_claude_jobs_retention_days")]
    pub claude_jobs_retention_days: u32,

    /// v0.9.0 W2 (F5) — delegation guardrails. Absent → all documented
    /// defaults (zero-config runs safely). Global engine policy the gateway
    /// enforces on every agent-initiated (Ambient) spawn/dispatch.
    #[serde(default, skip_serializing_if = "DelegationConfig::is_default")]
    pub delegation: DelegationConfig,
}

/// v0.9.0 W2 (F5) — delegation guardrail knobs. Every field defaults, so an
/// absent `delegation:` section (or absent individual keys) yields the
/// documented anti-runaway posture without any config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegationConfig {
    /// Max delegation depth. A delegated child's depth is `parent.depth + 1`
    /// (a human-created session is depth 0); a spawn that would exceed this is
    /// rejected.
    #[serde(default = "default_delegation_max_depth")]
    pub max_depth: u32,
    /// Max active (non-stopped) DIRECT children a single parent may hold.
    #[serde(default = "default_delegation_max_children")]
    pub max_children: u32,
    /// Max active delegated sessions (any `parent_sid`) in one project — the
    /// runaway-minting ceiling.
    #[serde(default = "default_delegation_max_delegated")]
    pub max_delegated: u32,
}

pub fn default_delegation_max_depth() -> u32 {
    2
}
pub fn default_delegation_max_children() -> u32 {
    5
}
pub fn default_delegation_max_delegated() -> u32 {
    16
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            max_depth: default_delegation_max_depth(),
            max_children: default_delegation_max_children(),
            max_delegated: default_delegation_max_delegated(),
        }
    }
}

impl DelegationConfig {
    /// True when this equals the built-in default posture — lets the config
    /// writer omit the section so an untouched `config.yaml` stays byte-stable.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Default value for `claude_jobs_retention_days` when the field is
/// absent from `config.yaml`. Kept as a free function so serde's
/// `#[serde(default = "...")]` can reference it.
pub fn default_claude_jobs_retention_days() -> u32 {
    7
}

impl Default for CcteamConfig {
    fn default() -> Self {
        Self {
            projects_root: None,
            projects: Vec::new(),
            watchdog: None,
            claude_jobs_retention_days: default_claude_jobs_retention_days(),
            delegation: DelegationConfig::default(),
        }
    }
}

/// One project registry entry. `path` is absolute; `team` mirrors
/// `state.json::team` so the registry can answer `ccteam ls` without
/// loading every state.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectEntry {
    pub slug: String,
    pub path: PathBuf,
    pub team: String,
    pub installed_at: DateTime<Utc>,
}

/// Absolute path to the config file under the given `~/.ccteam/`
/// root. Pure path arithmetic; never touches disk.
pub fn config_path(ccteam_root: &Path) -> PathBuf {
    ccteam_root.join(CONFIG_FILENAME)
}

/// Load `<root>/config.yaml`. Missing file → `Default::default()`
/// (zero projects, no `projects_root` override). An empty file (e.g.
/// the user `touch`ed it) is also treated as defaults.
///
/// Parse errors propagate — a corrupt config.yaml is a fail-loud
/// condition (we don't silently fall back to defaults because that
/// would erase the user's registry on a YAML typo).
pub fn load(ccteam_root: &Path) -> Result<CcteamConfig> {
    let path = config_path(ccteam_root);
    if !path.exists() {
        return Ok(CcteamConfig::default());
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(CcteamConfig::default());
    }
    serde_yaml::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Persist `cfg` atomically. Steps:
///
/// 1. Ensure `<root>/` exists.
/// 2. If `config.yaml` already exists, copy it to `config.yaml.bak`.
/// 3. Write serialized YAML to `config.yaml.tmp`.
/// 4. `rename` tmp → final.
///
/// This mirrors `ProjectState::save` so a crash between steps leaves
/// either the prior `.bak` or the next-version `.tmp` recoverable.
pub fn save(ccteam_root: &Path, cfg: &CcteamConfig) -> Result<()> {
    std::fs::create_dir_all(ccteam_root)
        .with_context(|| format!("create {}", ccteam_root.display()))?;
    let path = config_path(ccteam_root);
    let yaml = serde_yaml::to_string(cfg).context("serialize ccteam config")?;

    if path.exists() {
        let bak = path.with_extension("yaml.bak");
        std::fs::copy(&path, &bak)
            .with_context(|| format!("backup {} → {}", path.display(), bak.display()))?;
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Append `entry` to `config.yaml::projects`. Fails loud on slug
/// collision — the caller (e.g. `ccteam init`) should detect the
/// collision earlier so the user gets a clearer error, but this is
/// the last line of defense.
pub fn append_project(ccteam_root: &Path, entry: ProjectEntry) -> Result<()> {
    let mut cfg = load(ccteam_root)?;
    if cfg.projects.iter().any(|p| p.slug == entry.slug) {
        return Err(anyhow!(
            "slug `{}` already registered in {}",
            entry.slug,
            config_path(ccteam_root).display()
        ));
    }
    cfg.projects.push(entry);
    save(ccteam_root, &cfg)
}

/// Update or insert `entry`. Used by `ccteam init` re-runs against an
/// already-registered slug — refresh `path` / `team` / `installed_at`
/// without erroring on collision.
pub fn upsert_project(ccteam_root: &Path, entry: ProjectEntry) -> Result<()> {
    let mut cfg = load(ccteam_root)?;
    if let Some(existing) = cfg.projects.iter_mut().find(|p| p.slug == entry.slug) {
        *existing = entry;
    } else {
        cfg.projects.push(entry);
    }
    save(ccteam_root, &cfg)
}

/// Remove `slug` from the registry. Returns `true` iff the slug was
/// present.
pub fn remove_project(ccteam_root: &Path, slug: &str) -> Result<bool> {
    let mut cfg = load(ccteam_root)?;
    let before = cfg.projects.len();
    cfg.projects.retain(|p| p.slug != slug);
    if cfg.projects.len() == before {
        return Ok(false);
    }
    save(ccteam_root, &cfg)?;
    Ok(true)
}

/// Find a registered project by slug.
pub fn lookup_project(ccteam_root: &Path, slug: &str) -> Result<Option<ProjectEntry>> {
    let cfg = load(ccteam_root)?;
    Ok(cfg.projects.into_iter().find(|p| p.slug == slug))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn sample_entry(slug: &str, path: &Path) -> ProjectEntry {
        ProjectEntry {
            slug: slug.into(),
            path: path.to_path_buf(),
            team: "dev".into(),
            installed_at: now(),
        }
    }

    #[test]
    fn load_returns_default_on_missing_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = load(tmp.path()).unwrap();
        assert!(cfg.projects_root.is_none());
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn load_returns_default_on_empty_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(config_path(tmp.path()), "").unwrap();
        let cfg = load(tmp.path()).unwrap();
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let entry = sample_entry("foo", &PathBuf::from("/home/rob/code/foo"));
        let cfg = CcteamConfig {
            projects_root: Some(PathBuf::from("/work/repos")),
            projects: vec![entry.clone()],
            watchdog: None,
            claude_jobs_retention_days: default_claude_jobs_retention_days(),
            delegation: DelegationConfig::default(),
        };
        save(tmp.path(), &cfg).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn save_writes_bak_on_overwrite() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &CcteamConfig::default()).unwrap();
        let entry = sample_entry("bar", &PathBuf::from("/x/bar"));
        save(
            tmp.path(),
            &CcteamConfig {
                projects: vec![entry],
                ..Default::default()
            },
        )
        .unwrap();
        let bak = config_path(tmp.path()).with_extension("yaml.bak");
        assert!(
            bak.is_file(),
            "save must keep a .bak after the first overwrite"
        );
    }

    #[test]
    fn append_project_rejects_collision() {
        let tmp = TempDir::new().unwrap();
        let entry = sample_entry("dup", &PathBuf::from("/x/dup"));
        append_project(tmp.path(), entry.clone()).unwrap();
        let err = append_project(tmp.path(), entry).unwrap_err();
        assert!(format!("{err:#}").contains("already registered"));
    }

    #[test]
    fn upsert_project_overwrites_existing_entry() {
        let tmp = TempDir::new().unwrap();
        let first = sample_entry("foo", &PathBuf::from("/old/path"));
        upsert_project(tmp.path(), first).unwrap();
        let updated = sample_entry("foo", &PathBuf::from("/new/path"));
        upsert_project(tmp.path(), updated.clone()).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0], updated);
    }

    #[test]
    fn remove_project_returns_true_on_hit_false_on_miss() {
        let tmp = TempDir::new().unwrap();
        append_project(tmp.path(), sample_entry("a", &PathBuf::from("/x/a"))).unwrap();
        assert!(remove_project(tmp.path(), "a").unwrap());
        assert!(!remove_project(tmp.path(), "a").unwrap());
    }

    #[test]
    fn lookup_project_finds_or_returns_none() {
        let tmp = TempDir::new().unwrap();
        let e = sample_entry("hit", &PathBuf::from("/x/hit"));
        append_project(tmp.path(), e.clone()).unwrap();
        assert_eq!(lookup_project(tmp.path(), "hit").unwrap(), Some(e));
        assert_eq!(lookup_project(tmp.path(), "miss").unwrap(), None);
    }

    #[test]
    fn load_fails_loud_on_garbled_yaml() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(config_path(tmp.path()), "projects: [not a list\n").unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("config.yaml"));
    }
}
