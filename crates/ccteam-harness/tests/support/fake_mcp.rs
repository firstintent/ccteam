//! A stub ccteam MCP daemon (`POST /mcp`) shared by the Pi test binaries.
//!
//! Every managed session — whatever the vendor dialect — dials one HTTP
//! endpoint with its own `ccteam-sid:<sid>:<secret>` bearer, so any test that
//! spawns a managed session needs something on the other end. Without one the
//! spawn resolves to the default-bind fallback and dials **the developer's
//! real daemon on 127.0.0.1:7331** (or gets ECONNREFUSED on CI) — so pinning
//! this stub is test isolation, not convenience.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

#[derive(Clone, Default)]
pub struct McpCapture {
    pub calls: Arc<Mutex<Vec<McpCall>>>,
}

#[derive(Clone)]
pub struct McpCall {
    pub authorization: Option<String>,
    pub method: String,
    pub params: Value,
}

async fn handle(
    State(capture): State<McpCapture>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let method = request["method"].as_str().unwrap_or("missing").to_string();
    capture.calls.lock().unwrap().push(McpCall {
        authorization,
        method: method.clone(),
        params: request.get("params").cloned().unwrap_or(Value::Null),
    });
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match method.as_str() {
        "initialize" => {
            json!({"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fake-ccteam","version":"test"}})
        }
        "tools/list" => json!({"tools":[
            {"name":"status","description":"Status","inputSchema":{"type":"object","properties":{}}},
            {"name":"grok_claude_codex_kimi","description":"Discovery","inputSchema":{"type":"object","properties":{}}},
            {"name":"chat_send_file","description":"Send file","inputSchema":{"type":"object","properties":{}}},
            {"name":"agent","description":"Hire or task","inputSchema":{"type":"object","properties":{}}},
            {"name":"agent_read","description":"Read the team","inputSchema":{"type":"object","properties":{}}},
            {"name":"agent_stop","description":"Stop","inputSchema":{"type":"object","properties":{}}}
        ]}),
        "tools/call" => json!({"content":[{"type":"text","text":"ok"}],"isError":false}),
        _ => Value::Null,
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

/// Bind an ephemeral port and serve `POST /mcp`. Returns the server task and
/// the full URL a session should be pointed at.
pub async fn start_fake_mcp(capture: McpCapture) -> (tokio::task::JoinHandle<()>, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/mcp", post(handle))
        .with_state(capture);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (server, format!("http://{addr}/mcp"))
}
