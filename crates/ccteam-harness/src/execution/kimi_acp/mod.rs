//! Kimi Code ACP adapter — fifth harness vendor (`AgentVendor::Kimi`).
//!
//! Topology: **1 live session = 1 `kimi acp` child** (stdio JSON-RPC 2.0).
//! Zero PTY / pane / hook path. Wire SoT: `docs-local/versions/v0-9-5/prd.md`
//! (kimi 0.26.0, `references/kimi-code` is protocol reference only — never
//! vendored).
//!
//! Kimi specifics vs the opencode sibling this module mirrors:
//! - Model switches ride `session/set_model` (`{sessionId, modelId}`; kimi
//!   also accepts `session/set_config_option` with configId `model`, but the
//!   dedicated method is the pinned surface). `kimi acp` takes no model argv.
//! - The effort axis is kimi's `thinking` config option (ACP category
//!   `thought_level`): a select of the current model's own levels
//!   (`low|high|max`, plus `off` unless the model is always-thinking).
//!   Read from the `configOptions` snapshot (handshake + every
//!   `config_option_update`), written by `session/set_config_option`
//!   (`/model <id> <effort>`) — same shape the opencode sibling uses under
//!   its `effort` id. Spawn-time effort is still not wired (backlog): the
//!   session starts at kimi's own default and `/model` moves it.
//! - Kimi has no `--agent` persona face → sessions are roleless-only; kimi
//!   reads the project `AGENTS.md` natively (no prompt injection red line).
//! - Skip sessions use [`InboundPolicy::AutoAllowPermission`] — kimi's
//!   `session/request_permission` reverse RPC must never silently block the
//!   default (skip) posture.
//!
//! ## Known vendor limit: kimi hides its own turn failures (kimi 0.29.x)
//!
//! Verified on a live binary 2026-07-30. Kimi's ACP layer maps its internal
//! `turn.ended` reason to a stop reason as
//! `completed→end_turn`, `cancelled→cancelled`, `blocked→refusal`, and
//! **`failed→end_turn`** (only `provider.filtered` becomes `refusal`). The
//! error payload it holds at that moment — e.g.
//! `{"code":"provider.rate_limit","message":"429 The engine is currently
//! overloaded"}` after ten internal retries — goes to kimi's own rotating log
//! files and **never onto the wire or stderr** (its stderr carries node
//! warnings only). So a kimi turn that failed is indistinguishable, on every
//! channel ccteam can see, from one that answered: ccteam reports the partial
//! text as the reply because that is genuinely all the vendor said.
//!
//! ccteam's side of the contract is fixed generically — `stopReason` is parsed
//! and any non-clean reason becomes a real `TurnFailed`
//! ([`crate::execution::acp::AcpStopReason`]), which covers every ACP vendor
//! including kimi's `refusal`/`cancelled` paths. Recovering the *collapsed*
//! `failed` case would mean reading kimi's private session-log layout; that is
//! deliberately NOT done (unstable non-contract surface). The honest signal a
//! user gets meanwhile is the gateway's silence watchdog, which now reports the
//! turn's elapsed time and last observed activity. Fix belongs upstream.
//!
//! ## No account-level usage surface (why `account_usage` stays `None`)
//!
//! Kimi's ACP contract publishes two usage-shaped commands and NEITHER is
//! account-scoped: `/status` reports this session's model/permission/context
//! and `/usage` reports this session's token counters (`formatUsageReport` in
//! `references/kimi-code` `packages/acp-adapter/src/session.ts` — total /
//! current turn / per-model token totals, then the same context line). There
//! is no subscription tier, no rate-limit window, and no reset time anywhere on
//! the wire, so this adapter keeps the trait's default `None` and the vendor
//! simply does not appear in the usage map. The account-level numbers kimi
//! does publish live behind an authenticated HTTP endpoint against its
//! credential file, which is a different surface with a different owner (the
//! admin-gated vendor-quota probe), not something a session adapter reaches
//! into. Reading kimi's private session logs to synthesize one is a red line.
//!
//! ## Context usage arrives by pull, not push
//!
//! Kimi emits no `usage_update` and its `session/prompt` result carries no
//! usage at all (verified on a live 0.26.0 binary: the whole response is
//! `{"stopReason":"end_turn"}`), so there is nothing to fold at a turn
//! boundary. It does publish `status` in `available_commands_update`, and that
//! command reports real occupancy locally in ~15 ms — so this adapter carries
//! [`crate::execution::acp::KIMI_STATUS_PROBE`] and the shared turn runner
//! pulls after each turn. See that module for why the pull path is fenced the
//! way it is.

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

