//! DSH (DeepSeek Harness) ACP adapter — seventh vendor (`AgentVendor::Dsh`).
//!
//! Topology: one managed session = one `dsh --profile ccteam` child. The ACP
//! peer is ccteam's Cordis plugin, not the official DSH ACP demo.

pub mod handshake;
pub mod materialize;
pub mod spawn_spec;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::json;
use tokio::sync::broadcast;

use crate::execution::acp::{
    released_thread_status, route_acp_turn, AcpTransport, AcpTurnRoute, AcpTurnRunner,
    AcpTurnTuning, InboundPolicy, ModelInfo, SessionTranslateState,
};
use crate::execution::mcp_config::SessionMcpEndpoint;
use crate::execution::session_meta::read_session_meta;
use crate::execution::session_status::read_status_file;
use crate::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, ExecutionMode,
    HarnessAdapter, HarnessError, PermissionMode, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadStatus, ToolSurfaceRebuild, TurnId, TurnInput, TurnRouting, TurnSubmission,
};

use handshake::DshAgentOptions;
use spawn_spec::{build_spawn_spec, purge_mirrored_credentials, verify_dsh_version};
pub use spawn_spec::{dsh_bin, find_cached_dsh_bin, resolve_dsh_default_bin, DSH_BIN_ENV};

const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);
const EVENT_BUFFER: usize = 256;

/// Adapter name — stable id for handles / logs / tests.
pub const DSH_ACP_ADAPTER_NAME: &str = "dsh-acp";

const DSH_STATUS_GAP: &str = "DSH is driven through ccteam's own Cordis plugin (there is no vendor automation CLI). Vendor memory persists in this session's managed DSH home and survives restarts; deleting that directory resets DSH memory but keeps the ccteam transcript and ledger.";

struct LiveSession {
    transport: Arc<AcpTransport>,
    session_id: String,
    slug: String,
    sid: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    dsh_home: PathBuf,
    state: Arc<StdMutex<SessionTranslateState>>,
    event_tx: broadcast::Sender<ThreadEvent>,
    permission_mode: PermissionMode,
    _dispatcher: tokio::task::JoinHandle<()>,
}

/// Per-process singleton holding live DSH ACP sessions keyed by DSH session id.
#[derive(Clone, Default)]
pub struct DshAcpAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
}

impl std::fmt::Debug for DshAcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DshAcpAdapter").finish_non_exhaustive()
    }
}

