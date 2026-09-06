//! DSH (DeepSeek Harness) ACP adapter — seventh vendor (`AgentVendor::Dsh`).
//!
//! Topology (v0.10.3): ccteam spawns NO DSH child per hire. Each identity has
//! exactly one `dsh web` runtime, owned by
//! [`crate::execution::dsh_runtime::DshRuntimeManager`], whose embedded ccteam
//! Cordis plugin serves ACP on a unix socket; every hire is one CONNECTION to
//! that socket, and the human's DSH web UI is just another client of the same
//! runtime. Closing a hire closes its connection and nothing else — the
//! runtime, and the DSH memory in the identity's own home, outlive it.

pub mod handshake;
pub mod materialize;
pub mod spawn_spec;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::json;
use tokio::sync::broadcast;

use crate::execution::acp::{
    released_thread_status, route_acp_turn, AcpTransport, AcpTurnRoute, AcpTurnRunner,
    AcpTurnTuning, InboundPolicy, ModelInfo, SessionTranslateState,
};
use crate::execution::dsh_runtime::{DshRuntimeIdentity, DshRuntimeManager, DshRuntimeState};
use crate::execution::mcp_config::SessionMcpEndpoint;
use crate::execution::session_meta::read_session_meta;
use crate::execution::session_status::read_status_file;
use crate::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, EventAttachment, ExecutionMode,
    HarnessAdapter, HarnessError, PermissionMode, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadStatus, ToolSurfaceRebuild, TurnId, TurnInput, TurnRouting, TurnSubmission,
};

use handshake::{CcteamSessionMeta, DshAgentOptions};
pub use spawn_spec::{
    build_web_spawn_spec, dsh_bin, dsh_config_source, find_cached_dsh_bin, identity_socket_path,
    resolve_dsh_default_bin, socket_path_for_identity, tenant_home_segment, DshConfigSource,
    DshWebSpawnOptions, DSH_BIN_ENV, DSH_NATIVE_WEB_PROFILE, DSH_SOCKET_ENV, DSH_WEB_PROFILE,
};
use spawn_spec::{ccteam_root, dsh_socket_override, identity_dsh_home, project_cwd};

const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);
const EVENT_BUFFER: usize = 256;
/// How long to keep dialing the runtime's socket. The manager reports readiness
/// from the HTTP listener, but Cordis binds the plugin's socket on its own
/// schedule, so a fresh runtime is regularly reachable a beat later.
const CONNECT_BUDGET: Duration = Duration::from_secs(15);
const CONNECT_RETRY: Duration = Duration::from_millis(250);
/// How long `close_thread` waits for the runtime to acknowledge the cancel
/// before dropping the connection out from under it.
const CANCEL_ON_CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// Adapter name — stable id for handles / logs / tests.
pub const DSH_ACP_ADAPTER_NAME: &str = "dsh-acp";

const DSH_STATUS_GAP: &str = "DSH is driven through ccteam's own Cordis plugin (there is no vendor automation CLI). Vendor memory persists in this identity's DSH home and survives restarts; deleting that directory resets DSH memory but keeps the ccteam transcript and ledger.";

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

/// Resolve the ccteam `mode` axis to a DSH agent-preset id. The vendor's
/// shipped presets are `standard` | `code` (displayed "PTC") | `minimal` |
/// `cordis` (displayed "creator"); ccteam accepts both spellings and an unset
/// mode defaults a hire to `standard` (owner decree, matching DSH's own web
/// default). Presets pick the TOOLSET: a session created without one has no
/// bash/read/write at all in the web runtime.
pub fn mode_agent_preset(mode: Option<&str>) -> Result<String, HarnessError> {
    let token = mode
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("standard");
    match token.to_ascii_lowercase().as_str() {
        "ptc" | "code" => Ok("code".to_string()),
        "standard" => Ok("standard".to_string()),
        "minimal" => Ok("minimal".to_string()),
        "creator" | "cordis" => Ok("cordis".to_string()),
        other => Err(HarnessError::SpawnFailed(format!(
            "unknown DSH session mode `{other}`: accepts standard | ptc (code) | minimal | \
             creator (cordis); omit it for the ccteam default (standard)"
        ))),
    }
}

/// Per-process singleton holding live DSH ACP sessions keyed by DSH session id.
#[derive(Clone)]
pub struct DshAcpAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
    /// The daemon's ONE DSH runtime manager — the same instance ccteam web
    /// drives. Shared by construction, so "one identity, one `dsh web`
    /// process" cannot be broken by a consumer forgetting a convention.
    runtime: Arc<DshRuntimeManager>,
}