use crate::execution::acp::released_thread_status;
use crate::execution::acp::{
    route_acp_turn, AcpTurnRoute, AcpTurnRunner, AcpTurnTuning, KIMI_STATUS_PROBE,
};
use crate::execution::claude_common::unique_prompt_token;
use crate::execution::session_meta::read_session_meta;
use crate::execution::session_status::read_status_file;
use crate::{
    AgentSpecBrief, AgentVendor, ChoicePrompt, DetachOutcome, Directive, DirectiveOutcome,
    ExecutionMode, HarnessAdapter, HarnessError, PermissionMode, SpawnCtx, ThreadEvent,
    ThreadHandle, ThreadStatus, TurnId, TurnInput, TurnRouting, TurnSubmission,
};

use protocol::{
    acp_model_picker_options, known_efforts, pluck_model_info, pluck_session_id,
    split_trailing_effort, AcpModelOption, ModelInfo,
};
use spawn_spec::{build_argv, kimi_bin, KimiSpawnInput};
use translate::{apply_notification, SessionTranslateState};
use transport::{AcpTransport, InboundPolicy};

/// Max wait for the dispatcher to reach the turn boundary after the prompt
/// response, before finalizing anyway (best-effort if `turn_completed` is
/// ever absent). The boundary is normally already signalled by then.
const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);

/// Adapter name — stable id for handles / logs / tests.
pub const KIMI_ACP_ADAPTER_NAME: &str = "kimi-acp";

const EVENT_BUFFER: usize = 256;

struct LiveSession {
    transport: Arc<AcpTransport>,
    session_id: String,
    slug: String,
    sid: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    /// Vendor catalog from `session/new|resume|load` `availableModels` —
    /// drives `/model`. Never a ccteam-hardcoded name list.
    available_models: Vec<AcpModelOption>,
    state: Arc<StdMutex<SessionTranslateState>>,
    event_tx: broadcast::Sender<ThreadEvent>,
    permission_mode: PermissionMode,
    _dispatcher: tokio::task::JoinHandle<()>,
}

fn acp_choice_prompt(title: &str, options: Vec<crate::ChoiceOption>) -> ChoicePrompt {
    ChoicePrompt {
        token: unique_prompt_token("km"),
        title: title.to_string(),
        options,
        multi: false,
    }
}

/// Per-process singleton holding live Kimi ACP sessions keyed by sessionId.
#[derive(Clone, Default)]
pub struct KimiAcpAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
}

impl std::fmt::Debug for KimiAcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KimiAcpAdapter").finish_non_exhaustive()
    }
}

impl KimiAcpAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn crate_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn inbound_policy(mode: PermissionMode) -> InboundPolicy {
        // Skip (default): auto-allow every `session/request_permission` —
        // skip sessions must run tools without prompting (same posture as
        // opencode; a client that never answers would stall the vendor).
        //
        // Hitl: **fail-closed decline**, same posture as opencode hitl.
        // Decline only blocks THAT tool call (the turn continues — never a
        // kill); the full IM [同意][拒绝] bridge remains future work.
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
            .map_err(|e| HarnessError::SpawnFailed(format!("kimi initialize failed: {e}")))?;

