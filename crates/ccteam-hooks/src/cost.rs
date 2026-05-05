//! `ccteam hook cost-accumulate` — refresh `state.cost_used_usd` and
//! `state.context_tokens_used` after each tool round (M0.14, tech-
//! design §6.3 + §6.8). Total cost is computed by re-summing every
//! assistant message's `usage` block in the transcript against the
//! per-model rate table. Re-scanning is cheap (sub-millisecond for
//! typical session sizes) and idempotent — running the hook twice on
//! the same transcript gives the same total.

use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::Value;

use ccteam_core::{slug_from_project_dir, CcteamPaths, ProjectState};

#[derive(Debug, Clone, Copy)]
pub struct ModelRates {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_creation_per_mtok: f64,
}

/// Approximate Anthropic API list rates (USD per million tokens) for
/// the Claude 4 family. M1+ should make these configurable via
/// `~/.ccteam/config.yml` (interfaces.md §6.3).
pub fn rate_for_model(name: &str) -> ModelRates {
    let n = name.to_ascii_lowercase();
    if n.contains("opus") {
        ModelRates {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_read_per_mtok: 1.50,
            cache_creation_per_mtok: 18.75,
        }
    } else if n.contains("haiku") {
        ModelRates {
            input_per_mtok: 0.80,
            output_per_mtok: 4.0,
            cache_read_per_mtok: 0.08,
            cache_creation_per_mtok: 1.00,
        }
    } else {
        // Sonnet rates as the conservative default for unknown names.
        ModelRates {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        }
    }
}

/// USD cost contributed by a single assistant message.
pub fn message_cost_usd(message: &Value) -> f64 {
    let Some(usage) = message.get("usage") else {
        return 0.0;
    };
    let model = message.get("model").and_then(|s| s.as_str()).unwrap_or("");
    let rates = rate_for_model(model);
    let input = u64_field(usage, "input_tokens");
    let output = u64_field(usage, "output_tokens");
    let cache_read = u64_field(usage, "cache_read_input_tokens");
    let cache_create = u64_field(usage, "cache_creation_input_tokens");
    (input as f64 * rates.input_per_mtok
        + output as f64 * rates.output_per_mtok
        + cache_read as f64 * rates.cache_read_per_mtok
        + cache_create as f64 * rates.cache_creation_per_mtok)
        / 1_000_000.0
}

/// Sum cost across every assistant message in the transcript and pick
/// up the most-recent message's input + cache_read + cache_creation
/// (used for context-reset thresholding in M0.10).
pub fn scan_transcript(path: &Path) -> Result<(f64, u64)> {
    let body = std::fs::read_to_string(path)?;
    let mut total_cost = 0.0;
    let mut latest_context_tokens = 0_u64;
    for line in body.lines() {
        let Ok(v): std::result::Result<Value, _> = serde_json::from_str(line) else {
            continue;
        };
        // Claude Code 2.x records each turn with `type: "assistant"`
        // (or `type: "user"` etc.) and the API-shaped payload nested
        // under `message`. Older prototypes used `type: "message"`.
        // We rely on `message.role` rather than the top-level `type`
        // so this handler tracks both schemas without churn.
        let Some(msg) = v.get("message") else { continue };
        if msg.get("role").and_then(|s| s.as_str()) != Some("assistant") {
            continue;
        }
        total_cost += message_cost_usd(msg);
        if let Some(usage) = msg.get("usage") {
            latest_context_tokens = u64_field(usage, "input_tokens")
                + u64_field(usage, "cache_read_input_tokens")
                + u64_field(usage, "cache_creation_input_tokens");
        }
    }
    Ok((total_cost, latest_context_tokens))
}

pub fn cost_accumulate(paths: &CcteamPaths, stdin: &Value) -> Result<()> {
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

    let (total_cost, latest_tokens) = scan_transcript(Path::new(transcript_path))?;

    let mut state = ProjectState::load(&state_path)?;
    state.cost_used_usd = total_cost;
    state.context_tokens_used = latest_tokens;
    state.last_progress_event_at = Some(chrono::Utc::now());
    state.last_event_type = Some("PostToolUse".into());
    state.save(&state_path)?;
    Ok(())
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|n| n.as_u64()).unwrap_or(0)
}
