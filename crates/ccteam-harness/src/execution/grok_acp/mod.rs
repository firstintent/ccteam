//! Grok Build ACP adapter — third harness vendor (`AgentVendor::Grok`).
//!
//! Topology (Claude stream-json): **1 live session = 1 `grok agent stdio` child**.
//! Wire transport (Codex jsonrpc style): line-delimited JSON-RPC **2.0**.
//!
//! Wire SoT: `docs-local/versions/v0-8-23/dev-plan.md` §11 (grok 0.2.93).

pub mod bridge;
pub mod protocol;
pub mod spawn_spec;
pub mod translate;
pub mod transport;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::{
    AgentSpecBrief, AgentVendor, ChoicePrompt, ContextUsage, Directive, DirectiveOutcome,
    ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus,
    TurnId, TurnInput, TurnRouting, TurnSubmission,
};

use crate::execution::acp::{
    route_acp_turn, AcpTurnRoute, AcpTurnRunner, AcpTurnTuning, JsonRpcError,
};
use crate::execution::claude_common::unique_prompt_token;
use crate::execution::session_meta::read_session_meta;
use protocol::{
    acp_model_picker_options, known_efforts, pluck_model_info, pluck_session_id,
    split_trailing_effort, AcpModelOption, ModelInfo,
};
use spawn_spec::{build_argv, build_envs, grok_bin, GrokSpawnInput};
use translate::{apply_notification, SessionTranslateState};
use transport::{AcpTransport, InboundPolicy};

/// Max wait for the dispatcher to reach the turn boundary after the prompt
/// response, before finalizing anyway (best-effort if `turn_completed` is
/// ever absent). The boundary is normally already signalled by then.
const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);

/// Adapter name — stable id for handles / logs / tests.
pub const GROK_ACP_ADAPTER_NAME: &str = "grok-acp";

const EVENT_BUFFER: usize = 256;

/// Grok's ACP extension for a no-cancel, same-turn user-message interjection.
/// The leading underscore is the wire-level ACP extension prefix; Grok's
/// internal method name is `x.ai/interject`.
const GROK_INTERJECT_METHOD: &str = "_x.ai/interject";

struct LiveSession {
    transport: Arc<AcpTransport>,
    session_id: String,
    slug: String,
    sid: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    /// Vendor catalog from `session/new|load` `availableModels` — drives `/model`.
    available_models: Vec<AcpModelOption>,
    state: Arc<StdMutex<SessionTranslateState>>,
    /// Tokio's mutex is FIFO: concurrent messages enter the native
    /// interjection channel in adapter arrival order.
    interjection_order: tokio::sync::Mutex<()>,
    event_tx: broadcast::Sender<ThreadEvent>,
    _dispatcher: tokio::task::JoinHandle<()>,
}

fn acp_choice_prompt(title: &str, options: Vec<crate::ChoiceOption>) -> ChoicePrompt {
    ChoicePrompt {
        token: unique_prompt_token("gm"),
        title: title.to_string(),
        options,
        multi: false,
    }
}

/// Per-process singleton holding live Grok ACP sessions keyed by ACP sessionId.
#[derive(Clone, Default)]
pub struct GrokAcpAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
}

impl std::fmt::Debug for GrokAcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokAcpAdapter").finish_non_exhaustive()
    }
}

