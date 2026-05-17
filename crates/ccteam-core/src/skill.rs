//! ccteam-shipped skill installation.
//!
//! Skills live at `~/.claude/skills/<name>/SKILL.md` (or under a plugin
//! tree). The shipped skill bodies are baked into the binary and written
//! to disk by `ccteam doctor --install-skill`. The
//! `--install-meta-agent` path also calls this so a freshly-bootstrapped
//! meta-agent has all skills on first launch.
//!
//! The markdown bodies live under the repo-root `skills/` directory
//! (cross-dir `include_str!` into the ccteam-core binary).
//!
//! V0.5.0 F100 — 5 skills reduced to 3:
//! `ccteam-control` (CLI/MCP wrap),
//! `ccteam-creator` (new project + workflow/agent/skill scaffold),
//! `ccteam-team` (`/ccteam-team` in current session).
//! The deleted `ccteam-team-author` (V0.2 team plugin factory) and
//! `ccteam-project-creator` (V0.2.2 four-phase dispatch dialogue) bodies
//! are folded into `ccteam-creator` step 1/2/3/4 and the
//! `/ccteam-team` skill respectively.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::tool_surface::user_claude_dir;

/// Skill directory name for the CLI/MCP control wrap.
pub const CCTEAM_CONTROL_SKILL_NAME: &str = "ccteam-control";

/// V0.5.0 F100 — `ccteam-creator` skill name. Walks the user through
/// creating a new ccteam project (step 1/2/3/4 dialogue) and/or
/// authoring a `workflow.yaml` + `.claude/agents/<role>.md` +
/// optional project-local skills inside an existing project. Replaces
/// the V0.2.2 `ccteam-project-creator` (project creation dialogue) and
/// the V0.2 `ccteam-team-author` (team plugin factory; the factory
/// itself was deleted in V0.5.0 F100).
pub const CCTEAM_CREATOR_SKILL_NAME: &str = "ccteam-creator";

/// V0.5.0 F93a — `ccteam-team` skill name. Primary path for the
/// agent-team mode: `/ccteam-team "<task>"` in the user's current
/// Claude session spawns an Anthropic Agent Team in-process without
/// any ccteam workflow.yaml / `ccteam init` step. SKILL.md ships under
/// `<repo>/skills/ccteam-team/SKILL.md`.
pub const CCTEAM_TEAM_SKILL_NAME: &str = "ccteam-team";

/// V0.2.2 F44 + V0.5.0 F100: legacy skill directory names that
/// `ccteam doctor` migrates back when found.
///
/// - `cct-*` are the V0.2.2 F39-era pre-rename names (reverted in F44).
/// - `ccteam-team-author` and `ccteam-project-creator` are the V0.5.0
///   F100 deletions (folded into `ccteam-creator`). They remain in this
///   list so an upgrading user has the stale `~/.claude/skills/<old>/`
///   directories cleaned up.
pub const LEGACY_SKILL_NAMES: &[&str] = &[
    "cct-control",
    "cct-team-author",
    "cct-project-creator",
    "ccteam-team-author",
    "ccteam-project-creator",
];

/// Embedded `SKILL.md` body. Written verbatim during install.
///
/// Skill bodies live at `<repo>/skills/ccteam-<name>/SKILL.md`
/// (top-level `skills/` directory) so they're authoritative outside
/// the Rust source tree.
pub const CCTEAM_CONTROL_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/ccteam-control/SKILL.md"
));

/// V0.5.0 F100 — embedded `SKILL.md` body for `ccteam-creator`. The
/// merged body covers both new-project dispatch and in-project
/// workflow/agent/skill scaffolding.
pub const CCTEAM_CREATOR_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/ccteam-creator/SKILL.md"
));

/// Embedded `SKILL.md` body for the V0.5.0 F93a `/ccteam-team` skill.
pub const CCTEAM_TEAM_SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../skills/ccteam-team/SKILL.md"
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

/// V0.5.0 F100: install the merged `ccteam-creator` skill into
/// `~/.claude/skills/ccteam-creator/SKILL.md`. Same idempotent semantics
/// as `install_ccteam_control_skill`. The body absorbs the deleted
/// `ccteam-project-creator` step 1/2/3/4 new-project dialogue and the
/// in-project workflow/agent/skill scaffolding dialogue.
pub fn install_ccteam_creator_skill(opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_skill_body_into(
        &claude,
        CCTEAM_CREATOR_SKILL_NAME,
        CCTEAM_CREATOR_SKILL_MD,
        opts,
    )
}

