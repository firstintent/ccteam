//! Per-project `.mcp.json` merge helper for **third-party** MCP servers.
//!
//! A project `.mcp.json` is vendor-native config that Claude Code reads on next
//! start. ccteam writes third-party server entries here on request from the web
//! MCP page — **config write only**: ccteam never fetches, installs, or executes
//! a server.
//!
//! ccteam's OWN server is deliberately NOT written here. It is registered once
//! per vendor in the user-global config (`crate::mcp_register`, HTTP + a
//! machine-user ENROLLMENT credential — that file is shared by every process the
//! vendor starts, so it may name an identity but must grant nothing) and
//! overridden per session by the managed spawn path
//! (`ccteam_harness::execution::mcp_config`, HTTP + that session's own
//! principal). The historical stdio `mcpServers.ccteam` project template was
//! retired with the move to HTTP — the reserved-name guard below is what keeps
//! it from coming back through the third-party door.

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// MCP server entry name we own. Must match the name every wire dialect
/// registers ccteam under (`ccteam_harness::execution::mcp_config`'s
/// `CCTEAM_MCP_SERVER_NAME`) — V0.5 user muscle memory preserved
/// (README §九 F111 decision).
pub const CCTEAM_MCP_SERVER_KEY: &str = "ccteam";

/// v0.8.24 F1.12 — validate a third-party MCP server name for
/// [`merge_named_mcp_server`]: 1–50 chars of `[A-Za-z0-9_-]`, and NOT the
/// reserved `ccteam` key (ccteam's own entry is owned by the global
/// registration in `crate::mcp_register`, never a project file).
pub fn validate_mcp_server_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 50 {
        anyhow::bail!("mcp server name must be 1–50 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!("mcp server name must match [A-Za-z0-9_-]+ (got `{name}`)");
    }
    if name == CCTEAM_MCP_SERVER_KEY {
        anyhow::bail!("`{CCTEAM_MCP_SERVER_KEY}` is reserved for ccteam's own server");
    }
    Ok(())
}

/// v0.8.24 F1.12 — merge an arbitrary NAMED third-party server entry into a
/// project `.mcp.json` body (vendor-native config: Claude Code reads it on
/// next start). MERGE, never clobber: every other top-level key and every other
/// `mcpServers.*` entry is preserved, and re-running with the same entry is
/// byte-idempotent. **Config write only** — ccteam never fetches, installs, or
/// executes the server.
pub fn merge_named_mcp_server(existing: &str, name: &str, entry: Value) -> Result<String> {
    validate_mcp_server_name(name)?;
    if !entry.is_object() {
        anyhow::bail!("mcp server entry must be a JSON object");
    }
    let trimmed = existing.trim();
    let mut root = if trimmed.is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(trimmed)
            .with_context(|| "parse existing .mcp.json (must be a JSON object)")?
        {
            Value::Object(m) => m,
            other => anyhow::bail!("existing .mcp.json root is not a JSON object: {other}"),
        }
    };
    let servers_entry = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers_entry.is_object() {
        anyhow::bail!(".mcp.json `mcpServers` exists but is not a JSON object");
    }
    servers_entry
        .as_object_mut()
        .expect("checked above")
        .insert(name.to_string(), entry);
    serde_json::to_string_pretty(&Value::Object(root)).context("serialize merged .mcp.json body")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn named_merge_adds_third_party_and_preserves_siblings() {
        let base = r#"{"mcpServers": {"linear": {"command": "linear-mcp"}}}"#;
        let merged = merge_named_mcp_server(
            base,
            "context7",
            json!({"type": "http", "url": "https://mcp.context7.com/mcp"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["mcpServers"]["context7"]["type"], "http");
        assert_eq!(v["mcpServers"]["linear"]["command"], "linear-mcp");
        // Idempotent for the same entry.
        let again = merge_named_mcp_server(
            &merged,
            "context7",
            json!({"type": "http", "url": "https://mcp.context7.com/mcp"}),
        )
        .unwrap();
        assert_eq!(merged, again);
    }

    #[test]
    fn named_merge_creates_from_empty_and_preserves_other_top_level_keys() {
        let fresh = merge_named_mcp_server("", "context7", json!({"type": "http"})).unwrap();
        let v: Value = serde_json::from_str(&fresh).unwrap();
        assert_eq!(v["mcpServers"]["context7"]["type"], "http");

        let existing = r#"{"playwrightConfig": {"headless": true}}"#;
        let merged = merge_named_mcp_server(existing, "linear", json!({"command": "x"})).unwrap();
        let v: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["playwrightConfig"]["headless"], true);
        assert_eq!(v["mcpServers"]["linear"]["command"], "x");
    }

    #[test]
    fn named_merge_rejects_reserved_and_bad_names() {
        // `ccteam` stays reserved: its entry is the global HTTP registration
        // (`crate::mcp_register`) + the per-session override — never a project
        // file, and never a stdio child (there is no `mcp-serve` command left
        // to be one).
        assert!(merge_named_mcp_server("", "ccteam", json!({})).is_err());
        assert!(merge_named_mcp_server("", "", json!({"command": "x"})).is_err());
        assert!(merge_named_mcp_server("", "bad name", json!({"command": "x"})).is_err());
        assert!(merge_named_mcp_server("", "ok-name", json!("not-an-object")).is_err());
        assert!(validate_mcp_server_name("playwright").is_ok());
        assert!(validate_mcp_server_name("ccteam").is_err());
    }

    #[test]
    fn named_merge_rejects_malformed_bodies() {
        assert!(merge_named_mcp_server("[1,2,3]", "linear", json!({"command": "x"})).is_err());
        let err =
            merge_named_mcp_server(r#"{ "mcpServers": "oops" }"#, "linear", json!({})).unwrap_err();
        assert!(format!("{err:#}").contains("not a JSON object"));
    }
}
