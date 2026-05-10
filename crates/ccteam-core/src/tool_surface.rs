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

/// V0.2.2 F44: outcome of a single legacy skill directory check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacySkillAction {
    /// Directory absent — user never installed this F39-era skill, or
    /// already cleaned up. Reported as "no-op".
    NotFound,
    /// Body carried the `<!-- ccteam-managed:skill … -->` marker (or the
    /// matching frontmatter `name:`); we wrote the stale dir away.
    Removed,
    /// `dry_run=true` — would have removed; filesystem untouched.
    WouldRemove,
    /// Body had no marker — operator hand-edited the legacy skill.
    /// Preserved (with a warning logged via the report); user must
    /// remove manually.
    PreservedHandEdit,
}

/// V0.2.2 F44: report for one `~/.claude/skills/<legacy>/` directory.
#[derive(Debug, Clone)]
pub struct LegacySkillReport {
    /// `<claude_dir>/skills/<legacy_name>/`.
    pub target: PathBuf,
    /// One of the F39-era `LEGACY_SKILL_NAMES` entries (`cct-*`).
    pub legacy_name: String,
    pub action: LegacySkillAction,
}

/// V0.2.2 F44: detect and (when safe) remove
/// `~/.claude/skills/cct-{control,team-author,project-creator}/` dirs
/// left over from V0.2.2 (F39'd) installs. The `ccteam doctor` migration
/// path calls this after the canonical `ccteam-{control,team-author,
/// project-creator}` skills have been installed so users can collapse
/// to a single skill set.
///
/// **Safety contract**: only removes a legacy directory whose `SKILL.md`
/// body still carries the `<!-- ccteam-managed:skill ... -->` marker or
/// the matching `name: cct-{control,team-author,project-creator}` YAML
/// frontmatter. Operator hand-edits (no marker, no canonical
/// frontmatter) are preserved as-is — the user must remove them
/// manually if they want a clean tree.
///
/// `dry_run=true` reports without touching the filesystem.
pub fn migrate_legacy_skill_dirs(
    claude_dir: &Path,
    dry_run: bool,
) -> Result<Vec<LegacySkillReport>> {
    use crate::skill::LEGACY_SKILL_NAMES;

    let skills_dir = claude_dir.join("skills");
    let mut out: Vec<LegacySkillReport> = Vec::new();

    for legacy in LEGACY_SKILL_NAMES {
        let dir = skills_dir.join(legacy);
        let skill_md = dir.join("SKILL.md");
        if !dir.exists() {
            out.push(LegacySkillReport {
                target: dir,
                legacy_name: (*legacy).to_string(),
                action: LegacySkillAction::NotFound,
            });
            continue;
        }

        // Body must carry the ccteam-managed marker (the body we shipped
        // through V0.2.2 F39) **or** the canonical frontmatter
        // `name: cct-…` so we can be sure it's the unedited shipped
        // skill. Either signal alone is enough.
        let body = std::fs::read_to_string(&skill_md).unwrap_or_default();
        let canonical_frontmatter = format!("name: {legacy}");
        let is_managed = body.contains("<!-- ccteam-managed:skill begin -->")
            || body.contains(&canonical_frontmatter);
        if !is_managed {
            out.push(LegacySkillReport {
                target: dir,
                legacy_name: (*legacy).to_string(),
                action: LegacySkillAction::PreservedHandEdit,
            });
            continue;
        }

        if dry_run {
            out.push(LegacySkillReport {
                target: dir,
                legacy_name: (*legacy).to_string(),
                action: LegacySkillAction::WouldRemove,
            });
            continue;
        }

        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove_dir_all {}", dir.display()))?;
        out.push(LegacySkillReport {
            target: dir,
            legacy_name: (*legacy).to_string(),
            action: LegacySkillAction::Removed,
        });
    }
    Ok(out)
}

