//! `ccteam hook progress-append <event-type>` — append one event line
//! to `~/.ccteam/progress/<slug>.jsonl`.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use ccteam_core::{slug_from_project_dir, CcteamPaths};

pub fn progress_append(paths: &CcteamPaths, event_type: &str, stdin: &Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let slug = slug_from_project_dir(Path::new(cwd))?;

    let mut event = json!({
        "ts": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "event": event_type,
    });

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

    let line = serde_json::to_string(&event)? + "\n";
    let progress_path = paths.progress_jsonl(&slug);
    if let Some(parent) = progress_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&progress_path)
        .with_context(|| format!("open {}", progress_path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append to {}", progress_path.display()))?;
    Ok(())
}
