//! V0.6.0 Wave 3 F112 — `CodexAppServerAdapter` (mode-3 codex bot path).
//!
//! Talks to `codex app-server` over a Unix Domain Socket via the
//! `thread/start`, `thread/resume`, `turn/start`, `thread/archive`
//! JSON-RPC v2-lite methods (see [`super::codex_jsonrpc`]).
//!
//! ## Lifecycle
//!
//! - `start_thread`: ensure a `codex app-server` daemon is running →
//!   connect to its UDS → `initialize` (if needed) → `thread/start`
//!   with model + cwd hint → return [`ThreadHandle`] whose `identity`
//!   carries the codex `thread_id`.
//! - `submit_turn`: `turn/start` with `[{type:"text", text:...}]`.
//! - `events`: subscribe to broadcast notifications, translate
//!   `item/*` + `turn/*` notifications → [`ThreadEvent`]. **V0.6.1 F122**:
//!   also mirror the key boundary events (`turn/completed` / `turn/failed`
//!   / `error`) into the project's `progress.jsonl` as `agent_done`
//!   entries tagged `vendor: codex` so the `cost_24h_by_vendor["codex"]`
//!   roll-up + budget cap surfaces stay live without the orchestrator
//!   needing to wire a separate poller (the V0.6.0 Wave 3 D9 retained
//!   risk).
//! - `resume_thread`: `thread/resume` with the persistent id.
//! - `close_thread`: `thread/archive` + `thread/unsubscribe` (best-effort).
//!
//! ## Socket discovery
//!
//! Default: `$CODEX_HOME/app-server-control/app-server-control.sock`
//! (CODEX_HOME falls back to `~/.codex`). Override via env
//! `CCTEAM_CODEX_APP_SERVER_SOCKET`. Tests use a tempdir socket served
//! by a hand-rolled scripted JSON-RPC peer.
//!
//! Wave 1 (V0.6) decision: mode 3 codex bot is **not** an end-user
//! configuration today (Wave 1 mode-3 ships claude-only). This adapter
//! exists so the trait stack is uniform and `/ccteam-advise` can dual-
//! probe codex without touching tmux. The orchestrator's mode-3
//! dispatch (e2e-wiring's territory) decides which adapter to mount.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::execution::codex_jsonrpc::{CodexJsonRpcClient, Notification};
use crate::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadErrorEvent, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, TurnId, TurnInput,
    UnifiedTokenUsage,
};
use crate::paths::CcteamPaths;
use crate::progress::append_event;

/// Env override for the UDS path the adapter dials. Tests set this to
/// a tempdir socket; production resolves
/// `$CODEX_HOME/app-server-control/app-server-control.sock`.
pub const APP_SERVER_SOCKET_ENV: &str = "CCTEAM_CODEX_APP_SERVER_SOCKET";

/// Env override for the codex binary used when spawning the daemon
/// (parity with `claude_bg`'s [`crate::harness::CLAUDE_BIN_ENV`]). Tests
/// point this at a fake script that creates the socket without booting
/// real codex.
pub const CODEX_BIN_ENV: &str = "CCTEAM_CODEX_BIN";

/// V0.6.1 F122 — per-thread context the adapter consults when bridging
/// boundary events into `progress.jsonl`. Populated by `start_thread`
/// from the [`SpawnCtx`]; consumed by the `events()` stream the first
/// time a notification matches the thread.
///
/// `progress_path` lands at `~/.ccteam/progress/<slug>.jsonl`
/// (resolved via [`CcteamPaths::from_env`]) so the bridge stays
/// consistent with the orchestrator's own writes. Tests inject a
/// custom ctx via [`CodexAppServerAdapter::register_bridge_for_test`].
#[derive(Debug, Clone)]
pub struct ProgressBridgeCtx {
    pub progress_path: PathBuf,
    pub role: String,
    pub sid: String,
    pub slug: String,
    pub model: Option<String>,
}

