//! Antigravity CLI ACP adapter — eighth harness vendor (`AgentVendor::Agy`).
//!
//! Topology: **1 live session = 1 `agy acp` child** (stdio JSON-RPC 2.0).
//! Zero PTY / pane / hook path. Wire pin: agy 1.5.18, probed on a live binary
//! 2026-11-27 (`references/antigravity-cli` is the vendor's README/changelog —
//! protocol reference only, never vendored).
//!
//! What agy's ACP face looks like, and what this adapter does with it:
//! - **Handshake** is plain ACP: `initialize` → `session/new` (or
//!   `session/resume`, falling back to `session/load`). The result carries
//!   BOTH `models.availableModels` (composite `<model>-<level>` ids) and
//!   `configOptions` (a `model` select of base ids, a `thought_level` select
//!   scoped to the current model, a `mode` select). The shared reader prefers
//!   `configOptions`, so ccteam sees agy the way agy's own editors do: a model
//!   axis and an effort axis.
//! - **Model / effort** ride `session/set_config_option` (`model`, and the
//!   effort axis id agy declared — `thought_level`). The reply is the full
//!   post-change snapshot, which is applied as-is: agy accepts a composite id
//!   (`gemini-3.8-flash-low`) on the model axis and answers with the base
//!   model plus the level it implies, and switching to a model with no
//!   thinking axis drops the level — neither is something to guess locally.
//! - **Mode** (`default` / `accept-edits` / `auto` / `plan`) rides
//!   `session/set_mode`; agy validates the id (`Unknown mode: …`).
//! - **Permissions**: a skip session spawns `agy acp
//!   --dangerously-skip-permissions` (agy's own switch; no
//!   `session/request_permission` ever reaches the wire) with
//!   [`InboundPolicy::AutoAllowPermission`] as the second line. A hitl session
//!   passes no flag and declines: agy fails THAT tool call and the turn goes
//!   on (verified — the command did not run and the turn ended `end_turn`).
//!   The IM approve/deny bridge is the same future work as the ACP siblings.
//! - **Persona**: a non-empty role becomes `agy acp --agent <role>`, agy's
//!   native face. agy resolves the name against ITS agent definitions
//!   (`.agents/agents/` in the project, its own global config) and silently
//!   runs the default agent for a name it does not know — ccteam neither
//!   reads nor writes those files (no prompt injection red line).
//! - **Usage** is pushed: `usage_update{used,size}` after every model call and
//!   token counts on the `session/prompt` result. No pull probe needed. agy
//!   reports no USD, so cost reads "—".
//! - **Tool face**: `mcpServers[]` on `session/new|resume|load` (http +
//!   `{name,value}` header array — accepted and called with the session
//!   bearer on a live binary; tools surface as `mcp_ccteam_<tool>`).
//!
//! ## Never used: agy's system-prompt extension
//!
//! `initialize` advertises `_meta.antigravity.systemPrompt.append`. ccteam
//! does not send it and must not start to: no prompt injection is a red line.
//! agy reads the project's `AGENTS.md` / `GEMINI.md` itself.
//!
//! ## Mid-turn messages queue (agy's "steer" would end the turn early)
//!
//! agy does accept a second `session/prompt` while one is running, but it
//! answers the FIRST request at once with `end_turn` and no usage, then keeps
//! streaming under the second (measured 2026-11-27). The shared turn runner
//! finalizes a turn on its prompt response, so riding that would cut every
//! steered turn's reply off at the steer and report it complete. An Inject
//! request therefore degrades to the shared FIFO, exactly like kimi/opencode.
//!
//! ## No account-level usage surface
//!
//! The handshake names the subscription tier (`_meta.antigravity.tier`) and
//! nothing else: no window, no remaining quota, no reset time. `account_usage`
//! stays the trait default (`None`) rather than inventing numbers.

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
    acp_model_picker_options, pluck_model_info, pluck_session_id, released_thread_status,
    route_acp_turn, AcpModelOption, AcpTransport, AcpTurnRoute, AcpTurnRunner, AcpTurnTuning,
    InboundPolicy, ModelInfo, SessionTranslateState,
};
use crate::execution::claude_common::unique_prompt_token;
use crate::execution::session_meta::read_session_meta;
use crate::execution::session_status::read_status_file;
use crate::{
    AgentSpecBrief, AgentVendor, ChoicePrompt, DetachOutcome, Directive, DirectiveOutcome,
    ExecutionMode, HarnessAdapter, HarnessError, PermissionMode, SpawnCtx, ThreadEvent,
    ThreadHandle, ThreadStatus, TurnId, TurnInput, TurnRouting, TurnSubmission,
};

