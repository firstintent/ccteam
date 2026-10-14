//! Tool-surface foundation. Validates phase YAML's
//! `tools_required` (subagents / skills / MCP servers) against what's
//! reachable on this machine.
//!
//! V0.2 M0.20 (candidate 7) replaces the M0.5 `RECOMMENDED_AGENTS`
//! ln -sf protocol with Claude Code's in-memory plugin pipeline:
//! `bootstrap_project` writes `enabledPlugins` into the spawned
//! project's `.claude/settings.local.json` (v0.8.6 W2b — the
//! ccteam-managed local layer, never the user-committed
//! `settings.json`), Claude Code namespaces plugin agents as
//! `<plugin>:<name>` automatically, and ccteam-core no longer touches
//! `~/.claude/agents/`. The reachability check here
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
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
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
    std::fs::create_dir_all(&global).with_context(|| format!("create {}", global.display()))?;
    let local = project_dir.join(".claude").join("skills");
    std::fs::create_dir_all(&local).with_context(|| format!("create {}", local.display()))?;
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
        let mut subagents: BTreeSet<String> =
            BUILTIN_SUBAGENTS.iter().map(|s| (*s).to_string()).collect();
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
            MissingTool::Subagent(s) | MissingTool::Skill(s) | MissingTool::Mcp(s) => s.as_str(),
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
            return Err(err).with_context(|| format!("read_dir {}", agents_dir.display()));
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
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
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

/// Outcome of stripping ccteam's own Claude Code hooks from a project's
/// `.claude/settings.local.json`. See [`remove_ccteam_hooks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CcteamHookScrubAction {
    /// `settings.local.json` absent — nothing to do.
    NotFound,
    /// No ccteam hook entries found. Idempotent re-run after a successful
    /// scrub hits this branch.
    NoChangeNeeded,
    /// `dry_run = true` — would have removed N hook command entries.
    WouldRemove { entries: usize },
    /// Removed N hook command entries; the file still holds other keys
    /// (or other hooks) so it was atomically rewritten in place.
    Removed { entries: usize },
    /// Removed N hook command entries and the resulting JSON collapsed to an
    /// empty object, so the file — which existed only to carry ccteam's
    /// hooks — was deleted.
    RemovedNowEmpty { entries: usize },
}

/// Per-project report for [`remove_ccteam_hooks`].
#[derive(Debug, Clone)]
pub struct CcteamHookScrubReport {
    pub target: PathBuf,
    pub action: CcteamHookScrubAction,
}

