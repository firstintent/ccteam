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
    /// Live session status (model + context-window usage) for
    /// [`HarnessAdapter::thread_status`] → IM `/sessions` + the web statusline
    /// bar. Seeded with the `initialize` model; the per-session **status tap**
    /// ([`spawn_status_tap`]) overwrites it from each `assistant`/`result`
    /// message's `usage` as turns run (interior-mutable, shared with the tap).
    status: Arc<StdMutex<ThreadStatus>>,
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

/// Spawn the per-session status tap: fold each `assistant`/`result`
/// message's token `usage` (and the assistant's live `message.model`) into
/// the shared [`ThreadStatus`], so [`HarnessAdapter::thread_status`] reports
/// the current model + context-window usage without parsing a transcript.
/// Runs for the session's whole life (ends when the transport closes). An
/// `assistant` message carries BOTH `model` and `usage`; the per-turn
/// `result` updates context against the last-known model. Reuses the single
/// compute point
/// [`context_usage_from_usage`](crate::execution::transcript_tail::context_usage_from_usage)
/// so the number matches the TUI transcript path byte-for-byte.
fn spawn_status_tap(
    transport: Arc<StreamJsonTransport>,
    status: Arc<StdMutex<ThreadStatus>>,
    project_dir: PathBuf,
    sid: String,
) {
    use crate::execution::transcript_tail::context_usage_from_usage;
    let mut sub = transport.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(Outbound::Assistant(env)) => {
                        if let Some(usage) = env.message.get("usage") {
                            let model = env.message.get("model").and_then(|v| v.as_str());
                            let ctx = context_usage_from_usage(usage, model);
                            let snapshot = if let Ok(mut s) = status.lock() {
                                if let Some(m) = model.filter(|m| !m.is_empty()) {
                                    s.model = Some(m.to_string());
                                }
                                s.context = Some(ctx);
                                Some(s.clone())
                            } else {
                                None
                            };
                            if let Some(snap) = snapshot {
                                write_status_file(&project_dir, &sid, &snap);
                            }
                        }
                    }
                    Ok(Outbound::TurnResult(r)) => {
                        if let Some(usage) = &r.usage {
                            let snapshot = if let Ok(mut s) = status.lock() {
                                let model = s.model.clone();
                                s.context =
                                    Some(context_usage_from_usage(usage, model.as_deref()));
                                Some(s.clone())
                            } else {
                                None
                            };
                            if let Some(snap) = snapshot {
                                write_status_file(&project_dir, &sid, &snap);
                            }
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

/// Persisted per-session status snapshot path, next to the turns mirror:
/// `<project_dir>/.ccteam/chat/<sid>/status.json`. ccteam-owned (no
/// Anthropic-internal dependency). Unlike the TUI adapter — which re-derives
/// status from the on-disk transcript every call — a stream-json session's
/// status lives only in the in-memory `LiveSession`, so it would vanish on
/// idle-release / daemon restart (spawn-on-demand resume). Persisting it here
/// lets [`HarnessAdapter::thread_status`] answer for a released/resumed
/// session, giving the statusline the same durability the TUI gets for free.
fn status_json_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("status.json")
}

/// Persist the latest status atomically (tmp + rename). Best-effort: a write
/// failure only means a released session can't show its statusline until its
/// next turn — never worth failing anything over.
fn write_status_file(project_dir: &Path, sid: &str, status: &ThreadStatus) {
    let path = status_json_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(body) = serde_json::to_string(status) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Read the persisted status snapshot, or `None` if absent / unreadable.
fn read_status_file(project_dir: &Path, sid: &str) -> Option<ThreadStatus> {
    let body = std::fs::read_to_string(status_json_path(project_dir, sid)).ok()?;
    serde_json::from_str(&body).ok()
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
        // Seed the live status with the `initialize` model (context unknown
        // until the first turn's `usage` lands). The status tap below keeps it
        // current; thread_status reads it.
        let status = Arc::new(StdMutex::new(ThreadStatus {
            model: init.model.clone(),
            context: None,
        }));
        // Status tap (every session, not just hitl): watch the transport for
        // `assistant`/`result` messages and fold each one's `usage` (+ live
        // `message.model`) into `status`, so /sessions + the web statusline
        // show model + context% as the session burns context.
        spawn_status_tap(
            Arc::clone(&transport),
            Arc::clone(&status),
            ctx.project_dir.clone(),
            ctx.sid.clone(),
        );
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
            status,
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
        // `/model <id>` IS driveable in stream-json — the TUI picker has no
        // headless form, but the SDK control channel does (`set_model`). Handle
        // it BEFORE the bridge gate so it never falls into a DIALOG reject or a
        // verbatim passthrough. Empty arg → a usage hint (no pane to open a
        // picker on). Real-vendor `set_model` support is confirmed at smoke; an
        // unsupported build returns an error subtype → an honest refusal here.
        if name == "model" {
            let arg = d.args.trim();
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "set_model: no live stream-json session for this handle".into(),
                ));
            };
            if arg.is_empty() {
                return Ok(DirectiveOutcome::Rejected {
                    reason:
                        "用法: /model <model-id>（stream-json 通道无交互选择器，直接给 model id）"
                            .into(),
                });
            }
            let body = live
                .transport
                .request_control("set_model", json!({ "model": arg }), init_timeout())
                .await
                .map_err(|e| HarnessError::SubmitFailed(format!("set_model 失败: {e}")))?;
            if body.subtype != "success" {
                let why = body
                    .error
                    .unwrap_or_else(|| "vendor rejected set_model".into());
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!("/model 切换失败: {why}"),
                });
            }
            // Reflect the switch in the live status immediately (model only;
            // context refreshes on the next turn's usage).
            if let Ok(mut s) = live.status.lock() {
                s.model = Some(arg.to_string());
            }
            return Ok(DirectiveOutcome::Done {
                receipt: format!("已切换 model → {arg}"),
            });
        }
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
        // Live model + context-window usage, kept current by the per-session
        // status tap ([`spawn_status_tap`]) folding each turn's `usage`. A live
        // session WITH context is authoritative; otherwise fall back to the
        // persisted snapshot ([`status_json_path`]) so a released / resumed
        // session (idle-release, daemon restart — spawn-on-demand) still shows
        // its statusline, the same durability the TUI gets from the transcript.
        let live = self
            .lookup(&h.identity)
            .map(|l| l.status.lock().unwrap().clone());
        if let Some(s) = &live {
            if s.context.is_some() {
                return Ok(s.clone());
            }
        }
        let persisted = h
            .raw_extras
            .get("project_dir")
            .and_then(|v| v.as_str())
            .zip(h.raw_extras.get("sid").and_then(|v| v.as_str()))
            .and_then(|(pd, sid)| read_status_file(Path::new(pd), sid));
        Ok(match (live, persisted) {
            // Live (model from init, no turn yet) + persisted context → show
            // the live model with the last-known context.
            (Some(l), Some(p)) => ThreadStatus {
                model: l.model.or(p.model),
                context: p.context,
            },
            (Some(l), None) => l,
            (None, Some(p)) => p,
            (None, None) => ThreadStatus::default(),
        })
    }
}
