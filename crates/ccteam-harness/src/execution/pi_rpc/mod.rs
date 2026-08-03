//! Long-lived adapter for Pi's stable `pi --mode rpc` stdio surface.

pub mod bridge;
pub mod protocol;
pub mod role;
pub mod spawn_spec;
pub mod translate;
pub mod transport;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};

use bridge::{
    auto_allows_tool, materialize_bridge, parse_ready_status, MAX_PERMISSION_ENVELOPE_BYTES,
    PERMISSION_DIALOG_TITLE,
};
pub use bridge::{
    bridge_source, PiApprovalDecision, PiDialogKind, PiDialogRequest, PiDialogResponse,
    PiInteractionResolver, REQUIRED_MCP_TOOL_NAMES,
};
use protocol::{
    response_data, PiAvailableModels, PiContextUsage, PiEvent, PiExtensionUiRequest, PiModel,
    PiSessionState, PiSessionStats, PiThinkingLevels,
};
use role::resolve_role;
pub use role::{PiRoleDocument, PiRoleReader};
pub use spawn_spec::PI_BIN_ENV;
use spawn_spec::{
    build_spawn_spec, deterministic_session_id, PiSessionArg, PiSpawnInput, PiSpawnSpec,
};
use translate::PiTurnTranslator;
use transport::{PiTransport, PiTransportEvent};

use crate::execution::fs_atomic::atomic_write_durable;
use crate::execution::session_status::write_status_file;
use crate::{
    AgentSpecBrief, AgentVendor, ApprovalIR, ApprovalKind, ApprovalScope, ChoiceOption,
    ChoicePrompt, ContextSource, ContextUsage, Directive, DirectiveOutcome, ExecutionMode,
    HarnessAdapter, HarnessError, SessionTitleTarget, SpawnCtx, ThreadErrorEvent, ThreadEvent,
    ThreadHandle, ThreadStatus, TitleSync, TurnDisposition, TurnId, TurnInput, TurnRouting,
    TurnSubmission,
};

pub const PI_RPC_ADAPTER_NAME: &str = "pi-rpc";
const MIN_PI_VERSION: (u64, u64, u64) = (0, 83, 0);
static PI_TURN_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct PiRpcAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
    known: Arc<StdMutex<HashMap<String, StartRecipe>>>,
    role_reader: PiRoleReader,
    interaction_resolver: Arc<StdMutex<Option<Arc<dyn PiInteractionResolver>>>>,
}

impl std::fmt::Debug for PiRpcAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiRpcAdapter").finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct StartRecipe {
    spec: AgentSpecBrief,
    ctx: SpawnCtx,
}

struct LiveSession {
    identity: String,
    sid: String,
    project_dir: PathBuf,
    transport: RwLock<Arc<PiTransport>>,
    config: Mutex<LiveConfig>,
    translate: StdMutex<TranslateState>,
    event_tx: broadcast::Sender<ThreadEvent>,
    event_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    restart_lock: Mutex<()>,
    settling: AtomicBool,
    settled: Notify,
    cached_status: StdMutex<ThreadStatus>,
    outstanding_ui: StdMutex<HashSet<String>>,
}

#[derive(Debug, Clone)]
struct LiveConfig {
    role: String,
    role_prompt_path: Option<PathBuf>,
    role_prompt_sha: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    session_file: PathBuf,
    pi_version: String,
    ctx: SpawnCtx,
}

#[derive(Default)]
struct TranslateState {
    translator: PiTurnTranslator,
    completion: Option<Arc<CompletionGate>>,
}

struct CompletionGate {
    reservations: AtomicUsize,
    sealed: AtomicBool,
    drained: Notify,
}

struct CompletionPermit {
    gate: Arc<CompletionGate>,
}

impl CompletionGate {
    fn new_reserved() -> (Arc<Self>, CompletionPermit) {
        let gate = Arc::new(Self {
            reservations: AtomicUsize::new(1),
            sealed: AtomicBool::new(false),
            drained: Notify::new(),
        });
        let permit = CompletionPermit {
            gate: Arc::clone(&gate),
        };
        (gate, permit)
    }

    fn reserve(self: &Arc<Self>) -> Option<CompletionPermit> {
        if self.sealed.load(Ordering::Acquire) {
            return None;
        }
        self.reservations.fetch_add(1, Ordering::AcqRel);
        if self.sealed.load(Ordering::Acquire) {
            self.release();
            return None;
        }
        Some(CompletionPermit {
            gate: Arc::clone(self),
        })
    }

    fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
        if self.reservations.load(Ordering::Acquire) == 0 {
            self.drained.notify_waiters();
        }
    }

    fn release(&self) {
        if self.reservations.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }

    async fn wait_drained(&self) {
        loop {
            let notified = self.drained.notified();
            if self.reservations.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for CompletionPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiStateSidecar {
    session_id: String,
    session_file: PathBuf,
    pi_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role_prompt_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(default)]
    role: String,
}

struct ReadyTransport {
    transport: Arc<PiTransport>,
    events: broadcast::Receiver<PiTransportEvent>,
    state: PiSessionState,
    models: PiAvailableModels,
    levels: PiThinkingLevels,
    version: String,
}

impl PiRpcAdapter {
    pub fn new(role_reader: PiRoleReader) -> Self {
        Self {
            live: Arc::new(StdMutex::new(HashMap::new())),
            known: Arc::new(StdMutex::new(HashMap::new())),
            role_reader,
            interaction_resolver: Arc::new(StdMutex::new(None)),
        }
    }

    pub fn set_interaction_resolver(&self, resolver: Arc<dyn PiInteractionResolver>) {
        *self.interaction_resolver.lock().unwrap() = Some(resolver);
    }

    fn lookup(&self, identity: &str) -> Option<Arc<LiveSession>> {
        self.live.lock().unwrap().get(identity).cloned()
    }

    async fn spawn_ready(
        &self,
        spec: PiSpawnSpec,
        expected_identity: &str,
        sid: &str,
        expected_session_file: Option<&Path>,
        expected_model: Option<&str>,
        expected_effort: Option<&str>,
    ) -> Result<ReadyTransport, HarnessError> {
        let version = probe_version(&spec.bin).await?;
        let transport = PiTransport::connect_stdio(&spec)
            .await
            .map_err(HarnessError::SpawnFailed)?;
        let mut events = transport.take_startup_events();
        let resolver = self.interaction_resolver.lock().unwrap().clone();
        let handshake = async {
            let state_fut = transport.request(json!({"type":"get_state"}));
            let models_fut = transport.request(json!({"type":"get_available_models"}));
            let levels_fut = transport.request(json!({"type":"get_available_thinking_levels"}));
            let (state, models, levels) = tokio::join!(state_fut, models_fut, levels_fut);
            let state: PiSessionState = response_data(state?)?;
            let models: PiAvailableModels = response_data(models?)?;
            let levels: PiThinkingLevels = response_data(levels?)?;
            if models
                .models
                .iter()
                .any(|model| model.id.is_empty() || model.provider.is_empty())
            {
                return Err("Pi model feature probe returned an invalid model".to_string());
            }
            if levels.levels.is_empty() {
                return Err("Pi thinking-level feature probe returned no levels".to_string());
            }
            Ok::<_, String>((state, models, levels))
        };
        let ready_gate = wait_for_bridge_ready(sid, Arc::clone(&transport), &mut events, resolver);
        let (state, models, levels) = match tokio::time::timeout(Duration::from_secs(30), async {
            let (handshake, _) = tokio::try_join!(handshake, ready_gate)?;
            Ok::<_, String>(handshake)
        })
        .await
        {
            Ok(Ok(state)) => state,
            Ok(Err(error)) => {
                let stderr = transport.stderr_tail();
                let _ = transport.close().await;
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi RPC feature handshake failed (requires Pi >= 0.83.0): {error}; stderr: {stderr}"
                )));
            }
            Err(_) => {
                let stderr = transport.stderr_tail();
                let _ = transport.close().await;
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi RPC feature handshake timed out; stderr: {stderr}"
                )));
            }
        };
        if state.session_id != expected_identity {
            let _ = transport.close().await;
            return Err(HarnessError::SpawnFailed(format!(
                "Pi session identity mismatch: expected `{expected_identity}`, got `{}`",
                state.session_id
            )));
        }
        if let Some(expected) = expected_session_file {
            let actual = state.session_file.as_deref().map(Path::new);
            if actual != Some(expected) {
                let _ = transport.close().await;
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi resume file mismatch: expected `{}`, got `{}`",
                    expected.display(),
                    actual
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "none".to_string())
                )));
            }
        }
        if let Some(expected) = expected_model {
            let actual = state.model.as_ref().map(PiModel::canonical_id);
            if actual.as_deref() != Some(expected) {
                let _ = transport.close().await;
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi rejected or changed explicit model `{expected}` (effective `{}`)",
                    actual.as_deref().unwrap_or("none")
                )));
            }
        }
        if let Some(expected) = expected_effort {
            if state.thinking_level != expected {
                let _ = transport.close().await;
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi rejected or clamped explicit effort `{expected}` (effective `{}`)",
                    state.thinking_level
                )));
            }
        }
        Ok(ReadyTransport {
            transport,
            events,
            state,
            models,
            levels,
            version,
        })
    }

    fn spawn_event_pump(
        live: Arc<LiveSession>,
        mut input: broadcast::Receiver<PiTransportEvent>,
        resolver: Option<Arc<dyn PiInteractionResolver>>,
    ) {
        let live_for_task = Arc::clone(&live);
        let task = tokio::spawn(async move {
            let transport = live_for_task.transport.read().await.clone();
            loop {
                let output = match input.recv().await {
                    Ok(PiTransportEvent::Event(PiEvent::ExtensionUiRequest(request))) => {
                        if let Err(error) = resolve_ui_request(
                            &live_for_task.sid,
                            Arc::clone(&transport),
                            request,
                            resolver.clone(),
                            Some(&live_for_task.outstanding_ui),
                        )
                        .await
                        {
                            tracing::warn!(sid = %live_for_task.sid, %error, "Pi extension UI resolution failed");
                        }
                        continue;
                    }
                    Ok(PiTransportEvent::Event(PiEvent::ExtensionError { event, error })) => {
                        let _ = live_for_task
                            .event_tx
                            .send(ThreadEvent::Error(ThreadErrorEvent {
                                kind: "protocol".to_string(),
                                message: format!("Pi extension `{event}` failed: {error}"),
                            }));
                        continue;
                    }
                    Ok(PiTransportEvent::Event(event)) => live_for_task
                        .translate
                        .lock()
                        .unwrap()
                        .translator
                        .ingest(event),
                    Ok(PiTransportEvent::Error(message)) => {
                        if let Some(resolver) = resolver.as_ref() {
                            resolver.cancel_sid(&live_for_task.sid).await;
                        }
                        live_for_task.outstanding_ui.lock().unwrap().clear();
                        let mut state = live_for_task.translate.lock().unwrap();
                        let output = state.translator.transport_failed(message.clone());
                        if output.events.is_empty() {
                            let _ =
                                live_for_task
                                    .event_tx
                                    .send(ThreadEvent::Error(ThreadErrorEvent {
                                        kind: "protocol".to_string(),
                                        message,
                                    }));
                            return;
                        }
                        output
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let mut state = live_for_task.translate.lock().unwrap();
                        state.translator.transport_failed(format!(
                            "Pi RPC event subscriber lagged by {count} records"
                        ))
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let gate = if output.settled {
                    live_for_task.settling.store(true, Ordering::Release);
                    let gate = live_for_task.translate.lock().unwrap().completion.take();
                    if let Some(gate) = &gate {
                        gate.seal();
                    }
                    gate
                } else {
                    None
                };
                if let Some(gate) = gate {
                    gate.wait_drained().await;
                }
                for event in output.events {
                    let terminal = matches!(
                        event,
                        ThreadEvent::TurnCompleted { .. } | ThreadEvent::TurnFailed { .. }
                    );
                    let _ = live_for_task.event_tx.send(event);
                    if terminal {
                        let status = live_for_task.cached_status.lock().unwrap().clone();
                        write_status_file(&live_for_task.project_dir, &live_for_task.sid, &status);
                    }
                }
                if output.settled {
                    live_for_task.settling.store(false, Ordering::Release);
                    live_for_task.settled.notify_waiters();
                }
                if !transport.is_alive() {
                    return;
                }
            }
        });
        *live.event_task.lock().unwrap() = Some(task);
    }

    async fn wait_not_settling(live: &LiveSession) {
        loop {
            let notified = live.settled.notified();
            if !live.settling.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    async fn start_impl(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if ctx.remote.is_some() {
            return Err(HarnessError::NotImplemented {
                reason: "remote execution is not yet supported for Pi RPC".to_string(),
            });
        }
        let identity = deterministic_session_id(&ctx.sid);
        if let Some(live) = self.lookup(&identity) {
            if live.transport.read().await.is_alive() {
                return Ok(thread_handle(
                    &live,
                    pi_state_path(&ctx.project_dir, &ctx.sid),
                ));
            }
        }

        let sidecar_path = pi_state_path(&ctx.project_dir, &ctx.sid);
        let sidecar = read_sidecar(&sidecar_path)?;
        if let Some(sidecar) = &sidecar {
            if sidecar.session_id != identity {
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi sidecar identity mismatch: expected `{identity}`, got `{}`",
                    sidecar.session_id
                )));
            }
        }
        let role = resolve_role(&self.role_reader, &ctx.project_dir, &ctx.sid, &spec.role)?;
        let model = ctx
            .model_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| sidecar.as_ref().and_then(|state| state.model.clone()))
            .or_else(|| role.model.clone());
        let effort = ctx
            .effort
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| sidecar.as_ref().and_then(|state| state.effort.clone()))
            .or_else(|| role.effort.clone());
        if model.as_deref().is_some_and(|value| !value.contains('/')) {
            return Err(HarnessError::SpawnFailed(
                "Pi model must be canonical provider/model".to_string(),
            ));
        }
        let session = match sidecar.as_ref() {
            Some(state) if state.session_file.exists() => PiSessionArg::Resume {
                session_file: state.session_file.clone(),
            },
            Some(state) if turns_have_history(&ctx.project_dir, &ctx.sid) => {
                return Err(HarnessError::SpawnFailed(format!(
                    "Pi native session missing: {}",
                    state.session_file.display()
                )))
            }
            _ => PiSessionArg::Fresh {
                session_id: identity.clone(),
            },
        };
        let expected_session_file = match &session {
            PiSessionArg::Resume { session_file } => Some(session_file.clone()),
            PiSessionArg::Fresh { .. } => None,
        };
        let spawn = build_spawn_spec(
            ctx,
            PiSpawnInput {
                session,
                bridge_extension: materialize_bridge()?,
                system_prompt: role.prompt_path.clone(),
                model: model.clone(),
                effort: effort.clone(),
            },
        );
        let ready = self
            .spawn_ready(
                spawn,
                &identity,
                &ctx.sid,
                expected_session_file.as_deref(),
                model.as_deref(),
                effort.as_deref(),
            )
            .await?;
        let efforts = ready.levels.levels.clone();
        crate::model_catalog::record_vendor_models_best_effort(
            "pi",
            "Pi RPC get_available_models",
            ready
                .models
                .models
                .iter()
                .map(|model| crate::model_catalog::CatalogModel {
                    id: model.canonical_id(),
                    display_name: (!model.name.trim().is_empty()).then(|| model.name.clone()),
                    efforts: efforts.clone(),
                })
                .collect(),
        );
        let session_file = ready
            .state
            .session_file
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                HarnessError::SpawnFailed("Pi get_state returned no absolute sessionFile".into())
            })?;
        let effective_model = ready.state.model.as_ref().map(PiModel::canonical_id);
        let effective_effort = Some(ready.state.thinking_level.clone());
        let config = LiveConfig {
            role: role.role.clone(),
            role_prompt_path: role.prompt_path.clone(),
            role_prompt_sha: role.prompt_sha.clone(),
            model: effective_model.clone(),
            effort: effective_effort.clone(),
            session_file: session_file.clone(),
            pi_version: ready.version.clone(),
            ctx: ctx.clone(),
        };
        write_sidecar(&sidecar_path, &identity, &config)?;
        let status = ThreadStatus {
            model: effective_model,
            effort: effective_effort,
            context: None,
            goal: None,
        };
        let (event_tx, _) = broadcast::channel(256);
        let live = Arc::new(LiveSession {
            identity: identity.clone(),
            sid: ctx.sid.clone(),
            project_dir: ctx.project_dir.clone(),
            transport: RwLock::new(ready.transport),
            config: Mutex::new(config),
            translate: StdMutex::new(TranslateState::default()),
            event_tx,
            event_task: StdMutex::new(None),
            restart_lock: Mutex::new(()),
            settling: AtomicBool::new(false),
            settled: Notify::new(),
            cached_status: StdMutex::new(status),
            outstanding_ui: StdMutex::new(HashSet::new()),
        });
        Self::spawn_event_pump(
            Arc::clone(&live),
            ready.events,
            self.interaction_resolver.lock().unwrap().clone(),
        );
        self.live
            .lock()
            .unwrap()
            .insert(identity.clone(), Arc::clone(&live));
        self.known.lock().unwrap().insert(
            identity,
            StartRecipe {
                spec: spec.clone(),
                ctx: ctx.clone(),
            },
        );
        Ok(thread_handle(&live, sidecar_path))
    }

    async fn restart_role(
        &self,
        live: Arc<LiveSession>,
        role_name: &str,
    ) -> Result<(), HarnessError> {
        let _restart = live.restart_lock.lock().await;
        Self::wait_not_settling(&live).await;
        if live
            .translate
            .lock()
            .unwrap()
            .translator
            .active_turn_id()
            .is_some()
        {
            return Err(HarnessError::SubmitFailed(
                "Pi role cannot change while a turn is active; retry when settled".into(),
            ));
        }
        let mut config = live.config.lock().await;
        let new_role = resolve_role(&self.role_reader, &live.project_dir, &live.sid, role_name)?;
        if !config.session_file.exists() {
            return Err(HarnessError::SpawnFailed(format!(
                "Pi native session missing: {}",
                config.session_file.display()
            )));
        }
        let old = config.clone();
        if let Some(task) = live.event_task.lock().unwrap().take() {
            task.abort();
        }
        let old_transport = live.transport.read().await.clone();
        cancel_outstanding_ui(&live, &old_transport).await;
        let resolver = { self.interaction_resolver.lock().unwrap().clone() };
        if let Some(resolver) = resolver {
            resolver.cancel_sid(&live.sid).await;
        }
        old_transport
            .close()
            .await
            .map_err(HarnessError::ShutdownFailed)?;

        let candidate_spec = build_spawn_spec(
            &config.ctx,
            PiSpawnInput {
                session: PiSessionArg::Resume {
                    session_file: config.session_file.clone(),
                },
                bridge_extension: materialize_bridge()?,
                system_prompt: new_role.prompt_path.clone(),
                model: config.model.clone(),
                effort: config.effort.clone(),
            },
        );
        match self
            .spawn_ready(
                candidate_spec,
                &live.identity,
                &live.sid,
                Some(&config.session_file),
                config.model.as_deref(),
                config.effort.as_deref(),
            )
            .await
        {
            Ok(ready) => {
                let ready_events = ready.events;
                *live.transport.write().await = ready.transport;
                config.role = new_role.role;
                config.role_prompt_path = new_role.prompt_path;
                config.role_prompt_sha = new_role.prompt_sha;
                config.pi_version = ready.version;
                write_sidecar(
                    &pi_state_path(&live.project_dir, &live.sid),
                    &live.identity,
                    &config,
                )?;
                drop(config);
                Self::spawn_event_pump(
                    Arc::clone(&live),
                    ready_events,
                    self.interaction_resolver.lock().unwrap().clone(),
                );
                Ok(())
            }
            Err(candidate_error) => {
                let rollback_spec = build_spawn_spec(
                    &old.ctx,
                    PiSpawnInput {
                        session: PiSessionArg::Resume {
                            session_file: old.session_file.clone(),
                        },
                        bridge_extension: materialize_bridge()?,
                        system_prompt: old.role_prompt_path.clone(),
                        model: old.model.clone(),
                        effort: old.effort.clone(),
                    },
                );
                match self
                    .spawn_ready(
                        rollback_spec,
                        &live.identity,
                        &live.sid,
                        Some(&old.session_file),
                        old.model.as_deref(),
                        old.effort.as_deref(),
                    )
                    .await
                {
                    Ok(rollback) => {
                        let rollback_events = rollback.events;
                        *live.transport.write().await = rollback.transport;
                        *config = old;
                        drop(config);
                        Self::spawn_event_pump(
                            Arc::clone(&live),
                            rollback_events,
                            self.interaction_resolver.lock().unwrap().clone(),
                        );
                        Err(HarnessError::SpawnFailed(format!(
                            "Pi role restart failed and previous role was restored: {candidate_error}"
                        )))
                    }
                    Err(rollback_error) => Err(HarnessError::ThreadDied(format!(
                        "Pi role restart failed ({candidate_error}); rollback also failed ({rollback_error})"
                    ))),
                }
            }
        }
    }

    async fn apply_model(&self, live: &Arc<LiveSession>, model: &str) -> Result<(), HarnessError> {
        let (provider, model_id) = model.split_once('/').ok_or_else(|| {
            HarnessError::SubmitFailed("Pi model must be canonical provider/model".into())
        })?;
        let transport = live.transport.read().await.clone();
        response_data::<PiModel>(
            transport
                .request(json!({"type":"set_model", "provider":provider, "modelId":model_id}))
                .await
                .map_err(HarnessError::SubmitFailed)?,
        )
        .map_err(HarnessError::SubmitFailed)?;
        let state: PiSessionState = response_data(
            transport
                .request(json!({"type":"get_state"}))
                .await
                .map_err(HarnessError::SubmitFailed)?,
        )
        .map_err(HarnessError::SubmitFailed)?;
        let actual = state.model.as_ref().map(PiModel::canonical_id);
        if actual.as_deref() != Some(model) {
            return Err(HarnessError::SubmitFailed(format!(
                "Pi rejected or changed model `{model}` (effective `{}`)",
                actual.as_deref().unwrap_or("none")
            )));
        }
        let mut config = live.config.lock().await;
        config.model = actual.clone();
        write_sidecar(
            &pi_state_path(&live.project_dir, &live.sid),
            &live.identity,
            &config,
        )?;
        live.cached_status.lock().unwrap().model = actual;
        Ok(())
    }

    async fn apply_effort(
        &self,
        live: &Arc<LiveSession>,
        effort: &str,
    ) -> Result<(), HarnessError> {
        let transport = live.transport.read().await.clone();
        let levels: PiThinkingLevels = response_data(
            transport
                .request(json!({"type":"get_available_thinking_levels"}))
                .await
                .map_err(HarnessError::SubmitFailed)?,
        )
        .map_err(HarnessError::SubmitFailed)?;
        if !levels.levels.iter().any(|level| level == effort) {
            return Err(HarnessError::SubmitFailed(format!(
                "Pi does not support effort `{effort}` for the current model"
            )));
        }
        let response = transport
            .request(json!({"type":"set_thinking_level", "level":effort}))
            .await
            .map_err(HarnessError::SubmitFailed)?;
        if !response.success {
            return Err(HarnessError::SubmitFailed(
                response
                    .error
                    .unwrap_or_else(|| "Pi effort update rejected".to_string()),
            ));
        }
        let state: PiSessionState = response_data(
            transport
                .request(json!({"type":"get_state"}))
                .await
                .map_err(HarnessError::SubmitFailed)?,
        )
        .map_err(HarnessError::SubmitFailed)?;
        if state.thinking_level != effort {
            return Err(HarnessError::SubmitFailed(format!(
                "Pi rejected or clamped effort `{effort}` (effective `{}`)",
                state.thinking_level
            )));
        }
        let mut config = live.config.lock().await;
        config.effort = Some(effort.to_string());
        write_sidecar(
            &pi_state_path(&live.project_dir, &live.sid),
            &live.identity,
            &config,
        )?;
        live.cached_status.lock().unwrap().effort = Some(effort.to_string());
        Ok(())
    }
}

