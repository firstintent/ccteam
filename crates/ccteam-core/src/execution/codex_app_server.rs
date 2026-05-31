//! V0.6.0 Wave 3 F112 — `CodexAppServerAdapter` (mode-3 codex bot path).
//!
//! Talks to `codex app-server` over a Unix Domain Socket via the
//! `thread/start`, `thread/resume`, `turn/start`, `thread/archive`
//! JSON-RPC v2-lite methods (see [`super::codex_jsonrpc`]).
//!
//! ## Lifecycle
//!
//! - `start_thread`: ensure a `codex app-server` daemon is running →
//!   connect to its UDS → `initialize` handshake (negotiating
//!   `experimentalApi: true`) + `initialized` notification → `thread/start`
//!   with model + cwd hint → return [`ThreadHandle`] whose `identity`
//!   carries the codex `thread_id`. The handshake runs once per cached
//!   client inside `client()` (W3b catalog §7.2 — without it the server
//!   keeps `experimental_api = false` and silently filters ~30% of the
//!   notification surface, including `turn/plan/updated`).
//! - `submit_turn`: `turn/start` with `[{type:"text", text:...}]`.
//! - `events`: subscribe to broadcast notifications, translate
//!   `item/*` + `turn/*` notifications → [`ThreadEvent`]. **V0.6.1 F122**:
//!   also mirror the key boundary events (`turn/completed` + the `error`
//!   notification — the real wire name for turn failures, NOT `turn/failed`)
//!   into the project's `progress.jsonl` as `agent_done`
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

use crate::paths::CcteamPaths;
use crate::progress::append_event;
use ccteam_harness::execution::codex_jsonrpc::{CodexJsonRpcClient, Notification};
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadErrorEvent, ThreadEvent, ThreadHandle, ThreadItem, ThreadItemDetails, TurnId, TurnInput,
    UnifiedTokenUsage,
};

/// Env override for the UDS path the adapter dials. Tests set this to
/// a tempdir socket; production resolves
/// `$CODEX_HOME/app-server-control/app-server-control.sock`.
pub const APP_SERVER_SOCKET_ENV: &str = "CCTEAM_CODEX_APP_SERVER_SOCKET";

