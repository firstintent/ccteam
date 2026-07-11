//! v0.8.24 F1.12 — project-scoped third-party MCP server registration.
//!
//! The workflow → MCP Servers page: list what the project's `.mcp.json`
//! declares (vendor-native config Claude Code reads on next start) and
//! register a new server by **idempotently writing that config** — the
//! same seam `ccteam init` uses for ccteam's own entry
//! (`ccteam_core::merge_named_mcp_server` / `merge_project_mcp_json`).
//!
//! **Red line**: this surface writes configuration ONLY. ccteam never
//! fetches, installs, or executes a third-party server — Claude Code
//! launches it (stdio `command`) or dials it (`url`) itself, under the
//! vendor's own trust prompts. Env values are masked on read so a
//! token-bearing entry never echoes.
//!
//! ACL: GET rides the project ACL (`auth::project_acl_layer` covers every
//! `/projects/{slug}/*` route). POST (the config write) is additionally
//! **admin-only** (`deny_non_admin`) — same posture as the hosts page's
//! `register-mcp` write.

use std::collections::BTreeMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::auth::{deny_non_admin, Identity};
use crate::state::AppState;

/// One declared server from the project `.mcp.json` (masked view).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct McpServerView {
    /// The `mcpServers` key.
    pub name: String,
    /// `stdio` (command) | `http` | `sse` | `unknown`.
    pub kind: String,
    /// stdio launch command; `null` for url-typed entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// http/sse endpoint; `null` for stdio entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Env var NAMES only — values are never echoed (may carry tokens).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_keys: Option<Vec<String>>,
    /// True for ccteam's own reserved entry.
    pub is_ccteam: bool,
}

/// `GET .../mcp-servers` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct McpServersResponse {
    pub servers: Vec<McpServerView>,
    /// Whether ccteam's own server entry is present in this project's
    /// `.mcp.json` (the `ccteam init` seam).
    pub ccteam_registered: bool,
}

/// POST body: exactly ONE of `url` (http/sse server) or `command`
/// (stdio server) must be given.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterMcpServerForm {
    /// `mcpServers` key: `[A-Za-z0-9_-]{1,50}`, `ccteam` reserved.
    pub name: String,
    /// Remote server endpoint (`http(s)://…`) → `{type: "http", url}`.
    #[serde(default)]
    pub url: Option<String>,
    /// stdio launch command (resolved by Claude Code, NOT by ccteam).
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Env for the stdio child. Stored verbatim; never echoed back.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
}

fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.into() })),
    )
        .into_response()
}

fn reject_unknown_project(app: &AppState, slug: &str) -> Option<Response> {
    if app.paths.project_state(slug).exists() {
        return None;
    }
    Some(
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("project not found: {slug}")})),
        )
            .into_response(),
    )
}

/// Classify one `mcpServers` entry into the masked view.
fn view_of(name: &str, entry: &Value) -> McpServerView {
    let url = entry.get("url").and_then(|v| v.as_str());
    let command = entry.get("command").and_then(|v| v.as_str());
    let explicit_type = entry.get("type").and_then(|v| v.as_str());
    let kind = match (explicit_type, url, command) {
        (Some(t), _, _) => t.to_string(),
        (None, Some(_), _) => "http".to_string(),
        (None, None, Some(_)) => "stdio".to_string(),
        _ => "unknown".to_string(),
    };
    let args = entry.get("args").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    });
    let env_keys = entry
        .get("env")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect::<Vec<_>>())
        .filter(|k| !k.is_empty());
    McpServerView {
        name: name.to_string(),
        kind,
        command: command.map(str::to_string),
        args,
        url: url.map(str::to_string),
        env_keys,
        is_ccteam: name == ccteam_core::CCTEAM_MCP_SERVER_KEY,
    }
}

/// Read + parse the project `.mcp.json` into views. Missing file → empty.
fn read_servers(project_dir: &std::path::Path) -> anyhow::Result<Vec<McpServerView>> {
    let path = project_dir.join(".mcp.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path)?;
    let v: Value = serde_json::from_str(raw.trim())?;
    let Some(servers) = v.get("mcpServers").and_then(|s| s.as_object()) else {
        return Ok(Vec::new());
    };
    Ok(servers.iter().map(|(k, e)| view_of(k, e)).collect())
}

/// `GET /api/v1/projects/{slug}/mcp-servers` — list the project's declared
/// MCP servers (masked: env values never echo). Project ACL applies.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/mcp-servers",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Declared servers from the project `.mcp.json`", body = McpServersResponse),
        (status = 404, description = "Unknown project"),
    ),
)]
pub(crate) async fn handle_list_mcp_servers(
    State(app): State<AppState>,
    Path(slug): Path<String>,
) -> Response {
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    let project_dir = app.paths.project_dir(&slug);
    match tokio::task::spawn_blocking(move || read_servers(&project_dir)).await {
        Ok(Ok(servers)) => {
            let ccteam_registered = servers.iter().any(|s| s.is_ccteam);
            Json(McpServersResponse {
                servers,
                ccteam_registered,
            })
            .into_response()
        }
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("read .mcp.json: {err}")})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("worker: {err}")})),
        )
            .into_response(),
    }
}

