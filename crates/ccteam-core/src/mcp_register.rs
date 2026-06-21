//! ccteam's own MCP-server registration into the vendor configs.
//!
//! This is the **one** thing ccteam writes into a vendor's footprint
//! (red line: "ccteam executes/writes nothing else"; registering its own
//! MCP server is the single allowed write). Both the CLI (`ccteam config`)
//! and `ccteam-web`'s `POST /api/v1/hosts/{host}/register-mcp` call these
//! seams, so the merge logic lives in exactly one place.
//!
//! All writers are **merge, never clobber + idempotent**: an existing
//! config keeps every other key / MCP server; only `ccteam` is set. The
//! `_into` seams take an explicit path so they are unit-testable against a
//! temp dir without touching the real `~/.claude.json` / `~/.codex`.
//!
//! v0.8.18 柱1 — moved here from `ccteam-cli::mcp_serve` (which now
//! re-exports) so the web host page can register MCP without depending on
//! the CLI binary crate.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// Register the ccteam MCP server in Claude's `~/.claude.json`.
///
/// Strategy: read the file, ensure `mcpServers.ccteam` points at the
/// running binary's absolute path, write back atomically. A junk /
/// non-object root is tolerated (treated as "start fresh") rather than
/// failing the install.
pub fn install_mcp_into(claude_json: &Path, ccteam_bin: &Path) -> Result<()> {
    let bin = ccteam_bin
        .to_str()
        .ok_or_else(|| anyhow!("ccteam binary path not valid UTF-8"))?;
    let mut root = if claude_json.exists() {
        let bytes = std::fs::read(claude_json)
            .with_context(|| format!("read {}", claude_json.display()))?;
        if bytes.is_empty() {
            serde_json::Map::new()
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            }
        }
    } else {
        serde_json::Map::new()
    };
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let map = match servers {
        Value::Object(m) => m,
        _ => {
            *servers = Value::Object(serde_json::Map::new());
            servers.as_object_mut().unwrap()
        }
    };
    map.insert(
        crate::CCTEAM_MCP_SERVER_KEY.into(),
        json!({
            "command": bin,
            "args": crate::CCTEAM_MCP_SERVE_ARGS.to_vec(),
            "env": {},
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    atomic_write(claude_json, body.as_bytes())
}

/// Codex equivalent of [`install_mcp_into`]: register the ccteam MCP server
/// in Codex's `config.toml` as a `[mcp_servers.ccteam]` stdio table.
///
/// MERGE, never clobber: every other top-level key and every other
/// `[mcp_servers.*]` entry is preserved; only `mcp_servers.ccteam` is
/// set/replaced. Parent dir + file are created if absent. Idempotent.
pub fn install_codex_mcp_into(config_toml: &Path, ccteam_bin: &Path) -> Result<()> {
    let bin = ccteam_bin
        .to_str()
        .ok_or_else(|| anyhow!("ccteam binary path not valid UTF-8"))?;

    // Parse the existing config (or start empty). A non-table / unparseable
    // root is treated as "start fresh" rather than failing the install,
    // mirroring `install_mcp_into`'s tolerance for a junk `~/.claude.json`.
    let mut root: toml::Table = if config_toml.exists() {
        let body = std::fs::read_to_string(config_toml)
            .with_context(|| format!("read {}", config_toml.display()))?;
        if body.trim().is_empty() {
            toml::Table::new()
        } else {
            toml::from_str::<toml::Table>(&body)
                .with_context(|| format!("parse existing {}", config_toml.display()))?
        }
    } else {
        toml::Table::new()
    };

    // Ensure `[mcp_servers]` is a table, preserving any sibling servers.
    let servers_entry = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !servers_entry.is_table() {
        anyhow::bail!(
            "{}: `mcp_servers` exists but is not a TOML table",
            config_toml.display()
        );
    }
    let servers = servers_entry.as_table_mut().expect("checked above");

    // Build the ccteam server entry: command + shared args const.
    let mut entry = toml::Table::new();
    entry.insert("command".to_string(), toml::Value::String(bin.to_string()));
    entry.insert(
        "args".to_string(),
        toml::Value::Array(
            crate::CCTEAM_MCP_SERVE_ARGS
                .iter()
                .map(|s| toml::Value::String((*s).to_string()))
                .collect(),
        ),
    );
    servers.insert(
        crate::CCTEAM_MCP_SERVER_KEY.to_string(),
        toml::Value::Table(entry),
    );

    let body = toml::to_string_pretty(&root).context("serialize codex config.toml")?;
    atomic_write(config_toml, body.as_bytes())
}

/// Whether `~/.claude.json` already carries the ccteam MCP server entry.
/// Best-effort: a missing / unreadable / junk file reads as `false` (not
/// registered). Read-only — never writes.
pub fn claude_mcp_registered(claude_json: &Path) -> bool {
    let Ok(bytes) = std::fs::read(claude_json) else {
        return false;
    };
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    root.get("mcpServers")
        .and_then(Value::as_object)
        .map(|m| m.contains_key(crate::CCTEAM_MCP_SERVER_KEY))
        .unwrap_or(false)
}

/// Whether Codex's `config.toml` already carries `[mcp_servers.ccteam]`.
/// Best-effort: a missing / unreadable / junk file reads as `false`.
pub fn codex_mcp_registered(config_toml: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(config_toml) else {
        return false;
    };
    let Ok(root) = toml::from_str::<toml::Table>(&body) else {
        return false;
    };
    root.get("mcp_servers")
        .and_then(toml::Value::as_table)
        .map(|t| t.contains_key(crate::CCTEAM_MCP_SERVER_KEY))
        .unwrap_or(false)
}

/// Resolve `$CODEX_HOME/config.toml` (CODEX_HOME → `~/.codex` fallback),
/// mirroring `ccteam_harness::execution::codex_app_server`'s resolution.
pub fn resolve_codex_config_path() -> Result<PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .ok_or_else(|| anyhow!("cannot resolve CODEX_HOME or ~/.codex (no home dir)"))?;
    Ok(codex_home.join("config.toml"))
}

/// Atomic write via a sibling `.ccteam-mcp.tmp` + rename. Creates the
/// parent dir if absent.
fn atomic_write(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".ccteam-mcp.tmp");
    let tmp = PathBuf::from(tmp_os);
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(body)?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_mcp_into_writes_command_args_env_for_ccteam_server() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        install_mcp_into(&claude_json, &PathBuf::from("/usr/local/bin/ccteam")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        let entry = &v["mcpServers"]["ccteam"];
        assert_eq!(entry["command"], "/usr/local/bin/ccteam");
        assert!(entry["args"].is_array());
        assert!(entry["env"].is_object());
    }

    #[test]
    fn install_mcp_into_preserves_other_top_level_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            r#"{"projects":{"a":1},"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();
        install_mcp_into(&claude_json, &PathBuf::from("/x/ccteam")).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&claude_json).unwrap()).unwrap();
        assert_eq!(v["projects"]["a"], 1);
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["ccteam"]["command"], "/x/ccteam");
    }

    #[test]
    fn claude_mcp_registered_detects_presence_and_absence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        assert!(!claude_mcp_registered(&claude_json), "missing file → false");
        std::fs::write(&claude_json, r#"{"mcpServers":{"other":{}}}"#).unwrap();
        assert!(!claude_mcp_registered(&claude_json), "no ccteam → false");
        install_mcp_into(&claude_json, &PathBuf::from("/x/ccteam")).unwrap();
        assert!(claude_mcp_registered(&claude_json), "after install → true");
    }

    #[test]
    fn install_codex_mcp_into_merges_preserving_other_servers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "model = \"o3\"\n[mcp_servers.other]\ncommand = \"x\"\n",
        )
        .unwrap();
        install_codex_mcp_into(&config_toml, &PathBuf::from("/x/ccteam")).unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(root["model"].as_str(), Some("o3"));
        assert_eq!(root["mcp_servers"]["other"]["command"].as_str(), Some("x"));
        assert_eq!(
            root["mcp_servers"]["ccteam"]["command"].as_str(),
            Some("/x/ccteam")
        );
    }

    #[test]
    fn install_codex_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("nested").join(".codex").join("config.toml");
        install_codex_mcp_into(&config_toml, &PathBuf::from("/x/ccteam")).unwrap();
        assert!(!codex_mcp_registered(
            &config_toml.with_file_name("absent.toml")
        ));
        assert!(codex_mcp_registered(&config_toml), "after install → true");
        // Second install must not error or duplicate.
        install_codex_mcp_into(&config_toml, &PathBuf::from("/y/ccteam")).unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(
            root["mcp_servers"]["ccteam"]["command"].as_str(),
            Some("/y/ccteam"),
            "idempotent re-install replaces only the ccteam command"
        );
    }
}
