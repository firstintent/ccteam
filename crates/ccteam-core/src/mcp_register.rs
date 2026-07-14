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

/// Register the ccteam MCP server in Codex's `config.toml` as a
/// `[mcp_servers.ccteam]` Streamable HTTP table.
///
/// MERGE, never clobber: every other top-level key and every other
/// `[mcp_servers.*]` entry is preserved; only `mcp_servers.ccteam` is
/// set/replaced. The admin token is the bare value stored in
/// `~/.ccteam/secrets/web-token`; this writer adds the `ccteam:` wire prefix
/// and `Bearer` scheme. Parent dir + file are created if absent. Idempotent.
///
/// The global entry intentionally uses the same HTTP transport as Codex's
/// per-thread override. Codex 0.144.3 deep-merges those tables, so retaining a
/// legacy global `command` while a thread adds `url` creates an invalid mixed
/// transport and rejects `thread/start`.
pub fn install_codex_mcp_into(
    config_toml: &Path,
    mcp_http_url: &str,
    admin_token: &str,
) -> Result<()> {
    let url = mcp_http_url.trim();
    if url.is_empty() {
        anyhow::bail!("ccteam MCP HTTP URL must not be empty");
    }
    let token = admin_token.trim();
    if token.is_empty() {
        anyhow::bail!("ccteam admin web token must not be empty");
    }

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

    // Replace the whole ccteam table so a prior stdio command/args/env cannot
    // survive and combine with `url` under Codex's deep config merge.
    let mut entry = toml::Table::new();
    entry.insert("url".to_string(), toml::Value::String(url.to_string()));
    let mut headers = toml::Table::new();
    headers.insert(
        "Authorization".to_string(),
        toml::Value::String(format!("Bearer ccteam:{token}")),
    );
    entry.insert("http_headers".to_string(), toml::Value::Table(headers));
    servers.insert(
        crate::CCTEAM_MCP_SERVER_KEY.to_string(),
        toml::Value::Table(entry),
    );

    let body = toml::to_string_pretty(&root).context("serialize codex config.toml")?;
    atomic_write(config_toml, body.as_bytes())?;
    // This table now carries the admin bearer. Keep Codex's config private
    // even when the caller's umask would otherwise create a world-readable
    // replacement during the atomic rewrite.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config_toml, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", config_toml.display()))?;
    }
    Ok(())
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

/// Whether Codex's `config.toml` carries the current HTTP form of
/// `[mcp_servers.ccteam]`. A legacy stdio entry deliberately reads as `false`
/// so doctor/host readiness asks the operator to rerun `ccteam config mcp` and
/// removes the `mcp-serve` child instead of declaring the old shape ready.
/// Best-effort: a missing / unreadable / junk file reads as `false`.
pub fn codex_mcp_registered(config_toml: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(config_toml) else {
        return false;
    };
    let Ok(root) = toml::from_str::<toml::Table>(&body) else {
        return false;
    };
    let Some(entry) = root
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(crate::CCTEAM_MCP_SERVER_KEY))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(toml::Value::as_str)
        .is_some_and(|url| !url.trim().is_empty())
        && entry
            .get("http_headers")
            .and_then(toml::Value::as_table)
            .and_then(|headers| headers.get("Authorization"))
            .and_then(toml::Value::as_str)
            .is_some_and(|value| value.starts_with("Bearer ccteam:"))
        && !entry.contains_key("command")
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
        }
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
        install_codex_mcp_into(&config_toml, "http://127.0.0.1:7331/mcp", "admin-secret").unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(root["model"].as_str(), Some("o3"));
        assert_eq!(root["mcp_servers"]["other"]["command"].as_str(), Some("x"));
        assert_eq!(
            root["mcp_servers"]["ccteam"]["url"].as_str(),
            Some("http://127.0.0.1:7331/mcp")
        );
        assert_eq!(
            root["mcp_servers"]["ccteam"]["http_headers"]["Authorization"].as_str(),
            Some("Bearer ccteam:admin-secret")
        );
        assert!(root["mcp_servers"]["ccteam"].get("command").is_none());
    }

    #[test]
    fn install_codex_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("nested").join(".codex").join("config.toml");
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", "token-a").unwrap();
        assert!(!codex_mcp_registered(
            &config_toml.with_file_name("absent.toml")
        ));
        assert!(codex_mcp_registered(&config_toml), "after install → true");
        // Second install must not error or duplicate.
        install_codex_mcp_into(&config_toml, "http://localhost:7444/mcp", "token-b").unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(
            root["mcp_servers"]["ccteam"]["url"].as_str(),
            Some("http://localhost:7444/mcp"),
            "idempotent re-install replaces only the ccteam HTTP entry"
        );
        assert_eq!(
            root["mcp_servers"]["ccteam"]["http_headers"]["Authorization"].as_str(),
            Some("Bearer ccteam:token-b")
        );
    }

    #[test]
    fn codex_mcp_registered_rejects_legacy_stdio_shape() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[mcp_servers.ccteam]\ncommand = \"ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\n",
        )
        .unwrap();
        assert!(!codex_mcp_registered(&config_toml));
        install_codex_mcp_into(&config_toml, "http://localhost/mcp", "token").unwrap();
        assert!(codex_mcp_registered(&config_toml));
    }

    #[test]
    fn install_codex_mcp_into_replaces_legacy_stdio_entry_atomically() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[mcp_servers.ccteam]\ncommand = \"/old/ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\nenv = { CCTEAM_CHAT_SID = \"stale\" }\n",
        )
        .unwrap();

        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", "fresh").unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        let entry = root["mcp_servers"]["ccteam"].as_table().unwrap();
        assert_eq!(
            entry.get("url").and_then(toml::Value::as_str),
            Some("http://localhost:7331/mcp")
        );
        assert!(!entry.contains_key("command"));
        assert!(!entry.contains_key("args"));
        assert!(!entry.contains_key("env"));
    }

    #[test]
    fn install_codex_mcp_into_rejects_missing_http_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        assert!(install_codex_mcp_into(&config_toml, "", "token").is_err());
        assert!(install_codex_mcp_into(&config_toml, "http://localhost/mcp", " ").is_err());
        assert!(!config_toml.exists());
    }

    #[cfg(unix)]
    #[test]
    fn install_codex_mcp_into_makes_token_bearing_config_private() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        install_codex_mcp_into(&config_toml, "http://localhost/mcp", "secret").unwrap();
        assert_eq!(
            std::fs::metadata(&config_toml)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