/// Env override for the codex binary used when spawning the daemon
/// (parity with `claude_bg`'s [`ccteam_harness::CLAUDE_BIN_ENV`]). Tests
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
        // W3b catalog §7.2 defect fix: complete the `initialize` handshake
        // BEFORE returning the cached client (i.e. before the first
        // `thread/start` or `events()` subscribe). The previous code dialed
        // the UDS and went straight to `thread/start`, so the server kept
        // `experimental_api = false` and silently filtered ~30% of the
        // server→client notification surface — including `turn/plan/updated`
        // (the structured plan tree V0.6.1 F124 HITL needs),
        // `thread/tokenUsage/updated`, `thread/goal/*`, and `item/plan/delta`.
        // We do this once per cached client (client() memoises), so all
        // subsequent calls reuse the negotiated capabilities.
        Self::handshake(&shared).await?;
        *guard = Some(Arc::clone(&shared));
        Ok(shared)
    }

    /// W3b catalog §4.1 — send the Codex `initialize` request (negotiating
    /// `experimentalApi: true`) followed by the one-way `initialized`
    /// notification. Mirrors the handshake the official Codex clients run:
    /// `InitializeParams { clientInfo, capabilities }` per
    /// `references/codex/codex-rs/app-server-protocol/src/protocol/v1.rs:26-56`
    /// (camelCase wire), then `ClientNotification::Initialized` →
    /// `{"method":"initialized"}` per `common.rs:1519-1521`.
    ///
    /// We opt OUT of the realtime/voice + Windows-only + admin-UI noise
    /// notifications (server-side filter is cheaper than ccteam-side) but
    /// keep every business-critical surface (turn/*, item/*, account/*).
    async fn handshake(client: &CodexJsonRpcClient) -> Result<(), HarnessError> {
        let params = json!({
            "clientInfo": {
                "name": "ccteam",
                "version": crate::VERSION,
            },
            "capabilities": {
                "experimentalApi": true,
                "requestAttestation": false,
                "optOutNotificationMethods": [
                    "thread/realtime/started",
                    "thread/realtime/itemAdded",
                    "thread/realtime/transcript/delta",
                    "thread/realtime/transcript/done",
                    "thread/realtime/outputAudio/delta",
                    "thread/realtime/sdp",
                    "thread/realtime/error",
                    "thread/realtime/closed",
                    "windows/worldWritableWarning",
                    "windowsSandbox/setupCompleted",
                    "app/list/updated",
                    "skills/changed",
                    "fuzzyFileSearch/sessionUpdated",
                    "fuzzyFileSearch/sessionCompleted",
                    "remoteControl/status/changed"
                ]
            }
        });
        client
            .call("initialize", params)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("codex initialize handshake: {e:#}")))?;
        // `initialized` is a one-way client notification (no id, no
        // response) signalling readiness to receive server-initiated
        // requests + notifications.
        client
            .notify("initialized", Value::Null)
            .await
            .map_err(|e| {
                HarnessError::SpawnFailed(format!("codex initialized notification: {e:#}"))
            })?;
        Ok(())
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
                    progress_path: progress_path.clone(),
                    role: spec.role.clone(),
                    sid: ctx.sid.clone(),
                    slug: ctx.slug.clone(),
                    model: ctx.model_id.clone(),
                },
            )
            .await;
            // V0.8 rmux Slice 4 — Codex mode-3 typed-event producer.
            // Gated on `CCTEAM_TYPED_EVENTS`; subscribes to JSON-RPC
            // notifications and writes `typed_event` rows directly to
            // the same `progress.jsonl`. Bypasses `EventMerger`
            // (no pane base side) — see module docs at
            // `execution/codex_typed_events.rs`.
            let _ = crate::execution::codex_typed_events::maybe_start_codex_typed_event_tap(
                Arc::clone(&client),
                progress_path,
            );
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
                                        // V0.8 rmux W4-fu — some Codex
                                        // notifications (plan/tokenUsage/
                                        // status/rateLimits) carry no
                                        // `ThreadEvent` variant but must
                                        // still land in progress.jsonl as
                                        // additive observability rows. They
                                        // are NOT terminal and NOT yielded
                                        // into the event stream; the bridge
                                        // mirrors them and the loop keeps
                                        // pumping the next notification.
                                        if let Some(ctx) = bridge.bridge_for(&wanted).await {
                                            if let Some(line) =
                                                build_codex_notification_progress_line(
                                                    &notif, &wanted,
                                                )
                                            {
                                                if let Err(err) =
                                                    append_event(&ctx.progress_path, &line)
                                                {
                                                    tracing::warn!(
                                                        thread_id = %wanted,
                                                        error = %err,
                                                        "codex bridge: append codex-notif progress.jsonl failed"
                                                    );
                                                }
                                            }
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
    // that omit it). The real Codex v2 wire is camelCase (`threadId`),
    // so read it dual-key — otherwise the filter never fires against a
    // live binary and foreign-thread events would be falsely accepted.
    //
    // `turn/*` + `item/*` carry a top-level `threadId` (verified
    // `references/codex/codex-rs/app-server-protocol/src/protocol/v2/turn.rs:312-315`
    // + `.../v2/item.rs:1059-1066`), so the flat lookup gates them.
    // `thread/started` is the exception: its only id is nested at
    // `params.thread.id` (`ThreadStartedNotification { thread: Thread }`,
    // `.../v2/thread.rs:1122-1124` + `.../v2/thread_data.rs:105-106`).
    // Without consulting the nested id a foreign `thread/started` slipped
    // the gate, and its arm's `unwrap_or_else(|| wanted)` laundered the
    // foreign id into the wanted slot. Resolve the nested id as a fallback
    // so the foreign-thread filter fires uniformly.
    let resolved_tid = pluck_str(&notif.params, "thread_id", "threadId")
        .map(str::to_string)
        .or_else(|| notif.params.get("thread").and_then(pluck_id));
    if let Some(tid) = resolved_tid {
        if tid != wanted {
            return None;
        }
    }
    match notif.method.as_str() {
        // Real wire: `ThreadStartedNotification { thread: Thread }`, so the
        // id is `params.thread.id` (camelCase Thread). Fall back to a flat
        // `thread_id`/`threadId` (test fixtures) and finally `wanted`. The
        // foreign-thread gate above already consulted the nested
        // `thread.id`, so a non-matching id was filtered out before reaching
        // this arm — it never launders a foreign id into the wanted slot.
        "thread/started" => Some(ThreadEvent::ThreadStarted {
            thread_id: notif
                .params
                .get("thread")
                .and_then(pluck_id)
                .or_else(|| pluck_str(&notif.params, "thread_id", "threadId").map(str::to_string))
                .unwrap_or_else(|| wanted.to_string()),
        }),
        // Real wire: `TurnStartedNotification { threadId, turn: Turn }`,
        // where the turn id is `turn.id` (NOT `turn.turn_id`, and there is
        // no top-level `turnId`). `pluck_turn_id_from_params` resolves the
        // real shape first, then the snake/camel flat fallbacks the test
        // fixtures use.
        "turn/started" => Some(ThreadEvent::TurnStarted {
            turn_id: pluck_turn_id_from_params(&notif.params),
        }),
        "turn/completed" => Some(ThreadEvent::TurnCompleted {
            turn_id: pluck_turn_id_from_params(&notif.params),
            // NOTE: the real `turn/completed` wire has NO `usage` field
            // anywhere (the `Turn` struct carries id/items/status/error/
            // timing only). Token accounting flows through the separate
            // `thread/tokenUsage/updated` notification (W4-fu bridge). This
            // lookup therefore returns `None` against a live binary →
            // default usage; it stays only to satisfy synthetic test
            // fixtures that inline `usage`. Do NOT "fix" it to read the
            // turn object — there is nothing there to read.
            usage: pluck_usage(&notif.params).unwrap_or_default(),
        }),
        // W3b catalog §8.4 defect fix: the mode-3 app-server protocol has
        // **no** `turn/failed` notification. The real wire name for a turn
        // failure is `"error"` carrying an `ErrorNotification` payload
        // (`references/codex/codex-rs/app-server-protocol/src/protocol/v2/notification.rs:41`):
        //   { error: TurnError { message, .. }, will_retry: bool,
        //     thread_id, turn_id }
        // The former `"turn/failed"` arm was dead code (the catalog notes
        // turn failures were silently routed into warn_unknown_vendor_token),
        // so terminal Codex failures never surfaced as `agent_done
        // {status:"errored"}`.
        //
        // `will_retry == true` means the app-server will transparently
        // retry the turn (a transient upstream blip) and does NOT interrupt
        // the turn — surfacing it as TurnFailed would prematurely tear down
        // the progress bridge (is_terminal_progress drops it), so a later
        // `turn/completed` would never write its `agent_done`. We therefore
        // skip retryable errors and only emit TurnFailed on terminal ones.
        "error" => {
            // Real wire `ErrorNotification { error, willRetry, threadId,
            // turnId }` — read `willRetry`/`turnId` dual-key so a live
            // codex binary's terminal failure surfaces as TurnFailed
            // (snake_case kept for the in-module test fixtures).
            let will_retry = pluck_bool(&notif.params, "will_retry", "willRetry").unwrap_or(false);
            if will_retry {
                return None;
            }
            Some(ThreadEvent::TurnFailed {
                turn_id: pluck_str(&notif.params, "turn_id", "turnId")
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
            })
        }
        "item/started" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemStarted { item })
        }
        // NOTE (W3b catalog §8.2): there is **no** `item/updated`
        // notification in the mode-3 app-server protocol — the
        // server_notification_definitions! registry at
        // `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:1425-1517`
        // splits item state changes into typed `*Delta` notifications
        // (`item/agentMessage/delta`, `item/reasoning/textDelta`, ...) +
        // `item/completed`. The dot-named `item.updated` exists only in
        // the mode-2 `codex exec --json` stream (see codex_exec.rs). The
        // former arm here was a copy-paste artefact that never fired;
        // removed so the dispatch reflects the real wire surface.
        "item/completed" => {
            translate_item_event(&notif.params, |item| ThreadEvent::ItemCompleted { item })
        }
        "item/agentMessage/delta" => {
            let delta = notif
                .params
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Real wire `AgentMessageDeltaNotification` carries `itemId`.
            let id = pluck_str(&notif.params, "item_id", "itemId")
                .unwrap_or("")
                .to_string();
            Some(ThreadEvent::ItemUpdated {
                item: ThreadItem {
                    id,
                    details: ThreadItemDetails::AgentMessage(delta.to_string()),
                },
            })
        }
        // V0.8 rmux W4-fu — these four W4-unlocked notifications carry no
        // `ThreadEvent` variant; they are mirrored to `progress.jsonl` by
        // `build_codex_notification_progress_line` in the `events()` loop.
        // Return `None` *silently* here (NOT via the unknown-method warn
        // below) — they are explicitly handled, not protocol drift, and
        // `thread/tokenUsage/updated` in particular fires several times per
        // turn, so routing them through `warn_unknown_vendor_token` would
        // spam the logs and defeat its "surface real drift" purpose.
        "turn/plan/updated"
        | "thread/tokenUsage/updated"
        | "thread/status/changed"
        | "account/rateLimits/updated" => None,
        // V0.6.3 F144 — forward-compat: a `codex app-server` notification
        // `method` we don't yet propagate is **skipped** (`None`) so the
        // event stream is never broken — the orchestrator's
        // `progress.jsonl` poller stays the state-transition SoT for
        // anything we don't translate. Warn once per unknown method so a
        // Codex app-server protocol drift surfaces in the logs.
        other => {
            ccteam_harness::warn_unknown_vendor_token(
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
    // Real wire: the id lives at `item.id` (single-word, no casing
    // issue). The flat `item_id`/`itemId` fallback only matters for
    // hand-rolled fixtures that omit the `item` wrapper.
    let id = item_val
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| pluck_str(params, "item_id", "itemId"))
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
                // Real wire: `CommandExecutionStatus` is a
                // `#[serde(rename_all = "camelCase")]` enum, so the live
                // binary sends `"inProgress"` / `"completed"` / `"failed"`
                // / `"declined"` (verified
                // `references/codex/codex-rs/app-server-protocol/src/protocol/v2/item.rs:870-878`).
                // Fold through `camel_to_snake` so progress.jsonl reads in
                // ccteam's snake_case house style (`in_progress`); the
                // single-word variants pass through unchanged, and the
                // already-snake `in_progress` default is idempotent.
                status: camel_to_snake(
                    item_val
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("in_progress"),
                ),
            }
        }
        Some("file_change") | Some("fileChange") => {
            // Real wire: `FileChange { changes: Vec<FileUpdateChange> }`
            // where `FileUpdateChange { path, kind: PatchChangeKind, diff }`
            // and `PatchChangeKind` is an INTERNALLY-TAGGED enum
            // `#[serde(tag = "type", rename_all = "camelCase")]` →
            // `{"type":"add"}` / `{"type":"delete"}` /
            // `{"type":"update","movePath":<opt>}` (verified
            // `references/codex/codex-rs/app-server-protocol/src/protocol/v2/item.rs:918-935`).
            // The prior `changes[0].kind` string read always yielded `None`
            // against the live binary (kind is an object, not a string) →
            // every patch silently defaulted to `"update"`.
            let change = item_val.get("changes").and_then(|c| c.get(0));
            let path = change
                .and_then(|c| c.get("path"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_default();
            let kind_obj = change.and_then(|c| c.get("kind"));
            // The tag field is `type`; fold through `camel_to_snake` for
            // house style. An `update` carrying a `movePath` is a rename
            // (there is no distinct `rename` variant on the wire), so
            // surface the richer `"rename"` kind in that case.
            let kind = match kind_obj
                .and_then(|k| k.get("type"))
                .and_then(|v| v.as_str())
            {
                Some("update") | Some("Update") => {
                    let has_move = kind_obj
                        .and_then(|k| k.get("movePath").or_else(|| k.get("move_path")))
                        .map(|v| !v.is_null())
                        .unwrap_or(false);
                    if has_move {
                        "rename".to_string()
                    } else {
                        "update".to_string()
                    }
                }
                Some(other) => camel_to_snake(other),
                None => "update".to_string(),
            };
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
        // V0.6.3 F144 — forward-compat: a present-but-unrecognised item
        // `type` degrades to an empty agent message and warns once. A
        // missing `type` (`None`) is a shape gap, not a vocabulary
        // drift, so it stays silent.
        Some(other) => {
            ccteam_harness::warn_unknown_vendor_token(
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

/// V0.8 rmux W4-fu — read a string field tolerating both the real
/// Codex v2 wire casing (camelCase, e.g. `threadId`) and the snake_case
/// the existing arms / test fixtures use. The real `codex app-server`
/// notifications serialize with `#[serde(rename_all = "camelCase")]`
/// (verified at `app-server-protocol/src/protocol/common.rs:2833`
/// `serialize_thread_status_changed_notification`), so a camelCase-first
/// lookup is required for the live wire while snake_case keeps the
/// in-module test fixtures consistent. Scoped to the four W4-fu arms.
fn pluck_str<'a>(params: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .and_then(|v| v.as_str())
}

/// V0.8 rmux W4-fu — read a JSON sub-value tolerating both wire casings.
fn pluck_val(params: &Value, snake: &str, camel: &str) -> Option<Value> {
    params.get(camel).or_else(|| params.get(snake)).cloned()
}

/// V0.8 rmux — bool sibling of [`pluck_str`] for the `error`
/// notification's `willRetry` (real wire) / `will_retry` (test fixture).
fn pluck_bool(params: &Value, snake: &str, camel: &str) -> Option<bool> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .and_then(|v| v.as_bool())
}

/// V0.8 rmux — pull a `*Notification`'s nested object id. The real Codex
/// v2 `Thread`/`Turn` structs name their id field plain `id` (camelCase
/// rename leaves single-word `id` untouched); older ccteam test fixtures
/// used the redundant `thread_id`/`turn_id` inside the object, so accept
/// any of the three. Used for the `thread`/`turn` sub-objects that
/// `thread/started` + `turn/*` notifications carry.
fn pluck_id(obj: &Value) -> Option<String> {
    obj.get("id")
        .or_else(|| obj.get("thread_id"))
        .or_else(|| obj.get("turn_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// V0.8 rmux — resolve a turn id from a `turn/*` notification's params.
/// Real wire: `{ threadId, turn: { id, .. } }` (id at `turn.id`). Falls
/// back to a flat `turn_id`/`turnId` for the in-module fixtures. Empty
/// string when nothing matches (preserving the prior unwrap_or("")).
fn pluck_turn_id_from_params(params: &Value) -> String {
    params
        .get("turn")
        .and_then(pluck_id)
        .or_else(|| pluck_str(params, "turn_id", "turnId").map(str::to_string))
        .unwrap_or_default()
}

/// V0.8 rmux W4-fu — translate the four Codex app-server notifications
/// that have no [`ThreadEvent`] variant (`turn/plan/updated`,
/// `thread/tokenUsage/updated`, `thread/status/changed`,
/// `account/rateLimits/updated`) into additive `progress.jsonl` rows.
/// These were silently dropped by [`translate_notification`]'s
/// forward-compat `other` arm until the W4 `initialize` handshake
/// (`experimentalApi: true`) put them on the wire.
///
/// Returns `None` for any other method (so [`translate_notification`]'s
/// own dispatch still owns the `ThreadEvent`-bearing notifications) and
/// for notifications whose thread_id doesn't match `wanted`
/// (`account/rateLimits/updated` is thread-agnostic, so it is never
/// filtered out).
///
/// IMPORTANT (semantics): `turn/plan/updated` is Codex's `update_plan`
/// todo/checklist tool — the upstream source comments
/// "`update_plan` is a todo/checklist tool; it is not related to
/// plan-mode updates"
/// (`references/codex/codex-rs/app-server/src/bespoke_event_handling.rs`,
/// `handle_turn_plan_update`). It is fire-and-forget; Codex never awaits
/// a client response. We therefore map it to the observability-only
/// `codex_plan_updated` event, NOT the F98 `plan_pending` HITL event.
pub fn build_codex_notification_progress_line(notif: &Notification, wanted: &str) -> Option<Value> {
    // thread_id filter for the thread-scoped notifications (rate-limit
    // carries no thread_id, so skip the gate when absent).
    let matches_thread = |params: &Value| -> bool {
        match pluck_str(params, "thread_id", "threadId") {
            Some(tid) => tid == wanted,
            None => true,
        }
    };

    match notif.method.as_str() {
        "turn/plan/updated" => {
            if !matches_thread(&notif.params) {
                return None;
            }
            let plan = pluck_val(&notif.params, "plan", "plan").unwrap_or(Value::Array(vec![]));
            Some(crate::progress::build_codex_plan_updated_event(
                pluck_str(&notif.params, "thread_id", "threadId").unwrap_or(wanted),
                pluck_str(&notif.params, "turn_id", "turnId").unwrap_or(""),
                pluck_str(&notif.params, "explanation", "explanation"),
                plan,
            ))
        }
        "thread/tokenUsage/updated" => {
            if !matches_thread(&notif.params) {
                return None;
            }
            let usage = pluck_val(&notif.params, "token_usage", "tokenUsage")
                .unwrap_or(Value::Object(Default::default()));
            let total = usage
                .get("total")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let last = usage
                .get("last")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let window = usage
                .get("modelContextWindow")
                .or_else(|| usage.get("model_context_window"))
                .and_then(|v| v.as_i64());
            Some(crate::progress::build_codex_token_usage_event(
                pluck_str(&notif.params, "thread_id", "threadId").unwrap_or(wanted),
                pluck_str(&notif.params, "turn_id", "turnId").unwrap_or(""),
                total,
                last,
                window,
            ))
        }
        "thread/status/changed" => {
            if !matches_thread(&notif.params) {
                return None;
            }
            let status_obj = pluck_val(&notif.params, "status", "status")
                .unwrap_or(Value::Object(Default::default()));
            // ThreadStatus is internally tagged: {"type":"idle"} /
            // {"type":"active","activeFlags":["waitingOnApproval"]}.
            let status = status_obj
                .get("type")
                .and_then(|v| v.as_str())
                .map(camel_to_snake)
                .unwrap_or_default();
            let active_flags = status_obj
                .get("activeFlags")
                .or_else(|| status_obj.get("active_flags"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| f.as_str())
                        .map(camel_to_snake)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(crate::progress::build_codex_thread_status_event(
                pluck_str(&notif.params, "thread_id", "threadId").unwrap_or(wanted),
                &status,
                active_flags,
            ))
        }
        "account/rateLimits/updated" => {
            // No thread_id on this notification — it is account-scoped.
            let snapshot = pluck_val(&notif.params, "rate_limits", "rateLimits")
                .unwrap_or(Value::Object(Default::default()));
            Some(crate::progress::build_codex_rate_limit_event(snapshot))
        }
        _ => None,
    }
}

/// V0.8 rmux W4-fu — fold a camelCase identifier to snake_case so the
/// emitted `progress.jsonl` `status` / `active_flags` values read in
/// ccteam's snake_case house style regardless of the Codex wire casing
/// (`waitingOnApproval` → `waiting_on_approval`, `systemError` →
/// `system_error`). ASCII-only; Codex status/flag tokens are all ASCII.
fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Pull the thread id from a `thread/start` / `thread/resume` response.
/// Real wire: `ThreadStartResponse { thread: Thread }` where the id is
/// `thread.id` (camelCase Thread). [`pluck_id`] tolerates the older
/// `thread.thread_id` fixture shape; the flat `thread_id`/`threadId`
/// fallbacks cover responses that inline the id.
fn pluck_thread_id(v: &Value) -> Option<String> {
    v.get("thread")
        .and_then(pluck_id)
        .or_else(|| pluck_str(v, "thread_id", "threadId").map(str::to_string))
}

/// Pull the turn id from a `turn/start` response. Real wire:
/// `TurnStartResponse { turn: Turn }` with the id at `turn.id`.
fn pluck_turn_id(v: &Value) -> Option<String> {
    v.get("turn")
        .and_then(pluck_id)
        .or_else(|| pluck_str(v, "turn_id", "turnId").map(str::to_string))
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

    // W3b catalog §8.4 defect fix — turn failures arrive as the `"error"`
    // notification (NOT a `"turn/failed"` method, which does not exist in
    // the mode-3 protocol). A terminal `error` (will_retry=false) must
    // surface as TurnFailed so the bridge writes `agent_done
    // {status:"errored"}`; a transient `error` (will_retry=true) must be
    // skipped so the progress bridge isn't torn down mid-retry.

    #[test]
    fn translate_error_notification_terminal_surfaces_turn_failed() {
        let n = Notification {
            method: "error".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "will_retry": false,
                "error": { "message": "context window exceeded" },
            }),
        };
        let e = translate_notification(&n, "t-1").expect("terminal error must surface");
        match e {
            ThreadEvent::TurnFailed { turn_id, err } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(err.message, "context window exceeded");
                assert_eq!(err.kind, "turn_failed");
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn translate_error_notification_retryable_is_skipped() {
        let n = Notification {
            method: "error".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "will_retry": true,
                "error": { "message": "transient upstream 503" },
            }),
        };
        assert!(
            translate_notification(&n, "t-1").is_none(),
            "retryable error must be skipped so the bridge survives until turn/completed"
        );
    }

    #[test]
    fn translate_legacy_turn_failed_method_is_now_unknown() {
        // The dead `turn/failed` arm was removed; the (non-existent) wire
        // name now falls through to the forward-compat skip path.
        let n = Notification {
            method: "turn/failed".into(),
            params: json!({ "thread_id": "t-1", "turn_id": "u-1" }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
    }

    #[test]
    fn translate_item_updated_method_is_now_unknown() {
        // The dead `item/updated` arm (mode-2-only wire shape) was removed;
        // it must now fall through to the forward-compat skip path, not
        // produce a ThreadEvent.
        let n = Notification {
            method: "item/updated".into(),
            params: json!({
                "thread_id": "t-1",
                "item": { "id": "i-1", "type": "agent_message", "text": "x" }
            }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
    }

    // V0.6.3 F144 — forward-compat regression tests. Codex's app-server
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

    // V0.8 rmux W4-fu — the four notifications the W4 `initialize`
    // handshake unlocked. They have no ThreadEvent variant, so
    // translate_notification still skips them (None); the additive
    // progress.jsonl rows come from build_codex_notification_progress_line.

    #[test]
    fn turn_plan_updated_maps_to_codex_plan_updated_not_plan_pending() {
        // CRITICAL: Codex's `turn/plan/updated` is its `update_plan`
        // todo/checklist tool (upstream comment: "not related to plan-mode
        // updates"); it is fire-and-forget and must NOT trigger the F98
        // `plan_pending` HITL round-trip.
        let n = Notification {
            method: "turn/plan/updated".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "explanation": "drafting",
                "plan": [
                    { "step": "read repo", "status": "completed" },
                    { "step": "write code", "status": "inProgress" },
                ],
            }),
        };
        // No ThreadEvent variant.
        assert!(translate_notification(&n, "t-1").is_none());
        let line = build_codex_notification_progress_line(&n, "t-1").expect("plan row");
        assert_eq!(line["event"], crate::progress::CODEX_PLAN_UPDATED);
        assert_ne!(line["event"], crate::progress::PLAN_PENDING);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["turn_id"], "u-1");
        assert_eq!(line["explanation"], "drafting");
        assert_eq!(line["plan"][0]["step"], "read repo");
        assert_eq!(line["plan"][1]["status"], "inProgress");
    }

    #[test]
    fn w4fu_methods_return_none_silently_not_via_unknown_warn() {
        // These four are handled by build_codex_notification_progress_line,
        // so translate_notification must skip them via the explicit no-op
        // arms (silent None), NOT the forward-compat unknown-method warn
        // path — tokenUsage especially fires many times per turn.
        for method in [
            "turn/plan/updated",
            "thread/tokenUsage/updated",
            "thread/status/changed",
            "account/rateLimits/updated",
        ] {
            let n = Notification {
                method: method.into(),
                params: json!({ "thread_id": "t-1" }),
            };
            assert!(
                translate_notification(&n, "t-1").is_none(),
                "{method} must be skipped by translate_notification"
            );
        }
    }

    #[test]
    fn turn_plan_updated_camelcase_wire_is_handled() {
        // The real Codex v2 wire serializes params in camelCase
        // (`threadId`/`turnId`), per common.rs:2833. The dual-key helper
        // must accept it.
        let n = Notification {
            method: "turn/plan/updated".into(),
            params: json!({
                "threadId": "t-9",
                "turnId": "u-9",
                "plan": [ { "step": "x", "status": "pending" } ],
            }),
        };
        let line = build_codex_notification_progress_line(&n, "t-9").expect("camel plan row");
        assert_eq!(line["event"], crate::progress::CODEX_PLAN_UPDATED);
        assert_eq!(line["thread_id"], "t-9");
        assert_eq!(line["turn_id"], "u-9");
    }

    #[test]
    fn turn_plan_updated_foreign_thread_filtered() {
        let n = Notification {
            method: "turn/plan/updated".into(),
            params: json!({ "threadId": "other", "turnId": "u", "plan": [] }),
        };
        assert!(build_codex_notification_progress_line(&n, "ours").is_none());
    }

    #[test]
    fn thread_token_usage_maps_to_codex_token_usage() {
        let n = Notification {
            method: "thread/tokenUsage/updated".into(),
            params: json!({
                "thread_id": "t-1",
                "turn_id": "u-1",
                "token_usage": {
                    "total": { "total_tokens": 300, "input_tokens": 200, "output_tokens": 100,
                               "cached_input_tokens": 0, "reasoning_output_tokens": 0 },
                    "last":  { "total_tokens": 30,  "input_tokens": 20,  "output_tokens": 10,
                               "cached_input_tokens": 0, "reasoning_output_tokens": 0 },
                    "model_context_window": 200000,
                },
            }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
        let line = build_codex_notification_progress_line(&n, "t-1").expect("usage row");
        assert_eq!(line["event"], crate::progress::CODEX_TOKEN_USAGE);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["turn_id"], "u-1");
        assert_eq!(line["total"]["total_tokens"], 300);
        assert_eq!(line["last"]["output_tokens"], 10);
        assert_eq!(line["model_context_window"], 200000);
    }

    #[test]
    fn thread_token_usage_camelcase_wire() {
        let n = Notification {
            method: "thread/tokenUsage/updated".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "tokenUsage": {
                    "total": { "total_tokens": 5 },
                    "last":  { "total_tokens": 1 },
                    "modelContextWindow": 128000,
                },
            }),
        };
        let line = build_codex_notification_progress_line(&n, "t-1").expect("usage row");
        assert_eq!(line["event"], crate::progress::CODEX_TOKEN_USAGE);
        assert_eq!(line["total"]["total_tokens"], 5);
        assert_eq!(line["model_context_window"], 128000);
    }

    #[test]
    fn thread_status_active_waiting_on_approval() {
        // Internally-tagged ThreadStatus: {"type":"active","activeFlags":[...]}.
        let n = Notification {
            method: "thread/status/changed".into(),
            params: json!({
                "threadId": "t-1",
                "status": { "type": "active", "activeFlags": ["waitingOnApproval"] },
            }),
        };
        assert!(translate_notification(&n, "t-1").is_none());
        let line = build_codex_notification_progress_line(&n, "t-1").expect("status row");
        assert_eq!(line["event"], crate::progress::CODEX_THREAD_STATUS);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["status"], "active");
        assert_eq!(line["active_flags"][0], "waiting_on_approval");
    }

    #[test]
    fn thread_status_idle_has_no_flags() {
        let n = Notification {
            method: "thread/status/changed".into(),
            params: json!({ "threadId": "t-1", "status": { "type": "idle" } }),
        };
        let line = build_codex_notification_progress_line(&n, "t-1").expect("status row");
        assert_eq!(line["status"], "idle");
        assert_eq!(line["active_flags"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn account_rate_limits_maps_to_codex_rate_limit() {
        // No thread_id — account-scoped; never filtered by thread.
        let n = Notification {
            method: "account/rateLimits/updated".into(),
            params: json!({
                "rateLimits": {
                    "primary": { "usedPercent": 80, "windowDurationMins": 60, "resetsAt": 123 },
                    "rateLimitReachedType": null,
                },
            }),
        };
        assert!(translate_notification(&n, "any-thread").is_none());
        let line =
            build_codex_notification_progress_line(&n, "any-thread").expect("rate-limit row");
        assert_eq!(line["event"], crate::progress::CODEX_RATE_LIMIT);
        assert_eq!(line["vendor"], "codex");
        assert_eq!(line["snapshot"]["primary"]["usedPercent"], 80);
    }

    #[test]
    fn camel_to_snake_folds_codex_tokens() {
        assert_eq!(camel_to_snake("waitingOnApproval"), "waiting_on_approval");
        assert_eq!(camel_to_snake("systemError"), "system_error");
        assert_eq!(camel_to_snake("idle"), "idle");
        assert_eq!(camel_to_snake("NotLoaded"), "not_loaded");
    }

    // V0.8 rmux task #18 — Codex wire camelCase sweep. The real
    // `codex app-server` v2 wire serializes every multi-word field in
    // camelCase (`#[serde(rename_all = "camelCase")]`, verified in
    // `references/codex/codex-rs/app-server-protocol/src/protocol/`).
    // The arms below previously read snake_case only and silently failed
    // against a live binary. These tests feed the REAL wire shape; the
    // pre-existing snake_case tests above still pass (dual-key).

    #[test]
    fn translate_thread_started_real_wire_nested_camel() {
        // Real wire: ThreadStartedNotification { thread: Thread { id, .. } }.
        let n = Notification {
            method: "thread/started".into(),
            params: json!({ "thread": { "id": "t-1", "sessionId": "s-1" } }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ThreadStarted { thread_id } => assert_eq!(thread_id, "t-1"),
            other => panic!("expected ThreadStarted, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_started_real_wire_turn_dot_id() {
        // Real wire: TurnStartedNotification { threadId, turn: { id, .. } }.
        // The turn id is `turn.id` — NOT `turn.turn_id`, NOT top-level.
        let n = Notification {
            method: "turn/started".into(),
            params: json!({ "threadId": "t-1", "turn": { "id": "u-7", "status": "inProgress" } }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnStarted { turn_id } => assert_eq!(turn_id, "u-7"),
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_completed_real_wire_turn_dot_id_no_usage() {
        // Real wire `turn/completed` has the id at `turn.id` and NO usage
        // field anywhere (token data flows via thread/tokenUsage/updated).
        // Must extract the id and default the usage without panicking.
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({ "threadId": "t-1", "turn": { "id": "u-9", "status": "completed" } }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::TurnCompleted { turn_id, usage } => {
                assert_eq!(turn_id, "u-9");
                // No usage on the real wire → defaulted to zero.
                assert_eq!(usage.input_tokens, 0);
                assert_eq!(usage.output_tokens, 0);
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn translate_turn_completed_real_wire_foreign_thread_filtered() {
        // camelCase threadId on a foreign thread must be filtered out —
        // proves the thread filter reads the camelCase key (regression:
        // a snake-only filter would falsely accept this).
        let n = Notification {
            method: "turn/completed".into(),
            params: json!({ "threadId": "other", "turn": { "id": "u-9" } }),
        };
        assert!(
            translate_notification(&n, "ours").is_none(),
            "foreign camelCase threadId must be filtered"
        );
    }

    #[test]
    fn translate_error_notification_camelcase_terminal_surfaces_turn_failed() {
        // THE critical case (task #18): the real wire ErrorNotification is
        // { error, willRetry, threadId, turnId } in camelCase. A terminal
        // failure (willRetry=false) MUST surface as TurnFailed so the
        // bridge writes agent_done{status:"errored"}. A snake-only
        // `will_retry` read would default to false too, but a snake-only
        // `turn_id` would lose the id — assert both.
        let n = Notification {
            method: "error".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "willRetry": false,
                "error": { "message": "context window exceeded" },
            }),
        };
        let e = translate_notification(&n, "t-1").expect("terminal camelCase error must surface");
        match e {
            ThreadEvent::TurnFailed { turn_id, err } => {
                assert_eq!(turn_id, "u-1");
                assert_eq!(err.message, "context window exceeded");
                assert_eq!(err.kind, "turn_failed");
            }
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn translate_error_notification_camelcase_retryable_is_skipped() {
        // Real wire camelCase willRetry=true must still be read as true so
        // the retryable error is skipped (bridge survives until completion).
        let n = Notification {
            method: "error".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "willRetry": true,
                "error": { "message": "transient 503" },
            }),
        };
        assert!(
            translate_notification(&n, "t-1").is_none(),
            "camelCase retryable error must be skipped"
        );
    }

    #[test]
    fn translate_agent_message_delta_camelcase_item_id() {
        // Real wire AgentMessageDeltaNotification { threadId, turnId,
        // itemId, delta }. The item id must come from `itemId`.
        let n = Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "itemId": "i-42",
                "delta": "hel",
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemUpdated { item } => {
                assert_eq!(item.id, "i-42");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hel"),
                    other => panic!("expected agent message, got {other:?}"),
                }
            }
            other => panic!("expected ItemUpdated, got {other:?}"),
        }
    }

    #[test]
    fn translate_item_completed_camelcase_type_tag() {
        // Real wire ThreadItem is #[serde(tag="type", rename_all="camelCase")]
        // → type tag `agentMessage`; id at `item.id`. Carried inside
        // ItemCompletedNotification { item, threadId, turnId }.
        let n = Notification {
            method: "item/completed".into(),
            params: json!({
                "threadId": "t-1",
                "turnId": "u-1",
                "item": { "id": "i-1", "type": "agentMessage", "text": "hello" }
            }),
        };
        let e = translate_notification(&n, "t-1").unwrap();
        match e {
            ThreadEvent::ItemCompleted { item } => {
                assert_eq!(item.id, "i-1");
                match item.details {
                    ThreadItemDetails::AgentMessage(s) => assert_eq!(s, "hello"),
                    other => panic!("expected agent message, got {other:?}"),
                }
            }
            other => panic!("expected ItemCompleted, got {other:?}"),
        }
    }

    #[test]
    fn pluck_thread_id_resolves_real_wire_thread_dot_id() {
        // thread/start response: ThreadStartResponse { thread: Thread{ id } }.
        let resp = json!({ "thread": { "id": "thr_abc", "sessionId": "s" } });
        assert_eq!(pluck_thread_id(&resp), Some("thr_abc".to_string()));
        // Older fixture shape (thread.thread_id) still works.
        let legacy = json!({ "thread": { "thread_id": "thr_legacy" } });
        assert_eq!(pluck_thread_id(&legacy), Some("thr_legacy".to_string()));
        // Flat fallbacks.
        assert_eq!(
            pluck_thread_id(&json!({ "threadId": "thr_flat" })),
            Some("thr_flat".to_string())
        );
    }

    #[test]
    fn pluck_turn_id_resolves_real_wire_turn_dot_id() {
        // turn/start response: TurnStartResponse { turn: Turn{ id } }.
        let resp = json!({ "turn": { "id": "turn_abc", "status": "inProgress" } });
        assert_eq!(pluck_turn_id(&resp), Some("turn_abc".to_string()));
        let legacy = json!({ "turn": { "turn_id": "turn_legacy" } });
        assert_eq!(pluck_turn_id(&legacy), Some("turn_legacy".to_string()));
        assert_eq!(
            pluck_turn_id(&json!({ "turnId": "turn_flat" })),
            Some("turn_flat".to_string())
        );
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