impl GrokAcpAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn crate_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn handshake_and_new(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        mcp_servers: Vec<Value>,
    ) -> Result<(String, ModelInfo), HarnessError> {
        let _init = transport
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false
                    },
                    "clientInfo": {
                        "name": "ccteam",
                        "version": Self::crate_version()
                    }
                }),
            )
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("grok initialize failed: {e}")))?;

        transport
            .notify("notifications/initialized", Value::Null)
            .await
            .map_err(|e| {
                HarnessError::SpawnFailed(format!("grok notifications/initialized failed: {e}"))
            })?;

        // v0.9.0 W1 (G2) — wire the ccteam MCP tool face onto session/new
        // (was hardcoded `[]`, so grok children had no ccteam tools).
        let new_result = transport
            .call(
                "session/new",
                json!({
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": mcp_servers
                }),
            )
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("grok session/new failed: {e}")))?;

        let session_id = pluck_session_id(&new_result).ok_or_else(|| {
            HarnessError::SpawnFailed("grok session/new missing sessionId".into())
        })?;
        Ok((session_id, pluck_model_info(&new_result)))
    }

    async fn handshake_and_load(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        session_id: &str,
        mcp_servers: Vec<Value>,
    ) -> Result<ModelInfo, HarnessError> {
        let _init = transport
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false
                    },
                    "clientInfo": {
                        "name": "ccteam",
                        "version": Self::crate_version()
                    }
                }),
            )
            .await
            .map_err(|e| {
                HarnessError::SpawnFailed(format!("grok initialize (resume) failed: {e}"))
            })?;

        transport
            .notify("notifications/initialized", Value::Null)
            .await
            .map_err(|e| {
                HarnessError::SpawnFailed(format!(
                    "grok notifications/initialized (resume) failed: {e}"
                ))
            })?;

        // v0.9.0 W1 (G2) — resume/load carries the SAME mcpServers as fresh so
        // a cold-resumed grok child keeps the ccteam tool face.
        let load_result = transport
            .call(
                "session/load",
                json!({
                    "sessionId": session_id,
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": mcp_servers
                }),
            )
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("grok session/load failed: {e}")))?;
        Ok(pluck_model_info(&load_result))
    }

    #[allow(clippy::too_many_arguments)]
    fn register_live(
        &self,
        transport: Arc<AcpTransport>,
        session_id: String,
        slug: String,
        sid: String,
        project_dir: PathBuf,
        cwd: PathBuf,
        info: ModelInfo,
    ) -> Arc<LiveSession> {
        crate::model_catalog::record_vendor_models_best_effort(
            "grok",
            "ACP session availableModels",
            info.available
                .iter()
                .filter(|model| !model.model_id.trim().is_empty())
                .map(|model| crate::model_catalog::CatalogModel {
                    id: model.model_id.clone(),
                    display_name: (!model.name.trim().is_empty()).then(|| model.name.clone()),
                    efforts: model.efforts.clone(),
                })
                .collect(),
        );
        let state = Arc::new(StdMutex::new(SessionTranslateState {
            model: info.model,
            window_tokens: info.window,
            effort: info.effort,
            capture_vendor_started_turns: true,
            ..Default::default()
        }));
        let (event_tx, _) = broadcast::channel(EVENT_BUFFER);
        let dispatcher =
            spawn_notif_dispatcher(Arc::clone(&transport), Arc::clone(&state), event_tx.clone());
        let live = Arc::new(LiveSession {
            transport,
            session_id: session_id.clone(),
            slug,
            sid,
            project_dir,
            cwd,
            available_models: info.available,
            state,
            interjection_order: tokio::sync::Mutex::new(()),
            event_tx,
            _dispatcher: dispatcher,
        });
        if let Ok(mut map) = self.live.lock() {
            map.insert(session_id, Arc::clone(&live));
        }
        live
    }

    fn get_live(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        self.live
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned())
    }

    fn make_handle(live: &LiveSession) -> ThreadHandle {
        ThreadHandle {
            identity: live.session_id.clone(),
            vendor: AgentVendor::Grok,
            mode: ExecutionMode::Chat,
            started_at: Utc::now(),
            raw_extras: json!({
                // vendor_uuid is what gateway meta.json + resume paths read.
                "vendor_uuid": live.session_id,
                "sessionId": live.session_id,
                "slug": live.slug,
                "sid": live.sid,
                "project_dir": live.project_dir,
                "cwd": live.cwd,
                "protocol": "acp",
                "adapter": GROK_ACP_ADAPTER_NAME,
            }),
        }
    }

    fn thread_status_inner(&self, live: &LiveSession) -> ThreadStatus {
        let Ok(st) = live.state.lock() else {
            return ThreadStatus::default();
        };
        ThreadStatus {
            model: st.model.clone(),
            context: match (st.used_tokens, st.window_tokens) {
                (Some(used), Some(window)) => Some(ContextUsage {
                    used_tokens: used,
                    window_tokens: window,
                }),
                (None, Some(window)) => Some(ContextUsage {
                    used_tokens: 0,
                    window_tokens: window,
                }),
                _ => None,
            },
            effort: st.effort.clone(),
            goal: None,
        }
    }

    async fn submit_with_routing(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        let live = self.get_live(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("grok session {} not live", h.identity))
        })?;

        let text = match input {
            TurnInput::UserText(t) => t,
            other => {
                return Err(HarnessError::SubmitFailed(format!(
                    "grok_acp: unsupported turn input {other:?}"
                )));
            }
        };

        let route = {
            let mut state = live
                .state
                .lock()
                .map_err(|_| HarnessError::Io("grok state lock poisoned".into()))?;
            route_acp_turn(&mut state, &text, routing, true)
        };

        match route {
            AcpTurnRoute::Start {
                turn_id,
                turn_done,
                prompt_sent,
            } => {
                AcpTurnRunner {
                    transport: Arc::clone(&live.transport),
                    state: Arc::clone(&live.state),
                    event_tx: live.event_tx.clone(),
                    session_id: live.session_id.clone(),
                    tuning: AcpTurnTuning {
                        finalize_barrier: FINALIZE_BARRIER,
                        post_finalize_sleep: None,
                        label: "grok",
                    },
                }
                .spawn(turn_id.clone(), turn_done, prompt_sent, text);
                Ok(TurnSubmission::started(TurnId(turn_id)))
            }
            AcpTurnRoute::Queue { turn_id, .. } => Ok(TurnSubmission::queued(TurnId(turn_id))),
            AcpTurnRoute::Inject {
                active_turn_id,
                prompt_sent,
                reservation,
            } => {
                let _ordered = live.interjection_order.lock().await;
                // The first submit returns before its runner necessarily gets a
                // CPU slice. Waiting here guarantees `session/prompt` entered
                // the transport FIFO before `_x.ai/interject`, so a back-to-
                // back message cannot race into Grok's "no active turn" arm.
                if let Some(sent) = prompt_sent {
                    sent.wait().await.map_err(|e| {
                        HarnessError::SubmitFailed(format!(
                            "grok {GROK_INTERJECT_METHOD} prompt barrier: {e:#}"
                        ))
                    })?;
                }
                let response = live
                    .transport
                    .call(
                        GROK_INTERJECT_METHOD,
                        json!({
                            "sessionId": live.session_id,
                            "text": text,
                        }),
                    )
                    .await;
                let response =
                    match response {
                        Ok(response) => response,
                        Err(error) if grok_interject_missed_active_turn(&error) => {
                            // Defensive compatibility for a Grok build that
                            // explicitly rejects an idle interject. Current Grok
                            // instead admits it and self-starts a turn; the shared
                            // ACP translator captures that normal path.
                            let queued_id = {
                                let mut state = live.state.lock().map_err(|_| {
                                    HarnessError::Io("grok state lock poisoned".into())
                                })?;
                                match route_acp_turn(&mut state, &text, TurnRouting::Queue, true) {
                                    AcpTurnRoute::Queue { turn_id, .. } => turn_id,
                                    _ => {
                                        return Err(HarnessError::Io(
                                            "grok late interject did not queue".into(),
                                        ))
                                    }
                                }
                            };
                            tracing::debug!(
                                turn_id = %queued_id,
                                error = %error,
                                "grok ACP late interjection queued as follow-up"
                            );
                            return Ok(TurnSubmission::queued(TurnId(queued_id))
                                .hold_completion(reservation));
                        }
                        Err(error) => {
                            return Err(HarnessError::SubmitFailed(format!(
                                "grok {GROK_INTERJECT_METHOD}: {error}"
                            )))
                        }
                    };
                if !grok_interject_was_admitted(&response) {
                    return Err(HarnessError::SubmitFailed(format!(
                        "grok {GROK_INTERJECT_METHOD}: unexpected response {response}"
                    )));
                }
                tracing::debug!(
                    turn_id = %active_turn_id,
                    response = %response,
                    "grok ACP message injected into active turn"
                );
                Ok(TurnSubmission::injected(TurnId(active_turn_id)).hold_completion(reservation))
            }
        }
    }
}

