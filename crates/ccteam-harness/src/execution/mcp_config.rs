//! Curated per-session MCP config for Claude stream-json (v0.8.24 C1 / v0.9-W2).
//!
//! Spawn writes `<project>/.ccteam/chat/<sid>/mcp.json` with **only** the
//! ccteam MCP server (8 tools), then `build_argv` passes
//! `--mcp-config <path>` alongside `--strict-mcp-config` so ambient user
//! MCP servers are not inherited (avoids the historical self-referential
//! init deadlock).
//!
//! Two wire forms:
//! - **HTTP** (preferred): `type:"http"` against daemon `POST /mcp` with
//!   session bearer `ccteam-sid:<sid>:<secret>`.
//! - **stdio** (fallback): `ccteam internal mcp-serve` with secret env.
//!
//! Mode selection: `CCTEAM_MCP_CONFIG_MODE=stdio` forces stdio; otherwise
//! HTTP is used (Claude CLI supports `type:"http"` + headers).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::turns_mirror::chat_dir;

/// Filename under `.ccteam/chat/<sid>/`.
pub const MCP_CONFIG_FILENAME: &str = "mcp.json";

/// Resolve the path for a session's curated mcp config.
pub fn session_mcp_config_path(project_dir: &Path, sid: &str) -> PathBuf {
    chat_dir(project_dir, sid).join(MCP_CONFIG_FILENAME)
}

/// Session-scoped bearer for `POST /mcp` (see `ccteam-web` mcp route).
pub fn session_mcp_bearer(sid: &str, secret: &str) -> String {
    format!("ccteam-sid:{sid}:{secret}")
}

/// Default daemon MCP HTTP URL. Override with `CCTEAM_MCP_HTTP_URL`.
pub fn default_mcp_http_url() -> String {
    if let Ok(u) = std::env::var("CCTEAM_MCP_HTTP_URL") {
        let t = u.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Ok(base) = std::env::var("CCTEAM_WEB_URL") {
        let b = base.trim().trim_end_matches('/');
        if !b.is_empty() {
            return format!("{b}/mcp");
        }
    }
    "http://127.0.0.1:7331/mcp".to_string()
}

/// Which wire form to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpConfigMode {
    /// HTTP streamable MCP against the daemon.
    Http,
    /// stdio `ccteam internal mcp-serve` child.
    Stdio,
}

impl McpConfigMode {
    /// Env-driven selection (`CCTEAM_MCP_CONFIG_MODE=stdio` → Stdio, else Http).
    pub fn from_env() -> Self {
        match std::env::var("CCTEAM_MCP_CONFIG_MODE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "stdio" | "std-io" | "std" => Self::Stdio,
            _ => Self::Http,
        }
    }
}

/// Inputs for a curated session MCP config.
#[derive(Debug, Clone)]
pub struct CuratedMcpInput<'a> {
    pub sid: &'a str,
    pub secret: &'a str,
    pub role: &'a str,
    pub slug: &'a str,
    /// Absolute path to the ccteam binary (stdio mode).
    pub ccteam_bin: &'a Path,
    pub mode: McpConfigMode,
    /// Full MCP HTTP URL (http mode). Empty → [`default_mcp_http_url`].
    pub http_url: Option<&'a str>,
}

/// Build the JSON body for `--mcp-config` (object with `mcpServers`).
pub fn build_curated_mcp_json(input: &CuratedMcpInput<'_>) -> Value {
    let server = match input.mode {
        McpConfigMode::Http => {
            let url = input
                .http_url
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(default_mcp_http_url);
            let bearer = session_mcp_bearer(input.sid, input.secret);
            json!({
                "type": "http",
                "url": url,
                "headers": {
                    "Authorization": format!("Bearer {bearer}"),
                }
            })
        }
        McpConfigMode::Stdio => {
            let bin = input.ccteam_bin.to_string_lossy();
            json!({
                "command": bin,
                "args": ["internal", "mcp-serve"],
                "env": {
                    "CCTEAM_CHAT_SID": input.sid,
                    "CCTEAM_CHAT_SECRET": input.secret,
                    "CCTEAM_CHAT_ROLE": input.role,
                    "CCTEAM_CHAT_SLUG": input.slug,
                }
            })
        }
    };
    json!({
        "mcpServers": {
            "ccteam": server
        }
    })
}

/// Write curated mcp.json under the session chat dir. Returns the path.
pub fn write_session_mcp_config(
    project_dir: &Path,
    input: &CuratedMcpInput<'_>,
) -> Result<PathBuf> {
    let path = session_mcp_config_path(project_dir, input.sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&build_curated_mcp_json(input))?;
    // Atomic-ish: write tmp then rename (same dir).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(path)
}

/// Best-effort ACP `mcpServers` array entry (the ccteam MCP over HTTP),
/// **shared by both ACP vendors** (OpenCode + Grok) on `session/new`,
/// `session/resume`, and `session/load`. Empty vec when sid/secret missing
/// (caller still gets a valid session, just without the in-agent tool face).
///
/// ACP (OpenCode 1.17.x / Grok 0.2.x) validates MCP entries strictly:
/// `headers` must be an **array** of `{name, value}` (not a map). Wrong shape
/// → jsonrpc -32602 Invalid params on `session/new` (smoke 2026-07-11).
pub fn acp_mcp_servers_http(sid: &str, secret: &str) -> Vec<Value> {
    if sid.is_empty() || secret.is_empty() {
        return Vec::new();
    }
    let url = default_mcp_http_url();
    let bearer = session_mcp_bearer(sid, secret);
    vec![json!({
        "name": "ccteam",
        "type": "http",
        "url": url,
        "headers": [
            {
                "name": "Authorization",
                "value": format!("Bearer {bearer}"),
            }
        ]
    })]
}