/// Surgically strip ccteam's own Claude Code hooks from a project's
/// `.claude/settings.local.json` (the caller supplies the path).
///
/// docs-local issue #212: project creation no longer writes hooks — they only
/// ever served the retired tmux/rmux terminal protocol (the default
/// stream-json session is hookless: its events come from stdout, HITL from
/// `can_use_tool`). Projects created before that still carry the old
/// `hooks` block, so `ccteam init` (fresh or re-run), the web/IM create path
/// (`bootstrap_project_at_dir`) and `project rm --purge` all run this to
/// clean them.
///
/// **Scope (red line).** Removes ONLY entries whose `command` is a ccteam
/// hook ([`hook_command_is_ccteam_hook`]); every other key (`permissions`,
/// `env`, `enabledPlugins`, `effortLevel`, …) and any operator-authored hook
/// survives untouched. Empty inner hook lists + top-level event arrays are
/// pruned so the file shape stays clean, and if the whole object collapses
/// the file is deleted (`RemovedNowEmpty`).
///
/// Idempotent — a re-run after a successful scrub hits `NoChangeNeeded`.
pub fn remove_ccteam_hooks(settings_path: &Path, dry_run: bool) -> Result<CcteamHookScrubReport> {
    if !settings_path.exists() {
        return Ok(CcteamHookScrubReport {
            target: settings_path.to_path_buf(),
            action: CcteamHookScrubAction::NotFound,
        });
    }

    let body = std::fs::read_to_string(settings_path)
        .with_context(|| format!("read {}", settings_path.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("parse {}", settings_path.display()))?;

    let mut removed = 0usize;
    if let Some(hooks) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for (_event, list) in hooks.iter_mut() {
            let Some(arr) = list.as_array_mut() else {
                continue;
            };
            for entry in arr.iter_mut() {
                let Some(inner_hooks) = entry.get_mut("hooks").and_then(|h| h.as_array_mut())
                else {
                    continue;
                };
                let before = inner_hooks.len();
                inner_hooks.retain(|hook| !hook_command_is_ccteam_hook(hook));
                removed += before - inner_hooks.len();
            }
            // Drop now-empty inner hook lists (matcher entries whose hooks
            // we just emptied).
            arr.retain(|entry| {
                entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            });
        }
        // Drop now-empty top-level event arrays (e.g. SessionStart -> []).
        hooks.retain(|_event, list| list.as_array().map(|a| !a.is_empty()).unwrap_or(false));
    }

    if removed == 0 {
        return Ok(CcteamHookScrubReport {
            target: settings_path.to_path_buf(),
            action: CcteamHookScrubAction::NoChangeNeeded,
        });
    }

    // Drop an emptied `hooks` map so the collapse check below treats a
    // file that held only ccteam hooks as fully empty.
    if let Some(obj) = v.as_object_mut() {
        if obj
            .get("hooks")
            .and_then(|h| h.as_object())
            .map(|m| m.is_empty())
            .unwrap_or(false)
        {
            obj.remove("hooks");
        }
    }
    let now_empty = v.as_object().map(|o| o.is_empty()).unwrap_or(false);

    if dry_run {
        return Ok(CcteamHookScrubReport {
            target: settings_path.to_path_buf(),
            action: CcteamHookScrubAction::WouldRemove { entries: removed },
        });
    }

    if now_empty {
        std::fs::remove_file(settings_path)
            .with_context(|| format!("rm {}", settings_path.display()))?;
        return Ok(CcteamHookScrubReport {
            target: settings_path.to_path_buf(),
            action: CcteamHookScrubAction::RemovedNowEmpty { entries: removed },
        });
    }

    let body = serde_json::to_string_pretty(&v).context("serialize updated settings.local.json")?;
    let tmp = settings_path.with_extension("json.ccteam-hook-scrub.tmp");
    std::fs::write(&tmp, &body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, settings_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), settings_path.display()))?;
    Ok(CcteamHookScrubReport {
        target: settings_path.to_path_buf(),
        action: CcteamHookScrubAction::Removed { entries: removed },
    })
}

/// Hook kinds ccteam's `hooks/hook.sh` dispatcher has ever been invoked with
/// from a Claude Code settings file (`ccteam_hooks::dispatch`). A `hook.sh`
/// command only counts as ccteam's when its first argument is one of these,
/// so an operator's own `hook.sh` survives the scrub.
const CCTEAM_HOOK_KINDS: &[&str] = &[
    "progress-append",
    "load-context",
    "intercept-ask",
    "permission-request",
    "chat-progress",
];

