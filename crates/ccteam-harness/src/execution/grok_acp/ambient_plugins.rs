//! Managed-grok shield against ambient Claude-plugin MCP servers.
//!
//! ccteam already decided that a managed grok session must not inherit the
//! user's ambient Claude MCP surface — that decision is
//! `GROK_CLAUDE_MCPS_ENABLED=false` in [`super::spawn_spec::build_envs`],
//! which closes the `~/.claude.json` door. Claude *plugins* are a second
//! door to the same room: grok discovers every plugin Claude installed
//! (`~/.claude/plugins/installed_plugins.json`), force-enables the ones
//! `~/.claude/settings.json` marks `true`, and starts each plugin's
//! `.mcp.json` servers as its own stdio children. One live grok, one orphan
//! MCP child — and for the official Telegram plugin that child claims the
//! bot-token `getUpdates` long-poll, which Telegram allows exactly one
//! consumer of, so it fights ccteam's own IM gateway (the same structural
//! conflict [`crate::execution::claude_tui::TELEGRAM_PLUGIN_ID`] names on
//! the Claude side).
//!
//! Grok 0.2.118 offers no switch for this. Verified against the installed
//! binary, all of these leave the plugin's MCP child running:
//!
//! - every `GROK_CLAUDE_{SKILLS,RULES,AGENTS,MCPS,HOOKS,SESSIONS}_ENABLED`
//!   compat cell (plugins are not one of the six cells);
//! - the project `.claude/settings.local.json` `enabledPlugins: false` pin
//!   ccteam writes for Claude — grok reads the *user*-level file only, and
//!   merges `true` entries in, so a project `false` cannot cancel one;
//! - `[plugins].disabled` in `.grok/config.toml`, project **or** user scope
//!   — Claude-sourced plugins bypass grok's own enable/disable registry;
//! - `CLAUDE_CONFIG_DIR` — grok resolves `$HOME/.claude` directly.
//!
//! What does work is `--plugin-dir`: the highest-priority plugin scope,
//! this-process-only, and name collisions there shadow every lower scope.
//! So for each installed Claude plugin that declares MCP servers we pass a
//! ccteam-owned empty plugin of the same name. The real plugin loses the
//! collision, and an empty plugin has no `.mcp.json` to start.
//!
//! Scope note: this is keyed on "declares MCP servers", not on a plugin
//! name-list. A plugin the user installs tomorrow is covered without a code
//! change, and plugins that ship only skills/commands/agents (no MCP child
//! to leak) are left completely alone. Shadowing does cost the shadowed
//! plugin's skills inside managed sessions — accepted: a managed session's
//! tool face is ccteam's A2A surface, and the alternative is an unaccounted
//! child process per session.
//!
//! Everything here is fail-open: an unreadable registry or an unwritable
//! shadow root yields no flags and a warning, never a failed spawn.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Claude's plugin install registry, relative to the Claude home.
const INSTALLED_PLUGINS: &str = "plugins/installed_plugins.json";

/// Shadow plugin roots live under the ccteam home's `cache/` dir — derived
/// state, rebuilt on demand, safe to delete.
const SHADOW_SUBDIR: &str = "cache/grok-plugin-shadows";

/// Manifest locations grok reads, in its own precedence order. A manifest
/// may declare `mcpServers` inline instead of shipping a `.mcp.json`.
const MANIFESTS: [&str; 3] = [
    ".grok-plugin/plugin.json",
    ".claude-plugin/plugin.json",
    "plugin.json",
];

/// True when the plugin rooted at `dir` would hand grok MCP servers to start.
fn declares_mcp_servers(dir: &Path) -> bool {
    if dir.join(".mcp.json").is_file() {
        return true;
    }
    MANIFESTS.iter().any(|rel| {
        std::fs::read_to_string(dir.join(rel))
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|v| v.get("mcpServers").cloned())
            .is_some_and(|v| !v.is_null())
    })
}

