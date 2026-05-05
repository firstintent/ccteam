//! `ccteam hook parse-phase-end` — Stop hook handler. Parses the last
//! assistant message for `PHASE_DONE: <phase>` / `ESCALATE: <reason>`
//! sigils (per `docs/tech-design.md` §4.4) and writes the matching
//! `phase_done` / `escalate` event to progress.jsonl. M0.12 layers the
//! ralph-loop block-decision behavior on top.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use ccteam_core::{slug_from_project_dir, CcteamPaths};

use crate::transcript::{last_assistant_message, message_text};

pub fn parse_phase_end(paths: &CcteamPaths, stdin: &Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let transcript_path = stdin
        .get("transcript_path")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `transcript_path`"))?;

    let slug = slug_from_project_dir(Path::new(cwd))?;
    let progress_path = paths.progress_jsonl(&slug);

    let Some(msg) = last_assistant_message(Path::new(transcript_path))? else {
        return Ok(()); // no assistant message yet
    };
    let Some(text) = message_text(&msg) else {
        return Ok(()); // assistant message had no text block
    };

    let last_line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|s| s.trim())
        .unwrap_or("");

    let event = if let Some(phase) = last_line.strip_prefix("PHASE_DONE:") {
        Some(json!({
            "ts": now_rfc3339(),
            "event": "phase_done",
            "phase": phase.trim(),
        }))
    } else {
        last_line.strip_prefix("ESCALATE:").map(|reason| {
            json!({
                "ts": now_rfc3339(),
                "event": "escalate",
                "reason": reason.trim(),
            })
        })
    };

    let Some(event) = event else { return Ok(()) };

    if let Some(parent) = progress_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let line = serde_json::to_string(&event)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&progress_path)
        .with_context(|| format!("open {}", progress_path.display()))?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