/// V0.2.2 F44: outcome of a single project's settings.json hook
/// command rewrite (legacy `… cct hook …` → `<current_exe> hook …`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCmdRewriteAction {
    /// File absent — nothing to do.
    NotFound,
    /// File present, no hook command referenced an F39-era `cct`
    /// binary path. Idempotent re-run after a successful migration
    /// hits this branch.
    NoChangeNeeded,
    /// `dry_run=true` — would have rewritten N entries.
    WouldRewrite { entries: usize },
    /// Rewrote N hook commands and atomically wrote the file.
    Rewrote { entries: usize },
}

/// V0.2.2 F44: report for one `~/projects/<slug>/.claude/settings.json`.
#[derive(Debug, Clone)]
pub struct HookCmdRewriteReport {
    pub target: PathBuf,
    pub action: HookCmdRewriteAction,
}

/// V0.2.2 F44: rewrite F39-era `… /cct hook …` (or `cct hook …`
/// without an absolute prefix) entries in a project's settings.json
/// so they invoke the canonical `ccteam` binary at the running
/// process's `current_exe()` path. Idempotent — re-runs after success
/// hit `NoChangeNeeded`.
///
/// Detection rule: any `command` string that ends with the F39 segment
/// `/cct hook ` or starts with `cct hook ` (no slash). The replacement
/// substitutes everything up through the binary path with the new
/// binary path. Hook subcommands (e.g. `progress-append
/// session_start`) are preserved verbatim so this works for every hook
/// in `templates/settings.json`.
pub fn rewrite_legacy_hook_commands(
    settings_path: &Path,
    new_bin: &Path,
    dry_run: bool,
) -> Result<HookCmdRewriteReport> {
    if !settings_path.exists() {
        return Ok(HookCmdRewriteReport {
            target: settings_path.to_path_buf(),
            action: HookCmdRewriteAction::NotFound,
        });
    }

    let new_bin_str = new_bin
        .to_str()
        .ok_or_else(|| anyhow!("ccteam binary path not valid UTF-8: {}", new_bin.display()))?;
    if new_bin_str.contains('"') || new_bin_str.contains('\\') {
        return Err(anyhow!(
            "ccteam binary path contains characters that can't be embedded in settings.json: {new_bin_str}"
        ));
    }

    let body = std::fs::read_to_string(settings_path)
        .with_context(|| format!("read {}", settings_path.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", settings_path.display()))?;

    let mut rewritten = 0usize;
    let Some(hooks) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(HookCmdRewriteReport {
            target: settings_path.to_path_buf(),
            action: HookCmdRewriteAction::NoChangeNeeded,
        });
    };
    for (_event, list) in hooks.iter_mut() {
        let Some(arr) = list.as_array_mut() else {
            continue;
        };
        for entry in arr.iter_mut() {
            let Some(inner_hooks) =
                entry.get_mut("hooks").and_then(|h| h.as_array_mut())
            else {
                continue;
            };
            for hook in inner_hooks.iter_mut() {
                let Some(cmd_val) = hook.get_mut("command") else {
                    continue;
                };
                let Some(cmd) = cmd_val.as_str() else {
                    continue;
                };
                if let Some(new_cmd) = rewrite_one_hook_command(cmd, new_bin_str) {
                    *cmd_val = serde_json::Value::String(new_cmd);
                    rewritten += 1;
                }
            }
        }
    }

    if rewritten == 0 {
        return Ok(HookCmdRewriteReport {
            target: settings_path.to_path_buf(),
            action: HookCmdRewriteAction::NoChangeNeeded,
        });
    }
    if dry_run {
        return Ok(HookCmdRewriteReport {
            target: settings_path.to_path_buf(),
            action: HookCmdRewriteAction::WouldRewrite { entries: rewritten },
        });
    }

    let body = serde_json::to_string_pretty(&v).context("serialize updated settings.json")?;
    // Atomic write: write to `<path>.tmp` then rename so a crash mid-
    // write can't leave a half-rewritten settings.json on disk.
    let tmp = settings_path.with_extension("json.ccteam-migrate.tmp");
    std::fs::write(&tmp, &body)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, settings_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), settings_path.display()))?;
    Ok(HookCmdRewriteReport {
        target: settings_path.to_path_buf(),
        action: HookCmdRewriteAction::Rewrote { entries: rewritten },
    })
}