        // Harmless if the vendor does not require it; keeps parity with the
        // grok/opencode ACP clients.
        let _ = transport
            .notify("notifications/initialized", Value::Null)
            .await;
        Ok(())
    }

    async fn handshake_and_new(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        mcp_servers: Vec<Value>,
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
            .map_err(|e| HarnessError::SpawnFailed(format!("kimi session/new failed: {e}")))?;

        let session_id = pluck_session_id(&new_result).ok_or_else(|| {
            HarnessError::SpawnFailed("kimi session/new missing sessionId".into())
        })?;
        Ok((session_id, pluck_model_info(&new_result)))
    }

    /// Prefer `session/resume` (no history replay). Fall back to
    /// `session/load` only if resume fails; load replays history — translate
    /// drops replayed frames (isReplay), late updates are best-effort.
    async fn handshake_and_resume(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        session_id: &str,
        mcp_servers: Vec<Value>,
    ) -> Result<ModelInfo, HarnessError> {
        Self::handshake_initialize(transport).await?;
        // Resume/load MUST carry the SAME mcpServers as fresh — hardcoding
        // `[]` here would drop the ccteam tool face after any resume.
        let params = json!({
            "sessionId": session_id,
            "cwd": cwd.to_string_lossy(),
            "mcpServers": mcp_servers
        });
        match transport.call("session/resume", params.clone()).await {
            Ok(result) => Ok(pluck_model_info(&result)),
            Err(resume_err) => {
                tracing::warn!(
                    error = %resume_err,
                    session_id,
                    "kimi session/resume failed; falling back to session/load"
                );
                let load_result = transport.call("session/load", params).await.map_err(|e| {
                    HarnessError::SpawnFailed(format!(
                        "kimi session/load failed after resume error: {e}"
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
        crate::model_catalog::record_vendor_models_best_effort(
            "kimi",
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
            ..Default::default()
        }));
        // A reconnect (idle-release, capacity eviction, daemon restart) rejoins
        // a session whose context is already full; the handshake reports the
        // model catalog but never the occupancy. Seed the gaps from the
        // snapshot so the statusline resumes where it left off instead of
        // reading as a brand-new session.
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
            available_models: info.available,
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
            vendor: AgentVendor::Kimi,
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
                "adapter": KIMI_ACP_ADAPTER_NAME,
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
            context: st.context_usage(),
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
            HarnessError::ThreadDied(format!("kimi session {} not live", h.identity))
        })?;
        let text = match input {
            TurnInput::UserText(t) => t,
            other => {
                return Err(HarnessError::SubmitFailed(format!(
                    "kimi_acp: unsupported turn input {other:?}"
                )));
            }
        };

        // Kimi 0.29.1 has an internal `turn.steer`, but its ACP adapter exposes
        // only `session/prompt` -> `Session.prompt` (busy while active). Keep
        // the application intent as Inject and degrade explicitly to the shared
        // FIFO until Kimi exposes a native ACP interjection method.
        let route = {
            let mut state = live
                .state
                .lock()
                .map_err(|_| HarnessError::Io("kimi state lock poisoned".into()))?;
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
                    // kimi pushes no usage at all — pull it from the status
                    // command its own ACP layer advertises.
                    context_probe: Some(KIMI_STATUS_PROBE),
                    tuning: AcpTurnTuning {
                        finalize_barrier: FINALIZE_BARRIER,
                        post_finalize_sleep: None,
                        label: "kimi",
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
                        "kimi ACP has no native interject method; queued active-turn message"
                    );
                }
                Ok(TurnSubmission::queued(TurnId(turn_id)))
            }
            AcpTurnRoute::Inject { .. } => Err(HarnessError::Io(
                "kimi ACP routing selected unsupported native inject".into(),
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
        // Take the handshake backlog with the subscription: the vendor's
        // command catalog and any opening usage arrive before this task exists.
        let (early, mut sub) = transport.subscribe_with_early();
        for n in early {
            let events = if let Ok(mut guard) = state.lock() {
                apply_notification(&mut guard, &n)
            } else {
                Vec::new()
            };
            for ev in events {
                let _ = event_tx.send(ev);
            }
        }
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
impl HarnessAdapter for KimiAcpAdapter {
    fn name(&self) -> &'static str {
        KIMI_ACP_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Kimi
    }

    async fn start_thread(
        &self,
        _spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if let Some(mode) = ctx.mode.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            return Err(crate::execution::acp::spawn_pick_refused(
                "mode",
                mode,
                "Kimi has no session-mode axis (DSH agent presets only today)",
            ));
        }
        // Remote execution is claude-only (red line: 跨机 verdict 钉 claude);
        // see `codex_app_server.rs`'s identical guard for the rationale.
        if ctx.remote.is_some() {
            return Err(HarnessError::NotImplemented {
                reason: "remote execution (host != local) is not yet supported for kimi; \
                         use host=local"
                    .to_string(),
            });
        }
        // Roleless-only: kimi has no `--agent` persona face; it reads the
        // project AGENTS.md natively (no prompt injection).
        let bin = kimi_bin();
        let argv = build_argv(&bin, &KimiSpawnInput::default());
        // Child-only env: CCTEAM_CHAT_SID is the session's self-description —
        // without the explicit set the child inherits whatever stale value the
        // daemon's own environment chain carried, and an agent that reads
        // `env` mis-identifies itself.
        let envs = vec![(
            crate::execution::claude_common::CHAT_SID_ENV.to_string(),
            ctx.sid.clone(),
        )];
        let program = argv[0].clone();
        let args: Vec<String> = argv.into_iter().skip(1).collect();
        let cwd = if ctx.cwd.as_os_str().is_empty() {
            ctx.project_dir.clone()
        } else {
            ctx.cwd.clone()
        };
        let inbound = Self::inbound_policy(ctx.permission_mode);
        // The ccteam MCP tool face (HTTP + session bearer), passed
        // identically to session/new and session/resume|load. Empty when
        // sid/secret missing; failure to inject must not block the prompt
        // path (empty vec).
        //
        // Same open door as grok: this OFFERS the principal against a
        // same-named machine-credential entry in `$KIMI_CODE_HOME/mcp.json`,
        // and no flag makes it win (kimi's own `mergeCallerMcpServers` spreads
        // the caller last, so it is believed fine — unverified at runtime).
        // Identity does not ride on the offer: `spawn_for_session` records the
        // child pid and `/mcp` re-binds by process provenance; the daemon
        // still measures the outcome. See `mcp_config`'s module doc.
        let mcp_servers = crate::execution::mcp_config::acp_mcp_servers_http(&ctx.sid, &ctx.secret);

        // Cold-resume ladder: if meta.json already has a Kimi ACP sessionId
        // (vendor_uuid), `session/resume` (→ `session/load`) instead of
        // `session/new` so daemon rebuild / `/use` keep conversation context.
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
                let transport = AcpTransport::spawn_for_session(
                    &program,
                    &args,
                    &cwd,
                    &envs,
                    inbound,
                    &ctx.sid,
                    &ctx.project_dir,
                    KIMI_ACP_ADAPTER_NAME,
                )
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("spawn kimi acp: {e}")))?;
                let transport = Arc::new(transport);
                match Self::handshake_and_resume(&transport, &cwd, &uuid, mcp_servers.clone()).await
                {
                    Ok(info) => (transport, uuid, info),
                    Err(resume_err) => {
                        tracing::warn!(
                            error = %resume_err,
                            "kimi resume/load failed; falling back to session/new"
                        );
                        let _ = transport.shutdown().await;
                        let transport = AcpTransport::spawn_for_session(
                            &program,
                            &args,
                            &cwd,
                            &envs,
                            inbound,
                            &ctx.sid,
                            &ctx.project_dir,
                            KIMI_ACP_ADAPTER_NAME,
                        )
                        .await
                        .map_err(|e| {
                            HarnessError::SpawnFailed(format!("spawn kimi after resume fail: {e}"))
                        })?;
                        let transport = Arc::new(transport);
                        let (sid, info) =
                            Self::handshake_and_new(&transport, &cwd, mcp_servers.clone()).await?;
                        (transport, sid, info)
                    }
                }
            }
            None => {
                let transport = AcpTransport::spawn_for_session(
                    &program,
                    &args,
                    &cwd,
                    &envs,
                    inbound,
                    &ctx.sid,
                    &ctx.project_dir,
                    KIMI_ACP_ADAPTER_NAME,
                )
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("spawn kimi acp: {e}")))?;
                let transport = Arc::new(transport);
                let (sid, info) =
                    Self::handshake_and_new(&transport, &cwd, mcp_servers.clone()).await?;
                (transport, sid, info)
            }
        };

        // The effort axis id kimi just declared (`thinking`), kept before
        // `info` moves — never hardcoded here, see `ModelInfo`.
        let effort_config_id = info.effort_config_id.clone();
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
        // Spawn-time model + effort through the SAME vendor-native seams the
        // `/model` directive uses: `session/set_model` for the model (kimi's
        // pinned method; `kimi acp` takes no model argv) and
        // `session/set_config_option` for the effort axis it declared.
        //
        // A refusal FAILS the spawn (`spawn_pick_refused`): warning and
        // continuing handed the caller a session running on something other
        // than what they named, and reported success.
        if let Some(model) = ctx
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            live.transport
                .call(
                    "session/set_model",
                    json!({
                        "sessionId": live.session_id,
                        "modelId": model,
                    }),
                )
                .await
                .map_err(|e| crate::execution::acp::spawn_pick_refused("model", model, e))?;
            if let Ok(mut st) = live.state.lock() {
                st.model = Some(model.to_string());
            }
        }
        if let Some(effort) = ctx
            .effort
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            // The id kimi just declared wins; `thinking` is only the cold
            // fallback for a snapshot that omitted the axis (a model with no
            // thinking knob). Either way the vendor gets the call and owns the
            // verdict — same shape as the opencode sibling.
            let config_id = effort_config_id.as_deref().unwrap_or("thinking");
            live.transport
                .call(
                    "session/set_config_option",
                    json!({
                        "sessionId": live.session_id,
                        "configId": config_id,
                        "value": effort,
                    }),
                )
                .await
                .map_err(|e| crate::execution::acp::spawn_pick_refused("effort", effort, e))?;
            if let Ok(mut st) = live.state.lock() {
                st.effort = Some(effort.to_string());
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

    fn event_attachment(&self) -> crate::EventAttachment {
        // The ACP child owns a broadcast channel; `events()` looks the live
        // session up and subscribes to it, so a rebuild picks up the current
        // child and replays nothing.
        crate::EventAttachment::Rebuildable
    }

    async fn rebuild_tool_surface(
        &self,
        _h: &ThreadHandle,
    ) -> Result<crate::ToolSurfaceRebuild, HarnessError> {
        Ok(crate::ToolSurfaceRebuild::RespawnRequired {
            reason: "ACP carries `mcpServers` only on `session/new` / `session/load`; there is \
             no mid-session re-apply — `/new` rebuilds the tool face"
                .to_string(),
        })
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        if let Some(live) = self.get_live(persistent_id) {
            return Ok(Self::make_handle(&live));
        }
        // Cold resume needs the project cwd, which this bare-id entrypoint
        // lacks. The daemon rebuild path (`rebuild_session_from_meta`) instead
        // calls `start_thread` with the same sid, which reads meta.vendor_uuid
        // and runs the `session/resume|load` ladder — that is the working
        // cold-resume route for Kimi. Fail loudly here so nothing silently
        // relies on it.
        Err(HarnessError::NotImplemented {
            reason: format!(
                "kimi cold resume of {persistent_id} needs project cwd — rebuild via start_thread (rebuild_session_from_meta)"
            ),
        })
    }

    /// Daemon shutdown: let go of the local ACP child without stopping it
    /// (stdin EOF + no kill; the body record stays for the next daemon).
    async fn detach_thread(&self, h: &ThreadHandle) -> Result<DetachOutcome, HarnessError> {
        let live = {
            let mut map = self
                .live
                .lock()
                .map_err(|_| HarnessError::Io("live map poisoned".into()))?;
            map.remove(&h.identity)
        };
        let Some(live) = live else {
            return Ok(DetachOutcome::NotApplicable);
        };
        let in_flight = live
            .state
            .lock()
            .map(|state| state.buffer.is_some() || state.vendor_started_buffer.is_some())
            .unwrap_or(false);
        let pid = live.transport.detach().await;
        tracing::info!(
            sid = %live.sid,
            slug = %live.slug,
            ?pid,
            in_flight,
            "kimi-acp: body detached (left running; record kept for the next daemon)"
        );
        Ok(DetachOutcome::Detached { pid, in_flight })
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
                // No kimi-specific wording here any more: occupancy now comes
                // from the vendor's own `/status` command, and when it cannot
                // (a session that has not completed a turn yet), the shared
                // render point says "usage unknown" in the same words every
                // vendor uses.
                let receipt = status
                    .status_suffix()
                    .unwrap_or_else(|| "kimi · acp".into());
                Ok(DirectiveOutcome::Done { receipt })
            }
            "compact" => Ok(DirectiveOutcome::Rejected {
                reason: "kimi /compact: native command RPC not yet wired; restart session if context is full".into(),
            }),
            "model" => {
                let Some(live) = live else {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "kimi session not live".into(),
                    });
                };
                // Three forms (mirrors grok/opencode):
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
                                "用法: /model <model-id>（当前: {current}；vendor 未返回 availableModels）"
                            ),
                        });
                    }
                    return Ok(DirectiveOutcome::NeedsChoice(acp_choice_prompt(
                        "Choose a Kimi model:",
                        options,
                    )));
                }
                let efforts = known_efforts(&live.available_models);
                let (model_id, effort) = split_trailing_effort(&arg, &efforts);
                if model_id.is_empty() {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "用法: /model <model-id>".into(),
                    });
                }
                // Prefer vendor-listed id; still allow free-form (vendor rejects unknown).
                match live
                    .transport
                    .call(
                        "session/set_model",
                        json!({
                            "sessionId": live.session_id,
                            "modelId": model_id,
                        }),
                    )
                    .await
                {
                    Ok(_) => {
                        // Window from the catalog entry when present.
                        let window = live
                            .available_models
                            .iter()
                            .find(|m| m.model_id == model_id)
                            .and_then(|m| m.window);
                        if let Ok(mut st) = live.state.lock() {
                            st.model = Some(model_id.clone());
                            if let Some(w) = window {
                                st.window_tokens = Some(w);
                            }
                        }
                        let mut receipt = format!("已切换 model → {model_id}（live）");
                        if let Some(ref e) = effort {
                            // Kimi's effort ladder is the `thought_level` config
                            // option (`low|high|max`, plus `off` when the model
                            // allows it) — the same `set_config_option` call the
                            // opencode sibling makes under its own `effort` id.
                            match live
                                .transport
                                .call(
                                    "session/set_config_option",
                                    json!({
                                        "sessionId": live.session_id,
                                        "configId": "thinking",
                                        "value": e,
                                    }),
                                )
                                .await
                            {
                                Ok(_) => {
                                    if let Ok(mut st) = live.state.lock() {
                                        st.effort = Some(e.clone());
                                    }
                                    receipt.push_str(&format!("；effort → {e}"));
                                }
                                Err(err) => {
                                    receipt.push_str(&format!("；effort 切换失败: {err}"));
                                }
                            }
                        }
                        Ok(DirectiveOutcome::Done { receipt })
                    }
                    Err(e) => Ok(DirectiveOutcome::Rejected {
                        reason: format!("/model 切换失败: {e}"),
                    }),
                }
            }
            other => Ok(DirectiveOutcome::Rejected {
                reason: format!("kimi does not support /{other}"),
            }),
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        let Some(live) = self.get_live(&h.identity) else {
            // Released / restarted: answer from the persisted snapshot rather
            // than going silent.
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
