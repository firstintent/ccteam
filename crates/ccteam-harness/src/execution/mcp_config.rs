//! The ccteam MCP endpoint a managed session talks to: **one semantics, four
//! vendor dialects**.
//!
//! Every managed session needs exactly two facts to reach the daemon's
//! `POST /mcp`: the **URL** and its own **principal** bearer
//! (`ccteam-sid:<sid>:<secret>`). Those two facts are
//! [`SessionMcpEndpoint`], resolved **once** by [`SessionMcpEndpoint::resolve`]
//! — env override → the bind the running daemon recorded → default-bind
//! fallback. Everything downstream is a **pure projection** into the vendor's
//! own dialect:
//!
//! | vendor | `tool_surface` | projection |
//! |---|---|---|
//! | Claude | `NativeMcpConfig` | [`project_claude_mcp_json`] → `.ccteam/chat/<sid>/mcp.json` + `--mcp-config` |
//! | Codex | `NativeMcpConfig` | [`project_codex_thread_config`] → `thread/start` `config.mcp_servers` |
//! | Grok / OpenCode / Kimi | `NativeMcpConfig` | [`project_acp_mcp_servers`] → ACP `session/new` `mcpServers[]` |
//! | Pi | `ManagedSessionBridge` | [`project_bridge_child_env`] → child env the ccteam bridge extension reads |
//!
//! **A projection never decides whether there is an endpoint.** It takes an
//! already-resolved [`SessionMcpEndpoint`], so "did this session get a tool
//! face?" is answered in exactly one place (the principal: empty sid/secret →
//! `None`) instead of once per adapter. Pi's child env is a dialect like any
//! other — it is NOT "whatever the daemon happened to inherit"; a bridge
//! vendor that inherits its endpoint from the parent process breaks on a
//! healthy default daemon, because `ccteam start` exports nothing.
//!
//! **One wire form: HTTP.** `type:"http"` against daemon `POST /mcp`. This is
//! the same transport the global per-vendor registration uses
//! (`ccteam_core::mcp_register`, admin bearer); a managed session's config just
//! overrides the same-named entry with its own principal. There is no stdio
//! transport at all any more — the `internal mcp-serve` command that hosted it
//! is deleted, so nothing can fall back to a per-session child process.
//!
//! Claude's spawn also passes `--strict-mcp-config` on the stream-json path so
//! ambient user MCP servers are not inherited (avoids the historical
//! self-referential init deadlock); the terminal path omits it and relies on
//! `--mcp-config` winning the same-name merge, keeping the user's other
//! ambient servers.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::fs_atomic::atomic_write_durable;
use super::turns_mirror::chat_dir;

/// Filename under `.ccteam/chat/<sid>/`.
pub const MCP_CONFIG_FILENAME: &str = "mcp.json";

/// Filename under `~/.ccteam/run/` where a starting daemon records the MCP URL
/// its own web bind resolves to.
pub const DAEMON_MCP_URL_FILE: &str = "mcp-url";

/// Last-resort daemon MCP URL, matching `daemon_cli::DEFAULT_WEB_BIND`'s port.
/// Only correct when the daemon runs on the default bind — every production
/// path goes through [`resolve_mcp_http_url`], which also honours the bind the
/// running daemon actually recorded.
pub const FALLBACK_MCP_HTTP_URL: &str = "http://127.0.0.1:7331/mcp";

/// Child-env keys of the `ManagedSessionBridge` dialect (see
/// [`project_bridge_child_env`]). The ccteam bridge extension hard-fails on
/// load when either is missing, so both are mandatory, never best-effort.
pub const BRIDGE_MCP_URL_ENV: &str = "CCTEAM_MCP_HTTP_URL";
pub const BRIDGE_MCP_BEARER_ENV: &str = "CCTEAM_MCP_BEARER";

/// Resolve the path for a session's curated mcp config.
pub fn session_mcp_config_path(project_dir: &Path, sid: &str) -> PathBuf {
    chat_dir(project_dir, sid).join(MCP_CONFIG_FILENAME)
}

/// Session-scoped bearer for `POST /mcp` (see `ccteam-web` mcp route).
pub fn session_mcp_bearer(sid: &str, secret: &str) -> String {
    format!("ccteam-sid:{sid}:{secret}")
}

