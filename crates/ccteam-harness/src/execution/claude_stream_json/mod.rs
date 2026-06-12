//! v0.8.11 E1 — `ClaudeStreamJsonAdapter`: the Claude vendor's **second**
//! spawn path, a long-running `claude` child driven over a bidirectional
//! NDJSON (stream-json) pipe instead of a tmux PTY. It implements the same
//! [`HarnessAdapter`] trait and emits the same [`ThreadEvent`]
//! (CanonicalEvent) stream as [`super::claude_tui::ClaudeTuiAdapter`], so
//! the gateway's `spawn_event_pump` — the live daemon's only turns/progress
//! writer — consumes it **unchanged** (PRD §〇 decision 1; §七 ④ SoT writer
//! reuse).
//!
//! ## The four seams (PRD §七 ①)
//!
//! - [`spawn_spec`] — pure argv/env/cwd builder (host-portable).
//! - [`transport`] — bidirectional NDJSON over a generic `(reader, writer)`;
//!   the consumer never holds the [`tokio::process::Child`] (WS-replaceable).
//! - [`translate`] — NDJSON → [`ThreadEvent`].
//! - this module — the adapter + its live-session registry + SoT-writer
//!   reuse (the gateway pump).
//!
//! ## Red lines
//!
//! - **Zero injection**: persona only via `--agent`; [`spawn_spec`] never
//!   emits `--append-system-prompt` and this adapter never sends an
//!   `initialize.systemPrompt`.
//! - **Never kill a long session**: idle release / wake = close stdin +
//!   `--resume` (≡ resume-by-session-id); `close_thread` is the only kill
//!   path and is user-initiated. The deterministic per-(slug,sid) uuid is
//!   what makes `--resume` stateless across daemon restart.
//! - **No terminal scraping**: there is no terminal — naturally satisfied.

pub mod bridge;
pub mod protocol;
pub mod spawn_spec;
pub mod translate;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};

use crate::execution::progress_bridge::{
    append_event, build_chat_session_reset_event_with_reason, progress_jsonl_from_env,
};
use crate::execution::transcript_tail::anthropic_project_dir;
use crate::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};

use bridge::{ApprovalDecision, CanUseToolResolver, SlashClass};
use protocol::Outbound;
use spawn_spec::StreamJsonSpawnInput;
use translate::StreamTranslator;
use transport::StreamJsonTransport;

/// §七 ⑤ — host-facet-friendly session identity. `sid → vendor_uuid` is a
/// stable mapping (the uuid is derived deterministically from `(slug,
/// sid)`); `host` reserves the v0.9 host axis (`local` today; a `Sandbox
/// CR` ref later) without a one-shot re-key.
#[derive(Debug, Clone)]
pub struct SessionIdentity {
    pub sid: String,
    pub vendor_uuid: String,
    pub host: String,
}

/// One live stream-json session: the transport (owns the child privately)
/// plus the identity / routing context the adapter needs across calls.
struct LiveSession {
    identity: SessionIdentity,
    transport: Arc<StreamJsonTransport>,
    slug: String,
    role: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    /// Slash-command table from `system:init` (bridge gate, Wave 2).
    commands: Vec<String>,
    model: Option<String>,
}

/// The Claude stream-json adapter. A per-vendor singleton (mirrors
/// `CodexAppServerAdapter`) holding every live session keyed by its vendor
/// uuid. `ThreadHandle` (serializable, restart-surviving) carries only the
/// uuid + routing extras — never the live child — so a daemon restart
/// rebuilds via `--resume`.
#[derive(Clone, Default)]
pub struct ClaudeStreamJsonAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
    /// HITL resolver for `can_use_tool` reverse RPCs. `None` = no HITL
    /// wiring (a hitl session then default-denies, the safe direction).
    /// The gateway wires the production resolver (→ `permission/ask` → IM)
    /// in Wave 3; tests inject a deterministic stub.
    resolver: Option<Arc<dyn CanUseToolResolver>>,
}

impl std::fmt::Debug for ClaudeStreamJsonAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeStreamJsonAdapter")
            .finish_non_exhaustive()
    }
}

/// Adapter `name()` — the stable id used in handles, logs, and tests.
pub const STREAM_JSON_ADAPTER_NAME: &str = "claude-stream-json";

