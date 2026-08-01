//! Curated per-session MCP config for Claude stream-json (v0.8.24 C1 / v0.9-W2).
//!
//! Spawn writes `<project>/.ccteam/chat/<sid>/mcp.json` with **only** the
//! ccteam MCP server, then `build_argv` passes `--mcp-config <path>`. The
//! stream-json path also passes `--strict-mcp-config` so ambient user MCP
//! servers are not inherited (avoids the historical self-referential init
//! deadlock); the terminal path omits it and relies on `--mcp-config` winning
//! the same-name merge (claude's scope order puts `--mcp-config` above user
//! scope), keeping the user's other ambient servers.
//!
//! **One wire form: HTTP.** `type:"http"` against daemon `POST /mcp` with the
//! session bearer `ccteam-sid:<sid>:<secret>`. This is the same transport the
//! global per-vendor registration uses (`ccteam_core::mcp_register`, admin
//! bearer); a managed session's config just overrides the same-named entry
//! with its own principal. Nothing spawns a `ccteam internal mcp-serve` stdio
//! child any more — that binary stays only as a manually-wired escape hatch.

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

/// Last-resort daemon MCP URL, matching `daemon_cli::DEFAULT_WEB_BIND`'s port.
/// Only correct when the daemon runs on the default bind — prefer
/// `ccteam_core::mcp_register::resolve_mcp_http_url`, which also honours the
/// bind the running daemon actually recorded.
pub const FALLBACK_MCP_HTTP_URL: &str = "http://127.0.0.1:7331/mcp";

/// Explicit operator override of the daemon MCP URL, if any:
/// `CCTEAM_MCP_HTTP_URL` (full URL) or `CCTEAM_WEB_URL` (base + `/mcp`).
/// `None` when neither is set — the caller decides what to fall back to.
pub fn mcp_http_url_from_env() -> Option<String> {
    if let Ok(u) = std::env::var("CCTEAM_MCP_HTTP_URL") {
        let t = u.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Ok(base) = std::env::var("CCTEAM_WEB_URL") {
        let b = base.trim().trim_end_matches('/');
        if !b.is_empty() {
            return Some(format!("{b}/mcp"));
        }
    }
    None
}

/// Default daemon MCP HTTP URL: operator override, else the default-bind
/// fallback. Used where no `CcteamPaths` is in scope (session spawn, whose
/// config the daemon itself writes).
pub fn default_mcp_http_url() -> String {
    mcp_http_url_from_env().unwrap_or_else(|| FALLBACK_MCP_HTTP_URL.to_string())
}

/// Inputs for a curated session MCP config.
#[derive(Debug, Clone)]
pub struct CuratedMcpInput<'a> {
    pub sid: &'a str,
    pub secret: &'a str,
    /// Full MCP HTTP URL. `None` / empty → [`default_mcp_http_url`].
    pub http_url: Option<&'a str>,
}

/// Build the JSON body for `--mcp-config` (object with `mcpServers`).
pub fn build_curated_mcp_json(input: &CuratedMcpInput<'_>) -> Value {
    let url = input
        .http_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(default_mcp_http_url);
    let bearer = session_mcp_bearer(input.sid, input.secret);
    json!({
        "mcpServers": {
            "ccteam": {
                "type": "http",
                "url": url,
                "headers": {
                    "Authorization": format!("Bearer {bearer}"),
                }
            }
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
/// ccteam MCP server for THIS thread only. Codex 0.144.3 real-machine smoke
/// verifies this exact snake_case shape:
/// `{"mcp_servers":{"ccteam":{"url", "http_headers"}}}`.
///
/// The global Codex entry must also be HTTP. Codex deep-merges a per-thread
/// server override with the same-named global entry; a legacy global stdio
/// `command` plus this `url` is rejected as a mixed transport. With both sides
/// HTTP, the per-thread Authorization value replaces the global admin bearer
/// and carries this session's `(sid, secret)` principal directly to `/mcp`.
/// No per-thread `ccteam internal mcp-serve` child is spawned.
///
/// `None` when sid/secret is empty (fall through to the daemon's global HTTP
/// config, authenticated as admin) — never an empty override that strips tools.
pub fn codex_thread_mcp_config(sid: &str, secret: &str) -> Option<Value> {
    let url = default_mcp_http_url();
    codex_thread_mcp_config_at(sid, secret, &url)
}

/// Explicit-URL seam for deterministic integration tests and non-default
/// daemon binds. Production callers normally use [`codex_thread_mcp_config`].
pub fn codex_thread_mcp_config_at(sid: &str, secret: &str, http_url: &str) -> Option<Value> {
    if sid.is_empty() || secret.is_empty() {
        return None;
    }
    let url = http_url.trim();
    if url.is_empty() {
        return None;
    }
    let bearer = session_mcp_bearer(sid, secret);
    Some(json!({
        "mcp_servers": {
            "ccteam": {
                "url": url,
                "http_headers": {
                    "Authorization": format!("Bearer {bearer}"),
                },
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_form_has_type_url_and_session_bearer() {
        let v = build_curated_mcp_json(&CuratedMcpInput {
            sid: "s3",
            secret: "sekret",
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
    fn curated_config_never_emits_a_stdio_child() {
        // One wire form. A stdio `command`/`args`/`env` entry would re-open the
        // self-referential init path the HTTP move closed.
        let v = build_curated_mcp_json(&CuratedMcpInput {
            sid: "s1",
            secret: "abc",
            http_url: None,
        });
        let srv = &v["mcpServers"]["ccteam"];
        assert_eq!(srv["type"], "http");
        assert!(srv.get("command").is_none());
        assert!(srv.get("args").is_none());
        assert!(srv.get("env").is_none());
        // Empty/absent url falls back to the daemon default, never to stdio.
        assert!(srv["url"].as_str().is_some_and(|u| u.ends_with("/mcp")));
    }

    #[test]
    fn write_session_mcp_config_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session_mcp_config(
            tmp.path(),
            &CuratedMcpInput {
                sid: "s9",
                secret: "x",
                http_url: Some("http://127.0.0.1:9/mcp"),
            },
        )
        .unwrap();
        assert!(path.exists());
        assert!(path.ends_with("mcp.json"));
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(body["mcpServers"]["ccteam"]["type"], "http");
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
    fn codex_thread_config_snake_case_http_with_session_bearer() {
        let v = codex_thread_mcp_config_at("s7", "sek", "http://127.0.0.1:7331/mcp")
            .expect("non-empty principal -> Some config");
        // Codex config.toml schema: snake_case `mcp_servers`, HTTP fields are
        // `url` + `http_headers` (not Claude's `type` + `headers`).
        let srv = &v["mcp_servers"]["ccteam"];
        assert_eq!(srv["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(
            srv["http_headers"]["Authorization"],
            "Bearer ccteam-sid:s7:sek"
        );
        assert!(srv.get("command").is_none());
        assert!(srv.get("args").is_none());
        assert!(srv.get("env").is_none());
        // Empty secret / sid / URL -> None (global HTTP config remains).
        assert!(codex_thread_mcp_config_at("s7", "", "http://localhost/mcp").is_none());
        assert!(codex_thread_mcp_config_at("", "sek", "http://localhost/mcp").is_none());
        assert!(codex_thread_mcp_config_at("s7", "sek", " ").is_none());
    }
}