async fn wait_for_bridge_ready(
    sid: &str,
    transport: Arc<PiTransport>,
    events: &mut broadcast::Receiver<PiTransportEvent>,
    resolver: Option<Arc<dyn PiInteractionResolver>>,
) -> Result<Vec<String>, String> {
    loop {
        match events.recv().await {
            Ok(PiTransportEvent::Event(PiEvent::ExtensionUiRequest(request))) => {
                if request.method == "setStatus" {
                    let status_key = request
                        .payload
                        .get("statusKey")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let status_text = request.payload.get("statusText").and_then(Value::as_str);
                    if let Some(names) = parse_ready_status(status_key, status_text)? {
                        return Ok(names);
                    }
                }
                resolve_ui_request(sid, Arc::clone(&transport), request, resolver.clone(), None)
                    .await?;
            }
            Ok(PiTransportEvent::Event(PiEvent::ExtensionError { event, error })) => {
                return Err(format!("Pi bridge extension `{event}` failed: {error}"));
            }
            Ok(PiTransportEvent::Error(error)) => return Err(error),
            Ok(PiTransportEvent::Event(_)) => {}
            Err(broadcast::error::RecvError::Lagged(count)) => {
                return Err(format!(
                    "Pi bridge readiness subscriber lagged by {count} records"
                ));
            }
            Err(broadcast::error::RecvError::Closed) => {
                return Err("Pi child closed before bridge readiness".to_string());
            }
        }
    }
}