/// Predicate: does this `hook` JSON entry invoke one of ccteam's own hooks?
/// Every shape ccteam has ever written, independent of the install path:
/// - `<…>/hook.sh <kind> [args]` — the dispatcher (project bootstrap
///   template + terminal-protocol chat hooks, incl. the HITL
///   `permission-request` and the `AskUserQuestion` `intercept-ask` entries)
/// - `<…>/ccteam [internal] hook <kind> …` / `<…>/cct hook <kind> …` — the
///   pre-dispatcher binary forms (incl. the retired `cost-accumulate` /
///   `parse-phase-end` kinds)
/// - `ccteam mux hook-emit …` — the `CCTEAM_HOOK_VIA_DAEMON` variant
fn hook_command_is_ccteam_hook(hook: &serde_json::Value) -> bool {
    let Some(cmd) = hook.get("command").and_then(|v| v.as_str()) else {
        return false;
    };
    let mut tokens = cmd.split_whitespace();
    let Some(program) = tokens.next() else {
        return false;
    };
    let basename = program.rsplit('/').next().unwrap_or(program);
    let rest: Vec<&str> = tokens.collect();
    match basename {
        "hook.sh" => rest
            .first()
            .is_some_and(|kind| CCTEAM_HOOK_KINDS.contains(kind)),
        "ccteam" | "cct" => matches!(
            rest.as_slice(),
            ["hook", _, ..] | ["internal", "hook", _, ..] | ["mux", "hook-emit", ..]
        ),
        _ => false,
    }
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
        let plugin_skill = claude.join("plugins/marketplaces/x/plugins/foo/skills/plug-skill");
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
        assert!(foreign_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
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
        // LEGACY_SKILL_NAMES tracks every bundled skill ccteam has ever
        // shipped + since retired, so an upgrade can sweep stale installs.
        // Currently 10: 3 F39-era `cct-*`, 2 F100 deletions
        // (`ccteam-team-author` / `ccteam-project-creator`), the 3 v0.8.6
        // W4b deletions (`ccteam-control` / `ccteam-creator` /
        // `ccteam-im-setup`), and `ccteam-team` / `ccteam-scan`. Pin the
        // length so a future entry addition forces a conscious revisit.
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        assert_eq!(reports.len(), 10);
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
    fn migrate_legacy_skill_dirs_removes_team_and_scan() {
        // v0.8.6 FIX #7: `ccteam-team` / `ccteam-scan` were shipped
        // skills too; upgraders who installed them must have the stale
        // dirs swept by the same migration path.
        let tmp = tempfile::TempDir::new().unwrap();
        let claude = tmp.path().join(".claude");
        let team_dir = write_legacy_skill(
            &claude,
            "ccteam-team",
            "<!-- ccteam-managed:skill begin -->\n---\nname: ccteam-team\n---\n",
        );
        let scan_dir = write_legacy_skill(
            &claude,
            "ccteam-scan",
            "<!-- ccteam-managed:skill begin -->\n---\nname: ccteam-scan\n---\n",
        );
        let reports = migrate_legacy_skill_dirs(&claude, false).unwrap();
        for name in ["ccteam-team", "ccteam-scan"] {
            let r = reports
                .iter()
                .find(|r| r.legacy_name == name)
                .unwrap_or_else(|| panic!("{name} must be in LEGACY_SKILL_NAMES"));
            assert_eq!(r.action, LegacySkillAction::Removed, "{name}");
        }
        assert!(!team_dir.exists());
        assert!(!scan_dir.exists());
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

    // ───────────────────── remove_ccteam_hooks ─────────────────────

    #[test]
    fn remove_ccteam_hooks_not_found_when_file_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        let report = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(report.action, CcteamHookScrubAction::NotFound);
    }

    #[test]
    fn remove_ccteam_hooks_no_change_for_operator_only_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        let original = r#"{"permissions":{"allow":["Bash"]},"hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"/h/my-linter.sh"}]}]}}"#;
        std::fs::write(&path, original).unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(report.action, CcteamHookScrubAction::NoChangeNeeded);
        // File untouched (still parses, still has the operator hook).
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("my-linter.sh"));
    }

    #[test]
    fn remove_ccteam_hooks_strips_chat_and_ask_keeps_others() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{
              "permissions": {"allow": ["Bash"]},
              "hooks": {
                "SessionStart": [{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress session-start"}]}],
                "PreToolUse": [
                  {"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress pre-tool-use"}]},
                  {"matcher":"AskUserQuestion","hooks":[{"type":"command","command":"/h/hook.sh intercept-ask","timeout":660}]},
                  {"matcher":"Edit","hooks":[{"type":"command","command":"/h/my-linter.sh"}]}
                ]
              }
            }"#,
        )
        .unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        // 1 SessionStart + 1 PreToolUse chat-progress + 1 intercept-ask = 3.
        assert_eq!(report.action, CcteamHookScrubAction::Removed { entries: 3 });
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v.get("permissions").is_some(), "operator key kept");
        assert!(
            v.get("hooks").and_then(|h| h.get("SessionStart")).is_none(),
            "emptied SessionStart event pruned",
        );
        // Operator's Edit hook survives under PreToolUse.
        let body = serde_json::to_string(&v).unwrap();
        assert!(body.contains("my-linter.sh"));
        assert!(!body.contains("chat-progress") && !body.contains("intercept-ask"));
    }

    #[test]
    fn remove_ccteam_hooks_reports_now_empty_when_only_ccteam_hooks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{"hooks":{"SessionStart":[{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress session-start"}]}],"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress stop"}]}]}}"#,
        )
        .unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(
            report.action,
            CcteamHookScrubAction::RemovedNowEmpty { entries: 2 }
        );
        // The file existed only to carry ccteam's hooks — it is deleted.
        assert!(!path.exists(), "now-empty settings.local.json is removed");
    }

    #[test]
    fn remove_ccteam_hooks_matches_daemon_bus_variant() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"ccteam mux hook-emit --kind chat-progress --action stop"}]}]}}"#,
        )
        .unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(
            report.action,
            CcteamHookScrubAction::RemovedNowEmpty { entries: 1 }
        );
    }

    /// v0.8.7 review-fix (R-M4) — a hitl session installs the
    /// `PermissionRequest` hook (`{hook_sh} permission-request`, see
    /// `claude_tui::ensure_chat_hooks_installed`). `project rm --purge`
    /// (which runs `remove_ccteam_hooks`) is the `init` inverse and MUST clear
    /// it; otherwise the project keeps a live HITL gate after deregistration.
    /// This pins both: the entry is counted/removed AND its now-empty
    /// `PermissionRequest` event section is pruned, while an operator hook
    /// under the same section survives.
    #[test]
    fn remove_ccteam_hooks_strips_permission_request_keeps_operator() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "SessionStart": [{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress session-start"}]}],
                "PermissionRequest": [
                  {"hooks":[{"type":"command","command":"/h/hook.sh permission-request"}]},
                  {"matcher":"Bash","hooks":[{"type":"command","command":"/h/operator-gate.sh"}]}
                ]
              }
            }"#,
        )
        .unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        // 1 SessionStart chat-progress + 1 PermissionRequest ccteam hook = 2.
        assert_eq!(report.action, CcteamHookScrubAction::Removed { entries: 2 });
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let body = serde_json::to_string(&v).unwrap();
        assert!(
            !body.contains("permission-request"),
            "ccteam PermissionRequest hook must be purged (R-M4): {body}"
        );
        // The operator's own PermissionRequest gate survives; the section is
        // NOT pruned because it still has a non-ccteam entry.
        assert!(body.contains("operator-gate.sh"), "operator hook kept");
        assert!(
            v.get("hooks")
                .and_then(|h| h.get("PermissionRequest"))
                .is_some(),
            "PermissionRequest section kept (operator entry remains)"
        );
        assert!(
            v.get("hooks").and_then(|h| h.get("SessionStart")).is_none(),
            "emptied SessionStart pruned"
        );
    }

    /// v0.8.7 review-fix (R-M4) — a hitl-only file (just chat hooks + the
    /// `PermissionRequest` HITL hook, no operator content) collapses to empty
    /// on purge so the caller deletes it (true `init` inverse).
    #[test]
    fn remove_ccteam_hooks_now_empty_includes_permission_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress stop"}]}],"PermissionRequest":[{"hooks":[{"type":"command","command":"/h/hook.sh permission-request"}]}]}}"#,
        )
        .unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(
            report.action,
            CcteamHookScrubAction::RemovedNowEmpty { entries: 2 }
        );
        assert!(!path.exists(), "now-empty settings.local.json is removed");
    }

    #[test]
    fn remove_ccteam_hooks_dry_run_does_not_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        let original = r#"{"hooks":{"SessionStart":[{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress session-start"}]}]}}"#;
        std::fs::write(&path, original).unwrap();
        let report = remove_ccteam_hooks(&path, true).unwrap();
        assert_eq!(
            report.action,
            CcteamHookScrubAction::WouldRemove { entries: 1 }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "dry-run must not rewrite the file",
        );
    }

    #[test]
    fn remove_ccteam_hooks_idempotent_after_scrub() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{"permissions":{},"hooks":{"SessionStart":[{"matcher":"*","hooks":[{"type":"command","command":"/h/hook.sh chat-progress session-start"}]}]}}"#,
        )
        .unwrap();
        let first = remove_ccteam_hooks(&path, false).unwrap();
        assert!(matches!(
            first.action,
            CcteamHookScrubAction::Removed { .. }
        ));
        let second = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(second.action, CcteamHookScrubAction::NoChangeNeeded);
    }

    /// docs-local issue #212 — the exact block project bootstrap used to
    /// write (`hooks/hook.sh progress-append|load-context|intercept-ask`) is
    /// stripped, while the non-hook keys that file carried and a user hook in
    /// the same event section survive.
    #[test]
    fn remove_ccteam_hooks_strips_bootstrap_template_hooks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.local.json");
        std::fs::write(
            &path,
            r#"{
              "permissions": {"defaultMode": "bypassPermissions", "deny": []},
              "env": {"CCTEAM_HOME": "/home/u/.ccteam"},
              "hooks": {
                "SessionStart": [{"hooks": [
                  {"type":"command","command":"/home/u/.ccteam/hooks/hook.sh load-context","timeout":5},
                  {"type":"command","command":"/home/u/.ccteam/hooks/hook.sh progress-append session_start","async":true}
                ]}],
                "Stop": [{"hooks": [{"type":"command","command":"/home/u/.ccteam/hooks/hook.sh progress-append Stop","async":true}]}],
                "PreToolUse": [
                  {"hooks": [{"type":"command","command":"/home/u/.ccteam/hooks/hook.sh progress-append PreToolUse","async":true}]},
                  {"matcher":"AskUserQuestion","hooks":[{"type":"command","command":"/home/u/.ccteam/hooks/hook.sh intercept-ask","timeout":5}]},
                  {"matcher":"Bash","hooks":[{"type":"command","command":"/home/u/bin/hook.sh audit"}]}
                ]
              }
            }"#,
        )
        .unwrap();
        let report = remove_ccteam_hooks(&path, false).unwrap();
        assert_eq!(report.action, CcteamHookScrubAction::Removed { entries: 5 });
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["permissions"]["defaultMode"], "bypassPermissions");
        assert_eq!(v["env"]["CCTEAM_HOME"], "/home/u/.ccteam");
        let hooks = v["hooks"].as_object().unwrap();
        assert_eq!(
            hooks.keys().collect::<Vec<_>>(),
            vec!["PreToolUse"],
            "only the user's hook section remains"
        );
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "/home/u/bin/hook.sh audit",
            "a user hook.sh with a non-ccteam argument is not ccteam's"
        );
    }

    /// The pre-dispatcher binary forms (`ccteam hook …`, `ccteam internal
    /// hook …`, `cct hook …`) incl. retired kinds are ccteam's too; an
    /// unrelated command merely mentioning `hook` is not.
    #[test]
    fn hook_command_is_ccteam_hook_matches_legacy_binary_forms() {
        let is = |cmd: &str| hook_command_is_ccteam_hook(&serde_json::json!({ "command": cmd }));
        assert!(is("/usr/local/bin/ccteam hook cost-accumulate"));
        assert!(is("ccteam hook parse-phase-end"));
        assert!(is("/home/u/.cargo/bin/ccteam internal hook load-context"));
        assert!(is("/opt/cct hook progress-append Stop"));
        assert!(is(
            "ccteam mux hook-emit --kind chat-progress --action stop"
        ));
        assert!(!is("ccteam status"));
        assert!(!is("/usr/bin/jq .progress[]"));
        assert!(!is("/h/hook.sh"));
        assert!(!is("git hook run pre-commit"));
    }
}
