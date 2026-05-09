//! `cct-control` + `cct-team-author` skill installation (M1.8 + M0.22.1).
//!
//! Skills live at `~/.claude/skills/<name>/SKILL.md` (or under a plugin
//! tree). The `cct-control` and `cct-team-author` skill bodies are
//! shipped inside the binary and written to disk by
//! `cct doctor --install-skill`. The `--install-meta-agent` path also
//! calls this so a freshly-bootstrapped meta-agent has both skills on
//! first launch.
//!
//! V0.2.2 F39: skill names migrated from `ccteam-{control,team-author}`
//! to `cct-{control,team-author}`; the markdown bodies live under the
//! repo-root `skills/` directory (cross-dir `include_str!` into the
//! ccteam-core binary). Migration of V0.1/V0.2 user installs is handled
//! by `cct doctor` (see `migrate_legacy_skill_dirs` in tool_surface).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::tool_surface::user_claude_dir;

/// Skill directory name. Embedded SKILL.md ships under this folder
/// inside `~/.claude/skills/`. V0.2.2 F39: `ccteam-control` → `cct-control`.
pub const CCT_CONTROL_SKILL_NAME: &str = "cct-control";

/// V0.2 M0.22.1 — `cct-team-author` skill name. Walks the
/// meta-agent through the team-factory dialogue (phase list, tools,
/// golden rules, retro / verdict schema, plugin metadata) before
/// invoking `cct team init`.
///
/// V0.2.2 F39: renamed from `ccteam-team-author`.
pub const CCT_TEAM_AUTHOR_SKILL_NAME: &str = "cct-team-author";

/// V0.2.2 F39: legacy V0.1/V0.2 skill directory names that
/// `cct doctor` migrates. Exposed so the migration helper can scan
/// for stale installs without re-declaring the names.
pub const LEGACY_SKILL_NAMES: &[&str] = &["ccteam-control", "ccteam-team-author"];

/// Embedded `SKILL.md` body. Written verbatim during install.
///
/// V0.2.2 F39: skill bodies live at `<repo>/skills/cct-<name>/SKILL.md`
/// (top-level `skills/` directory) so they're authoritative outside
/// the Rust source tree.
pub const CCT_CONTROL_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/cct-control/SKILL.md"
));

/// Embedded `SKILL.md` body for the team-author skill (V0.2 M0.22.1).
pub const CCT_TEAM_AUTHOR_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/cct-team-author/SKILL.md"
));

