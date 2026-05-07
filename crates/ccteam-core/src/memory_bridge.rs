//! `ccteam doctor --install-memory-bridge` (M4.2).
//!
//! Lays down `~/.claude/rules/ccteam-lessons-{dev,product-research}.md`
//! occupying the only ccteam-managed section in each file. Anything the
//! user writes outside the `<!-- ccteam-managed:lessons begin/end -->`
//! marker pair is preserved on re-runs; the marker pair itself is
//! repaired (deduped / re-added at end-of-file) when malformed.
//!
//! The file body itself never holds a project's lessons — those land
//! between the markers via the retro phase's `Edit` call. This module
//! only owns the bridge file's frontmatter + structural skeleton.
//!
//! M4 architectural red line: this is the *only* ccteam-core code that
//! touches the cross-project memory surface. Retrieval lives entirely
//! inside Claude sessions via official `/memory` and rule auto-load.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::team::TeamSpec;
use crate::tool_surface::user_claude_dir;

const DEV_TEMPLATE: &str = include_str!("templates/memory_bridge_dev.md");
const PRODUCT_RESEARCH_TEMPLATE: &str =
    include_str!("templates/memory_bridge_product_research.md");

const MARKER_BEGIN: &str = "<!-- ccteam-managed:lessons begin -->";
const MARKER_END: &str = "<!-- ccteam-managed:lessons end -->";

/// Default body Claude lessons land in. Re-installed verbatim whenever
/// the marker pair is missing / duplicated and the file needs repair.
const CANONICAL_MARKED_BLOCK: &str = "<!-- ccteam-managed:lessons begin -->\n\
(Empty until first retro. Phase prompts append lessons here using `Edit`.)\n\
<!-- ccteam-managed:lessons end -->";

/// V0.2 §6.4 candidate 3: which teams need a memory bridge is now
/// disk-driven. Scan `<global_dir>/teams/<name>/team.yaml`; any team
/// with a non-empty `retro_schema` gets a `~/.claude/rules/ccteam-lessons-<name>.md`
/// scaffold. Shipped teams (dev / product-research) keep their richer
/// embedded templates; user-authored teams fall back to a generic
/// scaffold built from the team description.
fn discover_bridge_teams(global_dir: &Path) -> Vec<(String, String)> {
    let teams_dir = global_dir.join("teams");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&teams_dir) {
        Ok(e) => e,
        Err(_) => return out, // ~/.ccteam/teams/ not yet seeded
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let yaml = entry.path().join("team.yaml");
        if !yaml.exists() {
            continue;
        }
        let spec = match TeamSpec::load(&yaml) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    path = %yaml.display(),
                    error = %err,
                    "memory_bridge: skipping team with malformed team.yaml",
                );
                continue;
            }
        };
        if spec.retro_schema.is_empty() {
            continue;
        }
        let body = lookup_bridge_template(&spec);
        out.push((spec.name, body));
    }
    out
}

/// Map a team to its bridge file body. Shipped teams use the curated
/// `memory_bridge_<name>.md` template; user-authored teams get a
/// generic scaffold derived from the team's description so a fresh
/// `ccteam team publish` immediately has somewhere for retros to
/// `Edit` cross-project lessons into.
fn lookup_bridge_template(spec: &TeamSpec) -> String {
    match spec.name.as_str() {
        "dev" => DEV_TEMPLATE.to_string(),
        "product-research" => PRODUCT_RESEARCH_TEMPLATE.to_string(),
        _ => generic_bridge_template(spec),
    }
}