async fn resolve_ui_request(
    sid: &str,
    transport: Arc<PiTransport>,
    request: PiExtensionUiRequest,
    resolver: Option<Arc<dyn PiInteractionResolver>>,
    outstanding: Option<&StdMutex<HashSet<String>>>,
) -> Result<(), String> {
    let is_dialog = matches!(
        request.method.as_str(),
        "select" | "confirm" | "input" | "editor"
    );
    if is_dialog {
        if let Some(outstanding) = outstanding {
            outstanding.lock().unwrap().insert(request.id.clone());
        }
    }
    let result = resolve_ui_request_inner(sid, &transport, &request, resolver).await;
    if is_dialog {
        if let Some(outstanding) = outstanding {
            outstanding.lock().unwrap().remove(&request.id);
        }
    }
    result
}

async fn resolve_ui_request_inner(
    sid: &str,
    transport: &Arc<PiTransport>,
    request: &PiExtensionUiRequest,
    resolver: Option<Arc<dyn PiInteractionResolver>>,
) -> Result<(), String> {
    if request.method == "confirm"
        && request.payload.get("title").and_then(Value::as_str) == Some(PERMISSION_DIALOG_TITLE)
    {
        return resolve_permission_request(sid, transport, request, resolver).await;
    }

    let title = request
        .payload
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Pi extension request")
        .to_string();
    let kind = match request.method.as_str() {
        "select" => PiDialogKind::Select {
            options: request
                .payload
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        },
        "confirm" => PiDialogKind::Confirm {
            message: request
                .payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "input" => PiDialogKind::Input {
            placeholder: request
                .payload
                .get("placeholder")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        "editor" => PiDialogKind::Editor {
            prefill: request
                .payload
                .get("prefill")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        // These Pi RPC UI methods are fire-and-forget. The adapter does not
        // reinterpret their content as chat or prompt material.
        "notify" | "setStatus" | "setWidget" | "setTitle" | "set_editor_text" => return Ok(()),
        _ => {
            return transport
                .send(json!({"type":"extension_ui_response", "id":request.id, "cancelled":true}))
                .await;
        }
    };
    let dialog = PiDialogRequest {
        request_id: request.id.clone(),
        title,
        kind,
    };
    let timeout = ui_timeout(request);
    let response = match resolver.as_ref() {
        Some(resolver) => {
            tokio::select! {
                result = tokio::time::timeout(timeout, resolver.resolve_dialog(sid, &dialog)) => {
                    match result {
                        Ok(response) => response,
                        Err(_) => PiDialogResponse::Cancelled,
                    }
                }
                _ = transport.wait_closed() => {
                    resolver.cancel_sid(sid).await;
                    PiDialogResponse::Cancelled
                }
            }
        }
        None => PiDialogResponse::Cancelled,
    };
    let response = match response {
        PiDialogResponse::Value(value) => {
            json!({"type":"extension_ui_response", "id":request.id, "value":value})
        }
        PiDialogResponse::Confirmed(confirmed) => {
            json!({"type":"extension_ui_response", "id":request.id, "confirmed":confirmed})
        }
        PiDialogResponse::Cancelled => {
            json!({"type":"extension_ui_response", "id":request.id, "cancelled":true})
        }
    };
    if transport.is_alive() {
        transport.send(response).await
    } else {
        Ok(())
    }
}

async fn resolve_permission_request(
    sid: &str,
    transport: &Arc<PiTransport>,
    request: &PiExtensionUiRequest,
    resolver: Option<Arc<dyn PiInteractionResolver>>,
) -> Result<(), String> {
    let message = request
        .payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut deny_reason = None;
    let confirmed = if message.len() > MAX_PERMISSION_ENVELOPE_BYTES {
        deny_reason = Some("payload too large".to_string());
        false
    } else {
        let envelope: Value = match serde_json::from_str(message) {
            Ok(value) => value,
            Err(error) => {
                deny_reason = Some(format!("invalid permission envelope: {error}"));
                Value::Null
            }
        };
        let tool_call_id = envelope
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or("");
        let tool_name = envelope
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let input = envelope.get("input").cloned().unwrap_or(Value::Null);
        if tool_call_id.is_empty() || tool_name.is_empty() {
            deny_reason = Some("permission envelope missing tool identity".to_string());
            false
        } else if auto_allows_tool(tool_name) {
            true
        } else if let Some(resolver) = resolver.as_ref() {
            let approval = ApprovalIR {
                req_id: format!("{sid}/{tool_call_id}"),
                vendor: AgentVendor::Pi,
                kind: ApprovalKind::ToolUse,
                risk: resolver.classify_tool_risk(tool_name, &input),
                scope: ApprovalScope::Once,
                summary: Some(tool_name.to_string()),
                raw: json!({
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "input": input,
                }),
            };
            let decision = tokio::select! {
                result = tokio::time::timeout(ui_timeout(request), resolver.resolve_approval(sid, &approval)) => {
                    match result {
                        Ok(decision) => decision,
                        Err(_) => PiApprovalDecision::Deny("approval timed out".to_string()),
                    }
                }
                _ = transport.wait_closed() => {
                    resolver.cancel_sid(sid).await;
                    PiApprovalDecision::Deny("session closed".to_string())
                }
            };
            match decision {
                PiApprovalDecision::Allow => true,
                PiApprovalDecision::Deny(reason) => {
                    deny_reason = Some(reason);
                    false
                }
            }
        } else {
            deny_reason = Some("HITL approval resolver unavailable".to_string());
            false
        }
    };
    if let Some(reason) = deny_reason {
        tracing::info!(%sid, %reason, "Pi tool call denied");
    }
    if transport.is_alive() {
        transport
            .send(json!({
                "type":"extension_ui_response",
                "id":request.id,
                "confirmed":confirmed,
            }))
            .await
    } else {
        Ok(())
    }
}

fn ui_timeout(request: &PiExtensionUiRequest) -> Duration {
    request
        .payload
        .get("timeout")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .filter(|duration| !duration.is_zero())
        .unwrap_or_else(|| Duration::from_secs(120))
}

async fn cancel_outstanding_ui(live: &LiveSession, transport: &PiTransport) {
    let ids = live
        .outstanding_ui
        .lock()
        .unwrap()
        .drain()
        .collect::<Vec<_>>();
    for id in ids {
        let _ = transport
            .send(json!({"type":"extension_ui_response", "id":id, "cancelled":true}))
            .await;
    }
}

#[async_trait]
impl HarnessAdapter for PiRpcAdapter {
    fn name(&self) -> &'static str {
        PI_RPC_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Pi
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        self.start_impl(spec, ctx).await
    }

    async fn submit_turn_routed(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        let text = match input {
            TurnInput::UserText(text) => text,
            other => {
                return Err(HarnessError::SubmitFailed(format!(
                    "Pi RPC does not support turn input {other:?}"
                )))
            }
        };
        let live = self.lookup(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("Pi session {} is not live", h.identity))
        })?;
        Self::wait_not_settling(&live).await;
        let (turn_id, disposition, permit, command) = {
            let mut state = live.translate.lock().unwrap();
            if let Some(active) = state.translator.active_turn_id().map(str::to_string) {
                if routing == TurnRouting::Queue {
                    return Err(HarnessError::NotImplemented {
                        reason: "Pi follow_up shares the active agent_settled epoch; distinct queued canonical turns are not supported".into(),
                    });
                }
                let gate = state.completion.as_ref().ok_or_else(|| {
                    HarnessError::Io("Pi active turn is missing completion gate".into())
                })?;
                let permit = gate.reserve().ok_or_else(|| {
                    HarnessError::SubmitFailed("Pi turn is already settling".into())
                })?;
                (
                    active,
                    TurnDisposition::Injected,
                    permit,
                    json!({"type":"steer", "message":text}),
                )
            } else {
                let seq = PI_TURN_SEQ.fetch_add(1, Ordering::Relaxed);
                let turn_id = format!("pi-{}-{seq}", live.sid);
                let (gate, permit) = CompletionGate::new_reserved();
                state
                    .translator
                    .begin(turn_id.clone())
                    .map_err(HarnessError::SubmitFailed)?;
                state.completion = Some(gate);
                (
                    turn_id,
                    TurnDisposition::Started,
                    permit,
                    json!({"type":"prompt", "message":text}),
                )
            }
        };
        let transport = live.transport.read().await.clone();
        let response = transport.request(command).await;
        match response {
            Ok(response) if response.success => {
                let submission = match disposition {
                    TurnDisposition::Started => TurnSubmission::started(TurnId(turn_id)),
                    TurnDisposition::Injected => TurnSubmission::injected(TurnId(turn_id)),
                    TurnDisposition::Queued => unreachable!(),
                };
                Ok(submission.hold_completion(permit))
            }
            Ok(response) => {
                if disposition == TurnDisposition::Started {
                    let mut state = live.translate.lock().unwrap();
                    state.translator.cancel();
                    state.completion.take();
                }
                Err(HarnessError::SubmitFailed(response.error.unwrap_or_else(
                    || format!("Pi {} request rejected", response.command),
                )))
            }
            Err(error) => {
                if disposition == TurnDisposition::Started {
                    let mut state = live.translate.lock().unwrap();
                    state.translator.cancel();
                    state.completion.take();
                }
                Err(if transport.is_alive() {
                    HarnessError::SubmitFailed(error)
                } else {
                    HarnessError::ThreadDied(error)
                })
            }
        }
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let Some(live) = self.lookup(&h.identity) else {
            return Box::pin(futures::stream::empty());
        };
        let receiver = live.event_tx.subscribe();
        Box::pin(futures::stream::unfold(
            receiver,
            |mut receiver| async move {
                match receiver.recv().await {
                    Ok(event) => Some((event, receiver)),
                    Err(broadcast::error::RecvError::Lagged(count)) => Some((
                        ThreadEvent::Error(ThreadErrorEvent {
                            kind: "protocol".to_string(),
                            message: format!("Pi canonical event stream lagged by {count}"),
                        }),
                        receiver,
                    )),
                    Err(broadcast::error::RecvError::Closed) => None,
                }
            },
        ))
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        if let Some(live) = self.lookup(persistent_id) {
            if live.transport.read().await.is_alive() {
                return Ok(thread_handle(
                    &live,
                    pi_state_path(&live.project_dir, &live.sid),
                ));
            }
        }
        let recipe = self
            .known
            .lock()
            .unwrap()
            .get(persistent_id)
            .cloned()
            .ok_or_else(|| HarnessError::NotImplemented {
                reason: format!(
                    "Pi resume of `{persistent_id}` needs SpawnCtx; call start_thread, which is sidecar-resume aware"
                ),
            })?;
        self.start_impl(&recipe.spec, &recipe.ctx).await
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let live = self.live.lock().unwrap().remove(&h.identity);
        let Some(live) = live else {
            return Ok(());
        };
        let transport = live.transport.read().await.clone();
        cancel_outstanding_ui(&live, &transport).await;
        let resolver = { self.interaction_resolver.lock().unwrap().clone() };
        if let Some(resolver) = resolver {
            resolver.cancel_sid(&live.sid).await;
        }
        if let Some(task) = live.event_task.lock().unwrap().take() {
            task.abort();
        }
        transport
            .close()
            .await
            .map_err(HarnessError::ShutdownFailed)
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        directive: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        let live = self.lookup(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("Pi session {} is not live", h.identity))
        })?;
        let selected = directive
            .choice
            .as_ref()
            .and_then(|choice| choice.ids.first())
            .cloned()
            .unwrap_or_else(|| directive.args.trim().to_string());
        match directive.name.as_str() {
            "compact" => {
                let mut command = json!({"type":"compact"});
                if !directive.args.trim().is_empty() {
                    command["customInstructions"] = Value::String(directive.args);
                }
                let response = live
                    .transport
                    .read()
                    .await
                    .request(command)
                    .await
                    .map_err(HarnessError::SubmitFailed)?;
                if !response.success {
                    return Err(HarnessError::SubmitFailed(
                        response
                            .error
                            .unwrap_or_else(|| "Pi compaction rejected".into()),
                    ));
                }
                Ok(DirectiveOutcome::Done {
                    receipt: "Pi context compacted".to_string(),
                })
            }
            "model" if selected.is_empty() => {
                let response = live
                    .transport
                    .read()
                    .await
                    .request(json!({"type":"get_available_models"}))
                    .await
                    .map_err(HarnessError::SubmitFailed)?;
                let models: PiAvailableModels =
                    response_data(response).map_err(HarnessError::SubmitFailed)?;
                Ok(DirectiveOutcome::NeedsChoice(ChoicePrompt {
                    token: "pi-model".to_string(),
                    title: "Choose Pi model".to_string(),
                    options: models
                        .models
                        .into_iter()
                        .map(|model| ChoiceOption {
                            id: model.canonical_id(),
                            label: if model.name.is_empty() {
                                model.canonical_id()
                            } else {
                                model.name
                            },
                        })
                        .collect(),
                    multi: false,
                }))
            }
            "model" => {
                self.apply_model(&live, &selected).await?;
                Ok(DirectiveOutcome::Done {
                    receipt: format!("Pi model set to {selected}"),
                })
            }
            "effort" | "thinking" if selected.is_empty() => {
                let response = live
                    .transport
                    .read()
                    .await
                    .request(json!({"type":"get_available_thinking_levels"}))
                    .await
                    .map_err(HarnessError::SubmitFailed)?;
                let levels: PiThinkingLevels =
                    response_data(response).map_err(HarnessError::SubmitFailed)?;
                Ok(DirectiveOutcome::NeedsChoice(ChoicePrompt {
                    token: "pi-effort".to_string(),
                    title: "Choose Pi effort".to_string(),
                    options: levels
                        .levels
                        .into_iter()
                        .map(|level| ChoiceOption {
                            id: level.clone(),
                            label: level,
                        })
                        .collect(),
                    multi: false,
                }))
            }
            "effort" | "thinking" => {
                self.apply_effort(&live, &selected).await?;
                Ok(DirectiveOutcome::Done {
                    receipt: format!("Pi effort set to {selected}"),
                })
            }
            "role" => {
                self.restart_role(live, &selected).await?;
                Ok(DirectiveOutcome::Done {
                    receipt: if selected.is_empty() {
                        "Pi role cleared".to_string()
                    } else {
                        format!("Pi role set to {selected}")
                    },
                })
            }
            "interrupt" => {
                self.interrupt_turn(h).await?;
                Ok(DirectiveOutcome::Done {
                    receipt: "Pi turn interrupted".to_string(),
                })
            }
            "new" | "clear" => Ok(DirectiveOutcome::Redirect {
                hint: "Use /new to create a new ccteam sid".to_string(),
            }),
            name => {
                let response = live
                    .transport
                    .read()
                    .await
                    .request(json!({"type":"get_commands"}))
                    .await
                    .map_err(HarnessError::SubmitFailed)?;
                let commands = response_data::<Value>(response)
                    .map_err(HarnessError::SubmitFailed)?
                    .get("commands")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if commands
                    .iter()
                    .any(|command| command.get("name").and_then(Value::as_str) == Some(name))
                {
                    let text = if directive.args.trim().is_empty() {
                        format!("/{name}")
                    } else {
                        format!("/{name} {}", directive.args)
                    };
                    let turn = self.submit_turn(h, TurnInput::UserText(text)).await?;
                    Ok(DirectiveOutcome::Turn(turn))
                } else {
                    Ok(DirectiveOutcome::Rejected {
                        reason: format!(
                            "Pi RPC does not advertise `/{name}`; TUI-only commands are unavailable"
                        ),
                    })
                }
            }
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        let live = self.lookup(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("Pi session {} is not live", h.identity))
        })?;
        let transport = live.transport.read().await.clone();
        let query = async {
            tokio::join!(
                transport.request(json!({"type":"get_state"})),
                transport.request(json!({"type":"get_session_stats"}))
            )
        };
        let Ok((Ok(state), Ok(stats))) = tokio::time::timeout(Duration::from_secs(2), query).await
        else {
            return Ok(live.cached_status.lock().unwrap().clone());
        };
        let (Ok(state), Ok(stats)) = (
            response_data::<PiSessionState>(state),
            response_data::<PiSessionStats>(stats),
        ) else {
            return Ok(live.cached_status.lock().unwrap().clone());
        };
        let expected_session_file = live.config.lock().await.session_file.clone();
        if state.session_id != live.identity
            || stats.session_id != live.identity
            || Path::new(&stats.session_file) != expected_session_file
        {
            return Ok(live.cached_status.lock().unwrap().clone());
        }
        let status = ThreadStatus {
            model: state.model.as_ref().map(PiModel::canonical_id),
            effort: Some(state.thinking_level),
            context: stats.context_usage.map(context_from_pi),
            goal: None,
        };
        *live.cached_status.lock().unwrap() = status.clone();
        write_status_file(&live.project_dir, &live.sid, &status);
        Ok(status)
    }

    fn thread_is_live(&self, h: &ThreadHandle) -> bool {
        let Some(live) = self.lookup(&h.identity) else {
            return false;
        };
        live.transport
            .try_read()
            .map(|transport| transport.is_alive())
            // A brief config/restart lock is not evidence the child died.
            .unwrap_or(true)
    }

    async fn interrupt_turn(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let live = self.lookup(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("Pi session {} is not live", h.identity))
        })?;
        let response = live
            .transport
            .read()
            .await
            .request(json!({"type":"abort"}))
            .await
            .map_err(HarnessError::SubmitFailed)?;
        if response.success {
            Ok(())
        } else {
            Err(HarnessError::SubmitFailed(
                response
                    .error
                    .unwrap_or_else(|| "Pi abort rejected".to_string()),
            ))
        }
    }

    async fn set_session_title(
        &self,
        target: &SessionTitleTarget,
        title: &str,
    ) -> Result<TitleSync, HarnessError> {
        let Some(thread) = &target.thread else {
            return Ok(TitleSync::Deferred(
                "Pi title will sync on next resume".to_string(),
            ));
        };
        let live = self.lookup(&thread.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("Pi session {} is not live", thread.identity))
        })?;
        let response = live
            .transport
            .read()
            .await
            .request(json!({"type":"set_session_name", "name":title}))
            .await
            .map_err(HarnessError::SubmitFailed)?;
        if response.success {
            Ok(TitleSync::Pushed)
        } else {
            Err(HarnessError::SubmitFailed(
                response
                    .error
                    .unwrap_or_else(|| "Pi title update rejected".to_string()),
            ))
        }
    }
}

