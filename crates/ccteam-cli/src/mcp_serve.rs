//! Vendor MCP registration + the daemon `mcp.sock` one-shot client.
//!
//! **There is no stdio MCP server any more.** Every managed session and every
//! hand-started vendor session reaches ccteam over `POST /mcp` on the daemon's
//! HTTP surface; the registration written here is what points them at it. The
//! stdio server this module used to host existed for the era when the global
//! vendor entry spawned `ccteam internal mcp-serve` as a child — one orphan
//! child per session, a same-name double registration, and a JSON-RPC loop
//! duplicating what the HTTP route already does. That entry point is deleted
//! rather than deprecated: a command that still runs is a command a stale
//! config will still invoke.
//!
//! What remains:
//!
//! - **registration** — the per-vendor global entries (`install_*`), and the
//!   idempotent repair pass the daemon runs at start
//!   (`auto_register_vendor_mcp`). The writer itself lives in
//!   [`ccteam_core::mcp_register`]; this module is the CLI-side orchestration.
//! - **`mcp.sock` one-shot client** — [`forward_to_socket`], used by
//!   `ccteam config` to hot-reload the running daemon.
//! - **tool surface accessor** — [`tool_definitions`], for `doctor
//!   --verify-mcp`.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

use ccteam_core::CcteamPaths;

/// Single source of truth for the registered tool surface — delegated to
/// [`ccteam_im::mcp::tool_definitions`] (doctor `--verify-mcp` + tests).
pub(crate) fn tool_definitions() -> Vec<Value> {
    ccteam_im::mcp::tool_definitions()
}

/// Open a one-shot connection to the daemon `mcp.sock`, send one
/// JSON-RPC line, and read one response line back.
#[cfg(unix)]
pub(crate) async fn forward_to_socket(socket: &std::path::Path, req: &Value) -> Result<Value> {
    use tokio::io::AsyncWriteExt as _;
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    let mut lines = BufReader::new(reader).lines();
    let resp = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("mcp.sock closed before responding"))?;
    Ok(serde_json::from_str(&resp)?)
}

#[cfg(not(unix))]
pub(crate) async fn forward_to_socket(_socket: &std::path::Path, _req: &Value) -> Result<Value> {
    Err(anyhow!("mcp.sock forwarding is unix-only"))
}

/// `ccteam config` — register the ccteam MCP server in `~/.claude.json`.
pub fn install_mcp_into(
    claude_json: &std::path::Path,
    mcp_http_url: &str,
    admin_token: &str,
) -> Result<()> {
    ccteam_core::mcp_register::install_mcp_into(claude_json, mcp_http_url, admin_token)
}

/// Production path for Claude MCP install: like every other vendor, the global
/// `~/.claude.json` entry points at the daemon's HTTP `/mcp` endpoint and
/// authenticates with the admin web token, so a plain `claude` main session can
/// orchestrate. ccteam-managed Claude sessions override the same-named entry via
/// `--mcp-config` with their per-session bearer.
pub fn install_mcp() -> Result<std::path::PathBuf> {
    let claude_json = ccteam_core::projects::resolve_claude_json_path()?;
    let paths = CcteamPaths::from_env()?;
    let admin_token = ccteam_web::token::generate_or_load_token(&paths.web_token_path())?;
    let mcp_http_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run"));
    install_mcp_into(&claude_json, &mcp_http_url, &admin_token)?;
    Ok(claude_json)
}

/// Codex equivalent of [`install_mcp_into`].
pub fn install_codex_mcp_into(
    config_toml: &std::path::Path,
    mcp_http_url: &str,
    admin_token: &str,
) -> Result<()> {
    ccteam_core::mcp_register::install_codex_mcp_into(config_toml, mcp_http_url, admin_token)
}

/// Production path for Codex MCP install. Unlike Claude's historical global
/// stdio entry, Codex uses the daemon's HTTP MCP endpoint. The global entry is
/// authenticated with the admin web token; ccteam-managed Codex threads
/// override that header with their per-session bearer.
pub fn install_codex_mcp() -> Result<std::path::PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
        .ok_or_else(|| anyhow!("cannot resolve CODEX_HOME or ~/.codex (no home dir)"))?;
    let config_toml = codex_home.join("config.toml");
    let paths = CcteamPaths::from_env()?;
    let admin_token = ccteam_web::token::generate_or_load_token(&paths.web_token_path())?;
    let mcp_http_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run"));
    install_codex_mcp_into(&config_toml, &mcp_http_url, &admin_token)?;
    Ok(config_toml)
}