/// V0.6.0 F112 [`HarnessAdapter`] that drives mode-3 codex bot sessions
/// via `codex app-server` UDS. The adapter is stateless across threads
/// — each `start_thread` lazily connects (and caches) a client per
/// process so reused for `submit_turn` / `events` / `close_thread`.
///
/// **V0.6.1 F122**: holds an optional `bridges` map keyed by codex
/// `thread_id`. Each entry carries the project's `progress.jsonl`
/// path + role/sid/slug/model so the `events()` stream can mirror
/// `turn/completed` / `turn/failed` notifications into `agent_done`
/// rows tagged `vendor: codex`. Without an entry the stream behaves
/// exactly like V0.6.0 (translation only — no IO side effect).
#[derive(Clone, Default)]
pub struct CodexAppServerAdapter {
    inner: Arc<Mutex<Option<Arc<CodexJsonRpcClient>>>>,
    bridges: Arc<Mutex<HashMap<String, ProgressBridgeCtx>>>,
}

impl std::fmt::Debug for CodexAppServerAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexAppServerAdapter")
            .finish_non_exhaustive()
    }
}

impl CodexAppServerAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the UDS path the adapter should dial. Env override wins;
    /// otherwise `$CODEX_HOME/app-server-control/app-server-control.sock`
    /// with `CODEX_HOME` falling back to `~/.codex`.
    pub fn resolve_socket_path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(APP_SERVER_SOCKET_ENV) {
            return Some(PathBuf::from(p));
        }
        let home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))?;
        Some(
            home.join("app-server-control")
                .join("app-server-control.sock"),
        )
    }

    /// Lazily connect (or reuse) the JSON-RPC client. Spawning the
    /// daemon on demand is deliberately out-of-scope — callers (the
    /// orchestrator wiring + `ccteam doctor --check-codex-auth`) verify
    /// the daemon is running before enabling mode-3 codex. The adapter
    /// returns a clean [`HarnessError::SpawnFailed`] if the socket is
    /// missing, so the caller surfaces the right diagnostic.
    async fn client(&self) -> Result<Arc<CodexJsonRpcClient>, HarnessError> {
        let mut guard = self.inner.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(Arc::clone(c));
        }
        let path = Self::resolve_socket_path().ok_or_else(|| {
            HarnessError::SpawnFailed(
                "codex app-server socket path unresolved (set CODEX_HOME or \
                 CCTEAM_CODEX_APP_SERVER_SOCKET)"
                    .to_string(),
            )
        })?;
        let client = CodexJsonRpcClient::connect_uds(&path)
            .await
            .map_err(|err| {
                HarnessError::SpawnFailed(format!(
                    "connect codex app-server at {}: {err:#}",
                    path.display()
                ))
            })?;
        let shared = Arc::new(client);
        *guard = Some(Arc::clone(&shared));
        Ok(shared)
    }

    /// One-shot helper: drop the cached client (e.g. after detecting a
    /// dead reader task). Next call to `client()` will re-dial.
    pub async fn forget_client(&self) {
        *self.inner.lock().await = None;
    }

    /// V0.6.1 F122 — register a progress bridge for `thread_id`. Called
    /// from `start_thread` after the codex `thread/start` response lands;
    /// also exposed for tests that skip the spawn dance and want to
    /// drive the events stream directly.
    pub async fn register_bridge(&self, thread_id: String, ctx: ProgressBridgeCtx) {
        self.bridges.lock().await.insert(thread_id, ctx);
    }

    /// V0.6.1 F122 — test escape hatch. Equivalent to [`register_bridge`]
    /// but named so production call sites (orchestrator wiring) don't
    /// reach for it by accident.
    #[doc(hidden)]
    pub async fn register_bridge_for_test(&self, thread_id: String, ctx: ProgressBridgeCtx) {
        self.register_bridge(thread_id, ctx).await;
    }

    async fn bridge_for(&self, thread_id: &str) -> Option<ProgressBridgeCtx> {
        self.bridges.lock().await.get(thread_id).cloned()
    }

    async fn drop_bridge(&self, thread_id: &str) {
        self.bridges.lock().await.remove(thread_id);
    }
}