impl DshAcpAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn inbound_policy(mode: PermissionMode) -> InboundPolicy {
        match mode {
            PermissionMode::Hitl => InboundPolicy::DefaultDecline,
            PermissionMode::Skip => InboundPolicy::AutoAllowPermission,
        }
    }

    fn session_mcp_endpoint(ctx: &SpawnCtx) -> Result<SessionMcpEndpoint, HarnessError> {
        SessionMcpEndpoint::resolve(&ctx.sid, &ctx.secret).ok_or_else(|| {
            HarnessError::SpawnFailed(format!(
                "DSH sessions need a ccteam MCP principal (sid + per-session secret); \
                 sid=`{}` has none, so the ccteam DSH plugin cannot authenticate",
                ctx.sid
            ))
        })
    }

    fn get_live(&self, session_id: &str) -> Option<Arc<LiveSession>> {
        self.live
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned())
    }

    async fn spawn_transport(
        spawn: &spawn_spec::DshSpawnSpec,
        inbound: InboundPolicy,
        sid: &str,
    ) -> Result<Arc<AcpTransport>, HarnessError> {
        AcpTransport::spawn_for_session(
            &spawn.bin,
            &spawn.args,
            &spawn.cwd,
            &spawn.env,
            inbound,
            sid,
        )
        .await
        .map(Arc::new)
        .map_err(|e| HarnessError::SpawnFailed(format!("spawn dsh acp: {e}")))
    }

    async fn handshake_new(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        agent_options: &DshAgentOptions,
    ) -> Result<(String, ModelInfo), HarnessError> {
        handshake::initialize(transport).await?;
        handshake::session_new(transport, cwd, agent_options).await
    }

    async fn handshake_load(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        session_id: &str,
        agent_options: &DshAgentOptions,
    ) -> Result<ModelInfo, HarnessError> {
        handshake::initialize(transport).await?;
        handshake::session_load(transport, cwd, session_id, agent_options).await
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
        dsh_home: PathBuf,
        info: ModelInfo,
        permission_mode: PermissionMode,
        requested_model: Option<String>,
    ) -> Arc<LiveSession> {
        let state = Arc::new(StdMutex::new(SessionTranslateState {
            model: info.model.or(requested_model),
            window_tokens: info.window,
            effort: info.effort,
            ..Default::default()
        }));
        if let Some(snapshot) = read_status_file(&project_dir, &sid) {
            if let Ok(mut st) = state.lock() {
                st.seed_from_snapshot(&snapshot);
            }
        }
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
            dsh_home,
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

    fn make_handle(live: &LiveSession) -> ThreadHandle {
        ThreadHandle {
            identity: live.session_id.clone(),
            vendor: AgentVendor::Dsh,
            mode: ExecutionMode::Chat,
            started_at: Utc::now(),
            raw_extras: json!({
                "vendor_uuid": live.session_id,
                "sessionId": live.session_id,
                "slug": live.slug,
                "sid": live.sid,
                "project_dir": live.project_dir,
                "cwd": live.cwd,
                "dsh_home": live.dsh_home,
                "protocol": "acp",
                "adapter": DSH_ACP_ADAPTER_NAME,
                "permission_mode": match live.permission_mode {
                    PermissionMode::Skip => "skip",
                    PermissionMode::Hitl => "hitl",
                },
            }),
        }
    }

    fn thread_status_inner(&self, live: &LiveSession) -> ThreadStatus {
        live.state
            .lock()
            .map(|st| st.thread_status())
            .unwrap_or_default()
    }

    async fn submit_with_routing(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        let live = self.get_live(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("dsh session {} not live", h.identity))
        })?;
        let text = match input {
            TurnInput::UserText(t) => t,
            other => {
                return Err(HarnessError::SubmitFailed(format!(
                    "dsh_acp: unsupported turn input {other:?}"
                )));
            }
        };

        let route = {
            let mut state = live
                .state
                .lock()
                .map_err(|_| HarnessError::Io("dsh state lock poisoned".into()))?;
            route_acp_turn(&mut state, &text, routing, false)
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
                    project_dir: live.project_dir.clone(),
                    sid: live.sid.clone(),
                    context_probe: None,
                    tuning: AcpTurnTuning {
                        finalize_barrier: FINALIZE_BARRIER,
                        post_finalize_sleep: None,
                        label: "dsh",
                    },
                }
                .spawn(turn_id.clone(), turn_done, prompt_sent, text);
                Ok(TurnSubmission::started(TurnId(turn_id)))
            }
            AcpTurnRoute::Queue {
                turn_id,
                degraded_from_inject,
            } => {
                if degraded_from_inject {
                    tracing::debug!(
                        turn_id = %turn_id,
                        "DSH ACP has no native interject method; queued active-turn message"
                    );
                }
                Ok(TurnSubmission::queued(TurnId(turn_id)))
            }
            AcpTurnRoute::Inject { .. } => Err(HarnessError::Io(
                "dsh ACP routing selected unsupported native inject".into(),
            )),
        }
    }
}

