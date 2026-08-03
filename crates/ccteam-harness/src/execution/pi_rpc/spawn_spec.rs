//! Pure Pi RPC child argv/env construction.

use std::path::{Path, PathBuf};

use crate::{PermissionMode, SpawnCtx};

pub const PI_BIN_ENV: &str = "CCTEAM_PI_BIN";

#[derive(Debug, Clone)]
pub enum PiSessionArg {
    Fresh { session_id: String },
    Resume { session_file: PathBuf },
}

#[derive(Debug, Clone)]
pub struct PiSpawnInput {
    pub session: PiSessionArg,
    pub system_prompt: Option<PathBuf>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PiSpawnSpec {
    pub bin: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

pub fn pi_bin() -> String {
    std::env::var(PI_BIN_ENV).unwrap_or_else(|_| "pi".to_string())
}

pub fn build_spawn_spec(ctx: &SpawnCtx, input: PiSpawnInput) -> PiSpawnSpec {
    let mut args = vec!["--mode".to_string(), "rpc".to_string()];
    match input.session {
        PiSessionArg::Fresh { session_id } => {
            args.push("--session-id".to_string());
            args.push(session_id);
        }
        PiSessionArg::Resume { session_file } => {
            args.push("--session".to_string());
            args.push(session_file.to_string_lossy().into_owned());
        }
    }
    args.push("--approve".to_string());
    if let Some(path) = input.system_prompt {
        args.push("--system-prompt".to_string());
        args.push(path.to_string_lossy().into_owned());
    }
    if let Some(model) = input.model.filter(|value| !value.trim().is_empty()) {
        args.push("--model".to_string());
        args.push(model);
    }
    if let Some(effort) = input.effort.filter(|value| !value.trim().is_empty()) {
        args.push("--thinking".to_string());
        args.push(effort);
    }

    PiSpawnSpec {
        bin: pi_bin(),
        args,
        env: child_env(ctx),
        cwd: ctx.cwd.clone(),
    }
}

fn child_env(ctx: &SpawnCtx) -> Vec<(String, String)> {
    let mut env = vec![
        ("CCTEAM_CHAT_SID".to_string(), ctx.sid.clone()),
        (
            "CCTEAM_PERMISSION_MODE".to_string(),
            match ctx.permission_mode {
                PermissionMode::Skip => "skip",
                PermissionMode::Hitl => "hitl",
            }
            .to_string(),
        ),
    ];
    if !ctx.secret.is_empty() {
        env.push((
            "CCTEAM_MCP_BEARER".to_string(),
            format!("ccteam-sid:{}:{}", ctx.sid, ctx.secret),
        ));
    }
    if let Ok(url) = std::env::var("CCTEAM_MCP_HTTP_URL") {
        if !url.trim().is_empty() {
            env.push(("CCTEAM_MCP_HTTP_URL".to_string(), url));
        }
    }
    env
}

pub fn deterministic_session_id(sid: &str) -> String {
    format!("ccteam-{sid}")
}

pub fn is_absolute_prompt(path: &Path) -> bool {
    path.is_absolute()
}