/// Spawn the per-session HITL dispatcher: watch the transport for
/// `can_use_tool` reverse RPCs, resolve each via the wired resolver, and
/// reply with a `control_response`. A missing resolver default-denies (the
/// safe direction). `deny` blocks ONLY the tool call — the turn continues.
fn spawn_hitl_dispatcher(
    transport: Arc<StreamJsonTransport>,
    sid: String,
    resolver: Option<Arc<dyn CanUseToolResolver>>,
) {
    let mut sub = transport.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(Outbound::ControlRequest(creq)) => {
                        let Some(req) = bridge::parse_can_use_tool(&creq) else { continue };
                        let decision = match &resolver {
                            Some(r) => r.resolve(&sid, &req).await,
                            None => ApprovalDecision::deny(
                                "HITL approval is unavailable (no resolver wired) — denied",
                            ),
                        };
                        let line = protocol::can_use_tool_response_line(
                            &req.request_id,
                            decision.allow,
                            &req.input,
                            &decision.message,
                        );
                        if transport.send_line(line).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
}

/// Translate one outbound message and forward its events to the stream's
/// channel. `Err(())` means the consumer dropped the stream (stop).
async fn forward(
    translator: &mut StreamTranslator,
    tx: &mpsc::Sender<ThreadEvent>,
    out: Outbound,
) -> Result<(), ()> {
    if matches!(out, Outbound::Other) {
        return Ok(());
    }
    for ev in translator.ingest(out) {
        tx.send(ev).await.map_err(|_| ())?;
    }
    Ok(())
}

/// How long to wait for `system:init` before declaring the spawn failed.
/// claude startup (incl. auth) can be slow; tests shorten it via env.
fn init_timeout() -> Duration {
    std::env::var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(30))
}

impl ClaudeStreamJsonAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the HITL `can_use_tool` resolver (gateway wiring, Wave 3).
    pub fn with_resolver(mut self, resolver: Arc<dyn CanUseToolResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    fn lookup(&self, identity: &str) -> Option<Arc<LiveSession>> {
        self.live.lock().unwrap().get(identity).cloned()
    }

    /// The slash-command table claude advertised at `system:init` for a
    /// live session (by vendor uuid / handle identity). The Wave 2 bridge
    /// gate keys known-vs-unknown commands off this; exposed now so the
    /// captured table has a reader and so tests can assert it.
    pub fn session_command_table(&self, identity: &str) -> Option<Vec<String>> {
        self.lookup(identity).map(|live| live.commands.clone())
    }

    /// The host-facet identity (`sid` / `vendor_uuid` / `host`) for a live
    /// session — the §七 ⑤ mapping record, surfaced for the gateway + tests.
    pub fn session_identity(&self, identity: &str) -> Option<SessionIdentity> {
        self.lookup(identity).map(|live| live.identity.clone())
    }

    /// True when claude has already filed a transcript jsonl for this uuid
    /// under the project's Anthropic dir — the signal to `--resume` rather
    /// than mint a fresh `--session-id`.
    fn session_jsonl_exists(cwd: &Path, uuid: &str) -> bool {
        anthropic_project_dir(cwd)
            .map(|d| d.join(format!("{uuid}.jsonl")).exists())
            .unwrap_or(false)
    }

    /// Spawn the child + perform the `initialize` handshake, shutting the
    /// transport down on any failure so a dead child never lingers.
    ///
    /// claude (stream-json) does **not** emit a `system:init` line until the
    /// first user turn, so waiting for `system:init` at spawn would hang
    /// forever (the daemon waits for init while claude waits for input). The
    /// capability handshake is the `initialize` control_request →
    /// `control_response` (what the VS Code extension / SDK do); we parse the
    /// slash-command table + model out of its response. `system:init` is still
    /// captured opportunistically by the reader when it arrives with the first
    /// turn (the bridge gate's command table is seeded from the handshake).
    async fn spawn_and_init(
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<(Arc<StreamJsonTransport>, protocol::SystemMsg), HarnessError> {
        let transport = StreamJsonTransport::connect_stdio(argv, env, cwd)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("stream-json connect: {e:#}")))?;
        match transport
            .request_control("initialize", json!({}), init_timeout())
            .await
        {
            Ok(body) if body.subtype == "success" => Ok((
                Arc::new(transport),
                protocol::SystemMsg::from_initialize(&body),
            )),
            Ok(body) => {
                transport.shutdown().await;
                Err(HarnessError::SpawnFailed(format!(
                    "stream-json initialize rejected: {}",
                    body.error.unwrap_or_else(|| body.subtype.clone())
                )))
            }
            Err(e) => {
                transport.shutdown().await;
                Err(HarnessError::SpawnFailed(format!(
                    "stream-json init handshake: {e:#}"
                )))
            }
        }
    }
}