fn spawn_notif_dispatcher(
    transport: Arc<AcpTransport>,
    state: Arc<StdMutex<SessionTranslateState>>,
    event_tx: broadcast::Sender<ThreadEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (early, mut sub) = transport.subscribe_with_early();
        for n in early {
            for ev in crate::execution::acp::apply_notification_shared(&state, &n) {
                let _ = event_tx.send(ev);
            }
        }
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(n) => {
                        for ev in crate::execution::acp::apply_notification_shared(&state, &n) {
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
impl HarnessAdapter for DshAcpAdapter {
    fn name(&self) -> &'static str {
        DSH_ACP_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Dsh
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if ctx.remote.is_some() {
            return Err(HarnessError::NotImplemented {
                reason: "remote execution is not yet supported for DSH; use host=local".into(),
            });
        }
        if !spec.role.trim().is_empty() {
            return Err(HarnessError::SpawnFailed(
                "DSH sessions are roleless-only; ccteam does not inject role prompts".into(),
            ));
        }
        if let Some(effort) = ctx
            .effort
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            return Err(crate::execution::acp::spawn_pick_refused(
                "effort",
                effort,
                "DSH ACP has no reasoning-effort axis",
            ));
        }

        let mcp = Self::session_mcp_endpoint(ctx)?;
        let bin = dsh_bin();
        verify_dsh_version(&bin).await?;
        let spawn = build_spawn_spec(ctx, &mcp)?;
        let inbound = Self::inbound_policy(ctx.permission_mode);
        let agent_options = DshAgentOptions::new(ctx.model_id.as_deref());

        let prior_uuid = read_session_meta(&ctx.project_dir, &ctx.sid)
            .ok()
            .map(|m| m.vendor_uuid)
            .filter(|u| !u.trim().is_empty());
        if let Some(ref uuid) = prior_uuid {
            if let Some(live) = self.get_live(uuid) {
                return Ok(Self::make_handle(&live));
            }
        }

        let (transport, session_id, info) = match prior_uuid {
            Some(uuid) => {
                let transport = Self::spawn_transport(&spawn, inbound, &ctx.sid).await?;
                match Self::handshake_load(&transport, &spawn.cwd, &uuid, &agent_options).await {
                    Ok(info) => (transport, uuid, info),
                    Err(load_err) => {
                        tracing::warn!(
                            error = %load_err,
                            prior_session_id = %uuid,
                            "dsh session/load failed; falling back to session/new"
                        );
                        let _ = transport.shutdown().await;
                        let transport = Self::spawn_transport(&spawn, inbound, &ctx.sid).await?;
                        let (new_id, info) =
                            Self::handshake_new(&transport, &spawn.cwd, &agent_options).await?;
                        if new_id == uuid {
                            tracing::warn!(
                                session_id = %new_id,
                                "dsh session/new returned the failed load id"
                            );
                        }
                        (transport, new_id, info)
                    }
                }
            }
            None => {
                let transport = Self::spawn_transport(&spawn, inbound, &ctx.sid).await?;
                let (session_id, info) =
                    Self::handshake_new(&transport, &spawn.cwd, &agent_options).await?;
                (transport, session_id, info)
            }
        };

        let live = self.register_live(
            transport,
            session_id,
            ctx.slug.clone(),
            ctx.sid.clone(),
            ctx.project_dir.clone(),
            spawn.cwd,
            spawn.dsh_home,
            info,
            ctx.permission_mode,
            agent_options.requested_model_display(),
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

    fn event_attachment(&self) -> EventAttachment {
        EventAttachment::Rebuildable
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<ToolSurfaceRebuild, HarnessError> {
        Ok(ToolSurfaceRebuild::RespawnRequired {
            reason: "DSH loads the ccteam Cordis plugin and MCP bearer only at process start; \
                     respawn is lossless because `session/load` reattaches the vendor's managed DSH memory"
                .to_string(),
        })
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        if let Some(live) = self.get_live(persistent_id) {
            return Ok(Self::make_handle(&live));
        }
        Err(HarnessError::NotImplemented {
            reason: format!(
                "dsh cold resume of {persistent_id} needs project cwd — rebuild via start_thread (session/load ladder)"
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
            purge_mirrored_credentials(&live.dsh_home);
        }
        Ok(())
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();
        match name.as_str() {
            "status" | "context" => {
                let status = if let Some(live) = self.get_live(&h.identity) {
                    self.thread_status_inner(&live)
                } else {
                    released_thread_status(h)
                };
                let suffix = status
                    .status_suffix()
                    .unwrap_or_else(|| "dsh · acp".to_string());
                Ok(DirectiveOutcome::Done {
                    receipt: format!("{suffix}\n{DSH_STATUS_GAP}"),
                })
            }
            "compact" | "clear" | "model" => Ok(DirectiveOutcome::Rejected {
                reason: format!("dsh /{name} is not supported through ccteam; {DSH_STATUS_GAP}"),
            }),
            other => Ok(DirectiveOutcome::Rejected {
                reason: format!("dsh does not support /{other}"),
            }),
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        let Some(live) = self.get_live(&h.identity) else {
            return Ok(released_thread_status(h));
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

    fn handle() -> ThreadHandle {
        ThreadHandle {
            vendor: AgentVendor::Dsh,
            mode: ExecutionMode::Chat,
            identity: "s1".to_string(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        }
    }

    #[test]
    fn name_and_vendor_are_dsh() {
        let a = DshAcpAdapter::new();
        assert_eq!(a.name(), DSH_ACP_ADAPTER_NAME);
        assert_eq!(a.vendor(), AgentVendor::Dsh);
    }

    #[tokio::test]
    async fn resume_thread_is_not_implemented_for_cold_id() {
        let a = DshAcpAdapter::new();
        let err = a.resume_thread("some-vendor-uuid").await.unwrap_err();
        assert!(matches!(err, HarnessError::NotImplemented { .. }));
    }

    #[test]
    fn event_attachment_is_rebuildable() {
        let a = DshAcpAdapter::new();
        assert_eq!(a.event_attachment(), EventAttachment::Rebuildable);
    }

    #[tokio::test]
    async fn rebuild_tool_surface_needs_lossless_respawn() {
        let a = DshAcpAdapter::new();
        let outcome = a.rebuild_tool_surface(&handle()).await.unwrap();
        let ToolSurfaceRebuild::RespawnRequired { reason } = outcome;
        assert!(reason.contains("lossless"));
        assert!(reason.contains("session/load"));
    }

    #[tokio::test]
    async fn handle_directive_rejects_private_state_commands() {
        let a = DshAcpAdapter::new();
        for cmd in ["compact", "clear", "model"] {
            let outcome = a
                .handle_directive(
                    &handle(),
                    Directive {
                        name: cmd.to_string(),
                        args: String::new(),
                        choice: None,
                    },
                )
                .await
                .unwrap();
            assert!(matches!(outcome, DirectiveOutcome::Rejected { .. }));
        }
    }
}
