//! Tool-surface foundation (M0.5). Makes Claude Code's plugin agents
//! and skills callable from inside ccteam-managed long sessions.
//!
//! Two artifacts shipped here:
//!
//! 1. `RECOMMENDED_AGENTS` — the eight `claude-plugins-official` plugin
//!    agents that ccteam phase markdown is allowed to invoke via
//!    `Task(subagent_type=…)`. Plugin agents do **not** auto-register
//!    in Claude Code's `Task` namespace just because the plugin is
//!    installed (per `docs/claude-code-tool-surface.md` §1.1.2 / §1.2.5);
//!    we have to symlink each `<plugin>/agents/<name>.md` into
//!    `~/.claude/agents/` **before** the tmux session starts, since
//!    Claude Code scans that directory at session start and never again.
//!
//! 2. `ensure_skills_placeholders` — pre-creates the global and
//!    project-local skills directories (even empty) so Claude Code's
//!    live SKILL.md monitor (§1.2.4) attaches at session start. Without
//!    this, "lazy-inject a skill mid-session" silently fails.
//!
//! Pure-ish: file-system mutations only; no orchestrator side effects.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// One recommended plugin agent we link into `~/.claude/agents/`.
///
/// `filename` is the basename used inside `~/.claude/agents/` and the
/// `subagent_type` Claude Code will accept (e.g. `code-reviewer.md` →
/// `Task(subagent_type="code-reviewer")`). `plugin` is the
/// `claude-plugins-official` plugin directory name; `relpath` is the
/// path inside that plugin to the agent file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecommendedAgent {
    pub filename: &'static str,
    pub plugin: &'static str,
    pub relpath: &'static str,
}

impl RecommendedAgent {
    /// `subagent_type` Claude Code accepts for `Task(subagent_type=…)`,
    /// derived from `filename` (drop trailing `.md`).
    pub fn subagent_type(&self) -> &'static str {
        self.filename.strip_suffix(".md").unwrap_or(self.filename)
    }

    /// Absolute path to the plugin's agent file under
    /// `<claude_dir>/plugins/marketplaces/claude-plugins-official/plugins/`.
    pub fn source_path(&self, claude_dir: &Path) -> PathBuf {
        claude_dir
            .join("plugins")
            .join("marketplaces")
            .join("claude-plugins-official")
            .join("plugins")
            .join(self.plugin)
            .join(self.relpath)
    }
}

/// The eight plugin agents `bootstrap_project` symlinks into
/// `~/.claude/agents/`. Source list: `docs/claude-code-tool-surface.md`
/// §6.2 (kept identical so the doctor cross-check is straightforward).
pub const RECOMMENDED_AGENTS: &[RecommendedAgent] = &[
    RecommendedAgent {
        filename: "code-reviewer.md",
        plugin: "pr-review-toolkit",
        relpath: "agents/code-reviewer.md",
    },
    RecommendedAgent {
        filename: "silent-failure-hunter.md",
        plugin: "pr-review-toolkit",
        relpath: "agents/silent-failure-hunter.md",
    },
    RecommendedAgent {
        filename: "pr-test-analyzer.md",
        plugin: "pr-review-toolkit",
        relpath: "agents/pr-test-analyzer.md",
    },
    RecommendedAgent {
        filename: "type-design-analyzer.md",
        plugin: "pr-review-toolkit",
        relpath: "agents/type-design-analyzer.md",
    },
    RecommendedAgent {
        filename: "comment-analyzer.md",
        plugin: "pr-review-toolkit",
        relpath: "agents/comment-analyzer.md",
    },
    RecommendedAgent {
        filename: "code-architect.md",
        plugin: "feature-dev",
        relpath: "agents/code-architect.md",
    },
    RecommendedAgent {
        filename: "code-explorer.md",
        plugin: "feature-dev",
        relpath: "agents/code-explorer.md",
    },
    RecommendedAgent {
        filename: "code-simplifier.md",
        plugin: "code-simplifier",
        relpath: "agents/code-simplifier.md",
    },
];

