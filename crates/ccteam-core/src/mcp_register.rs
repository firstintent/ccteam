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

/// Register the ccteam MCP server in Grok CLI's `~/.grok/config.toml` as a
/// `[mcp_servers.ccteam]` HTTP entry — the same table `grok mcp add
/// --transport http` writes (`url` + `enabled` + `headers`; Grok's key is
/// `headers`, Codex's is `http_headers`).
///
/// MERGE, never clobber + idempotent, mirroring [`install_codex_mcp_into`].
/// The global entry carries the admin bearer, so any plain `grok` main
/// session can orchestrate (v0.9.3 vendor symmetry). ccteam-managed grok
/// sessions ALSO receive a same-named ACP-injected server with their
/// per-session principal; real-machine probe (grok 0.2.x, 2026-07-15): grok
/// connects both, dedups same-named tools, and `tools/call` lands on the
/// ACP-injected (session) server — so the delegation parent edge survives the
/// global entry.
pub fn install_grok_mcp_into(
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

    let mut entry = toml::Table::new();
    entry.insert("url".to_string(), toml::Value::String(url.to_string()));
    entry.insert("enabled".to_string(), toml::Value::Boolean(true));
    let mut headers = toml::Table::new();
    headers.insert(
        "Authorization".to_string(),
        toml::Value::String(format!("Bearer ccteam:{token}")),
    );
    entry.insert("headers".to_string(), toml::Value::Table(headers));
    servers.insert(
        crate::CCTEAM_MCP_SERVER_KEY.to_string(),
        toml::Value::Table(entry),
    );

    let body = toml::to_string_pretty(&root).context("serialize grok config.toml")?;
    atomic_write(config_toml, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(config_toml, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", config_toml.display()))?;
    }
    Ok(())
}

/// Register the ccteam MCP server in OpenCode's global `opencode.json` under
/// the runtime `mcp.<name>` shape (OpenCode 1.18.x CLI loader:
/// `packages/opencode/src/config/config.ts` deep-merges raw JSON and the MCP
/// service consumes `mcp.<name>` records — the `mcp.servers` shape belongs to
/// the separate core-v2 path, not the shipped CLI).
///
/// MERGE, never clobber + idempotent: only `mcp.ccteam` is set; every other
/// key survives. Admin bearer inside → 0600. ccteam-managed OpenCode sessions
/// override this by name at runtime (`MCP.add` replaces the same-named config
/// entry with the ACP-injected per-session principal).
pub fn install_opencode_mcp_into(
    opencode_json: &Path,
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

    let mut root = if opencode_json.exists() {
        let bytes = std::fs::read(opencode_json)
            .with_context(|| format!("read {}", opencode_json.display()))?;
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

    let mcp = root
        .entry("mcp")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let map = match mcp {
        Value::Object(m) => m,
        _ => {
            *mcp = Value::Object(serde_json::Map::new());
            mcp.as_object_mut().unwrap()
        }
    };
    map.insert(
        crate::CCTEAM_MCP_SERVER_KEY.into(),
        json!({
            "type": "remote",
            "url": url,
            "enabled": true,
            "headers": { "Authorization": format!("Bearer ccteam:{token}") },
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    atomic_write(opencode_json, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(opencode_json, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", opencode_json.display()))?;
    }
    Ok(())
}

/// Register the ccteam MCP server in Kimi Code CLI's global
/// `$KIMI_CODE_HOME/mcp.json` (default `~/.kimi-code/mcp.json`) as an
/// `mcpServers.ccteam` HTTP entry — the shape `docs/en/customization/mcp.md`
/// documents (`url` + `headers`; an entry with `url` and no `transport` is
/// HTTP). NOTE: the file schema's `headers` is a **map** — do NOT copy the
/// ACP `mcpServers` parameter's name/value array shape here.
///
/// MERGE, never clobber + idempotent: only `mcpServers.ccteam` is set; every
/// other key survives. Admin bearer inside → 0600. ccteam-managed kimi
/// sessions ALSO receive a same-named ACP-injected server with their
/// per-session principal (same dedup-by-name posture as grok/opencode).
pub fn install_kimi_mcp_into(mcp_json: &Path, mcp_http_url: &str, admin_token: &str) -> Result<()> {
    let url = mcp_http_url.trim();
    if url.is_empty() {
        anyhow::bail!("ccteam MCP HTTP URL must not be empty");
    }
    let token = admin_token.trim();
    if token.is_empty() {
        anyhow::bail!("ccteam admin web token must not be empty");
    }

    let mut root = if mcp_json.exists() {
        let bytes =
            std::fs::read(mcp_json).with_context(|| format!("read {}", mcp_json.display()))?;
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
            "url": url,
            "headers": { "Authorization": format!("Bearer ccteam:{token}") },
        }),
    );

    let body = serde_json::to_string_pretty(&Value::Object(root))?;
    atomic_write(mcp_json, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(mcp_json, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 0600 {}", mcp_json.display()))?;
    }
    Ok(())
}

/// Whether Kimi's `mcp.json` carries the ccteam HTTP MCP entry. Best-effort;
/// missing/junk file reads as `false`.
pub fn kimi_mcp_registered(mcp_json: &Path) -> bool {
    let Ok(bytes) = std::fs::read(mcp_json) else {
        return false;
    };
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(entry) = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|m| m.get(crate::CCTEAM_MCP_SERVER_KEY))
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.trim().is_empty())
        && entry
            .pointer("/headers/Authorization")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("Bearer ccteam:"))
}

/// Resolve Kimi Code CLI's user MCP config: `$KIMI_CODE_HOME/mcp.json`
/// (KIMI_CODE_HOME → `~/.kimi-code` fallback), mirroring kimi's own session
/// dir resolution.
pub fn resolve_kimi_config_path() -> Result<PathBuf> {
    let kimi_home = std::env::var_os("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".kimi-code")))
        .ok_or_else(|| anyhow!("cannot resolve KIMI_CODE_HOME or ~/.kimi-code (no home dir)"))?;
    Ok(kimi_home.join("mcp.json"))
}

/// Whether Grok's `config.toml` carries the current HTTP form of
/// `[mcp_servers.ccteam]`. Best-effort; missing/junk file reads as `false`.
pub fn grok_mcp_registered(config_toml: &Path) -> bool {
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
            .get("headers")
            .and_then(toml::Value::as_table)
            .and_then(|headers| headers.get("Authorization"))
            .and_then(toml::Value::as_str)
            .is_some_and(|value| value.starts_with("Bearer ccteam:"))
}

/// Whether OpenCode's global `opencode.json` carries the ccteam remote MCP
/// entry. Best-effort; missing/junk file reads as `false`.
pub fn opencode_mcp_registered(opencode_json: &Path) -> bool {
    let Ok(bytes) = std::fs::read(opencode_json) else {
        return false;
    };
    let Ok(Value::Object(root)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some(entry) = root
        .get("mcp")
        .and_then(Value::as_object)
        .and_then(|m| m.get(crate::CCTEAM_MCP_SERVER_KEY))
    else {
        return false;
    };
    entry
        .get("url")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.trim().is_empty())
        && entry
            .pointer("/headers/Authorization")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("Bearer ccteam:"))
}

/// Resolve Grok CLI's user config: `~/.grok/config.toml`.
pub fn resolve_grok_config_path() -> Result<PathBuf> {
    dirs::home_dir()
        .map(|h| h.join(".grok").join("config.toml"))
        .ok_or_else(|| anyhow!("cannot resolve ~/.grok (no home dir)"))
}

/// Resolve OpenCode's global config: `$XDG_CONFIG_HOME/opencode/opencode.json`
/// (default `~/.config/opencode/opencode.json`) — the middle file of the
/// loader's `config.json` → `opencode.json` → `opencode.jsonc` merge chain.
pub fn resolve_opencode_config_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .ok_or_else(|| anyhow!("cannot resolve XDG_CONFIG_HOME or ~/.config (no home dir)"))?;
    Ok(base.join("opencode").join("opencode.json"))
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

/// Whether Codex's `config.toml` carries a `[mcp_servers.ccteam]` entry that is
/// NOT the current HTTP form — i.e. a legacy stdio `command` entry left by a
/// pre-HTTP ccteam. Such an entry poisons the per-thread HTTP override the
/// daemon injects on `thread/start`: Codex deep-merges the same-named global and
/// per-thread tables, and a surviving `command` classifies the merged table as
/// stdio, so the override's `url` is rejected (`url is not supported for stdio`)
/// and `thread/start` fails. The daemon migrates such an entry to HTTP on
/// startup ([`install_codex_mcp_into`]).
///
/// Returns `false` when the file is missing / unreadable / unparseable, when
/// there is no `ccteam` entry at all (a per-thread override then stands alone as
/// valid `streamable_http`), or when the entry is already the HTTP form — so a
/// healthy or absent config is left untouched.
pub fn codex_mcp_entry_stale(config_toml: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(config_toml) else {
        return false;
    };
    let Ok(root) = toml::from_str::<toml::Table>(&body) else {
        return false;
    };
    let has_ccteam_entry = root
        .get("mcp_servers")
        .and_then(toml::Value::as_table)
        .map(|t| t.contains_key(crate::CCTEAM_MCP_SERVER_KEY))
        .unwrap_or(false);
    has_ccteam_entry && !codex_mcp_registered(config_toml)
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
    fn codex_mcp_entry_stale_only_flags_legacy_stdio_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Missing file → not stale (nothing to heal).
        let missing = tmp.path().join("absent.toml");
        assert!(!codex_mcp_entry_stale(&missing));

        // No ccteam entry → not stale (a per-thread override stands alone).
        let none = tmp.path().join("none.toml");
        std::fs::write(&none, "[mcp_servers.other]\ncommand = \"x\"\n").unwrap();
        assert!(!codex_mcp_entry_stale(&none));

        // Legacy stdio ccteam entry → STALE (needs migration to HTTP).
        let legacy = tmp.path().join("legacy.toml");
        std::fs::write(
            &legacy,
            "[mcp_servers.ccteam]\ncommand = \"ccteam\"\nargs = [\"internal\", \"mcp-serve\"]\n",
        )
        .unwrap();
        assert!(codex_mcp_entry_stale(&legacy));

        // Current HTTP form → not stale.
        install_codex_mcp_into(&legacy, "http://localhost:7331/mcp", "tok").unwrap();
        assert!(!codex_mcp_entry_stale(&legacy));
    }

    #[test]
    fn install_codex_mcp_into_rejects_missing_http_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        assert!(install_codex_mcp_into(&config_toml, "", "token").is_err());
        assert!(install_codex_mcp_into(&config_toml, "http://localhost/mcp", " ").is_err());
        assert!(!config_toml.exists());
    }

    #[test]
    fn install_grok_mcp_into_writes_grok_shape_and_merges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[cli]\nuse_leader = true\n[mcp_servers.other]\nurl = \"http://x/mcp\"\n",
        )
        .unwrap();
        install_grok_mcp_into(&config_toml, "http://127.0.0.1:7331/mcp", "tok").unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(root["cli"]["use_leader"].as_bool(), Some(true));
        assert_eq!(
            root["mcp_servers"]["other"]["url"].as_str(),
            Some("http://x/mcp")
        );
        let entry = root["mcp_servers"]["ccteam"].as_table().unwrap();
        assert_eq!(entry["url"].as_str(), Some("http://127.0.0.1:7331/mcp"));
        assert_eq!(entry["enabled"].as_bool(), Some(true));
        // Grok's key is `headers` (Codex's is `http_headers`).
        assert_eq!(
            entry["headers"]["Authorization"].as_str(),
            Some("Bearer ccteam:tok")
        );
        assert!(!entry.contains_key("http_headers"));
        assert!(grok_mcp_registered(&config_toml));
        assert!(!grok_mcp_registered(&tmp.path().join("absent.toml")));
    }

    #[test]
    fn install_grok_mcp_into_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join(".grok").join("config.toml");
        install_grok_mcp_into(&config_toml, "http://localhost:7331/mcp", "a").unwrap();
        install_grok_mcp_into(&config_toml, "http://localhost:7444/mcp", "b").unwrap();
        let root: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(
            root["mcp_servers"]["ccteam"]["url"].as_str(),
            Some("http://localhost:7444/mcp")
        );
        assert_eq!(
            root["mcp_servers"]["ccteam"]["headers"]["Authorization"].as_str(),
            Some("Bearer ccteam:b")
        );
    }

    #[test]
    fn install_opencode_mcp_into_writes_runtime_v1_shape_and_merges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let opencode_json = tmp.path().join("opencode.json");
        std::fs::write(
            &opencode_json,
            r#"{"$schema":"https://opencode.ai/config.json","model":"x/y","mcp":{"other":{"type":"local","command":["x"]}}}"#,
        )
        .unwrap();
        install_opencode_mcp_into(&opencode_json, "http://127.0.0.1:7331/mcp", "tok").unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&opencode_json).unwrap()).unwrap();
        assert_eq!(v["model"], "x/y");
        assert_eq!(v["mcp"]["other"]["type"], "local");
        // Runtime v1 shape: `mcp.<name>` (NOT the core-v2 `mcp.servers.<name>`).
        let entry = &v["mcp"]["ccteam"];
        assert_eq!(entry["type"], "remote");
        assert_eq!(entry["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(entry["enabled"], true);
        assert_eq!(entry["headers"]["Authorization"], "Bearer ccteam:tok");
        assert!(v["mcp"].get("servers").is_none());
        assert!(opencode_mcp_registered(&opencode_json));
        assert!(!opencode_mcp_registered(&tmp.path().join("absent.json")));
    }

    #[test]
    fn install_opencode_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let opencode_json = tmp
            .path()
            .join("nested")
            .join("opencode")
            .join("opencode.json");
        install_opencode_mcp_into(&opencode_json, "http://localhost:7331/mcp", "a").unwrap();
        install_opencode_mcp_into(&opencode_json, "http://localhost:7444/mcp", "b").unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&opencode_json).unwrap()).unwrap();
        assert_eq!(v["mcp"]["ccteam"]["url"], "http://localhost:7444/mcp");
        assert_eq!(
            v["mcp"]["ccteam"]["headers"]["Authorization"],
            "Bearer ccteam:b"
        );
    }

    #[cfg(unix)]
    #[test]
    fn grok_and_opencode_installs_make_token_bearing_configs_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let grok = tmp.path().join("config.toml");
        install_grok_mcp_into(&grok, "http://localhost/mcp", "s").unwrap();
        assert_eq!(
            std::fs::metadata(&grok).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let oc = tmp.path().join("opencode.json");
        install_opencode_mcp_into(&oc, "http://localhost/mcp", "s").unwrap();
        assert_eq!(
            std::fs::metadata(&oc).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn grok_and_opencode_installs_reject_missing_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(install_grok_mcp_into(&tmp.path().join("g.toml"), "", "t").is_err());
        assert!(install_grok_mcp_into(&tmp.path().join("g.toml"), "http://x/mcp", " ").is_err());
        assert!(install_opencode_mcp_into(&tmp.path().join("o.json"), "", "t").is_err());
        assert!(
            install_opencode_mcp_into(&tmp.path().join("o.json"), "http://x/mcp", " ").is_err()
        );
    }

    #[test]
    fn install_kimi_mcp_into_writes_http_shape_and_merges() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcp_json = tmp.path().join("mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"filesystem":{"command":"npx","args":["-y","fs"]}}}"#,
        )
        .unwrap();
        install_kimi_mcp_into(&mcp_json, "http://127.0.0.1:7331/mcp", "tok").unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&mcp_json).unwrap()).unwrap();
        // Sibling server survives (merge, never clobber).
        assert_eq!(v["mcpServers"]["filesystem"]["command"], "npx");
        let entry = &v["mcpServers"]["ccteam"];
        assert_eq!(entry["url"], "http://127.0.0.1:7331/mcp");
        // File-schema headers are a MAP (never the ACP name/value array).
        assert_eq!(entry["headers"]["Authorization"], "Bearer ccteam:tok");
        assert!(entry.get("command").is_none());
        assert!(kimi_mcp_registered(&mcp_json));
        assert!(!kimi_mcp_registered(&tmp.path().join("absent.json")));
    }

    #[test]
    fn install_kimi_mcp_into_is_idempotent_and_creates_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mcp_json = tmp.path().join("nested").join("kimi").join("mcp.json");
        install_kimi_mcp_into(&mcp_json, "http://localhost:7331/mcp", "a").unwrap();
        install_kimi_mcp_into(&mcp_json, "http://localhost:7444/mcp", "b").unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&mcp_json).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["ccteam"]["url"],
            "http://localhost:7444/mcp"
        );
        assert_eq!(
            v["mcpServers"]["ccteam"]["headers"]["Authorization"],
            "Bearer ccteam:b"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kimi_install_makes_token_bearing_config_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let mcp_json = tmp.path().join("mcp.json");
        install_kimi_mcp_into(&mcp_json, "http://localhost/mcp", "s").unwrap();
        assert_eq!(
            std::fs::metadata(&mcp_json).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn kimi_install_rejects_missing_credentials() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(install_kimi_mcp_into(&tmp.path().join("k.json"), "", "t").is_err());
        assert!(install_kimi_mcp_into(&tmp.path().join("k.json"), "http://x/mcp", " ").is_err());
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
