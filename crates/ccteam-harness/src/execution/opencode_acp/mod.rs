//! OpenCode ACP adapter — fourth harness vendor (`AgentVendor::Opencode`).
//!
//! Topology: **1 live session = 1 `opencode acp` child** (stdio JSON-RPC 2.0).
//! Zero PTY / pane / hook path. Wire SoT: `docs-local/versions/v0-8-24/dev-plan.md`.
//!
//! Pin: OpenCode release **1.17.17** (W0 fixture). Skip sessions use
//! [`InboundPolicy::AutoAllowPermission`] — not implementing
//! `session/request_permission` causes opencode to auto-reject tools.

pub mod spawn_spec;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::execution::acp::{
    apply_notification, fail_turn, finalize_from_prompt_result, pluck_model_info, pluck_session_id,
    AcpTransport, InboundPolicy, ModelInfo, SessionTranslateState,
};
use crate::execution::session_meta::read_session_meta;
use crate::{
    AgentSpecBrief, AgentVendor, ContextUsage, Directive, DirectiveOutcome, ExecutionMode,
    HarnessAdapter, HarnessError, PermissionMode, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadStatus, TurnId, TurnInput,
};

use spawn_spec::{build_argv, opencode_bin, OpencodeSpawnInput};

/// Max wait for any late usage_update / chunk after prompt response.
const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);

/// Adapter name — stable id for handles / logs / tests.
pub const OPENCODE_ACP_ADAPTER_NAME: &str = "opencode-acp";

const EVENT_BUFFER: usize = 256;

struct LiveSession {
    transport: Arc<AcpTransport>,
    session_id: String,
    slug: String,
    sid: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    state: Arc<StdMutex<SessionTranslateState>>,
    event_tx: broadcast::Sender<ThreadEvent>,
    permission_mode: PermissionMode,
    _dispatcher: tokio::task::JoinHandle<()>,
}

/// Per-process singleton holding live OpenCode ACP sessions keyed by sessionId.
#[derive(Clone, Default)]
pub struct OpencodeAcpAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
}

impl std::fmt::Debug for OpencodeAcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpencodeAcpAdapter").finish_non_exhaustive()
    }
}