fn grok_interject_missed_active_turn(error: &anyhow::Error) -> bool {
    error.downcast_ref::<JsonRpcError>().is_some_and(|rpc| {
        if rpc.code != Some(-32000) {
            return false;
        }
        let message = rpc.message.to_ascii_lowercase();
        message.contains("no active turn")
            || message.contains("turn is not active")
            || message.contains("no turn in progress")
    })
}

fn grok_interject_was_admitted(response: &Value) -> bool {
    response
        .pointer("/result/status")
        .or_else(|| response.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("queued"))
}

fn spawn_notif_dispatcher(
    transport: Arc<AcpTransport>,
    state: Arc<StdMutex<SessionTranslateState>>,
    event_tx: broadcast::Sender<ThreadEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut sub = transport.subscribe();
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(n) => {
                        let events = if let Ok(mut guard) = state.lock() {
                            apply_notification(&mut guard, &n)
                        } else {
                            Vec::new()
                        };
                        for ev in events {
                            let _ = event_tx.send(ev);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    })
}

#[async_trait]
impl HarnessAdapter for GrokAcpAdapter {
    fn name(&self) -> &'static str {
        GROK_ACP_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Grok
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        // v0.9.0 W3 (F3) — remote execution is claude-only in this version;
        // see `codex_app_server.rs`'s identical guard for the rationale.
        if ctx.remote.is_some() {
            return Err(HarnessError::NotImplemented {
                reason: "remote execution (host != local) is not yet supported for grok; \
                         use host=local"
                    .to_string(),
            });
        }
        // MVP roleless: ignore role (no systemPromptOverride / no --agent-profile).
        let bin = grok_bin();
        let argv = build_argv(
            &bin,
            &GrokSpawnInput {
                permission_mode: ctx.permission_mode,
                model_id: ctx.model_id.as_deref(),
            },
        );
        let program = argv[0].clone();
        let args: Vec<String> = argv.into_iter().skip(1).collect();
        // Child-only env: kill Grok's Claude MCP compat scan so the managed
        // session doesn't import ccteam's global stdio `mcp-serve` entry on
        // top of the ACP-injected HTTP server (see `spawn_spec::build_envs`).
        let envs = build_envs();
        let cwd = if ctx.cwd.as_os_str().is_empty() {
            ctx.project_dir.clone()
        } else {
            ctx.cwd.clone()
        };
        // v0.9.0 W1 (G2) — the ccteam MCP tool face (HTTP + session bearer),
        // passed identically to session/new and session/load (shared ACP
        // helper). Empty when sid/secret missing (roleless still gets tools;
        // secret is the gate). `ctx.secret` was previously dropped.
        let mcp_servers = crate::execution::mcp_config::acp_mcp_servers_http(&ctx.sid, &ctx.secret);