#[async_trait]
impl HarnessAdapter for CodexAppServerAdapter {
    fn name(&self) -> &'static str {
        "codex-app-server"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Codex
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        let client = self.client().await?;
        let cwd_str = ctx.cwd.to_string_lossy().to_string();
        let params = json!({
            "cwd": cwd_str,
            "session_source": "ccteam",
            "service_name": format!("ccteam/{}", ctx.slug),
            "developer_instructions": format!(
                "ccteam role: {} (slug={}, sid={})",
                spec.role, ctx.slug, ctx.sid
            ),
        });
        let result = client
            .call("thread/start", params)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("thread/start: {e:#}")))?;
        let thread_id = pluck_thread_id(&result).ok_or_else(|| {
            HarnessError::SpawnFailed(format!(
                "thread/start response missing thread.thread_id: {result}"
            ))
        })?;
        // V0.6.1 F122 — register a progress bridge so the events()
        // stream can mirror turn boundaries into progress.jsonl.
        // CcteamPaths resolution honours CCTEAM_HOME so test runs land
        // in their tempdir layout; production lands in ~/.ccteam/progress/.
        if let Ok(paths) = CcteamPaths::from_env() {
            let progress_path = paths.progress_jsonl(&ctx.slug);
            self.register_bridge(
                thread_id.clone(),
                ProgressBridgeCtx {
                    progress_path,
                    role: spec.role.clone(),
                    sid: ctx.sid.clone(),
                    slug: ctx.slug.clone(),
                    model: ctx.model_id.clone(),
                },
            )
            .await;
        }
        Ok(ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Chat,
            identity: thread_id.clone(),
            started_at: Utc::now(),
            raw_extras: json!({
                "thread_id": thread_id,
                "socket": Self::resolve_socket_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            }),
        })
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let client = self.client().await?;
        let items = turn_input_to_items(input)?;
        let params = json!({
            "thread_id": h.identity,
            "input": items,
        });
        let result = client
            .call("turn/start", params)
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("turn/start: {e:#}")))?;
        let turn_id = pluck_turn_id(&result).ok_or_else(|| {
            HarnessError::SubmitFailed(format!(
                "turn/start response missing turn.turn_id: {result}"
            ))
        })?;
        Ok(TurnId(turn_id))
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let adapter_setup = self.clone();
        let adapter_bridge = self.clone();
        let thread_id = h.identity.clone();
        // Build a futures stream by chaining: (1) one-shot setup that
        // either yields an Error event or returns a broadcast receiver,
        // then (2) the receiver-driven event flow with thread-id
        // filtering. If we can't get a client yet, surface a single
        // Error event and stop — orchestrator's progress.jsonl poller
        // remains the state-transition SoT (Wave 1 contract).
        let setup = async move {
            match adapter_setup.client().await {
                Ok(c) => Ok(c.subscribe()),
                Err(err) => Err(ThreadErrorEvent {
                    kind: "connect".into(),
                    message: err.to_string(),
                }),
            }
        };
        let s = stream::once(setup).flat_map(move |outcome| {
            let thread_id = thread_id.clone();
            let adapter_bridge = adapter_bridge.clone();
            match outcome {
                Err(err) => {
                    // F122: connect failures still bridge to progress.jsonl
                    // when a bridge ctx is registered (e.g. a test that
                    // registers manually and then drops the peer). Fire
                    // a best-effort write before yielding the Error event.
                    let adapter_for_err = adapter_bridge.clone();
                    let wanted = thread_id.clone();
                    let err_for_evt = err.clone();
                    let s = stream::once(async move {
                        if let Some(ctx) = adapter_for_err.bridge_for(&wanted).await {
                            if let Some(line) = build_progress_line(
                                &ThreadEvent::Error(err_for_evt.clone()),
                                &wanted,
                                &ctx,
                            ) {
                                let _ = append_event(&ctx.progress_path, &line);
                            }
                            adapter_for_err.drop_bridge(&wanted).await;
                        }
                        ThreadEvent::Error(err_for_evt)
                    });
                    s.boxed()
                }
                Ok(rx) => {
                    let s = stream::unfold((rx, adapter_bridge), move |(mut rx, bridge)| {
                        let wanted = thread_id.clone();
                        async move {
                            loop {
                                match rx.recv().await {
                                    Ok(notif) => {
                                        if let Some(evt) =
                                            translate_notification(&notif, &wanted)
                                        {
                                            if let Some(ctx) = bridge.bridge_for(&wanted).await {
                                                if let Some(line) =
                                                    build_progress_line(&evt, &wanted, &ctx)
                                                {
                                                    if let Err(err) = append_event(
                                                        &ctx.progress_path,
                                                        &line,
                                                    ) {
                                                        tracing::warn!(
                                                            thread_id = %wanted,
                                                            error = %err,
                                                            "codex bridge: append progress.jsonl failed"
                                                        );
                                                    }
                                                    // Terminal events: drop
                                                    // the bridge so we don't
                                                    // double-write if codex
                                                    // re-fires the same
                                                    // notification.
                                                    if is_terminal_progress(&evt) {
                                                        bridge.drop_bridge(&wanted).await;
                                                    }
                                                }
                                            }
                                            return Some((evt, (rx, bridge)));
                                        }
                                    }
                                    Err(
                                        tokio::sync::broadcast::error::RecvError::Lagged(n),
                                    ) => {
                                        tracing::warn!(
                                            n,
                                            "codex app-server event subscriber lagged"
                                        );
                                        continue;
                                    }
                                    Err(
                                        tokio::sync::broadcast::error::RecvError::Closed,
                                    ) => {
                                        return None;
                                    }
                                }
                            }
                        }
                    });
                    s.boxed()
                }
            }
        });
        Box::pin(s)
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        let client = self.client().await?;
        let result = client
            .call("thread/resume", json!({ "thread_id": persistent_id }))
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("thread/resume: {e:#}")))?;
        let thread_id = pluck_thread_id(&result).unwrap_or_else(|| persistent_id.to_string());
        Ok(ThreadHandle {
            vendor: AgentVendor::Codex,
            mode: ExecutionMode::Chat,
            identity: thread_id.clone(),
            started_at: Utc::now(),
            raw_extras: json!({ "thread_id": thread_id, "resumed": true }),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        // Best-effort archive — codex's `thread/archive` is the
        // "release server-side state" hook. Failure is logged but
        // never escalated (idempotent close semantics).
        let Ok(client) = self.client().await else {
            // No socket = nothing to close; matches V0.5.x missing-tmux
            // semantics for close_thread.
            return Ok(());
        };
        let archive = client
            .call("thread/archive", json!({ "thread_id": h.identity }))
            .await;
        if let Err(err) = archive {
            tracing::warn!(thread_id = %h.identity, error = %err, "thread/archive failed (best-effort)");
        }
        let _ = client
            .call("thread/unsubscribe", json!({ "thread_id": h.identity }))
            .await;
        Ok(())
    }
}