impl std::fmt::Debug for DshAcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DshAcpAdapter").finish_non_exhaustive()
    }
}

impl DshAcpAdapter {
    pub fn new(runtime: Arc<DshRuntimeManager>) -> Self {
        Self {
            live: Arc::new(StdMutex::new(HashMap::new())),
            runtime,
        }
    }

    /// The runtime manager this adapter drives — the daemon's single instance.
    pub fn runtime(&self) -> &Arc<DshRuntimeManager> {
        &self.runtime
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

    /// Make sure this identity's `dsh web` runtime is up, and report the
    /// identity the manager knows it by.
    async fn ensure_runtime(&self, owner: &str) -> Result<DshRuntimeIdentity, HarnessError> {
        let identity = DshRuntimeIdentity::for_owner_tag(owner);
        let status = self.runtime.start(&identity).await;
        match status.state {
            DshRuntimeState::Running | DshRuntimeState::Attached => Ok(identity),
            DshRuntimeState::Disabled => Err(HarnessError::SpawnFailed(
                "DSH is unavailable: this daemon has no DSH runtime. Run `ccteam start` with the \
                 web UI enabled (the DSH companion port is what turns the runtime on)."
                    .to_string(),
            )),
            other => Err(HarnessError::SpawnFailed(format!(
                "DSH runtime for `{owner}` is {}: {}",
                describe_state(other),
                status
                    .error_tail
                    .unwrap_or_else(|| "no output from the runtime".to_string())
            ))),
        }
    }

    /// Dial the runtime's ACP socket until it answers or the budget runs out.
    async fn connect(socket: &Path, inbound: InboundPolicy) -> Result<Arc<AcpTransport>, String> {
        let deadline = tokio::time::Instant::now() + CONNECT_BUDGET;
        loop {
            let failure = match AcpTransport::connect_unix(socket, inbound).await {
                Ok(transport) => return Ok(Arc::new(transport)),
                Err(err) => format!("{err:#}"),
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(failure);
            }
            tokio::time::sleep(CONNECT_RETRY).await;
        }
    }

    /// Why the socket did not answer, in the words the user can act on.
    async fn connect_failure(
        &self,
        identity: Option<&DshRuntimeIdentity>,
        socket: &Path,
        error: String,
    ) -> HarnessError {
        let Some(identity) = identity else {
            return HarnessError::SpawnFailed(format!(
                "cannot reach the DSH ACP socket {} named by {DSH_SOCKET_ENV}: {error}",
                socket.display()
            ));
        };
        let status = self.runtime.status(identity).await;
        let mut message = format!(
            "cannot reach the ccteam ACP socket {} inside the DSH runtime of `{}` (runtime {})",
            socket.display(),
            identity.owner_tag,
            describe_state(status.state)
        );
        if status.state == DshRuntimeState::Attached {
            // ccteam did not start this instance, so it never wrote the plugin
            // row into that home: the socket is missing because the human's own
            // `dsh web` predates the registration.
            message.push_str(
                ". That instance was started outside ccteam — register the plugin \
                 (`dsh plugin add @ccteam/ccteam-ui`) and restart your DSH web",
            );
        }
        message.push_str(&format!(": {error}"));
        if let Some(tail) = status.error_tail {
            message.push_str(&format!("; last runtime output: {tail}"));
        }
        HarnessError::SpawnFailed(message)
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
        generation: Option<u64>,
    ) -> Arc<LiveSession> {
        let state = Arc::new(StdMutex::new(SessionTranslateState {
            model: info.model.or(requested_model),
            window_tokens: info.window,
            effort: info.effort,
            // Stamp this THREAD so every observation the turn runner
            // persists says which one made it (`docs-local/issues/#14②`).
            generation,
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
                position,
            } => {
                if degraded_from_inject {
                    tracing::debug!(
                        turn_id = %turn_id,
                        "DSH ACP has no native interject method; queued active-turn message"
                    );
                }
                Ok(TurnSubmission::queued_at(TurnId(turn_id), position))
            }
            AcpTurnRoute::Inject { .. } => Err(HarnessError::Io(
                "dsh ACP routing selected unsupported native inject".into(),
            )),
        }
    }
}

fn describe_state(state: DshRuntimeState) -> &'static str {
    match state {
        DshRuntimeState::Disabled => "disabled",
        DshRuntimeState::Stopped => "stopped",
        DshRuntimeState::Starting => "still starting",
        DshRuntimeState::Running => "running",
        DshRuntimeState::Attached => "attached (started outside ccteam)",
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
        let cwd = project_cwd(ctx)?;
        let inbound = Self::inbound_policy(ctx.permission_mode);
        let agent_options = DshAgentOptions::new(ctx.model_id.as_deref());
        // `session/load` carries the identity WITHOUT a preset (the stored one
        // is authoritative); `session/new` — fresh or as the failed-load
        // fallback — carries the resolved mode, ccteam-defaulting to PTC.
        let agent_preset = mode_agent_preset(ctx.mode.as_deref())?;
        let meta = CcteamSessionMeta::new(&ctx.sid, &mcp, ctx.permission_mode);
        let new_meta = meta.clone().with_agent_preset(agent_preset);

        // The runtime is per identity, so the socket and the DSH home are too.
        // `CCTEAM_DSH_SOCKET` (test-only) names a socket a fake already serves
        // and skips the manager entirely.
        let (identity, socket, dsh_home) = match dsh_socket_override() {
            Some(socket) => (None, socket, PathBuf::new()),
            None => {
                let ccteam_home = ccteam_root()?;
                let identity = self.ensure_runtime(&ctx.owner).await?;
                let socket = identity_socket_path(&ctx.owner, &ccteam_home);
                let home = identity_dsh_home(&ctx.owner, &ccteam_home)?;
                (Some(identity), socket, home)
            }
        };

        let prior_uuid = read_session_meta(&ctx.project_dir, &ctx.sid)
            .ok()
            .map(|m| m.vendor_uuid)
            .filter(|u| !u.trim().is_empty());
        if let Some(ref uuid) = prior_uuid {
            if let Some(live) = self.get_live(uuid) {
                return Ok(Self::make_handle(&live));
            }
        }

        let transport = match Self::connect(&socket, inbound).await {
            Ok(transport) => transport,
            Err(err) => return Err(self.connect_failure(identity.as_ref(), &socket, err).await),
        };
        // Once per CONNECTION, before any session work: this is where the peer
        // proves it is a ccteam plugin new enough to honor `_meta.ccteam`.
        //
        // No `vendor_pids` registration here, deliberately: the process-lineage
        // fallback attributes an `/mcp` caller to ONE sid, and this process
        // serves every hire of the identity plus the human at the DSH UI.
        // Attribution is explicit instead — the bearer in `_meta.ccteam` IS the
        // per-session principal, and the plugin dials the daemon with it.
        handshake::initialize(&transport).await?;
        let (session_id, info) = match prior_uuid {
            Some(uuid) => {
                match handshake::session_load(&transport, &cwd, &uuid, &agent_options, &meta).await
                {
                    Ok(info) => (uuid, info),
                    Err(load_err) => {
                        tracing::warn!(
                            error = %load_err,
                            prior_session_id = %uuid,
                            "dsh session/load failed; falling back to session/new"
                        );
                        // A rejected `session/load` leaves the connection
                        // perfectly usable — the runtime and every other hire on
                        // it are untouched — so `session/new` reuses it.
                        let (new_id, info) =
                            handshake::session_new(&transport, &cwd, &agent_options, &new_meta)
                                .await?;
                        if new_id == uuid {
                            tracing::warn!(
                                session_id = %new_id,
                                "dsh session/new returned the failed load id"
                            );
                        }
                        (new_id, info)
                    }
                }
            }
            None => handshake::session_new(&transport, &cwd, &agent_options, &new_meta).await?,
        };

        let live = self.register_live(
            transport,
            session_id,
            ctx.slug.clone(),
            ctx.sid.clone(),
            ctx.project_dir.clone(),
            cwd,
            dsh_home,
            info,
            ctx.permission_mode,
            agent_options.requested_model_display(),
            ctx.generation_stamp(),
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
            reason: "a DSH session's ccteam identity is installed by its `session/new`; \
                     respawn is lossless because `session/load` reattaches the same DSH agent \
                     inside the identity's still-running runtime"
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
            // Cancel OUR turn, then drop OUR connection. The runtime keeps
            // running (it is the identity's, not this session's) and the DSH
            // agent stays live inside it, which is what makes a later
            // `session/load` a real resume instead of a fresh session.
            //
            // A REQUEST, not a notification: `shutdown` aborts the writer task,
            // so a queued-but-unwritten notification frame is simply dropped —
            // the cancel would reach the runtime only when it happened to win
            // the race. Awaiting the reply proves the runtime processed it
            // before this connection goes away; a peer that never answers just
            // gets disconnected, which cancels the turn anyway.
            let cancel = live
                .transport
                .call("session/cancel", json!({ "sessionId": live.session_id }));
            if tokio::time::timeout(CANCEL_ON_CLOSE_BUDGET, cancel)
                .await
                .is_err()
            {
                tracing::debug!(
                    session_id = %live.session_id,
                    "dsh session/cancel did not answer before close; disconnecting anyway"
                );
            }
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

    /// An unconfigured manager answers `disabled` and spawns nothing, so these
    /// unit tests can hold a real adapter without a real DSH anywhere.
    fn adapter() -> DshAcpAdapter {
        DshAcpAdapter::new(Arc::new(DshRuntimeManager::new(
            PathBuf::from("/nonexistent/ccteam-home"),
            Arc::new(|_root, _owner| Err(anyhow::anyhow!("no enrollment in tests"))),
            Arc::new(|_root, _owner| Err(anyhow::anyhow!("no REST token in tests"))),
        )))
    }

    fn handle() -> ThreadHandle {
        ThreadHandle {
            vendor: AgentVendor::Dsh,
            mode: ExecutionMode::Chat,
            identity: "s1".to_string(),
            started_at: chrono::Utc::now(),
            raw_extras: serde_json::Value::Null,
        }
    }

    /// "One identity, one `dsh web` process" holds because every consumer shares
    /// ONE manager. The adapter must therefore keep the instance it was handed —
    /// building its own would supervise a second child for the same home.
    #[test]
    fn adapter_keeps_the_runtime_manager_it_was_built_with() {
        let manager = Arc::new(DshRuntimeManager::new(
            PathBuf::from("/nonexistent/ccteam-home"),
            Arc::new(|_root, _owner| anyhow::bail!("no enrollment in tests")),
            Arc::new(|_root, _owner| anyhow::bail!("no REST token in tests")),
        ));
        let adapter = DshAcpAdapter::new(Arc::clone(&manager));
        assert!(Arc::ptr_eq(adapter.runtime(), &manager));
        assert!(
            Arc::ptr_eq(adapter.clone().runtime(), &manager),
            "cloning the adapter (the factory hands out clones) keeps the same manager"
        );
    }

    #[test]
    fn mode_tokens_resolve_to_vendor_preset_ids() {
        assert_eq!(mode_agent_preset(None).unwrap(), "standard");
        assert_eq!(mode_agent_preset(Some("")).unwrap(), "standard");
        assert_eq!(mode_agent_preset(Some("ptc")).unwrap(), "code");
        assert_eq!(mode_agent_preset(Some("PTC")).unwrap(), "code");
        assert_eq!(mode_agent_preset(Some("code")).unwrap(), "code");
        assert_eq!(mode_agent_preset(Some("standard")).unwrap(), "standard");
        assert_eq!(mode_agent_preset(Some("minimal")).unwrap(), "minimal");
        assert_eq!(mode_agent_preset(Some("creator")).unwrap(), "cordis");
        assert_eq!(mode_agent_preset(Some("cordis")).unwrap(), "cordis");
        let err = mode_agent_preset(Some("turbo")).unwrap_err().to_string();
        assert!(err.contains("turbo") && err.contains("standard"), "{err}");
    }

    #[test]
    fn name_and_vendor_are_dsh() {
        let a = adapter();
        assert_eq!(a.name(), DSH_ACP_ADAPTER_NAME);
        assert_eq!(a.vendor(), AgentVendor::Dsh);
    }

    #[tokio::test]
    async fn resume_thread_is_not_implemented_for_cold_id() {
        let a = adapter();
        let err = a.resume_thread("some-vendor-uuid").await.unwrap_err();
        assert!(matches!(err, HarnessError::NotImplemented { .. }));
    }

    #[test]
    fn event_attachment_is_rebuildable() {
        let a = adapter();
        assert_eq!(a.event_attachment(), EventAttachment::Rebuildable);
    }

    #[tokio::test]
    async fn rebuild_tool_surface_needs_lossless_respawn() {
        let a = adapter();
        let outcome = a.rebuild_tool_surface(&handle()).await.unwrap();
        let ToolSurfaceRebuild::RespawnRequired { reason } = outcome;
        assert!(reason.contains("lossless"));
        assert!(reason.contains("session/load"));
    }

    #[tokio::test]
    async fn handle_directive_rejects_private_state_commands() {
        let a = adapter();
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
