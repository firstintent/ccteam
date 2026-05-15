//! `ccteam-control` + `ccteam-team-author` skill installation (M1.8 + M0.22.1).
//!
//! Skills live at `~/.claude/skills/<name>/SKILL.md` (or under a plugin
//! tree). The `ccteam-control` and `ccteam-team-author` skill bodies are
//! shipped inside the binary and written to disk by
//! `ccteam doctor --install-skill`. The `--install-meta-agent` path also
//! calls this so a freshly-bootstrapped meta-agent has both skills on
//! first launch.
//!
//! The markdown bodies live under the repo-root `skills/` directory
//! (cross-dir `include_str!` into the ccteam-core binary).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::tool_surface::user_claude_dir;

/// Skill directory name. Embedded SKILL.md ships under this folder
/// inside `~/.claude/skills/`.
pub const CCTEAM_CONTROL_SKILL_NAME: &str = "ccteam-control";

/// V0.2 M0.22.1 — `ccteam-team-author` skill name. Walks the
/// meta-agent through the team-factory dialogue (phase list, tools,
/// golden rules, retro / verdict schema, plugin metadata) before
/// invoking `ccteam team init`.
pub const CCTEAM_TEAM_AUTHOR_SKILL_NAME: &str = "ccteam-team-author";

/// V0.2.2 F34 — `ccteam-project-creator` skill name. Walks the
/// meta-agent through the four-phase project-creation dialogue
/// (clarify brief → recommend slug → pick team → dispatch via
/// `ccteam new`). Replaces the inline `meta_agent_role.md` §2 dispatch
/// flow with a structured AskUserQuestion-driven UX.
pub const CCTEAM_PROJECT_CREATOR_SKILL_NAME: &str = "ccteam-project-creator";

/// V0.2.2 F44: legacy V0.2.2 (F39'd) skill directory names that
/// `ccteam doctor` migrates back. Exposed so the migration helper can
/// scan for stale `cct-*` installs without re-declaring the names.
pub const LEGACY_SKILL_NAMES: &[&str] = &["cct-control", "cct-team-author", "cct-project-creator"];

/// Embedded `SKILL.md` body. Written verbatim during install.
///
/// Skill bodies live at `<repo>/skills/ccteam-<name>/SKILL.md`
/// (top-level `skills/` directory) so they're authoritative outside
/// the Rust source tree.
pub const CCTEAM_CONTROL_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/ccteam-control/SKILL.md"
));

/// Embedded `SKILL.md` body for the team-author skill (V0.2 M0.22.1).
pub const CCTEAM_TEAM_AUTHOR_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/ccteam-team-author/SKILL.md"
));

/// Embedded `SKILL.md` body for the project-creator skill (V0.2.2 F34).
pub const CCTEAM_PROJECT_CREATOR_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/ccteam-project-creator/SKILL.md"
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

/// Install the `ccteam-control` skill into `~/.claude/skills/ccteam-control/SKILL.md`.
pub fn install_ccteam_control_skill(opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_into(&claude, opts)
}

/// V0.2 M0.22.1: install the `ccteam-team-author` skill into
/// `~/.claude/skills/ccteam-team-author/SKILL.md`. Same idempotent
/// semantics as `install_ccteam_control_skill`.
pub fn install_ccteam_team_author_skill(opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_skill_body_into(
        &claude,
        CCTEAM_TEAM_AUTHOR_SKILL_NAME,
        CCTEAM_TEAM_AUTHOR_SKILL_MD,
        opts,
    )
}

/// V0.2.2 F34: install the `ccteam-project-creator` skill into
/// `~/.claude/skills/ccteam-project-creator/SKILL.md`. Same idempotent
/// semantics as `install_ccteam_control_skill`. Carries the four-phase
/// project-creation dialogue the meta-agent now invokes instead of
/// inlining dispatch logic in `meta_agent_role.md` §2.
pub fn install_ccteam_project_creator_skill(
    opts: InstallSkillOptions,
) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_skill_body_into(
        &claude,
        CCTEAM_PROJECT_CREATOR_SKILL_NAME,
        CCTEAM_PROJECT_CREATOR_SKILL_MD,
        opts,
    )
}

