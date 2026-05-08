//! Tool-surface foundation. Validates phase YAML's
//! `tools_required` (subagents / skills / MCP servers) against what's
//! reachable on this machine.
//!
//! V0.2 M0.20 (candidate 7) replaces the M0.5 `RECOMMENDED_AGENTS`
//! ln -sf protocol with Claude Code's in-memory plugin pipeline:
//! `bootstrap_project` writes `enabledPlugins` into the spawned
//! project's `.claude/settings.json`, Claude Code namespaces plugin
//! agents as `<plugin>:<name>` automatically, and ccteam-core no
//! longer touches `~/.claude/agents/`. The reachability check here
//! walks the plugin pipeline (via [`plugin_resolution`]) plus
//! `~/.claude/agents/` (operator's user-authored customs) plus
//! `~/.claude/skills/` (user skills) plus plugin-shipped skills.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::plugin_resolution::{lookup_plugin_agent, KNOWN_PLUGIN_AGENTS};

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

/// Test-only helper: set `CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP=1` so
/// any subsequent `bootstrap_project` call in the same process
/// no-ops the `~/.claude/` mutation. Use from a once-init at the top
/// of test files that exercise `bootstrap_project` but don't care
/// about tool-surface side effects.
pub fn disable_tool_surface_bootstrap_for_tests() {
    std::env::set_var("CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP", "1");
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
/// neither the plugin shipping it is enabled nor a user-authored
/// `~/.claude/agents/code-reviewer.md` exists.
///
/// Schema: `docs/interfaces.md` §5.1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRequired {
    /// `subagent_type` strings (e.g. `code-reviewer`, `general-purpose`).
    /// Five built-ins are always reachable and don't need to be listed,
    /// but listing them is harmless. Plugin-shipped agents resolve via
    /// [`crate::plugin_resolution`] and require the plugin to be
    /// enabled in the spawned project's `enabledPlugins`.
    #[serde(default)]
    pub subagents: Vec<String>,
    /// Skill names callable via the `Skill` tool. Resolved against
    /// `~/.claude/skills/<name>/SKILL.md` and any installed plugin's
    /// `skills/<name>/SKILL.md`.
    #[serde(default)]
    pub skills: Vec<String>,
    /// MCP server names from `~/.claude.json` `mcpServers` keys.
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
///
/// For plugin-shipped agents the snapshot indexes both the bare name
/// (`code-reviewer`) and the namespaced one (`pr-review-toolkit:code-reviewer`)
/// — phase YAML can use either form, and the namespaced form survives
/// when two plugins ship a `code-simplifier`-style collision.
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
    /// (`~/.claude/`).
    ///
    /// Plugin-shipped subagents are picked up from
    /// [`KNOWN_PLUGIN_AGENTS`] when their plugin source file exists
    /// under `<claude_dir>/plugins/marketplaces/<mkt>/plugins/<plugin>/agents/<name>.md`.
    /// User-authored agents from `<claude_dir>/agents/<name>.md` still
    /// resolve (operator escape hatch). Skills and MCP servers come
    /// from the same paths as before.
    pub fn scan(claude_dir: &Path) -> Result<Self> {
        let mut subagents: BTreeSet<String> = BUILTIN_SUBAGENTS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let mut skills: BTreeSet<String> = BTreeSet::new();
        let mut mcp: BTreeSet<String> = BTreeSet::new();

        // Plugin-shipped subagents (V0.2 M0.20 — replaces the prior
        // ~/.claude/agents/ ln -sf scan). A plugin agent counts as
        // reachable iff its source file exists on disk; the project
        // session's `enabledPlugins` decides which plugins actually
        // load, but the source file existing is a necessary condition
        // either way.
        for agent in KNOWN_PLUGIN_AGENTS {
            if agent.source_path(claude_dir).is_file() {
                subagents.insert(agent.subagent.to_string());
                subagents.insert(format!("{}:{}", agent.plugin, agent.subagent));
            }
        }

        // ~/.claude/agents/<name>.md → subagent_type "<name>". Operator
        // escape hatch for custom agents that aren't shipped by any
        // plugin. ccteam-core no longer writes here (V0.2 M0.20).
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
                if let Some(a) = lookup_plugin_agent(name) {
                    format!(
                        "install plugin `{}` (e.g. `claude /plugin add {}`); \
                         spawned project sessions enable it via `enabledPlugins` automatically",
                        a.plugin_id(),
                        a.plugin_id(),
                    )
                } else {
                    format!(
                        "drop `{name}.md` into ~/.claude/agents/ (custom agent), or remove it from `tools_required.subagents`"
                    )
                }
            }
            MissingTool::Skill(name) => format!(
                "install the `{name}` skill — drop a SKILL.md at `~/.claude/skills/{name}/`, \
                 or install a plugin that ships it under \
                 `~/.claude/plugins/marketplaces/<market>/plugins/<plugin>/skills/{name}/`. \
                 If the skill is not needed, remove it from `tools_required.skills`."
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

/// Remove every `~/.claude/agents/<name>.md` symlink whose target
/// resolves into `<claude_dir>/plugins/marketplaces/`. Surfaced for
/// `ccteam doctor --migrate-recommended-agents` so V0.1 users with
/// the old M0.5 ln -sf protocol can clean up stale links once they
/// upgrade to V0.2's plugin pipeline.
///
/// Regular files (operator-authored agents) and symlinks pointing
/// outside the marketplace tree are preserved. Returns the report so
/// the doctor command can render per-agent status; `dry_run=true`
/// reports without touching the filesystem.
pub fn migrate_recommended_agent_symlinks(
    claude_dir: &Path,
    dry_run: bool,
) -> Result<Vec<MigrationReport>> {
    let agents_dir = claude_dir.join("agents");
    let marketplaces_root = claude_dir.join("plugins").join("marketplaces");
    let mut out: Vec<MigrationReport> = Vec::new();
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("read_dir {}", agents_dir.display()));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let raw = match std::fs::read_link(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let resolved = if raw.is_absolute() {
            raw.clone()
        } else {
            path.parent()
                .map(|p| p.join(&raw))
                .unwrap_or_else(|| raw.clone())
        };
        if !resolved.starts_with(&marketplaces_root) {
            continue;
        }
        if !dry_run {
            std::fs::remove_file(&path)
                .with_context(|| format!("remove {}", path.display()))?;
        }
        out.push(MigrationReport {
            target: path,
            previous_link: raw,
            removed: !dry_run,
        });
    }
    Ok(out)
}

/// One symlink the migrate command found / removed.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// `<claude_dir>/agents/<name>.md` whose stale ln -sf was found.
    pub target: PathBuf,
    /// What the symlink pointed at (relative or absolute, as stored).
    pub previous_link: PathBuf,
    /// `false` for `dry_run=true` runs.
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(claude.join("agents/user-custom.md"), "# stub").unwrap();
        // user-authored skill
        std::fs::create_dir_all(claude.join("skills/my-skill")).unwrap();
        std::fs::write(claude.join("skills/my-skill/SKILL.md"), "# stub").unwrap();
        // plugin-shipped skill
        let plugin_skill = claude
            .join("plugins/marketplaces/x/plugins/foo/skills/plug-skill");
        std::fs::create_dir_all(&plugin_skill).unwrap();
        std::fs::write(plugin_skill.join("SKILL.md"), "# stub").unwrap();

        let snap = ToolSurfaceSnapshot::scan(&claude).unwrap();
        assert!(snap.has_subagent("user-custom"));
        assert!(snap.has_skill("my-skill"));
        assert!(snap.has_skill("plug-skill"));
    }

    #[test]
    fn snapshot_picks_up_plugin_subagent_via_in_memory_pipeline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        // Stage the source file the way claude-plugins-official lays
        // out on disk after `claude /plugin add`.
        let src = claude
            .join("plugins/marketplaces/claude-plugins-official/plugins/pr-review-toolkit/agents/code-reviewer.md");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "# code-reviewer\n").unwrap();

        let snap = ToolSurfaceSnapshot::scan(&claude).unwrap();
        // Reachable under both bare and namespaced forms.
        assert!(snap.has_subagent("code-reviewer"));
        assert!(snap.has_subagent("pr-review-toolkit:code-reviewer"));
        // Other plugin agents whose source isn't staged don't leak in.
        assert!(!snap.has_subagent("code-architect"));
    }

    #[test]
    fn snapshot_reads_mcp_from_claude_json() {
        let tmp = tempfile::TempDir::new().unwrap();
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
    fn fix_hint_for_known_plugin_agent_names_plugin_id() {
        let m = MissingTool::Subagent("code-reviewer".into());
        let h = m.fix_hint();
        assert!(
            h.contains("pr-review-toolkit@claude-plugins-official"),
            "got: {h}",
        );
        assert!(h.contains("plugin"), "got: {h}");
    }

    #[test]
    fn fix_hint_for_unknown_subagent_points_at_user_agents_dir() {
        let m = MissingTool::Subagent("user-custom".into());
        let h = m.fix_hint();
        assert!(h.contains("custom agent"), "got: {h}");
        assert!(h.contains("~/.claude/agents/"), "got: {h}");
    }

    #[test]
    fn migrate_returns_empty_when_no_agents_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let reports = migrate_recommended_agent_symlinks(&claude, false).unwrap();
        assert!(reports.is_empty());
    }

    #[test]
    fn migrate_removes_only_marketplace_symlinks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let agents = claude.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        // Stage a plugin source file.
        let plugin_src = claude.join(
            "plugins/marketplaces/claude-plugins-official/plugins/pr-review-toolkit/agents/code-reviewer.md",
        );
        std::fs::create_dir_all(plugin_src.parent().unwrap()).unwrap();
        std::fs::write(&plugin_src, "# stub").unwrap();
        // ccteam-style symlink -> marketplace
        let stale = agents.join("code-reviewer.md");
        std::os::unix::fs::symlink(&plugin_src, &stale).unwrap();
        // Operator-authored regular file (must survive)
        let user_file = agents.join("user-custom.md");
        std::fs::write(&user_file, "# operator").unwrap();
        // Operator symlink to a non-marketplace path (must survive)
        let foreign_target = tmp.path().join("foreign.md");
        std::fs::write(&foreign_target, "# foreign").unwrap();
        let foreign_link = agents.join("foreign.md");
        std::os::unix::fs::symlink(&foreign_target, &foreign_link).unwrap();

        let reports = migrate_recommended_agent_symlinks(&claude, false).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].removed);
        assert!(!stale.exists());
        // Survivors:
        assert!(user_file.exists());
        assert!(foreign_link.symlink_metadata().unwrap().file_type().is_symlink());
    }

    #[test]
    fn migrate_dry_run_does_not_remove() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let agents = claude.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let plugin_src = claude.join(
            "plugins/marketplaces/claude-plugins-official/plugins/pr-review-toolkit/agents/code-reviewer.md",
        );
        std::fs::create_dir_all(plugin_src.parent().unwrap()).unwrap();
        std::fs::write(&plugin_src, "# stub").unwrap();
        let stale = agents.join("code-reviewer.md");
        std::os::unix::fs::symlink(&plugin_src, &stale).unwrap();

        let reports = migrate_recommended_agent_symlinks(&claude, true).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].removed);
        // Dry-run preserves the symlink.
        assert!(stale.symlink_metadata().unwrap().file_type().is_symlink());
    }
}
