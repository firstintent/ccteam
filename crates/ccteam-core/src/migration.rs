//! V0.4.2 F74 — one-shot migrations.
//!
//! `migrate_v041_to_v042` consolidates two pieces of V0.4.1 state into
//! the V0.4.2 single config file:
//!
//! 1. Every project under `paths.projects_root` with a parseable
//!    `.ccteam/state.json` is appended to `config.yaml::projects[]`.
//!    Slugs already in the registry are left alone (idempotent on
//!    re-run).
//! 2. `~/.ccteam/watchdog.yaml` is folded into `config.yaml::watchdog`
//!    if present, and the old file is renamed to
//!    `watchdog.yaml.migrated` so a future re-run is a no-op.
//!
//! Returns a [`MigrationReport`] describing what was changed so the
//! `ccteam doctor` callsite can print a human-readable summary.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::{self, ProjectEntry};
use crate::paths::CcteamPaths;
use crate::state::ProjectState;
use crate::watchdog;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MigrationReport {
    /// Slugs newly appended to `config.yaml::projects[]`.
    pub registered_slugs: Vec<String>,
    /// Slugs that were already in the registry — no-op for them.
    pub already_registered: Vec<String>,
    /// `.ccteam/state.json` dirs scanned but skipped (corrupt /
    /// missing fields).
    pub skipped_paths: Vec<PathBuf>,
    /// `true` iff `watchdog.yaml` existed and was folded into
    /// `config.yaml::watchdog`.
    pub watchdog_folded: bool,
    /// Where the previous `watchdog.yaml` was renamed to (only set
    /// when `watchdog_folded == true`).
    pub watchdog_archived_at: Option<PathBuf>,
}

/// Fold V0.4.1 state files into V0.4.2 `~/.ccteam/config.yaml`.
/// Idempotent — re-runs over a fully-migrated home are a no-op
/// (everything reported as `already_registered`, `watchdog_folded =
/// false`).
pub fn migrate_v041_to_v042(paths: &CcteamPaths) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();

    // -- Part 1: projects[] migration ---------------------------------
    let mut cfg = config::load(&paths.root).context("load config.yaml before migration")?;
    let known: std::collections::HashSet<String> =
        cfg.projects.iter().map(|p| p.slug.clone()).collect();

    if paths.projects_root.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&paths.projects_root)
            .with_context(|| format!("read_dir {}", paths.projects_root.display()))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(slug) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            let state_path = entry.path().join(".ccteam").join("state.json");
            if !state_path.exists() {
                continue;
            }
            if known.contains(&slug) {
                report.already_registered.push(slug);
                continue;
            }
            let state = match ProjectState::load(&state_path) {
                Ok(s) => s,
                Err(_) => {
                    report.skipped_paths.push(state_path);
                    continue;
                }
            };
            cfg.projects.push(ProjectEntry {
                slug: state.slug.clone(),
                path: entry.path(),
                team: state.team.clone(),
                installed_at: state.created_at,
            });
            report.registered_slugs.push(state.slug);
        }
    }

    // -- Part 2: watchdog.yaml fold -----------------------------------
    let watchdog_path = paths.root.join(watchdog::WATCHDOG_CONFIG_FILENAME);
    if watchdog_path.exists() && cfg.watchdog.is_none() {
        let parsed = watchdog::load_config_at(&watchdog_path)
            .with_context(|| format!("parse {}", watchdog_path.display()))?;
        cfg.watchdog = Some(parsed);
        report.watchdog_folded = true;
    }

    // Persist only when something changed (touching the file rotates
    // the .bak even for a no-op, which is wasteful on a fresh box).
    let changed = !report.registered_slugs.is_empty() || report.watchdog_folded;
    if changed {
        config::save(&paths.root, &cfg).context("save migrated config.yaml")?;
    }

    // Rename watchdog.yaml AFTER the config save so a crash leaves the
    // user one of {old file, new file}, never neither.
    if report.watchdog_folded {
        let archived = watchdog_path.with_extension("yaml.migrated");
        std::fs::rename(&watchdog_path, &archived).with_context(|| {
            format!(
                "rename {} → {}",
                watchdog_path.display(),
                archived.display(),
            )
        })?;
        report.watchdog_archived_at = Some(archived);
    }

    Ok(report)
}

