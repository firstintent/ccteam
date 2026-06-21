//! v0.8.11 E1 — `ClaudeStreamJsonAdapter`: the Claude vendor's **second**
//! spawn path, a long-running `claude` child driven over a bidirectional
//! NDJSON (stream-json) pipe instead of a tmux PTY. It implements the same
//! [`HarnessAdapter`] trait and emits the same [`ThreadEvent`]
//! (CanonicalEvent) stream as [`super::claude_tui::ClaudeTuiAdapter`], so
//! the gateway's `spawn_event_pump` — the live daemon's only turns/progress
//! writer — consumes it **unchanged** (PRD §〇 decision 1; §七 ④ SoT writer
//! reuse).
//!
//! ## The four seams (PRD §七 ①)
//!
//! - [`spawn_spec`] — pure argv/env/cwd builder (host-portable).
//! - [`transport`] — bidirectional NDJSON over a generic `(reader, writer)`;
//!   the consumer never holds the [`tokio::process::Child`] (WS-replaceable).
//! - [`translate`] — NDJSON → [`ThreadEvent`].
//! - this module — the adapter + its live-session registry + SoT-writer
//!   reuse (the gateway pump).
//!
//! ## Red lines
//!
//! - **Zero injection**: persona only via `--agent`; [`spawn_spec`] never
//!   emits `--append-system-prompt` and this adapter never sends an
//!   `initialize.systemPrompt`.
//! - **Never kill a long session**: idle release / wake = close stdin +
//!   `--resume` (≡ resume-by-session-id); `close_thread` is the only kill
//!   path and is user-initiated. The deterministic per-(slug,sid) uuid is
//!   what makes `--resume` stateless across daemon restart.
//! - **No terminal scraping**: there is no terminal — naturally satisfied.

pub mod bridge;
pub mod protocol;
pub mod spawn_spec;
pub mod translate;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};

use crate::execution::progress_bridge::{
    append_event, build_chat_session_reset_event_with_reason, progress_jsonl_from_env,
};
use crate::execution::transcript_tail::anthropic_project_dir;
use crate::{
    AgentSpecBrief, AgentVendor, Directive, DirectiveOutcome, ExecutionMode, HarnessAdapter,
    HarnessError, SpawnCtx, ThreadEvent, ThreadHandle, ThreadStatus, TurnId, TurnInput,
};

use bridge::{ApprovalDecision, CanUseToolResolver, SlashClass};
use protocol::Outbound;
use spawn_spec::StreamJsonSpawnInput;
use translate::StreamTranslator;
use transport::StreamJsonTransport;

/// §七 ⑤ — host-facet-friendly session identity. `sid → vendor_uuid` is a
/// stable mapping (the uuid is derived deterministically from `(slug,
/// sid)`); `host` reserves the v0.9 host axis (`local` today; a `Sandbox
/// CR` ref later) without a one-shot re-key.
#[derive(Debug, Clone)]
pub struct SessionIdentity {
    pub sid: String,
    pub vendor_uuid: String,
    pub host: String,
}

/// One live stream-json session: the transport (owns the child privately)
/// plus the identity / routing context the adapter needs across calls.
struct LiveSession {
    identity: SessionIdentity,
    transport: Arc<StreamJsonTransport>,
    slug: String,
    role: String,
    project_dir: PathBuf,
    cwd: PathBuf,
    /// Slash-command table from `system:init` (bridge gate, Wave 2).
    commands: Vec<String>,
    /// Live session status (model + context-window usage) for
    /// [`HarnessAdapter::thread_status`] → IM `/sessions` + the web statusline
    /// bar. Seeded with the `initialize` model; the per-session **status tap**
    /// ([`spawn_status_tap`]) overwrites it from each `assistant`/`result`
    /// message's `usage` as turns run (interior-mutable, shared with the tap).
    status: Arc<StdMutex<ThreadStatus>>,
}

/// The Claude stream-json adapter. A per-vendor singleton (mirrors
/// `CodexAppServerAdapter`) holding every live session keyed by its vendor
/// uuid. `ThreadHandle` (serializable, restart-surviving) carries only the
/// uuid + routing extras — never the live child — so a daemon restart
/// rebuilds via `--resume`.
#[derive(Clone, Default)]
pub struct ClaudeStreamJsonAdapter {
    live: Arc<StdMutex<HashMap<String, Arc<LiveSession>>>>,
    /// HITL resolver for `can_use_tool` reverse RPCs. `None` = no HITL
    /// wiring (a hitl session then default-denies, the safe direction).
    /// The gateway wires the production resolver (→ `permission/ask` → IM)
    /// in Wave 3; tests inject a deterministic stub.
    resolver: Option<Arc<dyn CanUseToolResolver>>,
}

impl std::fmt::Debug for ClaudeStreamJsonAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeStreamJsonAdapter")
            .finish_non_exhaustive()
    }
}

/// Adapter `name()` — the stable id used in handles, logs, and tests.
pub const STREAM_JSON_ADAPTER_NAME: &str = "claude-stream-json";

