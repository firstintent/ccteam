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
    AgentSpecBrief, AgentVendor, ContextUsage, Directive, DirectiveOutcome, ExecutionMode,
    HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId,
    TurnInput,
};

use crate::execution::session_meta::read_session_meta;
use protocol::{pluck_model_info, pluck_session_id, ModelInfo};
use spawn_spec::{build_argv, grok_bin, GrokSpawnInput};
use translate::{
    apply_notification, fail_turn, finalize_from_prompt_result, SessionTranslateState,
};
use transport::AcpTransport;

/// Max wait for the dispatcher to reach the turn boundary after the prompt
/// response, before finalizing anyway (best-effort if `turn_completed` is
/// ever absent). The boundary is normally already signalled by then.
const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);

/// Adapter name — stable id for handles / logs / tests.
pub const GROK_ACP_ADAPTER_NAME: &str = "grok-acp";

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
    _dispatcher: tokio::task::JoinHandle<()>,
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
                let transport = AcpTransport::spawn_command(&program, &args, &cwd)
                    .await
                    .map_err(|e| {
                        HarnessError::SpawnFailed(format!("spawn grok agent stdio: {e}"))
                    })?;
                let transport = Arc::new(transport);
                match Self::handshake_and_load(&transport, &cwd, &uuid, mcp_servers.clone()).await {
                    Ok(info) => (transport, uuid, info),
                    Err(load_err) => {
                        tracing::warn!(
                            error = %load_err,
                            "grok session/load failed; falling back to session/new"
                        );
                        let _ = transport.shutdown().await;
                        let transport = AcpTransport::spawn_command(&program, &args, &cwd)
                            .await
                            .map_err(|e| {
                                HarnessError::SpawnFailed(format!(
                                    "spawn grok after load fail: {e}"
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
                let transport = AcpTransport::spawn_command(&program, &args, &cwd)
                    .await
                    .map_err(|e| {
                        HarnessError::SpawnFailed(format!("spawn grok agent stdio: {e}"))
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
        );
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

        let turn_id = format!("t-{}", Utc::now().timestamp_millis());
        // Barrier the dispatcher fires on the turn boundary so finalize sees
        // every buffered chunk (see FINALIZE_BARRIER / SessionTranslateState).
        let turn_done = Arc::new(tokio::sync::Notify::new());
        {
            let mut st = live
                .state
                .lock()
                .map_err(|_| HarnessError::Io("grok state lock poisoned".into()))?;
            // Guard against a clobbered in-flight turn (the gateway serializes
            // turns per session; this is defense-in-depth).
            if st.buffer.is_some() {
                return Err(HarnessError::SubmitFailed(
                    "grok_acp: a turn is already in progress for this session".into(),
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
                    // Wait until the dispatcher has drained through the turn
                    // boundary (all `agent_message_chunk` buffered) before
                    // finalizing. A stored permit makes this return instantly
                    // when the boundary already arrived (the common case).
                    let _ = tokio::time::timeout(FINALIZE_BARRIER, turn_done.notified()).await;
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
                let arg = d.args.trim();
                if arg.is_empty() {
                    Ok(DirectiveOutcome::Rejected {
                        reason: "grok /model: list/set not yet wired; re-open session with desired model".into(),
                    })
                } else {
                    Ok(DirectiveOutcome::Rejected {
                        reason: "grok /model: set-model RPC not available; re-open session with -m"
                            .into(),
                    })
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