/// Test-injectable variant: write into `<claude_dir>/skills/...` so unit
/// tests can point at a tempdir without mutating `$HOME/.claude/`.
pub fn install_into(claude_dir: &Path, opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    install_skill_body_into(
        claude_dir,
        CCTEAM_CONTROL_SKILL_NAME,
        CCTEAM_CONTROL_SKILL_MD,
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

    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(&target, body).with_context(|| format!("write {}", target.display()))?;
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
        // Frontmatter follows the official Anthropic skill-creator spec:
        // file starts with `---`, then `name` + `description` only.
        // ccteam-managed markers + `allowed-tools` were dropped in
        // V0.4.4 to align with skill-creator; F44 reverse migration's
        // `is_managed` check falls back to the canonical `name:` field.
        assert!(body.starts_with("---\nname: ccteam-control"));
        // Required body chapters per interfaces §11.3.
        for required in [
            "Capability index",
            "Typical workflows",
            "Decision principles",
            "What this skill cannot do",
        ] {
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
        let target = tmp.path().join("skills/ccteam-control/SKILL.md");
        std::fs::write(&target, "tampered\n").unwrap();
        let r = install_into(
            tmp.path(),
            InstallSkillOptions {
                force: true,
                dry_run: false,
            },
        )
        .unwrap();
        assert_eq!(r.action, SkillInstallAction::Replaced);
        let body = std::fs::read_to_string(&target).unwrap();
        assert!(body.contains("name: ccteam-control"));
    }

    #[test]
    fn dry_run_does_not_touch_filesystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = install_into(
            tmp.path(),
            InstallSkillOptions {
                force: false,
                dry_run: true,
            },
        )
        .unwrap();
        assert_eq!(r.action, SkillInstallAction::DryRun { would_write: true },);
        assert!(!r.target.exists());
    }

    // ---------------- V0.2 M0.22.1 team-author skill ----------------

    #[test]
    fn team_author_skill_installs_under_ccteam_team_author_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = install_skill_body_into(
            tmp.path(),
            CCTEAM_TEAM_AUTHOR_SKILL_NAME,
            CCTEAM_TEAM_AUTHOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(report.action, SkillInstallAction::Wrote);
        let body = std::fs::read_to_string(&report.target).unwrap();
        assert!(body.starts_with("---\nname: ccteam-team-author"));
        assert!(body.contains("Capability index"));
    }

    #[test]
    fn team_author_skill_install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_skill_body_into(
            tmp.path(),
            CCTEAM_TEAM_AUTHOR_SKILL_NAME,
            CCTEAM_TEAM_AUTHOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        let r2 = install_skill_body_into(
            tmp.path(),
            CCTEAM_TEAM_AUTHOR_SKILL_NAME,
            CCTEAM_TEAM_AUTHOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(r2.action, SkillInstallAction::AlreadyPresent);
    }

    // -------------- V0.2.2 F34 project-creator skill --------------

    #[test]
    fn project_creator_skill_installs_with_phases() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = install_skill_body_into(
            tmp.path(),
            CCTEAM_PROJECT_CREATOR_SKILL_NAME,
            CCTEAM_PROJECT_CREATOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(report.action, SkillInstallAction::Wrote);
        let body = std::fs::read_to_string(&report.target).unwrap();
        assert!(body.starts_with("---\nname: ccteam-project-creator"));
        // The four-phase dialogue is the body's main contract; if any
        // disappears, the meta-agent silently loses behavior.
        for phase in [
            "Phase A — Clarify",
            "Phase B — Recommend",
            "Phase C — Pick a team",
            "Phase D — Dispatch",
        ] {
            assert!(body.contains(phase), "missing phase: {phase}");
        }
        assert!(body.contains("AskUserQuestion"));
    }

    #[test]
    fn project_creator_skill_install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_skill_body_into(
            tmp.path(),
            CCTEAM_PROJECT_CREATOR_SKILL_NAME,
            CCTEAM_PROJECT_CREATOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        let r2 = install_skill_body_into(
            tmp.path(),
            CCTEAM_PROJECT_CREATOR_SKILL_NAME,
            CCTEAM_PROJECT_CREATOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(r2.action, SkillInstallAction::AlreadyPresent);
    }
}