fn context_from_pi(context: PiContextUsage) -> ContextUsage {
    ContextUsage {
        used_tokens: context.tokens,
        window_tokens: context.context_window,
        source: ContextSource::Probed,
    }
}

fn thread_handle(live: &LiveSession, sidecar_path: PathBuf) -> ThreadHandle {
    ThreadHandle {
        vendor: AgentVendor::Pi,
        mode: ExecutionMode::Chat,
        identity: live.identity.clone(),
        started_at: Utc::now(),
        raw_extras: json!({
            "adapter": PI_RPC_ADAPTER_NAME,
            "protocol": "rpc",
            "vendor_uuid": live.identity,
            "sid": live.sid,
            "pi_state": sidecar_path,
        }),
    }
}

fn pi_state_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("pi-state.json")
}

fn read_sidecar(path: &Path) -> Result<Option<PiStateSidecar>, HarnessError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            HarnessError::SpawnFailed(format!("read {}: {error}", path.display()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(HarnessError::Io(format!(
            "read {}: {error}",
            path.display()
        ))),
    }
}

fn write_sidecar(path: &Path, identity: &str, config: &LiveConfig) -> Result<(), HarnessError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(&PiStateSidecar {
        session_id: identity.to_string(),
        session_file: config.session_file.clone(),
        pi_version: config.pi_version.clone(),
        role_prompt_sha: config.role_prompt_sha.clone(),
        model: config.model.clone(),
        effort: config.effort.clone(),
        role: config.role.clone(),
    })
    .map_err(|error| HarnessError::Io(format!("serialize Pi sidecar: {error}")))?;
    atomic_write_durable(path, &body).map_err(|error| HarnessError::Io(error.to_string()))
}