/// Render a `MigrationReport` to a stable human-readable block. Used
/// by `ccteam doctor --migrate-v041-to-v042`.
pub fn render_migration_report(report: &MigrationReport) -> String {
    let mut out = String::from("ccteam doctor --migrate-v041-to-v042\n\n");
    out.push_str(&format!(
        "  newly registered:    {} project(s)\n",
        report.registered_slugs.len()
    ));
    for slug in &report.registered_slugs {
        out.push_str(&format!("                       - {slug}\n"));
    }
    out.push_str(&format!(
        "  already registered:  {}\n",
        report.already_registered.len()
    ));
    out.push_str(&format!(
        "  skipped (corrupt):   {}\n",
        report.skipped_paths.len()
    ));
    for path in &report.skipped_paths {
        out.push_str(&format!("                       - {}\n", path.display()));
    }
    out.push_str(&format!(
        "  watchdog.yaml:       {}\n",
        if report.watchdog_folded {
            "folded into config.yaml"
        } else {
            "no change"
        },
    ));
    if let Some(p) = &report.watchdog_archived_at {
        out.push_str(&format!("  archived at:         {}\n", p.display()));
    }
    out.push_str(&format!("  config.yaml:         {}\n", "updated"));
    out.push_str("\nrerun is safe — already-registered slugs are skipped.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> CcteamPaths {
        CcteamPaths {
            root: tmp.path().join(".ccteam"),
            projects_root: tmp.path().join("projects"),
        }
    }

    fn make_v041_project(paths: &CcteamPaths, slug: &str, team: &str) {
        let dir = paths.project_dir(slug);
        std::fs::create_dir_all(paths.project_ccteam_dir(slug)).unwrap();
        let mut state = ProjectState::initial_for_team(slug.into(), team.into());
        state.tmux_session = format!("ccteam-{slug}");
        state.save(&paths.project_state(slug)).unwrap();
        // Touch the project dir so paths.project_dir resolves.
        assert!(dir.is_dir());
    }

    #[test]
    fn migrate_appends_every_legacy_project_to_config_yaml() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        make_v041_project(&p, "alpha", "dev");
        make_v041_project(&p, "beta", "research");

        let report = migrate_v041_to_v042(&p).unwrap();
        assert_eq!(report.registered_slugs, vec!["alpha", "beta"]);
        assert!(report.already_registered.is_empty());

        let cfg = config::load(&p.root).unwrap();
        let slugs: Vec<&str> = cfg.projects.iter().map(|e| e.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha", "beta"]);
        assert_eq!(cfg.projects[0].team, "dev");
        assert_eq!(cfg.projects[1].team, "research");
    }

    #[test]
    fn migrate_is_idempotent_on_rerun() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        make_v041_project(&p, "alpha", "dev");

        let r1 = migrate_v041_to_v042(&p).unwrap();
        assert_eq!(r1.registered_slugs.len(), 1);
        let r2 = migrate_v041_to_v042(&p).unwrap();
        assert!(r2.registered_slugs.is_empty());
        assert_eq!(r2.already_registered, vec!["alpha"]);
    }

    #[test]
    fn migrate_folds_watchdog_yaml_and_archives_it() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        std::fs::create_dir_all(&p.root).unwrap();
        let watchdog_path = p.root.join(watchdog::WATCHDOG_CONFIG_FILENAME);
        std::fs::write(
            &watchdog_path,
            "notify_on_cycle_count: 5\nnotify_mode: verbose\n",
        )
        .unwrap();

        let report = migrate_v041_to_v042(&p).unwrap();
        assert!(report.watchdog_folded);
        assert!(report.watchdog_archived_at.is_some());

        let cfg = config::load(&p.root).unwrap();
        let w = cfg.watchdog.expect("watchdog folded");
        assert_eq!(w.notify_on_cycle_count, 5);
        assert!(matches!(w.notify_mode, watchdog::NotifyMode::Verbose));

        assert!(!watchdog_path.exists(), "old watchdog.yaml must be archived");
        let archived = watchdog_path.with_extension("yaml.migrated");
        assert!(archived.is_file());
    }

    #[test]
    fn migrate_skips_watchdog_fold_when_config_already_has_watchdog() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        std::fs::create_dir_all(&p.root).unwrap();
        // Pre-seed config.yaml with a watchdog block.
        let cfg = config::CcteamConfig {
            watchdog: Some(watchdog::WatchdogConfig {
                notify_on_cycle_count: 7,
                ..Default::default()
            }),
            ..Default::default()
        };
        config::save(&p.root, &cfg).unwrap();
        // Also drop a stale watchdog.yaml.
        let watchdog_path = p.root.join(watchdog::WATCHDOG_CONFIG_FILENAME);
        std::fs::write(&watchdog_path, "notify_on_cycle_count: 99\n").unwrap();

        let report = migrate_v041_to_v042(&p).unwrap();
        assert!(!report.watchdog_folded);
        // Old file stays — migration doesn't overwrite user choice.
        assert!(watchdog_path.is_file());
        let cfg_after = config::load(&p.root).unwrap();
        assert_eq!(cfg_after.watchdog.unwrap().notify_on_cycle_count, 7);
    }

    #[test]
    fn migrate_reports_skipped_for_corrupt_state_json() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        std::fs::create_dir_all(p.project_ccteam_dir("corrupt")).unwrap();
        std::fs::write(p.project_state("corrupt"), "{not json").unwrap();

        let report = migrate_v041_to_v042(&p).unwrap();
        assert!(report.registered_slugs.is_empty());
        assert_eq!(report.skipped_paths.len(), 1);
    }

    #[test]
    fn migrate_empty_home_is_a_clean_noop() {
        let tmp = TempDir::new().unwrap();
        let p = paths(&tmp);
        let report = migrate_v041_to_v042(&p).unwrap();
        assert!(report.registered_slugs.is_empty());
        assert!(!report.watchdog_folded);
        // No config.yaml should have been written (nothing to write).
        assert!(!config::config_path(&p.root).exists());
    }
}