/// Build the `.mcp.json` entry from a validated form. Pure — unit-tested.
fn entry_from_form(form: &RegisterMcpServerForm) -> Result<Value, String> {
    let url = form.url.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let command = form
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (url, command) {
        (Some(_), Some(_)) => Err("give either `url` or `command`, not both".into()),
        (None, None) => Err("one of `url` (http server) or `command` (stdio) is required".into()),
        (Some(u), None) => {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err("`url` must start with http:// or https://".into());
            }
            Ok(json!({ "type": "http", "url": u }))
        }
        (None, Some(c)) => {
            let mut entry = serde_json::Map::new();
            entry.insert("command".into(), json!(c));
            if let Some(args) = form.args.as_ref().filter(|a| !a.is_empty()) {
                entry.insert("args".into(), json!(args));
            }
            if let Some(env) = form.env.as_ref().filter(|e| !e.is_empty()) {
                entry.insert("env".into(), json!(env));
            }
            Ok(Value::Object(entry))
        }
    }
}

/// `POST /api/v1/projects/{slug}/mcp-servers` — idempotently merge one
/// third-party server into the project `.mcp.json` (vendor-native config;
/// Claude Code picks it up on its next start). **Admin-only** write; ccteam
/// executes/downloads NOTHING here.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/mcp-servers",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug")),
    request_body = RegisterMcpServerForm,
    responses(
        (status = 201, description = "Merged; `{ok, name, path}`", body = serde_json::Value),
        (status = 400, description = "Bad name / bad entry (url XOR command)"),
        (status = 403, description = "Non-admin"),
        (status = 404, description = "Unknown project"),
    ),
)]
pub(crate) async fn handle_register_mcp_server(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(slug): Path<String>,
    Json(form): Json<RegisterMcpServerForm>,
) -> Response {
    // Config write → admin-only (same posture as hosts register-mcp).
    if let Some(deny) = deny_non_admin(&identity) {
        return deny;
    }
    if let Some(resp) = reject_unknown_project(&app, &slug) {
        return resp;
    }
    let name = form.name.trim().to_string();
    if let Err(err) = ccteam_core::validate_mcp_server_name(&name) {
        return bad_request(err.to_string());
    }
    let entry = match entry_from_form(&form) {
        Ok(e) => e,
        Err(msg) => return bad_request(msg),
    };
    let project_dir = app.paths.project_dir(&slug);
    let write = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let path = project_dir.join(".mcp.json");
        let existing = if path.exists() {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        let merged = ccteam_core::merge_named_mcp_server(&existing, &name, entry)?;
        // Atomic write (tmp + rename) — never leave a torn config.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, merged.as_bytes())?;
        std::fs::rename(&tmp, &path)?;
        Ok(path.display().to_string())
    })
    .await;
    match write {
        Ok(Ok(path)) => (
            StatusCode::CREATED,
            Json(json!({ "ok": true, "name": form.name.trim(), "path": path })),
        )
            .into_response(),
        Ok(Err(err)) => bad_request(format!("{err}")),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("worker: {err}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(
        name: &str,
        url: Option<&str>,
        command: Option<&str>,
        args: Option<Vec<String>>,
    ) -> RegisterMcpServerForm {
        RegisterMcpServerForm {
            name: name.into(),
            url: url.map(str::to_string),
            command: command.map(str::to_string),
            args,
            env: None,
        }
    }

    #[test]
    fn entry_from_form_requires_url_xor_command() {
        assert!(entry_from_form(&form("a", None, None, None)).is_err());
        assert!(entry_from_form(&form("a", Some("https://x"), Some("npx"), None)).is_err());
        // whitespace-only counts as absent
        assert!(entry_from_form(&form("a", Some("  "), None, None)).is_err());
    }

    #[test]
    fn entry_from_form_url_must_be_http() {
        assert!(entry_from_form(&form("a", Some("ftp://x"), None, None)).is_err());
        let e =
            entry_from_form(&form("a", Some("https://mcp.context7.com/mcp"), None, None)).unwrap();
        assert_eq!(e["type"], "http");
        assert_eq!(e["url"], "https://mcp.context7.com/mcp");
    }

    #[test]
    fn entry_from_form_stdio_carries_command_and_args() {
        let e = entry_from_form(&form(
            "pw",
            None,
            Some("npx"),
            Some(vec!["@playwright/mcp@latest".into()]),
        ))
        .unwrap();
        assert_eq!(e["command"], "npx");
        assert_eq!(e["args"][0], "@playwright/mcp@latest");
        assert!(e.get("env").is_none(), "empty env omitted");
    }

    #[test]
    fn view_masks_env_values() {
        let v = view_of(
            "linear",
            &serde_json::json!({
                "command": "linear-mcp",
                "env": { "LINEAR_TOKEN": "sekrit" }
            }),
        );
        assert_eq!(v.kind, "stdio");
        assert_eq!(
            v.env_keys.as_deref(),
            Some(&["LINEAR_TOKEN".to_string()][..])
        );
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("sekrit"), "env VALUES must never echo: {s}");
    }

    #[test]
    fn view_classifies_url_and_ccteam() {
        let v = view_of(
            "context7",
            &serde_json::json!({"type":"sse","url":"https://x"}),
        );
        assert_eq!(v.kind, "sse");
        assert!(!v.is_ccteam);
        let c = view_of(
            "ccteam",
            &serde_json::json!({"command":"/usr/local/bin/ccteam","args":["internal","mcp-serve"]}),
        );
        assert!(c.is_ccteam);
        assert_eq!(c.kind, "stdio");
    }
}