/// Translate a [`TurnInput`] into the codex `UserInput[]` payload
/// shape. Mirrors `references/codex/codex-rs/app-server-protocol/src/
/// protocol/v2/turn.rs::UserInput` variants.
pub fn turn_input_to_items(input: TurnInput) -> Result<Value, HarnessError> {
    let items = match input {
        TurnInput::UserText(text) => json!([{ "type": "text", "text": text }]),
        TurnInput::Artifact(path) => {
            let body = std::fs::read_to_string(&path)
                .map_err(|e| HarnessError::SubmitFailed(format!("read artifact: {e}")))?;
            json!([
                { "type": "text", "text": format!("<artifact path=\"{}\">\n{}\n</artifact>", path.display(), body) },
            ])
        }
        TurnInput::SystemDirective(d) => {
            return Err(HarnessError::SubmitFailed(format!(
                "codex app-server: SystemDirective \"{d}\" not supported \
                 (codex has no slash-command surface — use turn/start with text)"
            )))
        }
        TurnInput::Image(path) => json!([
            { "type": "localImage", "path": path.to_string_lossy() },
        ]),
        TurnInput::ToolResult { call_id, content } => json!([
            {
                "type": "text",
                "text": serde_json::to_string(&json!({ "call_id": call_id, "content": content }))
                    .unwrap_or_default(),
            }
        ]),
    };
    Ok(items)
}

