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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

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

/// Tools a phase markdown declares it needs at runtime. Validated
/// against [`ToolSurfaceSnapshot`] at orchestrator startup so a phase
/// that asks for `Task(subagent_type="code-reviewer")` fails fast if
/// the agent file is missing under `~/.claude/agents/`.
///
/// Schema: `docs/interfaces.md` §5.1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRequired {
    /// `subagent_type` strings (e.g. `code-reviewer`, `general-purpose`).
    /// Five built-ins are always reachable and don't need to be listed,
    /// but listing them is harmless.
    #[serde(default)]
    pub subagents: Vec<String>,
    /// Skill names callable via the `Skill` tool. Resolved against
    /// `~/.claude/skills/<name>/SKILL.md` and any installed plugin's
    /// `skills/<name>/SKILL.md`.
    #[serde(default)]
    pub skills: Vec<String>,
    /// MCP server names from `~/.claude.json` `mcpServers` keys
    /// (and any plugin `.mcp.json` once M2 lands plugin-aware
    /// MCP discovery).
    #[serde(default)]
    pub mcp: Vec<String>,
}

impl ToolsRequired {
    /// True when no tools are declared.
    pub fn is_empty(&self) -> bool {
        self.subagents.is_empty() && self.skills.is_empty() && self.mcp.is_empty()
    }
}

/// `subagent_type` strings always reachable in any Claude Code session
/// without registration (per `docs/claude-code-tool-surface.md` §1.1.1).
/// Treated as built-in by [`ToolSurfaceSnapshot::scan`] so phase
/// templates can list them without forcing extra setup.
pub const BUILTIN_SUBAGENTS: &[&str] = &[
    "general-purpose",
    "Explore",
    "Plan",
    "claude-code-guide",
    "statusline-setup",
];

/// What's actually reachable on the current machine. Built once at
/// orchestrator startup and used to validate every phase template's
/// `tools_required`.
#[derive(Debug, Clone, Default)]
pub struct ToolSurfaceSnapshot {
    pub subagents: BTreeSet<String>,
    pub skills: BTreeSet<String>,
    pub mcp: BTreeSet<String>,
}

impl ToolSurfaceSnapshot {
    /// Production entry point — resolve `~/.claude/` then scan.
    pub fn from_user_claude() -> Result<Self> {
        let dir = user_claude_dir()?;
        Self::scan(&dir)
    }

    /// Build a snapshot by reading the filesystem under `claude_dir`
    /// (`~/.claude/`). MCP servers come from `<claude_dir>/.claude.json`
    /// or `<claude_dir>/mcp_servers.json` when present. Plugin-supplied
    /// skills are pulled from
    /// `<claude_dir>/plugins/marketplaces/*/plugins/*/skills/<name>/`.
    pub fn scan(claude_dir: &Path) -> Result<Self> {
        let mut subagents: BTreeSet<String> = BUILTIN_SUBAGENTS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let mut skills: BTreeSet<String> = BTreeSet::new();
        let mut mcp: BTreeSet<String> = BTreeSet::new();

        // ~/.claude/agents/<name>.md → subagent_type "<name>"
        let agents_dir = claude_dir.join("agents");
        if let Ok(entries) = std::fs::read_dir(&agents_dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().is_some_and(|x| x == "md") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        subagents.insert(stem.to_string());
                    }
                }
            }
        }

        // ~/.claude/skills/<name>/SKILL.md → skill "<name>"
        let skills_dir = claude_dir.join("skills");
        collect_skills_from(&skills_dir, &mut skills);

        // Plugin-shipped skills:
        // ~/.claude/plugins/marketplaces/<market>/plugins/<plugin>/skills/<name>/SKILL.md
        let marketplaces = claude_dir.join("plugins").join("marketplaces");
        if let Ok(market_entries) = std::fs::read_dir(&marketplaces) {
            for market in market_entries.flatten() {
                let plugins = market.path().join("plugins");
                if let Ok(plugin_entries) = std::fs::read_dir(&plugins) {
                    for plugin in plugin_entries.flatten() {
                        let p_skills = plugin.path().join("skills");
                        collect_skills_from(&p_skills, &mut skills);
                    }
                }
            }
        }

        // MCP servers from a few well-known locations. Order:
        //   1. ~/.claude.json (parent of ~/.claude/), top-level mcpServers
        //   2. ~/.claude/mcp_servers.json
        if let Some(parent) = claude_dir.parent() {
            collect_mcp_from(&parent.join(".claude.json"), &mut mcp);
        }
        collect_mcp_from(&claude_dir.join("mcp_servers.json"), &mut mcp);

        Ok(Self {
            subagents,
            skills,
            mcp,
        })
    }

    /// True if `name` is reachable as a `subagent_type` in this
    /// snapshot. Comparison is case-sensitive — Claude Code's built-ins
    /// include `Explore` / `Plan` with leading capitals.
    pub fn has_subagent(&self, name: &str) -> bool {
        self.subagents.contains(name)
    }

    pub fn has_skill(&self, name: &str) -> bool {
        self.skills.contains(name)
    }

    pub fn has_mcp(&self, name: &str) -> bool {
        self.mcp.contains(name)
    }
}