#[async_trait]
impl HarnessAdapter for ClaudeStreamJsonAdapter {
    fn name(&self) -> &'static str {
        STREAM_JSON_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        // v0.8.11 E2 — pin-point isolate the official Telegram plugin (its
        // bot-token getUpdates poll structurally collides with ccteam's IM
        // gateway). Same managed layer the tmux path uses; only this one plugin.
        crate::execution::claude_tui::ensure_telegram_plugin_disabled(&ctx.project_dir)?;
        let bin = spawn_spec::claude_bin();
        // §七 ⑤ — stable per-(slug,sid) uuid: the stateless resume key.
        let uuid = spawn_spec::deterministic_session_uuid(&ctx.slug, &ctx.sid);
        let resume = Self::session_jsonl_exists(&ctx.cwd, &uuid);

        let make_argv = |resume: bool| {
            spawn_spec::build_argv(
                &bin,
                &StreamJsonSpawnInput {
                    role: &spec.role,
                    session_uuid: &uuid,
                    resume,
                    model_id: ctx.model_id.as_deref(),
                    permission_mode: ctx.permission_mode,
                },
            )
        };
        let env = spawn_spec::build_env(&spec.role, &ctx.slug, &ctx.secret, &ctx.sid);

        // Try the resume spawn first when a prior transcript exists; on
        // failure fall back to a fresh `--session-id` spawn and emit a
        // chat_session_reset with an explicit reason (the honest
        // context-loss signal — never silently synthesize).
        let (transport, init) = match Self::spawn_and_init(&make_argv(resume), &env, &ctx.cwd).await
        {
            Ok(ok) => ok,
            Err(resume_err) if resume => {
                tracing::warn!(
                    sid = %ctx.sid,
                    slug = %ctx.slug,
                    error = %resume_err,
                    "claude-stream-json: --resume spawn failed; falling back to fresh --session-id"
                );
                let fresh = Self::spawn_and_init(&make_argv(false), &env, &ctx.cwd).await?;
                if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
                    let ev = build_chat_session_reset_event_with_reason(
                        &spec.role,
                        &ctx.sid,
                        "resume_failed_fallback_to_fresh",
                    );
                    if let Err(err) = append_event(&progress_path, &ev) {
                        tracing::warn!(error = %err, "claude-stream-json: append reset event failed");
                    }
                }
                fresh
            }
            Err(e) => return Err(e),
        };

        let identity = SessionIdentity {
            sid: ctx.sid.clone(),
            vendor_uuid: uuid.clone(),
            host: "local".to_string(),
        };
        let model = init.model.clone();
        // HITL: only a hitl session (`--permission-prompt-tool stdio`) ever
        // receives `can_use_tool` reverse RPCs. Spawn the dispatcher that
        // resolves each via the wired resolver (→ IM approve/deny) and
        // replies with a control_response. A skip session never gets one,
        // so no dispatcher is needed.
        if ctx.permission_mode.is_hitl() {
            spawn_hitl_dispatcher(
                Arc::clone(&transport),
                ctx.sid.clone(),
                self.resolver.clone(),
            );
        }
        let live = LiveSession {
            identity: identity.clone(),
            transport,
            slug: ctx.slug.clone(),
            role: spec.role.clone(),
            project_dir: ctx.project_dir.clone(),
            cwd: ctx.cwd.clone(),
            commands: init.slash_commands.clone(),
            model,
        };
        self.live
            .lock()
            .unwrap()
            .insert(uuid.clone(), Arc::new(live));

        tracing::info!(
            event = "stream_json_started",
            sid = %ctx.sid,
            slug = %ctx.slug,
            role = %spec.role,
            vendor_uuid = %uuid,
            resumed = resume,
            "claude-stream-json: session live"
        );

        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: uuid.clone(),
            started_at: Utc::now(),
            raw_extras: json!({
                "adapter": STREAM_JSON_ADAPTER_NAME,
                "protocol": "stream-json",
                "host": identity.host,
                "vendor_uuid": uuid,
                "sid": ctx.sid,
                "slug": ctx.slug,
                "role": spec.role,
                "project_dir": ctx.project_dir.to_string_lossy(),
                "cwd": ctx.cwd.to_string_lossy(),
            }),
        })
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let Some(live) = self.lookup(&h.identity) else {
            return Err(HarnessError::SubmitFailed(format!(
                "stream-json session not live: {} (resume_thread / start_thread first)",
                h.identity
            )));
        };
        let text = match input {
            TurnInput::UserText(s) => s,
            TurnInput::Artifact(p) => {
                format!("Look at the file I just placed at {}", p.display())
            }
            TurnInput::Image(p) => {
                format!("Look at the image I just placed at {}", p.display())
            }
            TurnInput::ToolResult { call_id, content } => {
                let body = match content {
                    serde_json::Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                format!("Tool result for {call_id}: {body}")
            }
        };
        live.transport
            .send_line(protocol::user_text_line(&text))
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("stream-json send: {e:#}")))?;

        // Synthesize a turn id (the pump keys turns.jsonl off its own seq;
        // this id is only for adapter-side correlation / logs).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(TurnId::new(format!("turn-{nanos:x}")))
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let Some(live) = self.lookup(&h.identity) else {
            // No live session (resumed handle pre-spawn / unknown): empty
            // stream. The gateway resume path re-establishes via
            // start_thread, then re-subscribes.
            return Box::pin(futures::stream::empty());
        };
        let mut sub = live.transport.subscribe();
        let transport = Arc::clone(&live.transport);
        let (tx, rx) = mpsc::channel::<ThreadEvent>(64);
        tokio::spawn(async move {
            let mut translator = StreamTranslator::new();
            loop {
                tokio::select! {
                    msg = sub.recv() => match msg {
                        Ok(out) => {
                            if forward(&mut translator, &tx, out).await.is_err() {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n, "claude-stream-json: events subscriber lagged");
                        }
                        // The transport was dropped — emit the in-flight signal.
                        Err(broadcast::error::RecvError::Closed) => {
                            if let Some(ev) = translator.on_close() {
                                let _ = tx.send(ev).await;
                            }
                            return;
                        }
                    },
                    // The broadcast sender lives on the transport, so a dead
                    // child never yields `Closed` here — the explicit close
                    // signal does. Drain any buffered messages first (so a
                    // final answer emitted just before EOF isn't lost), then —
                    // if a turn was still in flight — emit the honest
                    // in-flight-loss signal before ending the stream (E3).
                    _ = transport.wait_closed() => {
                        while let Ok(out) = sub.try_recv() {
                            if forward(&mut translator, &tx, out).await.is_err() {
                                return;
                            }
                        }
                        if let Some(ev) = translator.on_close() {
                            let _ = tx.send(ev).await;
                        }
                        return;
                    }
                }
            }
        });
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        }))
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        // A live session for this uuid (idle wake within one daemon
        // lifetime) → hand back a handle pointing at it. Otherwise we
        // cannot rebuild without the SpawnCtx (cwd/role): the gateway falls
        // back to `start_thread`, which IS resume-aware (deterministic uuid
        // + jsonl-presence → `--resume`).
        if let Some(live) = self.lookup(persistent_id) {
            if live.transport.is_initialized() {
                return Ok(ThreadHandle {
                    vendor: AgentVendor::Claude,
                    mode: ExecutionMode::Chat,
                    identity: persistent_id.to_string(),
                    started_at: Utc::now(),
                    raw_extras: json!({
                        "adapter": STREAM_JSON_ADAPTER_NAME,
                        "protocol": "stream-json",
                        "host": live.identity.host,
                        "vendor_uuid": persistent_id,
                        "sid": live.identity.sid,
                        "slug": live.slug,
                        "role": live.role,
                        "project_dir": live.project_dir.to_string_lossy(),
                        "cwd": live.cwd.to_string_lossy(),
                    }),
                });
            }
        }
        Err(HarnessError::NotImplemented {
            reason: format!(
                "stream-json resume of {persistent_id} needs the SpawnCtx; \
                 caller must invoke start_thread (resume-aware via the \
                 deterministic per-sid uuid + --resume)"
            ),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let live = self.live.lock().unwrap().remove(&h.identity);
        if let Some(live) = live {
            live.transport.shutdown().await;
        }
        Ok(())
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        // Bridge gate (PRD E1): classify against the live init command
        // table. ccteam's own IM commands never reach here — the gateway
        // intercepts them before `handle_directive`.
        let commands = self
            .lookup(&h.identity)
            .map(|live| live.commands.clone())
            .unwrap_or_default();
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();
        match bridge::classify_slash(&name, &commands) {
            SlashClass::Reject => Ok(DirectiveOutcome::Rejected {
                reason: bridge::reject_reason(&name),
            }),
            SlashClass::Passthrough => {
                // Known prompt/local (incl. /compact /clear /context) OR
                // unknown → forward verbatim as user text.
                let line = if d.args.trim().is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {}", d.args.trim())
                };
                let turn = self.submit_turn(h, TurnInput::UserText(line)).await?;
                Ok(DirectiveOutcome::Turn(turn))
            }
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // Wave 1 — model from init; context-window accounting (from the
        // `result.usage` stream) is a Wave 2 enhancement. Default
        // (statusless) is a valid answer for an unknown handle.
        Ok(self
            .lookup(&h.identity)
            .map(|live| ThreadStatus {
                model: live.model.clone(),
                context: None,
            })
            .unwrap_or_default())
    }
}
