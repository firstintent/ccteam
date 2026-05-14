//! Static map from `subagent_type` (the bare name a phase YAML's
//! `tools_required.subagents` lists, eg `code-reviewer`) to the plugin
//! that ships it (`pr-review-toolkit@claude-plugins-official`).
//!
//! Used by `bootstrap_project` to compute the `enabledPlugins` set
//! written into the spawned project's `<project>/.claude/settings.json`,
//! and by the tool-surface validator + `ccteam doctor --tool-surface`
//! to decide whether a phase's declared subagent dependency is
//! reachable through Claude Code's in-memory plugin pipeline (V0.2 §6.5
//! candidate 7 / review §2.2). Replaces the M0.5 `RECOMMENDED_AGENTS`
//! ln -sf protocol — Claude Code's plugin pipeline auto-namespaces
//! plugin agents as `<plugin>:<name>` once the plugin is enabled, so
//! ccteam-core no longer has to symlink each agent file into
//! `~/.claude/agents/`.
//!
//! The map is intentionally static for V0.2 — V0.3 will add a runtime
//! discovery pass over `~/.claude/plugins/marketplaces/<mkt>/plugins/`
//! that picks up any plugin shipping `<plugin>/agents/<name>.md`.

use std::path::{Path, PathBuf};

/// One plugin agent ccteam phase YAML can reference by its bare name
/// in `tools_required.subagents`. Maps that bare name to the plugin
/// that ships it so `bootstrap_project` knows which `<plugin>@<mkt>`
/// to enable in the project's `.claude/settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginAgent {
    /// The bare `subagent_type` phase YAML lists (no namespace prefix).
    pub subagent: &'static str,
    /// The plugin directory name under
    /// `<claude_dir>/plugins/marketplaces/<marketplace>/plugins/`.
    pub plugin: &'static str,
    /// The marketplace directory name.
    pub marketplace: &'static str,
    /// Path inside the plugin to the agent file (relative).
    pub relpath: &'static str,
}

impl PluginAgent {
    /// `enabledPlugins` map key (`<plugin>@<marketplace>`).
    pub fn plugin_id(&self) -> String {
        format!("{}@{}", self.plugin, self.marketplace)
    }

    /// Absolute path to the plugin's agent file.
    pub fn source_path(&self, claude_dir: &Path) -> PathBuf {
        claude_dir
            .join("plugins")
            .join("marketplaces")
            .join(self.marketplace)
            .join("plugins")
            .join(self.plugin)
            .join(self.relpath)
    }
}

/// V0.2 static plugin-agent map. Same eight `claude-plugins-official`
/// agents previously hard-symlinked by `RECOMMENDED_AGENTS`; expressed
/// here as data the plugin-pipeline writer + reachability checker both
/// consume.
pub const KNOWN_PLUGIN_AGENTS: &[PluginAgent] = &[
    PluginAgent {
        subagent: "code-reviewer",
        plugin: "pr-review-toolkit",
        marketplace: "claude-plugins-official",
        relpath: "agents/code-reviewer.md",
    },
    PluginAgent {
        subagent: "silent-failure-hunter",
        plugin: "pr-review-toolkit",
        marketplace: "claude-plugins-official",
        relpath: "agents/silent-failure-hunter.md",
    },
    PluginAgent {
        subagent: "pr-test-analyzer",
        plugin: "pr-review-toolkit",
        marketplace: "claude-plugins-official",
        relpath: "agents/pr-test-analyzer.md",
    },
    PluginAgent {
        subagent: "type-design-analyzer",
        plugin: "pr-review-toolkit",
        marketplace: "claude-plugins-official",
        relpath: "agents/type-design-analyzer.md",
    },
    PluginAgent {
        subagent: "comment-analyzer",
        plugin: "pr-review-toolkit",
        marketplace: "claude-plugins-official",
        relpath: "agents/comment-analyzer.md",
    },
    PluginAgent {
        subagent: "code-architect",
        plugin: "feature-dev",
        marketplace: "claude-plugins-official",
        relpath: "agents/code-architect.md",
    },
    PluginAgent {
        subagent: "code-explorer",
        plugin: "feature-dev",
        marketplace: "claude-plugins-official",
        relpath: "agents/code-explorer.md",
    },
    PluginAgent {
        subagent: "code-simplifier",
        plugin: "code-simplifier",
        marketplace: "claude-plugins-official",
        relpath: "agents/code-simplifier.md",
    },
];

/// Look up a bare `subagent_type` in the static map. Returns `None`
/// for built-ins (`general-purpose`, `Explore`, …) and user-authored
/// agents that ccteam doesn't ship a plugin for.
pub fn lookup_plugin_agent(subagent: &str) -> Option<&'static PluginAgent> {
    KNOWN_PLUGIN_AGENTS.iter().find(|a| a.subagent == subagent)
}

/// Compute the `enabledPlugins` map (`<plugin>@<mkt>` → `true`) a
/// spawned project session needs to wire the plugin pipeline for every
/// subagent the team's phase YAML declares. Bare names with no entry
/// in [`KNOWN_PLUGIN_AGENTS`] are ignored — they're either built-ins
/// (no enable needed) or user-authored agents the operator drops into
/// `~/.claude/agents/` themselves.
pub fn plugins_to_enable<'a, I: IntoIterator<Item = &'a str>>(
    subagents: I,
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for name in subagents {
        if let Some(a) = lookup_plugin_agent(name) {
            out.insert(a.plugin_id());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_plugin_agents_has_eight_distinct_subagents() {
        assert_eq!(KNOWN_PLUGIN_AGENTS.len(), 8);
        let mut names: Vec<&str> = KNOWN_PLUGIN_AGENTS.iter().map(|a| a.subagent).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn plugin_id_renders_plugin_at_marketplace() {
        let cr = lookup_plugin_agent("code-reviewer").unwrap();
        assert_eq!(cr.plugin_id(), "pr-review-toolkit@claude-plugins-official");
    }

    #[test]
    fn lookup_returns_none_for_builtin() {
        assert!(lookup_plugin_agent("general-purpose").is_none());
        assert!(lookup_plugin_agent("Explore").is_none());
    }

    #[test]
    fn plugins_to_enable_dedups_and_skips_unknown() {
        let set = plugins_to_enable([
            "code-reviewer",
            "silent-failure-hunter", // same plugin as code-reviewer
            "code-architect",
            "general-purpose", // built-in, skipped
            "user-authored",   // unknown, skipped
        ]);
        let mut v: Vec<&String> = set.iter().collect();
        v.sort();
        assert_eq!(
            v,
            vec![
                &"feature-dev@claude-plugins-official".to_string(),
                &"pr-review-toolkit@claude-plugins-official".to_string(),
            ],
        );
    }

    #[test]
    fn plugins_to_enable_empty_when_no_plugin_subagents() {
        let set = plugins_to_enable(["general-purpose", "Explore"]);
        assert!(set.is_empty());
    }
}