/// Production path for Grok CLI MCP install (v0.9.3 vendor symmetry): the
/// global `~/.grok/config.toml` entry authenticates with the admin web token
/// so a plain `grok` main session can orchestrate; ccteam-managed grok
/// sessions keep their ACP-injected per-session principal (same-name dedup,
/// injected server wins — real-machine verified).
pub fn install_grok_mcp() -> Result<std::path::PathBuf> {
    let config_toml = ccteam_core::mcp_register::resolve_grok_config_path()?;
    let paths = CcteamPaths::from_env()?;
    let admin_token = ccteam_web::token::generate_or_load_token(&paths.web_token_path())?;
    let mcp_http_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run"));
    ccteam_core::mcp_register::install_grok_mcp_into(&config_toml, &mcp_http_url, &admin_token)?;
    Ok(config_toml)
}

/// Production path for OpenCode MCP install (v0.9.3 vendor symmetry). Managed
/// OpenCode sessions override the same-named entry at runtime (`MCP.add`
/// replaces by name with the ACP-injected per-session principal).
pub fn install_opencode_mcp() -> Result<std::path::PathBuf> {
    let opencode_json = ccteam_core::mcp_register::resolve_opencode_config_path()?;
    let paths = CcteamPaths::from_env()?;
    let admin_token = ccteam_web::token::generate_or_load_token(&paths.web_token_path())?;
    let mcp_http_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run"));
    ccteam_core::mcp_register::install_opencode_mcp_into(
        &opencode_json,
        &mcp_http_url,
        &admin_token,
    )?;
    Ok(opencode_json)
}

/// Production path for Kimi Code MCP install (vendor symmetry): the global
/// `$KIMI_CODE_HOME/mcp.json` entry authenticates with the admin web token so
/// a plain `kimi` main session can orchestrate; ccteam-managed kimi sessions
/// keep their ACP-injected per-session principal (same-name dedup posture as
/// grok/opencode).
pub fn install_kimi_mcp() -> Result<std::path::PathBuf> {
    let mcp_json = ccteam_core::mcp_register::resolve_kimi_config_path()?;
    let paths = CcteamPaths::from_env()?;
    let admin_token = ccteam_web::token::generate_or_load_token(&paths.web_token_path())?;
    let mcp_http_url =
        ccteam_harness::execution::mcp_config::resolve_mcp_http_url(&paths.root.join("run"));
    ccteam_core::mcp_register::install_kimi_mcp_into(&mcp_json, &mcp_http_url, &admin_token)?;
    Ok(mcp_json)
}

/// Best-effort daemon-start registration for every installed/configured vendor.
///
/// The gate is deliberately read-only and never executes a vendor binary: an
/// explicit binary override must exist, a default binary must be executable on
/// `PATH`, or the vendor config file must already exist. `Ok(None)` means the
/// vendor was absent and was skipped; errors are returned per vendor so callers
/// can report every outcome without aborting daemon startup.
pub fn auto_register_vendor_mcp() -> Vec<(&'static str, Result<Option<std::path::PathBuf>>)> {
    ccteam_core::host_registry::AGENT_PROBE_SPECS
        .iter()
        .filter(|spec| spec.tool_surface.uses_native_mcp_config())
        .map(|spec| {
            let result = auto_register_one(spec);
            if matches!(result, Ok(None)) {
                tracing::debug!(
                    vendor = spec.vendor,
                    "vendor absent; skipping MCP registration"
                );
            }
            (spec.vendor, result)
        })
        .collect()
}

fn auto_register_one(
    spec: &ccteam_core::host_registry::AgentProbeSpec,
) -> Result<Option<std::path::PathBuf>> {
    if !spec.tool_surface.uses_native_mcp_config() {
        return Ok(None);
    }
    let should_register = if ccteam_core::host_registry::bin_resolvable(spec) {
        true
    } else {
        vendor_config_path(spec.vendor)?.exists()
    };
    if !should_register {
        return Ok(None);
    }
    let path = match spec.vendor {
        "claude" => install_mcp(),
        "codex" => install_codex_mcp(),
        "grok" => install_grok_mcp(),
        "opencode" => install_opencode_mcp(),
        "kimi" => install_kimi_mcp(),
        other => anyhow::bail!("unsupported MCP registration vendor: {other}"),
    }?;
    Ok(Some(path))
}