/// Translate a single codex notification → [`ThreadEvent`]. Filters
/// notifications whose `params.thread_id` doesn't match `wanted`.
/// Returns `None` for notification methods we don't yet propagate
/// (e.g. `thread/status/changed` — orchestrator state polling owns
/// that today).
pub fn translate_notification(notif: &Notification, wanted: &str) -> Option<ThreadEvent> {
    // thread_id filter (some notifications carry the id, some don't —
    // we only filter when present so we don't drop turn-scoped events
    // that omit it).
    if let Some(tid) = notif.params.get("thread_id").and_then(|v| v.as_str()) {
        if tid != wanted {
            return None;
        }
    }
    match notif.method.as_str() {
        "thread/started" => Some(ThreadEvent::ThreadStarted {
            thread_id: notif
                .params
                .get("thread_id")
                .and_then(|v| v.as_str())
                .unwrap_or(wanted)
                .to_string(),
        }),
        "turn/started" => Some(ThreadEvent::TurnStarted {
            turn_id: notif
                .params
                .get("turn_id")
                .or_else(|| notif.params.get("turn").and_then(|t| t.get("turn_id")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }),
        "turn/completed" => Some(ThreadEvent::TurnCompleted {
            turn_id: notif
                .params
                .get("turn_id")
                .or_else(|| notif.params.get("turn").and_then(|t| t.get("turn_id")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            usage: pluck_usage(&notif.params).unwrap_or_default(),
        }),
        "turn/failed" => Some(ThreadEvent::TurnFailed {
            turn_id: notif
                .params
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            err: ThreadErrorEvent {
                kind: "turn_failed".into(),
                message: notif
                    .params
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no message)")
                    .to_string(),
            },
        }),
        "item/started" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemStarted { item })
        }
        "item/updated" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemUpdated { item })
        }
        "item/completed" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemCompleted { item })
        }
        "item/agentMessage/delta" => {
            let delta = notif
                .params
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let id = notif
                .params
                .get("item_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ThreadEvent::ItemUpdated {
                item: ThreadItem {
                    id,
                    details: ThreadItemDetails::AgentMessage(delta.to_string()),
                },
            })
        }
        // V0.6.3 F142 — forward-compat: a `codex app-server` notification
        // `method` we don't yet propagate is **skipped** (`None`) so the
        // event stream is never broken — the orchestrator's
        // `progress.jsonl` poller stays the state-transition SoT for
        // anything we don't translate. Warn once per unknown method so a
        // Codex app-server protocol drift surfaces in the logs.
        other => {
            crate::vendor_compat::warn_unknown_vendor_token(
                "codex_app_server_notification",
                other,
                "skipping this notification; event stream continues",
            );
            None
        }
    }
}

/// V0.6.1 F122 — translate a [`ThreadEvent`] into the `progress.jsonl`
/// row the cost/budget pipelines consume. Mirrors the orchestrator's
/// own `translate_thread_event` shape (vendor-tagged `agent_done`) so
/// `compute_cost_summary` rolls codex turns into
/// `cost_24h_by_vendor["codex"]` without any consumer-side changes.
///
/// Returns `None` for events the bridge intentionally does **not**
/// surface (`ThreadStarted` is covered by `agent_spawn`; `Item*` /
/// `TurnStarted` are presentation-only and noisy).
pub fn build_progress_line(
    evt: &ThreadEvent,
    thread_id: &str,
    ctx: &ProgressBridgeCtx,
) -> Option<Value> {
    match evt {
        ThreadEvent::TurnCompleted { turn_id, usage } => {
            let cost = ccteam_cost::estimate_cost(
                usage,
                ccteam_cost::Vendor::Codex,
                ctx.model.as_deref().unwrap_or(""),
            );
            Some(json!({
                "event": "agent_done",
                "role": ctx.role,
                "session_id": ctx.sid,
                "slug": ctx.slug,
                "status": "completed",
                "vendor": "codex",
                "cost_usd": cost,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "usage": serde_json::to_value(usage).unwrap_or(Value::Null),
                "ts": Utc::now().to_rfc3339(),
            }))
        }
        ThreadEvent::TurnFailed { turn_id, err } => Some(json!({
            "event": "agent_done",
            "role": ctx.role,
            "session_id": ctx.sid,
            "slug": ctx.slug,
            "status": "errored",
            "vendor": "codex",
            "error": err.message,
            "thread_id": thread_id,
            "turn_id": turn_id,
            "ts": Utc::now().to_rfc3339(),
        })),
        ThreadEvent::Error(err) => Some(json!({
            "event": "agent_done",
            "role": ctx.role,
            "session_id": ctx.sid,
            "slug": ctx.slug,
            "status": "errored",
            "vendor": "codex",
            "error": err.message,
            "thread_id": thread_id,
            "ts": Utc::now().to_rfc3339(),
        })),
        ThreadEvent::ThreadStarted { .. }
        | ThreadEvent::TurnStarted { .. }
        | ThreadEvent::ItemStarted { .. }
        | ThreadEvent::ItemUpdated { .. }
        | ThreadEvent::ItemCompleted { .. } => None,
    }
}

