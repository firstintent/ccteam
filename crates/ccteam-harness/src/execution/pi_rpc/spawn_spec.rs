//! Pure Pi RPC child argv/env construction.

use std::path::{Path, PathBuf};

use crate::execution::mcp_config::{project_bridge_child_env, SessionMcpEndpoint};
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
    pub bridge_extension: PathBuf,
    /// The ccteam MCP endpoint this session's bridge extension dials.
    /// **Required, not optional**: `ToolSurfaceMode::ManagedSessionBridge`
    /// means the bridge IS the tool surface, and the extension hard-fails on
    /// load without it — so a Pi spawn spec cannot be built without one, and
    /// the caller resolves it (or refuses the spawn) up front.
    pub mcp: SessionMcpEndpoint,
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
    if ctx.permission_mode == PermissionMode::Hitl {
        // Pi keeps explicitly supplied CLI extensions under this flag. In
        // strict HITL mode that leaves the ccteam bridge loaded while keeping
        // later user extensions from rewriting an already-approved tool call.
        args.push("--no-extensions".to_string());
    }
    args.push("-e".to_string());
    args.push(input.bridge_extension.to_string_lossy().into_owned());
    // Catalog registration is ccteam's project-trust decision. Managed Pi
    // sessions therefore bypass Pi's separate interactive trust prompt.
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
        env: child_env(ctx, &input.mcp),
        cwd: ctx.cwd.clone(),
    }
}

/// Pi's dialect of the session MCP endpoint is child env — the same projection
/// every other vendor gets as a config file or an RPC parameter. It is built
/// from the resolved endpoint, never read off the parent process: `ccteam
/// start` exports nothing, so an inherited-env bridge dies on load even on a
/// perfectly healthy default daemon.
fn child_env(ctx: &SpawnCtx, mcp: &SessionMcpEndpoint) -> Vec<(String, String)> {
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
    env.extend(project_bridge_child_env(mcp));
    env
}

pub fn deterministic_session_id(sid: &str) -> String {
    format!("ccteam-{sid}")
}

pub fn is_absolute_prompt(path: &Path) -> bool {
    path.is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::mcp_config::{BRIDGE_MCP_BEARER_ENV, BRIDGE_MCP_URL_ENV};

    fn ctx(mode: PermissionMode) -> SpawnCtx {
        SpawnCtx {
            generation: 0,
            slug: "demo".to_string(),
            sid: "s42".to_string(),
            owner: "user:web-api".into(),
            cwd: PathBuf::from("/tmp/demo"),
            project_dir: PathBuf::from("/tmp/demo"),
            secret: "sekret".to_string(),
            permission_mode: mode,
            ..SpawnCtx::default()
        }
    }

    fn spec(mode: PermissionMode) -> PiSpawnSpec {
        build_spawn_spec(
            &ctx(mode),
            PiSpawnInput {
                session: PiSessionArg::Fresh {
                    session_id: deterministic_session_id("s42"),
                },
                bridge_extension: PathBuf::from("/tmp/bridge.mjs"),
                mcp: SessionMcpEndpoint::at("http://127.0.0.1:9100/mcp", "s42", "sekret").unwrap(),
                system_prompt: None,
                model: None,
                effort: None,
            },
        )
    }

    fn env_of(spec: &PiSpawnSpec, key: &str) -> Option<String> {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// The regression this whole seam exists for: the bridge extension
    /// hard-fails on load without an endpoint, and `ccteam start` exports
    /// nothing — so an endpoint inherited from the parent process meant every
    /// `/new pi` died on a perfectly healthy default daemon. The endpoint is
    /// now an input, so a Pi spawn spec cannot exist without one.
    #[test]
    fn child_env_always_carries_the_resolved_endpoint() {
        for mode in [PermissionMode::Skip, PermissionMode::Hitl] {
            let spec = spec(mode);
            assert_eq!(
                env_of(&spec, BRIDGE_MCP_URL_ENV).as_deref(),
                Some("http://127.0.0.1:9100/mcp"),
                "bridge url must come from the endpoint, never the parent env"
            );
            assert_eq!(
                env_of(&spec, BRIDGE_MCP_BEARER_ENV).as_deref(),
                Some("ccteam-sid:s42:sekret")
            );
            assert_eq!(env_of(&spec, "CCTEAM_CHAT_SID").as_deref(), Some("s42"));
        }
    }

    #[test]
    fn permission_mode_is_projected_and_hitl_pins_extensions() {
        assert_eq!(
            env_of(&spec(PermissionMode::Skip), "CCTEAM_PERMISSION_MODE").as_deref(),
            Some("skip")
        );
        let hitl = spec(PermissionMode::Hitl);
        assert_eq!(
            env_of(&hitl, "CCTEAM_PERMISSION_MODE").as_deref(),
            Some("hitl")
        );
        assert!(hitl.args.iter().any(|a| a == "--no-extensions"));
        assert!(!spec(PermissionMode::Skip)
            .args
            .iter()
            .any(|a| a == "--no-extensions"));
    }

    /// The bearer is the session principal; it must never reach argv (process
    /// listings are world-readable).
    #[test]
    fn the_principal_never_lands_in_argv() {
        let spec = spec(PermissionMode::Skip);
        assert!(spec.args.iter().all(|a| !a.contains("sekret")));
        assert!(spec.args.iter().any(|a| a == "-e"));
    }
}
