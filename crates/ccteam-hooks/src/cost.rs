//! `ccteam hook cost-accumulate` — refresh state.json's
//! `context_tokens_used` after each tool round. Dollar-cost
//! computation lives in M0.14 (per development-plan §2.1); M0.3
//! tracks token totals so M0.10's 60%-of-context reset can fire.

use std::path::Path;

use anyhow::{anyhow, Result};

use ccteam_core::{slug_from_project_dir, CcteamPaths, ProjectState};

use crate::transcript::last_assistant_message;

pub fn cost_accumulate(paths: &CcteamPaths, stdin: &serde_json::Value) -> Result<()> {
    let cwd = stdin
        .get("cwd")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `cwd`"))?;
    let transcript_path = stdin
        .get("transcript_path")
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow!("hook stdin missing `transcript_path`"))?;

    let slug = slug_from_project_dir(Path::new(cwd))?;
    let state_path = paths.project_state(&slug);

    let Some(msg) = last_assistant_message(Path::new(transcript_path))? else {
        return Ok(());
    };
    let Some(usage) = msg.get("usage") else {
        return Ok(());
    };

    let input = usage
        .get("input_tokens")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let cache_create = usage
        .get("cache_creation_input_tokens")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);

    let mut state = ProjectState::load(&state_path)?;
    state.context_tokens_used = input + cache_read + cache_create;
    state.last_progress_event_at = Some(chrono::Utc::now());
    state.last_event_type = Some("PostToolUse".into());
    state.save(&state_path)?;

    Ok(())
}