/// V0.2.2 F44: rewrite one hook `command` string. Returns `None` when
/// the command does not look like an F39-era `cct hook …` invocation
/// (so the caller can leave it alone — preserves operator-authored
/// hooks). Otherwise returns the rewritten string with the binary path
/// swapped to `new_bin` (the canonical `ccteam` binary).
fn rewrite_one_hook_command(cmd: &str, new_bin: &str) -> Option<String> {
    // Already pointing at the canonical `ccteam` binary? Idempotency.
    if cmd.contains("/ccteam hook ") || cmd.starts_with("ccteam hook ") {
        return None;
    }
    // Absolute path ending with `/cct hook …` (F39 binary name).
    if let Some(idx) = cmd.find("/cct hook ") {
        let tail = &cmd[idx + "/cct".len()..];
        return Some(format!("{new_bin}{tail}"));
    }
    // Bare `cct hook …` with no path prefix (uncommon but legal).
    if let Some(rest) = cmd.strip_prefix("cct hook ") {
        return Some(format!("{new_bin} hook {rest}"));
    }
    None
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

    // -------------------- V0.2.2 F44 reverse-migration helpers --------------------

    fn write_legacy_skill(claude: &Path, name: &str, body: &str) -> PathBuf {
        let dir = claude.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, body).unwrap();
        dir
    }

    #[test]
    fn migrate_legacy_skill_dirs_reports_not_found_when_clean() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        assert_eq!(reports.len(), 3);
        for r in &reports {
            assert_eq!(r.action, LegacySkillAction::NotFound);
        }
    }

    #[test]
    fn migrate_legacy_skill_dirs_removes_managed_install() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let dir = write_legacy_skill(
            &claude,
            "cct-control",
            "<!-- ccteam-managed:skill begin -->\n---\nname: cct-control\n---\n# body\n<!-- ccteam-managed:skill end -->\n",
        );
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        let ctrl = reports
            .iter()
            .find(|r| r.legacy_name == "cct-control")
            .unwrap();
        assert_eq!(ctrl.action, LegacySkillAction::Removed);
        assert!(!dir.exists());
    }

    #[test]
    fn migrate_legacy_skill_dirs_removes_via_frontmatter_only() {
        // F39 always shipped the marker, but if a user installed the
        // body via some other path that stripped the comment marker,
        // detect via frontmatter `name:` instead.
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let dir = write_legacy_skill(
            &claude,
            "cct-team-author",
            "---\nname: cct-team-author\n---\n# body\n",
        );
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        let ta = reports
            .iter()
            .find(|r| r.legacy_name == "cct-team-author")
            .unwrap();
        assert_eq!(ta.action, LegacySkillAction::Removed);
        assert!(!dir.exists());
    }

    #[test]
    fn migrate_legacy_skill_dirs_preserves_hand_edits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let dir = write_legacy_skill(
            &claude,
            "cct-control",
            "---\nname: my-custom-fork\n---\n# user wrote this\n",
        );
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        let ctrl = reports
            .iter()
            .find(|r| r.legacy_name == "cct-control")
            .unwrap();
        assert_eq!(ctrl.action, LegacySkillAction::PreservedHandEdit);
        assert!(dir.exists());
    }

    #[test]
    fn migrate_legacy_skill_dirs_dry_run_does_not_remove() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let dir = write_legacy_skill(
            &claude,
            "cct-control",
            "<!-- ccteam-managed:skill begin -->\n---\nname: cct-control\n---\n",
        );
        let reports = migrate_legacy_skill_dirs(&claude, true).unwrap();
        let ctrl = reports
            .iter()
            .find(|r| r.legacy_name == "cct-control")
            .unwrap();
        assert_eq!(ctrl.action, LegacySkillAction::WouldRemove);
        assert!(dir.exists());
    }

    #[test]
    fn migrate_legacy_skill_dirs_handles_project_creator() {
        // F39 added cct-project-creator (F34); F44 must clean it up too.
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let dir = write_legacy_skill(
            &claude,
            "cct-project-creator",
            "<!-- ccteam-managed:skill begin -->\n---\nname: cct-project-creator\n---\n",
        );
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        let pc = reports
            .iter()
            .find(|r| r.legacy_name == "cct-project-creator")
            .unwrap();
        assert_eq!(pc.action, LegacySkillAction::Removed);
        assert!(!dir.exists());
    }

    #[test]
    fn migrate_legacy_skill_dirs_idempotent_after_first_run() {
        // Running F44 migration twice is a no-op the second time.
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        write_legacy_skill(
            &claude,
            "cct-control",
            "<!-- ccteam-managed:skill begin -->\n---\nname: cct-control\n---\n",
        );
        // First pass removes.
        migrate_legacy_skill_dirs(&claude, false).unwrap();
        // Second pass is a no-op (everything reports NotFound).
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        for r in &reports {
            assert_eq!(r.action, LegacySkillAction::NotFound);
        }
    }

    fn legacy_settings_json(legacy_bin: &str) -> String {
        format!(
            r#"{{
  "hooks": {{
    "SessionStart": [
      {{"hooks": [
        {{"type": "command", "command": "{legacy_bin} hook load-context", "timeout": 5}},
        {{"type": "command", "command": "{legacy_bin} hook progress-append session_start", "async": true}}
      ]}}
    ],
    "Stop": [
      {{"hooks": [{{"type": "command", "command": "{legacy_bin} hook parse-phase-end"}}]}}
    ]
  }}
}}"#
        )
    }

    #[test]
    fn rewrite_legacy_hook_commands_swaps_absolute_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, legacy_settings_json("/home/u/.cargo/bin/cct")).unwrap();
        let new_bin = PathBuf::from("/home/u/.cargo/bin/ccteam");
        let rep = rewrite_legacy_hook_commands(&path, &new_bin, false).unwrap();
        assert_eq!(rep.action, HookCmdRewriteAction::Rewrote { entries: 3 });
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("/home/u/.cargo/bin/ccteam hook load-context"), "got: {body}");
        assert!(!body.contains("/cct hook"), "F39-era path survived: {body}");
    }

    #[test]
    fn rewrite_legacy_hook_commands_dry_run_does_not_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let original = legacy_settings_json("/home/u/.cargo/bin/cct");
        std::fs::write(&path, &original).unwrap();
        let new_bin = PathBuf::from("/home/u/.cargo/bin/ccteam");
        let rep = rewrite_legacy_hook_commands(&path, &new_bin, true).unwrap();
        assert_eq!(rep.action, HookCmdRewriteAction::WouldRewrite { entries: 3 });
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, original);
    }

    #[test]
    fn rewrite_legacy_hook_commands_idempotent_when_already_ccteam() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, legacy_settings_json("/home/u/.cargo/bin/ccteam")).unwrap();
        let new_bin = PathBuf::from("/home/u/.cargo/bin/ccteam");
        let rep = rewrite_legacy_hook_commands(&path, &new_bin, false).unwrap();
        assert_eq!(rep.action, HookCmdRewriteAction::NoChangeNeeded);
    }

    #[test]
    fn rewrite_legacy_hook_commands_handles_missing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let new_bin = PathBuf::from("/usr/local/bin/ccteam");
        let rep = rewrite_legacy_hook_commands(&path, &new_bin, false).unwrap();
        assert_eq!(rep.action, HookCmdRewriteAction::NotFound);
    }

    #[test]
    fn rewrite_one_hook_command_handles_bare_cct_prefix() {
        let got = rewrite_one_hook_command("cct hook progress-append PreToolUse", "/x/ccteam");
        assert_eq!(
            got.as_deref(),
            Some("/x/ccteam hook progress-append PreToolUse"),
        );
    }

    #[test]
    fn rewrite_one_hook_command_returns_none_for_unrelated_commands() {
        assert!(
            rewrite_one_hook_command("/usr/bin/jq .progress[]", "/x/ccteam").is_none(),
            "should not touch operator-authored hooks",
        );
    }
}
