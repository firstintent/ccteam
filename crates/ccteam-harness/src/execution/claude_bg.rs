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
//!
//! ## V0.8 W3 — opt-in foreground-in-mux path
//!
//! `claude --bg` self-detaches: the launcher invocation prints the
//! `backgrounded · <id>` marker and exits sub-second while the real
//! worker daemonizes (writing its own pid to `state.json`). A mux
//! session supervising the `--bg` invocation would therefore only see
//! the launcher exit, never the worker — useless for daemon-owned
//! child supervision. See `docs/versions/v0-8-rmux/w3-mode2-bg-findings.md`
//! for the full determination + path analysis.
//!
//! The W3 deliverable is an **opt-in** foreground path, default OFF,
//! gated by `CCTEAM_CLAUDE_BG_VIA_MUX=1`. When enabled, `start_thread`
//! spawns `claude -p <prompt> --agent <role>` **in the foreground inside
//! a mux session** (`MuxSessionKind::Ephemeral`, name `ccteam-bg-<sid>`)
//! so the daemon owns the child lifecycle. The `-p`/`--print` process
//! runs the agentic loop to completion and exits, giving mux a real
//! termination signal that corresponds to agent completion (typed
//! `ProcessExited` under RmuxBackend W2; stream-end / `is_alive` poll
//! under the V0.8-default TmuxBackend). The default `--bg` path + the
//! F80 file-based poller stay untouched.

use std::process::Command;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream};

use crate::tmux_ops::session_name_for_slug;
use crate::{
    default_backend, parse_backgrounded_short_id, parse_pid_from_state, sigterm_pid,
    state_json_path, AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError,
    MuxSessionId, MuxSessionKind, MuxSessionSpec, SpawnCtx, ThreadEvent, ThreadHandle, TurnId,
    TurnInput, TurnRouting, TurnSubmission, CLAUDE_BIN_ENV,
};
use crate::{Directive, DirectiveOutcome, ThreadStatus};

/// Env flag that opts mode-2 bg spawns into the W3 foreground-in-mux
/// path. Unset / any value other than `"1"` keeps the legacy
/// self-detaching `claude --bg` direct-spawn (the V0.8 default).
pub const CLAUDE_BG_VIA_MUX_ENV: &str = "CCTEAM_CLAUDE_BG_VIA_MUX";

/// True iff `CCTEAM_CLAUDE_BG_VIA_MUX=1` is set in the environment.
fn bg_via_mux_enabled() -> bool {
    std::env::var(CLAUDE_BG_VIA_MUX_ENV)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Logical mux session name for a W3 foreground-in-mux bg spawn.
/// Distinct prefix (`ccteam-bg-`) from the `ccteam-<slug>` long-session
/// chat sessions so `list_sessions` / peek tooling can tell them apart.
/// Built directly (not via `session_name_for_slug`, which prepends its
/// own `ccteam-` prefix) so the marker prefix stays single.
fn bg_mux_session_name(ctx: &SpawnCtx) -> String {
    format!("ccteam-bg-{}-{}", ctx.slug, ctx.sid)
        .trim_end_matches('-')
        .to_string()
}

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

    /// V0.8 W3 — foreground-in-mux bg spawn (opt-in via
    /// `CCTEAM_CLAUDE_BG_VIA_MUX=1`).
    ///
    /// Spawns `claude -p <prompt-from-extra_args> --agent <role>
    /// --dangerously-skip-permissions` **in the foreground** inside a
    /// `MuxSessionKind::Ephemeral` session, so the daemon owns the child
    /// lifecycle. Unlike `--bg` (which self-detaches beyond mux's reach),
    /// `-p` runs the agentic loop to completion and exits — mux observes
    /// the real termination.
    ///
    /// The returned [`ThreadHandle`] preserves the legacy
    /// `raw_extras.tmux_session` field for downstream parity and adds
    /// `mux_session` + `via_mux: true` so callers can route teardown
    /// through `ProcessBackend::kill` (see [`Self::close_thread`]).
    async fn start_thread_via_mux(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        let bin = std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string());
        let session_name = bg_mux_session_name(ctx);
        let backend = default_backend();
        let id = MuxSessionId::new(session_name.clone());

        if backend
            .exists(&id)
            .await
            .map_err(|err| HarnessError::SpawnFailed(format!("mux exists: {err}")))?
        {
            return Err(HarnessError::SpawnFailed(format!(
                "mux bg session already exists: {session_name} (sid collision)"
            )));
        }

        // Foreground `claude -p ... --agent <role>`. The prompt + any
        // model flags travel via `ctx.extra_args` (same channel the
        // legacy `--bg` path used). `-p` is the supported non-interactive
        // surface; `--agent` is a top-level option orthogonal to it.
        let mut argv: Vec<String> = vec![
            bin,
            "-p".to_string(),
            "--agent".to_string(),
            spec.role.clone(),
            "--dangerously-skip-permissions".to_string(),
        ];
        argv.extend(ctx.extra_args.iter().cloned());

        let mux_spec = MuxSessionSpec::new(session_name.clone(), argv, ctx.cwd.clone())
            .with_kind(MuxSessionKind::Ephemeral);
        backend
            .spawn(mux_spec)
            .await
            .map_err(|err| HarnessError::SpawnFailed(format!("mux spawn bg session: {err:#}")))?;

        let pid = backend
            .pane_pid(&id)
            .await
            .ok()
            .flatten()
            .and_then(|n| u32::try_from(n).ok());

        // Legacy `tmux_session` field kept for SessionRecord parity; the
        // identity stays the mux session name so `close_thread` can route
        // teardown through the backend.
        let mut extras = serde_json::json!({
            "tmux_session": session_name.clone(),
            "mux_session": session_name.clone(),
            "via_mux": true,
        });
        if let Some(pid_val) = pid {
            extras["pid"] = serde_json::json!(pid_val);
        }

        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Bg,
            identity: session_name,
            started_at: Utc::now(),
            raw_extras: extras,
        })
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

        if bg_via_mux_enabled() {
            return self.start_thread_via_mux(spec, ctx).await;
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

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        _routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        self.submit_turn(h, input)
            .await
            .map(TurnSubmission::started)
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
        // V0.8 W3 — handles minted by the foreground-in-mux path carry
        // `via_mux: true`; route teardown through `ProcessBackend::kill`
        // (idempotent) so the daemon-owned child is reaped. The legacy
        // file-based SIGTERM path below is only for self-detached
        // `--bg` workers.
        let via_mux = h
            .raw_extras
            .get("via_mux")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if via_mux {
            let session = h
                .raw_extras
                .get("mux_session")
                .and_then(|v| v.as_str())
                .unwrap_or(h.identity.as_str());
            if session.is_empty() {
                tracing::warn!(
                    "ClaudeBgAdapter::close_thread: via_mux handle missing session; nothing to kill"
                );
                return Ok(());
            }
            let backend = default_backend();
            return backend
                .kill(&MuxSessionId::new(session.to_string()))
                .await
                .map_err(|err| HarnessError::ShutdownFailed(format!("mux kill {session}: {err}")));
        }

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

    async fn handle_directive(
        &self,
        _h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        // bg / single-turn path has no interactive command surface.
        // Explicit Rejected (a first-class answer), never Err.
        Ok(DirectiveOutcome::Rejected {
            reason: format!(
                "/{} is not available on the background (claude --bg) path",
                d.name
            ),
        })
    }

    async fn thread_status(&self, _h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        Ok(ThreadStatus::default())
    }
}