#[derive(Debug, Clone, Copy, Default)]
pub struct InstallSkillOptions {
    /// Write even when the file is already there. Default: false (skip
    /// to preserve any operator hand-edits).
    pub force: bool,
    /// Don't actually write; report what would happen.
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct InstallSkillReport {
    pub target: PathBuf,
    pub action: SkillInstallAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillInstallAction {
    Wrote,
    AlreadyPresent,
    Replaced,
    DryRun { would_write: bool },
}

/// Install the `cct-control` skill into `~/.claude/skills/cct-control/SKILL.md`.
pub fn install_cct_control_skill(opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_into(&claude, opts)
}

/// V0.2 M0.22.1: install the `cct-team-author` skill into
/// `~/.claude/skills/cct-team-author/SKILL.md`. Same idempotent
/// semantics as `install_cct_control_skill`.
pub fn install_cct_team_author_skill(
    opts: InstallSkillOptions,
) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_skill_body_into(
        &claude,
        CCT_TEAM_AUTHOR_SKILL_NAME,
        CCT_TEAM_AUTHOR_SKILL_MD,
        opts,
    )
}

/// Test-injectable variant: write into `<claude_dir>/skills/...` so unit
/// tests can point at a tempdir without mutating `$HOME/.claude/`.
pub fn install_into(claude_dir: &Path, opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    install_skill_body_into(
        claude_dir,
        CCT_CONTROL_SKILL_NAME,
        CCT_CONTROL_SKILL_MD,
        opts,
    )
}

/// Shared idempotent writer used by both install_into (control) and
/// the team-author install path. Skill name selects the `<claude_dir>/skills/<name>/`
/// directory; body is the verbatim SKILL.md contents.
pub fn install_skill_body_into(
    claude_dir: &Path,
    skill_name: &str,
    body: &str,
    opts: InstallSkillOptions,
) -> Result<InstallSkillReport> {
    let dir = claude_dir.join("skills").join(skill_name);
    let target = dir.join("SKILL.md");
    let exists = target.exists();

    if opts.dry_run {
        return Ok(InstallSkillReport {
            target,
            action: SkillInstallAction::DryRun {
                would_write: !exists || opts.force,
            },
        });
    }

    if exists && !opts.force {
        return Ok(InstallSkillReport {
            target,
            action: SkillInstallAction::AlreadyPresent,
        });
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(&target, body)
        .with_context(|| format!("write {}", target.display()))?;
    Ok(InstallSkillReport {
        target,
        action: if exists {
            SkillInstallAction::Replaced
        } else {
            SkillInstallAction::Wrote
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_skill_md_with_yaml_frontmatter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = install_into(tmp.path(), InstallSkillOptions::default()).unwrap();
        assert_eq!(report.action, SkillInstallAction::Wrote);
        assert!(report.target.exists());
        let body = std::fs::read_to_string(&report.target).unwrap();
        // V0.2.2 F39: bodies are wrapped in `<!-- ccteam-managed:skill begin/end -->`
        // markers; the YAML front matter follows the open marker.
        assert!(
            body.contains("<!-- ccteam-managed:skill begin -->"),
            "must contain ccteam-managed begin marker (F39 migration prerequisite)",
        );
        assert!(
            body.contains("<!-- ccteam-managed:skill end -->"),
            "must contain ccteam-managed end marker",
        );
        assert!(body.contains("---\nname: cct-control"));
        assert!(body.contains("allowed-tools: [Bash]"));
        // Required body chapters per interfaces §11.3.
        for required in ["Capability index", "Typical workflows", "Decision principles", "What this skill cannot do"] {
            assert!(body.contains(required), "missing chapter: {required}");
        }
    }

    #[test]
    fn install_is_idempotent_without_force() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_into(tmp.path(), InstallSkillOptions::default()).unwrap();
        let r2 = install_into(tmp.path(), InstallSkillOptions::default()).unwrap();
        assert_eq!(r2.action, SkillInstallAction::AlreadyPresent);
    }

    #[test]
    fn force_overwrites_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_into(tmp.path(), InstallSkillOptions::default()).unwrap();
        let target = tmp.path().join("skills/cct-control/SKILL.md");
        std::fs::write(&target, "tampered\n").unwrap();
        let r = install_into(tmp.path(), InstallSkillOptions { force: true, dry_run: false }).unwrap();
        assert_eq!(r.action, SkillInstallAction::Replaced);
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("name: cct-control"));
    }

    #[test]
    fn dry_run_does_not_touch_filesystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = install_into(
            tmp.path(),
            InstallSkillOptions { force: false, dry_run: true },
        )
        .unwrap();
        assert_eq!(
            r.action,
            SkillInstallAction::DryRun { would_write: true },
        );
        assert!(!r.target.exists());
    }

    // ---------------- V0.2 M0.22.1 team-author skill ----------------

    #[test]
    fn team_author_skill_installs_under_cct_team_author_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = install_skill_body_into(
            tmp.path(),
            CCT_TEAM_AUTHOR_SKILL_NAME,
            CCT_TEAM_AUTHOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(report.action, SkillInstallAction::Wrote);
        let body = std::fs::read_to_string(&report.target).unwrap();
        assert!(body.contains("<!-- ccteam-managed:skill begin -->"));
        assert!(body.contains("name: cct-team-author"));
        assert!(body.contains("Capability index"));
    }

    #[test]
    fn team_author_skill_install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_skill_body_into(
            tmp.path(),
            CCT_TEAM_AUTHOR_SKILL_NAME,
            CCT_TEAM_AUTHOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        let r2 = install_skill_body_into(
            tmp.path(),
            CCT_TEAM_AUTHOR_SKILL_NAME,
            CCT_TEAM_AUTHOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(r2.action, SkillInstallAction::AlreadyPresent);
    }
}