fn vendor_config_path(vendor: &str) -> Result<std::path::PathBuf> {
    match vendor {
        "claude" => ccteam_core::projects::resolve_claude_json_path(),
        "codex" => ccteam_core::mcp_register::resolve_codex_config_path(),
        "grok" => ccteam_core::mcp_register::resolve_grok_config_path(),
        "opencode" => ccteam_core::mcp_register::resolve_opencode_config_path(),
        "kimi" => ccteam_core::mcp_register::resolve_kimi_config_path(),
        other => anyhow::bail!("unsupported MCP registration vendor: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_im::mcp::MCP_PROTOCOL_VERSION;
    use serde_json::json;

    /// Exact set of MCP tool names (8 tools; `screenshot` culled and the
    /// bare-name status beacon alias added 2026-07-26).
    const EXPECTED_TOOL_NAMES: &[&str] = &[
        "chat_send_file",
        "grok_claude_codex_kimi",
        "session_collect",
        "session_dispatch",
        "session_list",
        "session_spawn",
        "session_stop",
        "status",
    ];

    #[test]
    fn tool_definitions_count_matches_spec() {
        assert_eq!(tool_definitions().len(), 8);
        assert_eq!(tool_definitions().len(), EXPECTED_TOOL_NAMES.len());
    }

    #[test]
    fn tool_definitions_exact_set() {
        let tools = tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        let mut expected: Vec<&str> = EXPECTED_TOOL_NAMES.to_vec();
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn tool_definitions_have_unique_names_and_object_schemas() {
        let tools = tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8, "tool names must be unique");
        for tool in &tools {
            // Wire names are BARE — the client namespaces by server key
            // (`mcp__ccteam__session_spawn`); a baked-in prefix doubles up.
            assert!(!tool["name"].as_str().unwrap().starts_with("ccteam__"));
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn install_mcp_into_writes_http_url_and_admin_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        install_mcp_into(&claude_json, "http://127.0.0.1:7331/mcp", "admin-token").unwrap();
        let body = std::fs::read_to_string(&claude_json).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        let entry = &v["mcpServers"]["ccteam"];
        assert_eq!(entry["type"], "http");
        assert_eq!(entry["url"], "http://127.0.0.1:7331/mcp");
        assert_eq!(
            entry["headers"]["Authorization"],
            "Bearer ccteam:admin-token"
        );
        // Vendor symmetry: no global registration spawns an `mcp-serve` child.
        assert!(entry.get("command").is_none());
        assert!(entry.get("args").is_none());
    }

    #[test]
    fn install_mcp_into_preserves_other_top_level_keys() {
        let tmp = tempfile::TempDir::new().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(
            &claude_json,
            r#"{"userID": "rob", "mcpServers": {"playwright": {"command": "npx"}}}"#,
        )
        .unwrap();
        install_mcp_into(&claude_json, "http://localhost:7331/mcp", "tok").unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["userID"], "rob");
        assert_eq!(v["mcpServers"]["playwright"]["command"], "npx");
        assert_eq!(
            v["mcpServers"]["ccteam"]["url"],
            "http://localhost:7331/mcp"
        );
    }

    #[test]
    fn install_codex_mcp_into_writes_http_url_and_admin_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        install_codex_mcp_into(&config_toml, "http://127.0.0.1:7331/mcp", "admin-token").unwrap();
        let body = std::fs::read_to_string(&config_toml).unwrap();
        let v: toml::Value = toml::from_str(&body).unwrap();
        assert_eq!(
            v["mcp_servers"]["ccteam"]["url"].as_str().unwrap(),
            "http://127.0.0.1:7331/mcp"
        );
        assert_eq!(
            v["mcp_servers"]["ccteam"]["http_headers"]["Authorization"]
                .as_str()
                .unwrap(),
            "Bearer ccteam:admin-token"
        );
        assert!(v["mcp_servers"]["ccteam"].get("command").is_none());
    }

    #[test]
    fn install_codex_mcp_into_merges_preserving_other_keys_and_servers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "model = \"gpt-5\"\n\n[mcp_servers.foo]\ncommand = \"foo-server\"\nargs = [\"--flag\"]\n",
        )
        .unwrap();
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", "tok").unwrap();
        let v: toml::Value =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(v["model"].as_str().unwrap(), "gpt-5");
        assert_eq!(
            v["mcp_servers"]["foo"]["command"].as_str().unwrap(),
            "foo-server"
        );
        assert_eq!(
            v["mcp_servers"]["foo"]["args"][0].as_str().unwrap(),
            "--flag"
        );
        assert_eq!(
            v["mcp_servers"]["ccteam"]["url"].as_str().unwrap(),
            "http://localhost:7331/mcp"
        );
    }

    #[test]
    fn install_codex_mcp_into_is_idempotent_and_replaces_only_ccteam() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("config.toml");
        std::fs::write(
            &config_toml,
            "[mcp_servers.ccteam]\ncommand = \"/old/bin/ccteam\"\nargs = [\"mcp-serve\"]\n\n[mcp_servers.playwright]\ncommand = \"npx\"\n",
        )
        .unwrap();
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", "new-token").unwrap();
        let first = std::fs::read_to_string(&config_toml).unwrap();
        let v: toml::Value = toml::from_str(&first).unwrap();
        assert_eq!(
            v["mcp_servers"]["ccteam"]["url"].as_str().unwrap(),
            "http://localhost:7331/mcp"
        );
        assert!(v["mcp_servers"]["ccteam"].get("command").is_none());
        assert!(v["mcp_servers"]["ccteam"].get("args").is_none());
        assert_eq!(
            v["mcp_servers"]["playwright"]["command"].as_str().unwrap(),
            "npx"
        );
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", "new-token").unwrap();
        let second = std::fs::read_to_string(&config_toml).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn install_codex_mcp_into_creates_missing_file_and_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_toml = tmp.path().join("nested").join(".codex").join("config.toml");
        assert!(!config_toml.exists());
        install_codex_mcp_into(&config_toml, "http://localhost:7331/mcp", "tok").unwrap();
        assert!(config_toml.exists());
        let v: toml::Value =
            toml::from_str(&std::fs::read_to_string(&config_toml).unwrap()).unwrap();
        assert_eq!(
            v["mcp_servers"]["ccteam"]["url"].as_str().unwrap(),
            "http://localhost:7331/mcp"
        );
    }

    #[tokio::test]
    async fn handle_initialize_returns_tools_capability() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let resp = ccteam_im::mcp::handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "ccteam");
        let instructions = resp["result"]["instructions"].as_str().unwrap();
        assert!(
            instructions.contains("image_path"),
            "instructions: {instructions}"
        );
        assert!(instructions.contains("file_path"));
        assert!(instructions.contains("Read"));
        assert!(instructions.contains("<channel"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forward_to_socket_round_trips_one_line() {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("mcp.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: Value = serde_json::from_str(&req_line).unwrap();
            assert_eq!(req["params"]["name"], "chat_send_file");
            let resp = json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "content": [{"type":"text","text":"delivered: queued"}], "isError": false }
            });
            let mut line = serde_json::to_string(&resp).unwrap();
            line.push('\n');
            writer.write_all(line.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        });

        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "chat_send_file", "arguments": { "path": "/x.png" } }
        });
        let resp = forward_to_socket(&socket, &req).await.unwrap();
        assert_eq!(
            resp.pointer("/result/content/0/text")
                .unwrap()
                .as_str()
                .unwrap(),
            "delivered: queued"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handle_tools_list_returns_full_tool_set() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let resp = ccteam_im::mcp::handle_request(&paths, &req).await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 8);
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        names.sort();
        let mut expected = EXPECTED_TOOL_NAMES.to_vec();
        expected.sort();
        assert_eq!(names, expected);
        for gone in [
            "ccteam__admin_ls",
            "ccteam__admin_change_persona",
            "ccteam__admin_add_tool",
            "ccteam__advise_vote",
            "ccteam__advise_parallel",
            "ccteam__chat_register_bot",
            "ccteam__chat_unregister_bot",
            "ccteam__chat_list_bots",
            "ccteam__chat_lifecycle",
            "ccteam__workflow_show",
            // 2026-07-26 cull: tmux-era pane screenshot (web route stays).
            "screenshot",
            // Pre-rename prefixed wire names — no compat alias.
            "ccteam__status",
            "ccteam__screenshot",
            "ccteam__chat_send_file",
            "ccteam__session_spawn",
            "ccteam__session_list",
        ] {
            assert!(!names.contains(&gone), "culled tool present: {gone}");
        }
    }

    /// 2026-07-26 cull — a stdio `screenshot` call must fall through to the
    /// protocol core's unknown-tool error (no local renderer path left).
    #[tokio::test]
    async fn handle_tools_call_screenshot_is_unknown_after_cull() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "screenshot",
                "arguments": { "slug": "no-such-slug-xyz", "lines": 5 }
            }
        });
        let resp = ccteam_im::mcp::handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("unknown tool: screenshot"),
            "expected unknown-tool error, got: {text}"
        );
    }

    #[tokio::test]
    async fn handle_notifications_initialized_returns_no_response() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        assert!(ccteam_im::mcp::handle_request(&paths, &req).await.is_none());
    }

    #[tokio::test]
    async fn handle_tools_call_ls_returns_empty_projects_array_for_fresh_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "status", "arguments": {} }
        });
        let resp = ccteam_im::mcp::handle_request(&paths, &req).await.unwrap();
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(parsed["projects"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn handle_tools_call_unknown_tool_returns_iserror_true() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "ccteam__no_such_tool", "arguments": {} }
        });
        let resp = ccteam_im::mcp::handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn ls_succeeds_without_daemon_and_annotates_health() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let req = json!({
            "jsonrpc": "2.0",
            "id": 72,
            "method": "tools/call",
            "params": { "name": "status", "arguments": {} }
        });
        let resp = ccteam_im::mcp::handle_request(&paths, &req).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(content).unwrap();
        assert_eq!(
            parsed["daemon"]["status"], "unreachable",
            "status must annotate daemon health when daemon is down"
        );
    }
}