// =====================================================================
// Resolution — ONE chain, shared by managed spawn and global registration
// =====================================================================

/// Explicit operator override of the daemon MCP URL, if any:
/// `CCTEAM_MCP_HTTP_URL` (full URL) or `CCTEAM_WEB_URL` (base + `/mcp`).
fn mcp_http_url_from_env() -> Option<String> {
    if let Ok(u) = std::env::var(BRIDGE_MCP_URL_ENV) {
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

/// Record the MCP URL implied by the daemon's actual web bind, so every reader
/// — managed session spawn, out-of-band registration (`ccteam config mcp`,
/// doctor), the hosts page — targets the port the daemon really listens on
/// instead of guessing the default.
///
/// Called at daemon start, before auto-registration. Best-effort by contract:
/// the caller logs and continues, since a stale/missing record only costs the
/// default guess. A non-parsing bind is `Ok(())` with nothing written — the
/// startup path reports that error properly a few lines later.
pub fn record_daemon_mcp_url(run_dir: &Path, web_bind: &str) -> Result<()> {
    let Some(url) = mcp_url_from_bind(web_bind) else {
        return Ok(());
    };
    std::fs::create_dir_all(run_dir).with_context(|| format!("create {}", run_dir.display()))?;
    atomic_write_durable(&run_dir.join(DAEMON_MCP_URL_FILE), url.as_bytes())
}

/// Resolve the daemon MCP URL: explicit operator override
/// (`CCTEAM_MCP_HTTP_URL` / `CCTEAM_WEB_URL`) → the running daemon's recorded
/// bind → the default-bind fallback.
///
/// This is what makes both an HTTP registration and a managed session correct
/// on a non-default `--web-bind`. The old stdio entry was port-agnostic (the
/// child dialled the unix socket), so without this the move to HTTP would break
/// exactly the users who bind somewhere else.
pub fn resolve_mcp_http_url(run_dir: &Path) -> String {
    if let Some(url) = mcp_http_url_from_env() {
        return url;
    }
    if let Ok(body) = std::fs::read_to_string(run_dir.join(DAEMON_MCP_URL_FILE)) {
        let trimmed = body.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    FALLBACK_MCP_HTTP_URL.to_string()
}

/// `<ccteam root>/run` — where the daemon records its bind. `None` only when
/// neither `CCTEAM_HOME` nor a home directory can be resolved.
pub fn daemon_run_dir() -> Option<PathBuf> {
    crate::adapter::ccteam_root_from_env().map(|root| root.join("run"))
}

/// [`resolve_mcp_http_url`] against the ambient ccteam root. Used where no
/// explicit run dir is in scope — i.e. session spawn, which the daemon itself
/// performs. Prefer the `run_dir`-taking form when the caller already holds
/// `CcteamPaths`.
pub fn daemon_mcp_http_url() -> String {
    match daemon_run_dir() {
        Some(run_dir) => resolve_mcp_http_url(&run_dir),
        None => mcp_http_url_from_env().unwrap_or_else(|| FALLBACK_MCP_HTTP_URL.to_string()),
    }
}

/// `<bind>` → the loopback URL a vendor on this host should dial. A wildcard
/// bind (`0.0.0.0` / `[::]`) is not a destination, so it maps to loopback on the
/// same port; a concrete address is kept as-is. `None` when unparseable.
fn mcp_url_from_bind(web_bind: &str) -> Option<String> {
    let addr: std::net::SocketAddr = web_bind.trim().parse().ok()?;
    let port = addr.port();
    if addr.ip().is_unspecified() {
        return Some(format!("http://127.0.0.1:{port}/mcp"));
    }
    Some(match addr.ip() {
        std::net::IpAddr::V4(v4) => format!("http://{v4}:{port}/mcp"),
        std::net::IpAddr::V6(v6) => format!("http://[{v6}]:{port}/mcp"),
    })
}

// =====================================================================
// The single semantic type
// =====================================================================

/// What a managed session needs to reach the ccteam MCP face, independent of
/// any vendor dialect. Both fields are non-empty by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMcpEndpoint {
    url: String,
    bearer: String,
}

impl SessionMcpEndpoint {
    /// Endpoint at an EXPLICIT url. `None` when the session has no principal
    /// (empty sid/secret) or the url is blank — a session without a principal
    /// gets no tool face rather than an unauthenticated one.
    ///
    /// Used by the satellite path, whose url is the
    /// [`crate::execution::remote_exec::ExecSpec::DAEMON_URL_TOKEN`]
    /// placeholder the satellite substitutes on arrival, and by tests that pin
    /// a port.
    pub fn at(url: &str, sid: &str, secret: &str) -> Option<Self> {
        let url = url.trim();
        if url.is_empty() || sid.is_empty() || secret.is_empty() {
            return None;
        }
        Some(Self {
            url: url.to_string(),
            bearer: session_mcp_bearer(sid, secret),
        })
    }

    /// Endpoint for a session spawned by a daemon whose run dir is known.
    pub fn resolve_in(run_dir: &Path, sid: &str, secret: &str) -> Option<Self> {
        Self::at(&resolve_mcp_http_url(run_dir), sid, secret)
    }

    /// Endpoint for a session spawned by THIS daemon — the production entry
    /// point for every adapter.
    pub fn resolve(sid: &str, secret: &str) -> Option<Self> {
        Self::at(&daemon_mcp_http_url(), sid, secret)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    fn authorization(&self) -> String {
        format!("Bearer {}", self.bearer)
    }
}

/// The name every dialect registers ccteam's MCP server under. It is what a
/// vendor calls the server back by — Claude's `mcp_reconnect` control request
/// takes exactly this string — so the projections below and any code that asks
/// a vendor about its ccteam tool face read it from one place.
/// `session_mcp_projections_use_the_shared_server_name` holds the `json!`
/// literals (macro object keys must be literals) to this constant.
pub const CCTEAM_MCP_SERVER_NAME: &str = "ccteam";

// =====================================================================
// Dialect projections — pure functions of the endpoint
// =====================================================================

/// Claude `--mcp-config` body (object with `mcpServers`).
pub fn project_claude_mcp_json(ep: &SessionMcpEndpoint) -> Value {
    json!({
        "mcpServers": {
            "ccteam": {
                "type": "http",
                "url": ep.url(),
                "headers": {
                    "Authorization": ep.authorization(),
                }
            }
        }
    })
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
pub fn project_codex_thread_config(ep: &SessionMcpEndpoint) -> Value {
    json!({
        "mcp_servers": {
            "ccteam": {
                "url": ep.url(),
                "http_headers": {
                    "Authorization": ep.authorization(),
                },
            }
        }
    })
}

/// ACP `mcpServers` array entry, **shared by all three ACP vendors** (Grok +
/// OpenCode + Kimi) on `session/new`, `session/resume`, and `session/load`.
///
/// ACP (OpenCode 1.17.x / Grok 0.2.x) validates MCP entries strictly:
/// `headers` must be an **array** of `{name, value}` (not a map). Wrong shape
/// → jsonrpc -32602 Invalid params on `session/new` (smoke 2026-07-11).
pub fn project_acp_mcp_servers(ep: &SessionMcpEndpoint) -> Vec<Value> {
    vec![json!({
        "name": "ccteam",
        "type": "http",
        "url": ep.url(),
        "headers": [
            {
                "name": "Authorization",
                "value": ep.authorization(),
            }
        ]
    })]
}

/// `ManagedSessionBridge` dialect: the child env the ccteam-owned bridge
/// extension reads at load time. A bridge vendor has no native MCP config
/// surface, so these two keys ARE its `mcp.json` — mandatory, resolved from the
/// same endpoint as every other vendor, never inherited from the parent
/// process.
pub fn project_bridge_child_env(ep: &SessionMcpEndpoint) -> Vec<(String, String)> {
    vec![
        (BRIDGE_MCP_URL_ENV.to_string(), ep.url().to_string()),
        (BRIDGE_MCP_BEARER_ENV.to_string(), ep.bearer().to_string()),
    ]
}

// =====================================================================
// Adapter-facing composites (resolve + project)
// =====================================================================

/// Write the Claude dialect under the session chat dir. Returns the path.
pub fn write_session_mcp_config(
    project_dir: &Path,
    sid: &str,
    ep: &SessionMcpEndpoint,
) -> Result<PathBuf> {
    let path = session_mcp_config_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(&project_claude_mcp_json(ep))?;
    // Atomic-ish: write tmp then rename (same dir).
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(path)
}

/// ACP `mcpServers` for this session. Empty vec when sid/secret is missing
/// (caller still gets a valid session, just without the in-agent tool face).
pub fn acp_mcp_servers_http(sid: &str, secret: &str) -> Vec<Value> {
    SessionMcpEndpoint::resolve(sid, secret)
        .map(|ep| project_acp_mcp_servers(&ep))
        .unwrap_or_default()
}

/// Codex per-thread `config` for this session. `None` when sid/secret is empty
/// — fall through to the daemon's global HTTP config, authenticated as admin
/// — never an empty override that strips tools.
pub fn codex_thread_mcp_config(sid: &str, secret: &str) -> Option<Value> {
    SessionMcpEndpoint::resolve(sid, secret).map(|ep| project_codex_thread_config(&ep))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(url: &str, sid: &str, secret: &str) -> SessionMcpEndpoint {
        SessionMcpEndpoint::at(url, sid, secret).expect("non-empty principal + url")
    }

    #[test]
    fn endpoint_requires_a_principal_and_a_url() {
        assert!(SessionMcpEndpoint::at("http://127.0.0.1:7331/mcp", "s1", "sek").is_some());
        assert!(SessionMcpEndpoint::at("http://127.0.0.1:7331/mcp", "", "sek").is_none());
        assert!(SessionMcpEndpoint::at("http://127.0.0.1:7331/mcp", "s1", "").is_none());
        assert!(SessionMcpEndpoint::at("  ", "s1", "sek").is_none());
    }

    #[test]
    fn http_form_has_type_url_and_session_bearer() {
        let v = project_claude_mcp_json(&ep("http://127.0.0.1:7331/mcp", "s3", "sekret"));
        let srv = &v["mcpServers"]["ccteam"];
        assert_eq!(srv["type"], "http");
        assert_eq!(srv["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(
            srv["headers"]["Authorization"],
            "Bearer ccteam-sid:s3:sekret"
        );
    }

    /// The projections' `json!` object keys must be literals, so this holds
    /// all four of them to [`CCTEAM_MCP_SERVER_NAME`] — the string a vendor is
    /// asked to reconnect BY (Claude's `mcp_reconnect{serverName}`). If a
    /// dialect ever renamed its entry, the rebuild would silently target a
    /// server that does not exist.
    #[test]
    fn session_mcp_projections_use_the_shared_server_name() {
        let ep = ep("http://127.0.0.1:7331/mcp", "s1", "abc");
        assert!(project_claude_mcp_json(&ep)["mcpServers"][CCTEAM_MCP_SERVER_NAME].is_object());
        assert!(
            project_codex_thread_config(&ep)["mcp_servers"][CCTEAM_MCP_SERVER_NAME].is_object()
        );
        let acp = project_acp_mcp_servers(&ep);
        assert_eq!(acp[0]["name"], CCTEAM_MCP_SERVER_NAME);
        let env = project_bridge_child_env(&ep);
        assert!(
            env.iter().any(|(k, _)| k == BRIDGE_MCP_URL_ENV),
            "bridge dialect must still deliver the url: {env:?}"
        );
    }

    #[test]
    fn curated_config_never_emits_a_stdio_child() {
        // One wire form. A stdio `command`/`args`/`env` entry would re-open the
        // self-referential init path the HTTP move closed.
        let v = project_claude_mcp_json(&ep("http://127.0.0.1:7331/mcp", "s1", "abc"));
        let srv = &v["mcpServers"]["ccteam"];
        assert_eq!(srv["type"], "http");
        assert!(srv.get("command").is_none());
        assert!(srv.get("args").is_none());
        assert!(srv.get("env").is_none());
    }

    #[test]
    fn write_session_mcp_config_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path =
            write_session_mcp_config(tmp.path(), "s9", &ep("http://127.0.0.1:9/mcp", "s9", "x"))
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
        let v = project_acp_mcp_servers(&ep("http://127.0.0.1:7331/mcp", "s3", "sekret"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0]["name"], "ccteam");
        assert_eq!(v[0]["type"], "http");
        assert!(v[0]["headers"].is_array(), "ACP requires headers[]");
        assert_eq!(v[0]["headers"][0]["name"], "Authorization");
        assert_eq!(v[0]["headers"][0]["value"], "Bearer ccteam-sid:s3:sekret");
        // No principal → no tool face (composite, not the projection).
        assert!(acp_mcp_servers_http("", "x").is_empty());
        assert!(acp_mcp_servers_http("s1", "").is_empty());
    }

    #[test]
    fn codex_thread_config_snake_case_http_with_session_bearer() {
        let v = project_codex_thread_config(&ep("http://127.0.0.1:7331/mcp", "s7", "sek"));
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
        // Empty secret / sid -> None (global HTTP config remains).
        assert!(codex_thread_mcp_config("s7", "").is_none());
        assert!(codex_thread_mcp_config("", "sek").is_none());
    }

    #[test]
    fn bridge_env_carries_the_same_endpoint_as_every_other_dialect() {
        let endpoint = ep("http://127.0.0.1:9100/mcp", "s5", "sek");
        let env = project_bridge_child_env(&endpoint);
        let get = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or_default()
        };
        assert_eq!(get(BRIDGE_MCP_URL_ENV), "http://127.0.0.1:9100/mcp");
        assert_eq!(get(BRIDGE_MCP_BEARER_ENV), "ccteam-sid:s5:sek");
        // Same url the native dialects carry — the bridge is a dialect, not a
        // second endpoint.
        assert_eq!(
            project_claude_mcp_json(&endpoint)["mcpServers"]["ccteam"]["url"],
            get(BRIDGE_MCP_URL_ENV)
        );
    }

    #[test]
    fn mcp_url_from_bind_maps_wildcard_to_loopback_and_keeps_concrete() {
        // A wildcard bind is not a destination — a vendor on this host must
        // dial loopback on the SAME port (the whole point: non-default ports).
        assert_eq!(
            mcp_url_from_bind("0.0.0.0:8080").as_deref(),
            Some("http://127.0.0.1:8080/mcp")
        );
        assert_eq!(
            mcp_url_from_bind("[::]:9000").as_deref(),
            Some("http://127.0.0.1:9000/mcp")
        );
        assert_eq!(
            mcp_url_from_bind("127.0.0.1:9099").as_deref(),
            Some("http://127.0.0.1:9099/mcp")
        );
        assert_eq!(
            mcp_url_from_bind("192.168.1.5:7331").as_deref(),
            Some("http://192.168.1.5:7331/mcp")
        );
        assert_eq!(
            mcp_url_from_bind("[::1]:7331").as_deref(),
            Some("http://[::1]:7331/mcp")
        );
        // Unparseable → None, so startup writes nothing and reports the bind
        // error through its own path.
        assert!(mcp_url_from_bind("not-an-addr").is_none());
        assert!(mcp_url_from_bind("").is_none());
    }

    /// The chain that used to be split in two: managed spawn read env-only
    /// while global registration read the recorded bind, so a non-default
    /// `--web-bind` gave a correct global config and a session pointing at
    /// 7331. One chain now serves both.
    #[test]
    fn resolve_prefers_recorded_bind_over_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("run");
        // Nothing recorded yet → default-bind fallback.
        assert_eq!(resolve_mcp_http_url(&run_dir), FALLBACK_MCP_HTTP_URL);
        // A daemon on a custom port records it; every reader follows.
        record_daemon_mcp_url(&run_dir, "0.0.0.0:8080").unwrap();
        assert_eq!(resolve_mcp_http_url(&run_dir), "http://127.0.0.1:8080/mcp");
        assert_eq!(
            SessionMcpEndpoint::resolve_in(&run_dir, "s1", "sek")
                .unwrap()
                .url(),
            "http://127.0.0.1:8080/mcp"
        );
        // Re-record on restart (idempotent, last write wins).
        record_daemon_mcp_url(&run_dir, "0.0.0.0:9100").unwrap();
        assert_eq!(resolve_mcp_http_url(&run_dir), "http://127.0.0.1:9100/mcp");
        // An unparseable bind leaves the previous record intact.
        record_daemon_mcp_url(&run_dir, "bogus").unwrap();
        assert_eq!(resolve_mcp_http_url(&run_dir), "http://127.0.0.1:9100/mcp");
    }
}
