//! V0.6.0 F111 — per-project `.mcp.json` template + merge helper.
//!
//! The `ccteam-creator` skill writes a project-local `.mcp.json` so the
//! Claude Code session launched in that project picks up the `ccteam`
//! MCP server without needing the user-global `~/.claude.json` entry
//! that `ccteam doctor --install-mcp` produces. This file is the
//! authoritative template + the merge helper used at project bootstrap
//! time.
//!
//! Semantics (PRD F111 §C):
//! - If `<project>/.mcp.json` does **not** exist → render the template
//!   verbatim with the resolved `ccteam` binary path baked in.
//! - If `<project>/.mcp.json` **already** exists → parse it, merge our
//!   `mcpServers.ccteam` entry, and rewrite it. **Never** clobber other
//!   `mcpServers.*` entries the user already wired up (Playwright,
//!   Linear, etc.).
//!
//! Wave 1 lands this helper alone; Wave 2 wires it into the
//! `ccteam-creator` skill execute phase (per `docs/v0-6-0/prd.md` F111
//! file clean list).

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

/// MCP server entry name we own. Matches `SERVER_NAME` in
/// `crates/ccteam-cli/src/mcp_serve.rs` — V0.5 user muscle memory
/// preserved (README §九 F111 decision).
pub const CCTEAM_MCP_SERVER_KEY: &str = "ccteam";

/// Render a fresh `.mcp.json` body that registers the ccteam server
/// only. Used when a project has no pre-existing `.mcp.json`. The
/// returned string is pretty-printed JSON suitable for `std::fs::write`.
///
/// `ccteam_bin` should be the absolute path to the running `ccteam`
/// binary (use `current_ccteam_bin()` for daemon-side callers).
pub fn render_project_mcp_json(ccteam_bin: &Path) -> Result<String> {
    let mut root = Map::new();
    let mut servers = Map::new();
    servers.insert(CCTEAM_MCP_SERVER_KEY.into(), ccteam_server_entry(ccteam_bin));
    root.insert("mcpServers".into(), Value::Object(servers));
    serde_json::to_string_pretty(&Value::Object(root))
        .context("serialize fresh .mcp.json body")
}

/// Merge our `mcpServers.ccteam` entry into an existing `.mcp.json`
/// body (`existing` may be empty `""` to mean "no file yet"). Returns
/// the pretty-printed merged JSON. Preserves every other top-level key
/// and every other `mcpServers.*` entry unchanged.
///
/// Idempotent: rerunning with the same `ccteam_bin` produces byte-
/// identical output.
pub fn merge_project_mcp_json(existing: &str, ccteam_bin: &Path) -> Result<String> {
    let trimmed = existing.trim();
    let mut root = if trimmed.is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Value>(trimmed)
            .with_context(|| "parse existing .mcp.json (must be a JSON object)")?
        {
            Value::Object(m) => m,
            other => anyhow::bail!(
                "existing .mcp.json root is not a JSON object: {other}"
            ),
        }
    };
    let servers_entry = root
        .entry("mcpServers".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers_entry.is_object() {
        // Defensive: don't silently overwrite a string / array there.
        anyhow::bail!(".mcp.json `mcpServers` exists but is not a JSON object");
    }
    let servers = servers_entry.as_object_mut().expect("checked above");
    servers.insert(
        CCTEAM_MCP_SERVER_KEY.into(),
        ccteam_server_entry(ccteam_bin),
    );
    serde_json::to_string_pretty(&Value::Object(root))
        .context("serialize merged .mcp.json body")
}

/// The single shared description of the ccteam server entry. Kept
/// intentionally tiny — `mcp-serve` reads everything else it needs
/// from `CCTEAM_*` env (set by the parent shell or by Claude Code's
/// settings.json).
fn ccteam_server_entry(ccteam_bin: &Path) -> Value {
    json!({
        "command": ccteam_bin.to_string_lossy(),
        "args": ["mcp-serve"],
        "env": {},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn bin() -> PathBuf {
        PathBuf::from("/usr/local/bin/ccteam")
    }

    #[test]
    fn render_fresh_includes_ccteam_server_entry() {
        let body = render_project_mcp_json(&bin()).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["mcpServers"]["ccteam"]["command"], "/usr/local/bin/ccteam");
        assert_eq!(v["mcpServers"]["ccteam"]["args"][0], "mcp-serve");
    }

    #[test]
    fn merge_into_empty_string_equivalent_to_render() {
        let a = render_project_mcp_json(&bin()).unwrap();
        let b = merge_project_mcp_json("", &bin()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn merge_preserves_other_top_level_keys() {
        let existing = r#"{
            "playwrightConfig": { "headless": true },
            "mcpServers": { "linear": { "command": "linear-mcp" } }
        }"#;
        let merged = merge_project_mcp_json(existing, &bin()).unwrap();
        let v: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["playwrightConfig"]["headless"], true);
        assert_eq!(v["mcpServers"]["linear"]["command"], "linear-mcp");
        assert_eq!(v["mcpServers"]["ccteam"]["command"], "/usr/local/bin/ccteam");
    }

    #[test]
    fn merge_overwrites_only_ccteam_entry() {
        let existing = r#"{
            "mcpServers": {
                "ccteam": { "command": "/old/bin/ccteam", "args": ["mcp-serve"] },
                "playwright": { "command": "npx" }
            }
        }"#;
        let merged = merge_project_mcp_json(existing, &bin()).unwrap();
        let v: Value = serde_json::from_str(&merged).unwrap();
        // Updated.
        assert_eq!(v["mcpServers"]["ccteam"]["command"], "/usr/local/bin/ccteam");
        // Untouched.
        assert_eq!(v["mcpServers"]["playwright"]["command"], "npx");
    }

    #[test]
    fn merge_is_idempotent() {
        let first = merge_project_mcp_json("", &bin()).unwrap();
        let second = merge_project_mcp_json(&first, &bin()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn merge_rejects_non_object_root() {
        let err = merge_project_mcp_json("[1,2,3]", &bin()).unwrap_err();
        assert!(format!("{err:#}").contains("not a JSON object"));
    }

    #[test]
    fn merge_rejects_non_object_mcp_servers() {
        let existing = r#"{ "mcpServers": "oops" }"#;
        let err = merge_project_mcp_json(existing, &bin()).unwrap_err();
        assert!(format!("{err:#}").contains("not a JSON object"));
    }
}