impl OpencodeAcpAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn crate_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn inbound_policy(mode: PermissionMode) -> InboundPolicy {
        // Skip (default): auto-allow every permission request (client not
        // implementing the method would make opencode auto-REJECT tools).
        //
        // Hitl (v0.8.24 gap-fix): **fail-closed decline**, same posture as
        // grok hitl (no --always-approve + transport default-decline). The
        // former MVP auto-allow made a hitl session behave exactly like
        // skip — an approval bypass (红线: hitl must never silently allow).
        // Decline only blocks THAT tool call (opencode rejects it and the
        // turn continues — never a kill, never a panic); the full IM
        // [同意][拒绝] bridge remains the v0.9-W5 work item.
        match mode {
            PermissionMode::Hitl => InboundPolicy::DefaultDecline,
            _ => InboundPolicy::AutoAllowPermission,
        }
    }

    async fn handshake_initialize(transport: &AcpTransport) -> Result<(), HarnessError> {
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
            .map_err(|e| HarnessError::SpawnFailed(format!("opencode initialize failed: {e}")))?;

        // OpenCode does not require notifications/initialized, but sending is
        // harmless and keeps parity with Grok/ACP clients.
        let _ = transport
            .notify("notifications/initialized", Value::Null)
            .await;
        Ok(())
    }

    async fn handshake_and_new(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        mcp_servers: Vec<serde_json::Value>,
    ) -> Result<(String, ModelInfo), HarnessError> {
        Self::handshake_initialize(transport).await?;
        let new_result = transport
            .call(
                "session/new",
                json!({
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": mcp_servers
                }),
            )
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("opencode session/new failed: {e}")))?;

        let session_id = pluck_session_id(&new_result).ok_or_else(|| {
            HarnessError::SpawnFailed("opencode session/new missing sessionId".into())
        })?;
        Ok((session_id, pluck_model_info(&new_result)))
    }

    /// Prefer `session/resume` (no history replay). Fall back to `session/load`
    /// only if resume fails; load may emit untagged history — translate drops
    /// frames only when isReplay is set, so load fallback discards updates
    /// that arrive before the load response returns (transport call blocks
    /// until response; late updates still possible — best-effort).
    async fn handshake_and_resume(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        session_id: &str,
    ) -> Result<ModelInfo, HarnessError> {
        Self::handshake_initialize(transport).await?;
        let params = json!({
            "sessionId": session_id,
            "cwd": cwd.to_string_lossy(),
            "mcpServers": []
        });
        match transport.call("session/resume", params.clone()).await {
            Ok(result) => Ok(pluck_model_info(&result)),
            Err(resume_err) => {
                tracing::warn!(
                    error = %resume_err,
                    session_id,
                    "opencode session/resume failed; falling back to session/load"
                );
                let load_result = transport.call("session/load", params).await.map_err(|e| {
                    HarnessError::SpawnFailed(format!(
                        "opencode session/load failed after resume error: {e}"
                    ))
                })?;
                Ok(pluck_model_info(&load_result))
            }
        }
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
        permission_mode: PermissionMode,
    ) -> Arc<LiveSession> {
        let state = Arc::new(StdMutex::new(SessionTranslateState {
            model: info.model,
            window_tokens: info.window,
            effort: info.effort,
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
            state,
            event_tx,
            permission_mode,
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
            vendor: AgentVendor::Opencode,
            mode: ExecutionMode::Chat,
            started_at: Utc::now(),
            raw_extras: json!({
                "vendor_uuid": live.session_id,
                "sessionId": live.session_id,
                "slug": live.slug,
                "sid": live.sid,
                "project_dir": live.project_dir,
                "cwd": live.cwd,
                "protocol": "acp",
                "adapter": OPENCODE_ACP_ADAPTER_NAME,
                "permission_mode": match live.permission_mode {
                    PermissionMode::Skip => "skip",
                    PermissionMode::Hitl => "hitl",
                },
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
impl HarnessAdapter for OpencodeAcpAdapter {
    fn name(&self) -> &'static str {
        OPENCODE_ACP_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Opencode
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        // MVP roleless: ignore role (no persona injection).
        let bin = opencode_bin();
        let argv = build_argv(&bin, &OpencodeSpawnInput::default());
        let program = argv[0].clone();
        let args: Vec<String> = argv.into_iter().skip(1).collect();
        let cwd = if ctx.cwd.as_os_str().is_empty() {
            ctx.project_dir.clone()
        } else {
            ctx.cwd.clone()
        };
        let inbound = Self::inbound_policy(ctx.permission_mode);
        // v0.8.24 C1 — best-effort ccteam MCP inject into session/new.
        // Failure to load MCP must not block the prompt path (empty vec).
        let mcp_servers =
            crate::execution::mcp_config::opencode_mcp_servers_http(&ctx.sid, &ctx.secret);

        let prior_uuid = read_session_meta(&ctx.project_dir, &ctx.sid)
            .ok()
            .map(|m| m.vendor_uuid)
            .filter(|u| !u.trim().is_empty());

        if let Some(ref uuid) = prior_uuid {
            if let Some(live) = self.get_live(uuid) {
                return Ok(Self::make_handle(&live));
            }
        }

        let try_resume = prior_uuid.clone();
        let (transport, session_id, info) = match try_resume {
            Some(uuid) => {
                let transport =
                    AcpTransport::spawn_command_with_policy(&program, &args, &cwd, inbound)
                        .await
                        .map_err(|e| {
                            HarnessError::SpawnFailed(format!("spawn opencode acp: {e}"))
                        })?;
                let transport = Arc::new(transport);
                match Self::handshake_and_resume(&transport, &cwd, &uuid).await {
                    Ok(info) => (transport, uuid, info),
                    Err(resume_err) => {
                        tracing::warn!(
                            error = %resume_err,
                            "opencode resume/load failed; falling back to session/new"
                        );
                        let _ = transport.shutdown().await;
                        let transport =
                            AcpTransport::spawn_command_with_policy(&program, &args, &cwd, inbound)
                                .await
                                .map_err(|e| {
                                    HarnessError::SpawnFailed(format!(
                                        "spawn opencode after resume fail: {e}"
                                    ))
                                })?;
                        let transport = Arc::new(transport);
                        let (sid, info) =
                            Self::handshake_and_new(&transport, &cwd, mcp_servers.clone()).await?;
                        (transport, sid, info)
                    }
                }
            }
            None => {
                let transport =
                    AcpTransport::spawn_command_with_policy(&program, &args, &cwd, inbound)
                        .await
                        .map_err(|e| {
                            HarnessError::SpawnFailed(format!("spawn opencode acp: {e}"))
                        })?;
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
            ctx.permission_mode,
        );
        // v0.8.24 A-U3 — best-effort spawn-time model/effort via the SAME
        // vendor-native seam the `/model` directive uses
        // (`session/set_config_option`; opencode's `session/new` takes no
        // model). A failure must never fail the spawn — the session then
        // runs on opencode's self-selected default (honest degrade, warn
        // only). Model value shape is opencode's `provider/model[/variant]`;
        // effort must be one of the model's variants.
        for (config_id, value) in [
            ("model", ctx.model_id.as_deref()),
            ("effort", ctx.effort.as_deref()),
        ] {
            let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
                continue;
            };
            match live
                .transport
                .call(
                    "session/set_config_option",
                    json!({
                        "sessionId": live.session_id,
                        "configId": config_id,
                        "value": value,
                    }),
                )
                .await
            {
                Ok(_) => {
                    if let Ok(mut st) = live.state.lock() {
                        if config_id == "model" {
                            st.model = Some(value.to_string());
                        } else {
                            st.effort = Some(value.to_string());
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    sid = %ctx.sid,
                    config_id,
                    value,
                    error = %e,
                    "opencode spawn-time set_config_option failed; continuing with vendor default"
                ),
            }
        }
        let mut handle = Self::make_handle(&live);
        if let Ok(st) = live.state.lock() {
            if let Some(m) = &st.model {
                handle.raw_extras["model"] = json!(m);
            }
        }
        Ok(handle)
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let live = self.get_live(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("opencode session {} not live", h.identity))
        })?;

        let text = match input {
            TurnInput::UserText(t) => t,
            other => {
                return Err(HarnessError::SubmitFailed(format!(
                    "opencode_acp: unsupported turn input {other:?}"
                )));
            }
        };

        let turn_id = format!("t-{}", Utc::now().timestamp_millis());
        let turn_done = Arc::new(tokio::sync::Notify::new());
        {
            let mut st = live
                .state
                .lock()
                .map_err(|_| HarnessError::Io("opencode state lock poisoned".into()))?;
            if st.buffer.is_some() {
                return Err(HarnessError::SubmitFailed(
                    "opencode_acp: a turn is already in progress for this session".into(),
                ));
            }
            st.begin_turn(turn_id.clone(), Arc::clone(&turn_done));
        }

        let transport = Arc::clone(&live.transport);
        let state = Arc::clone(&live.state);
        let event_tx = live.event_tx.clone();
        let session_id = live.session_id.clone();
        let turn_id_bg = turn_id.clone();

        tokio::spawn(async move {
            let _ = event_tx.send(ThreadEvent::TurnStarted {
                turn_id: turn_id_bg.clone(),
            });
            let result = transport
                .call(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": [{ "type": "text", "text": text }]
                    }),
                )
                .await;
            let events = match result {
                Ok(result) => {
                    // Brief wait for any trailing usage_update after response.
                    let _ = tokio::time::timeout(FINALIZE_BARRIER, turn_done.notified()).await;
                    // Also sleep a tick so usage_update that races the response
                    // is applied by the dispatcher before we finalize.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    if let Ok(mut st) = state.lock() {
                        finalize_from_prompt_result(&mut st, &result)
                    } else {
                        Vec::new()
                    }
                }
                Err(e) => {
                    if let Ok(mut st) = state.lock() {
                        fail_turn(&mut st, &e.to_string())
                    } else {
                        Vec::new()
                    }
                }
            };
            for ev in events {
                let _ = event_tx.send(ev);
            }
        });

        Ok(TurnId(turn_id))
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
        Err(HarnessError::NotImplemented {
            reason: format!(
                "opencode cold resume of {persistent_id} needs project cwd — rebuild via start_thread (rebuild_session_from_meta)"
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
                    .unwrap_or_else(|| "opencode · acp".into());
                Ok(DirectiveOutcome::Done { receipt })
            }
            "compact" => {
                // OpenCode treats `/compact` as a slash prompt (summarize).
                if let Some(live) = live {
                    let _ = self
                        .submit_turn(h, TurnInput::UserText("/compact".into()))
                        .await;
                    let _ = live;
                    Ok(DirectiveOutcome::Done {
                        receipt: "opencode /compact submitted as prompt".into(),
                    })
                } else {
                    Ok(DirectiveOutcome::Rejected {
                        reason: "opencode session not live".into(),
                    })
                }
            }
            "model" => {
                let arg = d.args.trim();
                let Some(live) = live else {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "opencode session not live".into(),
                    });
                };
                if arg.is_empty() {
                    let model = live
                        .state
                        .lock()
                        .ok()
                        .and_then(|s| s.model.clone())
                        .unwrap_or_else(|| "(unknown)".into());
                    return Ok(DirectiveOutcome::Done {
                        receipt: format!("opencode model: {model}"),
                    });
                }
                match live
                    .transport
                    .call(
                        "session/set_config_option",
                        json!({
                            "sessionId": live.session_id,
                            "configId": "model",
                            "value": arg,
                        }),
                    )
                    .await
                {
                    Ok(_) => {
                        if let Ok(mut st) = live.state.lock() {
                            st.model = Some(arg.to_string());
                        }
                        Ok(DirectiveOutcome::Done {
                            receipt: format!("opencode model set to {arg}"),
                        })
                    }
                    Err(e) => Ok(DirectiveOutcome::Rejected {
                        reason: format!("opencode set_config_option model failed: {e}"),
                    }),
                }
            }
            other => Ok(DirectiveOutcome::Rejected {
                reason: format!("opencode does not support /{other}"),
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
