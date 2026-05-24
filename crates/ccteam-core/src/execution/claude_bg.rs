//! V0.6.0 F107 — `ClaudeBgAdapter` (replaces V0.5.x `ClaudeCodeAdapter`).
//!
//! Zero behaviour change vs. V0.5.1:
//!
//! - `start_thread` runs `claude --bg --agent <role>
//!   --dangerously-skip-permissions`, parses the `backgrounded · <id>`
//!   marker line, returns a [`ThreadHandle`] with `identity = job_id`
//!   and `raw_extras = {"tmux_session": "<logical name>"}`.
//! - `close_thread` reads `~/.claude/jobs/<job_id>/state.json` for pid,
//!   SIGTERMs it (idempotent on ESRCH or missing state.json).
//! - `events` returns an empty stream — Wave 1 keeps the existing
//!   orchestrator F80 stale-spawn-cleanup poller (`claude_job::probe_job`)
//!   as the agent_done driver. Wave 2 / Wave 3 adapters whose protocol
//!   *does* stream structured events will populate this stream, and the
//!   orchestrator will gradually retire the legacy poller (R2 SoT still
//!   `progress.jsonl`).
//! - `submit_turn` returns a synthetic `bg-<job_id>` TurnId because bg
//!   spawns are single-turn (prompt was passed via `extra_args` at
//!   `start_thread` time).
//! - `resume_thread` returns `NotImplemented` (red line R3: every spawn
//!   is a fresh 1M context for Claude vendor).

use std::process::Command;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream};

use crate::harness::{
    parse_backgrounded_short_id, parse_pid_from_state, sigterm_pid, state_json_path,
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput, CLAUDE_BIN_ENV,
};
use crate::tmux::session_name_for_slug;

/// V0.6.0 F107 [`HarnessAdapter`] for Anthropic's Claude Code bg surface.
///
/// Stateless zero-size struct — share one `'static` instance across all
/// sessions of this adapter type.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeBgAdapter;

impl ClaudeBgAdapter {
    /// Build a fresh `ClaudeBgAdapter`. Const for `static` usage.
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HarnessAdapter for ClaudeBgAdapter {
    fn name(&self) -> &'static str {
        "claude-bg"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if spec.role.is_empty() {
            return Err(HarnessError::SpawnFailed(
                "claude --bg requires a non-empty role (AgentSpecBrief::role)".into(),
            ));
        }

        let bin = std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string());

        // Move the synchronous spawn off the async runtime — `claude
        // --bg` blocks until the bg session detaches (sub-second
        // typically, but never block the executor).
        let spec_role = spec.role.clone();
        let cwd = ctx.cwd.clone();
        let extra_args = ctx.extra_args.clone();
        let bin_for_blocking = bin.clone();
        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(&bin_for_blocking);
            cmd.arg("--bg")
                .arg("--agent")
                .arg(&spec_role)
                .arg("--dangerously-skip-permissions");
            for a in &extra_args {
                cmd.arg(a);
            }
            cmd.current_dir(&cwd);
            cmd.output()
        })
        .await
        .map_err(|err| HarnessError::SpawnFailed(format!("join blocking spawn: {err}")))?
        .map_err(|err| HarnessError::SpawnFailed(format!("invoke {bin}: {err}")))?;

        if !output.status.success() {
            return Err(HarnessError::SpawnFailed(format!(
                "claude --bg exited non-zero ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let job_id = parse_backgrounded_short_id(&stdout).ok_or_else(|| {
            HarnessError::SpawnFailed(format!(
                "claude --bg stdout missing `backgrounded · <id>` line: {}",
                stdout.trim()
            ))
        })?;

        // Logical tmux session name (no real tmux session for bg; kept
        // for state.json `SessionRecord` parity).
        let tmux_session = format!("{}-{}", session_name_for_slug(&ctx.slug), ctx.sid)
            .trim_start_matches('-')
            .to_string();

        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Bg,
            identity: job_id,
            started_at: Utc::now(),
            raw_extras: serde_json::json!({
                "tmux_session": tmux_session,
            }),
        })
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        // `claude --bg` is single-turn: prompt was passed via
        // `SpawnCtx::extra_args` at `start_thread` time. We synthesize
        // a turn id keyed on the job_id so caller code can correlate.
        Ok(TurnId::new(format!("bg-{}", h.identity)))
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // Wave 1: empty stream. The orchestrator's F80 stale-spawn
        // poller (`claude_job::probe_job` against
        // `~/.claude/jobs/<id>/state.json`) remains the agent_done
        // driver. Wave 2 / Wave 3 will populate this stream and the
        // poller will be retired.
        Box::pin(stream::empty())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: "claude --bg is single-turn fresh-context every spawn (red line R3 \
                     Claude vendor); resume is intentionally unsupported"
                .to_string(),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let job_id = h.identity.as_str();
        if job_id.is_empty() {
            tracing::warn!("ClaudeBgAdapter::close_thread: empty identity; nothing to terminate");
            return Ok(());
        }
        let path = state_json_path(job_id);
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    job_id = %job_id,
                    "ClaudeBgAdapter::close_thread: state.json missing — job already cleaned up"
                );
                return Ok(());
            }
            Err(err) => {
                return Err(HarnessError::ShutdownFailed(format!(
                    "read {}: {err}",
                    path.display()
                )))
            }
        };

        let pid = match parse_pid_from_state(&raw) {
            Some(p) => p,
            None => {
                tracing::warn!(
                    job_id = %job_id,
                    "ClaudeBgAdapter::close_thread: state.json lacks pid; nothing to terminate"
                );
                return Ok(());
            }
        };

        sigterm_pid(pid)
            .map_err(|err| HarnessError::ShutdownFailed(format!("SIGTERM pid {pid}: {err}")))
    }
}
