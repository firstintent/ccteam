//! Delegated install of a vendor-native Claude Code **plugin** from the
//! ccteam-hub marketplace.
//!
//! Unlike agent / skill catalog entries — which ccteam carries verbatim and
//! copies into `.claude/{agents,skills}/` after a sha256 check — a *plugin*
//! (the `/plugin marketplace add` kind: slash commands + native deps +
//! install scripts) is **never** copied or executed by ccteam. Carrying its
//! body would force the hub to break its "verbatim-copy, never execute" red
//! line (running a third-party `install.sh`, building tree-sitter, …).
//!
//! Instead ccteam *delegates* to Claude Code's own plugin installer through
//! the two settings keys the vendor exposes for exactly this purpose
//! (`references/claude-code/src/utils/settings/types.ts`):
//!
//! - `extraKnownMarketplaces` — *"Additional marketplaces to make available
//!   for this repository. Typically used in repository .claude/settings.json
//!   to ensure team members have required plugin sources."* The map key MUST
//!   equal the marketplace's declared `name`.
//! - `enabledPlugins` — `"<plugin>@<marketplace>": true` turns a plugin on.
//!
//! ccteam writes both into the project's **`.claude/settings.local.json`** —
//! the layer it already owns (same file as the managed hooks +
//! `ensure_telegram_plugin_disabled`), so the user's committed
//! `.claude/settings.json` is never touched. On the next launch in that
//! project Claude Code sees the marketplace + the enabled plugin and does
//! ALL of the fetch / native-dep / install work itself, sandboxed by the
//! vendor. ccteam executes nothing.
//!
//! Because the actual install is the vendor's, the hub's pinning is
//! advisory for plugins (Claude tracks the marketplace's live ref): this
//! module records the pointer, it does not enforce a sha.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

/// The `enabledPlugins` map key for a plugin: `"<plugin>@<marketplace>"`.
/// This is the same `<plugin>@<marketplace>` convention Claude Code uses
/// everywhere (see [`crate::plugin_resolution`]).
pub fn enabled_plugin_key(plugin: &str, marketplace: &str) -> String {
    format!("{plugin}@{marketplace}")
}

/// Path of the ccteam-managed local settings for `project_dir`.
fn settings_local_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude").join("settings.local.json")
}

/// Read + parse `.claude/settings.local.json` as a JSON object (missing /
/// unparsable → empty object, mirroring the vendor + the hooks writer).
fn read_settings_object(settings_path: &Path) -> Value {
    if settings_path.exists() {
        std::fs::read_to_string(settings_path)
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
    } else {
        json!({})
    }
}

/// Enable a vendor-native plugin in `project_dir` by writing the marketplace
/// pointer + the enable flag into `.claude/settings.local.json`.
///
/// - `marketplace_name` — the marketplace's DECLARED name (from its
///   `.claude-plugin/marketplace.json`); becomes the `extraKnownMarketplaces`
///   key and the `@<marketplace>` half of the enable key.
/// - `marketplace_source` — the vendor `MarketplaceSource` object, e.g.
///   `{"source":"github","repo":"owner/repo"}`. Stored verbatim under
///   `extraKnownMarketplaces[name].source`.
/// - `plugin` — the plugin's name (from the marketplace's `plugins[]`).
///
/// Idempotent + non-clobbering: every other settings key (and every other
/// marketplace / enabled plugin) is preserved; re-running with the same args
/// is a no-op rewrite. Returns the `enabledPlugins` key that was set.
pub fn enable_marketplace_plugin(
    project_dir: &Path,
    marketplace_name: &str,
    marketplace_source: &Value,
    plugin: &str,
) -> Result<String> {
    let marketplace_name = marketplace_name.trim();
    let plugin = plugin.trim();
    if marketplace_name.is_empty() {
        bail!("marketplace name is empty");
    }
    if plugin.is_empty() {
        bail!("plugin name is empty");
    }
    if !marketplace_source.is_object() {
        bail!("marketplace source must be a JSON object, got {marketplace_source}");
    }

    let settings_path = settings_local_path(project_dir);
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut root = read_settings_object(&settings_path);
    let obj = root
        .as_object_mut()
        .expect("read_settings_object guarantees an object");

    // extraKnownMarketplaces[name] = { source: <MarketplaceSource> }
    let marketplaces = obj
        .entry("extraKnownMarketplaces")
        .or_insert_with(|| json!({}));
    let marketplaces = marketplaces.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("settings.local.json `extraKnownMarketplaces` is not an object")
    })?;
    // Preserve any sibling keys (e.g. autoUpdate) the user already set on this
    // marketplace; only (re)write `source`.
    let entry = marketplaces
        .entry(marketplace_name.to_string())
        .or_insert_with(|| json!({}));
    match entry.as_object_mut() {
        Some(map) => {
            map.insert("source".to_string(), marketplace_source.clone());
        }
        None => {
            let mut map = Map::new();
            map.insert("source".to_string(), marketplace_source.clone());
            *entry = Value::Object(map);
        }
    }

    // enabledPlugins["<plugin>@<marketplace>"] = true
    let key = enabled_plugin_key(plugin, marketplace_name);
    let enabled = obj.entry("enabledPlugins").or_insert_with(|| json!({}));
    let enabled = enabled
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.local.json `enabledPlugins` is not an object"))?;
    enabled.insert(key.clone(), Value::Bool(true));

    let serialized =
        serde_json::to_string_pretty(&root).context("serialize settings.local.json")?;
    std::fs::write(&settings_path, serialized)
        .with_context(|| format!("write {}", settings_path.display()))?;
    Ok(key)
}