fn turns_have_history(project_dir: &Path, sid: &str) -> bool {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("turns.jsonl")
        .metadata()
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
}

async fn probe_version(bin: &str) -> Result<String, HarnessError> {
    let output = tokio::process::Command::new(bin)
        .arg("--version")
        .output()
        .await
        .map_err(|error| HarnessError::SpawnFailed(format!("run `{bin} --version`: {error}")))?;
    if !output.status.success() {
        return Err(HarnessError::SpawnFailed(format!(
            "`{bin} --version` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let version = raw
        .split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .find(|part| part.chars().filter(|character| *character == '.').count() >= 2)
        .ok_or_else(|| HarnessError::SpawnFailed(format!("unrecognised Pi version `{raw}`")))?;
    let mut numbers = version
        .split('.')
        .filter_map(|part| part.parse::<u64>().ok());
    let parsed = (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
    );
    if parsed < MIN_PI_VERSION {
        return Err(HarnessError::SpawnFailed(format!(
            "Pi {version} is unsupported; upgrade to >= 0.83.0"
        )));
    }
    Ok(version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_context_tokens_stay_unknown_and_probed() {
        let context = context_from_pi(PiContextUsage {
            tokens: None,
            context_window: 200_000,
        });
        assert_eq!(context.used_tokens, None);
        assert_eq!(context.window_tokens, 200_000);
        assert_eq!(context.source, ContextSource::Probed);
        assert_eq!(context.pct(), None);
    }
}