/// What `link_recommended_agents_into` did for one agent. Surfaced so
/// `ccteam doctor --install-recommended-agents` can render a per-agent
/// status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLinkAction {
    /// Created a fresh symlink target → source.
    Linked,
    /// A symlink already pointed at our source; no-op.
    AlreadyLinked,
    /// Symlink existed but pointed elsewhere; left alone (force=false)
    /// or replaced (force=true).
    Replaced { previous_target: PathBuf },
    /// A regular file existed at the target; skipped to preserve user
    /// authorship. `force=true` would replace it instead.
    SkippedUserFile,
    /// Source file is missing under the plugins dir; skipped. The user
    /// likely uninstalled the plugin (or the marketplace path differs).
    SkippedSourceMissing { source: PathBuf },
    /// Would-be no-op in `--dry-run` mode.
    DryRun { would: Box<AgentLinkAction> },
}

impl AgentLinkAction {
    /// True if this action made (or would make) the symlink correct
    /// in non-dry-run mode. Used by the `bootstrap` and `doctor` paths
    /// to decide overall success.
    pub fn is_ok(&self) -> bool {
        matches!(
            self,
            AgentLinkAction::Linked
                | AgentLinkAction::AlreadyLinked
                | AgentLinkAction::Replaced { .. }
                | AgentLinkAction::DryRun { .. }
        )
    }
}

/// One per-agent line in the report.
#[derive(Debug, Clone)]
pub struct AgentLinkReport {
    pub agent: RecommendedAgent,
    pub target: PathBuf,
    pub action: AgentLinkAction,
}

/// `link_recommended_agents` policy knob.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkOptions {
    /// Replace user-authored regular files / symlinks pointing
    /// elsewhere. Default false: keep user files and warn instead.
    pub force: bool,
    /// Print + return what *would* happen but don't touch the
    /// filesystem. Used by `ccteam doctor --install-recommended-agents
    /// --dry-run`.
    pub dry_run: bool,
}

/// Symlink every `RECOMMENDED_AGENTS` entry from its plugin source into
/// `<claude_dir>/agents/`. Test seam — production callers go through
/// [`link_recommended_agents`] which resolves `claude_dir` from
/// `dirs::home_dir()`.
pub fn link_recommended_agents_into(
    claude_dir: &Path,
    opts: LinkOptions,
) -> Result<Vec<AgentLinkReport>> {
    let agents_dir = claude_dir.join("agents");
    if !opts.dry_run {
        std::fs::create_dir_all(&agents_dir)
            .with_context(|| format!("create {}", agents_dir.display()))?;
    }

    let mut out = Vec::with_capacity(RECOMMENDED_AGENTS.len());
    for agent in RECOMMENDED_AGENTS {
        let source = agent.source_path(claude_dir);
        let target = agents_dir.join(agent.filename);
        let action = link_one_agent(&source, &target, opts)?;
        out.push(AgentLinkReport {
            agent: *agent,
            target,
            action,
        });
    }
    Ok(out)
}

fn link_one_agent(
    source: &Path,
    target: &Path,
    opts: LinkOptions,
) -> Result<AgentLinkAction> {
    if !source.exists() {
        return Ok(AgentLinkAction::SkippedSourceMissing {
            source: source.to_path_buf(),
        });
    }

    let inner = if let Some(existing) = read_symlink_safe(target)? {
        if existing == source {
            AgentLinkAction::AlreadyLinked
        } else if opts.force {
            if !opts.dry_run {
                std::fs::remove_file(target).with_context(|| {
                    format!("remove existing symlink {}", target.display())
                })?;
                symlink_file(source, target)?;
            }
            AgentLinkAction::Replaced {
                previous_target: existing,
            }
        } else {
            AgentLinkAction::Replaced {
                previous_target: existing,
            }
        }
    } else if target.exists() {
        if opts.force {
            if !opts.dry_run {
                std::fs::remove_file(target).with_context(|| {
                    format!("remove user file {}", target.display())
                })?;
                symlink_file(source, target)?;
            }
            AgentLinkAction::Linked
        } else {
            AgentLinkAction::SkippedUserFile
        }
    } else {
        if !opts.dry_run {
            symlink_file(source, target)?;
        }
        AgentLinkAction::Linked
    };

    Ok(if opts.dry_run {
        AgentLinkAction::DryRun {
            would: Box::new(inner),
        }
    } else {
        inner
    })
}