/// V0.5.0 F93a: install the `ccteam-team` skill into
/// `~/.claude/skills/ccteam-team/SKILL.md`. Same idempotent semantics
/// as the other shipped skills. Entry point for the V0.5.0 primary
/// path — once installed, `/ccteam-team "<task>"` in any Claude
/// session in any git repo spawns an Anthropic Agent Team in-process.
pub fn install_ccteam_team_skill(opts: InstallSkillOptions) -> Result<InstallSkillReport> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_skill_body_into(&claude, CCTEAM_TEAM_SKILL_NAME, CCTEAM_TEAM_SKILL_MD, opts)
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

/// Shared idempotent writer used by every `install_*_skill` entry point.
/// Skill name selects the `<claude_dir>/skills/<name>/` directory; body
/// is the verbatim SKILL.md contents.
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

    // ----------------- V0.5.0 F100 ccteam-creator skill -----------------

    #[test]
    fn ccteam_creator_skill_installs_under_canonical_dir() {
        // V0.5.0 F100: ccteam-creator is the merged successor to the
        // V0.2 `ccteam-team-author` and the V0.2.2 `ccteam-project-creator`.
        // The body must carry the canonical frontmatter so Claude Code's
        // skill loader picks it up, plus the step 1/2/3/4 dialogue
        // headings the dispatch flow relies on.
        let tmp = tempfile::TempDir::new().unwrap();
        let report = install_skill_body_into(
            tmp.path(),
            CCTEAM_CREATOR_SKILL_NAME,
            CCTEAM_CREATOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(report.action, SkillInstallAction::Wrote);
        let body = std::fs::read_to_string(&report.target).unwrap();
        assert!(body.starts_with("---\nname: ccteam-creator"));
    }

    #[test]
    fn ccteam_creator_skill_install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_skill_body_into(
            tmp.path(),
            CCTEAM_CREATOR_SKILL_NAME,
            CCTEAM_CREATOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        let r2 = install_skill_body_into(
            tmp.path(),
            CCTEAM_CREATOR_SKILL_NAME,
            CCTEAM_CREATOR_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(r2.action, SkillInstallAction::AlreadyPresent);
    }

    // ----------------- V0.5.0 F93a ccteam-team skill -----------------

    #[test]
    fn ccteam_team_skill_installs_under_canonical_dir() {
        // V0.5.0 F93a: ccteam-team skill ships the `/ccteam:team`
        // primary-path entry. The body must declare the canonical
        // `name: ccteam-team` frontmatter so Claude Code's skill loader
        // picks it up, and must contain the plan-first protocol +
        // Worker Preamble that the skill is designed around.
        let tmp = tempfile::TempDir::new().unwrap();
        let report = install_skill_body_into(
            tmp.path(),
            CCTEAM_TEAM_SKILL_NAME,
            CCTEAM_TEAM_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(report.action, SkillInstallAction::Wrote);
        assert!(report.target.exists());
        let body = std::fs::read_to_string(&report.target).unwrap();
        assert!(body.starts_with("---\nname: ccteam-team"));
        // Plan-first protocol is the F93a red line — verify it's
        // literally in the skill body so a future edit can't silently
        // drop it.
        assert!(body.contains("TEAM PLAN"));
        assert!(body.contains("Worker Preamble"));
        // The entry syntax block must include all four forms documented
        // in the PRD F93a §需求.
        for form in ["/ccteam-team <task>", "N \"<task>\"", "N:role", "auto"] {
            assert!(body.contains(form), "missing entry syntax form: {form}");
        }
    }

    #[test]
    fn ccteam_team_skill_install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        install_skill_body_into(
            tmp.path(),
            CCTEAM_TEAM_SKILL_NAME,
            CCTEAM_TEAM_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        let r2 = install_skill_body_into(
            tmp.path(),
            CCTEAM_TEAM_SKILL_NAME,
            CCTEAM_TEAM_SKILL_MD,
            InstallSkillOptions::default(),
        )
        .unwrap();
        assert_eq!(r2.action, SkillInstallAction::AlreadyPresent);
    }
}