/// V0.6.1 F122 — return `true` for events that close out a thread from
/// the bridge's point of view. After a terminal write the bridge drops
/// its ctx so a duplicate `turn/completed` notification (codex
/// app-server may re-broadcast on resubscribe) doesn't double-count.
fn is_terminal_progress(evt: &ThreadEvent) -> bool {
    matches!(
        evt,
        ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. } | ThreadEvent::Error(_)
    )
}

fn translate_item_event(
    params: &Value,
    ctor: fn(ThreadItem) -> ThreadEvent,
) -> Option<ThreadEvent> {
    let item_val = params
        .get("item")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let id = item_val
        .get("id")
        .or_else(|| params.get("item_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let details = match item_val.get("type").and_then(|v| v.as_str()) {
        Some("agent_message") | Some("agentMessage") => ThreadItemDetails::AgentMessage(
            item_val
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        Some("reasoning") => ThreadItemDetails::Reasoning(
            item_val
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        Some("command_execution") | Some("commandExecution") => {
            ThreadItemDetails::CommandExecution {
                cmd: item_val
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                status: item_val
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("in_progress")
                    .to_string(),
            }
        }
        Some("file_change") | Some("fileChange") => {
            let path = item_val
                .get("changes")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("path"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_default();
            let kind = item_val
                .get("changes")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("kind"))
                .and_then(|v| v.as_str())
                .unwrap_or("update")
                .to_string();
            ThreadItemDetails::FileChange { path, kind }
        }
        Some("mcp_tool_call") | Some("mcpToolCall") => ThreadItemDetails::ToolCall {
            name: item_val
                .get("tool")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            args: item_val.get("arguments").cloned().unwrap_or(Value::Null),
        },
        Some("web_search") | Some("webSearch") => ThreadItemDetails::WebSearch {
            query: item_val
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        },
        Some("error") => ThreadItemDetails::Error(
            item_val
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ),
        // V0.6.3 F142 — forward-compat: a present-but-unrecognised item
        // `type` degrades to an empty agent message and warns once. A
        // missing `type` (`None`) is a shape gap, not a vocabulary
        // drift, so it stays silent.
        Some(other) => {
            crate::vendor_compat::warn_unknown_vendor_token(
                "codex_app_server_item",
                other,
                "degraded to empty agent message",
            );
            ThreadItemDetails::AgentMessage(String::new())
        }
        None => ThreadItemDetails::AgentMessage(String::new()),
    };
    Some(ctor(ThreadItem { id, details }))
}

fn pluck_thread_id(v: &Value) -> Option<String> {
    v.get("thread")
        .and_then(|t| t.get("thread_id"))
        .or_else(|| v.get("thread_id"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

fn pluck_turn_id(v: &Value) -> Option<String> {
    v.get("turn")
        .and_then(|t| t.get("turn_id"))
        .or_else(|| v.get("turn_id"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

fn pluck_usage(v: &Value) -> Option<UnifiedTokenUsage> {
    let raw = v
        .get("usage")
        .cloned()
        .or_else(|| v.get("turn").and_then(|t| t.get("usage")).cloned())?;
    serde_json::from_value(raw).ok()
}

/// Convenience: build a placeholder client-less adapter. Test-only;
/// production callers go through `client()` which dials the socket.
#[cfg(test)]
fn _placeholder() -> CodexAppServerAdapter {
    CodexAppServerAdapter::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolve_socket_env_override_wins() {
        std::env::set_var(APP_SERVER_SOCKET_ENV, "/tmp/ccteam-test-codex.sock");
        let p = CodexAppServerAdapter::resolve_socket_path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/ccteam-test-codex.sock"));
        std::env::remove_var(APP_SERVER_SOCKET_ENV);
    }

    #[test]
    fn turn_input_user_text_shape() {
        let v = turn_input_to_items(TurnInput::UserText("hi".into())).unwrap();
        assert_eq!(v[0]["type"], "text");
        assert_eq!(v[0]["text"], "hi");
    }

    #[test]
    fn turn_input_image_shape() {
        let v = turn_input_to_items(TurnInput::Image(PathBuf::from("/img.png"))).unwrap();
        assert_eq!(v[0]["type"], "localImage");
        assert_eq!(v[0]["path"], "/img.png");
    }

    #[test]
    fn turn_input_system_directive_rejected() {
        let err = turn_input_to_items(TurnInput::SystemDirective("/compact".into())).unwrap_err();
        assert!(matches!(err, HarnessError::SubmitFailed(_)));
    }

    #[test]
    fn translate_thread_started() {
        let n = Notification {
            method: "thread/started".into(),
            params: json!({ "thread_id": "t-1" }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ThreadStarted { thread_id } => assert_eq!(thread_id, "t-1"),
            _ => panic!("expected ThreadStarted"),
        }
    }

    #[test]
    fn translate_filters_foreign_thread() {
        let n = Notification {
            method: "turn/started".into(),
            params: json!({ "thread_id": "other", "turn_id": "x" }),
        };
        assert!(translate_notification(&n, "ours").is_none());
    }

    #[test]
    fn translate_turn_completed_extracts_usage() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cached_input_tokens": 0,
                },
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnCompleted { turn_id, usage } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            _ => panic!("expected TurnCompleted"),
        }
    }

    #[test]
    fn translate_item_completed_agent_message() {
        let n = Notification {
            method: "item/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "item": { "id": "i-1", "type": "agent_message", "text": "hello" }
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemCompleted { item } => {
                assert_eq!(item.id, "i-1");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hello"),
                    _ => panic!("expected agent_message"),
                }
            }
            _ => panic!("expected ItemCompleted"),
        }
    }

    // V0.6.3 F142 — forward-compat regression tests. Codex's app-server
    // protocol may grow a notification method or item type ccteam
    // doesn't translate; the seam must skip it (no panic, stream keeps
    // flowing) and warn once.

    #[test]
    fn translate_unknown_notification_method_is_skipped() {
        let n = Notification {
            method: "thread/checkpoint/created".into(),
            params: json!({ "thread_id": "t-1", "checkpoint_id": "c-1" }),
        };
        assert!(
            translate_notification(&n, "t-1").is_none(),
            "unknown notification method must be skipped"
        );
    }

    #[test]
    fn translate_known_notification_with_future_fields_does_not_panic() {
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    // A future usage field codex may add.
                    "speculative_tokens": 99,
                },
                // A future top-level field.
                "carbon_grams": 0.001,
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnCompleted { turn_id, usage } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(usage.input_tokens, 10);
            }
            _ => panic!("expected TurnCompleted"),
        }
    }

    #[test]
    fn translate_item_unknown_type_degrades_to_empty_message() {
        let n = Notification {
            method: "item/completed".into(),
            params: json!({
                "thread_id": "t-1",
                "item": { "id": "i-2", "type": "quantum_blob", "data": [1, 2] }
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemCompleted { item } => {
                assert_eq!(item.id, "i-2");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, ""),
                    other => panic!("expected empty agent message, got {other:?}"),
                }
            }
            _ => panic!("expected ItemCompleted"),
        }
    }
}