/// One missing tool, used by [`missing_tools`] to drive both the
/// orchestrator's fail-fast error and the doctor command's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingTool {
    Subagent(String),
    Skill(String),
    Mcp(String),
}

impl MissingTool {
    pub fn kind(&self) -> &'static str {
        match self {
            MissingTool::Subagent(_) => "subagent",
            MissingTool::Skill(_) => "skill",
            MissingTool::Mcp(_) => "mcp",
        }
    }

    pub fn name(&self) -> &str {
        match self {
            MissingTool::Subagent(s)
            | MissingTool::Skill(s)
            | MissingTool::Mcp(s) => s.as_str(),
        }
    }

    /// One-line CLI hint to fix this missing tool. Used by
    /// `ccteam doctor --tool-surface` and the orchestrator's startup
    /// error message.
    pub fn fix_hint(&self) -> String {
        match self {
            MissingTool::Subagent(name) => {
                if RECOMMENDED_AGENTS.iter().any(|a| a.subagent_type() == name) {
                    "run `ccteam doctor --install-recommended-agents` to symlink the plugin agent".into()
                } else {
                    format!(
                        "drop `{name}.md` into ~/.claude/agents/ (custom agent), or remove it from `tools_required.subagents`"
                    )
                }
            }
            MissingTool::Skill(name) => format!(
                "install the `{name}` skill (e.g. `~/.claude/skills/{name}/SKILL.md`) or remove it from `tools_required.skills`"
            ),
            MissingTool::Mcp(name) => format!(
                "register MCP server `{name}` in ~/.claude.json `mcpServers` section, or remove it from `tools_required.mcp`"
            ),
        }
    }
}

/// Cross-check `req` against `snap` and return any tools that aren't
/// reachable. Empty result means the phase template is good to go.
pub fn missing_tools(req: &ToolsRequired, snap: &ToolSurfaceSnapshot) -> Vec<MissingTool> {
    let mut out = Vec::new();
    for s in &req.subagents {
        if !snap.has_subagent(s) {
            out.push(MissingTool::Subagent(s.clone()));
        }
    }
    for s in &req.skills {
        if !snap.has_skill(s) {
            out.push(MissingTool::Skill(s.clone()));
        }
    }
    for s in &req.mcp {
        if !snap.has_mcp(s) {
            out.push(MissingTool::Mcp(s.clone()));
        }
    }
    out
}

fn collect_skills_from(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            out.insert(name.to_string());
        }
    }
}