/// Plugin names (the part before `@<marketplace>`) that Claude has installed
/// and whose install path declares MCP servers. Sorted + deduped so the argv
/// a session spawns with is deterministic.
///
/// `claude_home` is the `.claude` directory itself, not `$HOME`.
pub fn mcp_bearing_plugins(claude_home: &Path) -> Vec<String> {
    let registry = claude_home.join(INSTALLED_PLUGINS);
    let Ok(body) = std::fs::read_to_string(&registry) else {
        // No Claude plugins installed (or no Claude at all) — nothing to shield.
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<Value>(&body) else {
        tracing::warn!(path = %registry.display(), "unparseable Claude plugin registry; grok ambient-plugin shield off");
        return Vec::new();
    };
    let Some(plugins) = root.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut names: Vec<String> = plugins
        .iter()
        .filter(|(_, installs)| {
            installs
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get("installPath").and_then(Value::as_str))
                .any(|path| declares_mcp_servers(Path::new(path)))
        })
        .map(|(id, _)| id.split('@').next().unwrap_or(id).to_string())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Materialize one empty shadow plugin per name from
/// [`mcp_bearing_plugins`] and return the dirs to pass as `--plugin-dir`.
///
/// Idempotent: an already-correct manifest is left untouched, so repeated
/// spawns don't churn mtimes. A name whose shadow can't be written is
/// skipped with a warning rather than failing the spawn.
pub fn ensure_shadows_in(claude_home: &Path, shadow_root: &Path) -> Vec<PathBuf> {
    mcp_bearing_plugins(claude_home)
        .into_iter()
        .filter_map(|name| match write_shadow(shadow_root, &name) {
            Ok(dir) => Some(dir),
            Err(e) => {
                tracing::warn!(plugin = %name, error = %e, "could not write grok plugin shadow; ambient MCP child may start");
                None
            }
        })
        .collect()
}

fn write_shadow(shadow_root: &Path, name: &str) -> std::io::Result<PathBuf> {
    let dir = shadow_root.join(name);
    let manifest_dir = dir.join(".claude-plugin");
    let manifest = manifest_dir.join("plugin.json");
    let body = format!(
        "{{\n  \"name\": \"{name}\",\n  \"version\": \"0.0.0\",\n  \
         \"description\": \"Empty stand-in: ccteam shadows this plugin inside managed grok sessions so its MCP servers stay out of the session.\"\n}}\n"
    );
    if std::fs::read_to_string(&manifest).is_ok_and(|on_disk| on_disk == body) {
        return Ok(dir);
    }
    std::fs::create_dir_all(&manifest_dir)?;
    std::fs::write(&manifest, body)?;
    Ok(dir)
}

/// Spawn-time entry point: shield the real `$HOME/.claude` using the real
/// ccteam home. Empty when either home is unresolvable.
pub fn managed_shadow_dirs() -> Vec<PathBuf> {
    let (Some(home), Some(root)) = (dirs::home_dir(), crate::ccteam_root_from_env()) else {
        return Vec::new();
    };
    ensure_shadows_in(&home.join(".claude"), &root.join(SHADOW_SUBDIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay down a Claude home whose registry lists `plugins`, each entry a
    /// `(id, has_mcp_json)` pair, and return the `.claude` dir.
    fn claude_home_with(tmp: &Path, plugins: &[(&str, bool)]) -> PathBuf {
        let claude = tmp.join(".claude");
        let mut entries = serde_json::Map::new();
        for (id, has_mcp) in plugins {
            let install = claude.join("plugins/cache").join(id);
            std::fs::create_dir_all(&install).unwrap();
            if *has_mcp {
                std::fs::write(install.join(".mcp.json"), r#"{"mcpServers":{"x":{}}}"#).unwrap();
            }
            entries.insert(
                (*id).to_string(),
                serde_json::json!([{ "scope": "user", "installPath": install }]),
            );
        }
        std::fs::create_dir_all(claude.join("plugins")).unwrap();
        std::fs::write(
            claude.join(INSTALLED_PLUGINS),
            serde_json::to_string(&serde_json::json!({ "version": 2, "plugins": entries }))
                .unwrap(),
        )
        .unwrap();
        claude
    }

    #[test]
    fn only_mcp_bearing_plugins_are_named() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = claude_home_with(
            tmp.path(),
            &[
                ("telegram@claude-plugins-official", true),
                ("frontend-design@claude-plugins-official", false),
            ],
        );
        // Skills-only plugins keep working inside managed grok; only the
        // ones that would start a child process get shadowed.
        assert_eq!(mcp_bearing_plugins(&claude), vec!["telegram".to_string()]);
    }

    #[test]
    fn manifest_declared_mcp_servers_count_too() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = claude_home_with(tmp.path(), &[("inline@mkt", false)]);
        let install = claude.join("plugins/cache/inline@mkt/.claude-plugin");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(
            install.join("plugin.json"),
            r#"{"name":"inline","mcpServers":".mcp.json"}"#,
        )
        .unwrap();
        assert_eq!(mcp_bearing_plugins(&claude), vec!["inline".to_string()]);
    }

    #[test]
    fn missing_or_unparseable_registry_fails_open() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(mcp_bearing_plugins(&tmp.path().join(".claude")).is_empty());

        let claude = tmp.path().join("broken");
        std::fs::create_dir_all(claude.join("plugins")).unwrap();
        std::fs::write(claude.join(INSTALLED_PLUGINS), "{not json").unwrap();
        assert!(mcp_bearing_plugins(&claude).is_empty());
    }

    #[test]
    fn shadow_is_an_empty_plugin_of_the_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = claude_home_with(tmp.path(), &[("telegram@claude-plugins-official", true)]);
        let root = tmp.path().join("shadows");

        let dirs = ensure_shadows_in(&claude, &root);
        assert_eq!(dirs, vec![root.join("telegram")]);

        // The whole point: the shadow declares the name and nothing else, so
        // the real plugin loses the collision and no MCP child starts.
        let manifest = dirs[0].join(".claude-plugin/plugin.json");
        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        assert_eq!(parsed["name"], "telegram");
        assert!(parsed.get("mcpServers").is_none());
        assert!(!dirs[0].join(".mcp.json").exists());
    }

    #[test]
    fn ensure_shadows_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = claude_home_with(tmp.path(), &[("telegram@claude-plugins-official", true)]);
        let root = tmp.path().join("shadows");

        let first = ensure_shadows_in(&claude, &root);
        let stamp = std::fs::metadata(first[0].join(".claude-plugin/plugin.json"))
            .unwrap()
            .modified()
            .unwrap();
        let second = ensure_shadows_in(&claude, &root);
        assert_eq!(first, second);
        assert_eq!(
            std::fs::metadata(second[0].join(".claude-plugin/plugin.json"))
                .unwrap()
                .modified()
                .unwrap(),
            stamp,
            "an unchanged shadow must not be rewritten on every spawn"
        );
    }
}