/// Codex `thread/start` / `thread/resume` `config` override injecting the
/// ccteam MCP server for THIS thread only (verified on codex 0.144.1, W1
/// spike): `{"mcp_servers": {"ccteam": {command, args, env}}}` (snake_case,
/// mirroring `~/.codex/config.toml`). The per-thread entry REPLACES a
/// same-named global entry (spike: one spawn, per-thread env wins), so the
/// thread's ccteam server carries the caller's identity env. stdio form (the
/// spawned `ccteam internal mcp-serve` bridges to the daemon socket + forwards
/// the identity); the daemon-singleton app-server topology is unchanged.
/// `None` when sid/secret is empty (fall through to the daemon's global config,
/// identity-less) — never an empty override that would strip tools.
pub fn codex_thread_mcp_config(
    sid: &str,
    secret: &str,
    role: &str,
    slug: &str,
    ccteam_bin: &Path,
) -> Option<Value> {
    if sid.is_empty() || secret.is_empty() {
        return None;
    }
    let bin = ccteam_bin.to_string_lossy();
    Some(json!({
        "mcp_servers": {
            "ccteam": {
                "command": bin,
                "args": ["internal", "mcp-serve"],
                "env": {
                    "CCTEAM_CHAT_SID": sid,
                    "CCTEAM_CHAT_SECRET": secret,
                    "CCTEAM_CHAT_ROLE": role,
                    "CCTEAM_CHAT_SLUG": slug,
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn http_form_has_type_url_and_session_bearer() {
        let v = build_curated_mcp_json(&CuratedMcpInput {
            sid: "s3",
            secret: "sekret",
            role: "cto",
            slug: "demo",
            ccteam_bin: Path::new("/usr/bin/ccteam"),
            mode: McpConfigMode::Http,
            http_url: Some("http://127.0.0.1:7331/mcp"),
        });
        let srv = &v["mcpServers"]["ccteam"];
        assert_eq!(srv["type"], "http");
        assert_eq!(srv["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(
            srv["headers"]["Authorization"],
            "Bearer ccteam-sid:s3:sekret"
        );
    }

    #[test]
    fn stdio_form_has_command_args_and_secret_env() {
        let v = build_curated_mcp_json(&CuratedMcpInput {
            sid: "s1",
            secret: "abc",
            role: "cto",
            slug: "demo",
            ccteam_bin: Path::new("/opt/ccteam"),
            mode: McpConfigMode::Stdio,
            http_url: None,
        });
        let srv = &v["mcpServers"]["ccteam"];
        assert_eq!(srv["command"], "/opt/ccteam");
        assert_eq!(srv["args"][0], "internal");
        assert_eq!(srv["args"][1], "mcp-serve");
        assert_eq!(srv["env"]["CCTEAM_CHAT_SECRET"], "abc");
        assert_eq!(srv["env"]["CCTEAM_CHAT_SID"], "s1");
        assert_eq!(srv["env"]["CCTEAM_CHAT_ROLE"], "cto");
    }

    #[test]
    fn write_session_mcp_config_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session_mcp_config(
            tmp.path(),
            &CuratedMcpInput {
                sid: "s9",
                secret: "x",
                role: "",
                slug: "p",
                ccteam_bin: &PathBuf::from("/bin/ccteam"),
                mode: McpConfigMode::Http,
                http_url: Some("http://127.0.0.1:9/mcp"),
            },
        )
        .unwrap();
        assert!(path.exists());
        assert!(path.ends_with("mcp.json"));
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(body["mcpServers"]["ccteam"].is_object());
    }

    #[test]
    fn session_bearer_format() {
        assert_eq!(session_mcp_bearer("s2", "tok"), "ccteam-sid:s2:tok");
    }

    #[test]
    fn acp_mcp_headers_are_name_value_array() {
        let v = acp_mcp_servers_http("s3", "sekret");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0]["name"], "ccteam");
        assert_eq!(v[0]["type"], "http");
        assert!(v[0]["headers"].is_array(), "ACP requires headers[]");
        assert_eq!(v[0]["headers"][0]["name"], "Authorization");
        assert_eq!(v[0]["headers"][0]["value"], "Bearer ccteam-sid:s3:sekret");
        assert!(acp_mcp_servers_http("", "x").is_empty());
        assert!(acp_mcp_servers_http("s1", "").is_empty());
    }

    #[test]
    fn codex_thread_config_snake_case_stdio_with_identity_env() {
        let v = codex_thread_mcp_config("s7", "sek", "reviewer", "demo", Path::new("/opt/ccteam"))
            .expect("non-empty secret -> Some config");
        // snake_case `mcp_servers` (codex config.toml schema), stdio form.
        let srv = &v["mcp_servers"]["ccteam"];
        assert_eq!(srv["command"], "/opt/ccteam");
        assert_eq!(srv["args"][0], "internal");
        assert_eq!(srv["args"][1], "mcp-serve");
        assert_eq!(srv["env"]["CCTEAM_CHAT_SID"], "s7");
        assert_eq!(srv["env"]["CCTEAM_CHAT_SECRET"], "sek");
        assert_eq!(srv["env"]["CCTEAM_CHAT_ROLE"], "reviewer");
        assert_eq!(srv["env"]["CCTEAM_CHAT_SLUG"], "demo");
        // Empty secret / sid -> None (no override; daemon global config used).
        assert!(codex_thread_mcp_config("s7", "", "r", "d", Path::new("/x")).is_none());
        assert!(codex_thread_mcp_config("", "sek", "r", "d", Path::new("/x")).is_none());
    }
}