/// Is `plugin@marketplace` enabled in `project_dir`'s local settings?
///
/// Best-effort (a missing / unparsable settings file → `false`): the web
/// catalog uses this to badge a plugin row `Installed`. Note this reflects
/// only that ccteam wrote the enable flag — the vendor's actual fetch happens
/// on the next launch, so a freshly-enabled plugin reads `true` here before
/// Claude Code has materialized it.
pub fn marketplace_plugin_enabled(
    project_dir: &Path,
    plugin: &str,
    marketplace_name: &str,
) -> bool {
    let settings_path = settings_local_path(project_dir);
    let root = read_settings_object(&settings_path);
    let key = enabled_plugin_key(plugin.trim(), marketplace_name.trim());
    root.get("enabledPlugins")
        .and_then(Value::as_object)
        .and_then(|m| m.get(&key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh_source(repo: &str) -> Value {
        json!({ "source": "github", "repo": repo })
    }

    #[test]
    fn enable_writes_both_keys_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let src = gh_source("Egonex-AI/Understand-Anything");

        let key =
            enable_marketplace_plugin(dir, "understand-anything", &src, "understand-anything")
                .unwrap();
        assert_eq!(key, "understand-anything@understand-anything");

        let body = std::fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["extraKnownMarketplaces"]["understand-anything"]["source"]["repo"],
            "Egonex-AI/Understand-Anything"
        );
        assert_eq!(
            v["extraKnownMarketplaces"]["understand-anything"]["source"]["source"],
            "github"
        );
        assert_eq!(
            v["enabledPlugins"]["understand-anything@understand-anything"],
            json!(true)
        );

        // Re-run → byte-identical (idempotent).
        let before = body;
        enable_marketplace_plugin(dir, "understand-anything", &src, "understand-anything").unwrap();
        let after = std::fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap();
        assert_eq!(before, after, "second enable must be a no-op rewrite");

        assert!(marketplace_plugin_enabled(
            dir,
            "understand-anything",
            "understand-anything"
        ));
        assert!(!marketplace_plugin_enabled(
            dir,
            "nope",
            "understand-anything"
        ));
    }

    #[test]
    fn enable_preserves_existing_settings_and_other_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.local.json"),
            serde_json::to_string_pretty(&json!({
                "hooks": { "Stop": [{ "matcher": "*" }] },
                "enabledPlugins": { "other@mkt": true },
                "extraKnownMarketplaces": { "mkt": { "source": gh_source("a/b"), "autoUpdate": true } }
            }))
            .unwrap(),
        )
        .unwrap();

        enable_marketplace_plugin(
            dir,
            "egonex",
            &gh_source("Egonex-AI/Understand-Anything"),
            "ua",
        )
        .unwrap();

        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        // Untouched neighbours preserved.
        assert!(v["hooks"].is_object());
        assert_eq!(v["enabledPlugins"]["other@mkt"], json!(true));
        assert_eq!(
            v["extraKnownMarketplaces"]["mkt"]["autoUpdate"],
            json!(true)
        );
        // New plugin added.
        assert_eq!(v["enabledPlugins"]["ua@egonex"], json!(true));
        assert_eq!(
            v["extraKnownMarketplaces"]["egonex"]["source"]["repo"],
            "Egonex-AI/Understand-Anything"
        );
    }

    #[test]
    fn empty_inputs_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(enable_marketplace_plugin(tmp.path(), "", &gh_source("a/b"), "p").is_err());
        assert!(enable_marketplace_plugin(tmp.path(), "m", &gh_source("a/b"), "").is_err());
        assert!(enable_marketplace_plugin(tmp.path(), "m", &json!("notobj"), "p").is_err());
    }
}