/// Resolve a symlink at `path` to its absolute target if (and only if)
/// `path` is a symlink. Returns `Ok(None)` for regular files / missing
/// paths so the caller can branch on those separately.
fn read_symlink_safe(path: &Path) -> Result<Option<PathBuf>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("symlink_metadata {}", path.display()));
        }
    };
    if !meta.file_type().is_symlink() {
        return Ok(None);
    }
    let raw = std::fs::read_link(path)
        .with_context(|| format!("read_link {}", path.display()))?;
    let resolved = if raw.is_absolute() {
        raw
    } else {
        path.parent()
            .map(|p| p.join(&raw))
            .unwrap_or(raw)
    };
    Ok(Some(resolved))
}

#[cfg(unix)]
fn symlink_file(source: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, target)
        .with_context(|| format!("symlink {} → {}", target.display(), source.display()))
}

#[cfg(not(unix))]
fn symlink_file(_source: &Path, _target: &Path) -> Result<()> {
    Err(anyhow!(
        "ccteam tool-surface symlinks are not supported on this platform yet",
    ))
}

/// Resolve the user's `~/.claude/` directory (where Claude Code scans
/// `agents/` and `skills/`). Honors `CLAUDE_CONFIG_HOME` for testing.
pub fn user_claude_dir() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("CLAUDE_CONFIG_HOME") {
        return Ok(PathBuf::from(s));
    }
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory"))?;
    Ok(home.join(".claude"))
}

/// Production entry point: resolve `~/.claude/` and call
/// [`link_recommended_agents_into`].
pub fn link_recommended_agents(opts: LinkOptions) -> Result<Vec<AgentLinkReport>> {
    let claude_dir = user_claude_dir()?;
    link_recommended_agents_into(&claude_dir, opts)
}