/// Spawn the per-session HITL dispatcher: watch the transport for
/// `can_use_tool` reverse RPCs, resolve each via the wired resolver, and
/// reply with a `control_response`. A missing resolver default-denies (the
/// safe direction). `deny` blocks ONLY the tool call — the turn continues.
fn spawn_hitl_dispatcher(
    transport: Arc<StreamJsonTransport>,
    sid: String,
    resolver: Option<Arc<dyn CanUseToolResolver>>,
) {
    let mut sub = transport.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(Outbound::ControlRequest(creq)) => {
                        let Some(req) = bridge::parse_can_use_tool(&creq) else { continue };
                        let decision = match &resolver {
                            Some(r) => r.resolve(&sid, &req).await,
                            None => ApprovalDecision::deny(
                                "HITL approval is unavailable (no resolver wired) — denied",
                            ),
                        };
                        let line = protocol::can_use_tool_response_line(
                            &req.request_id,
                            decision.allow,
                            &req.input,
                            &decision.message,
                        );
                        if transport.send_line(line).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
}

/// Carry the `[1m]` context-window tag from the CURRENT model onto the API's
/// `message.model` (which omits it). The user requests 1M via `set_model
/// opus[1m]`, but claude's API model id is the bare `claude-opus-4-8`; without
/// this the status tap would re-stamp the session bare → the window heuristic
/// (`context_window_for_model`) reads 200k and the `[1m]` display is lost. A
/// later `set_model` resets the current model (with or without `[1m]`), so this
/// only preserves the live intent — it never invents a tag the user didn't ask
/// for, and a switch to a non-`[1m]` model clears it.
fn preserve_1m_tag(current: Option<&str>, api_model: &str) -> String {
    let had_1m = current
        .map(|c| c.to_ascii_lowercase().ends_with("[1m]"))
        .unwrap_or(false);
    if had_1m && !api_model.to_ascii_lowercase().ends_with("[1m]") {
        format!("{api_model}[1m]")
    } else {
        api_model.to_string()
    }
}

/// Spawn the per-session status tap: fold each `assistant`/`result`
/// message's token `usage` (and the assistant's live `message.model`) into
/// the shared [`ThreadStatus`], so [`HarnessAdapter::thread_status`] reports
/// the current model + context-window usage without parsing a transcript.
/// Runs for the session's whole life (ends when the transport closes). An
/// `assistant` message carries BOTH `model` and `usage`; the per-turn
/// `result` updates context against the last-known model. Reuses the single
/// compute point
/// [`context_usage_from_usage`](crate::execution::transcript_tail::context_usage_from_usage)
/// so the number matches the TUI transcript path byte-for-byte.
fn spawn_status_tap(
    transport: Arc<StreamJsonTransport>,
    status: Arc<StdMutex<ThreadStatus>>,
    project_dir: PathBuf,
    sid: String,
) {
    use crate::execution::transcript_tail::context_usage_from_usage;
    let mut sub = transport.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = transport.wait_closed() => return,
                msg = sub.recv() => match msg {
                    Ok(Outbound::Assistant(env)) => {
                        if let Some(usage) = env.message.get("usage") {
                            let api_model = env.message.get("model").and_then(|v| v.as_str());
                            let snapshot = if let Ok(mut s) = status.lock() {
                                // Re-stamp the model from the API id, but carry over
                                // a user-set `[1m]` tag (the API omits it) so the
                                // window heuristic below + the display keep 1M.
                                if let Some(m) = api_model.filter(|m| !m.is_empty()) {
                                    s.model = Some(preserve_1m_tag(s.model.as_deref(), m));
                                }
                                // Window from the `[1m]`-preserved model (the
                                // result branch's get_context_usage is authoritative
                                // when it answers; this keeps the per-message update
                                // from flickering an `opus[1m]` session back to 200k).
                                s.context =
                                    Some(context_usage_from_usage(usage, s.model.as_deref()));
                                Some(s.clone())
                            } else {
                                None
                            };
                            if let Some(snap) = snapshot {
                                write_status_file(&project_dir, &sid, &snap);
                            }
                        }
                    }
                    Ok(Outbound::TurnResult(r)) => {
                        // Prefer claude's OWN context accounting
                        // (`get_context_usage` → real totalTokens + maxTokens, no
                        // hardcoded [1m]/200k window). Fall back to the
                        // result.usage token-sum + model heuristic only if the
                        // vendor build doesn't answer.
                        let real = get_context_usage(&transport).await;
                        // The vendor's runtime-resolved reasoning effort (Opus
                        // 4.6+ / Codex), e.g. `xhigh`. None on an older CLI
                        // without `get_settings` or a model with no effort axis.
                        let effort = get_applied_effort(&transport).await;
                        let snapshot = if let Ok(mut s) = status.lock() {
                            if let Some(e) = effort {
                                s.effort = Some(e);
                            }
                            if let Some((used, window)) = real {
                                s.context = Some(crate::ContextUsage {
                                    used_tokens: used,
                                    window_tokens: window,
                                });
                            } else if let Some(usage) = &r.usage {
                                let model = s.model.clone();
                                s.context =
                                    Some(context_usage_from_usage(usage, model.as_deref()));
                            }
                            // Show the FULL model id (…[1m]) when the real window
                            // is 1M — both statusline surfaces tag the 1M id the
                            // same way (rmux derives the window FROM the [1m];
                            // stream-json derives the [1m] FROM the real window).
                            let is_1m =
                                matches!(&s.context, Some(c) if c.window_tokens >= 1_000_000);
                            if is_1m {
                                if let Some(m) = s.model.as_mut() {
                                    if !m.to_ascii_lowercase().ends_with("[1m]") {
                                        m.push_str("[1m]");
                                    }
                                }
                            }
                            Some(s.clone())
                        } else {
                            None
                        };
                        if let Some(snap) = snapshot {
                            write_status_file(&project_dir, &sid, &snap);
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
}

/// Query claude's REAL context accounting via the `get_context_usage`
/// control_request → `(totalTokens, maxTokens)`. This is the vendor's actual
/// window for the session (e.g. a default Opus 4.8 session reports
/// `maxTokens: 200000` even though the model advertises a 1M capability), so
/// it replaces the brittle `[1m]`-suffix → 1M/200k heuristic for the live
/// context bar. `None` on timeout / error / an older CLI without the subtype
/// (the caller then falls back to the usage-sum + heuristic). Short timeout —
/// it must never stall the status tap.
async fn get_context_usage(transport: &StreamJsonTransport) -> Option<(u64, u64)> {
    let body = transport
        .request_control("get_context_usage", json!({}), Duration::from_secs(3))
        .await
        .ok()?;
    if body.subtype != "success" {
        return None;
    }
    let resp = body.response.as_ref()?;
    let used = resp.get("totalTokens").and_then(|v| v.as_u64())?;
    let window = resp
        .get("maxTokens")
        .and_then(|v| v.as_u64())
        .or_else(|| resp.get("rawMaxTokens").and_then(|v| v.as_u64()))?;
    Some((used, window))
}

/// The reasoning-effort levels claude accepts (Opus 4.6+), low→high. Mirrors
/// the vendor `EFFORT_LEVELS` (`/effort`); used to validate a `/model <id>
/// <effort>` / `/effort <level>` argument before it touches settings.
pub(crate) const EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Normalize a user-typed effort token to a canonical level, or `None` if it
/// isn't one (so `/model opus xhigh` splits cleanly but `/model opus-4-8`
/// never mis-reads a model fragment as effort).
pub(crate) fn normalize_effort(arg: &str) -> Option<String> {
    let a = arg.trim().to_ascii_lowercase();
    EFFORT_LEVELS
        .iter()
        .find(|l| **l == a)
        .map(|l| (*l).to_string())
}

/// Split a `/model` argument into `(model, effort?)`: the effort is the
/// trailing whitespace-separated token IFF it's a valid level, so
/// `opus[1m] xhigh` → `("opus[1m]", Some("xhigh"))` while a bare model id
/// (`opus-4-8`, `claude-3-5-haiku`) is never mis-split.
pub(crate) fn split_model_effort(arg: &str) -> (String, Option<String>) {
    let arg = arg.trim();
    if let Some((head, tail)) = arg.rsplit_once(char::is_whitespace) {
        if let Some(eff) = normalize_effort(tail) {
            return (head.trim().to_string(), Some(eff));
        }
    }
    (arg.to_string(), None)
}

/// Read the vendor's REAL runtime-resolved reasoning effort via the
/// `get_settings` control_request → `response.applied.effort` (the level that
/// "will actually be sent to the API", after env / session / model defaults).
/// `None` on timeout / error / an older CLI without the subtype, or a model
/// with no effort axis. Short timeout — must never stall the status tap.
async fn get_applied_effort(transport: &StreamJsonTransport) -> Option<String> {
    let body = transport
        .request_control("get_settings", json!({}), Duration::from_secs(3))
        .await
        .ok()?;
    if body.subtype != "success" {
        return None;
    }
    body.response
        .as_ref()?
        .get("applied")?
        .get("effort")?
        .as_str()
        .map(str::to_string)
}

/// Persist a reasoning-effort level into the project's ccteam-managed
/// `.claude/settings.local.json` as `effortLevel` (the vendor key claude reads
/// at startup; there is NO live `set_effort` control — `set_model` is
/// model-only). Idempotent + non-clobbering: every sibling key is preserved.
/// Like the plugin-enable path, it lands in the gitignored `local` layer and
/// NEVER touches the user's `settings.json`; it takes effect on the session's
/// next start (`/new`). `cwd` is the session's project root.
fn set_effort_level(cwd: &Path, level: &str) -> std::io::Result<()> {
    let path = cwd.join(".claude").join("settings.local.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| json!({}));
    root.as_object_mut()
        .expect("filtered to object")
        .insert("effortLevel".to_string(), json!(level));
    let body = serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".to_string());
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// Live-apply a settings delta to a running stream-json session via the
/// `apply_flag_settings` control_request — vendor doc: *"Merges the provided
/// settings into the flag settings layer, updating the active configuration."*
/// i.e. an IMMEDIATE, no-restart settings change (the same control iOS/remote
/// clients use for runtime config — there is no per-setting control like a
/// `set_effort`; this generic merge is the mechanism). Verified empirically
/// against the live vendor (`applied.effort` flips high→low with no restart).
///
/// This is the generic hook every live-config command rides on (`/effort`
/// today, `/set <key> <value>` for anything else, more dedicated commands
/// later). `settings` is the JSON object to merge (e.g. `{"effortLevel":"max"}`).
/// Returns Ok on a `success` control_response, else Err(vendor reason).
async fn apply_flag_settings_live(
    live: &LiveSession,
    settings: serde_json::Value,
) -> Result<(), String> {
    let body = live
        .transport
        .request_control(
            "apply_flag_settings",
            json!({ "settings": settings }),
            init_timeout(),
        )
        .await
        .map_err(|e| format!("apply_flag_settings 失败: {e}"))?;
    if body.subtype != "success" {
        return Err(body
            .error
            .unwrap_or_else(|| "vendor rejected apply_flag_settings".into()));
    }
    Ok(())
}

/// Settings keys a chat `/set` command must NEVER live-mutate: the
/// safety / HITL / execution boundary. (`apply_flag_settings` is powerful — it
/// can merge ANY settings key into the active config — so the user-facing
/// escape hatch is fenced to keep a chat command from silently weakening
/// permissions, hooks, or the MCP surface.)
const SET_PROTECTED_KEYS: &[&str] = &["permissions", "hooks", "mcpServers"];

/// Persisted per-session status snapshot path, next to the turns mirror:
/// `<project_dir>/.ccteam/chat/<sid>/status.json`. ccteam-owned (no
/// Anthropic-internal dependency). Unlike the TUI adapter — which re-derives
/// status from the on-disk transcript every call — a stream-json session's
/// status lives only in the in-memory `LiveSession`, so it would vanish on
/// idle-release / daemon restart (spawn-on-demand resume). Persisting it here
/// lets [`HarnessAdapter::thread_status`] answer for a released/resumed
/// session, giving the statusline the same durability the TUI gets for free.
fn status_json_path(project_dir: &Path, sid: &str) -> PathBuf {
    project_dir
        .join(".ccteam")
        .join("chat")
        .join(sid)
        .join("status.json")
}

/// Persist the latest status atomically (tmp + rename). Best-effort: a write
/// failure only means a released session can't show its statusline until its
/// next turn — never worth failing anything over.
fn write_status_file(project_dir: &Path, sid: &str, status: &ThreadStatus) {
    let path = status_json_path(project_dir, sid);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(body) = serde_json::to_string(status) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Read the persisted status snapshot, or `None` if absent / unreadable.
fn read_status_file(project_dir: &Path, sid: &str) -> Option<ThreadStatus> {
    let body = std::fs::read_to_string(status_json_path(project_dir, sid)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Translate one outbound message and forward its events to the stream's
/// channel. `Err(())` means the consumer dropped the stream (stop).
async fn forward(
    translator: &mut StreamTranslator,
    tx: &mpsc::Sender<ThreadEvent>,
    out: Outbound,
) -> Result<(), ()> {
    if matches!(out, Outbound::Other) {
        return Ok(());
    }
    for ev in translator.ingest(out) {
        tx.send(ev).await.map_err(|_| ())?;
    }
    Ok(())
}

/// How long to wait for `system:init` before declaring the spawn failed.
/// claude startup (incl. auth) can be slow; tests shorten it via env.
fn init_timeout() -> Duration {
    std::env::var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(30))
}

impl ClaudeStreamJsonAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the HITL `can_use_tool` resolver (gateway wiring, Wave 3).
    pub fn with_resolver(mut self, resolver: Arc<dyn CanUseToolResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    fn lookup(&self, identity: &str) -> Option<Arc<LiveSession>> {
        self.live.lock().unwrap().get(identity).cloned()
    }

    /// The slash-command table claude advertised at `system:init` for a
    /// live session (by vendor uuid / handle identity). The Wave 2 bridge
    /// gate keys known-vs-unknown commands off this; exposed now so the
    /// captured table has a reader and so tests can assert it.
    pub fn session_command_table(&self, identity: &str) -> Option<Vec<String>> {
        self.lookup(identity).map(|live| live.commands.clone())
    }

    /// The host-facet identity (`sid` / `vendor_uuid` / `host`) for a live
    /// session — the §七 ⑤ mapping record, surfaced for the gateway + tests.
    pub fn session_identity(&self, identity: &str) -> Option<SessionIdentity> {
        self.lookup(identity).map(|live| live.identity.clone())
    }

    /// True when claude has already filed a transcript jsonl for this uuid
    /// under the project's Anthropic dir — the signal to `--resume` rather
    /// than mint a fresh `--session-id`.
    fn session_jsonl_exists(cwd: &Path, uuid: &str) -> bool {
        anthropic_project_dir(cwd)
            .map(|d| d.join(format!("{uuid}.jsonl")).exists())
            .unwrap_or(false)
    }

    /// Spawn the child + perform the `initialize` handshake, shutting the
    /// transport down on any failure so a dead child never lingers.
    ///
    /// claude (stream-json) does **not** emit a `system:init` line until the
    /// first user turn, so waiting for `system:init` at spawn would hang
    /// forever (the daemon waits for init while claude waits for input). The
    /// capability handshake is the `initialize` control_request →
    /// `control_response` (what the VS Code extension / SDK do); we parse the
    /// slash-command table + model out of its response. `system:init` is still
    /// captured opportunistically by the reader when it arrives with the first
    /// turn (the bridge gate's command table is seeded from the handshake).
    async fn spawn_and_init(
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<(Arc<StreamJsonTransport>, protocol::SystemMsg), HarnessError> {
        let transport = StreamJsonTransport::connect_stdio(argv, env, cwd)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("stream-json connect: {e:#}")))?;
        match transport
            .request_control("initialize", json!({}), init_timeout())
            .await
        {
            Ok(body) if body.subtype == "success" => Ok((
                Arc::new(transport),
                protocol::SystemMsg::from_initialize(&body),
            )),
            Ok(body) => {
                transport.shutdown().await;
                Err(HarnessError::SpawnFailed(format!(
                    "stream-json initialize rejected: {}",
                    body.error.unwrap_or_else(|| body.subtype.clone())
                )))
            }
            Err(e) => {
                transport.shutdown().await;
                Err(HarnessError::SpawnFailed(format!(
                    "stream-json init handshake: {e:#}"
                )))
            }
        }
    }
}

#[async_trait]
impl HarnessAdapter for ClaudeStreamJsonAdapter {
    fn name(&self) -> &'static str {
        STREAM_JSON_ADAPTER_NAME
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        // v0.8.11 E2 — pin-point isolate the official Telegram plugin (its
        // bot-token getUpdates poll structurally collides with ccteam's IM
        // gateway). Same managed layer the tmux path uses; only this one plugin.
        crate::execution::claude_tui::ensure_telegram_plugin_disabled(&ctx.project_dir)?;
        let bin = spawn_spec::claude_bin();
        // §七 ⑤ — stable per-(slug,sid) uuid: the stateless resume key.
        let uuid = spawn_spec::deterministic_session_uuid(&ctx.slug, &ctx.sid);
        let resume = Self::session_jsonl_exists(&ctx.cwd, &uuid);

        let make_argv = |resume: bool| {
            spawn_spec::build_argv(
                &bin,
                &StreamJsonSpawnInput {
                    role: &spec.role,
                    session_uuid: &uuid,
                    resume,
                    model_id: ctx.model_id.as_deref(),
                    permission_mode: ctx.permission_mode,
                },
            )
        };
        let env = spawn_spec::build_env(&spec.role, &ctx.slug, &ctx.secret, &ctx.sid);

        // Try the resume spawn first when a prior transcript exists; on
        // failure fall back to a fresh `--session-id` spawn and emit a
        // chat_session_reset with an explicit reason (the honest
        // context-loss signal — never silently synthesize).
        let (transport, init) = match Self::spawn_and_init(&make_argv(resume), &env, &ctx.cwd).await
        {
            Ok(ok) => ok,
            Err(resume_err) if resume => {
                tracing::warn!(
                    sid = %ctx.sid,
                    slug = %ctx.slug,
                    error = %resume_err,
                    "claude-stream-json: --resume spawn failed; falling back to fresh --session-id"
                );
                let fresh = Self::spawn_and_init(&make_argv(false), &env, &ctx.cwd).await?;
                if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
                    let ev = build_chat_session_reset_event_with_reason(
                        &spec.role,
                        &ctx.sid,
                        "resume_failed_fallback_to_fresh",
                    );
                    if let Err(err) = append_event(&progress_path, &ev) {
                        tracing::warn!(error = %err, "claude-stream-json: append reset event failed");
                    }
                }
                fresh
            }
            Err(e) => return Err(e),
        };

        let identity = SessionIdentity {
            sid: ctx.sid.clone(),
            vendor_uuid: uuid.clone(),
            host: "local".to_string(),
        };
        // Seed the live status with the `initialize` model (context unknown
        // until the first turn's `usage` lands). The status tap below keeps it
        // current; thread_status reads it.
        let status = Arc::new(StdMutex::new(ThreadStatus {
            model: init.model.clone(),
            context: None,
            // Effort is unknown until the first turn — the status tap reads the
            // vendor's runtime-resolved level via `get_settings`.
            effort: None,
        }));
        // Status tap (every session, not just hitl): watch the transport for
        // `assistant`/`result` messages and fold each one's `usage` (+ live
        // `message.model`) into `status`, so /sessions + the web statusline
        // show model + context% as the session burns context.
        spawn_status_tap(
            Arc::clone(&transport),
            Arc::clone(&status),
            ctx.project_dir.clone(),
            ctx.sid.clone(),
        );
        // HITL: only a hitl session (`--permission-prompt-tool stdio`) ever
        // receives `can_use_tool` reverse RPCs. Spawn the dispatcher that
        // resolves each via the wired resolver (→ IM approve/deny) and
        // replies with a control_response. A skip session never gets one,
        // so no dispatcher is needed.
        if ctx.permission_mode.is_hitl() {
            spawn_hitl_dispatcher(
                Arc::clone(&transport),
                ctx.sid.clone(),
                self.resolver.clone(),
            );
        }
        let live = LiveSession {
            identity: identity.clone(),
            transport,
            slug: ctx.slug.clone(),
            role: spec.role.clone(),
            project_dir: ctx.project_dir.clone(),
            cwd: ctx.cwd.clone(),
            commands: init.slash_commands.clone(),
            status,
        };
        self.live
            .lock()
            .unwrap()
            .insert(uuid.clone(), Arc::new(live));

        tracing::info!(
            event = "stream_json_started",
            sid = %ctx.sid,
            slug = %ctx.slug,
            role = %spec.role,
            vendor_uuid = %uuid,
            resumed = resume,
            "claude-stream-json: session live"
        );

        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: uuid.clone(),
            started_at: Utc::now(),
            raw_extras: json!({
                "adapter": STREAM_JSON_ADAPTER_NAME,
                "protocol": "stream-json",
                "host": identity.host,
                "vendor_uuid": uuid,
                "sid": ctx.sid,
                "slug": ctx.slug,
                "role": spec.role,
                "project_dir": ctx.project_dir.to_string_lossy(),
                "cwd": ctx.cwd.to_string_lossy(),
            }),
        })
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let Some(live) = self.lookup(&h.identity) else {
            return Err(HarnessError::SubmitFailed(format!(
                "stream-json session not live: {} (resume_thread / start_thread first)",
                h.identity
            )));
        };
        let text = match input {
            TurnInput::UserText(s) => s,
            TurnInput::Artifact(p) => {
                format!("Look at the file I just placed at {}", p.display())
            }
            TurnInput::Image(p) => {
                format!("Look at the image I just placed at {}", p.display())
            }
            TurnInput::ToolResult { call_id, content } => {
                let body = match content {
                    serde_json::Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                format!("Tool result for {call_id}: {body}")
            }
        };
        live.transport
            .send_line(protocol::user_text_line(&text))
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("stream-json send: {e:#}")))?;

        // Synthesize a turn id (the pump keys turns.jsonl off its own seq;
        // this id is only for adapter-side correlation / logs).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(TurnId::new(format!("turn-{nanos:x}")))
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let Some(live) = self.lookup(&h.identity) else {
            // No live session (resumed handle pre-spawn / unknown): empty
            // stream. The gateway resume path re-establishes via
            // start_thread, then re-subscribes.
            return Box::pin(futures::stream::empty());
        };
        let mut sub = live.transport.subscribe();
        let transport = Arc::clone(&live.transport);
        let (tx, rx) = mpsc::channel::<ThreadEvent>(64);
        tokio::spawn(async move {
            let mut translator = StreamTranslator::new();
            loop {
                tokio::select! {
                    msg = sub.recv() => match msg {
                        Ok(out) => {
                            if forward(&mut translator, &tx, out).await.is_err() {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n, "claude-stream-json: events subscriber lagged");
                        }
                        // The transport was dropped — emit the in-flight signal.
                        Err(broadcast::error::RecvError::Closed) => {
                            if let Some(ev) = translator.on_close() {
                                let _ = tx.send(ev).await;
                            }
                            return;
                        }
                    },
                    // The broadcast sender lives on the transport, so a dead
                    // child never yields `Closed` here — the explicit close
                    // signal does. Drain any buffered messages first (so a
                    // final answer emitted just before EOF isn't lost), then —
                    // if a turn was still in flight — emit the honest
                    // in-flight-loss signal before ending the stream (E3).
                    _ = transport.wait_closed() => {
                        while let Ok(out) = sub.try_recv() {
                            if forward(&mut translator, &tx, out).await.is_err() {
                                return;
                            }
                        }
                        if let Some(ev) = translator.on_close() {
                            let _ = tx.send(ev).await;
                        }
                        return;
                    }
                }
            }
        });
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        }))
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        // A live session for this uuid (idle wake within one daemon
        // lifetime) → hand back a handle pointing at it. Otherwise we
        // cannot rebuild without the SpawnCtx (cwd/role): the gateway falls
        // back to `start_thread`, which IS resume-aware (deterministic uuid
        // + jsonl-presence → `--resume`).
        if let Some(live) = self.lookup(persistent_id) {
            if live.transport.is_initialized() {
                return Ok(ThreadHandle {
                    vendor: AgentVendor::Claude,
                    mode: ExecutionMode::Chat,
                    identity: persistent_id.to_string(),
                    started_at: Utc::now(),
                    raw_extras: json!({
                        "adapter": STREAM_JSON_ADAPTER_NAME,
                        "protocol": "stream-json",
                        "host": live.identity.host,
                        "vendor_uuid": persistent_id,
                        "sid": live.identity.sid,
                        "slug": live.slug,
                        "role": live.role,
                        "project_dir": live.project_dir.to_string_lossy(),
                        "cwd": live.cwd.to_string_lossy(),
                    }),
                });
            }
        }
        Err(HarnessError::NotImplemented {
            reason: format!(
                "stream-json resume of {persistent_id} needs the SpawnCtx; \
                 caller must invoke start_thread (resume-aware via the \
                 deterministic per-sid uuid + --resume)"
            ),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let live = self.live.lock().unwrap().remove(&h.identity);
        if let Some(live) = live {
            live.transport.shutdown().await;
        }
        Ok(())
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        // Bridge gate (PRD E1): classify against the live init command
        // table. ccteam's own IM commands never reach here — the gateway
        // intercepts them before `handle_directive`.
        let commands = self
            .lookup(&h.identity)
            .map(|live| live.commands.clone())
            .unwrap_or_default();
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();
        // `/model <id>` IS driveable in stream-json — the TUI picker has no
        // headless form, but the SDK control channel does (`set_model`). Handle
        // it BEFORE the bridge gate so it never falls into a DIALOG reject or a
        // verbatim passthrough. Empty arg → a usage hint (no pane to open a
        // picker on). Real-vendor `set_model` support is confirmed at smoke; an
        // unsupported build returns an error subtype → an honest refusal here.
        // `/effort <level>` — set the reasoning effort. There is NO live
        // `set_effort` control, but `apply_flag_settings` merges `{effortLevel}`
        // into the runtime flagSettings layer and updates the active config
        // IMMEDIATELY — no restart, no context loss (the mechanism iOS/remote
        // clients use; verified empirically). Also persist to settings.local.json
        // so the level survives an idle-release / `--resume`.
        if name == "effort" {
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "effort: no live stream-json session for this handle".into(),
                ));
            };
            let Some(level) = normalize_effort(&d.args) else {
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!(
                        "用法: /effort <{}>（reasoning effort，live 生效）",
                        EFFORT_LEVELS.join("|")
                    ),
                });
            };
            return match apply_flag_settings_live(&live, json!({ "effortLevel": level })).await {
                Ok(()) => {
                    // Live now → persist for resume + reflect truthfully.
                    let _ = set_effort_level(&live.cwd, &level);
                    if let Ok(mut s) = live.status.lock() {
                        s.effort = Some(level.clone());
                    }
                    Ok(DirectiveOutcome::Done {
                        receipt: format!("已切换 effort → {level}（live 生效）"),
                    })
                }
                Err(why) => Ok(DirectiveOutcome::Rejected {
                    reason: format!("/effort 切换失败: {why}"),
                }),
            };
        } else if name == "set" {
            // Generic live-config escape hatch: `/set <key> <value>` merges one
            // setting into the active config via `apply_flag_settings` (the same
            // runtime-settings hook `/effort` uses). Value = JSON if it parses,
            // else a bare string. Fenced off the safety/HITL boundary.
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "set: no live stream-json session for this handle".into(),
                ));
            };
            let args = d.args.trim();
            let mut it = args.splitn(2, char::is_whitespace);
            let key = it.next().unwrap_or("").trim();
            let raw = it.next().unwrap_or("").trim();
            if key.is_empty() || raw.is_empty() {
                return Ok(DirectiveOutcome::Rejected {
                    reason: "用法: /set <settings-key> <value>（live 应用一个 Claude 设置，如 /set effortLevel xhigh）".into(),
                });
            }
            if SET_PROTECTED_KEYS.contains(&key) {
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!("/set 不允许改 `{key}`（安全/HITL 边界，受保护）"),
                });
            }
            let value: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
            return match apply_flag_settings_live(&live, json!({ key: value.clone() })).await {
                Ok(()) => Ok(DirectiveOutcome::Done {
                    receipt: format!("已 live 应用: {key} = {value}"),
                }),
                Err(why) => Ok(DirectiveOutcome::Rejected {
                    reason: format!("/set {key} 失败: {why}"),
                }),
            };
        } else if name == "model" {
            let arg = d.args.trim();
            let Some(live) = self.lookup(&h.identity) else {
                return Err(HarnessError::SubmitFailed(
                    "set_model: no live stream-json session for this handle".into(),
                ));
            };
            if arg.is_empty() {
                return Ok(DirectiveOutcome::Rejected {
                    reason:
                        "用法: /model <model-id> [effort]（无交互选择器，直接给 model id；可选 effort=low|medium|high|xhigh|max，live 生效）"
                            .into(),
                });
            }
            // "<model> [effort]" — effort is the trailing token iff a valid level
            // (so `/model opus[1m] xhigh` splits but `/model opus-4-8` doesn't).
            let (model, effort) = split_model_effort(arg);
            let body = live
                .transport
                .request_control("set_model", json!({ "model": model }), init_timeout())
                .await
                .map_err(|e| HarnessError::SubmitFailed(format!("set_model 失败: {e}")))?;
            if body.subtype != "success" {
                let why = body
                    .error
                    .unwrap_or_else(|| "vendor rejected set_model".into());
                return Ok(DirectiveOutcome::Rejected {
                    reason: format!("/model 切换失败: {why}"),
                });
            }
            if let Ok(mut s) = live.status.lock() {
                s.model = Some(model.clone());
            }
            // Optional effort rides along — now also LIVE via apply_flag_settings.
            let mut receipt = format!("已切换 model → {model}（live）");
            if let Some(level) = effort {
                match apply_flag_settings_live(&live, json!({ "effortLevel": level })).await {
                    Ok(()) => {
                        let _ = set_effort_level(&live.cwd, &level);
                        if let Ok(mut s) = live.status.lock() {
                            s.effort = Some(level.clone());
                        }
                        receipt.push_str(&format!("；effort → {level}（live）"));
                    }
                    Err(why) => receipt.push_str(&format!("；effort 切换失败: {why}")),
                }
            }
            return Ok(DirectiveOutcome::Done { receipt });
        }
        match bridge::classify_slash(&name, &commands) {
            SlashClass::Reject => Ok(DirectiveOutcome::Rejected {
                reason: bridge::reject_reason(&name),
            }),
            SlashClass::Passthrough => {
                // Known prompt/local (incl. /compact /clear /context) OR
                // unknown → forward verbatim as user text.
                let line = if d.args.trim().is_empty() {
                    format!("/{name}")
                } else {
                    format!("/{name} {}", d.args.trim())
                };
                let turn = self.submit_turn(h, TurnInput::UserText(line)).await?;
                Ok(DirectiveOutcome::Turn(turn))
            }
        }
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // Live model + context-window usage, kept current by the per-session
        // status tap ([`spawn_status_tap`]) folding each turn's `usage`. A live
        // session WITH context is authoritative; otherwise fall back to the
        // persisted snapshot ([`status_json_path`]) so a released / resumed
        // session (idle-release, daemon restart — spawn-on-demand) still shows
        // its statusline, the same durability the TUI gets from the transcript.
        let live = self
            .lookup(&h.identity)
            .map(|l| l.status.lock().unwrap().clone());
        if let Some(s) = &live {
            if s.context.is_some() {
                return Ok(s.clone());
            }
        }
        let persisted = h
            .raw_extras
            .get("project_dir")
            .and_then(|v| v.as_str())
            .zip(h.raw_extras.get("sid").and_then(|v| v.as_str()))
            .and_then(|(pd, sid)| read_status_file(Path::new(pd), sid));
        Ok(match (live, persisted) {
            // Live (model from init, no turn yet) + persisted context → show
            // the live model with the last-known context.
            (Some(l), Some(p)) => ThreadStatus {
                model: l.model.or(p.model),
                context: p.context,
                effort: l.effort.or(p.effort),
            },
            (Some(l), None) => l,
            (None, Some(p)) => p,
            (None, None) => ThreadStatus::default(),
        })
    }
}