        // Cold-resume ladder: if meta.json already has a Grok ACP sessionId
        // (vendor_uuid), `session/load` instead of `session/new` so daemon
        // rebuild / `/use` keep conversation context (isReplay filtered).
        let prior_uuid = read_session_meta(&ctx.project_dir, &ctx.sid)
            .ok()
            .map(|m| m.vendor_uuid)
            .filter(|u| !u.trim().is_empty());

        if let Some(ref uuid) = prior_uuid {
            if let Some(live) = self.get_live(uuid) {
                return Ok(Self::make_handle(&live));
            }
        }

        let try_load = prior_uuid.clone();
        let (transport, session_id, info) = match try_load {
            Some(uuid) => {
                let transport = AcpTransport::spawn_command_full(
                    &program,
                    &args,
                    &cwd,
                    &envs,
                    InboundPolicy::DefaultDecline,
                )
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("spawn grok agent stdio: {e}")))?;
                let transport = Arc::new(transport);
                match Self::handshake_and_load(&transport, &cwd, &uuid, mcp_servers.clone()).await {
                    Ok(info) => (transport, uuid, info),
                    Err(load_err) => {
                        tracing::warn!(
                            error = %load_err,
                            "grok session/load failed; falling back to session/new"
                        );
                        let _ = transport.shutdown().await;
                        let transport = AcpTransport::spawn_command_full(
                            &program,
                            &args,
                            &cwd,
                            &envs,
                            InboundPolicy::DefaultDecline,
                        )
                        .await
                        .map_err(|e| {
                            HarnessError::SpawnFailed(format!("spawn grok after load fail: {e}"))
                        })?;
                        let transport = Arc::new(transport);
                        let (sid, info) =
                            Self::handshake_and_new(&transport, &cwd, mcp_servers.clone()).await?;
                        (transport, sid, info)
                    }
                }
            }
            None => {
                let transport = AcpTransport::spawn_command_full(
                    &program,
                    &args,
                    &cwd,
                    &envs,
                    InboundPolicy::DefaultDecline,
                )
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("spawn grok agent stdio: {e}")))?;
                let transport = Arc::new(transport);
                let (sid, info) =
                    Self::handshake_and_new(&transport, &cwd, mcp_servers.clone()).await?;
                (transport, sid, info)
            }
        };

        let live = self.register_live(
            transport,
            session_id,
            ctx.slug.clone(),
            ctx.sid.clone(),
            ctx.project_dir.clone(),
            cwd,
            info,
        );
        let mut handle = Self::make_handle(&live);
        if let Ok(st) = live.state.lock() {
            if let Some(m) = &st.model {
                handle.raw_extras["model"] = json!(m);
            }
        }
        Ok(handle)
    }

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        self.submit_with_routing(h, input, routing).await
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let Some(live) = self.get_live(&h.identity) else {
            return stream::empty().boxed();
        };
        let rx = live.event_tx.subscribe();
        stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => return Some((ev, rx)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed()
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        if let Some(live) = self.get_live(persistent_id) {
            return Ok(Self::make_handle(&live));
        }
        // Cold resume needs the project cwd, which this bare-id entrypoint
        // lacks. The daemon rebuild path (`rebuild_session_from_meta`) instead
        // calls `start_thread` with the same sid, which reads meta.vendor_uuid
        // and runs the `session/load` ladder — that is the working cold-resume
        // route for Grok. Fail loudly here so nothing silently relies on it.
        Err(HarnessError::NotImplemented {
            reason: format!(
                "grok cold resume of {persistent_id} needs project cwd — rebuild via start_thread (rebuild_session_from_meta)"
            ),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let live = {
            let mut map = self
                .live
                .lock()
                .map_err(|_| HarnessError::Io("live map poisoned".into()))?;
            map.remove(&h.identity)
        };
        if let Some(live) = live {
            let _ = live
                .transport
                .notify("session/cancel", json!({ "sessionId": live.session_id }))
                .await;
            let _ = live.transport.shutdown().await;
        }
        Ok(())
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();
        let live = self.get_live(&h.identity);
        match name.as_str() {
            "status" | "context" => {
                let status = if let Some(live) = live {
                    self.thread_status_inner(&live)
                } else {
                    ThreadStatus::default()
                };
                let receipt = status
                    .status_suffix()
                    .unwrap_or_else(|| "grok · acp".into());
                Ok(DirectiveOutcome::Done { receipt })
            }
            "compact" => Ok(DirectiveOutcome::Rejected {
                reason: "grok /compact: native command RPC not yet wired; restart session if context is full".into(),
            }),
            "model" => {
                let Some(live) = live else {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "grok session not live".into(),
                    });
                };
                // Three forms (mirrors Claude stream-json):
                // (1) picker re-entry — `d.choice` carries the picked option id
                //     (`"<modelId>"` or `"<modelId> <effort>"`);
                // (2) explicit `/model <id> [effort]`;
                // (3) bare `/model` → NeedsChoice from captured availableModels.
                let picked = d.choice.as_ref().and_then(|c| {
                    c.ids
                        .first()
                        .cloned()
                        .or_else(|| c.free_text.clone().filter(|s| !s.trim().is_empty()))
                });
                let arg = match picked {
                    Some(p) => p,
                    None => d.args.trim().to_string(),
                };
                if arg.is_empty() {
                    let options = acp_model_picker_options(&live.available_models);
                    if options.is_empty() {
                        let current = live
                            .state
                            .lock()
                            .ok()
                            .and_then(|s| s.model.clone())
                            .unwrap_or_else(|| "(unknown)".into());
                        return Ok(DirectiveOutcome::Rejected {
                            reason: format!(
                                "用法: /model <model-id> [effort]（当前: {current}；vendor 未返回 availableModels）"
                            ),
                        });
                    }
                    return Ok(DirectiveOutcome::NeedsChoice(acp_choice_prompt(
                        "Choose a Grok model:",
                        options,
                    )));
                }
                let efforts = known_efforts(&live.available_models);
                let (model_id, effort) = split_trailing_effort(&arg, &efforts);
                if model_id.is_empty() {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "用法: /model <model-id> [effort]".into(),
                    });
                }
                // Prefer vendor-listed id; still allow free-form (vendor rejects unknown).
                let mut params = json!({
                    "sessionId": live.session_id,
                    "modelId": model_id,
                });
                if let Some(ref e) = effort {
                    params["_meta"] = json!({ "reasoningEffort": e });
                }
                match live
                    .transport
                    .call("session/set_model", params)
                    .await
                {
                    Ok(result) => {
                        // Prefer vendor ack (`_meta.model.Ok`), else the requested id.
                        let applied = result
                            .pointer("/_meta/model/Ok")
                            .and_then(|v| v.as_str())
                            .unwrap_or(model_id.as_str())
                            .to_string();
                        // Window from the catalog entry when present.
                        let window = live
                            .available_models
                            .iter()
                            .find(|m| m.model_id == applied)
                            .and_then(|m| m.window);
                        if let Ok(mut st) = live.state.lock() {
                            st.model = Some(applied.clone());
                            if let Some(e) = effort.clone() {
                                st.effort = Some(e);
                            }
                            if let Some(w) = window {
                                st.window_tokens = Some(w);
                            }
                        }
                        let mut receipt = format!("已切换 model → {applied}（live）");
                        if let Some(e) = effort {
                            receipt.push_str(&format!("；effort → {e}"));
                        }
                        Ok(DirectiveOutcome::Done { receipt })
                    }
                    Err(e) => Ok(DirectiveOutcome::Rejected {
                        reason: format!("/model 切换失败: {e}"),
                    }),
                }
            }
            other => Ok(DirectiveOutcome::Rejected {
                reason: format!("grok does not support /{other}"),
            }),
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        let Some(live) = self.get_live(&h.identity) else {
            return Ok(ThreadStatus::default());
        };
        Ok(self.thread_status_inner(&live))
    }

    async fn interrupt_turn(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let Some(live) = self.get_live(&h.identity) else {
            return Ok(());
        };
        live.transport
            .notify("session/cancel", json!({ "sessionId": live.session_id }))
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("session/cancel: {e}")))?;
        Ok(())
    }

    fn thread_is_live(&self, h: &ThreadHandle) -> bool {
        self.get_live(&h.identity).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_uses_vendor_captured_efforts_only() {
        let known = known_efforts(&[AcpModelOption {
            model_id: "grok-4.5".into(),
            name: "Grok 4.5".into(),
            description: String::new(),
            window: None,
            efforts: vec!["high".into(), "low".into()],
        }]);
        assert_eq!(
            split_trailing_effort("grok-4.5 low", &known),
            ("grok-4.5".into(), Some("low".into()))
        );
        assert_eq!(
            split_trailing_effort("grok-4.5 HIGH", &known),
            ("grok-4.5".into(), Some("high".into()))
        );
        assert_eq!(
            split_trailing_effort("my-model turbo", &known),
            ("my-model turbo".into(), None)
        );
    }

    #[test]
    fn picker_options_from_vendor_catalog() {
        let models = vec![
            AcpModelOption {
                model_id: "grok-4.5".into(),
                name: "Grok 4.5".into(),
                description: String::new(),
                window: Some(500_000),
                efforts: vec!["high".into(), "low".into()],
            },
            AcpModelOption {
                model_id: "composer".into(),
                name: "Composer".into(),
                description: String::new(),
                window: None,
                efforts: vec![],
            },
        ];
        let opts = acp_model_picker_options(&models);
        assert_eq!(opts.len(), 3);
        assert_eq!(opts[0].id, "grok-4.5 high");
        assert_eq!(opts[2].id, "composer");
    }
}