/// Pre-create both skills directories so Claude Code's live SKILL.md
/// monitor attaches at session start (tool-surface §1.2.4 — top-level
/// dir must exist at startup or the watcher won't fire). Idempotent.
pub fn ensure_skills_placeholders(claude_dir: &Path, project_dir: &Path) -> Result<()> {
    let global = claude_dir.join("skills");
    std::fs::create_dir_all(&global)
        .with_context(|| format!("create {}", global.display()))?;
    let local = project_dir.join(".claude").join("skills");
    std::fs::create_dir_all(&local)
        .with_context(|| format!("create {}", local.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_claude_dir(tmp: &tempfile::TempDir) -> PathBuf {
        let dir = tmp.path().join(".claude");
        // Stage all 8 plugin agent files so link_one_agent finds sources.
        for agent in RECOMMENDED_AGENTS {
            let src = agent.source_path(&dir);
            std::fs::create_dir_all(src.parent().unwrap()).unwrap();
            std::fs::write(&src, format!("# stub for {}\n", agent.filename)).unwrap();
        }
        dir
    }

    #[test]
    fn recommended_set_has_eight_distinct_filenames() {
        assert_eq!(RECOMMENDED_AGENTS.len(), 8);
        let mut names: Vec<&str> = RECOMMENDED_AGENTS.iter().map(|a| a.filename).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8, "filenames must be unique under ~/.claude/agents/");
    }

    #[test]
    fn subagent_type_drops_md_suffix() {
        let a = RECOMMENDED_AGENTS[0];
        assert_eq!(a.filename, "code-reviewer.md");
        assert_eq!(a.subagent_type(), "code-reviewer");
    }

    #[test]
    fn link_recommended_agents_creates_eight_symlinks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = fake_claude_dir(&tmp);

        let reports = link_recommended_agents_into(&claude, LinkOptions::default()).unwrap();
        assert_eq!(reports.len(), 8);
        for r in &reports {
            assert_eq!(r.action, AgentLinkAction::Linked, "{:?}", r);
            let meta = std::fs::symlink_metadata(&r.target).unwrap();
            assert!(meta.file_type().is_symlink());
        }
    }

    #[test]
    fn link_recommended_agents_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = fake_claude_dir(&tmp);

        link_recommended_agents_into(&claude, LinkOptions::default()).unwrap();
        let reports = link_recommended_agents_into(&claude, LinkOptions::default()).unwrap();
        for r in reports {
            assert_eq!(r.action, AgentLinkAction::AlreadyLinked, "{:?}", r);
        }
    }

    #[test]
    fn link_recommended_agents_skips_user_authored_file_without_force() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = fake_claude_dir(&tmp);
        let agents = claude.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let user_file = agents.join("code-reviewer.md");
        std::fs::write(&user_file, "USER VERSION").unwrap();

        let reports = link_recommended_agents_into(&claude, LinkOptions::default()).unwrap();
        let cr = reports
            .iter()
            .find(|r| r.agent.filename == "code-reviewer.md")
            .unwrap();
        assert_eq!(cr.action, AgentLinkAction::SkippedUserFile);
        // User file untouched.
        assert_eq!(std::fs::read_to_string(&user_file).unwrap(), "USER VERSION");
    }

    #[test]
    fn link_recommended_agents_force_replaces_user_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = fake_claude_dir(&tmp);
        let agents = claude.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let user_file = agents.join("code-reviewer.md");
        std::fs::write(&user_file, "USER VERSION").unwrap();

        link_recommended_agents_into(&claude, LinkOptions { force: true, dry_run: false })
            .unwrap();
        let meta = std::fs::symlink_metadata(&user_file).unwrap();
        assert!(meta.file_type().is_symlink(), "force should replace user file");
    }

    #[test]
    fn link_recommended_agents_skips_when_source_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude"); // do NOT pre-stage sources

        let reports = link_recommended_agents_into(&claude, LinkOptions::default()).unwrap();
        for r in reports {
            assert!(
                matches!(r.action, AgentLinkAction::SkippedSourceMissing { .. }),
                "expected SkippedSourceMissing, got {:?}",
                r.action,
            );
        }
        // No symlinks created.
        let agents = claude.join("agents");
        if agents.exists() {
            let count = std::fs::read_dir(&agents).unwrap().count();
            assert_eq!(count, 0, "should not create any symlinks when sources missing");
        }
    }

    #[test]
    fn link_recommended_agents_dry_run_no_filesystem_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = fake_claude_dir(&tmp);
        let agents_dir = claude.join("agents");

        let reports = link_recommended_agents_into(
            &claude,
            LinkOptions { force: false, dry_run: true },
        )
        .unwrap();
        for r in reports {
            assert!(
                matches!(r.action, AgentLinkAction::DryRun { .. }),
                "expected DryRun, got {:?}",
                r.action,
            );
            assert!(
                !r.target.exists(),
                "dry_run must not create {}",
                r.target.display(),
            );
        }
        // Even the parent agents dir shouldn't be created in dry-run.
        assert!(!agents_dir.exists(), "agents dir should not be auto-created in dry-run");
    }

    #[test]
    fn ensure_skills_placeholders_creates_both_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let project = tmp.path().join("projects/demo");
        std::fs::create_dir_all(&project).unwrap();
        ensure_skills_placeholders(&claude, &project).unwrap();
        assert!(claude.join("skills").is_dir());
        assert!(project.join(".claude/skills").is_dir());
    }

    #[test]
    fn ensure_skills_placeholders_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let project = tmp.path().join("projects/demo");
        std::fs::create_dir_all(&project).unwrap();
        ensure_skills_placeholders(&claude, &project).unwrap();
        ensure_skills_placeholders(&claude, &project).unwrap();
        assert!(claude.join("skills").is_dir());
    }
}