#[cfg(test)]
mod effort_tests {
    use super::{
        normalize_effort, preserve_1m_tag, set_effort_level, split_model_effort, EFFORT_LEVELS,
    };

    #[test]
    fn preserve_1m_tag_carries_user_intent_without_inventing_it() {
        // User set `opus[1m]`; the API id is bare → carry the tag over (so the
        // window heuristic keeps 1M and the statusline keeps showing [1m]).
        assert_eq!(
            preserve_1m_tag(Some("opus[1m]"), "claude-opus-4-8"),
            "claude-opus-4-8[1m]"
        );
        assert_eq!(
            preserve_1m_tag(Some("claude-opus-4-8[1m]"), "claude-opus-4-8"),
            "claude-opus-4-8[1m]"
        );
        // No [1m] in the current model → never invent one (a 200k model stays 200k).
        assert_eq!(
            preserve_1m_tag(Some("claude-sonnet-4-6"), "claude-sonnet-4-6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(preserve_1m_tag(None, "claude-opus-4-8"), "claude-opus-4-8");
        // API id already carries [1m] → don't double it.
        assert_eq!(
            preserve_1m_tag(Some("opus[1m]"), "claude-opus-4-8[1m]"),
            "claude-opus-4-8[1m]"
        );
    }

    #[test]
    fn normalize_effort_accepts_levels_case_insensitively_and_rejects_others() {
        for lvl in EFFORT_LEVELS {
            assert_eq!(normalize_effort(lvl).as_deref(), Some(*lvl));
        }
        assert_eq!(normalize_effort("XHigh").as_deref(), Some("xhigh"));
        assert_eq!(normalize_effort(" max ").as_deref(), Some("max"));
        assert_eq!(normalize_effort("turbo"), None);
        assert_eq!(normalize_effort("opus-4-8"), None);
        assert_eq!(normalize_effort(""), None);
    }

    #[test]
    fn split_model_effort_only_peels_a_valid_trailing_level() {
        assert_eq!(
            split_model_effort("opus[1m] xhigh"),
            ("opus[1m]".to_string(), Some("xhigh".to_string()))
        );
        assert_eq!(
            split_model_effort("claude-opus-4-8 max"),
            ("claude-opus-4-8".to_string(), Some("max".to_string()))
        );
        // No trailing level → the whole arg is the model (never mis-split).
        assert_eq!(
            split_model_effort("claude-opus-4-8"),
            ("claude-opus-4-8".to_string(), None)
        );
        assert_eq!(
            split_model_effort("opus[1m]"),
            ("opus[1m]".to_string(), None)
        );
    }

    #[test]
    fn set_effort_level_writes_effortlevel_preserving_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        // Pre-existing sibling key must survive the effortLevel write.
        std::fs::write(&settings, r#"{"enabledPlugins":{"x@y":true}}"#).unwrap();

        set_effort_level(dir.path(), "xhigh").unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["effortLevel"], "xhigh");
        assert_eq!(v["enabledPlugins"]["x@y"], true, "sibling preserved");

        // Idempotent overwrite of the same key.
        set_effort_level(dir.path(), "high").unwrap();
        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v2["effortLevel"], "high");
        assert_eq!(v2["enabledPlugins"]["x@y"], true);
    }

    #[test]
    fn set_effort_level_creates_settings_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        set_effort_level(dir.path(), "medium").unwrap();
        let body = std::fs::read_to_string(dir.path().join(".claude").join("settings.local.json"))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["effortLevel"], "medium");
    }
}