fn generic_bridge_template(spec: &TeamSpec) -> String {
    let description = if spec.description.trim().is_empty() {
        format!("`{}` team", spec.name)
    } else {
        spec.description.trim().to_string()
    };
    format!(
        "---\ndescription: cross-project memory for {description}. Appended by retro phases at session end; only the marker block is ccteam-managed.\nactivation:\n  paths:\n    - ~/projects/{name}-*\n---\n\n# Lessons learned ({name} team)\n\n(Empty until first retro. Phase prompts append lessons here using `Edit`.)\n\n<!-- ccteam-managed:lessons begin -->\n<!-- ccteam-managed:lessons end -->\n",
        description = description,
        name = spec.name,
    )
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InstallMemoryBridgeOptions {
    /// Don't actually write; report what would happen.
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct MemoryBridgeReport {
    pub team: String,
    pub target: PathBuf,
    pub action: MemoryBridgeAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryBridgeAction {
    /// File did not exist; full template written.
    Wrote,
    /// File present and the marker pair is intact (exactly one begin,
    /// one end, in order). No write occurred.
    AlreadyPresent,
    /// File present but markers were missing / duplicated / unbalanced.
    /// Stripped every existing marker block, appended one canonical
    /// block at end-of-file. User content outside markers preserved.
    RepairedMarkedSection,
    DryRun {
        would_write: bool,
    },
}

/// Install lessons files for every team with a non-empty
/// `retro_schema` into `~/.claude/rules/`. Teams are discovered by
/// scanning `<global_dir>/teams/` (V0.2 §6.4 candidate 3 — formerly a
/// hardcoded `const TEAMS` lock-stepped with the in-binary
/// TEAM_BUNDLES).
pub fn install_memory_bridge(
    global_dir: &Path,
    opts: InstallMemoryBridgeOptions,
) -> Result<Vec<MemoryBridgeReport>> {
    let claude = user_claude_dir().context("resolve ~/.claude/")?;
    install_into(global_dir, &claude, opts)
}

/// Test-injectable variant: write under `<claude_dir>/rules/...` so
/// unit tests can point at a tempdir without mutating `$HOME/.claude/`.
/// `global_dir` is the equivalent of `~/.ccteam/` — `teams/<name>/team.yaml`
/// underneath it drives which teams get a bridge.
pub fn install_into(
    global_dir: &Path,
    claude_dir: &Path,
    opts: InstallMemoryBridgeOptions,
) -> Result<Vec<MemoryBridgeReport>> {
    let rules_dir = claude_dir.join("rules");
    if !opts.dry_run {
        std::fs::create_dir_all(&rules_dir)
            .with_context(|| format!("create {}", rules_dir.display()))?;
    }
    let teams = discover_bridge_teams(global_dir);
    let mut reports = Vec::with_capacity(teams.len());
    for (team, template) in &teams {
        let target = rules_dir.join(format!("ccteam-lessons-{team}.md"));
        let action = install_one(&target, template, opts)
            .with_context(|| format!("install memory bridge for team {team}"))?;
        reports.push(MemoryBridgeReport {
            team: team.clone(),
            target,
            action,
        });
    }
    Ok(reports)
}

fn install_one(
    target: &Path,
    canonical_template: &str,
    opts: InstallMemoryBridgeOptions,
) -> Result<MemoryBridgeAction> {
    if !target.exists() {
        if opts.dry_run {
            return Ok(MemoryBridgeAction::DryRun { would_write: true });
        }
        std::fs::write(target, canonical_template)
            .with_context(|| format!("write {}", target.display()))?;
        return Ok(MemoryBridgeAction::Wrote);
    }
    let body = std::fs::read_to_string(target)
        .with_context(|| format!("read {}", target.display()))?;
    if marked_section_intact(&body) {
        return Ok(if opts.dry_run {
            MemoryBridgeAction::DryRun { would_write: false }
        } else {
            MemoryBridgeAction::AlreadyPresent
        });
    }
    if opts.dry_run {
        return Ok(MemoryBridgeAction::DryRun { would_write: true });
    }
    let repaired = repair_marked_section(&body);
    std::fs::write(target, repaired)
        .with_context(|| format!("write {}", target.display()))?;
    Ok(MemoryBridgeAction::RepairedMarkedSection)
}

/// Exactly one begin marker, exactly one end marker, begin precedes end.
/// Content between the markers is Claude's territory and never inspected.
fn marked_section_intact(body: &str) -> bool {
    let begins = body.match_indices(MARKER_BEGIN).count();
    let ends = body.match_indices(MARKER_END).count();
    if begins != 1 || ends != 1 {
        return false;
    }
    let bi = body.find(MARKER_BEGIN).expect("just counted one");
    let ei = body.find(MARKER_END).expect("just counted one");
    bi < ei
}

/// Strip every begin/end block from `body`, preserving everything
/// outside marker pairs, then append one fresh canonical block.
fn repair_marked_section(body: &str) -> String {
    let user_content = strip_all_marked_blocks(body);
    let trimmed = user_content.trim_end();
    let mut out = String::with_capacity(trimmed.len() + CANONICAL_MARKED_BLOCK.len() + 4);
    out.push_str(trimmed);
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(CANONICAL_MARKED_BLOCK);
    out.push('\n');
    out
}

fn strip_all_marked_blocks(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    loop {
        let Some(bi) = rest.find(MARKER_BEGIN) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..bi]);
        let after_begin = &rest[bi + MARKER_BEGIN.len()..];
        let Some(ei) = after_begin.find(MARKER_END) else {
            // Unbalanced begin — drop everything from here to EOF;
            // canonical block will be appended fresh.
            break;
        };
        rest = &after_begin[ei + MARKER_END.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// Seed shipped team yamls under `<tmp>/ccteam-home/teams/<name>/`
    /// so `install_into(global, claude, ...)` finds dev / product-research
    /// during the disk scan introduced in V0.2 §6.4 candidate 3.
    /// Returns `(global_dir, claude_dir)` — claude_dir is `<tmp>/`,
    /// matching the historical layout the per-test assertions expect.
    fn seed(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let global = tmp.path().join("ccteam-home");
        crate::templates::write_all_global_team_templates(&global, true).unwrap();
        (global, tmp.path().to_path_buf())
    }

    #[test]
    fn install_creates_both_lessons_files_with_intact_markers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        assert_eq!(reports.len(), 2);
        for r in &reports {
            assert_eq!(r.action, MemoryBridgeAction::Wrote);
            assert!(r.target.is_file(), "{} not written", r.target.display());
            let body = read(&r.target);
            assert!(marked_section_intact(&body), "markers not intact");
            assert!(body.contains("paths:"), "frontmatter paths missing");
        }
        let dev = &reports[0];
        assert_eq!(dev.team, "dev");
        assert!(read(&dev.target).contains("~/projects/dev-*"));
        let pr = &reports[1];
        assert_eq!(pr.team, "product-research");
        assert!(read(&pr.target).contains("~/projects/product-research-*"));
    }

    #[test]
    fn install_idempotent_when_files_present_and_intact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        // Capture baseline content (including default empty marked block body).
        let dev_target = tmp.path().join("rules/ccteam-lessons-dev.md");
        let baseline = read(&dev_target);

        let reports2 =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        for r in &reports2 {
            assert_eq!(r.action, MemoryBridgeAction::AlreadyPresent);
        }
        // File must not have been touched.
        assert_eq!(read(&dev_target), baseline);
    }

    #[test]
    fn install_preserves_lessons_written_between_markers() {
        // Simulate a retro phase having edited the marked block.
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        let dev_target = tmp.path().join("rules/ccteam-lessons-dev.md");
        let with_lessons = read(&dev_target).replace(
            "(Empty until first retro. Phase prompts append lessons here using `Edit`.)",
            "## tech_stack\n- Rust 1.78 + tokio\n\n## pitfalls\n- forgot to clone Arc",
        );
        std::fs::write(&dev_target, &with_lessons).unwrap();

        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        assert_eq!(reports[0].action, MemoryBridgeAction::AlreadyPresent);
        let after = read(&dev_target);
        assert!(after.contains("Rust 1.78 + tokio"), "lessons clobbered");
        assert!(after.contains("forgot to clone Arc"), "lessons clobbered");
    }

    #[test]
    fn install_repairs_missing_markers_and_keeps_user_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let rules_dir = tmp.path().join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let target = rules_dir.join("ccteam-lessons-dev.md");
        std::fs::write(
            &target,
            "---\nname: my own rules file\n---\n\n# manually authored notes\n\n\
             - a personal note\n",
        )
        .unwrap();

        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        assert_eq!(reports[0].action, MemoryBridgeAction::RepairedMarkedSection);
        let after = read(&target);
        assert!(after.contains("# manually authored notes"));
        assert!(after.contains("- a personal note"));
        assert!(marked_section_intact(&after));
    }

    #[test]
    fn install_repairs_duplicated_marker_blocks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let rules_dir = tmp.path().join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let target = rules_dir.join("ccteam-lessons-dev.md");
        // User accidentally pasted the marker block twice.
        let dup = format!(
            "# header\n\nsome user prose\n\n{block}\n\n{block}\n",
            block = CANONICAL_MARKED_BLOCK,
        );
        std::fs::write(&target, &dup).unwrap();

        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        assert_eq!(reports[0].action, MemoryBridgeAction::RepairedMarkedSection);
        let after = read(&target);
        assert!(marked_section_intact(&after));
        assert_eq!(after.matches(MARKER_BEGIN).count(), 1);
        assert_eq!(after.matches(MARKER_END).count(), 1);
        assert!(after.contains("some user prose"), "user prose dropped");
    }

    #[test]
    fn install_repairs_unbalanced_begin_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let rules_dir = tmp.path().join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        let target = rules_dir.join("ccteam-lessons-dev.md");
        // Begin marker but no end — file got cut off mid-write.
        let body = format!(
            "# header\n\nuser prose stays\n\n{MARKER_BEGIN}\nstray content with no end marker\n",
        );
        std::fs::write(&target, &body).unwrap();

        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        assert_eq!(reports[0].action, MemoryBridgeAction::RepairedMarkedSection);
        let after = read(&target);
        assert!(marked_section_intact(&after));
        assert!(after.contains("user prose stays"));
        assert!(
            !after.contains("stray content with no end marker"),
            "unbalanced block contents should be dropped",
        );
    }

    #[test]
    fn dry_run_reports_actions_without_touching_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let opts = InstallMemoryBridgeOptions { dry_run: true };
        let reports = install_into(&global, &claude, opts).unwrap();
        for r in &reports {
            assert_eq!(r.action, MemoryBridgeAction::DryRun { would_write: true });
            assert!(!r.target.exists(), "dry-run wrote {}", r.target.display());
        }
        // Now install for real; second dry-run should report no-op.
        install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        let reports2 = install_into(&global, &claude, opts).unwrap();
        for r in &reports2 {
            assert_eq!(r.action, MemoryBridgeAction::DryRun { would_write: false });
        }
    }

    #[test]
    fn install_skips_team_when_retro_schema_is_empty() {
        // V0.2 §6.4 candidate 3: meta-agent ships with `retro_schema: []`,
        // so the disk-scan path must NOT install a bridge for it. Also
        // verifies the discovery is data-driven — adding a team.yaml
        // with retro_schema produces a bridge without touching ccteam-core.
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        assert!(
            reports.iter().all(|r| r.team != "meta-agent"),
            "meta-agent has empty retro_schema and must not get a bridge: {:?}",
            reports.iter().map(|r| &r.team).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn install_picks_up_user_authored_team_via_disk_scan() {
        // V0.2 §6.4 candidate 3 — the team registry is now disk-driven.
        // A user-authored team.yaml with non-empty retro_schema gets a
        // bridge (with the generic fallback template) without ccteam-core
        // changes.
        let tmp = tempfile::TempDir::new().unwrap();
        let (global, claude) = seed(&tmp);
        let custom_dir = global.join("teams").join("custom");
        std::fs::create_dir_all(&custom_dir).unwrap();
        std::fs::write(
            custom_dir.join("team.yaml"),
            "name: custom\ndescription: A user-authored team\nphase_dir: phases-custom\nretro_schema:\n  - field: outcomes\n    description: Things observed\n",
        )
        .unwrap();
        let reports =
            install_into(&global, &claude, InstallMemoryBridgeOptions::default()).unwrap();
        let custom = reports
            .iter()
            .find(|r| r.team == "custom")
            .expect("custom team bridge missing");
        assert_eq!(custom.action, MemoryBridgeAction::Wrote);
        let body = read(&custom.target);
        assert!(body.contains("ccteam-managed:lessons begin"));
        assert!(body.contains("custom"));
    }

    #[test]
    fn marked_section_intact_rejects_zero_or_multiple_pairs() {
        assert!(!marked_section_intact(""));
        assert!(!marked_section_intact("# only header"));
        assert!(!marked_section_intact(&format!(
            "{MARKER_BEGIN}\n{MARKER_BEGIN}\nx\n{MARKER_END}\n{MARKER_END}\n",
        )));
        // End before begin → not intact.
        assert!(!marked_section_intact(&format!(
            "{MARKER_END}\nx\n{MARKER_BEGIN}\n",
        )));
        assert!(marked_section_intact(&format!(
            "header\n{MARKER_BEGIN}\nbody\n{MARKER_END}\nfooter\n",
        )));
    }
}