fn collect_mcp_from(path: &Path, out: &mut BTreeSet<String>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(v): std::result::Result<serde_json::Value, _> = serde_json::from_slice(&bytes) else {
        return;
    };
    if let Some(servers) = v.get("mcpServers").and_then(|x| x.as_object()) {
        for k in servers.keys() {
            out.insert(k.clone());
        }
    }
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

    #[test]
    fn snapshot_collects_builtin_subagents_even_in_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let snap = ToolSurfaceSnapshot::scan(&claude).unwrap();
        for s in BUILTIN_SUBAGENTS {
            assert!(snap.has_subagent(s), "missing built-in subagent `{s}`");
        }
        assert!(!snap.has_subagent("code-reviewer"));
    }

    #[test]
    fn snapshot_picks_up_user_agents_and_plugin_skills() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        // user-authored agent
        std::fs::create_dir_all(claude.join("agents")).unwrap();
        std::fs::write(claude.join("agents/code-reviewer.md"), "# stub").unwrap();
        // user-authored skill
        std::fs::create_dir_all(claude.join("skills/my-skill")).unwrap();
        std::fs::write(claude.join("skills/my-skill/SKILL.md"), "# stub").unwrap();
        // plugin-shipped skill
        let plugin_skill = claude
            .join("plugins/marketplaces/x/plugins/foo/skills/plug-skill");
        std::fs::create_dir_all(&plugin_skill).unwrap();
        std::fs::write(plugin_skill.join("SKILL.md"), "# stub").unwrap();

        let snap = ToolSurfaceSnapshot::scan(&claude).unwrap();
        assert!(snap.has_subagent("code-reviewer"));
        assert!(snap.has_skill("my-skill"));
        assert!(snap.has_skill("plug-skill"));
    }

    #[test]
    fn snapshot_reads_mcp_from_claude_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Layout: <home>/.claude/ for claude_dir, <home>/.claude.json sibling
        let claude = tmp.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            tmp.path().join(".claude.json"),
            r#"{"mcpServers": {"playwright": {"command": "..."}, "ccteam": {"command": "..."}}}"#,
        )
        .unwrap();
        let snap = ToolSurfaceSnapshot::scan(&claude).unwrap();
        assert!(snap.has_mcp("playwright"));
        assert!(snap.has_mcp("ccteam"));
    }

    #[test]
    fn missing_tools_returns_each_unmet_dependency() {
        let snap = ToolSurfaceSnapshot {
            subagents: ["general-purpose"].iter().map(|s| s.to_string()).collect(),
            skills: BTreeSet::new(),
            mcp: BTreeSet::new(),
        };
        let req = ToolsRequired {
            subagents: vec!["general-purpose".into(), "code-reviewer".into()],
            skills: vec!["x-skill".into()],
            mcp: vec!["nope".into()],
        };
        let miss = missing_tools(&req, &snap);
        assert_eq!(miss.len(), 3);
        assert_eq!(miss[0], MissingTool::Subagent("code-reviewer".into()));
        assert_eq!(miss[1], MissingTool::Skill("x-skill".into()));
        assert_eq!(miss[2], MissingTool::Mcp("nope".into()));
    }

    #[test]
    fn missing_tools_empty_when_all_present() {
        let snap = ToolSurfaceSnapshot {
            subagents: ["code-reviewer", "general-purpose"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            skills: ["my-skill"].iter().map(|s| s.to_string()).collect(),
            mcp: ["mcp1"].iter().map(|s| s.to_string()).collect(),
        };
        let req = ToolsRequired {
            subagents: vec!["code-reviewer".into()],
            skills: vec!["my-skill".into()],
            mcp: vec!["mcp1".into()],
        };
        assert!(missing_tools(&req, &snap).is_empty());
    }

    #[test]
    fn missing_tool_fix_hint_recommends_doctor_for_recommended_agents() {
        let m = MissingTool::Subagent("code-reviewer".into());
        let h = m.fix_hint();
        assert!(h.contains("ccteam doctor"), "got: {h}");

        let m2 = MissingTool::Subagent("user-custom".into());
        let h2 = m2.fix_hint();
        assert!(h2.contains("custom agent"), "got: {h2}");
    }
}