use spawn_spec::{agy_bin, build_argv, AgySpawnInput};

/// Max wait for the notification dispatcher to drain after the prompt
/// response before finalizing. agy sends no `turn_completed` frame; its
/// updates precede the response on the wire, so this covers only the
/// dispatcher-vs-response task race (same value as the ACP siblings).
const FINALIZE_BARRIER: std::time::Duration = std::time::Duration::from_millis(750);

/// How long `close_thread` waits for agy to acknowledge `session/close`.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Adapter name — stable id for handles / logs / tests.
pub const AGY_ACP_ADAPTER_NAME: &str = "agy-acp";

const EVENT_BUFFER: usize = 256;

struct LiveSession {
    transport: Arc<AcpTransport>,
    session_id: String,
    slug: String,
    sid: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    /// agy's own catalog (`configOptions`) — drives `/model`. Never a
    /// ccteam-hardcoded name list. Behind a lock because the effort levels are
    /// scoped to the CURRENT model: every `session/set_config_option` reply
    /// republishes them, and a picker built from the handshake snapshot would
    /// keep offering the first model's levels for whatever runs now.
    available_models: StdMutex<Vec<AcpModelOption>>,
    /// The effort axis id agy declared in the handshake (`thought_level`).
    effort_config_id: Option<String>,
    state: Arc<StdMutex<SessionTranslateState>>,
    event_tx: broadcast::Sender<ThreadEvent>,
    permission_mode: PermissionMode,
    _dispatcher: tokio::task::JoinHandle<()>,
}

fn acp_choice_prompt(title: &str, options: Vec<crate::ChoiceOption>) -> ChoicePrompt {
    ChoicePrompt {
        token: unique_prompt_token("ag"),
        title: title.to_string(),
        options,
        multi: false,
    }
}

/// Per-process singleton holding live `agy acp` sessions keyed by sessionId.
#[derive(Clone, Default)]
pub struct AgyAcpAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
}

impl std::fmt::Debug for AgyAcpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgyAcpAdapter").finish_non_exhaustive()
    }
}

impl AgyAcpAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    fn crate_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn inbound_policy(mode: PermissionMode) -> InboundPolicy {
        // Skip (default): the spawn flag already stops agy asking; auto-allow
        // is the second line for any ask that still surfaces (a client that
        // never answers stalls the vendor).
        //
        // Hitl: **fail-closed decline**. agy fails only THAT tool call and the
        // turn continues (never a kill); the IM approve/deny bridge is future
        // work shared with the other ACP vendors.
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
            .map_err(|e| HarnessError::SpawnFailed(format!("agy initialize failed: {e}")))?;

