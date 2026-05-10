//! `ccteam hook progress-append <event-type>` — append one event line
//! to `~/.ccteam/progress/<slug>.jsonl`. The actual JSONL append lives
//! in `ccteam_core::progress::append_event`; this module only handles
//! Claude Code → ccteam event-shape translation.

use std::path::Path;

use anyhow::{anyhow, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use ccteam_core::{progress::append_event, session_context_from_cwd, CcteamPaths};

pub fn progress_append(paths: &CcteamPaths, event_type: &str, stdin: &Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let context = session_context_from_cwd(Path::new(cwd), paths)?;

    let mut event = json!({
        "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "event": event_type,
    });
    if let Some(sid) = &context.sid {
        event["sid"] = json!(sid);
    }

    if let Some(tool) = stdin.get("tool_name").and_then(|s| s.as_str()) {
        event["tool"] = json!(tool);
    }
    if let Some(p) = stdin
        .get("tool_input")
        .and_then(|t| t.get("file_path"))
        .and_then(|s| s.as_str())
    {
        event["path"] = json!(p);
    }
    if let Some(cmd) = stdin
        .get("tool_input")
        .and_then(|t| t.get("command"))
        .and_then(|s| s.as_str())
    {
        event["cmd"] = json!(cmd);
    }
    if let Some(exit) = stdin
        .get("tool_response")
        .and_then(|r| r.get("exit_code"))
        .and_then(|n| n.as_i64())
    {
        event["exit_code"] = json!(exit);
    }

    append_event(&paths.progress_jsonl_for_context(&context), &event)
}