        // Harmless if the vendor does not require it; keeps parity with the
        // other ACP clients.
        let _ = transport
            .notify("notifications/initialized", Value::Null)
            .await;
        Ok(())
    }

    async fn handshake_and_new(
        transport: &AcpTransport,
        cwd: &std::path::Path,
        mcp_servers: Vec<Value>,
        requested_model: Option<&str>,
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
            .map_err(|e| HarnessError::SpawnFailed(format!("agy session/new failed: {e}")))?;

        let session_id = pluck_session_id(&new_result)
            .ok_or_else(|| HarnessError::SpawnFailed("agy session/new missing sessionId".into()))?;
        let info = pluck_model_info(&new_result);
        crate::execution::acp::record_fresh_default_model("agy", requested_model, &info);
        Ok((session_id, info))
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
                    "agy session/resume failed; falling back to session/load"
                );
                let load_result = transport.call("session/load", params).await.map_err(|e| {
                    HarnessError::SpawnFailed(format!(
                        "agy session/load failed after resume error: {e}"
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
        generation: Option<u64>,
    ) -> Arc<LiveSession> {
        crate::model_catalog::record_vendor_models_best_effort(
            "agy",
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
            // Stamp this THREAD so every observation the turn runner
            // persists says which one made it (`docs-local/issues/#14②`).
            generation,
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
        let dispatcher = crate::execution::acp::spawn_notification_dispatcher(
            Arc::clone(&transport),
            Arc::clone(&state),
            event_tx.clone(),
        );
        let live = Arc::new(LiveSession {
            transport,
            session_id: session_id.clone(),
            slug,
            sid,
            project_dir,
            cwd,
            available_models: StdMutex::new(info.available),
            effort_config_id: info.effort_config_id,
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
            vendor: AgentVendor::Agy,
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
                "adapter": AGY_ACP_ADAPTER_NAME,
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
            stop_hook_blocks: None,
            generation: st.generation,
        }
    }

    async fn submit_with_routing(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
        routing: TurnRouting,
    ) -> Result<TurnSubmission, HarnessError> {
        let live = self.get_live(&h.identity).ok_or_else(|| {
            HarnessError::ThreadDied(format!("agy session {} not live", h.identity))
        })?;
        let text = match input {
            TurnInput::UserText(t) => t,
            other => {
                return Err(HarnessError::SubmitFailed(format!(
                    "agy_acp: unsupported turn input {other:?}"
                )));
            }
        };

        // Queue-only: agy's second `session/prompt` ends the first request
        // early (module doc), so an Inject degrades explicitly to the shared
        // FIFO rather than truncating the running turn's reply.
        let route = {
            let mut state = live
                .state
                .lock()
                .map_err(|_| HarnessError::Io("agy state lock poisoned".into()))?;
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
                    // agy pushes `usage_update{used,size}`.
                    context_probe: None,
                    tuning: AcpTurnTuning {
                        finalize_barrier: FINALIZE_BARRIER,
                        post_finalize_sleep: None,
                        label: "agy",
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
                        "agy ACP steer would end the running turn; queued active-turn message"
                    );
                }
                Ok(TurnSubmission::queued_at(TurnId(turn_id), position))
            }
            AcpTurnRoute::Inject { .. } => Err(HarnessError::Io(
                "agy ACP routing selected unsupported native inject".into(),
            )),
        }
    }
}

/// `configOptions[].id` of agy's model axis (ACP category `model`).
const MODEL_CONFIG_ID: &str = "model";

/// agy's effort axis id — the cold fallback only; the id agy declared in the
/// handshake ([`ModelInfo::effort_config_id`]) is what normally travels.
const EFFORT_CONFIG_ID: &str = "thought_level";

/// One `session/set_config_option` call, with agy's reply applied to the
/// session state.
///
/// The reply is the whole post-change `configOptions` snapshot and is the only
/// honest source for what the session now runs: a composite id on the model
/// axis comes back as base model + implied level, and a model with no thinking
/// axis comes back with the level gone. Echoing the requested value into the
/// state instead would misreport both.
async fn set_config_option(live: &LiveSession, config_id: &str, value: &str) -> anyhow::Result<()> {
    let result = live
        .transport
        .call(
            "session/set_config_option",
            json!({
                "sessionId": live.session_id,
                "configId": config_id,
                "value": value,
            }),
        )
        .await?;
    let info = pluck_model_info(&result);
    if !info.available.is_empty() {
        if let Ok(mut models) = live.available_models.lock() {
            *models = info.available;
        }
    }
    if let Ok(mut st) = live.state.lock() {
        if let Some(model) = info.model {
            st.model = Some(model);
        }
        st.effort = info.effort;
    }
    Ok(())
}

/// Land the caller's explicit spawn-time picks on a freshly handshaken
/// session; the first refusal is returned as the spawn error.
async fn apply_spawn_picks(live: &LiveSession, ctx: &SpawnCtx) -> Result<(), HarnessError> {
    use crate::execution::acp::spawn_pick_refused;
    let pick = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    if let Some(model) = pick(&ctx.model_id) {
        set_config_option(live, MODEL_CONFIG_ID, &model)
            .await
            .map_err(|e| spawn_pick_refused("model", &model, e))?;
    }
    if let Some(effort) = pick(&ctx.effort) {
        // The id agy declared wins; the fallback only matters for a snapshot
        // that omitted the axis (a model with no thinking knob), where agy
        // still gets the call and owns the verdict.
        let config_id = live.effort_config_id.as_deref().unwrap_or(EFFORT_CONFIG_ID);
        set_config_option(live, config_id, &effort)
            .await
            .map_err(|e| spawn_pick_refused("effort", &effort, e))?;
    }
    if let Some(mode) = pick(&ctx.mode) {
        live.transport
            .call(
                "session/set_mode",
                json!({ "sessionId": live.session_id, "modeId": mode }),
            )
            .await
            .map_err(|e| spawn_pick_refused("mode", &mode, e))?;
    }
    Ok(())
}

/// Split a `/model` argument into `(model, effort?)`.
///
/// agy model ids never contain whitespace, so a second token is always the
/// effort. It is NOT checked against a local list: the levels on offer depend
/// on the model being switched TO, which only agy knows (`Thinking level
/// "xhigh" is not offered for model …` is its answer, relayed verbatim).
fn split_model_arg(arg: &str) -> (&str, Option<&str>) {
    let mut parts = arg.split_whitespace();
    let model = parts.next().unwrap_or_default();
    (model, parts.next())
}

#[async_trait]
impl HarnessAdapter for AgyAcpAdapter {
    fn name(&self) -> &'static str {
        AGY_ACP_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Agy
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        // Remote execution is claude-only (red line: 跨机 verdict 钉 claude);
        // see `codex_app_server.rs`'s identical guard for the rationale.
        if ctx.remote.is_some() {
            return Err(HarnessError::NotImplemented {
                reason: "remote execution (host != local) is not yet supported for agy; \
                         use host=local"
                    .to_string(),
            });
        }
        // Persona + permission posture are the only argv facts; agy reads the
        // project AGENTS.md / GEMINI.md natively (no prompt injection).
        let bin = agy_bin();
        let argv = build_argv(
            &bin,
            &AgySpawnInput {
                permission_mode: ctx.permission_mode,
                role: spec.role.trim(),
            },
        );
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
        // Same open door as the ACP siblings: this OFFERS the principal
        // against a same-named machine-credential entry in agy's own
        // `~/.gemini/config/mcp_config.json`, and nothing on the wire says
        // which one agy keeps (unverified at runtime). Identity does not ride
        // on the offer: `spawn_for_session` records the child pid and `/mcp`
        // re-binds by process provenance; the daemon still measures the
        // outcome. See `mcp_config`'s module doc.
        let mcp_servers = crate::execution::mcp_config::acp_mcp_servers_http(&ctx.sid, &ctx.secret);

        // Cold-resume ladder: if meta.json already has an agy ACP sessionId
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
                    AGY_ACP_ADAPTER_NAME,
                )
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("spawn agy acp: {e}")))?;
                let transport = Arc::new(transport);
                match Self::handshake_and_resume(&transport, &cwd, &uuid, mcp_servers.clone()).await
                {
                    Ok(info) => (transport, uuid, info),
                    Err(resume_err) => {
                        tracing::warn!(
                            error = %resume_err,
                            "agy resume/load failed; falling back to session/new"
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
                            AGY_ACP_ADAPTER_NAME,
                        )
                        .await
                        .map_err(|e| {
                            HarnessError::SpawnFailed(format!("spawn agy after resume fail: {e}"))
                        })?;
                        let transport = Arc::new(transport);
                        let (sid, info) = Self::handshake_and_new(
                            &transport,
                            &cwd,
                            mcp_servers.clone(),
                            ctx.model_id.as_deref(),
                        )
                        .await?;
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
                    AGY_ACP_ADAPTER_NAME,
                )
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("spawn agy acp: {e}")))?;
                let transport = Arc::new(transport);
                let (sid, info) = Self::handshake_and_new(
                    &transport,
                    &cwd,
                    mcp_servers.clone(),
                    ctx.model_id.as_deref(),
                )
                .await?;
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
            ctx.generation_stamp(),
        );
        // Spawn-time picks through the SAME vendor-native seams the `/model`
        // directive uses (`agy acp` takes no model argv). Model first: the
        // effort levels on offer belong to the model just selected.
        //
        // A refusal FAILS the spawn (`spawn_pick_refused`): a caller who named
        // a model, an effort or a mode gets that session or an error — never
        // one quietly running on something else. And a failed spawn leaves
        // nothing behind: the session is already live and registered by now,
        // so the refusal path closes it rather than orphaning an `agy`
        // process nobody holds a handle to.
        if let Err(refused) = apply_spawn_picks(&live, ctx).await {
            let _ = self.close_thread(&Self::make_handle(&live)).await;
            return Err(refused);
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
        // cold-resume route for Agy. Fail loudly here so nothing silently
        // relies on it.
        Err(HarnessError::NotImplemented {
            reason: format!(
                "agy cold resume of {persistent_id} needs project cwd — rebuild via start_thread (rebuild_session_from_meta)"
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
            "agy-acp: body detached (left running; record kept for the next daemon)"
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
            // `session/close` is agy's own teardown: it stops the session's
            // subagents and background tasks (dev servers it started), which
            // killing the ACP process alone leaves to chance. It ends only
            // this process's hold — the conversation still resumes by id in
            // the next one (verified live). Bounded: a wedged agy must not
            // hold up a release.
            let _ = tokio::time::timeout(
                CLOSE_GRACE,
                live.transport
                    .call("session/close", json!({ "sessionId": live.session_id })),
            )
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
                // No agy-specific wording here any more: occupancy now comes
                // from the vendor's own `/status` command, and when it cannot
                // (a session that has not completed a turn yet), the shared
                // render point says "usage unknown" in the same words every
                // vendor uses.
                let receipt = status.status_suffix().unwrap_or_else(|| "agy · acp".into());
                Ok(DirectiveOutcome::Done { receipt })
            }
            "compact" => {
                // agy publishes `compact` in its command catalog and runs a
                // leading slash command from the prompt text, so the directive
                // is a pass-through prompt (red line: `/compact` 完全透传).
                if live.is_none() {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "agy session not live".into(),
                    });
                }
                self.submit_turn(h, TurnInput::UserText("/compact".into()))
                    .await?;
                Ok(DirectiveOutcome::Done {
                    receipt: "agy /compact submitted as prompt".into(),
                })
            }
            "model" => {
                let Some(live) = live else {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "agy session not live".into(),
                    });
                };
                // Three forms (mirrors the ACP siblings):
                // (1) picker re-entry — `d.choice` carries the picked option id
                //     (`"<modelId>"` or `"<modelId> <effort>"`);
                // (2) explicit `/model <id> [effort]`;
                // (3) bare `/model` → NeedsChoice from agy's own catalog.
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
                    let options = live
                        .available_models
                        .lock()
                        .map(|models| acp_model_picker_options(&models))
                        .unwrap_or_default();
                    if options.is_empty() {
                        let current = live
                            .state
                            .lock()
                            .ok()
                            .and_then(|s| s.model.clone())
                            .unwrap_or_else(|| "(unknown)".into());
                        return Ok(DirectiveOutcome::Rejected {
                            reason: format!(
                                "用法: /model <model-id> [effort]（当前: {current}；vendor 未返回 model options）"
                            ),
                        });
                    }
                    return Ok(DirectiveOutcome::NeedsChoice(acp_choice_prompt(
                        "Choose an Antigravity model:",
                        options,
                    )));
                }
                let (model_id, effort) = split_model_arg(&arg);
                if model_id.is_empty() {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: "用法: /model <model-id> [effort]".into(),
                    });
                }
                // Free-form ids are allowed: agy validates (`Unknown model ID`).
                if let Err(e) = set_config_option(&live, MODEL_CONFIG_ID, model_id).await {
                    return Ok(DirectiveOutcome::Rejected {
                        reason: format!("/model 切换失败: {e}"),
                    });
                }
                let mut receipt = format!("已切换 model → {model_id}（live）");
                if let Some(e) = effort {
                    let config_id = live.effort_config_id.as_deref().unwrap_or(EFFORT_CONFIG_ID);
                    match set_config_option(&live, config_id, e).await {
                        Ok(()) => receipt.push_str(&format!("；effort → {e}")),
                        Err(err) => receipt.push_str(&format!("；effort 切换失败: {err}")),
                    }
                }
                Ok(DirectiveOutcome::Done { receipt })
            }
            other => Ok(DirectiveOutcome::Rejected {
                reason: format!("agy does not support /{other}"),
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

    /// What this session's in-flight turn has said so far (GitHub #197 E/G).
    /// One shared ACP implementation — see [`crate::execution::acp::in_flight_narration`].
    fn in_flight_narration(&self, h: &ThreadHandle) -> Option<crate::PartialNarration> {
        let live = self.get_live(&h.identity)?;
        crate::execution::acp::in_flight_narration(&live.state)
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
