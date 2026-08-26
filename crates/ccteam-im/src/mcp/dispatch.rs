//! Daemon-side MCP dispatch: stateful intercepts + protocol-core fallback.
//!
//! Owns the live gateway / pending registry / event sink needed for
//! `interaction/ask`, `permission/ask`, `chat_send_file`, `session_*`, and
//! `ccteam/reload`. Both transports on top stay thin: the local `mcp.sock` loop
//! does read-line → [`McpDispatch::dispatch`] → write-line, and ccteam-web's
//! `POST /mcp` resolves the caller's credential, then calls
//! [`McpDispatch::dispatch_as`] with the tier it proved.

use std::sync::Arc;

use ccteam_core::CcteamPaths;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::gateway::{Gateway, GatewayEvent};
use crate::pending::PendingInteractions;

use super::protocol;

/// Sender half of the gateway-event channel that the IM daemon consumes.
pub type GatewayEventSink = tokio::sync::mpsc::UnboundedSender<GatewayEvent>;

/// Shared pending-interaction registry (gateway + MCP handler both hold it).
pub type PendingRegistry = Arc<Mutex<PendingInteractions>>;

/// Shared gateway handle so `session_*` tools drive the in-memory session map.
pub type GatewayHandle = Arc<Mutex<Gateway>>;

/// Daemon-side MCP request dispatcher.
///
/// Fields are `Option` so a socket connection still works when IM / web
/// pieces are not wired (structured errors for stateful tools; protocol
/// core still serves `status` / `tools/list`).
pub struct McpDispatch {
    /// ccteam path layout (home + projects root).
    pub paths: CcteamPaths,
    /// Outbound IM funnel (file sends + HITL buttons).
    pub sink: Option<GatewayEventSink>,
    /// Shared pending-interaction registry for External-origin prompts.
    pub pending: Option<PendingRegistry>,
    /// Live gateway session map.
    pub gateway: Option<GatewayHandle>,
}

/// Who is invoking the dispatch — decides how the privileged intercepts
/// authenticate (v0.9 T4 review fix).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum McpCaller {
    /// A caller that speaks for ONE session: `session_*` authenticates by the
    /// `(sid, secret)` principal carried in the `_caller_*` args, and the
    /// internal-bus methods (`interaction/ask`, `permission/ask`) are served.
    ///
    /// Every `POST /mcp` caller lands here — a managed session under its own
    /// principal, or a hand-started client under the ledger node the daemon
    /// minted for its enrollment binding at `initialize` — as does an mcp.sock
    /// line that presents no admin token.
    Ambient,
    /// The local `mcp.sock` caller that presented the admin web token
    /// (`_caller_admin_token`, promoted by `McpDispatch::promote_local_admin`):
    /// reading that 0600 file proves same-uid, so `session_*` skips the
    /// per-session principal gate and names its target with an explicit
    /// `project`; the internal-bus methods are NOT exposed (in-band /
    /// daemon-internal responsibility, not an operator API).
    ///
    /// **Not reachable over HTTP.** `POST /mcp` resolves only a session
    /// principal or an enrollment credential and strips `_caller_admin_token`:
    /// a durable credential a static vendor config can carry cannot say which
    /// process is speaking, so the data plane's admin tier was deleted rather
    /// than narrowed. Nothing ccteam ships injects the arg any more either (the
    /// stdio forwarder that did is gone), so in practice this tier is reached
    /// only by a same-uid local client that reads the token and writes to the
    /// socket itself.
    Admin,
    /// A per-user tenant: a human/root caller like Admin, but every project/sid
    /// operation is scoped to projects owned by `user:<user_id>`.
    ///
    /// **No production producer.** It existed for a tenant web bearer at
    /// `POST /mcp`, which that route no longer accepts — a tenant's external
    /// agent enrolls instead, and the credential's owner carries the same
    /// scoping into an Ambient node. The variant survives as the tenant-scoping
    /// arm the tests drive directly.
    User {
        /// Stable tenant identity id resolved from the bearer registry.
        user_id: String,
    },
}

impl McpCaller {
    fn user_id(&self) -> Option<&str> {
        match self {
            Self::User { user_id } => Some(user_id),
            Self::Ambient | Self::Admin => None,
        }
    }

    /// True for the tiers that are not one session speaking for itself (Admin /
    /// User). The name is historical — neither is an HTTP door any more — but
    /// the distinction it draws is the one the internal-bus refusal needs.
    fn is_front_door(&self) -> bool {
        !matches!(self, Self::Ambient)
    }
}

impl McpDispatch {
    /// Dispatch one JSON-RPC request arriving on the local `mcp.sock` path.
    /// Wire-compatible with the historical `handle_mcp_socket_connection`.
    ///
    /// A LOCAL caller may present the admin web token (`_caller_admin_token` in
    /// the tool arguments). A matching token promotes the call to
    /// [`McpCaller::Admin`]; the token file is `0600` under
    /// `~/.ccteam/secrets/`, so presenting it proves same-user file access,
    /// exactly like running the `ccteam` CLI. A missing or wrong token leaves
    /// the call on the fail-closed Ambient path. The arg is stripped either way
    /// so nothing downstream ever sees it.
    ///
    /// This socket is now the ONLY way that tier is reached: the stdio forwarder
    /// that used to inject the arg for a hand-started session is deleted, and
    /// `POST /mcp` has no admin tier to promote into (a hand-started session
    /// enrolls and gets a real principal instead). What survives here is the
    /// same-uid trust the socket already implies, not a route ccteam drives.
    pub async fn dispatch(&self, req: Value) -> Option<Value> {
        let (req, caller) = self.promote_local_admin(req);
        self.dispatch_as(req, caller).await
    }

    /// Socket-only admin promotion (see [`Self::dispatch`]). Constant-time
    /// token compare; never logs the presented value.
    fn promote_local_admin(&self, mut req: Value) -> (Value, McpCaller) {
        let presented = match req
            .pointer_mut("/params/arguments")
            .and_then(|a| a.as_object_mut())
        {
            Some(args) => match args.remove("_caller_admin_token") {
                Some(v) => v.as_str().unwrap_or_default().to_string(),
                None => return (req, McpCaller::Ambient),
            },
            None => return (req, McpCaller::Ambient),
        };
        let expected = std::fs::read_to_string(self.paths.web_token_path())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if !expected.is_empty() && ccteam_core::session_secret::ct_eq(&expected, &presented) {
            (req, McpCaller::Admin)
        } else {
            (req, McpCaller::Ambient)
        }
    }

    /// Is this Ambient caller an EXTERNAL ledger node rather than a session
    /// ccteam spawned?
    ///
    /// The internal bus (`interaction/ask` / `permission/ask`) is refused for
    /// the front-door tiers, which used to be the same thing as "not one of
    /// ccteam's own sessions". Enrollment broke that equivalence: a
    /// hand-started agent now arrives Ambient too, holding a principal ccteam
    /// minted for its node. Legitimate for `session_*` — that is the whole
    /// point of enrolling — but the ask bus is how ccteam's OWN sessions get a
    /// human in front of a blocked tool call, and an outside process should not
    /// be able to raise a prompt in the operator's IM that is indistinguishable
    /// from one.
    ///
    /// Read from the LEDGER, not from the tier, so any future caller class that
    /// arrives with an external node's sid is covered without another patch.
    async fn caller_is_external_node(&self, req: &Value) -> bool {
        let Some(sid) = req
            .pointer("/params/arguments/_caller_sid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return false;
        };
        let Some(gateway) = self.gateway.as_ref() else {
            return false;
        };
        gateway.lock().await.is_external_node(sid)
    }

    /// Dispatch one JSON-RPC request as `caller`. Order matches the historical
    /// `handle_mcp_socket_connection` intercept chain exactly:
    /// `interaction/ask` → `permission/ask` → `chat_send_file` →
    /// `session_*` → `ccteam/reload` → protocol core.
    pub async fn dispatch_as(&self, mut req: Value, caller: McpCaller) -> Option<Value> {
        // A tenant web bearer is the sole identity source. Strip every private
        // caller field before routing so no tool can mistake client-supplied
        // `_caller_*` metadata for a managed-session principal or project scope.
        if caller.user_id().is_some() {
            strip_untrusted_caller_args(&mut req);
        }
        if is_interaction_ask_call(&req) {
            if caller.is_front_door() || self.caller_is_external_node(&req).await {
                return Some(internal_bus_not_exposed(&req));
            }
            Some(
                execute_interaction_ask(
                    &req,
                    self.sink.as_ref(),
                    self.pending.as_ref(),
                    self.gateway.as_ref(),
                )
                .await,
            )
        } else if is_permission_ask_call(&req) {
            if caller.is_front_door() || self.caller_is_external_node(&req).await {
                return Some(internal_bus_not_exposed(&req));
            }
            Some(
                execute_permission_ask(
                    &req,
                    self.sink.as_ref(),
                    self.pending.as_ref(),
                    self.gateway.as_ref(),
                )
                .await,
            )
        } else if is_chat_send_file_call(&req) {
            Some(
                execute_chat_send_file(
                    &req,
                    self.sink.as_ref(),
                    self.gateway.as_ref(),
                    &caller,
                    &self.paths,
                )
                .await,
            )
        } else if is_session_tool_call(&req) {
            Some(
                execute_session_tool_with_paths(
                    &req,
                    self.gateway.as_ref(),
                    caller.clone(),
                    &self.paths,
                )
                .await,
            )
        } else if is_status_call(&req) {
            Some(execute_status(&req, self.gateway.as_ref(), caller.clone(), &self.paths).await)
        } else if is_reload_call(&req) {
            if caller.user_id().is_some() {
                return Some(internal_bus_not_exposed(&req));
            }
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let ok = if let Some(gw) = self.gateway.as_ref() {
                gw.lock().await.request_im_reload()
            } else {
                false
            };
            Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "reloaded": ok },
            }))
        } else {
            protocol::handle_request(&self.paths, &req).await
        }
    }
}

/// Remove every private caller field from an external user's tool arguments.
/// Known fields are not enumerated deliberately: future `_caller_*` additions
/// are fail-closed automatically.
fn strip_untrusted_caller_args(req: &mut Value) {
    if let Some(args) = req
        .pointer_mut("/params/arguments")
        .and_then(Value::as_object_mut)
    {
        strip_caller_args(args);
    }
}

fn strip_caller_args(args: &mut serde_json::Map<String, Value>) {
    args.retain(|key, _| !key.starts_with("_caller_"));
}

fn user_can_see_project(paths: &CcteamPaths, user_id: &str, slug: &str) -> bool {
    ccteam_core::ProjectState::load(&paths.project_state(slug))
        .map(|state| ccteam_core::identity::can_see_owner(user_id, false, state.owner.as_deref()))
        .unwrap_or(false)
}

fn visible_user_projects(paths: &CcteamPaths, user_id: &str) -> Vec<String> {
    ccteam_core::collect_projects(paths)
        .unwrap_or_default()
        .into_iter()
        .filter(|project| {
            ccteam_core::identity::can_see_owner(user_id, false, project.state.owner.as_deref())
        })
        .map(|project| project.state.slug)
        .collect()
}

/// MCP-DX-1 — cap on how many project slugs an error message enumerates.
const ERROR_PROJECT_LIST_MAX: usize = 20;

/// Registered project slugs from the config catalog (the same SoT `status`
/// lists, local + satellite-bound). Read-only; used to make project-resolution
/// errors actionable instead of a dead end.
fn registered_project_slugs(paths: &CcteamPaths) -> Vec<String> {
    ccteam_core::collect_projects(paths)
        .unwrap_or_default()
        .into_iter()
        .map(|project| project.state.slug)
        .collect()
}

/// Bounded, comma-separated slug list for error messages.
fn format_slug_list(slugs: &[String]) -> String {
    let shown: Vec<&str> = slugs
        .iter()
        .take(ERROR_PROJECT_LIST_MAX)
        .map(String::as_str)
        .collect();
    let mut out = shown.join(", ");
    if slugs.len() > shown.len() {
        out.push_str(&format!(", … ({} total)", slugs.len()));
    }
    out
}

/// Admin-facing catalog hint appended to project-resolution errors. `None`
/// paths (test-only construction) → empty.
fn admin_project_catalog_hint(paths: Option<&CcteamPaths>) -> String {
    let Some(paths) = paths else {
        return String::new();
    };
    let slugs = registered_project_slugs(paths);
    if slugs.is_empty() {
        " — no projects are registered yet (run `ccteam init` in a project directory, or IM `/newproject`)".to_string()
    } else {
        format!(" — registered projects: {}", format_slug_list(&slugs))
    }
}

/// MCP-DX-2 — the sole registered project, when the catalog holds exactly
/// one. Used as the unambiguous default for an admin `session_spawn` that
/// names no project (two or more candidates keep the explicit-or-error
/// contract; zero keeps the "no projects registered" hint).
fn sole_registered_project(paths: Option<&CcteamPaths>) -> Option<String> {
    let slugs = registered_project_slugs(paths?);
    match slugs.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Tenant-facing hint listing ONLY the caller's own visible projects. The text
/// is a pure function of the caller identity — never of the probed input — so
/// a foreign and a nonexistent project keep byte-identical errors (no
/// existence disclosure; see `user_spawn_requires_own_explicit_project`).
fn user_project_list_hint(visible: &[String]) -> String {
    if visible.is_empty() {
        " — you have no projects yet (create one from the web console)".to_string()
    } else {
        format!(" — your projects: {}", format_slug_list(visible))
    }
}

/// Character-level Levenshtein distance (slugs are short; the O(n·m) DP is
/// plenty). Used only for admin-facing "did you mean" hints.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row.push((prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1));
        }
        prev = row;
    }
    prev[b.len()]
}

/// Closest registered slug for a "did you mean" hint: containment (≥3 chars)
/// or an edit distance within half the longer name. `None` when nothing is
/// reasonably close (a wild guess is worse than no hint).
fn nearest_slug<'a>(input: &str, candidates: &'a [String]) -> Option<&'a str> {
    let input_lower = input.to_lowercase();
    candidates
        .iter()
        .map(|candidate| {
            let cand_lower = candidate.to_lowercase();
            let contained = input_lower.len() >= 3
                && (cand_lower.contains(&input_lower) || input_lower.contains(&cand_lower));
            let distance = if contained {
                0
            } else {
                levenshtein(&input_lower, &cand_lower)
            };
            (distance, candidate)
        })
        .filter(|(distance, candidate)| {
            *distance <= 2.max(input.chars().count().max(candidate.chars().count()) / 2)
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.as_str())
}

/// v0.8.7 W1 — cap on how many child turns `session_collect` returns when the
/// caller doesn't pass `n`. Keeps a runaway transcript from flooding the
/// cto's context in one poll.
const SESSION_COLLECT_DEFAULT_N: usize = 20;
const SESSION_COLLECT_DEFAULT_MAX_CHARS: usize = 10_000;
const SESSION_COLLECT_MIN_MAX_CHARS: usize = 500;
const SESSION_COLLECT_MAX_MAX_CHARS: usize = 50_000;
use crate::delegation::{DelegationOutcome, DelegationSummary};

/// Keep inline waits below the shortest common MCP client deadline (~300s),
/// leaving 60s for spawn/submit work around the wait itself. The wire still
/// accepts the documented 0..=600 request range; only execution is capped.
const EFFECTIVE_INLINE_WAIT_CEILING_SECONDS: u64 = 240;

fn requested_inline_wait_seconds(args: &serde_json::Value) -> u64 {
    args.get("wait_seconds")
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
        .min(600)
}

fn effective_inline_wait_seconds(requested: u64) -> u64 {
    requested.min(EFFECTIVE_INLINE_WAIT_CEILING_SECONDS)
}

/// v0.8.5 D6 — how long the `interaction/ask` handler waits for the user to
/// answer before forgetting the prompt + reporting a timeout (the hook then
/// degrades to deny-with-reason). Matches the gateway pending TTL default.
const INTERACTION_ASK_TIMEOUT_SECS: u64 = 600;

/// v0.8.7 review-fix (R-L1) — a HITL `permission/ask` prompt gets a SHORTER
/// deadline than the 600s `interaction/ask`: a tool-approval blocks the
/// agent's whole turn, so a long park is worse than a fast fail-safe deny.
/// On lapse the hook still denies (fail-safe = deny). Env-overridable
/// (`CCTEAM_PERMISSION_PROMPT_TTL_SECS`) for ops + tests.
///
/// v0.8.22 P0-2 — delegates to [`crate::hitl::permission_prompt_timeout_secs`]
/// (the SAME knob the stream-json protocol's in-process HITL resolver reads),
/// so the terminal protocol's `permission/ask` hook and the stream-json
/// protocol's `can_use_tool` resolver can never drift on the TTL.
fn permission_prompt_timeout_secs() -> u64 {
    crate::hitl::permission_prompt_timeout_secs()
}

/// JSON-RPC `-32601` for the internal-bus methods (`interaction/ask`,
/// `permission/ask`) on the Admin / User tiers: those are in-band /
/// daemon-internal responsibilities, deliberately not an operator API
/// (tech-design v0.9 §1.1 — HITL stays on vendor-native channels).
///
/// Scope note: the gate is the caller TIER, not the transport. Since `POST /mcp`
/// resolves every caller to Ambient, an HTTP caller — a managed session or an
/// enrolled client — reaches these methods; what is refused is the local
/// admin-token promotion (and the test-only tenant tier).
fn internal_bus_not_exposed(req: &serde_json::Value) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("method not available on this transport: {method}"),
        },
    })
}

/// Monotonic id source so each `chat_send_file` gets a distinct durable
/// ledger row (avoids `{id}-0` collisions in `outbound.jsonl`).
static CHAT_SEND_FILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn is_chat_send_file_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req.pointer("/params/name").and_then(|n| n.as_str()) == Some("chat_send_file")
}

#[derive(Debug, Clone)]
struct ChatSendFileTarget {
    channel: String,
    chat_id: String,
    /// Present for a live ccteam session. The server-resolved project path is
    /// the only authority the web staging/persistence path trusts.
    session: Option<crate::gateway::SessionResolve>,
}

impl ChatSendFileTarget {
    fn delivery_only((channel, chat_id): (String, String)) -> Self {
        Self {
            channel,
            chat_id,
            session: None,
        }
    }
}

/// Resolve addressing, validate the file, and enqueue a `GatewayEvent`
/// onto the shared sink (the IM consumer does the actual `sendPhoto` /
/// `sendDocument`). Returns a tools/call-shaped JSON-RPC response.
async fn execute_chat_send_file(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    gateway: Option<&GatewayHandle>,
    caller: &McpCaller,
    paths: &CcteamPaths,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    // v0.8.7 (FIX-1) — resolve the live session's reply target FIRST, under
    // the gateway lock, then DROP the guard before any fs read / send (lock
    // discipline §7-1, mirroring run_session_collect). `None` here means no
    // live (project, role) session is tracked → run_chat_send_file falls back
    // to the on-disk registry. We resolve here (async) and inject the result
    // into the sync builder so build_send_file_event stays unit-testable.
    let live_target = match caller.user_id() {
        Some(user_id) => match user_delivery_target(paths, user_id) {
            Ok(target) => Some(ChatSendFileTarget::delivery_only(target)),
            Err(text) => return session_tool_response(id, text, true),
        },
        None => resolve_live_reply_target(&args, gateway).await,
    };
    let (text, is_error) = match run_chat_send_file(&args, sink, gateway, paths, live_target).await
    {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        },
    })
}

/// Resolve an external tenant's own IM destination from the tenant registry.
/// No tool argument participates in addressing. A durable `linked_chat` wins;
/// otherwise a configured per-tenant bot may supply its first allowlisted
/// recipient. A bot without a known recipient is not deliverable yet.
pub(crate) fn user_delivery_target(
    paths: &CcteamPaths,
    user_id: &str,
) -> std::result::Result<(String, String), String> {
    let registry = ccteam_core::tenants::TenantRegistry::load(&paths.users_dir());
    let tenant = registry.by_id(user_id).ok_or_else(|| {
        "chat_send_file: authenticated user is no longer registered; refresh the MCP credential"
            .to_string()
    })?;

    if let Some(linked) = tenant.linked_chat.as_deref() {
        let (platform, chat_id) = linked.split_once(':').ok_or_else(|| {
            "chat_send_file: linked IM is invalid; expected `channel:chat_id`".to_string()
        })?;
        if platform.is_empty() || chat_id.is_empty() || platform == "web" {
            return Err(
                "chat_send_file: no linked IM destination is configured for this user".to_string(),
            );
        }
        let channel = match platform {
            "telegram" if tenant.telegram.is_some() => format!("telegram@{user_id}"),
            "lark" if tenant.lark.is_some() => format!("lark@{user_id}"),
            other => other.to_string(),
        };
        return Ok((channel, chat_id.to_string()));
    }

    if let Some(chat_id) = tenant
        .telegram
        .as_ref()
        .and_then(|telegram| telegram.allowed_chat_ids.first())
    {
        return Ok((format!("telegram@{user_id}"), chat_id.clone()));
    }
    if let Some(open_id) = tenant
        .lark
        .as_ref()
        .and_then(|lark| lark.allowed_user_ids.first())
    {
        return Ok((format!("lark@{user_id}"), open_id.clone()));
    }

    Err(
        "chat_send_file: no linked IM destination is configured for this user; link an IM chat or configure the tenant bot recipient first"
            .to_string(),
    )
}

/// v0.8.7 (FIX-1) — resolve the live `(channel, chat_id)` for the firing
/// session the `chat_send_file` args name, by looking up the gateway's
/// in-memory session map. The gateway guard is taken and dropped INSIDE this
/// fn (the lookup is sync + holds no `.await`) so callers never hold it across
/// an fs read / send. `None` when no gateway handle, no firing sid, or no
/// tracked session matches.
///
/// v0.8.8 F1 — keyed by the firing session's ccteam sid (`_caller_sid`,
/// injected by the in-pane `forward_chat_send_file` forwarder from
/// `CCTEAM_CHAT_SID`): post-dedup `(slug, role)` is no longer unique, so the
/// sid is the only safe way to reach the SPECIFIC session's reply target.
async fn resolve_live_reply_target(
    args: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
) -> Option<ChatSendFileTarget> {
    let gw = gateway?;
    let sid = args
        .get("_caller_sid")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if sid.is_empty() {
        return None;
    }
    let guard = gw.lock().await;
    let (channel, chat_id) = guard.reply_target_for(sid)?;
    // IM delivery historically needs only the live reply binding. Project
    // metadata is an additional requirement solely for the web copy/read
    // path, so do not make Telegram/Lark depend on it.
    let session = if channel == "web" {
        guard.session_resolve(sid)
    } else {
        None
    };
    Some(ChatSendFileTarget {
        channel,
        chat_id,
        session,
    })
}

async fn run_chat_send_file(
    args: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    gateway: Option<&GatewayHandle>,
    paths: &CcteamPaths,
    live_target: Option<ChatSendFileTarget>,
) -> std::result::Result<String, String> {
    let sink = sink.ok_or_else(|| "chat_send_file: IM gateway not running".to_string())?;
    let seq = CHAT_SEND_FILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let event_target = live_target
        .as_ref()
        .map(|target| (target.channel.clone(), target.chat_id.clone()));
    let mut event = build_send_file_event(args, seq, event_target)?;
    if event.channel == "web" {
        let session = live_target
            .as_ref()
            .and_then(|target| target.session.as_ref())
            .ok_or_else(|| "chat_send_file: web delivery has no live session scope".to_string())?;
        stage_web_outbound_file(&mut event, session, paths, seq)?;
    }
    let dest = format!("{}/{}", event.channel, event.chat_id);
    sink.send(event.clone())
        .map_err(|_| "chat_send_file: gateway sink closed".to_string())?;
    // The MCP dispatcher owns the delivery mpsc, while the gateway owns the
    // per-session SSE fan-out. Publish the web reference after enqueueing so
    // the current SPA renders it live; no bytes or daemon path enter the SSE.
    if event.channel == "web" {
        if let Some(gateway) = gateway {
            gateway.lock().await.broadcast_external_event(event);
        }
    }
    Ok(format!("delivered: queued to {dest}"))
}

/// Telegram bot-send ceilings: `sendPhoto` ≤ 10 MB, `sendDocument` ≤ 50 MB.
const OUTBOUND_PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;
const OUTBOUND_DOCUMENT_MAX_BYTES: u64 = 50 * 1024 * 1024;

fn outbound_max_bytes(kind: crate::transport::OutboundFileKind) -> u64 {
    match kind {
        crate::transport::OutboundFileKind::Photo => OUTBOUND_PHOTO_MAX_BYTES,
        crate::transport::OutboundFileKind::Document => OUTBOUND_DOCUMENT_MAX_BYTES,
    }
}

fn project_host(paths: &CcteamPaths, slug: &str) -> String {
    ccteam_core::config::load(&paths.root)
        .ok()
        .and_then(|config| {
            config
                .projects
                .into_iter()
                .find(|project| project.slug == slug)
        })
        .map(|project| project.host)
        .unwrap_or_else(|| ccteam_core::LOCAL_HOST.to_string())
}

/// Copy a web-bound outbound file into the owning project's asset directory,
/// attach a basename handle, and append the reference-only transcript row.
/// The agent-supplied source path never becomes a browser URL.
fn stage_web_outbound_file(
    event: &mut crate::gateway::GatewayEvent,
    session: &crate::gateway::SessionResolve,
    paths: &CcteamPaths,
    seq: u64,
) -> std::result::Result<(), String> {
    use ccteam_harness::execution::turns_mirror::{append_turn, TurnRecord};

    let host = project_host(paths, &session.project);
    if host != ccteam_core::LOCAL_HOST {
        return Err(format!(
            "project `{}` runs on remote host `{host}` — attachments are not yet supported for remote projects",
            session.project
        ));
    }

    let upload_dir = crate::transport::project_uploads_dir(&session.project_dir);
    std::fs::create_dir_all(&upload_dir)
        .map_err(|err| format!("chat_send_file: create uploads dir: {err}"))?;
    let millis = chrono::Utc::now().timestamp_millis();
    let mut staged_paths = Vec::new();
    let mut references = Vec::with_capacity(event.attachments.len());

    for (index, attachment) in event.attachments.iter_mut().enumerate() {
        let source = std::path::Path::new(&attachment.path);
        let original_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        let (staged, _) =
            crate::transport::next_project_upload_path(&session.project_dir, original_name, millis);
        let staged_name = staged
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "chat_send_file: staged upload name is not valid UTF-8".to_string())?
            .to_string();
        let tmp = staged.with_file_name(format!(
            ".{staged_name}.{}.{}.part",
            std::process::id(),
            seq.saturating_add(index as u64)
        ));
        let copied = match std::fs::copy(source, &tmp) {
            Ok(size) => size,
            Err(err) => {
                for path in staged_paths {
                    let _ = std::fs::remove_file(path);
                }
                return Err(format!("chat_send_file: stage web attachment: {err}"));
            }
        };
        if copied > outbound_max_bytes(attachment.kind) {
            let _ = std::fs::remove_file(&tmp);
            for path in staged_paths {
                let _ = std::fs::remove_file(path);
            }
            return Err(format!(
                "chat_send_file: file grew beyond the {:?} limit while staging",
                attachment.kind
            ));
        }
        if let Err(err) = std::fs::rename(&tmp, &staged) {
            let _ = std::fs::remove_file(&tmp);
            for path in staged_paths {
                let _ = std::fs::remove_file(path);
            }
            return Err(format!("chat_send_file: commit web attachment: {err}"));
        }

        attachment.id = staged_name.clone();
        attachment.size = copied;
        references.push(
            attachment.attachment_ref().map_err(|err| {
                format!("chat_send_file: build staged attachment reference: {err}")
            })?,
        );
        staged_paths.push(staged);
    }

    event.sid = Some(session.sid.clone());
    event.slug = Some(session.project.clone());
    if event.content.is_empty() {
        event.content = event
            .attachments
            .first()
            .and_then(|attachment| attachment.caption.clone())
            .unwrap_or_default();
    }
    let record = TurnRecord {
        turn_id: event.id.clone(),
        ts: chrono::Utc::now(),
        vendor: session.vendor.clone(),
        role: session.role.clone(),
        user: String::new(),
        assistant: event.content.clone(),
        usage: serde_json::Value::Null,
        status: None,
        tool_calls: Vec::new(),
        attachments: references,
        outcome: None,
        error_kind: None,
        error: None,
    };
    if let Err(err) = append_turn(&session.project_dir, &session.sid, &record) {
        for path in staged_paths {
            let _ = std::fs::remove_file(path);
        }
        return Err(format!(
            "chat_send_file: persist web attachment reference: {err}"
        ));
    }
    Ok(())
}

/// Pure core of `run_chat_send_file`: parse args, validate the file
/// (exists + within the send ceiling), and build the `GatewayEvent`
/// addressed to the firing session's `live_target` — the SINGLE source of
/// truth for session→chat addressing. Only I/O is the file stat, so it is
/// unit-testable.
fn build_send_file_event(
    args: &serde_json::Value,
    seq: u64,
    live_target: Option<(String, String)>,
) -> std::result::Result<crate::gateway::GatewayEvent, String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat_send_file: missing `path`".to_string())?;
    let caption = args
        .get("caption")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let kind = parse_outbound_kind(args.get("kind").and_then(|v| v.as_str()), path);

    let meta =
        std::fs::metadata(path).map_err(|_| format!("chat_send_file: file not found: {path}"))?;
    let max = outbound_max_bytes(kind);
    if meta.len() > max {
        return Err(format!(
            "chat_send_file: file too large ({} MB) for {:?} (limit {} MB)",
            meta.len() / (1024 * 1024),
            kind,
            max / (1024 * 1024),
        ));
    }
    // v0.8.8 — single source of truth: the firing session's live reply target
    // (its `owner` ChatKey, set at spawn, keyed by sid via `reply_target_for`).
    // NO registry fallback — the two-store addressing is gone; a missing
    // binding is a spawn/bind-flow defect, surfaced precisely, not papered over.
    let sid = args
        .get("_caller_sid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let (channel, chat_id) = live_target.ok_or_else(|| {
        format!(
            "chat_send_file: no IM chat bound to firing session sid={sid:?} ({slug}/{role}); owner unset at spawn/bind"
        )
    })?;
    Ok(crate::gateway::GatewayEvent {
        id: format!("chat-send-file-{slug}-{role}-{seq}"),
        channel,
        chat_id,
        thread_ts: None,
        content: String::new(),
        kind: crate::gateway::GatewayEventKind::Answer,
        attachments: vec![crate::transport::OutboundFile {
            id: String::new(),
            size: 0,
            path: path.to_string(),
            caption,
            kind,
        }],
        options: Vec::new(),
        status: None,
        // Web staging replaces this with the server-resolved caller sid so
        // the current per-session SSE can render the reference live. IM-only
        // delivery keeps the historical `None`.
        sid: None,
        slug: if slug.is_empty() {
            None
        } else {
            Some(slug.to_string())
        },
    })
}

/// `kind` arg → [`OutboundFileKind`], inferring photo from common image
/// extensions when omitted.
fn parse_outbound_kind(kind: Option<&str>, path: &str) -> crate::transport::OutboundFileKind {
    use crate::transport::OutboundFileKind;
    match kind {
        Some("photo") => OutboundFileKind::Photo,
        Some("document") => OutboundFileKind::Document,
        _ => {
            let lower = path.to_lowercase();
            let is_image = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
                .iter()
                .any(|ext| lower.ends_with(ext));
            if is_image {
                OutboundFileKind::Photo
            } else {
                OutboundFileKind::Document
            }
        }
    }
}

/// v0.8.5 D6 — true when this line is the AskUserQuestion hook's
/// `interaction/ask` RPC (raw JSON-RPC `method`, not a `tools/call`).
fn is_interaction_ask_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("interaction/ask")
}

/// v0.8.5 D6 — handle one `interaction/ask` request from the AskUserQuestion
/// chat hook. Mint a token, build a [`ChoicePrompt`], resolve the bot's home
/// chat, register an External-origin pending in the SHARED registry, emit a
/// `GatewayEvent` so IM renders buttons, then block (with a TTL, holding NO
/// lock) on the user's selection.
///
/// Request:  `{"jsonrpc":"2.0","id":N,"method":"interaction/ask",
///             "params":{"slug","role","question","options":[..],"multi"}}`
/// Response: `{"result":{"answers":{<question>:<label>}}}` on a pick,
///           `{"result":{"timeout":true}}` on TTL lapse, or a JSON-RPC
///           `error` when addressing / wiring is unavailable (the hook then
///           degrades to deny-with-reason).
async fn execute_interaction_ask(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    pending: Option<&PendingRegistry>,
    gateway: Option<&GatewayHandle>,
) -> serde_json::Value {
    use crate::gateway::{GatewayEvent, GatewayEventKind};
    use crate::pending::InteractionOrigin;
    use crate::transport::MessageOption;
    use ccteam_harness::{ChoiceOption, ChoicePrompt, ChoiceSelection};

    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let err_resp = |msg: String| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": { "code": -32000, "message": msg },
        })
    };

    let (Some(sink), Some(pending)) = (sink, pending) else {
        return err_resp("interaction/ask: IM gateway not running".to_string());
    };
    let params = req
        .pointer("/params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let question = params
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let multi = params
        .get("multi")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let options: Vec<String> = params
        .get("options")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| o.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if question.is_empty() || options.is_empty() {
        return err_resp("interaction/ask: empty question or options".to_string());
    }

    // Resolve addressing first — no point registering a pending we can't show.
    // v0.8.8 — single source of truth: the live firing session's reply target
    // (its `owner` ChatKey, set at spawn, keyed by sid via `reply_target_for`;
    // resolve under the gateway lock, drop the guard before the long await —
    // the lookup is sync). NO registry fallback — the two-store addressing is
    // gone; a missing binding (empty/unpropagated sid, or no live session for
    // it) is a spawn/bind-flow defect, surfaced precisely.
    let session_sid = params
        .get("session_sid")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let live_target = match gateway {
        Some(gw) if !session_sid.is_empty() => {
            let guard = gw.lock().await;
            guard.reply_target_for(session_sid)
        }
        _ => None,
    };
    let Some((channel, chat_id)) = live_target else {
        return err_resp(format!(
            "interaction/ask: no IM chat bound to firing session sid={session_sid:?} ({slug}/{role}); owner unset at spawn/bind — not falling back to the registry"
        ));
    };

    // Mint a short token (≤16B ASCII, no `:` — the ChoicePrompt contract).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("h{:x}", (nanos as u64) & 0xff_ffff_ffff);

    let prompt = ChoicePrompt {
        token: token.clone(),
        title: question.clone(),
        options: options
            .iter()
            .map(|o| ChoiceOption {
                id: o.clone(),
                label: o.clone(),
            })
            .collect(),
        multi,
    };
    let message_options: Vec<MessageOption> = prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{token}:{i}"),
            label: opt.label.clone(),
            // v0.8.7 review-fix (R-H1) — carry the stable option id (e.g.
            // "allow"/"deny") so the web SSE consumer can resolve by
            // {token, selection=id} through the same pending machinery.
            id: opt.id.clone(),
        })
        .collect();

    // Register the External-origin pending under the SHARED registry, keyed by
    // the token itself (the gateway resolves token-globally via take_by_token).
    // Release the guard BEFORE the long await (lock discipline §7-1).
    let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
    let ttl = std::time::Duration::from_secs(INTERACTION_ASK_TIMEOUT_SECS);
    {
        let mut guard = pending.lock().await;
        guard.register(
            token.clone(),
            prompt.clone(),
            InteractionOrigin::External { reply: tx },
            std::time::Instant::now() + ttl,
        );
    }

    // Render the buttons in IM.
    if sink
        .send(GatewayEvent {
            id: format!("interaction-{token}"),
            channel,
            chat_id,
            thread_ts: None,
            content: question.clone(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: message_options,
            status: None,
            // The D6 `interaction/ask` hook prompt has no gateway session.
            sid: None,
            slug: if slug.is_empty() {
                None
            } else {
                Some(slug.to_string())
            },
        })
        .is_err()
    {
        // Sink closed: forget the pending so it can't leak.
        pending.lock().await.take_by_token(&token);
        return err_resp("interaction/ask: gateway sink closed".to_string());
    }

    // Block on the selection, holding NO lock. The daemon enforces the TTL.
    match tokio::time::timeout(ttl, rx).await {
        Ok(Ok(selection)) => {
            // Map the resolved real id(s) back to label(s) for the hook echo.
            let label = prompt
                .options
                .iter()
                .find(|o| selection.ids.first() == Some(&o.id))
                .map(|o| o.label.clone())
                .or_else(|| selection.ids.first().cloned())
                .or_else(|| selection.free_text.clone())
                .unwrap_or_default();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "answers": { question: label } },
            })
        }
        _ => {
            // Timeout or sender dropped — forget the pending (best-effort).
            pending.lock().await.take_by_token(&token);
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "timeout": true },
            })
        }
    }
}

/// v0.8.7 W2 (DB.3) — true when this line is the HITL `PermissionRequest`
/// hook's `permission/ask` RPC (raw JSON-RPC `method`, not a `tools/call`).
fn is_permission_ask_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("permission/ask")
}

/// In-place IM hot-reload — `ccteam config` sends `{"method":"ccteam/reload"}`
/// over the daemon's mcp.sock after persisting `credentials.json`. The handler
/// signals the gateway's daemon reload task to rebuild the credential-driven
/// IM channel listeners without restarting any agent session or the daemon.
fn is_reload_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("ccteam/reload")
}

/// v0.8.7 W2 (DB.3/DB.4) — handle one `permission/ask` request from a HITL
/// session's `PermissionRequest` hook. Builds a 2-option (Approve / Deny)
/// [`ChoicePrompt`], renders it to the bound IM chat as clickable buttons,
/// and BLOCKS (with a TTL, holding NO lock) on the user's click — the exact
/// blocking-External-pending mechanism as [`execute_interaction_ask`].
///
/// Request:  `{"jsonrpc":"2.0","id":N,"method":"permission/ask",
///             "params":{"slug","role","tool_name","tool_input",
///                       "session_id","cwd"}}`
/// Response: `{"result":{"behavior":"allow"|"deny"}}` on a click,
///           `{"result":{"timeout":true}}` on TTL lapse (hook → deny), or a
///           JSON-RPC `error` when addressing / wiring is unavailable (the
///           hook then fail-safe denies).
async fn execute_permission_ask(
    req: &serde_json::Value,
    sink: Option<&GatewayEventSink>,
    pending: Option<&PendingRegistry>,
    gateway: Option<&GatewayHandle>,
) -> serde_json::Value {
    use crate::gateway::{GatewayEvent, GatewayEventKind};
    use crate::pending::InteractionOrigin;
    use crate::transport::MessageOption;
    use ccteam_harness::{ChoiceOption, ChoicePrompt, ChoiceSelection};

    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let err_resp = |msg: String| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": { "code": -32000, "message": msg },
        })
    };

    let (Some(sink), Some(pending)) = (sink, pending) else {
        return err_resp("permission/ask: IM gateway not running".to_string());
    };
    let params = req
        .pointer("/params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let tool_name = params
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if tool_name.is_empty() {
        return err_resp("permission/ask: missing tool_name".to_string());
    }
    let tool_input = params
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // v0.8.8 F1 — the firing session's own ccteam sid (`s<N>`), reported by the
    // hook via `session_sid` (sourced from `CCTEAM_CHAT_SID` / `X-Ccteam-Sid`).
    // 红线:ccteam 的 `s<N>` sid,不是 Anthropic 的 `session_id` UUID。
    let session_sid = params
        .get("session_sid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    // Single source of truth for the approval-prompt destination: the firing
    // session's live reply target (its `owner` ChatKey, set at spawn, keyed by
    // sid via `reply_target_for`). Resolve it AND the canonical sid label in one
    // read-only gateway lock dropped before the long await (lock discipline
    // §7-1). NO registry fallback — the two-store addressing is gone; a missing
    // binding is a spawn/bind-flow defect, surfaced precisely.
    let (dest, sid_label) = match (gateway, session_sid.is_empty()) {
        (Some(gw), false) => {
            let guard = gw.lock().await;
            (
                guard.reply_target_for(session_sid),
                guard.session_sid_for(session_sid),
            )
        }
        _ => (None, None),
    };
    let Some((channel, chat_id)) = dest else {
        return err_resp(format!(
            "permission/ask: no IM chat bound to firing session sid={session_sid:?} ({slug}/{role}); owner unset at spawn/bind — not falling back to the registry"
        ));
    };
    let session_desc = match (&sid_label, role.is_empty()) {
        (Some(sid), false) => format!("session {sid} ({role})"),
        (Some(sid), true) => format!("session {sid}"),
        (None, false) => format!("session ({role})"),
        (None, true) => "session".to_string(),
    };

    let summary = summarize_tool_input(&tool_name, &tool_input);
    let risk = crate::hitl::classify_tool_risk(&tool_name, &tool_input);
    let title = format!(
        "{badge} {session_desc} wants to run: {summary}",
        badge = crate::hitl::risk_badge(risk),
    );

    // Mint a short token (≤16B ASCII, no `:` — the ChoicePrompt contract).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("p{:x}", (nanos as u64) & 0xff_ffff_ffff);

    // The option `id`s are the decision wire values the hook maps to a
    // PermissionRequest behavior; the labels are the human-clickable text.
    let prompt = ChoicePrompt {
        token: token.clone(),
        title: title.clone(),
        options: vec![
            ChoiceOption {
                id: "allow".to_string(),
                label: "✅ Approve".to_string(),
            },
            ChoiceOption {
                id: "deny".to_string(),
                label: "⛔ Deny".to_string(),
            },
        ],
        multi: false,
    };
    let message_options: Vec<MessageOption> = prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{token}:{i}"),
            label: opt.label.clone(),
            // v0.8.7 review-fix (R-H1) — carry the stable option id (e.g.
            // "allow"/"deny") so the web SSE consumer can resolve by
            // {token, selection=id} through the same pending machinery.
            id: opt.id.clone(),
        })
        .collect();

    // Register the External-origin pending (token-keyed); release the guard
    // BEFORE the long await (lock discipline §7-1).
    // v0.8.7 review-fix (R-L1) — a permission prompt blocks the whole turn, so
    // it uses a SHORTER TTL than the 600s interaction/ask; fail-safe stays deny.
    let (tx, rx) = tokio::sync::oneshot::channel::<ChoiceSelection>();
    let ttl_secs = permission_prompt_timeout_secs();
    let ttl = std::time::Duration::from_secs(ttl_secs);
    {
        let mut guard = pending.lock().await;
        guard.register(
            token.clone(),
            prompt.clone(),
            InteractionOrigin::External { reply: tx },
            std::time::Instant::now() + ttl,
        );
        // v0.8.22 P1 (review §3.1-3) — tag this pending with its sid so a web
        // SSE reconnect (or a brand-new tab) can re-seed it, same as the
        // stream-json protocol's `ask_permission` does.
        if let Some(sid) = sid_label.as_deref() {
            guard.tag_sid(&token, sid.to_string());
        }
    }

    // Render the approve/deny buttons in IM.
    if sink
        .send(GatewayEvent {
            id: format!("permission-{token}"),
            channel,
            chat_id,
            thread_ts: None,
            content: title,
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: message_options,
            status: None,
            // sid set so a per-session web UI stream can show the approval
            // (None would route to IM fine but be filtered out of SSE).
            sid: sid_label.clone(),
            slug: if slug.is_empty() {
                None
            } else {
                Some(slug.to_string())
            },
        })
        .is_err()
    {
        pending.lock().await.take_by_token(&token);
        return err_resp("permission/ask: gateway sink closed".to_string());
    }

    // v0.8.7 review-fix (R-L1) — emit a progress.jsonl line so an operator
    // (`ccteam status` / dashboard / `progress`) sees the session is PARKED
    // awaiting approval, not silently stuck. Best-effort: a write failure must
    // never block the approval flow. progress.jsonl stays the state SoT (we
    // only append; nothing here mutates session state).
    emit_permission_prompt_outstanding(slug, role, &tool_name, &summary, ttl_secs);

    // Block on the click, holding NO lock. The daemon enforces the TTL; on
    // lapse the hook degrades to deny.
    match tokio::time::timeout(ttl, rx).await {
        Ok(Ok(selection)) => {
            // The resolved id IS the decision ("allow" / "deny").
            let behavior = match selection.ids.first().map(String::as_str) {
                Some("allow") => "allow",
                _ => "deny",
            };
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "behavior": behavior },
            })
        }
        _ => {
            pending.lock().await.take_by_token(&token);
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "timeout": true },
            })
        }
    }
}

/// v0.8.7 review-fix (R-L1) — append a `chat_permission_prompt_outstanding`
/// line to the project's `progress.jsonl` so an operator sees the session is
/// parked awaiting approval. Best-effort: resolve the path from env (the
/// daemon process has `CCTEAM_HOME`); any failure (no env, write error) is
/// logged and swallowed — the approval flow must never depend on this signal.
/// progress.jsonl stays the state SoT (append-only; nothing here mutates
/// session state). A blank `slug` (the hook didn't pass one) is skipped.
fn emit_permission_prompt_outstanding(
    slug: &str,
    role: &str,
    tool_name: &str,
    summary: &str,
    ttl_secs: u64,
) {
    if slug.is_empty() {
        return;
    }
    let Ok(paths) = ccteam_core::CcteamPaths::from_env() else {
        return;
    };
    let event = ccteam_core::progress::build_chat_permission_prompt_outstanding_event(
        role, tool_name, summary, ttl_secs,
    );
    if let Err(e) = ccteam_core::progress::append_event(&paths.progress_jsonl(slug), &event) {
        tracing::warn!(%slug, %role, %e, "failed to append permission-prompt-outstanding progress line");
    }
}

/// v0.8.7 W2 (DB.4) — render a short, human-readable one-liner of a tool
/// call for the approval prompt. Picks the most useful field per common tool
/// (`Bash` → command, file tools → path) and truncates so the IM message
/// stays compact. Falls back to the tool name when no obvious field exists.
///
/// v0.8.22 P0-2 — delegates to [`crate::hitl::summarize_tool_input`] (the
/// SAME renderer the stream-json protocol's in-process HITL resolver uses),
/// so an approval prompt reads identically regardless of which protocol
/// produced it.
fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String {
    crate::hitl::summarize_tool_input(tool_name, tool_input)
}

// =====================================================================
// v0.9.0 W1 (F1) — session scheduling: daemon-side `session_*` tool handlers.
//
// The stdio MCP server (or HTTP `/mcp` session bearer) forwards
// `session_*` calls here (it doesn't own the gateway). This is where
// we (a) authenticate the caller by its `(sid, secret)` PRINCIPAL — any live
// session that holds the secret, role-agnostic; the retired cto-only gate is
// gone — and (b) drive the gateway session map (spawn / dispatch / list /
// stop) or tail a child's transcript (collect). The project scope is the
// SERVER's view of the caller's session (`CallerCtx.slug`), never the
// caller-supplied `_caller_slug`.
//
// Lock discipline (CLAUDE.md §6): spawn/dispatch/list/stop call the
// gateway's own async methods, so we hold the gateway lock across their
// `.await` (the gateway IS the lock target — the same pattern ccteam-web's
// AppState uses over HTTP). `collect` only needs a synchronous
// `session_resolve`, so we copy out the role + project_dir and DROP the
// guard BEFORE the (blocking) `read_all_turns` fs read.
// =====================================================================

/// True for a `tools/call` whose tool name is in the `session_` group.
fn is_session_tool_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req
            .pointer("/params/name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n.starts_with("session_"))
}

/// True for a `tools/call` whose tool name is `status` or its bare-name
/// beacon alias (a pure alias: same handler, same response).
fn is_status_call(req: &serde_json::Value) -> bool {
    if req.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
        return false;
    }
    matches!(
        req.pointer("/params/name").and_then(|n| n.as_str()),
        Some("status") | Some(protocol::STATUS_BEACON_TOOL_NAME)
    )
}

/// v0.10 T1 — daemon-aware `status`: return the base status JSON with the
/// vendor panel + routing notes appended for the caller's project's bound
/// host. Ambient (session principal, which is every `POST /mcp` caller) is
/// scoped to its OWN project — any self-reported `project`/`_caller_slug` is
/// ignored (the panel would otherwise leak another project's host). Admin (the
/// local mcp.sock admin-token tier) may name a `project`, else falls back to a
/// supplied `_caller_slug` (like `session_spawn`; nothing ccteam ships injects
/// one now that the stdio forwarder is gone). The vendor panel probes + reads
/// fs, so it runs off the async runtime.
async fn execute_status(
    req: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
    caller: McpCaller,
    paths: &CcteamPaths,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if let Some(user_id) = caller.user_id() {
        return execute_user_status(id, &args, paths, user_id).await;
    }

    // Base body (slim projects + daemon health), reused verbatim
    // from the protocol core so the daemon-aware path never drifts from the
    // local fallback.
    let base = super::protocol::tool_ls(paths).unwrap_or_else(|_| "{}".to_string());

    // Resolve the caller's project scope (server-side; never trust a
    // self-reported project on the Ambient path).
    let ctx = if caller == McpCaller::Ambient {
        let sid = args
            .get("_caller_sid")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let secret = args
            .get("_caller_secret")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match gateway {
            Some(gw) => gw.lock().await.verify_session_principal(sid, secret),
            None => None,
        }
    } else {
        None
    };
    let project = match super::vendor_panel::resolve_status_project(
        caller,
        args.get("project").and_then(|v| v.as_str()),
        args.get("_caller_slug").and_then(|v| v.as_str()),
        ctx.as_ref(),
    ) {
        Ok(p) => p,
        Err(msg) => {
            // Ambient caller not authenticated: base status + honest note, NO
            // panel (we won't render another project's host).
            return session_tool_response(id, format!("{base}\n\n{msg}"), false);
        }
    };

    let hub_models = crate::hub::load_models_catalog(&crate::hub::hub_base(), paths, false).await;
    let paths_owned = paths.clone();
    let slug_owned = project.clone();
    let section = tokio::task::spawn_blocking(move || {
        super::vendor_panel::render_section(&paths_owned, slug_owned.as_deref(), &hub_models)
    })
    .await
    .unwrap_or_else(|_| "vendors: panel unavailable (probe worker failed)".to_string());

    session_tool_response(id, format!("{base}\n\n{section}"), false)
}

async fn execute_user_status(
    id: serde_json::Value,
    args: &serde_json::Value,
    paths: &CcteamPaths,
    user_id: &str,
) -> serde_json::Value {
    let explicit = args
        .get("project")
        .and_then(|project| project.as_str())
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .map(str::to_string);
    if let Some(project) = explicit.as_deref() {
        if !user_can_see_project(paths, user_id, project) {
            return session_tool_response(id, "status: project not found".to_string(), true);
        }
    }

    let base =
        super::protocol::tool_ls_for_user(paths, user_id).unwrap_or_else(|_| "{}".to_string());
    let projects = explicit
        .map(|project| vec![project])
        .unwrap_or_else(|| visible_user_projects(paths, user_id));
    if projects.is_empty() {
        return session_tool_response(
            id,
            format!(
                "{base}\n\nvendors: no projects are visible to this user; project-scoped host, budget, and routing details are withheld"
            ),
            false,
        );
    }

    let hub_models = crate::hub::load_models_catalog(&crate::hub::hub_base(), paths, false).await;
    let paths_owned = paths.clone();
    let sections = tokio::task::spawn_blocking(move || {
        projects
            .iter()
            .map(|project| {
                super::vendor_panel::render_section(
                    &paths_owned,
                    Some(project.as_str()),
                    &hub_models,
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    })
    .await
    .unwrap_or_else(|_| "vendors: panel unavailable (probe worker failed)".to_string());

    session_tool_response(id, format!("{base}\n\n{sections}"), false)
}

/// Build a tools/call-shaped JSON-RPC response carrying one text block.
fn session_tool_response(id: serde_json::Value, text: String, is_error: bool) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        },
    })
}

/// v0.9.0 W1 (F1) — handle one forwarded `session_*` call. Authenticates
/// the caller by its `(sid, secret)` PRINCIPAL (Ambient path), then dispatches
/// to the gateway. Returns a JSON-RPC response (the caller side propagates
/// `isError` to the agent).
async fn execute_session_tool_with_paths(
    req: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
    caller: McpCaller,
    paths: &CcteamPaths,
) -> serde_json::Value {
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let name = req
        .pointer("/params/name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let mut args = req
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // The gateway must be running (web + IM both up). Mirror chat_send_file's
    // "gateway not running" structured error rather than panicking. It is also
    // REQUIRED to authenticate the caller (the secret map lives there), so a
    // missing gateway is a HARD STOP for every session_* call — fail-closed,
    // never a fall-through that would skip the principal check.
    let Some(gateway) = gateway else {
        return session_tool_response(
            id,
            format!("{name}: gateway not running (start ccteam with web + IM enabled)"),
            true,
        );
    };

    // Authenticate the Ambient caller by its `(sid, secret)` PRINCIPAL and
    // resolve its CallerCtx (server-side sid + slug + role). This is the sole
    // security-relevant check (best-effort defense-in-depth; single-uid honest
    // scope in `verify_session_principal`). Role plays NO part — the retired
    // cto-only pre-filter is gone. A missing/wrong secret or unknown sid fails
    // closed. We then OVERWRITE the identity args from CallerCtx so nothing
    // downstream trusts a caller-supplied `_caller_slug`/`_caller_sid`/role.
    //
    // `McpCaller::Admin` (the local mcp.sock caller whose admin web token was
    // already verified against the 0600 file) skips the principal gate: it names
    // its target with an explicit `project` arg (fleet-wide, same as the web
    // admin Identity). No HTTP caller arrives on this arm — `POST /mcp` has no
    // admin tier.
    match &caller {
        McpCaller::Ambient => {
            let caller_sid = args
                .get("_caller_sid")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let caller_secret = args
                .get("_caller_secret")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let ctx = {
                let gw = gateway.lock().await;
                gw.verify_session_principal(caller_sid, caller_secret)
            };
            let Some(ctx) = ctx else {
                return session_tool_response(
                    id,
                    format!(
                        "{name}: permission denied — caller could not be authenticated (no live session holds the presented (sid, secret) principal)"
                    ),
                    true,
                );
            };
            if let Some(obj) = args.as_object_mut() {
                obj.insert("_caller_slug".to_string(), serde_json::json!(ctx.slug));
                obj.insert("_caller_sid".to_string(), serde_json::json!(ctx.sid));
                obj.insert("_caller_role".to_string(), serde_json::json!(ctx.role));
                // v0.9.0 W2 (F2/F5) — the caller's delegation depth
                // (server-resolved from CallerCtx, never caller-supplied).
                obj.insert("_caller_depth".to_string(), serde_json::json!(ctx.depth));
            }
        }
        McpCaller::User { user_id } => {
            if let Some(obj) = args.as_object_mut() {
                strip_caller_args(obj);
            }
            if let Err(message) =
                authorize_user_session_tool(&name, &mut args, gateway, paths, user_id).await
            {
                return session_tool_response(id, message, true);
            }
        }
        McpCaller::Admin => {}
    }

    // v0.9.5 feedback fix — every session_* call carries a hard server-side
    // deadline so a busy daemon (lock contention, a slow spawn/submit) returns
    // a READABLE error instead of hanging the caller's whole turn on a
    // never-resolving tool call. spawn/dispatch budget for process startup +
    // any explicit inline wait; the read-only tools are short.
    let requested_wait_secs = requested_inline_wait_seconds(&args);
    let effective_wait_secs = effective_inline_wait_seconds(requested_wait_secs);
    let budget = std::time::Duration::from_secs(match name.as_str() {
        "session_spawn" | "session_dispatch" => 60 + effective_wait_secs,
        "session_stop" => 30,
        _ => 15,
    });
    match tokio::time::timeout(
        budget,
        run_session_tool(&name, &args, gateway, caller, paths),
    )
    .await
    {
        Ok(Ok(text)) => session_tool_response(id, text, false),
        Ok(Err(text)) => session_tool_response(id, text, true),
        Err(_) => session_tool_response(
            id,
            format!(
                "{name}: timed out after {}s — the daemon is busy (lock contention or a slow \
                 spawn/submit); the operation may still complete in the background. Retry, and \
                 check session_list / session_collect before assuming it failed.",
                budget.as_secs()
            ),
            true,
        ),
    }
}

#[cfg(test)]
async fn execute_session_tool(
    req: &serde_json::Value,
    gateway: Option<&GatewayHandle>,
    caller: McpCaller,
) -> serde_json::Value {
    let paths = CcteamPaths {
        root: std::path::PathBuf::new(),
        projects_root: std::path::PathBuf::new(),
    };
    execute_session_tool_with_paths(req, gateway, caller, &paths).await
}

async fn authorize_user_session_tool(
    name: &str,
    args: &mut serde_json::Value,
    gateway: &GatewayHandle,
    paths: &CcteamPaths,
    user_id: &str,
) -> std::result::Result<(), String> {
    match name {
        "session_spawn" => {
            // The hint enumerates the caller's OWN projects only (a pure
            // function of identity, not of the probed input): actionable
            // recovery without existence disclosure.
            let hint = || user_project_list_hint(&visible_user_projects(paths, user_id));
            let explicit = args
                .get("project")
                .and_then(|project| project.as_str())
                .map(str::trim)
                .filter(|project| !project.is_empty())
                .map(str::to_string);
            let project = match explicit {
                Some(project) => {
                    args["_caller_project_source"] = serde_json::json!("explicit");
                    project
                }
                // MCP-DX-2 — exactly one visible project is an unambiguous
                // default (identity-derived, same disclosure surface as the
                // hint); two or more keep the explicit-or-error contract.
                None => match visible_user_projects(paths, user_id).as_slice() {
                    [only] => {
                        args["project"] = serde_json::json!(only);
                        args["_caller_project_source"] = serde_json::json!("sole");
                        only.clone()
                    }
                    _ => {
                        return Err(format!(
                            "session_spawn: missing `project` — tenant MCP callers must name one of their own projects explicitly{}",
                            hint()
                        ));
                    }
                },
            };
            if !user_can_see_project(paths, user_id, &project) {
                return Err(format!("session_spawn: project not found{}", hint()));
            }
        }
        "session_dispatch" | "session_collect" | "session_stop" => {
            let sid = args
                .get("sid")
                .and_then(|sid| sid.as_str())
                .filter(|sid| !sid.is_empty())
                .ok_or_else(|| format!("{name}: missing required `sid`"))?;
            let project = {
                let gateway = gateway.lock().await;
                gateway
                    .session_resolve(sid)
                    .map(|resolved| resolved.project)
                    // An enrolled hand-started client is a ledger row without a
                    // live-map row, so the live map alone cannot answer "whose
                    // project is this sid in". This gate decides VISIBILITY only
                    // — a tenant who can see the node in `session_list` must
                    // reach the honest not-driveable refusal instead of
                    // "session not found", while a node in someone else's
                    // project stays indistinguishable from an unknown sid.
                    .or_else(|| gateway.external_node(sid).map(|meta| meta.slug))
            };
            if project
                .as_deref()
                .is_none_or(|project| !user_can_see_project(paths, user_id, project))
            {
                return Err(format!("{name}: session not found"));
            }
        }
        "session_list" => {
            let visible = visible_user_projects(paths, user_id);
            if let Some(project) = args
                .get("project")
                .and_then(|project| project.as_str())
                .map(str::trim)
                .filter(|project| !project.is_empty())
            {
                if !visible.iter().any(|candidate| candidate == project) {
                    return Err(format!(
                        "session_list: project not found{}",
                        user_project_list_hint(&visible)
                    ));
                }
            }
            if let Some(obj) = args.as_object_mut() {
                obj.insert(
                    "_caller_visible_projects".to_string(),
                    serde_json::json!(visible),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

/// Dispatch a privileged `session_*` call to the gateway. Returns `Ok(body)`
/// (a pretty JSON string) on success, `Err(msg)` on a tool-level error.
async fn run_session_tool(
    name: &str,
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
    paths: &CcteamPaths,
) -> std::result::Result<String, String> {
    match name {
        "session_spawn" => run_session_spawn_at(args, gateway, caller, Some(paths)).await,
        "session_dispatch" => run_session_dispatch(args, gateway, caller).await,
        "session_collect" => run_session_collect(args, gateway, caller).await,
        "session_list" => run_session_list_at(args, gateway, Some(paths)).await,
        "session_stop" => run_session_stop(args, gateway, caller).await,
        other => Err(format!("unknown session tool: {other}")),
    }
}

/// `session_spawn` — create a session in the caller's own project and return
/// its `s{n}` id + vendor resume key + host. v0.9.0 W1 (F1/G1): the caller is
/// authenticated by its `(sid, secret)` PRINCIPAL (see [`execute_session_tool`]),
/// so `_caller_slug` here is the SERVER's view of the caller's project — an
/// Ambient caller can only spawn into that project. Admin (the local mcp.sock
/// admin-token tier) names the target with an explicit `project` (fleet-wide).
///
/// MCP facets are `{role?, vendor?, model?, effort?, permission_mode?, title?}`.
/// `role` empty/absent = roleless (bare vendor reads the project
/// CLAUDE.md/AGENTS.md). `title` is metadata/ledger only — NEVER concatenated
/// into any prompt.
/// **Which session is this call coming FROM** — the one answer every
/// lineage-carrying feature reads (delegation parent, depth guardrails, the
/// dispatcher stamped on a first task, the `caller_sid` echo).
///
/// Deliberately separate from the authentication TIER. The two were conflated:
/// `McpCaller::Admin => None` read "authenticated as admin" as "is not a
/// session", so a plain local agent that ccteam already mirrors in the ledger
/// spawned children that mounted as ROOTS — the topology lost an edge that
/// exists. Each lineage feature derived itself from the tier independently,
/// which is why fixing one of them would not have fixed the others.
///
/// Sources, strongest first:
///
/// 1. **Verified principal** (`Ambient`) — cryptographic, server-resolved. This
///    is how a hand-started client gets its edge now: it enrolls, the daemon
///    mints it a ledger node at `initialize`, and it calls as that node.
/// 2. **Declared and validated** (`Admin`) — the local mcp.sock admin-token
///    caller holds no per-session principal and carries no process context to
///    infer one from, so it may NAME its own sid. Same-uid is already this
///    path's trust boundary (an admin caller can spawn and stop anything), so
///    declaring a parent adds no authority — but it is checked against the
///    ledger, never taken on faith: an unknown sid is a loud error rather than
///    a silent root.
/// 3. **Never** for a tenant (`User`): their `_caller_*` args are stripped
///    upstream, and a declaration must not smuggle identity back in.
async fn resolve_call_origin(
    caller: &McpCaller,
    args: &Value,
    gateway: Option<&std::sync::Arc<tokio::sync::Mutex<crate::gateway::Gateway>>>,
    deadline: Option<crate::gateway::GatewayDeadline>,
) -> Result<Option<crate::gateway::DelegationParent>, String> {
    match caller {
        McpCaller::Ambient => {
            let caller_sid = args
                .get("_caller_sid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if caller_sid.is_empty() {
                return Ok(None);
            }
            Ok(Some(crate::gateway::DelegationParent {
                sid: caller_sid,
                depth: args
                    .get("_caller_depth")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
                role: args
                    .get("_caller_role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            }))
        }
        McpCaller::Admin => {
            let Some(declared) = args
                .get("parent_sid")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                // Nothing declared → a root spawn, as before: the admin front
                // door IS rootless when a human drives it.
                return Ok(None);
            };
            let Some(gateway) = gateway else {
                return Err(format!(
                    "session_spawn: parent_sid `{declared}` cannot be validated (no live gateway)"
                ));
            };
            let view = {
                let gw = match deadline {
                    Some(deadline) => deadline
                        .lock(gateway)
                        .await
                        .map_err(|error| mcp_gateway_error("session_spawn", &error))?,
                    None => crate::latency::gateway_lock(gateway, "mcp.spawn.resolve").await,
                };
                gw.session_views().into_iter().find(|v| v.sid == declared)
            };
            let Some(view) = view else {
                return Err(format!(
                    "session_spawn: parent_sid `{declared}` is not a live session — run session_list to find your own sid, or omit parent_sid for a root spawn"
                ));
            };
            Ok(Some(crate::gateway::DelegationParent {
                sid: view.sid,
                depth: view.delegation_depth,
                role: view.role,
            }))
        }
        McpCaller::User { .. } => Ok(None),
    }
}

async fn run_session_spawn_at(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
    paths: Option<&CcteamPaths>,
) -> std::result::Result<String, String> {
    let deadline = crate::gateway::GatewayDeadline::start();
    if args.get("host").is_some() {
        return Err(crate::remote_host::HOST_SPAWN_PARAM_REMOVED.to_string());
    }
    if args.get("protocol").is_some() {
        return Err(PROTOCOL_SPAWN_PARAM_REMOVED.to_string());
    }
    // Roleless is a first-class form; absent or "" both mean roleless.
    let role = args
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let vendor = parse_session_vendor(args)?;
    // Optional `permission_mode` (`skip` default / `hitl`).
    let permission_mode = ccteam_harness::PermissionMode::parse_opt(
        args.get("permission_mode").and_then(|v| v.as_str()),
    )
    .map_err(|e| format!("session_spawn: {e}"))?;
    let protocol = derive_session_protocol(vendor);
    // Optional model/effort (composer facets), forwarded to EVERY vendor
    // verbatim — the vendor owns the verdict on its own value set. Grok's
    // effort used to be zeroed right here, which handed the caller a 201 and
    // a live sid for a session that quietly ran at the default; a rejected
    // token is honest feedback, a swallowed one is not. Same contract as the
    // REST `spawn_tuning_from_form`.
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let effort = args
        .get("effort")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let tuning = crate::gateway::SpawnTuning {
        model,
        effort,
        mode,
    };
    // Optional `title` — metadata/ledger only, NEVER concatenated into any
    // prompt. Validate ≤80 chars; W1 accepts + echoes it (meta persistence
    // lands with the W2 delegation ledger).
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if let Some(t) = &title {
        let n = t.chars().count();
        if n > 80 {
            return Err(format!(
                "session_spawn: `title` too long ({n} chars; max 80)"
            ));
        }
    }
    // Resolve only after validating the request facets: falling through to
    // scratch is a side effect and malformed spawns must not create projects.
    let project_resolution = resolve_spawn_project(args, &caller, paths)?;
    let project = project_resolution.slug.clone();
    // v0.9.1 delegation-ergonomics — optional FIRST task: spawn+dispatch in one
    // call (the dominant flow; saves the second round-trip and closes the
    // crash window between a spawn and its first dispatch). Identical
    // semantics to session_dispatch{sid, task}: async by default with a
    // completion notification; `wait_seconds` blocks inline; `notify:false`
    // opts out. Cycle checks are moot for a fresh child; the spawn guardrails
    // (depth/children/delegated/budget) below already gate the delegation.
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    // v0.9.5 feedback fix — a title-less child renders as `title: null`
    // everywhere (session_list, team view, notifications). When the spawn
    // carries a first task, derive a short label from its first line. Ledger/
    // display only (task → label, never label → prompt; the injection red line
    // is untouched).
    let title = title.or_else(|| task.as_deref().map(derive_title_from_task));
    let requested_wait_seconds = requested_inline_wait_seconds(args);
    let effective_wait_seconds = effective_inline_wait_seconds(requested_wait_seconds);
    let notify = parse_notify_mode("session_spawn", args)?;
    // Operator/unowned projects retain the caller-derived pool. Tenant-owned
    // projects ignore this fallback in the gateway and make every
    // session_spawn inherit the tenant principal.
    let fallback_owner_id = match &caller {
        McpCaller::User { user_id } => user_id.clone(),
        McpCaller::Ambient | McpCaller::Admin => "web-api".to_string(),
    };
    // v0.9.0 W2 (F7) — optional idempotency key: a client retry with the same
    // key replays the original spawn (same sid) with zero side effects.
    let idem_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    // v0.9.0 W2 (F2/F5) — the delegation parent. Ambient = the caller's
    // server-resolved principal (sid/depth/role, injected in `execute_session_tool`
    // from CallerCtx — never caller-supplied). Admin (the local mcp.sock
    // admin-token tier) = a human/root spawn unless it declares a `parent_sid`.
    // Guardrails apply only when a real parent is present.
    let parent = resolve_call_origin(&caller, args, Some(gateway), Some(deadline)).await?;
    // The dispatcher identity for an optional first `task` (captured before
    // `parent` moves into the create call).
    let parent_sid_for_task = parent.as_ref().map(|p| p.sid.clone());
    // v0.9.2 — surface WHO spawned this child so a rootless spawn is
    // self-explanatory: an undeclared admin caller is a root spawn BY DESIGN, an
    // ambient caller is the delegation parent. This is the diagnostic for the
    // "my agent's children lost their parent edge" class of misconfiguration
    // (today: an agent whose vendor loaded the global config instead of its own,
    // so its calls ride an enrolled client's node rather than its principal).
    let caller_label = match (&caller, parent.as_ref()) {
        // Echo the RESOLVED origin, not just the tier: a caller that declared
        // itself can see what ccteam actually attributed the child to.
        (McpCaller::Admin, Some(p)) => format!("admin:{}", p.sid),
        (McpCaller::Admin, None) => "admin".to_string(),
        (McpCaller::Ambient, Some(p)) => format!("ambient:{}", p.sid),
        (McpCaller::Ambient, None) => "ambient".to_string(),
        (McpCaller::User { user_id }, _) => format!("user:{user_id}"),
    };

    // v0.10 T1 — availability discovery: before minting a sid, fail fast when
    // the vendor is not installed on the project's BOUND host, listing the
    // vendors that ARE installed there (from the same probe/report snapshot)
    // + freshness. A host-offline satellite is NOT handled here — it stays
    // "host offline" via `prepare_host_for_spawn` (never a local fallback).
    // Auth is never checked; model ids stay opaque passthrough (no catalog
    // validation). A resolve/probe miss never blocks (the existing gates own
    // the unknown-host/offline cases).
    let vendor_wire = session_vendor_wire(vendor);
    {
        let (bound_host, local_snapshot, sat_snapshot) = {
            let gw = deadline
                .lock(gateway)
                .await
                .map_err(|error| mcp_gateway_error("session_spawn", &error))?;
            let host = gw.project_bound_host(&project);
            let (local, satellite) = if host == ccteam_core::LOCAL_HOST {
                (gw.local_vendor_availability_override(), None)
            } else {
                (None, gw.satellite_agent_snapshot(&host))
            };
            (host, local, satellite)
        };
        if bound_host == ccteam_core::LOCAL_HOST {
            // Probe OFF the gateway lock (cached; shells out only on a cold
            // cache), so we never hold the mutex across a `<bin> --version`.
            let avail = match local_snapshot {
                Some(snapshot) => snapshot,
                None => tokio::task::spawn_blocking(|| {
                    ccteam_core::host_registry::probe_availability(false)
                })
                .await
                .unwrap_or_default(),
            };
            if let Some(row) = avail.iter().find(|a| a.vendor == vendor_wire) {
                if !row.installed {
                    let installed: Vec<String> = avail
                        .iter()
                        .filter(|a| a.installed)
                        .map(|a| a.vendor.to_string())
                        .collect();
                    return Err(super::vendor_panel::spawn_unavailable_message(
                        vendor_wire,
                        &bound_host,
                        &installed,
                        "just now",
                    ));
                }
            }
        } else if let Some((online, age, agents)) = sat_snapshot {
            // Only an ONLINE satellite's report is authoritative for "not
            // installed"; offline defers to the host-offline gate.
            if online {
                if let Some(a) = agents.iter().find(|a| a.vendor.as_str() == vendor_wire) {
                    if !a.installed {
                        let installed: Vec<String> = agents
                            .iter()
                            .filter(|a| a.installed)
                            .map(|a| a.vendor.clone())
                            .collect();
                        return Err(super::vendor_panel::spawn_unavailable_message(
                            vendor_wire,
                            &bound_host,
                            &installed,
                            &format!("{age}s ago"),
                        ));
                    }
                }
                // Vendor absent from the report → unknown; do not block.
            }
        }
    }

    // Per-key singleflight preserves idempotency while the vendor spawn itself
    // runs without the global gateway lock.
    let _idem_claim = if let Some(key) = idem_key.as_deref() {
        Some(
            crate::gateway::Gateway::claim_spawn_idempotency(gateway, &project, key, deadline)
                .await
                .map_err(|error| mcp_gateway_error("session_spawn", &error))?,
        )
    } else {
        None
    };
    let replay = if let Some(key) = idem_key.as_deref() {
        let mut gw = deadline
            .lock(gateway)
            .await
            .map_err(|error| mcp_gateway_error("session_spawn", &error))?;
        gw.spawn_idem_replay(&project, key)
    } else {
        None
    };
    // Idempotent replay: return the ORIGINAL body verbatim (+ a replay flag).
    if let Some(body) = replay {
        return Ok(mark_idempotent_replay(&body));
    }
    let created = crate::gateway::Gateway::create_delegated_session_shared(
        Arc::clone(gateway),
        project.clone(),
        role.clone(),
        vendor,
        permission_mode,
        protocol,
        fallback_owner_id,
        tuning,
        parent,
        title.clone(),
        deadline,
    )
    .await
    .map_err(|error| {
        if error
            .downcast_ref::<crate::gateway::GatewayRequestError>()
            .is_some()
        {
            mcp_gateway_error("session_spawn", &error)
        } else {
            spawn_create_error(error, &project, &caller, paths)
        }
    })?;
    let sid = created.sid;
    let resolved = gateway.lock().await.session_resolve(&sid);
    // Read the child meta once for the vendor resume key + the delegation
    // lineage (parent_sid/depth) the ledger just persisted.
    // vendor_session_id = the vendor's native resume key (`meta.vendor_uuid`).
    // May be empty for some vendors at spawn time — return "" honestly (the
    // codex-plugin-cc lesson: always surface the resume key when we have it).
    let child_meta = resolved.and_then(|r| {
        ccteam_harness::execution::session_meta::read_session_meta(&r.project_dir, &sid).ok()
    });
    let vendor_session_id = child_meta
        .as_ref()
        .map(|m| m.vendor_uuid.clone())
        .unwrap_or_default();
    let parent_sid = child_meta.as_ref().and_then(|m| m.parent_sid.clone());
    let delegation_depth = child_meta.as_ref().map(|m| m.delegation_depth).unwrap_or(0);
    let host = child_meta
        .as_ref()
        .map(|meta| meta.host.clone())
        .unwrap_or_else(|| ccteam_core::LOCAL_HOST.to_string());

    let mut body = serde_json::json!({
        "ok": true,
        "sid": sid,
        "project": project,
        "project_source": project_resolution.source,
        "role": role,
        "vendor": session_vendor_wire(vendor),
        "protocol": protocol.as_str(),
        "host": host,
        "vendor_session_id": vendor_session_id,
        "permission_mode": permission_mode.as_str(),
        "parent_sid": parent_sid,
        "delegation_depth": delegation_depth,
        "caller": caller_label,
        "hint": "dispatch a task with session_dispatch{sid, task}, then read the result with session_collect{sid}.",
    });
    if let Some(t) = &title {
        body["title"] = serde_json::json!(t);
    }
    // v0.9.1 — dispatch the optional first task through the SAME submit path
    // session_dispatch uses; its outcome (turn_id / status / inline result /
    // hint) merges into the spawn body so one call returns everything. The
    // caller's parent link doubles as the dispatcher identity (empty = admin,
    // ledger-only submit without a watch).
    if let Some(task) = task {
        let dispatcher_sid = parent_sid_for_task.as_deref().unwrap_or("");
        let frag = dispatch_task(
            gateway,
            "session_spawn",
            dispatcher_sid,
            &sid,
            task,
            requested_wait_seconds,
            effective_wait_seconds,
            notify,
            title.clone(),
            deadline,
        )
        .await?;
        if let Some(obj) = body.as_object_mut() {
            obj.remove("hint");
            obj.extend(frag);
        }
    }
    let out = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string());
    // v0.9.0 W2 (F7) — record for idempotent replay (the exact body a retry
    // returns, with a replay flag added). Keyed per-project by the client key.
    if let Some(key) = idem_key.as_deref() {
        gateway.lock().await.spawn_idem_record(&project, key, &out);
    }
    Ok(out)
}

/// Test-only 3-arg shim (the historical signature) — production goes through
/// [`run_session_spawn_at`] with real paths for catalog-aware errors.
#[cfg(test)]
async fn run_session_spawn(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    run_session_spawn_at(args, gateway, caller, None).await
}

struct SpawnProjectResolution {
    slug: String,
    source: &'static str,
}

/// Resolve the MCP spawn project. Every rung must NAME the project the caller
/// is actually bound to — explicit argument, the caller's own cwd, or the sole
/// registered project (zero ambiguity). A caller with none of those is
/// REFUSED and told which slugs exist.
///
/// There is deliberately no "daemon default" or lazily-provisioned scratch
/// rung. Falling back to a project the caller never named is the same defect
/// as the chat-side `current_project_for` fallback that was removed on
/// 2026-07-28: it lands somebody else's agent — and its turns, cost and files
/// — in a workspace they were never granted. This entry point was the one the
/// sweep missed, and it fires exactly when identity is already degraded (an
/// HTTP caller carries no cwd), so the two defects compounded: a session whose
/// principal did not arrive spawned children into a shared workspace and
/// reported success.
///
/// Ambient principals and tenants keep their identity-scoped behavior.
fn resolve_spawn_project(
    args: &serde_json::Value,
    caller: &McpCaller,
    paths: Option<&CcteamPaths>,
) -> std::result::Result<SpawnProjectResolution, String> {
    let arg = |name: &str| {
        args.get(name)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    match caller {
        McpCaller::Ambient => Ok(SpawnProjectResolution {
            slug: arg("_caller_slug")
                .ok_or_else(|| "session_spawn: no project (caller slug unset)".to_string())?,
            source: "principal",
        }),
        McpCaller::User { .. } => Ok(SpawnProjectResolution {
            slug: arg("project").ok_or_else(|| {
                "session_spawn: missing `project` — tenant MCP callers must name one of their own projects explicitly"
                    .to_string()
            })?,
            source: match args
                .get("_caller_project_source")
                .and_then(|value| value.as_str())
            {
                Some("sole") => "sole",
                _ => "explicit",
            },
        }),
        McpCaller::Admin => {
            if let Some(slug) = arg("project") {
                return Ok(SpawnProjectResolution {
                    slug,
                    source: "explicit",
                });
            }
            if let Some(slug) = arg("_caller_slug") {
                return Ok(SpawnProjectResolution {
                    slug,
                    source: "cwd",
                });
            }
            if let Some(slug) = sole_registered_project(paths) {
                return Ok(SpawnProjectResolution {
                    slug,
                    source: "sole",
                });
            }
            Err(format!(
                "session_spawn: missing `project` — name the workspace to spawn into{}",
                admin_project_catalog_hint(paths)
            ))
        }
    }
}

/// MCP-DX-1 — enrich a spawn-create failure. An ADMIN caller naming an
/// unknown project gets a "did you mean" + the registered catalog (the config
/// SoT the gateway also syncs from). Tenant visibility is enforced before the
/// create and ambient callers never name a project, so neither reaches this
/// enrichment — no cross-tenant existence disclosure.
fn spawn_create_error(
    err: anyhow::Error,
    project: &str,
    caller: &McpCaller,
    paths: Option<&CcteamPaths>,
) -> String {
    let base = format!("session_spawn: {err}");
    if !err.to_string().starts_with("unknown project") || !matches!(caller, McpCaller::Admin) {
        return base;
    }
    let Some(paths) = paths else { return base };
    let slugs = registered_project_slugs(paths);
    let suggestion = nearest_slug(project, &slugs)
        .map(|slug| format!(" — did you mean `{slug}`?"))
        .unwrap_or_default();
    format!(
        "{base}{suggestion}{}",
        admin_project_catalog_hint(Some(paths))
    )
}

fn mcp_gateway_error(tool: &str, err: &anyhow::Error) -> String {
    let code = err
        .downcast_ref::<crate::gateway::GatewayRequestError>()
        .map(crate::gateway::GatewayRequestError::error_code);
    match code {
        Some(code) => format!("{tool} failed: {err} (error_code={code})"),
        None => format!("{tool} failed: {err}"),
    }
}

/// v0.9.0 W2 (F7) — mark a recorded idempotency body as a replay: parse it,
/// insert `"idempotent_replay": true`, re-serialize. On a parse miss (should
/// never happen — we only store our own bodies) return the stored body as-is.
fn mark_idempotent_replay(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("idempotent_replay".to_string(), serde_json::json!(true));
            }
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string())
        }
        Err(_) => body.to_string(),
    }
}

/// Stable error for the removed MCP `session_spawn.protocol` input.
pub const PROTOCOL_SPAWN_PARAM_REMOVED: &str = "session_spawn: `protocol` was removed; the channel is derived from `vendor` (claude/codex/pi = stream-json, grok/opencode/kimi/dsh = acp) — omit `protocol`";

/// Derive the sole wire channel for an MCP-spawned vendor session:
/// claude/codex/pi = stream-json; grok/opencode/kimi/dsh = acp. The `protocol`
/// parameter was removed on 2026-07-26, mirroring the earlier `host` removal:
/// callers must omit a facet that carries no choice.
fn derive_session_protocol(vendor: ccteam_harness::AgentVendor) -> ccteam_harness::SessionProtocol {
    match vendor {
        ccteam_harness::AgentVendor::Grok
        | ccteam_harness::AgentVendor::Opencode
        | ccteam_harness::AgentVendor::Kimi
        | ccteam_harness::AgentVendor::Dsh => ccteam_harness::SessionProtocol::Acp,
        ccteam_harness::AgentVendor::Claude
        | ccteam_harness::AgentVendor::Codex
        | ccteam_harness::AgentVendor::Pi => ccteam_harness::SessionProtocol::StreamJson,
    }
}

/// Lowercase wire string for a spawned session's vendor (response field).
fn session_vendor_wire(v: ccteam_harness::AgentVendor) -> &'static str {
    match v {
        ccteam_harness::AgentVendor::Claude => "claude",
        ccteam_harness::AgentVendor::Codex => "codex",
        ccteam_harness::AgentVendor::Grok => "grok",
        ccteam_harness::AgentVendor::Opencode => "opencode",
        ccteam_harness::AgentVendor::Kimi => "kimi",
        ccteam_harness::AgentVendor::Pi => "pi",
        ccteam_harness::AgentVendor::Dsh => "dsh",
    }
}

/// `session_dispatch` — forward a task as a user turn to a session by sid.
/// v0.9.0 W2 (F2/F5/F7): an Ambient (agent) dispatch (a) rejects a cycle
/// (target == caller or an ancestor), (b) arms a durable completion watch on
/// the child (parent = the dispatcher) so its next turn notifies the parent,
/// (c) emits `delegation_dispatched`, and (d) optionally blocks up to
/// `wait_seconds` for the child's answer inline. `idempotency_key` makes a
/// client retry replay the original turn (never double-dispatch). `title` is
/// ledger/notification only — NEVER concatenated into the task.
async fn run_session_dispatch(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let deadline = crate::gateway::GatewayDeadline::start();
    let sid = arg_session_sid(args)?;
    // Driveability before anything else: ccteam has no thread to submit into for
    // an enrolled hand-started client, and every path below would call it
    // unknown instead of saying so.
    assert_target_is_driveable("session_dispatch", gateway, &sid, Some(deadline)).await?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "session_dispatch: missing `task`".to_string())?
        .to_string();
    // R-M3 — only operate sessions in the caller's own project.
    assert_caller_owns_session(
        "session_dispatch",
        args,
        gateway,
        &sid,
        &caller,
        Some(deadline),
    )
    .await?;

    let requested_wait_seconds = requested_inline_wait_seconds(args);
    let effective_wait_seconds = effective_inline_wait_seconds(requested_wait_seconds);
    let notify = parse_notify_mode("session_dispatch", args)?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if let Some(t) = &title {
        let n = t.chars().count();
        if n > 80 {
            return Err(format!(
                "session_dispatch: `title` too long ({n} chars; max 80)"
            ));
        }
    }
    let idem_key = args
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    // The dispatcher's server-resolved principal (Ambient only; injected in
    // `execute_session_tool`). A delegation is armed only for an agent caller.
    let caller_sid = match &caller {
        McpCaller::Ambient => args
            .get("_caller_sid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        McpCaller::Admin | McpCaller::User { .. } => String::new(),
    };
    let caller_slug = args
        .get("_caller_slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let is_delegation = !caller_sid.is_empty();

    // ---- Scope 1: idempotent replay + cycle guard (fast, no submit) ----
    {
        let mut gw = deadline
            .lock(gateway)
            .await
            .map_err(|error| mcp_gateway_error("session_dispatch", &error))?;
        if let Some(key) = idem_key.as_deref() {
            if let Some(body) = gw.dispatch_idem_replay(&sid, key) {
                return Ok(mark_idempotent_replay(&body));
            }
        }
        if is_delegation {
            let emit_cycle = |gw: &crate::gateway::Gateway| {
                if let Some((vendor, host, _)) = gw.session_vendor_host_slug(&sid) {
                    gw.emit_delegation_progress(
                        &caller_slug,
                        ccteam_harness::execution::progress_bridge::DELEGATION_DENIED,
                        &caller_sid,
                        &sid,
                        vendor,
                        &host,
                        None,
                        title.as_deref(),
                        Some("cycle"),
                    );
                }
            };
            if sid == caller_sid {
                emit_cycle(&gw);
                return Err(
                    "session_dispatch: delegation denied: cannot dispatch a session to itself (cycle)"
                        .to_string(),
                );
            }
            if gw.ancestor_chain(&caller_sid).contains(&sid) {
                emit_cycle(&gw);
                return Err(format!(
                    "session_dispatch: delegation denied: target {sid} is an ancestor of the caller {caller_sid} (cycle)"
                ));
            }
            // Budget gate: the CHILD's vendor accrues the cost of the task.
            if let Some((vendor, host, slug)) = gw.session_vendor_host_slug(&sid) {
                if gw.delegation_budget_exceeded(&slug, vendor) {
                    gw.emit_delegation_progress(
                        &slug,
                        ccteam_harness::execution::progress_bridge::DELEGATION_DENIED,
                        &caller_sid,
                        &sid,
                        vendor,
                        &host,
                        None,
                        title.as_deref(),
                        Some("budget"),
                    );
                    return Err(format!(
                        "session_dispatch: delegation denied: vendor `{}` has reached its 24h budget for project `{slug}` (adjust budgets or wait for the window to slide)",
                        crate::delegation::vendor_key(vendor)
                    ));
                }
            }
        }
    }

    // ---- Scope 2 + wait: the shared submit half (also used by spawn{task}) ----
    let frag = dispatch_task(
        gateway,
        "session_dispatch",
        &caller_sid,
        &sid,
        task,
        requested_wait_seconds,
        effective_wait_seconds,
        notify,
        title,
        deadline,
    )
    .await?;
    let mut body = serde_json::json!({ "ok": true, "sid": sid });
    if let Some(obj) = body.as_object_mut() {
        obj.extend(frag);
    }
    let out = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string());
    // v0.9.0 W2 (F7) — record for idempotent replay.
    if let Some(key) = idem_key.as_deref() {
        gateway.lock().await.dispatch_idem_record(&sid, key, &out);
    }
    Ok(out)
}

/// v0.9.1 delegation-ergonomics — the shared submit half of a dispatch, used
/// Parse the optional `notify` arg shared by `session_spawn`/`session_dispatch`:
/// `"final"` (default — notify once, when the dispatched task's vendor turn
/// completes and the child goes idle) / `"all"` (every mirrored assistant
/// message of that task; debug firehose) / `"off"` (ledger-only). The
/// pre-v0.9.5 boolean form still parses (`true`→final, `false`→off). Carries
/// whether the caller named the mode — see [`NotifyRequest`].
fn parse_notify_mode(
    tool: &str,
    args: &serde_json::Value,
) -> std::result::Result<NotifyRequest, String> {
    match args.get("notify") {
        None | Some(serde_json::Value::Null) => Ok(NotifyRequest::defaulted()),
        Some(v) => ccteam_harness::NotifyMode::parse_value(v)
            .map(NotifyRequest::explicit)
            .map_err(|e| format!("{tool}: {e}")),
    }
}

/// What a dispatch asked for on the `notify` axis: the mode, plus whether the
/// caller ASKED for it or just took the default. The difference only matters
/// for a target that is not one of the caller's own sessions (a handoff to a
/// peer): there, a default is not a request, and defaulting a peer into a
/// completion watch is how a one-off handoff became a standing subscription to
/// someone else's conversation.
#[derive(Debug, Clone, Copy)]
struct NotifyRequest {
    mode: ccteam_harness::NotifyMode,
    explicit: bool,
}

impl NotifyRequest {
    /// No `notify` arg — `final`, but only because nobody said otherwise.
    const fn defaulted() -> Self {
        Self {
            mode: ccteam_harness::NotifyMode::Final,
            explicit: false,
        }
    }

    /// The caller named a mode.
    const fn explicit(mode: ccteam_harness::NotifyMode) -> Self {
        Self {
            mode,
            explicit: true,
        }
    }
}

/// The completion-notification route for one dispatch. A managed ambient
/// caller has a parent session transport; the admin/user tiers do not, and
/// neither does an enrolled hand-started client (a real delegation parent that
/// ccteam holds no thread for). `notify:off` is distinct from a missing route so
/// it stays intentional and does not produce an operational warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionNotificationRoute {
    ParentSession,
    Disabled,
    Unavailable,
    /// v0.10.1 — the target is not one of the caller's own sessions and the
    /// caller did not ask for a notification: a handoff, deliberately not
    /// subscribed. Distinct from `Disabled` (which the caller chose) so the
    /// hint can say what to do instead.
    PeerUnsubscribed,
}

#[derive(Debug, Clone, Copy)]
struct InlineWaitWindow {
    effective_seconds: u64,
}

impl CompletionNotificationRoute {
    /// `parent_is_external`: the parent sid is an enrolled hand-started client.
    /// MCP is client-dial-in, so ccteam has no conversation of its own to put a
    /// completion turn into — the edge is real, the return transport is not.
    /// That is exactly what `Unavailable` already means for the admin front
    /// door, hence no new variant. An explicit `notify:off` still wins: an
    /// intentional opt-out is not a missing channel.
    fn resolve(
        caller_sid: &str,
        notify: NotifyRequest,
        parent_is_external: bool,
        peer_unsubscribed: bool,
    ) -> Self {
        if notify.mode == ccteam_harness::NotifyMode::Off {
            Self::Disabled
        } else if caller_sid.is_empty() || parent_is_external {
            Self::Unavailable
        } else if peer_unsubscribed {
            Self::PeerUnsubscribed
        } else {
            Self::ParentSession
        }
    }

    fn deliverable(self) -> bool {
        self == Self::ParentSession
    }

    fn async_hint(self) -> &'static str {
        match self {
            Self::ParentSession => {
                "the child runs asynchronously; you will be notified on completion (or poll session_collect{sid})."
            }
            Self::Disabled => {
                "the child runs asynchronously; notifications are disabled; poll session_collect{sid}."
            }
            Self::Unavailable => {
                "the child runs asynchronously; this caller has no completion notification channel; poll session_collect{sid}."
            }
            Self::PeerUnsubscribed => {
                "the task was handed to a session you did not delegate, so no completion watch was armed; poll session_collect{sid}, or re-dispatch with notify:\"final\" to be told when that one task ends."
            }
        }
    }
}

/// Derive a short ledger/display label from a spawn's first task: first
/// non-empty line, capped at 60 chars (with an ellipsis when cut). Display
/// only — never fed back into any prompt.
fn derive_title_from_task(task: &str) -> String {
    let line = task.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= 60 {
        line.to_string()
    } else {
        let head: String = line.chars().take(59).collect();
        format!("{head}…")
    }
}

/// by BOTH `session_dispatch` and `session_spawn{task}` (one-call
/// spawn+dispatch, the dominant delegation flow). Subscribe (if waiting) →
/// submit the task as a verbatim user turn → arm the delegation watch (agent
/// callers only; `caller_sid` empty = admin, no watch; a target the caller
/// never delegated is ledger-only unless `notify` was explicit) → emit
/// `delegation_dispatched` → optionally block inline for the child's answer.
/// Returns the response FRAGMENT (`turn_id`/`status`/result fields/`hint`)
/// the caller merges into its own body; `tool` prefixes error strings.
#[allow(clippy::too_many_arguments)]
async fn dispatch_task(
    gateway: &GatewayHandle,
    tool: &str,
    caller_sid: &str,
    sid: &str,
    task: String,
    _requested_wait_seconds: u64,
    effective_wait_seconds: u64,
    notify: NotifyRequest,
    title: Option<String>,
    deadline: crate::gateway::GatewayDeadline,
) -> std::result::Result<serde_json::Map<String, serde_json::Value>, String> {
    let is_delegation = !caller_sid.is_empty();
    let (rx, parent_is_external, peer_unsubscribed) = {
        let gw = deadline
            .lock(gateway)
            .await
            .map_err(|error| mcp_gateway_error(tool, &error))?;
        // Whether a completion turn is deliverable is a property of the PARENT's
        // ledger row, not of the caller's auth tier: a hand-started client dials
        // in over MCP, so there is no thread to steer and no session to resume.
        // Asked once, here, so the armed watch and the response fragment can
        // never disagree about it.
        let parent_is_external = is_delegation && gw.is_external_node(caller_sid);
        // Subscribe BEFORE submitting so a fast child can't answer before we
        // start listening (the wait races the child's own turn).
        let rx = if effective_wait_seconds > 0 {
            Some(gw.subscribe_events())
        } else {
            None
        };
        // v0.10.1 — is the target one of the caller's OWN sessions? A dispatch
        // to a session the caller never delegated is a HANDOFF: the target has
        // its own parent, or is a root with its own human. `session_list` draws
        // no edge for it (that tree is spawn lineage) and `session_stop` refuses
        // it, so a watch armed here is an edge nobody can see or take down. The
        // default `notify` is a default, not a request — only an explicit one
        // subscribes the caller to a session it does not own.
        let peer_unsubscribed =
            is_delegation && !notify.explicit && !gw.lineage_reaches(sid, caller_sid);
        (rx, parent_is_external, peer_unsubscribed)
    };
    if is_delegation {
        // The watch is armed either way — the completion edge belongs in the
        // ledger (`delegation_completed` fires off the mirror, whatever the
        // notify mode). Durable watch IO is explicitly outside the gateway
        // mutex; a generation fence rejects a concurrently replaced child. An
        // external parent gets it with notifications OFF: left on, the first
        // completion would submit into a session ccteam must never re-spawn,
        // fail, and drop the watch — silently ending that child's completion
        // accounting. A peer handoff gets the same treatment for the opposite
        // reason: the edge is real and worth recording, the subscription was
        // never asked for.
        let watch_notify = if parent_is_external || peer_unsubscribed {
            ccteam_harness::NotifyMode::Off
        } else {
            notify.mode
        };
        crate::gateway::Gateway::arm_delegation_watch_shared(
            Arc::clone(gateway),
            sid,
            caller_sid,
            watch_notify,
            title.clone(),
            None,
            deadline,
        )
        .await
        .map_err(|error| mcp_gateway_error(tool, &error))?;
    }
    let turn_id = match crate::gateway::Gateway::submit_to_sid_shared(
        Arc::clone(gateway),
        sid,
        task,
        deadline,
    )
    .await
    {
        Ok(turn_id) => turn_id,
        Err(error) => {
            if is_delegation {
                crate::gateway::Gateway::disarm_delegation_watch_shared(Arc::clone(gateway), sid)
                    .await;
            }
            return Err(mcp_gateway_error(tool, &error));
        }
    };
    if is_delegation {
        let gw = gateway.lock().await;
        if let Some((vendor, host, slug)) = gw.session_vendor_host_slug(sid) {
            gw.emit_delegation_progress(
                &slug,
                ccteam_harness::execution::progress_bridge::DELEGATION_DISPATCHED,
                caller_sid,
                sid,
                vendor,
                &host,
                Some(&turn_id),
                title.as_deref(),
                None,
            );
        }
    }
    let notification_route = CompletionNotificationRoute::resolve(
        caller_sid,
        notify,
        parent_is_external,
        peer_unsubscribed,
    );
    if notification_route == CompletionNotificationRoute::Unavailable {
        tracing::warn!(
            tool,
            child_sid = %sid,
            turn_id = %turn_id,
            notify = notify.mode.as_str(),
            parent_is_external,
            "ccteam MCP completion notification unavailable: caller has no managed parent session; poll session_collect"
        );
    } else if notification_route == CompletionNotificationRoute::PeerUnsubscribed {
        tracing::info!(
            tool,
            caller_sid,
            child_sid = %sid,
            turn_id = %turn_id,
            "ccteam MCP handoff to a session the caller did not delegate: ledger-only, no completion watch armed"
        );
    }

    // ---- wait branch (OFF the gateway lock) ----
    if let Some(rx) = rx {
        Ok(dispatch_wait_for_completion(
            gateway,
            sid,
            &turn_id,
            InlineWaitWindow {
                effective_seconds: effective_wait_seconds,
            },
            rx,
            is_delegation,
            notification_route,
        )
        .await)
    } else {
        let mut m = serde_json::Map::new();
        m.insert("turn_id".to_string(), serde_json::json!(turn_id));
        if turn_id.starts_with("queued-behind-body:") {
            // One sid, one body: the child's process from before a ccteam
            // restart is still finishing its turn; the task is queued behind
            // it and runs the moment that body exits (the notification then
            // arrives as usual).
            m.insert("status".to_string(), serde_json::json!("queued"));
            m.insert(
                "queued_behind".to_string(),
                serde_json::json!("detached_body"),
            );
            m.insert(
                "hint".to_string(),
                serde_json::json!(
                    "the session's body from before a ccteam restart is still finishing its \
                     turn; your task is queued and runs next — session_stop ends that body \
                     now, session_list shows it as activity:detached"
                ),
            );
            return Ok(m);
        }
        m.insert("status".to_string(), serde_json::json!("dispatched"));
        m.insert(
            "notify_deliverable".to_string(),
            serde_json::json!(notification_route.deliverable()),
        );
        m.insert(
            "hint".to_string(),
            serde_json::json!(notification_route.async_hint()),
        );
        Ok(m)
    }
}

/// Build the existing normal pending fragment, adding cap metadata only when
/// an over-ceiling request actually reaches its shorter effective deadline.
fn pending_dispatch_response(
    turn_id: &str,
    _wait: InlineWaitWindow,
    _hit_effective_deadline: bool,
    notification_route: CompletionNotificationRoute,
) -> serde_json::Map<String, serde_json::Value> {
    let mut response = serde_json::Map::new();
    response.insert("turn_id".to_string(), serde_json::json!(turn_id));
    response.insert("status".to_string(), serde_json::json!("pending"));
    if !notification_route.deliverable() {
        response.insert("notify_deliverable".into(), serde_json::json!(false));
    }
    response
}

/// v0.9.0 W2 (F2) — the OFF-lock half of a `wait_seconds>0` dispatch. Awaits an
/// `Answer` for `child_sid` on the gateway broadcast until the deadline. NEVER
/// holds the gateway lock across the await (lock discipline). On completion it
/// reads the child's freshly-appended turn (clean text) + cost from meta and,
/// for a delegation, disarms the watch (the caller already has the result
/// inline — suppress the redundant notification). On timeout it returns
/// `pending` and leaves the watch armed (the child is not cancelled). When a
/// request above the effective inline ceiling reaches that ceiling, the
/// pending response also reports the requested/effective waits and an honest
/// collect-or-notification hint.
///
/// v0.9.5 feedback fix — an `Answer` frame alone is NOT completion: codex
/// mirrors interim narration as separate answers inside one still-running
/// vendor turn, so returning on the first frame handed back "completed" with
/// the child's first checkpoint note (and disarmed the real completion's
/// watch). After a frame arrives, completion additionally requires the child's
/// turn to no longer be in flight (`session_turn_in_flight`, the same cell the
/// pump clears on completion or failure), re-checked on a short poll tick.
async fn dispatch_wait_for_completion(
    gateway: &GatewayHandle,
    child_sid: &str,
    turn_id: &str,
    wait: InlineWaitWindow,
    mut rx: tokio::sync::broadcast::Receiver<crate::gateway::GatewayEvent>,
    is_delegation: bool,
    notification_route: CompletionNotificationRoute,
) -> serde_json::Map<String, serde_json::Value> {
    // MCP-DX-1 — elapsed telemetry: the wait starts right after the submit, so
    // submit→completion is an honest task-duration approximation for a wait
    // that covers the whole task.
    let wait_started = tokio::time::Instant::now();
    let deadline = wait_started + std::time::Duration::from_secs(wait.effective_seconds);
    // Re-check cadence for "answer seen, is the turn still in flight?".
    const BOUNDARY_POLL: std::time::Duration = std::time::Duration::from_millis(200);
    let mut saw_answer = false;
    let mut hit_effective_deadline = false;
    let completed = loop {
        if saw_answer {
            let in_flight = {
                let gw = gateway.lock().await;
                gw.session_turn_in_flight(child_sid)
            };
            if !in_flight {
                break true;
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            hit_effective_deadline = true;
            break false;
        }
        // While mid-turn with an answer already seen, wake at least every
        // BOUNDARY_POLL to re-check the in-flight cell (the pump clears it on
        // the terminal boundary, which emits no separate completion frame).
        let wait_slice = if saw_answer {
            remaining.min(BOUNDARY_POLL)
        } else {
            remaining
        };
        match tokio::time::timeout(wait_slice, rx.recv()).await {
            Ok(Ok(ev)) => {
                let hit = ev.sid.as_deref() == Some(child_sid)
                    && matches!(ev.kind, crate::gateway::GatewayEventKind::Answer)
                    && ev.status.is_some();
                if hit {
                    saw_answer = true;
                }
            }
            // Broadcast lag → keep waiting (we may have missed unrelated frames).
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            // Sender gone (daemon shutdown) → pending.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break false,
            // Poll tick (or deadline) → loop re-checks in-flight / remaining.
            Err(_) => {}
        }
    };

    if !completed {
        return pending_dispatch_response(
            turn_id,
            wait,
            hit_effective_deadline,
            notification_route,
        );
    }

    // Resolve the child (sync) under a brief lock, then read its transcript
    // tail OFF the lock for a clean, unprefixed result.
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve(child_sid)
    };
    let (result_record, cost_usd, meta_turn) = resolved
        .as_ref()
        .map(|r| {
            let last =
                ccteam_harness::execution::turns_mirror::read_all_turns(&r.project_dir, &r.sid)
                    .ok()
                    .and_then(|all| all.into_iter().rev().find(|t| !t.assistant.is_empty()));
            // Session-ledger telemetry (MCP-DX-1): cumulative cost + raw
            // tokens, same semantics as session_list/collect (tokens present
            // even for vendors with no USD price table).
            let (cost, turn) =
                ccteam_harness::execution::session_meta::read_session_meta(&r.project_dir, &r.sid)
                    .ok()
                    .map(|m| (m.cost_usd, m.turn_count))
                    .unwrap_or((None, 0));
            (last, cost, turn)
        })
        .unwrap_or((None, None, 0));

    // Inline completion: the caller already holds the result → disarm the watch
    // so a delegation doesn't ALSO wake the parent with a redundant turn.
    if is_delegation {
        crate::gateway::Gateway::disarm_delegation_watch_shared(Arc::clone(gateway), child_sid)
            .await;
    }

    let record = result_record.as_ref();
    let mut m = DelegationSummary {
        sid: child_sid,
        turn_id,
        turn: record
            .and_then(|turn| turn.status.as_ref().map(|status| status.turn))
            .unwrap_or(meta_turn),
        outcome: match record {
            Some(turn) if turn.failed() => DelegationOutcome::Failed {
                kind: turn.error_kind.clone(),
                error: turn.error.clone(),
            },
            _ => DelegationOutcome::Done,
        },
        context_pct: record.and_then(|turn| crate::delegation::context_pct(turn.status.as_ref())),
        cost_usd,
        answer: record
            .map(|turn| turn.assistant.as_str())
            .unwrap_or_default(),
    }
    .inline_result();
    if !notification_route.deliverable() {
        m.insert("notify_deliverable".into(), serde_json::json!(false));
    }
    m
}

/// v0.8.7 review-fix (R-L3) — pure paging core of [`run_session_collect`],
/// extracted so the cursor/truncation contract is unit-testable without a
/// gateway or filesystem. Given ALL mirrored turns, an optional `since`
/// turn-id cursor, and a page size `n`, returns `(rows, next_cursor,
/// truncated)`:
///
/// - keeps only assistant-side turns AFTER `since` (or all when `since` is
///   `None` / not found — never silently lose turns on a stale cursor),
/// - returns the OLDEST `n` of those (so repeated polls page forward in order),
/// - `truncated` is true when more than `n` were available → the caller polls
///   again with `next_cursor` to fetch the remainder (the old code kept the
///   NEWEST `n` and dropped the middle of a > `n` burst).
fn page_collected_turns(
    all: &[ccteam_harness::execution::turns_mirror::TurnRecord],
    since: Option<&str>,
    n: usize,
    tail: bool,
) -> (Vec<serde_json::Value>, Option<String>, bool) {
    let after: Vec<&ccteam_harness::execution::turns_mirror::TurnRecord> = match since {
        Some(cursor) => match all.iter().position(|t| t.turn_id == cursor) {
            Some(idx) => all.iter().skip(idx + 1).collect(),
            // Cursor not found (rotated / typo) → return everything so the
            // caller never silently loses turns.
            None => all.iter().collect(),
        },
        None => all.iter().collect(),
    };
    let mut rows: Vec<serde_json::Value> = after
        .iter()
        .filter(|t| !t.assistant.is_empty())
        .map(|t| {
            let mut row = serde_json::json!({
                "turn_id": t.turn_id,
                "ts": t.ts.to_rfc3339(),
                "content": t.assistant,
            });
            if let Some(outcome) = t.outcome.as_deref() {
                row["outcome"] = serde_json::json!(outcome);
            }
            if let Some(kind) = t.error_kind.as_deref() {
                row["error_kind"] = serde_json::json!(kind);
            }
            if let Some(error) = t.error.as_deref() {
                row["error"] = serde_json::json!(error);
            }
            row
        })
        .collect();
    let truncated = rows.len() > n;
    if tail {
        // v0.9.1 — the "final answer" shape: keep the NEWEST n (chronological
        // order preserved inside the page).
        let drop = rows.len().saturating_sub(n);
        rows.drain(..drop);
    } else {
        rows.truncate(n);
    }
    let last_turn_id = rows
        .last()
        .and_then(|r| r.get("turn_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    (rows, last_turn_id, truncated)
}

fn collect_max_chars(args: &serde_json::Value) -> usize {
    let Some(value) = args.get("max_chars") else {
        return SESSION_COLLECT_DEFAULT_MAX_CHARS;
    };
    if let Some(n) = value.as_u64() {
        return n.clamp(
            SESSION_COLLECT_MIN_MAX_CHARS as u64,
            SESSION_COLLECT_MAX_MAX_CHARS as u64,
        ) as usize;
    }
    value
        .as_i64()
        .map(|n| {
            n.clamp(
                SESSION_COLLECT_MIN_MAX_CHARS as i64,
                SESSION_COLLECT_MAX_MAX_CHARS as i64,
            ) as usize
        })
        .unwrap_or(SESSION_COLLECT_DEFAULT_MAX_CHARS)
}

/// Fairly allocate a total output-character budget across turn contents:
/// short turns stay intact and the remaining budget is shared between long
/// turns. The returned budgets sum to at most `max_chars`.
fn collected_turn_budgets(lengths: &[usize], max_chars: usize) -> Vec<usize> {
    let mut budgets = vec![0; lengths.len()];
    if lengths.iter().sum::<usize>() <= max_chars {
        return lengths.to_vec();
    }
    let mut active: Vec<usize> = lengths
        .iter()
        .enumerate()
        .filter_map(|(idx, len)| (*len > 0).then_some(idx))
        .collect();
    let mut remaining = max_chars;
    while !active.is_empty() {
        let share = remaining / active.len();
        let settled: Vec<usize> = active
            .iter()
            .copied()
            .filter(|idx| lengths[*idx] <= share)
            .collect();
        if settled.is_empty() {
            let each = remaining / active.len();
            let extra = remaining % active.len();
            for (pos, idx) in active.into_iter().enumerate() {
                budgets[idx] = each + usize::from(pos < extra);
            }
            break;
        }
        for idx in &settled {
            budgets[*idx] = lengths[*idx];
            remaining = remaining.saturating_sub(lengths[*idx]);
        }
        active.retain(|idx| !settled.contains(idx));
    }
    budgets
}

/// Apply the collect character budget to the already-selected turn page.
/// Returns `(original_total_chars, any_content_truncated)`.
fn bound_collected_turns(rows: &mut [serde_json::Value], max_chars: usize) -> (usize, bool) {
    let lengths: Vec<usize> = rows
        .iter()
        .map(|row| {
            row.get("content")
                .and_then(|v| v.as_str())
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0)
        })
        .collect();
    let total_chars = lengths.iter().sum();
    if total_chars <= max_chars {
        return (total_chars, false);
    }
    let budgets = collected_turn_budgets(&lengths, max_chars);
    let mut truncated = false;
    for ((row, original_chars), budget) in rows.iter_mut().zip(lengths).zip(budgets) {
        if original_chars <= budget {
            continue;
        }
        let content = row
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let bounded = crate::delegation::truncate_head_tail_with_marker(content, budget, |n| {
            format!(
                "…[truncated {n} chars — full text stays in the session ledger; page with session_collect{{sid, since, n}}]…"
            )
        });
        row["content"] = serde_json::json!(bounded.text);
        truncated |= bounded.truncated;
    }
    (total_chars, truncated)
}

/// v0.9.1 — honest per-sid activity for the MCP surfaces: the SAME resolver the
/// web session list and IM `/status` use (`ccteam_core::stall`, →
/// `working|idle|stale|stuck`), so a polling parent can tell "child still
/// thinking" from "turn done" without scraping anything. `live` is the child's
/// in-flight turn as the daemon sees it, snapshotted under the gateway lock by
/// the caller — a mid-turn child reads `working` even if its project's progress
/// stream is unreadable. Best-effort: any read miss degrades to `None` (field
/// omitted).
fn classify_session_activity(
    projection: Option<&crate::progress_projection::ProgressProjection>,
    slug: &str,
    sid: &str,
    live: Option<ccteam_core::stall::LiveTurn>,
) -> Option<String> {
    let snapshot = projection?.project_snapshot(slug);
    let now = chrono::Utc::now();
    let silent_seconds = snapshot
        .last_valid
        .as_ref()
        .and_then(|event| ccteam_core::stall::progress_event_age_seconds(event, now))
        .unwrap_or(0);
    let activity = snapshot.session_activity(sid, silent_seconds, live, now);
    Some(activity.status.activity.to_string())
}

/// `session_collect` — tail the child's `turns.jsonl` (assistant turns).
/// Polled MVP: resolve sid → role + project_dir under the lock, drop the
/// guard, then read the ccteam-owned mirror. `since` is a turn_id cursor.
async fn run_session_collect(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    // Same gate as dispatch/stop: ccteam mirrors no transcript for a client it
    // never spawned, so the honest answer is what the session is — not an empty
    // page or an "unknown session" from the resolve below.
    assert_target_is_driveable("session_collect", gateway, &sid, None).await?;
    let since = args.get("since").and_then(|v| v.as_str()).map(String::from);
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|x| x as usize)
        .unwrap_or(SESSION_COLLECT_DEFAULT_N);
    let tail = args.get("tail").and_then(|v| v.as_bool()).unwrap_or(false);
    let max_chars = collect_max_chars(args);
    // R-M3 — only collect from sessions in the caller's own project.
    assert_caller_owns_session("session_collect", args, gateway, &sid, &caller, None).await?;

    // Resolve under the lock (sync) — with the child's in-flight turn, which is
    // a cheap in-memory peek — then DROP the guard before the fs read.
    let (resolved, live, projection) = {
        let gw = gateway.lock().await;
        (
            gw.session_resolve(&sid),
            gw.live_turn_for(&sid),
            gw.progress_projection(),
        )
    };
    let resolved = resolved.ok_or_else(|| format!("session_collect: unknown session: {sid}"))?;
    // A collectable session is one the gateway still tracks → "live" (the same
    // cheap liveness hint `session_list` reports; a fully-stopped session is no
    // longer resolvable so it errors above).
    let status = "live";

    // Tail the ccteam-owned transcript mirror.
    // v0.8.8 F1 — the mirror is keyed by `sid` (`.ccteam/chat/<sid>/turns.jsonl`),
    // not role, so read by `resolved.sid` (role is a content label only).
    let all = ccteam_harness::execution::turns_mirror::read_all_turns(
        &resolved.project_dir,
        &resolved.sid,
    )
    .map_err(|e| format!("session_collect: read turns.jsonl for {sid}: {e}"))?;

    // v0.9.0 W2 (F2) — surface the vendor resume key + accrued cost from meta.
    let meta = ccteam_harness::execution::session_meta::read_session_meta(
        &resolved.project_dir,
        &resolved.sid,
    )
    .ok();
    let vendor_session_id = meta
        .as_ref()
        .map(|m| m.vendor_uuid.clone())
        .unwrap_or_default();
    let cost_usd = meta.as_ref().and_then(|m| m.cost_usd);
    let tokens_total = meta.as_ref().and_then(|m| m.tokens_total);
    let model = meta
        .as_ref()
        .and_then(|m| m.observed_model.as_deref().or(m.model.as_deref()));
    let latest_status = all.iter().rev().find_map(|turn| turn.status.clone());
    // Apply the `since` cursor + page forward (R-L3 — oldest-first, no silent
    // drop of a > `n` burst; `tail:true` flips to newest-first). Pure logic in
    // `page_collected_turns`.
    let (mut rows, last_turn_id, page_truncated) =
        page_collected_turns(&all, since.as_deref(), n, tail);
    let (total_chars, content_truncated) = bound_collected_turns(&mut rows, max_chars);
    let truncated = page_truncated || content_truncated;

    let mut body = serde_json::json!({
        "ok": true,
        "sid": sid,
        "role": resolved.role,
        "vendor_session_id": vendor_session_id,
        "status": status,
        "turns": rows,
        // Cursor to pass as `since` on the next poll (None when no turns yet).
        // On truncation this is the boundary turn → poll again to get the rest.
        "cursor": last_turn_id,
        // True when more turns than `n` were available after `since`; the caller
        // should poll again with `cursor` to page through the remainder.
        "truncated": truncated,
        // Original character count across this selected page, before any
        // max_chars excerpts were applied.
        "total_chars": total_chars,
    });
    if let Some(pct) = latest_status
        .as_ref()
        .and_then(|status| status.context.as_ref())
        .and_then(|context| context.pct())
    {
        body["context_pct"] = serde_json::json!(pct.round() as u64);
    }
    if let Some(c) = cost_usd {
        body["cost_usd"] = serde_json::json!(c);
    }
    if let Some(t) = tokens_total {
        // v0.9.5 — honest token ledger for vendors with no USD price table.
        body["tokens_total"] = serde_json::json!(t);
    }
    let model = latest_status
        .as_ref()
        .and_then(|status| status.model.as_deref())
        .or(model);
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        body["model"] = serde_json::json!(model);
    }
    // v0.9.1 — honest per-sid activity (same resolver the web session list
    // uses): `working` = the child is mid-turn (keep polling), `idle` = the
    // turn is done. Best-effort: a read miss just omits the field.
    if let Some(activity) = classify_session_activity(
        projection.as_deref(),
        &resolved.project,
        &resolved.sid,
        live,
    ) {
        body["activity"] = serde_json::json!(activity);
    }
    // v0.9.0 W2 (F2) — a real collection by an agent is a ledger point.
    if caller == McpCaller::Ambient && !rows.is_empty() {
        if let (Some(m), Some(caller_sid)) = (
            meta.as_ref(),
            args.get("_caller_sid")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty()),
        ) {
            let gw = gateway.lock().await;
            gw.emit_delegation_progress(
                &resolved.project,
                ccteam_harness::execution::progress_bridge::DELEGATION_COLLECTED,
                caller_sid,
                &resolved.sid,
                m.vendor,
                &m.host,
                last_turn_id.as_deref(),
                None,
                None,
            );
        }
    }
    Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string()))
}

/// Default row cap for `session_list` (rows are already sorted most-recent
/// first by `session_views`; pass `limit` to widen/narrow).
const SESSION_LIST_DEFAULT_LIMIT: usize = 30;

/// `session_list` — snapshot the gateway's live sessions.
///
/// v0.9.5 feedback fix — a fleet of tens of live sessions dumped verbatim
/// flooded the caller's context (most rows stale, every null field spelled
/// out). The listing now accepts `project` / `activity` / `limit` filters,
/// caps at [`SESSION_LIST_DEFAULT_LIMIT`] most-recently-active rows by
/// default (explicit `truncated`/`total` fields say when a cap bit), and
/// omits null/empty row fields.
#[cfg(test)]
async fn run_session_list(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    run_session_list_at(args, gateway, None).await
}

async fn run_session_list_at(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    paths: Option<&CcteamPaths>,
) -> std::result::Result<String, String> {
    let caller_visible_projects: Option<std::collections::HashSet<String>> = args
        .get("_caller_visible_projects")
        .and_then(|projects| projects.as_array())
        .map(|projects| {
            projects
                .iter()
                .filter_map(|project| project.as_str().map(str::to_string))
                .collect()
        });
    // WHICH row is the caller. `current` cannot answer that — it means "the
    // active session of some chat", a fact about the fleet's routing that has
    // nothing to do with who is asking — and a caller that read it as "me"
    // spent a debugging round treating another session's tool calls as its own
    // identity being used by someone else (measured 2026-08-10, same title on
    // both rows). The caller's sid is server-resolved (`_caller_sid`, written
    // from the verified principal in `execute_session_tool`), so it covers a
    // managed session and an enrolled client's ledger node alike. An
    // admin/local/tenant-token caller is not a session and has no sid: nothing
    // is marked rather than guessed.
    let caller_sid = args
        .get("_caller_sid")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|sid| !sid.is_empty());
    let filter_project = args
        .get("project")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let filter_activity = args
        .get("activity")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "all");
    if let Some(a) = filter_activity.as_deref() {
        if !matches!(a, "working" | "idle" | "stale" | "stuck") {
            return Err(format!(
                "session_list: invalid `activity` filter `{a}` (expected `working` | `idle` | `stale` | `stuck` | `all`)"
            ));
        }
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, 500))
        .unwrap_or(SESSION_LIST_DEFAULT_LIMIT);

    // Both halves of the activity answer come from under ONE lock hold, and
    // both are cheap in-memory reads. Projection catch-ups happen below, after
    // the guard drops (a fleet's streams are far too big to touch under the
    // gateway mutex).
    let (views, live_turns, projection, status_roots) = {
        let gw = gateway.lock().await;
        let views = gw.session_views();
        let status_roots = views
            .iter()
            .filter_map(|view| {
                gw.session_resolve(&view.sid)
                    .map(|resolved| (view.sid.clone(), resolved.project_dir))
            })
            .collect::<std::collections::HashMap<_, _>>();
        (
            views,
            gw.live_turns(),
            gw.progress_projection(),
            status_roots,
        )
    };
    let context_pcts = status_roots
        .iter()
        .filter_map(|(sid, dir)| {
            ccteam_harness::execution::turns_mirror::read_all_turns(dir, sid)
                .ok()
                .and_then(|turns| turns.into_iter().rev().find_map(|turn| turn.status))
                .and_then(|status| status.context.and_then(|context| context.pct()))
                .map(|pct| (sid.clone(), pct.round() as u64))
        })
        .collect::<std::collections::HashMap<_, _>>();
    // v0.9.1 — honest activity per row (same resolver as the web session
    // list): one incremental snapshot per DISTINCT project, not per session.
    // Tests and daemonless callers may not have enabled the gateway projection;
    // when explicit paths exist, construct the same byte-cursor reader locally.
    let projection = projection.or_else(|| {
        paths.map(|paths| crate::progress_projection::ProgressProjection::new(paths.clone()))
    });
    let mut activity_ctx = std::collections::HashMap::new();
    if let Some(projection) = projection.as_ref() {
        for view in &views {
            if caller_visible_projects
                .as_ref()
                .is_some_and(|visible| !visible.contains(&view.project))
            {
                continue;
            }
            activity_ctx
                .entry(view.project.clone())
                .or_insert_with(|| projection.project_snapshot(&view.project));
        }
    }
    let now = chrono::Utc::now();
    // Classify once per view, then filter (project + activity), keeping the
    // most-recently-active-first order `session_views` already established.
    let classified: Vec<(&crate::gateway::SessionView, Option<String>)> = views
        .iter()
        .map(|v| {
            // A detached body (alive from before a daemon restart, not driven
            // from here) is its own state: neither working nor idle.
            if v.detached.is_some() {
                return (v, Some("detached".to_string()));
            }
            let activity = activity_ctx.get(&v.project).map(|snapshot| {
                let silent = snapshot
                    .last_valid
                    .as_ref()
                    .and_then(|event| ccteam_core::stall::progress_event_age_seconds(event, now))
                    .unwrap_or(0);
                snapshot
                    .session_activity(&v.sid, silent, live_turns.get(&v.sid).copied(), now)
                    .status
                    .activity
                    .to_string()
            });
            (v, activity)
        })
        .filter(|(v, activity)| {
            if caller_visible_projects
                .as_ref()
                .is_some_and(|visible| !visible.contains(&v.project))
            {
                return false;
            }
            if let Some(p) = filter_project.as_deref() {
                if v.project != p {
                    return false;
                }
            }
            if let Some(want) = filter_activity.as_deref() {
                return activity.as_deref() == Some(want);
            }
            true
        })
        .collect();
    let total = classified.len();
    let truncated = total > limit;
    let rows: Vec<serde_json::Value> = classified
        .iter()
        .take(limit)
        .map(|(v, activity)| {
            // Slim rows: null/empty/default fields are omitted rather than
            // spelled out (the caller reads these into its context).
            let mut row = serde_json::Map::new();
            row.insert("sid".into(), serde_json::json!(v.sid));
            row.insert("project".into(), serde_json::json!(v.project));
            if !v.role.is_empty() {
                row.insert("role".into(), serde_json::json!(v.role));
            }
            row.insert("vendor".into(), serde_json::json!(v.vendor));
            // The caller's OWN row (see `caller_sid`). Named nothing like
            // `current` on purpose: the two answer different questions, and
            // reading one as the other is the failure this ends.
            if caller_sid == Some(v.sid.as_str()) {
                row.insert("is_self".into(), serde_json::json!(true));
            }
            if v.current {
                row.insert("current".into(), serde_json::json!(true));
            }
            if let Some(a) = activity {
                // v0.9.1 — the honest busy signal (`working|idle|stale|stuck`;
                // `detached` = its body outlived a daemon restart and is
                // finishing unobserved — dispatches queue behind it).
                row.insert("activity".into(), serde_json::json!(a));
            }
            if let Some(d) = &v.detached {
                row.insert(
                    "detached".into(),
                    serde_json::json!({
                        "pid": d.pid,
                        "since": d.since,
                        "reason": d.reason,
                        "hint": "body from before a ccteam restart is still finishing its turn; dispatch queues behind it, session_stop ends it now",
                    }),
                );
            }
            if !v.last_active.is_empty() {
                row.insert("last_active".into(), serde_json::json!(v.last_active));
            }
            if v.waiting_approval {
                row.insert("waiting_approval".into(), serde_json::json!(true));
            }
            // v0.9.0 W2 (F2) — delegation topology + attribution.
            if let Some(p) = &v.parent_sid {
                row.insert("parent_sid".into(), serde_json::json!(p));
                row.insert(
                    "delegation_depth".into(),
                    serde_json::json!(v.delegation_depth),
                );
            }
            if v.host != "local" {
                row.insert("host".into(), serde_json::json!(v.host));
            }
            if let Some(c) = v.cost_usd {
                row.insert("cost_usd".into(), serde_json::json!(c));
            }
            if let Some(t) = v.tokens_total {
                // v0.9.5 — honest token ledger for unpriced vendors.
                row.insert("tokens_total".into(), serde_json::json!(t));
            }
            if let Some(pct) = context_pcts.get(&v.sid) {
                row.insert("context_pct".into(), serde_json::json!(pct));
            }
            if let Some(model) = v.model.as_deref().filter(|model| !model.trim().is_empty()) {
                row.insert("model".into(), serde_json::json!(model));
            }
            if let Some(t) = &v.title {
                row.insert("title".into(), serde_json::json!(t));
            }
            serde_json::Value::Object(row)
        })
        .collect();
    // v0.9.0 W2 (F2) — a `tree` view (roots → children by `parent_sid`) so a
    // caller sees the delegation topology without recomputing it. Built over
    // the FILTERED set (not the limit cut) so topology stays whole. Roots =
    // sessions whose parent isn't in this set (a true root, or a parent in
    // another project the caller can't see).
    let filtered: Vec<crate::gateway::SessionView> =
        classified.iter().map(|(v, _)| (*v).clone()).collect();
    let sids: std::collections::HashSet<&str> = filtered.iter().map(|v| v.sid.as_str()).collect();
    let tree: Vec<serde_json::Value> = filtered
        .iter()
        .filter(|v| {
            v.parent_sid
                .as_deref()
                .map(|p| !sids.contains(p))
                .unwrap_or(true)
        })
        .map(|v| session_tree_node(v, &filtered))
        .collect();
    let mut body = serde_json::json!({
        "ok": true,
        "sessions": rows,
        "tree": tree,
        "total": total,
    });
    if truncated {
        body["truncated"] = serde_json::json!(true);
        body["hint"] = serde_json::json!(format!(
            "{total} sessions matched but only the {limit} most recently active are shown — narrow with project/activity or raise `limit`."
        ));
    }
    Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_string()))
}

/// v0.9.0 W2 (F2) — build one node of the `session_list` delegation tree:
/// `{sid, role, vendor, children:[...]}` recursively (children = sessions whose
/// `parent_sid` is this sid). Depth is bounded by the live set, so the
/// recursion terminates.
fn session_tree_node(
    v: &crate::gateway::SessionView,
    all: &[crate::gateway::SessionView],
) -> serde_json::Value {
    let children: Vec<serde_json::Value> = all
        .iter()
        .filter(|c| c.parent_sid.as_deref() == Some(v.sid.as_str()) && c.sid != v.sid)
        .map(|c| session_tree_node(c, all))
        .collect();
    serde_json::json!({
        "sid": v.sid,
        "role": v.role,
        "vendor": v.vendor,
        "children": children,
    })
}

/// `session_stop` — deregister + close a session by sid (explicit command).
async fn run_session_stop(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    // Ahead of both scope checks: a hand-started client's process belongs to its
    // operator, and the descendant walk below would otherwise reject it as "not
    // a descendant" — true, but not the reason.
    assert_target_is_driveable("session_stop", gateway, &sid, None).await?;
    // R-M3 — only stop sessions in the caller's own project (explicit command,
    // never a proactive kill; the scope check just prevents cross-project stop).
    assert_caller_owns_session("session_stop", args, gateway, &sid, &caller, None).await?;
    // v0.9.0 W2 (F2) — an Ambient (agent) caller may only stop its OWN
    // descendants (walk the target's parent chain; it must reach the caller).
    // Admin/human callers are unrestricted (fleet-wide).
    let caller_sid = match &caller {
        McpCaller::Ambient => args
            .get("_caller_sid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        McpCaller::Admin | McpCaller::User { .. } => String::new(),
    };
    let mut gw = gateway.lock().await;
    if !caller_sid.is_empty() && !gw.ancestor_chain(&sid).contains(&caller_sid) {
        return Err(format!(
            "session_stop: permission denied — session {sid} is not a descendant of the caller {caller_sid} (an agent may only stop the sessions it delegated)"
        ));
    }
    // Capture the delegation event fields + drop the child's own watch BEFORE
    // the stop removes it from the live map.
    let stopped_meta = gw.session_vendor_host_slug(&sid);
    if !caller_sid.is_empty() {
        gw.disarm_delegation_watch(&sid);
    }
    gw.stop_session(&sid)
        .await
        .map_err(|e| format!("session_stop failed: {e}"))?;
    if !caller_sid.is_empty() {
        if let Some((vendor, host, slug)) = stopped_meta {
            gw.emit_delegation_progress(
                &slug,
                ccteam_harness::execution::progress_bridge::DELEGATION_STOPPED,
                &caller_sid,
                &sid,
                vendor,
                &host,
                None,
                None,
                None,
            );
        }
    }
    drop(gw);
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "ok": true,
        "sid": sid,
        "stopped": true,
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

/// Pull a required `sid` arg (the gateway `s{n}` id).
fn arg_session_sid(args: &serde_json::Value) -> std::result::Result<String, String> {
    args.get("sid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| "missing required `sid`".to_string())
}

/// Refuse a sid-addressed DRIVE on a hand-started client's ledger node.
///
/// Every driving tool calls this the moment it has its target, ahead of its own
/// resolution: an external node deliberately has no row in the live map (that
/// map is the set of sessions ccteam holds a thread for), so `session_resolve`
/// (dispatch/collect) and the descendant walk (stop) would report a session the
/// caller can SEE in `session_list` as unknown — a correct refusal that reads as
/// a ccteam bug. One shared message
/// ([`crate::external_nodes::not_driveable_error`]) says what the session IS: a
/// process its own operator drives, usable as a delegation parent.
async fn assert_target_is_driveable(
    tool: &str,
    gateway: &GatewayHandle,
    sid: &str,
    deadline: Option<crate::gateway::GatewayDeadline>,
) -> std::result::Result<(), String> {
    let is_external = match deadline {
        Some(deadline) => deadline
            .lock(gateway)
            .await
            .map_err(|error| mcp_gateway_error(tool, &error))?
            .is_external_node(sid),
        None => gateway.lock().await.is_external_node(sid),
    };
    if is_external {
        return Err(crate::external_nodes::not_driveable_error(tool, sid));
    }
    Ok(())
}

/// v0.8.7 review-fix (R-M3) — project-scope a sid-addressed `session_*` call:
/// the caller may only dispatch/collect/stop a session that runs in the
/// caller's OWN bound project (`_caller_slug`). Resolves the sid under the
/// gateway lock (sync `session_resolve`, no `.await` held), drops the guard,
/// then compares the session's project to the ambient slug. An unknown sid, an
/// unset ambient slug, or a project mismatch all reject — so a cto bound to
/// project A can never operate a project-B sid (even one another chat created).
/// Meaningful now that R-M1 gives the caller a verified identity.
async fn assert_caller_owns_session(
    name: &str,
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    sid: &str,
    caller: &McpCaller,
    deadline: Option<crate::gateway::GatewayDeadline>,
) -> std::result::Result<(), String> {
    // v0.9 T4 review fix — the verified admin (local mcp.sock admin token)
    // operates fleet-wide (same semantics as the web admin Identity): no ambient
    // slug to bind to. Unknown sids still fail inside the op itself.
    let resolved = {
        let gw = match deadline {
            Some(deadline) => deadline
                .lock(gateway)
                .await
                .map_err(|error| mcp_gateway_error(name, &error))?,
            None => crate::latency::gateway_lock(gateway, "mcp.session.resolve").await,
        };
        gw.session_resolve(sid)
    };
    match caller {
        McpCaller::Admin => Ok(()),
        McpCaller::User { user_id } => {
            let Some(resolved) = resolved else {
                return Err(format!("{name}: session not found"));
            };
            let state_path = CcteamPaths::project_state_in(&resolved.project_dir);
            let allowed = ccteam_core::ProjectState::load(&state_path)
                .map(|state| {
                    ccteam_core::identity::can_see_owner(user_id, false, state.owner.as_deref())
                })
                .unwrap_or(false);
            if allowed {
                Ok(())
            } else {
                Err(format!("{name}: session not found"))
            }
        }
        McpCaller::Ambient => {
            let caller_slug = args
                .get("_caller_slug")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{name}: no project scope (ambient slug unset)"))?
                .to_string();
            let resolved = resolved.ok_or_else(|| format!("{name}: unknown session: {sid}"))?;
            if resolved.project != caller_slug {
                return Err(format!(
                    "{name}: permission denied — session {sid} runs in project `{}`, but the caller is bound to project `{caller_slug}`",
                    resolved.project
                ));
            }
            Ok(())
        }
    }
}

/// Parse the optional `vendor` arg (default `claude`), lowercasing first so a
/// stray `"Claude"` still lands in the right variant (Bug A defense).
fn parse_session_vendor(
    args: &serde_json::Value,
) -> std::result::Result<ccteam_harness::AgentVendor, String> {
    match args.get("vendor").and_then(|v| v.as_str()) {
        None => Ok(ccteam_harness::AgentVendor::Claude),
        Some(raw) => match raw.to_lowercase().as_str() {
            "" | "claude" => Ok(ccteam_harness::AgentVendor::Claude),
            "codex" => Ok(ccteam_harness::AgentVendor::Codex),
            "grok" => Ok(ccteam_harness::AgentVendor::Grok),
            "opencode" => Ok(ccteam_harness::AgentVendor::Opencode),
            "kimi" => Ok(ccteam_harness::AgentVendor::Kimi),
            "pi" => Ok(ccteam_harness::AgentVendor::Pi),
            "dsh" => Ok(ccteam_harness::AgentVendor::Dsh),
            other => Err(format!(
                "session_spawn: invalid vendor `{other}`: expected `claude`, `codex`, `grok`, `opencode`, `kimi`, `pi`, or `dsh`"
            )),
        },
    }
}

#[cfg(test)]
mod chat_send_file_tests {
    use super::*;
    use crate::transport::OutboundFileKind;

    #[test]
    fn parse_outbound_kind_infers_photo_from_extension() {
        assert_eq!(
            parse_outbound_kind(None, "/x/shot.PNG"),
            OutboundFileKind::Photo
        );
        assert_eq!(
            parse_outbound_kind(None, "/x/a.jpeg"),
            OutboundFileKind::Photo
        );
        assert_eq!(
            parse_outbound_kind(None, "/x/report.pdf"),
            OutboundFileKind::Document
        );
        // Explicit kind overrides the extension.
        assert_eq!(
            parse_outbound_kind(Some("document"), "/x/shot.png"),
            OutboundFileKind::Document
        );
    }

    #[test]
    fn build_send_file_event_uses_live_target_and_attaches() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "caption": "the chart",
            "slug": "dev-foo",
            "role": "lead",
        });
        let live = Some(("telegram".to_string(), "chat-42".to_string()));
        let evt = build_send_file_event(&args, 7, live).unwrap();
        assert_eq!(evt.channel, "telegram");
        assert_eq!(evt.chat_id, "chat-42");
        assert_eq!(evt.attachments.len(), 1);
        assert_eq!(evt.attachments[0].kind, OutboundFileKind::Photo);
        assert_eq!(evt.attachments[0].caption.as_deref(), Some("the chart"));
        assert!(evt.id.ends_with("-7"));
    }

    /// v0.8.8 — the firing session's live reply target is the single source of
    /// truth; no registry is consulted (the actively-chatting agent pushes a
    /// file back without any prior `chat_register_bot`).
    #[test]
    fn build_send_file_event_uses_live_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("shot.png");
        std::fs::write(&file, b"png").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "slug": "dev-foo",
            "role": "cto",
        });
        let live = Some(("telegram".to_string(), "live-chat-7".to_string()));
        let evt = build_send_file_event(&args, 1, live).unwrap();
        assert_eq!(evt.channel, "telegram");
        assert_eq!(evt.chat_id, "live-chat-7");
    }

    /// v0.8.8 — a web live target routes to the web channel (the single source
    /// of truth carries whatever channel the firing session is bound to).
    #[test]
    fn build_send_file_event_live_target_web_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("x.txt");
        std::fs::write(&file, b"hi").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "lead",
        });
        let live = Some(("web".to_string(), "web-live".to_string()));
        let evt = build_send_file_event(&args, 2, live).unwrap();
        assert_eq!(evt.channel, "web");
        assert_eq!(evt.chat_id, "web-live");
    }

    #[test]
    fn web_outbound_is_copied_and_persisted_as_project_reference() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let project_dir = paths.projects_root.join("dev-foo");
        std::fs::create_dir_all(&project_dir).unwrap();
        let source = tmp.path().join("agent-output").join("chart.png");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"chart-bytes").unwrap();
        let args = serde_json::json!({
            "path": source.to_string_lossy(),
            "caption": "the chart",
            "slug": "spoofed-slug",
            "role": "spoofed-role",
            "_caller_sid": "s7",
        });
        let mut event =
            build_send_file_event(&args, 9, Some(("web".to_string(), "web-api".to_string())))
                .unwrap();
        let session = crate::gateway::SessionResolve {
            sid: "s7".into(),
            role: "reviewer".into(),
            vendor: "codex".into(),
            project: "dev-foo".into(),
            project_dir: project_dir.clone(),
        };

        stage_web_outbound_file(&mut event, &session, &paths, 9).unwrap();
        assert_eq!(event.sid.as_deref(), Some("s7"));
        assert_eq!(event.slug.as_deref(), Some("dev-foo"));
        assert_eq!(event.content, "the chart");
        assert_eq!(event.attachments[0].path, source.to_string_lossy());
        assert_eq!(event.attachments[0].size, 11);
        let id = event.attachments[0].id.clone();
        assert!(id.ends_with("-chart.png"), "got {id}");
        assert_eq!(
            std::fs::read(crate::transport::project_uploads_dir(&project_dir).join(&id)).unwrap(),
            b"chart-bytes"
        );
        std::fs::remove_file(&source).unwrap();
        let live_reference = event.attachments[0].attachment_ref().unwrap();
        assert_eq!(live_reference.name, "chart.png");
        assert_eq!(live_reference.size, 11);

        let turns =
            ccteam_harness::execution::turns_mirror::read_all_turns(&project_dir, "s7").unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].vendor, "codex");
        assert_eq!(turns[0].role, "reviewer");
        assert_eq!(turns[0].attachments.len(), 1);
        assert_eq!(turns[0].attachments[0].id, id);
        assert_eq!(turns[0].attachments[0].name, "chart.png");
        assert_eq!(turns[0].attachments[0].size, 11);
        let row = serde_json::to_string(&turns[0]).unwrap();
        assert!(!row.contains(source.to_string_lossy().as_ref()));
        assert!(!row.contains("base64"));
    }

    #[test]
    fn web_outbound_rejects_remote_host_project_without_local_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let project_dir = paths.projects_root.join("remote-demo");
        std::fs::create_dir_all(&project_dir).unwrap();
        ccteam_core::config::upsert_project(
            &paths.root,
            ccteam_core::ProjectEntry {
                slug: "remote-demo".into(),
                path: project_dir.clone(),
                host: "sat-a".into(),
                remote_slug: Some("remote-demo".into()),
                remote_path: None,
                team: "dev".into(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        let source = tmp.path().join("report.txt");
        std::fs::write(&source, b"report").unwrap();
        let args = serde_json::json!({
            "path": source.to_string_lossy(),
            "slug": "remote-demo",
            "role": "reviewer",
            "_caller_sid": "s8",
        });
        let mut event =
            build_send_file_event(&args, 10, Some(("web".to_string(), "web-api".to_string())))
                .unwrap();
        let session = crate::gateway::SessionResolve {
            sid: "s8".into(),
            role: "reviewer".into(),
            vendor: "claude".into(),
            project: "remote-demo".into(),
            project_dir: project_dir.clone(),
        };

        let error = stage_web_outbound_file(&mut event, &session, &paths, 10).unwrap_err();
        assert!(error.contains("remote host `sat-a`"), "got {error}");
        assert!(!crate::transport::project_uploads_dir(&project_dir).exists());
    }

    #[test]
    fn build_send_file_event_errors_on_missing_file() {
        let args = serde_json::json!({
            "path": "/nope/does-not-exist.png", "slug": "dev-foo", "role": "lead",
        });
        let err = build_send_file_event(&args, 0, None).unwrap_err();
        assert!(err.contains("file not found"), "got: {err}");
    }

    /// v0.8.8 — no live target (None) → precise error pointing at the
    /// spawn/bind flow; the registry is NOT consulted (single source of truth).
    #[test]
    fn build_send_file_event_errors_when_no_live_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("x.txt");
        std::fs::write(&file, b"hi").unwrap();
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "ghost",
        });
        let err = build_send_file_event(&args, 0, None).unwrap_err();
        assert!(err.contains("no IM chat bound"), "got: {err}");
    }

    #[test]
    fn build_send_file_event_errors_on_oversized_photo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("huge.png");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(11 * 1024 * 1024).unwrap(); // 11 MB (sparse) > 10 MB photo limit
        let args = serde_json::json!({
            "path": file.to_string_lossy(), "slug": "dev-foo", "role": "lead",
        });
        let err = build_send_file_event(&args, 0, None).unwrap_err();
        assert!(err.contains("too large"), "got: {err}");
    }

    #[test]
    fn tenant_delivery_uses_own_linked_im_and_never_client_addressing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let mut tenants = ccteam_core::tenants::TenantRegistry::default();
        let alice = tenants.add("alice");
        tenants.link_chat(&alice.id, "telegram:chat-42");
        tenants.set_telegram(
            &alice.id,
            Some(ccteam_core::tenants::TenantTelegram {
                bot_token: "123:test".into(),
                allowed_chat_ids: Vec::new(),
            }),
        );
        tenants.save(&paths.users_dir()).unwrap();

        assert_eq!(
            user_delivery_target(&paths, &alice.id).unwrap(),
            (format!("telegram@{}", alice.id), "chat-42".to_string())
        );
        assert!(user_delivery_target(&paths, "ubob")
            .unwrap_err()
            .contains("no longer registered"));
    }

    #[test]
    fn tenant_delivery_without_link_or_bot_recipient_is_readable_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let mut tenants = ccteam_core::tenants::TenantRegistry::default();
        let alice = tenants.add("alice");
        tenants.save(&paths.users_dir()).unwrap();
        let error = user_delivery_target(&paths, &alice.id).unwrap_err();
        assert!(error.contains("no linked IM destination"), "{error}");
    }
}

#[cfg(test)]
mod session_tool_tests {
    use super::*;
    use crate::delegation::INLINE_RESULT_MAX_CHARS;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": args },
        })
    }

    fn stub_vendor_availability(installed: bool) -> Vec<ccteam_core::VendorAvailability> {
        ccteam_core::AGENT_PROBE_SPECS
            .iter()
            .map(|spec| ccteam_core::VendorAvailability {
                vendor: spec.vendor,
                harness_id: spec.harness_id,
                installed,
                version: installed.then(|| format!("{} test stub", spec.vendor)),
            })
            .collect()
    }

    fn mark_stub_vendors_installed(gateway: &mut Gateway) {
        gateway.set_local_vendor_availability_for_tests(stub_vendor_availability(true));
    }

    #[test]
    fn effective_inline_wait_seconds_caps_at_transport_safe_ceiling() {
        for (requested, expected) in [(600, 240), (240, 240), (0, 0), (30, 30)] {
            assert_eq!(effective_inline_wait_seconds(requested), expected);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn capped_dispatch_wait_returns_honest_pending_and_keeps_child_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gateway, principal) = dispatch_gateway(true, 10_000, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gateway,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();

        // Inject a one-second effective wait into the shared production path;
        // the real ceiling remains a non-overridable 240s constant.
        let response = serde_json::Value::Object(
            dispatch_task(
                &gateway,
                "session_dispatch",
                &principal,
                &child,
                "slow task".to_string(),
                600,
                1,
                NotifyRequest::defaulted(),
                None,
                crate::gateway::GatewayDeadline::start(),
            )
            .await
            .expect("a capped inline timeout is a normal pending response"),
        );
        assert_eq!(response["status"], "pending");
        assert!(response["turn_id"].as_str().is_some());
        assert!(response.get("requested_wait_seconds").is_none());
        assert!(response.get("effective_wait_seconds").is_none());
        assert!(response.get("hint").is_none());
        assert!(
            gateway.lock().await.session_turn_in_flight(&child),
            "a capped pending response must not cancel the child turn"
        );
    }

    /// A dispatcher with only `paths` wired (enough for the local-admin
    /// promotion, which never touches gateway/sink/pending).
    fn dispatch_with_root(root: &std::path::Path) -> McpDispatch {
        McpDispatch {
            paths: CcteamPaths {
                root: root.to_path_buf(),
                projects_root: root.join("projects"),
            },
            sink: None,
            pending: None,
            gateway: None,
        }
    }

    fn write_web_token(root: &std::path::Path, token: &str) {
        let secrets = root.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(secrets.join("web-token"), format!("{token}\n")).unwrap();
    }

    // The LOCAL socket promotes a caller presenting the admin web token to
    // Admin semantics, and strips the token arg either way. This is the only
    // remaining door into that tier — the hand-started-session fallback it was
    // built for now enrolls and calls as a real principal instead.
    #[test]
    fn promote_local_admin_upgrades_on_matching_token_and_strips_arg() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_web_token(tmp.path(), "tok-abc123");
        let d = dispatch_with_root(tmp.path());
        let req = call(
            "session_list",
            json!({ "_caller_admin_token": "tok-abc123" }),
        );
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Admin);
        assert!(
            req.pointer("/params/arguments/_caller_admin_token")
                .is_none(),
            "token arg must be stripped before dispatch"
        );
    }

    #[test]
    fn promote_local_admin_fails_closed_on_wrong_or_missing_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_web_token(tmp.path(), "tok-abc123");
        let d = dispatch_with_root(tmp.path());

        // Wrong token → Ambient (and still stripped).
        let req = call("session_list", json!({ "_caller_admin_token": "wrong" }));
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
        assert!(req
            .pointer("/params/arguments/_caller_admin_token")
            .is_none());

        // No token arg → Ambient, request untouched.
        let req = call("session_list", json!({ "_caller_sid": "s1" }));
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
        assert_eq!(
            req.pointer("/params/arguments/_caller_sid"),
            Some(&json!("s1"))
        );

        // Token file absent on the daemon → Ambient even with an arg.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let d2 = dispatch_with_root(tmp2.path());
        let req = call("session_list", json!({ "_caller_admin_token": "anything" }));
        let (_req, caller) = d2.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
    }

    // v0.8.7 review-fix (R-M1/R-M3) — a no-process stub adapter so a real
    // `Gateway` can mint per-session secrets + track project scope without
    // spawning a `claude` pane. `start_thread` records the `(sid, secret)` the
    // gateway minted so the test can present the real secret to the gate.
    struct StubSpawnBarrier {
        armed: std::sync::atomic::AtomicBool,
        entered: std::sync::atomic::AtomicUsize,
        entered_notify: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }

    impl Default for StubSpawnBarrier {
        fn default() -> Self {
            Self {
                armed: std::sync::atomic::AtomicBool::new(false),
                entered: std::sync::atomic::AtomicUsize::new(0),
                entered_notify: tokio::sync::Notify::new(),
                release: tokio::sync::Semaphore::new(0),
            }
        }
    }

    impl StubSpawnBarrier {
        async fn wait_for(&self, count: usize) {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while self.entered.load(std::sync::atomic::Ordering::SeqCst) < count {
                    self.entered_notify.notified().await;
                }
            })
            .await
            .expect("concurrent MCP spawns reach the vendor barrier");
        }
    }

    #[derive(Clone, Default)]
    struct StubAdapter {
        spawns: std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
        /// v0.9.0 W2 — when true, `submit_turn` enqueues an echo AgentMessage
        /// the pump folds into an `Answer` (for the dispatch-wait tests).
        /// Default false = empty event stream (existing principal tests).
        answer: bool,
        /// Optional terminal failure in place of the normal echo + completed
        /// boundary. Used to prove MCP wait/collect preserve canonical errors.
        turn_failure: Option<ccteam_harness::ThreadErrorEvent>,
        /// v0.9.5 — when true (with `answer`), `submit_turn` prepends an
        /// interim narration message BEFORE the echo answer + boundary
        /// (models a codex child narrating checkpoints inside one turn).
        narrate: bool,
        /// Delay (ms) before `events()` yields — forces a `wait` timeout.
        event_delay_ms: u64,
        events: std::sync::Arc<
            tokio::sync::Mutex<std::collections::VecDeque<(String, ccteam_harness::ThreadEvent)>>,
        >,
        notify: std::sync::Arc<tokio::sync::Notify>,
        spawn_barrier: Option<std::sync::Arc<StubSpawnBarrier>>,
    }

    #[async_trait::async_trait]
    impl ccteam_harness::HarnessAdapter for StubAdapter {
        fn name(&self) -> &'static str {
            "stub-gate-test"
        }
        fn vendor(&self) -> ccteam_harness::AgentVendor {
            ccteam_harness::AgentVendor::Claude
        }
        async fn start_thread(
            &self,
            spec: &ccteam_harness::AgentSpecBrief,
            ctx: &ccteam_harness::SpawnCtx,
        ) -> std::result::Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError>
        {
            self.spawns
                .lock()
                .await
                .push((ctx.sid.clone(), ctx.secret.clone()));
            if let Some(barrier) = self.spawn_barrier.as_ref() {
                if barrier.armed.load(std::sync::atomic::Ordering::SeqCst) {
                    barrier
                        .entered
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    barrier.entered_notify.notify_waiters();
                    barrier
                        .release
                        .acquire()
                        .await
                        .expect("test barrier stays open")
                        .forget();
                }
            }
            Ok(ccteam_harness::ThreadHandle {
                vendor: ccteam_harness::AgentVendor::Claude,
                mode: ccteam_harness::ExecutionMode::Chat,
                identity: format!("{}-{}-{}", ctx.slug, spec.role, ctx.sid),
                started_at: chrono::Utc::now(),
                raw_extras: json!({}),
            })
        }
        async fn submit_turn(
            &self,
            h: &ccteam_harness::ThreadHandle,
            input: ccteam_harness::TurnInput,
        ) -> std::result::Result<ccteam_harness::TurnId, ccteam_harness::HarnessError> {
            if self.answer {
                let text = match input {
                    ccteam_harness::TurnInput::UserText(t) => t,
                    _ => String::new(),
                };
                let mut q = self.events.lock().await;
                q.push_back((
                    h.identity.clone(),
                    ccteam_harness::ThreadEvent::TurnStarted {
                        turn_id: format!("turn-{}", h.identity),
                    },
                ));
                if self.narrate {
                    q.push_back((
                        h.identity.clone(),
                        ccteam_harness::ThreadEvent::ItemCompleted {
                            item: ccteam_harness::ThreadItem {
                                id: "msg-0".into(),
                                details: ccteam_harness::ThreadItemDetails::AgentMessage(
                                    "interim narration checkpoint".into(),
                                ),
                            },
                        },
                    ));
                }
                if let Some(err) = self.turn_failure.clone() {
                    q.push_back((
                        h.identity.clone(),
                        ccteam_harness::ThreadEvent::TurnFailed {
                            turn_id: format!("turn-{}", h.identity),
                            err,
                            usage: ccteam_harness::UnifiedTokenUsage::default(),
                            model: None,
                        },
                    ));
                } else {
                    q.push_back((
                        h.identity.clone(),
                        ccteam_harness::ThreadEvent::ItemCompleted {
                            item: ccteam_harness::ThreadItem {
                                id: "msg-1".into(),
                                details: ccteam_harness::ThreadItemDetails::AgentMessage(format!(
                                    "echo: {text}"
                                )),
                            },
                        },
                    ));
                    // Every REAL adapter follows the answer with a turn boundary —
                    // required since v0.9.5: a `wait_seconds` dispatch completes on
                    // the boundary (turn no longer in flight), not the first frame.
                    q.push_back((
                        h.identity.clone(),
                        ccteam_harness::ThreadEvent::TurnCompleted {
                            turn_id: format!("turn-{}", h.identity),
                            usage: Default::default(),
                            model: None,
                        },
                    ));
                }
                drop(q);
                self.notify.notify_one();
            }
            Ok(ccteam_harness::TurnId::new(format!("turn-{}", h.identity)))
        }
        async fn submit_turn_routed(
            &self,
            h: &ccteam_harness::ThreadHandle,
            input: ccteam_harness::TurnInput,
            _routing: ccteam_harness::TurnRouting,
        ) -> std::result::Result<ccteam_harness::TurnSubmission, ccteam_harness::HarnessError>
        {
            self.submit_turn(h, input)
                .await
                .map(ccteam_harness::TurnSubmission::started)
        }
        async fn rebuild_tool_surface(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> Result<ccteam_harness::ToolSurfaceRebuild, ccteam_harness::HarnessError> {
            // Test double: no tool face to rebuild.
            Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
                reason: "test double".to_string(),
            })
        }

        fn event_attachment(&self) -> ccteam_harness::EventAttachment {
            // Scripted test stream: one-shot. Re-attaching would replay
            // the script, which is exactly what `Rebuildable` forbids.
            ccteam_harness::EventAttachment::OneShot
        }

        fn events(
            &self,
            h: &ccteam_harness::ThreadHandle,
        ) -> futures::stream::BoxStream<'static, ccteam_harness::ThreadEvent> {
            if !self.answer {
                return Box::pin(futures::stream::empty());
            }
            let events = std::sync::Arc::clone(&self.events);
            let notify = std::sync::Arc::clone(&self.notify);
            let wanted = h.identity.clone();
            let delay = self.event_delay_ms;
            Box::pin(futures::stream::unfold((), move |_| {
                let events = std::sync::Arc::clone(&events);
                let notify = std::sync::Arc::clone(&notify);
                let wanted = wanted.clone();
                async move {
                    loop {
                        if delay > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        }
                        let mut guard = events.lock().await;
                        if let Some(idx) = guard.iter().position(|(t, _)| t == &wanted) {
                            let (_, evt) = guard.remove(idx).unwrap();
                            return Some((evt, ()));
                        }
                        drop(guard);
                        notify.notified().await;
                    }
                }
            }))
        }
        async fn resume_thread(
            &self,
            _persistent_id: &str,
        ) -> std::result::Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError>
        {
            Err(ccteam_harness::HarnessError::NotImplemented {
                reason: "stub".to_string(),
            })
        }
        async fn close_thread(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> std::result::Result<(), ccteam_harness::HarnessError> {
            Ok(())
        }
        async fn handle_directive(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _d: ccteam_harness::Directive,
        ) -> std::result::Result<ccteam_harness::DirectiveOutcome, ccteam_harness::HarnessError>
        {
            Ok(ccteam_harness::DirectiveOutcome::Rejected {
                reason: "stub".to_string(),
            })
        }
        async fn thread_status(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> std::result::Result<ccteam_harness::ThreadStatus, ccteam_harness::HarnessError>
        {
            Ok(ccteam_harness::ThreadStatus {
                model: Some("stub-model".into()),
                context: Some(ccteam_harness::ContextUsage::known(
                    19,
                    100,
                    ccteam_harness::ContextSource::Reported,
                )),
                ..Default::default()
            })
        }
    }

    /// Build a real `Gateway` (stub adapter) with a cto session in `alpha` and
    /// a reviewer session in `beta`, returning the handle + the cto's minted
    /// secret so an end-to-end gate test can present a real `(role, secret)`.
    /// The secret is read back from the stub adapter's spawn recording (the
    /// gateway minted + injected it into the spawn ctx).
    async fn gateway_with_cto_and_cross_project() -> (GatewayHandle, String, String, String) {
        let stub = StubAdapter::default();
        let stub_for_factory = stub.clone();
        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(stub_for_factory.clone())
                as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gw =
            crate::gateway::Gateway::new_with_factory(factory, "alpha", "/tmp/cc-gate-alpha");
        mark_stub_vendors_installed(&mut gw);
        gw.register_project("beta", "/tmp/cc-gate-beta");
        let cto_sid = gw
            .create_session_api(
                "alpha".into(),
                "cto".into(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let beta_sid = gw
            .create_session_api(
                "beta".into(),
                "reviewer".into(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let cto_secret = stub
            .spawns
            .lock()
            .await
            .iter()
            .find(|(sid, _)| sid == &cto_sid)
            .map(|(_, secret)| secret.clone())
            .expect("stub recorded the cto session's minted secret");
        assert_eq!(cto_secret.len(), 32, "the minted secret is 128-bit hex");
        (
            std::sync::Arc::new(tokio::sync::Mutex::new(gw)),
            cto_sid,
            beta_sid,
            cto_secret,
        )
    }

    fn seed_owned_project(paths: &CcteamPaths, slug: &str, owner: &str) -> std::path::PathBuf {
        let dir = paths.projects_root.join(slug);
        std::fs::create_dir_all(dir.join(".ccteam")).unwrap();
        let mut state = ccteam_core::ProjectState::initial(slug.to_string());
        state.owner = Some(owner.to_string());
        state.save(&CcteamPaths::project_state_in(&dir)).unwrap();
        ccteam_core::config::upsert_project(
            &paths.root,
            ccteam_core::ProjectEntry {
                slug: slug.to_string(),
                path: dir.clone(),
                host: ccteam_core::LOCAL_HOST.to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .unwrap();
        dir
    }

    /// Tenant-scope fixture with Alice/Bob/admin projects and one live root in
    /// each. Every path is inside the caller's tempdir; no environment lookup or
    /// project bootstrap is involved.
    async fn gateway_with_tenant_projects(
        tmp: &std::path::Path,
    ) -> (CcteamPaths, GatewayHandle, String, String, String) {
        let paths = CcteamPaths {
            root: tmp.join("home"),
            projects_root: tmp.join("projects"),
        };
        let alice_dir = seed_owned_project(&paths, "alice", "user:ualice");
        let bob_dir = seed_owned_project(&paths, "bob", "user:ubob");
        let admin_dir = seed_owned_project(&paths, "admin", "user:web-api");

        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(StubAdapter::default())
                as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gateway = Gateway::new_with_factory(factory, "alice", alice_dir);
        mark_stub_vendors_installed(&mut gateway);
        gateway.register_project("bob", bob_dir);
        gateway.register_project("admin", admin_dir);
        gateway.enable_project_creation(paths.clone());

        let alice_sid = gateway
            .create_session_api_proto(
                "alice".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
                ccteam_harness::SessionProtocol::StreamJson,
                "ualice".into(),
            )
            .await
            .unwrap()
            .sid;
        let bob_sid = gateway
            .create_session_api_proto(
                "bob".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
                ccteam_harness::SessionProtocol::StreamJson,
                "ubob".into(),
            )
            .await
            .unwrap()
            .sid;
        let admin_sid = gateway
            .create_session_api_proto(
                "admin".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
                ccteam_harness::SessionProtocol::StreamJson,
                "web-api".into(),
            )
            .await
            .unwrap()
            .sid;
        (
            paths,
            std::sync::Arc::new(tokio::sync::Mutex::new(gateway)),
            alice_sid,
            bob_sid,
            admin_sid,
        )
    }

    /// v0.9.0 W1 (F1) — end-to-end: a caller presenting the WRONG secret for a
    /// real sid is rejected by `execute_session_tool` (the `(sid, secret)`
    /// principal is the authoritative check; a forged role arg is irrelevant).
    #[tokio::test]
    async fn execute_session_tool_rejects_wrong_secret() {
        let (gw, cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "session_list",
            json!({
                "_caller_sid": cto_sid,
                "_caller_secret": "ffffffffffffffffffffffffffffffff",
            }),
        );
        let resp = execute_session_tool(&req, Some(&gw), McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("could not be authenticated"),
            "wrong secret must fail auth, got: {text}"
        );
    }

    /// v0.9.0 W1 (F1) — end-to-end: the CORRECT `(sid, secret)` principal passes
    /// the gate and the call reaches the gateway (session_list returns rows).
    #[tokio::test]
    async fn execute_session_tool_allows_correct_principal() {
        let (gw, cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "session_list",
            json!({
                "_caller_sid": cto_sid,
                "_caller_secret": cto_secret,
            }),
        );
        let resp = execute_session_tool(&req, Some(&gw), McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], false, "correct principal passes");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"sessions\""), "got: {text}");
    }

    /// v0.9.0 W1 (F1) — end-to-end: a caller authenticated for project `alpha`
    /// is REJECTED when it tries to dispatch/collect/stop a `beta` sid
    /// (cross-project). The scope comes from the SERVER-resolved CallerCtx.slug.
    #[tokio::test]
    async fn execute_session_tool_rejects_cross_project_sid() {
        let (gw, cto_sid, beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        for tool in ["session_dispatch", "session_collect", "session_stop"] {
            let mut args = json!({
                "_caller_sid": cto_sid.clone(),
                "_caller_secret": cto_secret.clone(),
                "sid": beta_sid.clone(),
            });
            if tool == "session_dispatch" {
                args["task"] = json!("do something");
            }
            let resp = execute_session_tool(&call(tool, args), Some(&gw), McpCaller::Ambient).await;
            assert_eq!(resp["result"]["isError"], true, "{tool} must reject");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("permission denied") && text.contains("bound to project `alpha`"),
                "{tool}: cross-project must be denied with a clear reason, got: {text}"
            );
        }
    }

    /// v0.9.0 W1 (F1) — server-side slug overwrite: even if the caller SPOOFS
    /// `_caller_slug: "beta"`, the gate overwrites it from CallerCtx (the real
    /// project of the presented sid = `alpha`), so a `beta` sid is still denied.
    #[tokio::test]
    async fn execute_session_tool_overwrites_spoofed_caller_slug() {
        let (gw, cto_sid, beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let resp = execute_session_tool(
            &call(
                "session_collect",
                json!({
                    "_caller_sid": cto_sid,
                    "_caller_secret": cto_secret,
                    "_caller_slug": "beta", // spoof attempt — must be ignored
                    "sid": beta_sid,
                }),
            ),
            Some(&gw),
            McpCaller::Ambient,
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], true,
            "spoofed slug must not grant cross-project access"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("bound to project `alpha`"),
            "server must use CallerCtx.slug (alpha), not the spoofed `beta`, got: {text}"
        );
    }

    /// v0.9.0 W1 (F1) — positive control: the SAME caller operating its OWN
    /// `alpha` sid is allowed (the scope check isn't blanket-deny).
    #[tokio::test]
    async fn execute_session_tool_allows_same_project_sid() {
        let (gw, cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let target_sid = cto_sid.clone();
        let resp = execute_session_tool(
            &call(
                "session_collect",
                json!({
                    "_caller_sid": cto_sid,
                    "_caller_secret": cto_secret,
                    "sid": target_sid,
                }),
            ),
            Some(&gw),
            McpCaller::Ambient,
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], false,
            "same-project collect must be allowed: {resp}"
        );
    }

    #[test]
    fn is_session_tool_call_matches_only_session_tools_calls() {
        assert!(is_session_tool_call(&call("session_spawn", json!({}))));
        assert!(is_session_tool_call(&call(
            "session_collect",
            json!({ "sid": "s1" })
        )));
        // Foreign tool name.
        assert!(!is_session_tool_call(&call(
            "ccteam__chat_register_bot",
            json!({})
        )));
        // Right name, wrong method.
        assert!(!is_session_tool_call(&json!({
            "method": "tools/list",
            "params": { "name": "session_spawn" }
        })));
    }

    /// v0.9.0 W1 (F1) — an Ambient caller whose `(sid, secret)` principal
    /// resolves to no live session is REJECTED (needs a gateway to check).
    #[tokio::test]
    async fn execute_session_tool_ambient_denies_unknown_principal() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "session_list",
            json!({ "_caller_sid": "s999", "_caller_secret": "deadbeefdeadbeefdeadbeefdeadbeef" }),
        );
        let resp = execute_session_tool(&req, Some(&gw), McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("could not be authenticated"),
            "unknown principal must be denied, got: {text}"
        );
    }

    /// v0.9.0 W1 (F1) — fail-closed: with no gateway wired, EVERY Ambient
    /// session_* call is refused ("gateway not running"), never a fall-through
    /// that would skip the principal check.
    #[tokio::test]
    async fn execute_session_tool_ambient_gateway_down_fails_closed() {
        let req = call(
            "session_list",
            json!({ "_caller_sid": "s1", "_caller_secret": "abc" }),
        );
        let resp = execute_session_tool(&req, None, McpCaller::Ambient).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("gateway not running"),
            "gateway-down must fail closed, got: {text}"
        );
    }

    /// v0.9 T4 — the verified admin tier (local mcp.sock admin token) skips the
    /// principal gate entirely: NO `_caller_*` args, straight to the op (which
    /// then reports gateway-down here — proving the gate was bypassed, not that
    /// it denied).
    #[tokio::test]
    async fn execute_session_tool_admin_bypasses_gate_reports_gateway_down() {
        let req = call("session_list", json!({}));
        let resp = execute_session_tool(&req, None, McpCaller::Admin).await;
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            !text.contains("permission denied") && !text.contains("could not be authenticated"),
            "admin must skip the principal gate, got: {text}"
        );
        assert!(
            text.contains("gateway not running"),
            "expected gateway-down after the bypassed gate, got: {text}"
        );
    }

    /// v0.9 T4 — admin `session_list` works with NO ambient args and reaches the
    /// live gateway (fleet-wide semantics, same as the web admin Identity).
    #[tokio::test]
    async fn execute_session_tool_admin_lists_sessions_fleet_wide() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let resp = execute_session_tool(
            &call("session_list", json!({})),
            Some(&gw),
            McpCaller::Admin,
        )
        .await;
        assert_eq!(
            resp["result"]["isError"], false,
            "admin bypasses the principal gate: {resp}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"sessions\""), "got: {text}");
    }

    #[tokio::test]
    async fn user_spawn_is_root_owned_by_tenant_and_spoofed_caller_fields_are_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let req = call(
            "session_spawn",
            json!({
                "project": "alice",
                "vendor": "claude",
                "_caller_sid": "s999",
                "_caller_slug": "bob",
                "_caller_role": "forged",
                "_caller_depth": 99,
            }),
        );
        let response = execute_session_tool_with_paths(
            &req,
            Some(&gateway),
            McpCaller::User {
                user_id: "ualice".into(),
            },
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(body["project"], "alice");
        assert_eq!(body["project_source"], "explicit");
        assert!(
            body["parent_sid"].is_null(),
            "tenant spawn is a root: {body}"
        );
        assert_eq!(body["delegation_depth"], 0);
        assert_eq!(body["caller"], "user:ualice");

        let sid = body["sid"].as_str().unwrap();
        let meta = ccteam_harness::execution::session_meta::read_session_meta(
            &paths.projects_root.join("alice"),
            sid,
        )
        .unwrap();
        assert_eq!(meta.owner, "user:ualice");
        assert!(meta.parent_sid.is_none());
    }

    #[tokio::test]
    async fn admin_spawn_in_tenant_project_inherits_project_owner() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let response = execute_session_tool_with_paths(
            &call(
                "session_spawn",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                }),
            ),
            Some(&gateway),
            McpCaller::Admin,
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        let sid = body["sid"].as_str().unwrap();
        let meta = ccteam_harness::execution::session_meta::read_session_meta(
            &paths.projects_root.join("alice"),
            sid,
        )
        .unwrap();

        assert_eq!(meta.owner, "user:ualice");
        assert!(
            meta.parent_sid.is_none(),
            "an admin spawn that declares NO origin stays a root"
        );

        // …but an admin-tier caller that names itself gets the edge. It is
        // anonymous to the bridge (no per-session principal, and a socket line
        // carries no process context), so the declaration is the only signal —
        // validated against the ledger, never taken on faith.
        let response = execute_session_tool_with_paths(
            &call(
                "session_spawn",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                    "parent_sid": sid,
                }),
            ),
            Some(&gateway),
            McpCaller::Admin,
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(body["parent_sid"].as_str(), Some(sid), "{body}");
        assert_eq!(body["delegation_depth"].as_u64(), Some(1), "{body}");
        assert_eq!(
            body["caller"].as_str(),
            Some(format!("admin:{sid}").as_str()),
            "the response echoes the resolved origin: {body}"
        );
        let child_meta = ccteam_harness::execution::session_meta::read_session_meta(
            &paths.projects_root.join("alice"),
            body["sid"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            child_meta.parent_sid.as_deref(),
            Some(sid),
            "the ledger carries the edge, so the tree mounts"
        );

        // An unknown sid is a LOUD error, never a silent root.
        let response = execute_session_tool_with_paths(
            &call(
                "session_spawn",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                    "parent_sid": "s404",
                }),
            ),
            Some(&gateway),
            McpCaller::Admin,
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("not a live session"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn ambient_child_in_tenant_project_inherits_project_owner() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let response = run_session_spawn_at(
            &ambient(
                &alice_sid,
                "alice",
                json!({
                    "vendor": "claude",
                }),
            ),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&response).unwrap();
        let sid = body["sid"].as_str().unwrap();
        let meta = ccteam_harness::execution::session_meta::read_session_meta(
            &paths.projects_root.join("alice"),
            sid,
        )
        .unwrap();

        assert_eq!(meta.owner, "user:ualice");
        assert_eq!(meta.parent_sid.as_deref(), Some(alice_sid.as_str()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_spawn_task_in_tenant_project_never_targets_admin_frontends() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let alice_dir = seed_owned_project(&paths, "alice", "user:ualice");
        crate::credentials::save(
            &paths.im_credentials_path(),
            &crate::credentials::Credentials {
                telegram: Some(crate::credentials::TelegramCreds {
                    bot_token: "123:test".into(),
                    allowed_chat_ids: vec!["admin-chat".into()],
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let stub = StubAdapter {
            answer: true,
            ..Default::default()
        };
        let stub_for_factory = stub.clone();
        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(stub_for_factory.clone())
                as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gateway = Gateway::new_with_factory(factory, "alice", &alice_dir);
        mark_stub_vendors_installed(&mut gateway);
        gateway.enable_project_creation(paths.clone());
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gateway.set_event_sink(tx);
        let gateway = std::sync::Arc::new(tokio::sync::Mutex::new(gateway));

        let response = execute_session_tool_with_paths(
            &call(
                "session_spawn",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                    "task": "tenant-only result",
                }),
            ),
            Some(&gateway),
            McpCaller::Admin,
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        let sid = body["sid"].as_str().unwrap().to_string();

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let event = events
                    .recv()
                    .await
                    .expect("gateway event sink remains open");
                if matches!(event.kind, crate::gateway::GatewayEventKind::Answer) {
                    break event;
                }
            }
        })
        .await
        .expect("spawn task produces an answer");
        let mut answers = vec![first];
        for _ in 0..100 {
            if !gateway.lock().await.session_turn_in_flight(&sid) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        while let Ok(event) = events.try_recv() {
            if matches!(event.kind, crate::gateway::GatewayEventKind::Answer) {
                answers.push(event);
            }
        }

        assert!(
            answers
                .iter()
                .any(|event| event.channel == "web" && event.chat_id == "ualice"),
            "tenant web owns the answer route: {answers:?}"
        );
        assert!(
            answers.iter().all(
                |event| !(event.channel == "web" && event.chat_id == "web-api"
                    || event.channel == "telegram" && event.chat_id == "admin-chat")
            ),
            "tenant output must never target an admin frontend: {answers:?}"
        );
    }

    #[tokio::test]
    async fn user_spawn_requires_own_explicit_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        // MCP-DX-2 — a SECOND visible project keeps `project` genuinely
        // ambiguous (with exactly one, spawn now auto-defaults; see
        // `user_spawn_missing_project_defaults_to_sole_visible`).
        seed_owned_project(&paths, "alice2", "user:ualice");
        let caller = McpCaller::User {
            user_id: "ualice".into(),
        };

        let missing = execute_session_tool_with_paths(
            &call("session_spawn", json!({"vendor": "claude"})),
            Some(&gateway),
            caller.clone(),
            &paths,
        )
        .await;
        assert_eq!(missing["result"]["isError"], true);
        let missing_text = missing["result"]["content"][0]["text"].as_str().unwrap();
        assert!(missing_text.contains("missing `project`"), "{missing_text}");
        // MCP-DX-1 — actionable recovery: the error enumerates the caller's
        // OWN projects (identity-derived, never input-derived).
        assert!(missing_text.contains("your projects:"), "{missing_text}");
        assert!(missing_text.contains("alice"), "{missing_text}");
        assert!(missing_text.contains("alice2"), "{missing_text}");
        assert!(!missing_text.contains("bob"), "{missing_text}");

        // A foreign and a nonexistent project must stay BYTE-IDENTICAL (no
        // existence disclosure) — the appended own-project hint is a constant
        // for the caller, so the property is preserved.
        let mut denied_texts = Vec::new();
        for project in ["bob", "admin", "unknown"] {
            let denied = execute_session_tool_with_paths(
                &call("session_spawn", json!({"project": project})),
                Some(&gateway),
                caller.clone(),
                &paths,
            )
            .await;
            assert_eq!(denied["result"]["isError"], true, "{project}: {denied}");
            let text = denied["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                text.starts_with("session_spawn: project not found"),
                "{text}"
            );
            assert!(text.contains("your projects: alice"), "{text}");
            denied_texts.push(text);
        }
        assert!(
            denied_texts.windows(2).all(|pair| pair[0] == pair[1]),
            "foreign vs unknown project errors must be byte-identical: {denied_texts:?}"
        );
    }

    /// MCP-DX-2 — a tenant with exactly ONE visible project no longer needs
    /// to name it: spawn auto-defaults into it (identity-derived, the same
    /// disclosure surface as the own-projects hint).
    #[tokio::test]
    async fn user_spawn_missing_project_defaults_to_sole_visible() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let response = execute_session_tool_with_paths(
            &call("session_spawn", json!({"vendor": "claude"})),
            Some(&gateway),
            McpCaller::User {
                user_id: "ualice".into(),
            },
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(
            body["project"], "alice",
            "the sole visible project must be the default target: {body}"
        );
        assert_eq!(body["caller"], "user:ualice");
        assert_eq!(body["project_source"], "sole");
    }

    /// MCP-DX-1 — "did you mean" suggests close/contained names only; a wild
    /// guess is suppressed (worse than no hint).
    #[test]
    fn nearest_slug_suggests_close_and_contained_names_only() {
        let candidates = vec!["robchat".to_string(), "demo".to_string()];
        assert_eq!(nearest_slug("mychat", &candidates), Some("robchat"));
        assert_eq!(nearest_slug("chat", &candidates), Some("robchat"));
        assert_eq!(nearest_slug("demo2", &candidates), Some("demo"));
        assert_eq!(nearest_slug("Robchat", &candidates), Some("robchat"));
        assert_eq!(nearest_slug("zzz", &candidates), None);
        assert_eq!(nearest_slug("mychat", &[]), None);
    }

    #[test]
    fn format_slug_list_caps_and_reports_total() {
        let many: Vec<String> = (0..25).map(|i| format!("p{i}")).collect();
        let rendered = format_slug_list(&many);
        assert!(rendered.starts_with("p0, p1"), "{rendered}");
        assert!(rendered.ends_with("… (25 total)"), "{rendered}");
        assert!(!rendered.contains("p24"), "{rendered}");
        assert_eq!(format_slug_list(&many[..2]), "p0, p1");
    }

    /// MCP-DX-1 — an admin caller naming a nonexistent project gets a
    /// "did you mean" + the registered catalog instead of a dead end (the
    /// external-agent feedback: cwd-derived guesses like `mychat` vs the
    /// registered `robchat`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_spawn_unknown_project_suggests_nearest_and_lists_catalog() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        seed_owned_project(&paths, "robchat", "user:web-api");
        seed_owned_project(&paths, "demo", "user:web-api");
        let err = run_session_spawn_at(
            &json!({"project": "mychat", "vendor": "claude"}),
            &gw,
            McpCaller::Admin,
            Some(&paths),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("session_spawn: unknown project: mychat"),
            "{err}"
        );
        assert!(err.contains("did you mean `robchat`?"), "{err}");
        assert!(err.contains("registered projects: "), "{err}");
        assert!(err.contains("demo"), "{err}");
    }

    /// A caller that names no project and has no cwd is REFUSED, even when a
    /// `default_project` is configured and a shared `default_project` dir is
    /// available. Landing an unnamed caller in a workspace it was never
    /// granted is the defect, not a convenience: nothing may be created and
    /// the error must name the real slugs so the caller can pick one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_spawn_without_project_basis_is_refused_and_creates_nothing() {
        ccteam_core::tool_surface::disable_tool_surface_bootstrap_for_tests();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let alpha = seed_owned_project(&paths, "alpha", "user:web-api");
        let beta = seed_owned_project(&paths, "beta", "user:web-api");
        let config_path = ccteam_core::ccteam_config_path(&paths.root);
        let mut yaml = std::fs::read_to_string(&config_path).unwrap();
        yaml.push_str("default_project: beta\n");
        std::fs::write(&config_path, yaml).unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, &alpha).await;
        gw.lock().await.register_project("beta", beta);

        let err = run_session_spawn_at(
            &json!({"vendor": "claude"}),
            &gw,
            McpCaller::Admin,
            Some(&paths),
        )
        .await
        .unwrap_err();
        assert!(err.contains("missing `project`"), "{err}");
        assert!(err.contains("alpha"), "error names the catalog: {err}");
        assert!(err.contains("beta"), "error names the catalog: {err}");
        // The configured default was NOT silently used, and no scratch
        // workspace was provisioned as a side effect of a refused spawn.
        let scratch = paths.root.join("default_project");
        assert!(!scratch.exists(), "no scratch project provisioned");
        let cfg = ccteam_core::load_ccteam_config(&paths.root).unwrap();
        assert_eq!(cfg.projects.len(), 2, "catalog untouched: {cfg:?}");
    }

    /// MCP-DX-2 — with exactly ONE registered project, an admin spawn that
    /// names no project defaults to it instead of dead-ending (external MCP
    /// hosts run with a cwd outside any registered project, so `missing
    /// project` used to be unrecoverable without a docs lookup). The fixture
    /// The sole catalog project is selected and reported as such.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_spawn_missing_project_defaults_to_sole_registered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let robchat = seed_owned_project(&paths, "robchat", "user:web-api");
        let (gw, _principal) = dispatch_gateway(false, 0, &robchat).await;
        gw.lock().await.register_project("robchat", robchat.clone());
        let body = parse(
            &run_session_spawn_at(
                &json!({"vendor": "claude"}),
                &gw,
                McpCaller::Admin,
                Some(&paths),
            )
            .await
            .unwrap(),
        );
        assert_eq!(body["project"], "robchat", "{body}");
        assert_eq!(body["project_source"], "sole", "{body}");
        assert!(body.get("note").is_none(), "{body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_spawn_project_source_covers_explicit_cwd_and_principal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let alpha = seed_owned_project(&paths, "alpha", "user:web-api");
        let beta = seed_owned_project(&paths, "beta", "user:web-api");
        let (gw, principal) = dispatch_gateway(false, 0, &alpha).await;
        gw.lock().await.register_project("beta", beta);

        for (args, caller, source, project) in [
            (
                json!({"vendor":"claude","project":"alpha"}),
                McpCaller::Admin,
                "explicit",
                "alpha",
            ),
            (
                json!({"vendor":"claude","_caller_slug":"alpha"}),
                McpCaller::Admin,
                "cwd",
                "alpha",
            ),
            (
                ambient(&principal, "alpha", json!({"vendor":"claude"})),
                McpCaller::Ambient,
                "principal",
                "alpha",
            ),
        ] {
            let body = parse(
                &run_session_spawn_at(&args, &gw, caller, Some(&paths))
                    .await
                    .unwrap(),
            );
            assert_eq!(body["project_source"], source, "{body}");
            assert_eq!(body["project"], project, "{body}");
            assert!(body.get("note").is_none(), "{body}");
        }
    }

    /// MCP-CULL-3 — the wire protocol is derived from the vendor, and the
    /// removed input parameter is rejected for every value (including a
    /// formerly accepted matching value).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_protocol_is_derived_and_removed_param_is_rejected() {
        use ccteam_harness::{AgentVendor, SessionProtocol};
        for (vendor, derived) in [
            (AgentVendor::Claude, SessionProtocol::StreamJson),
            (AgentVendor::Codex, SessionProtocol::StreamJson),
            (AgentVendor::Grok, SessionProtocol::Acp),
            (AgentVendor::Opencode, SessionProtocol::Acp),
            (AgentVendor::Kimi, SessionProtocol::Acp),
            (AgentVendor::Pi, SessionProtocol::StreamJson),
            (AgentVendor::Dsh, SessionProtocol::Acp),
        ] {
            assert_eq!(derive_session_protocol(vendor), derived);
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, tmp.path()).await;
        for args in [
            json!({"vendor": "grok", "protocol": "acp"}),
            json!({"vendor": "grok", "protocol": "stream-json"}),
            json!({"vendor": "claude", "protocol": "terminal"}),
            json!({"vendor": "claude", "protocol": "bogus"}),
            json!({"vendor": "claude", "protocol": null}),
        ] {
            let err = run_session_spawn_at(&args, &gw, McpCaller::Admin, None)
                .await
                .unwrap_err();
            assert_eq!(err, PROTOCOL_SPAWN_PARAM_REMOVED);
        }
    }

    /// MCP-DX-2 — pure resolution rule: exactly one catalog entry → that
    /// slug; zero or several → no default.
    #[test]
    fn sole_registered_project_requires_exactly_one_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        assert_eq!(sole_registered_project(None), None);
        assert_eq!(sole_registered_project(Some(&paths)), None);
        seed_owned_project(&paths, "robchat", "user:web-api");
        assert_eq!(
            sole_registered_project(Some(&paths)).as_deref(),
            Some("robchat")
        );
        seed_owned_project(&paths, "demo", "user:web-api");
        assert_eq!(sole_registered_project(Some(&paths)), None);
    }

    /// MCP-DX-1 — an inline-wait completion carries submit→completion timing
    /// and the child's session ledger (cost + raw tokens), so a waiting caller
    /// can log per-vendor speed/cost without a second collect round-trip.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_completion_reports_ledger_and_elapsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // Seed the child's ledger the way the event pump would have.
        let mut meta =
            ccteam_harness::execution::session_meta::read_session_meta(tmp.path(), &child).unwrap();
        meta.cost_usd = Some(0.12);
        meta.tokens_total = Some(12_345);
        gw.lock()
            .await
            .persist_session_meta(tmp.path(), &meta)
            .unwrap();

        let frag = dispatch_task(
            &gw,
            "session_dispatch",
            &principal,
            &child,
            "quick question".to_string(),
            6,
            6,
            NotifyRequest::defaulted(),
            None,
            crate::gateway::GatewayDeadline::start(),
        )
        .await
        .unwrap();
        let response = serde_json::Value::Object(frag);
        assert_eq!(response["status"], "completed", "{response}");
        assert_eq!(response["cost_usd"], 0.12, "{response}");
        assert!(
            response.get("status_line").is_none(),
            "MCP stays numeric: {response}"
        );
        assert_eq!(response["context_pct"], 19, "{response}");
        let keys: Vec<_> = response.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                "context_pct",
                "cost_usd",
                "result_text",
                "status",
                "turn",
                "turn_id"
            ]
        );
        for obsolete in ["tokens_total", "elapsed_seconds", "model", "result_turn"] {
            assert!(response.get(obsolete).is_none(), "{obsolete}: {response}");
        }
    }

    #[tokio::test]
    async fn user_foreign_and_unknown_sid_errors_are_identical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let caller = McpCaller::User {
            user_id: "ualice".into(),
        };

        for tool in ["session_dispatch", "session_collect", "session_stop"] {
            let invoke = |sid: &str| {
                let mut args = json!({"sid": sid});
                if tool == "session_dispatch" {
                    args["task"] = json!("do not leak");
                }
                call(tool, args)
            };
            let foreign = execute_session_tool_with_paths(
                &invoke(&bob_sid),
                Some(&gateway),
                caller.clone(),
                &paths,
            )
            .await;
            let unknown = execute_session_tool_with_paths(
                &invoke("s999999"),
                Some(&gateway),
                caller.clone(),
                &paths,
            )
            .await;
            assert_eq!(foreign["result"]["isError"], true, "{tool}: {foreign}");
            assert_eq!(unknown["result"]["isError"], true, "{tool}: {unknown}");
            assert_eq!(
                foreign["result"]["content"][0]["text"], unknown["result"]["content"][0]["text"],
                "{tool}: forbidden and unknown sids must be indistinguishable"
            );
        }
    }

    #[tokio::test]
    async fn user_session_list_filters_to_owned_projects_and_overwrites_spoofed_scope() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, bob_sid, admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let response = execute_session_tool_with_paths(
            &call(
                "session_list",
                json!({"_caller_visible_projects": ["alice", "bob", "admin"]}),
            ),
            Some(&gateway),
            McpCaller::User {
                user_id: "ualice".into(),
            },
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        let sids: Vec<&str> = body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|session| session["sid"].as_str())
            .collect();
        assert!(sids.contains(&alice_sid.as_str()), "{body}");
        assert!(!sids.contains(&bob_sid.as_str()), "{body}");
        assert!(!sids.contains(&admin_sid.as_str()), "{body}");
        assert_eq!(body["total"], 1);
    }

    /// 2026-07-26 cull — `screenshot` fell out of the MCP surface entirely;
    /// any caller (tenant included) now gets the protocol core's unknown-tool
    /// error, never a renderer path.
    #[tokio::test]
    async fn screenshot_is_unknown_tool_after_cull() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let dispatch = McpDispatch {
            paths,
            sink: None,
            pending: None,
            gateway: Some(gateway),
        };
        let response = dispatch
            .dispatch_as(
                call("screenshot", json!({"slug": "bob"})),
                McpCaller::User {
                    user_id: "ualice".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("unknown tool: screenshot"), "{text}");
    }

    /// v0.9 T4 review fix — the internal-bus methods are refused on the admin
    /// (HTTP) transport with JSON-RPC `-32601`; they remain mcp.sock-only
    /// (HITL stays on vendor-native / in-band channels — tech-design §1.1).
    #[tokio::test]
    async fn dispatch_as_admin_refuses_internal_bus_methods() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dispatch = McpDispatch {
            paths: ccteam_core::CcteamPaths {
                root: tmp.path().join("home"),
                projects_root: tmp.path().join("projects"),
            },
            sink: None,
            pending: None,
            gateway: None,
        };
        for method in ["permission/ask", "interaction/ask"] {
            let req = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": {}});
            let resp = dispatch
                .dispatch_as(req, McpCaller::Admin)
                .await
                .expect("refusal is an error response, not a notification");
            assert_eq!(resp["error"]["code"], -32601, "{method}: {resp}");
            assert_eq!(resp["id"], 7, "{method}: id must round-trip");
        }
    }

    #[test]
    fn parse_session_vendor_defaults_to_claude_and_lowercases() {
        assert_eq!(
            parse_session_vendor(&json!({})).unwrap(),
            ccteam_harness::AgentVendor::Claude
        );
        assert_eq!(
            parse_session_vendor(&json!({ "vendor": "Claude" })).unwrap(),
            ccteam_harness::AgentVendor::Claude
        );
        assert_eq!(
            parse_session_vendor(&json!({ "vendor": "codex" })).unwrap(),
            ccteam_harness::AgentVendor::Codex
        );
        assert_eq!(
            parse_session_vendor(&json!({ "vendor": "pi" })).unwrap(),
            ccteam_harness::AgentVendor::Pi
        );
        assert!(parse_session_vendor(&json!({ "vendor": "gpt" })).is_err());
    }

    #[test]
    fn arg_session_sid_requires_non_empty() {
        assert_eq!(arg_session_sid(&json!({ "sid": "s3" })).unwrap(), "s3");
        assert!(arg_session_sid(&json!({})).is_err());
        assert!(arg_session_sid(&json!({ "sid": "" })).is_err());
    }

    #[test]
    fn session_tool_response_shapes_content_and_is_error() {
        let ok = session_tool_response(json!(1), "done".into(), false);
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(ok["result"]["content"][0]["text"], "done");
        let err = session_tool_response(json!(2), "boom".into(), true);
        assert_eq!(err["result"]["isError"], true);
    }

    // ── v0.8.7 W2 (DB.3/DB.4) — HITL permission/ask wiring ────────────────

    #[test]
    fn is_permission_ask_call_matches_only_the_raw_method() {
        assert!(is_permission_ask_call(
            &json!({ "method": "permission/ask" })
        ));
        // Not a tools/call, and not interaction/ask.
        assert!(!is_permission_ask_call(
            &json!({ "method": "interaction/ask" })
        ));
        assert!(!is_permission_ask_call(&call("session_spawn", json!({}))));
    }

    #[test]
    fn summarize_tool_input_picks_the_useful_field() {
        // Bash → command.
        assert_eq!(
            summarize_tool_input("Bash", &json!({ "command": "rm -rf /tmp/x" })),
            "Bash rm -rf /tmp/x"
        );
        // Write with no content → just the path (no dangling preview).
        assert_eq!(
            summarize_tool_input("Write", &json!({ "file_path": "/a/b.rs" })),
            "Write /a/b.rs"
        );
        // No dedicated renderer + empty params → just the tool name.
        assert_eq!(summarize_tool_input("Glob", &json!({})), "Glob");
    }

    /// v0.8.22 P1 (review §3.1-2) — delegates to `crate::hitl`'s
    /// tool-aware summarizer: a `Write`/`Edit` approval now shows the
    /// content/diff, not just the path (see `hitl.rs`'s own unit tests for
    /// full per-tool coverage; this just locks in the CLI's delegation).
    #[test]
    fn summarize_tool_input_write_shows_content_preview() {
        assert_eq!(
            summarize_tool_input(
                "Write",
                &json!({ "file_path": "/a/b.rs", "content": "fn f(){}" })
            ),
            "Write /a/b.rs\n  + fn f(){}"
        );
    }

    #[test]
    fn summarize_tool_input_truncates_long_detail() {
        let long = "x".repeat(500);
        let out = summarize_tool_input("Bash", &json!({ "command": long }));
        // Truncated with an ellipsis; comfortably bounded (~200-char cap).
        assert!(out.ends_with('…'));
        assert!(
            out.chars().count() <= 207,
            "got {} chars",
            out.chars().count()
        );
    }

    /// permission/ask with no gateway/sink/pending wired returns a JSON-RPC
    /// error (the hook then fail-safe denies) — never panics. Deterministic:
    /// passes `None` for sink/pending/gateway, so no socket / IM is touched.
    #[tokio::test]
    async fn permission_ask_without_gateway_returns_error() {
        let req = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "permission/ask",
            "params": { "slug": "s", "role": "r", "tool_name": "Bash" }
        });
        let resp = execute_permission_ask(&req, None, None, None).await;
        assert!(
            resp.get("error").is_some(),
            "no gateway ⇒ JSON-RPC error so the hook denies: {resp}"
        );
        assert_eq!(resp["id"], json!(7));
    }

    /// v0.8.7 review-fix (R-L1) — the resolved HITL permission-prompt TTL is
    /// SHORTER than the 600s interaction/ask TTL (a tool-approval parks the
    /// whole turn, so a fast fail-safe deny beats a long park). Exercises the
    /// runtime resolver (default path, env unset) so the relation is a real
    /// value comparison, not a const fold. The env override is exercised by an
    /// integration test (separate process — env mutation discipline).
    #[test]
    fn permission_prompt_ttl_is_shorter_than_interaction_ttl() {
        let ttl = permission_prompt_timeout_secs();
        let interaction = INTERACTION_ASK_TIMEOUT_SECS;
        assert!(
            ttl < interaction,
            "permission TTL ({ttl}s) must be shorter than interaction TTL ({interaction}s)"
        );
        assert!(ttl >= 1, "TTL must be clamped to >= 1s, got {ttl}");
    }

    /// v0.8.7 review-fix (R-L1) — `emit_permission_prompt_outstanding` with a
    /// blank slug is a no-op (nothing to address) and never panics. The
    /// env-resolved write path is covered by an integration test (separate
    /// process); here we pin the cheap guard.
    #[test]
    fn emit_permission_prompt_outstanding_blank_slug_is_noop() {
        // Must not panic / must not touch the filesystem for a blank slug.
        emit_permission_prompt_outstanding("", "cto", "Bash", "rm -rf /", 120);
    }

    // ---------- v0.8.7 review-fix (R-L3) session_collect paging ----------

    fn turn(id: &str) -> ccteam_harness::execution::turns_mirror::TurnRecord {
        ccteam_harness::execution::turns_mirror::TurnRecord {
            turn_id: id.to_string(),
            ts: chrono::Utc::now(),
            vendor: "claude".to_string(),
            role: "cto".to_string(),
            user: "q".to_string(),
            assistant: format!("a-{id}"),
            usage: serde_json::Value::Null,
            status: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
            outcome: None,
            error_kind: None,
            error: None,
        }
    }

    /// A burst of MORE than `n` turns after the cursor must NOT silently drop
    /// the middle: `page_collected_turns` returns the OLDEST `n`, sets the
    /// cursor to that page's boundary, and flags `truncated` so a follow-up
    /// poll fetches the rest. Walking the cursor returns EVERY turn in order.
    #[test]
    fn page_collected_turns_pages_a_burst_without_loss() {
        let all: Vec<_> = (0..25).map(|i| turn(&format!("t{i}"))).collect();
        // First poll, no cursor, page size 10.
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 10, false);
        assert_eq!(rows.len(), 10);
        assert!(truncated, "25 > 10 ⇒ truncated");
        assert_eq!(rows[0]["turn_id"], "t0", "oldest-first (not the newest 10)");
        assert_eq!(rows[9]["turn_id"], "t9");
        assert_eq!(cursor.as_deref(), Some("t9"), "cursor = boundary turn");

        // Second poll from the boundary.
        let (rows2, cursor2, truncated2) = page_collected_turns(&all, Some("t9"), 10, false);
        assert_eq!(rows2.len(), 10);
        assert!(truncated2);
        assert_eq!(
            rows2[0]["turn_id"], "t10",
            "no gap — resumes right after t9"
        );
        assert_eq!(cursor2.as_deref(), Some("t19"));

        // Third poll drains the remainder.
        let (rows3, _c3, truncated3) = page_collected_turns(&all, Some("t19"), 10, false);
        assert_eq!(rows3.len(), 5);
        assert!(!truncated3, "final page is not truncated");
        assert_eq!(rows3[0]["turn_id"], "t20");
        assert_eq!(rows3[4]["turn_id"], "t24");

        // The three pages reconstruct the full ordered set — zero loss.
        let mut seen: Vec<String> = Vec::new();
        for page in [&rows, &rows2, &rows3] {
            for r in page {
                seen.push(r["turn_id"].as_str().unwrap().to_string());
            }
        }
        let expected: Vec<String> = (0..25).map(|i| format!("t{i}")).collect();
        assert_eq!(seen, expected, "every turn returned exactly once, in order");
    }

    /// A short backlog (≤ `n`) returns everything, `truncated:false`, cursor =
    /// last turn. An unknown cursor returns everything (never silently lose).
    #[test]
    fn page_collected_turns_short_and_unknown_cursor() {
        let all: Vec<_> = (0..3).map(|i| turn(&format!("t{i}"))).collect();
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 20, false);
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
        assert_eq!(cursor.as_deref(), Some("t2"));
        // Unknown cursor → all turns (defensive, no loss).
        let (rows_u, _c, trunc_u) = page_collected_turns(&all, Some("ghost"), 20, false);
        assert_eq!(rows_u.len(), 3);
        assert!(!trunc_u);
    }

    /// v0.9.1 — `tail:true` returns the NEWEST `n` (chronological inside the
    /// page), the "just give me the final answer" shape; cursor = newest turn.
    #[test]
    fn page_collected_turns_tail_returns_newest() {
        let all: Vec<_> = (0..25).map(|i| turn(&format!("t{i}"))).collect();
        let (rows, cursor, truncated) = page_collected_turns(&all, None, 3, true);
        assert_eq!(rows.len(), 3);
        assert!(truncated, "25 > 3 ⇒ truncated");
        assert_eq!(rows[0]["turn_id"], "t22", "newest 3, oldest of them first");
        assert_eq!(rows[2]["turn_id"], "t24", "ends at the newest turn");
        assert_eq!(cursor.as_deref(), Some("t24"));
        // `since` still applies before the tail cut.
        let (rows2, _c, trunc2) = page_collected_turns(&all, Some("t22"), 5, true);
        assert_eq!(rows2.len(), 2, "only t23/t24 exist after t22");
        assert!(!trunc2);
        assert_eq!(rows2[0]["turn_id"], "t23");
    }

    #[test]
    fn collect_max_chars_defaults_and_clamps() {
        assert_eq!(collect_max_chars(&json!({})), 10_000);
        assert_eq!(collect_max_chars(&json!({ "max_chars": 1 })), 500);
        assert_eq!(collect_max_chars(&json!({ "max_chars": -10 })), 500);
        assert_eq!(collect_max_chars(&json!({ "max_chars": 999_999 })), 50_000);
        assert_eq!(collect_max_chars(&json!({ "max_chars": 12_345 })), 12_345);
    }

    #[test]
    fn collect_character_budget_is_total_across_turns() {
        let long = format!("HEAD{}TAIL", "🦀".repeat(900));
        let mut rows = vec![
            json!({ "turn_id": "t1", "content": "short" }),
            json!({ "turn_id": "t2", "content": long }),
        ];
        let (total_chars, truncated) = bound_collected_turns(&mut rows, 500);
        assert_eq!(total_chars, 5 + 908);
        assert!(truncated);
        let returned: usize = rows
            .iter()
            .map(|r| r["content"].as_str().unwrap().chars().count())
            .sum();
        assert_eq!(returned, 500);
        assert_eq!(rows[0]["content"], "short");
        let excerpt = rows[1]["content"].as_str().unwrap();
        assert!(excerpt.starts_with("HEAD"));
        assert!(excerpt.ends_with("TAIL"));
        assert!(excerpt.contains("page with session_collect{sid, since, n}"));
    }

    // ========================================================================
    // v0.9.0 W2 (F2/F7) — dispatch-handler: idempotency, cycle, stop, wait.
    // The handlers are called directly with the `_caller_*` context that
    // `execute_session_tool` injects (so no secret dance).
    // ========================================================================

    /// Inject the server-resolved caller identity `execute_session_tool` sets.
    fn ambient(caller_sid: &str, slug: &str, mut args: serde_json::Value) -> serde_json::Value {
        let o = args.as_object_mut().unwrap();
        o.insert("_caller_sid".into(), json!(caller_sid));
        o.insert("_caller_slug".into(), json!(slug));
        o.insert("_caller_role".into(), json!(""));
        o.insert("_caller_depth".into(), json!(0));
        args
    }

    /// A delegation-wired gateway (fresh stub per spawn — own event stream;
    /// `answer`/`delay_ms` control the wait tests). Returns (handle, principal).
    async fn dispatch_gateway(
        answer: bool,
        delay_ms: u64,
        project_dir: &std::path::Path,
    ) -> (GatewayHandle, String) {
        dispatch_gateway_opts(answer, false, delay_ms, None, project_dir).await
    }

    /// [`dispatch_gateway`] with the narration knob (v0.9.5 wait-boundary test).
    async fn dispatch_gateway_opts(
        answer: bool,
        narrate: bool,
        delay_ms: u64,
        turn_failure: Option<ccteam_harness::ThreadErrorEvent>,
        project_dir: &std::path::Path,
    ) -> (GatewayHandle, String) {
        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(StubAdapter {
                answer,
                turn_failure: turn_failure.clone(),
                narrate,
                event_delay_ms: delay_ms,
                ..Default::default()
            }) as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gw = Gateway::new_with_factory(factory, "alpha", project_dir);
        mark_stub_vendors_installed(&mut gw);
        let (dtx, drx) = tokio::sync::mpsc::unbounded_channel();
        gw.set_delegation_notifier_tx(dtx);
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });
        let principal = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let handle = std::sync::Arc::new(tokio::sync::Mutex::new(gw));
        tokio::spawn(Gateway::run_delegation_notifier(
            std::sync::Arc::clone(&handle),
            drx,
        ));
        (handle, principal)
    }

    fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap()
    }

    #[tokio::test]
    async fn session_spawn_rejects_removed_host_parameter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let args = ambient(&principal, "alpha", json!({"host": "sat-a"}));
        let err = run_session_spawn(&args, &gw, McpCaller::Ambient)
            .await
            .unwrap_err();
        assert_eq!(err, crate::remote_host::HOST_SPAWN_PARAM_REMOVED);
        assert_eq!(gw.lock().await.session_views().len(), 1);
    }

    #[tokio::test]
    async fn session_spawn_reports_vendor_not_installed_from_empty_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gateway, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        gateway
            .lock()
            .await
            .set_local_vendor_availability_for_tests(stub_vendor_availability(false));

        let error = run_session_spawn(
            &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
            &gateway,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("vendor `claude` is not installed on host `local`"),
            "{error}"
        );
        assert!(error.contains("installed there: none"), "{error}");
        assert!(error.contains("observed just now"), "{error}");
        assert!(error.contains("one-click install"), "{error}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_idempotency_replay_returns_same_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let a = ambient(
            &principal,
            "alpha",
            json!({ "idempotency_key": "k1", "vendor": "claude" }),
        );
        let r1 = parse(
            &run_session_spawn(&a, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        let r2 = parse(
            &run_session_spawn(&a, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        assert_eq!(r1["sid"], r2["sid"], "replay returns the original sid");
        assert_eq!(r2["idempotent_replay"], json!(true));
        assert!(
            r1.get("idempotent_replay").is_none(),
            "first is not a replay"
        );
        // Exactly ONE child was created (principal + 1 child = 2 sessions).
        let list = parse(&run_session_list(&serde_json::json!({}), &gw).await.unwrap());
        let children = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["parent_sid"] == json!(principal))
            .count();
        assert_eq!(children, 1, "no double-spawn: {list}");
    }

    /// Two independent MCP session_spawn calls must reach vendor startup at
    /// the same time; only the post-spawn admission seam is serialized.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn session_spawn_fanout_reaches_two_vendor_spawns_concurrently() {
        let tmp = tempfile::TempDir::new().unwrap();
        let barrier = std::sync::Arc::new(StubSpawnBarrier::default());
        let factory: crate::daemon::AdapterFactory = {
            let barrier = std::sync::Arc::clone(&barrier);
            std::sync::Arc::new(move |_, _| {
                std::sync::Arc::new(StubAdapter {
                    spawn_barrier: Some(std::sync::Arc::clone(&barrier)),
                    ..Default::default()
                })
                    as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
            })
        };
        let mut gateway = Gateway::new_with_factory(factory, "alpha", tmp.path());
        mark_stub_vendors_installed(&mut gateway);
        let gateway = std::sync::Arc::new(tokio::sync::Mutex::new(gateway));
        barrier
            .armed
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let spawn = |role: &'static str| {
            let gateway = std::sync::Arc::clone(&gateway);
            tokio::spawn(async move {
                run_session_spawn(
                    &json!({"project": "alpha", "vendor": "claude", "role": role}),
                    &gateway,
                    McpCaller::Admin,
                )
                .await
            })
        };
        let first = spawn("first");
        let second = spawn("second");
        barrier.wait_for(2).await;
        assert_eq!(
            barrier.entered.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "both fan-out branches reached phase-2 vendor startup"
        );
        barrier.release.add_permits(2);
        let first = first.await.unwrap().expect("first spawn succeeds");
        let second = second.await.unwrap().expect("second spawn succeeds");
        assert_ne!(parse(&first)["sid"], parse(&second)["sid"]);
    }

    /// v0.9.5 feedback fix — `session_list` accepts `project`/`activity`/
    /// `limit` filters, caps rows (flagging `truncated` + `total`), slims
    /// null/empty fields out of each row, and rejects a bogus activity value.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_list_filters_limit_and_slim_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        for _ in 0..3 {
            run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        }
        // Unfiltered: principal + 3 children.
        let all = parse(&run_session_list(&json!({}), &gw).await.unwrap());
        assert_eq!(all["total"], json!(4));
        assert_eq!(all["sessions"].as_array().unwrap().len(), 4);
        assert!(all.get("truncated").is_none(), "under the cap: {all}");
        // Slim rows: empty role / absent title / false current are omitted.
        let row = &all["sessions"].as_array().unwrap()[0];
        assert!(row.get("role").is_none(), "empty role omitted: {row}");
        assert!(row.get("status").is_none(), "static status omitted: {row}");

        // limit=2 → truncated + hint, total still 4.
        let capped = parse(&run_session_list(&json!({"limit": 2}), &gw).await.unwrap());
        assert_eq!(capped["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(capped["total"], json!(4));
        assert_eq!(capped["truncated"], json!(true));
        assert!(capped["hint"]
            .as_str()
            .unwrap()
            .contains("most recently active"));

        // project filter: a non-existent slug matches nothing.
        let none = parse(
            &run_session_list(&json!({"project": "nope"}), &gw)
                .await
                .unwrap(),
        );
        assert_eq!(none["total"], json!(0));

        // bogus activity value → readable error.
        let err = run_session_list(&json!({"activity": "busy"}), &gw)
            .await
            .unwrap_err();
        assert!(err.contains("invalid `activity` filter"), "{err}");
    }

    /// A listing that never says which row is the CALLER makes every caller
    /// guess, and the nearest-looking field (`current`) answers a different
    /// question — "the active session of some chat". Measured 2026-08-10: a
    /// caller took the `current` row for itself, and since that other session
    /// ran the same prompt (same title), read its tool calls as its own
    /// identity being used by somebody else. `is_self` follows the caller's
    /// server-resolved principal; `current` follows the fleet's routing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_list_marks_the_calling_session_not_the_current_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let mut children = Vec::new();
        for _ in 0..2 {
            let spawned = parse(
                &run_session_spawn(
                    &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                    &gw,
                    McpCaller::Ambient,
                )
                .await
                .unwrap(),
            );
            children.push(spawned["sid"].as_str().unwrap().to_string());
        }

        let marked = |list: &serde_json::Value| -> Vec<String> {
            list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row.get("is_self") == Some(&json!(true)))
                .map(|row| row["sid"].as_str().unwrap().to_string())
                .collect()
        };

        // The principal asks: exactly its own row is marked.
        let as_principal = parse(
            &run_session_list(&ambient(&principal, "alpha", json!({})), &gw)
                .await
                .unwrap(),
        );
        assert_eq!(
            marked(&as_principal),
            vec![principal.clone()],
            "exactly the caller's row is marked: {as_principal}"
        );

        // The incident's exact shape: a DIFFERENT session is `current`, and
        // being `current` marks nothing.
        let current_rows: Vec<&serde_json::Value> = as_principal["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row.get("current") == Some(&json!(true)))
            .collect();
        let foreign_current = current_rows
            .iter()
            .find(|row| row["sid"] != json!(principal.as_str()))
            .expect("a session other than the caller is some chat's current one");
        assert!(
            foreign_current.get("is_self").is_none(),
            "`current` never marks the caller: {foreign_current}"
        );

        // The marker follows the CALLER, not the fleet: same gateway, same
        // rows, a child asking sees the mark move onto its own row.
        let as_child = parse(
            &run_session_list(&ambient(&children[0], "alpha", json!({})), &gw)
                .await
                .unwrap(),
        );
        assert_eq!(
            marked(&as_child),
            vec![children[0].clone()],
            "the mark is the caller's, not a property of the row: {as_child}"
        );
    }

    /// An admin / local caller is not a session: it has no sid, so no row is
    /// its own. Marking the nearest candidate would be a guess, and a guessed
    /// identity is exactly the failure this field exists to end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_list_marks_nothing_when_the_caller_has_no_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        run_session_spawn(
            &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();

        for args in [json!({}), json!({ "_caller_sid": "" })] {
            let list = parse(&run_session_list(&args, &gw).await.unwrap());
            let rows = list["sessions"].as_array().unwrap();
            assert_eq!(rows.len(), 2, "both sessions listed: {list}");
            assert!(
                rows.iter().all(|row| row.get("is_self").is_none()),
                "a sid-less caller owns no row: {list}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_list_surfaces_requested_model_and_omits_vendor_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let spawned = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({"vendor": "claude", "model": "future-model-verbatim"}),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        let child_sid = spawned["sid"].as_str().unwrap();
        ccteam_harness::execution::turns_mirror::append_turn(
            tmp.path(),
            child_sid,
            &ccteam_harness::execution::turns_mirror::TurnRecord {
                turn_id: format!("{child_sid}-1"),
                ts: chrono::Utc::now(),
                vendor: "claude".into(),
                role: String::new(),
                user: String::new(),
                assistant: "done".into(),
                usage: serde_json::Value::Null,
                status: Some(ccteam_harness::TurnStatus {
                    model: Some("future-model-verbatim".into()),
                    context: Some(ccteam_harness::ContextUsage::known(
                        19,
                        100,
                        ccteam_harness::ContextSource::Reported,
                    )),
                    turn: 1,
                    cost_usd: None,
                    tokens_total: Some(123),
                }),
                tool_calls: Vec::new(),
                attachments: Vec::new(),
                outcome: None,
                error_kind: None,
                error: None,
            },
        )
        .unwrap();

        let list = parse(&run_session_list(&json!({}), &gw).await.unwrap());
        let child = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sid"] == child_sid)
            .unwrap();
        assert_eq!(child["model"], json!("future-model-verbatim"));
        assert_eq!(child["context_pct"], json!(19));
        let parent = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sid"] == principal)
            .unwrap();
        assert!(
            parent.get("model").is_none(),
            "vendor default is omitted: {parent}"
        );
    }

    /// v0.9.5 feedback fix — a title-less `session_spawn{task}` derives a
    /// short display label from the task's first line (ledger only), and the
    /// `notify` arg accepts the mode strings while rejecting garbage.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_derives_title_from_task_and_notify_modes_parse() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let long_line = format!("Refactor the harness layer {}", "x".repeat(80));
        run_session_spawn(
            &ambient(
                &principal,
                "alpha",
                json!({ "vendor": "claude", "task": format!("{long_line}\nsecond line"), "notify": "off" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let list = parse(&run_session_list(&json!({}), &gw).await.unwrap());
        let title = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|s| s.get("title").and_then(|t| t.as_str().map(String::from)))
            .expect("spawned child carries a derived title");
        assert!(title.starts_with("Refactor the harness layer"));
        assert_eq!(title.chars().count(), 60, "capped at 60 chars: {title}");
        assert!(title.ends_with('…'));

        // Explicit titles are never overridden by the derivation.
        run_session_spawn(
            &ambient(
                &principal,
                "alpha",
                json!({ "vendor": "claude", "task": "some task", "title": "my label", "notify": "all" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let list = parse(&run_session_list(&json!({}), &gw).await.unwrap());
        assert!(
            list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s.get("title").and_then(|t| t.as_str()) == Some("my label")),
            "explicit title wins: {list}"
        );

        // Garbage notify → readable error, no spawn side effect.
        let before = gw.lock().await.session_views().len();
        let err = run_session_spawn(
            &ambient(
                &principal,
                "alpha",
                json!({ "vendor": "claude", "task": "t", "notify": "sometimes" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(err.contains("invalid notify mode"), "{err}");
        assert_eq!(gw.lock().await.session_views().len(), before);
    }

    /// v0.9.1 — `session_spawn{task}`: one call spawns AND dispatches (the
    /// dominant flow). The response merges the dispatch outcome (`turn_id` +
    /// `status:dispatched`) into the spawn body, and the delegation lineage
    /// is intact (`parent_sid` = the caller).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_with_task_dispatches_in_one_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let r = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "vendor": "claude", "task": "do the thing" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["status"], json!("dispatched"), "dispatch merged: {r}");
        assert!(
            r["turn_id"].as_str().is_some_and(|t| !t.is_empty()),
            "turn_id present: {r}"
        );
        assert_eq!(r["parent_sid"], json!(principal));
        assert_eq!(r["notify_deliverable"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_fallback_async_calls_report_notification_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, tmp.path()).await;

        let spawned = parse(
            &run_session_spawn(
                &json!({
                    "project": "alpha",
                    "vendor": "claude",
                    "task": "first task"
                }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        let spawned_sid = spawned["sid"].as_str().unwrap();
        assert_eq!(spawned["caller"], "admin");
        assert_eq!(spawned["parent_sid"], serde_json::Value::Null);
        assert_eq!(spawned["notify_deliverable"], false);
        let hint = spawned["hint"].as_str().unwrap();
        assert!(hint.contains("poll session_collect{sid}"), "{hint}");
        assert!(!hint.contains("you will be notified"), "{hint}");
        assert!(
            ccteam_harness::read_delegation_watch(tmp.path(), spawned_sid).is_none(),
            "an admin fallback caller has no parent watch"
        );

        let child = parse(
            &run_session_spawn(
                &json!({ "project": "alpha", "vendor": "claude" }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let dispatched = parse(
            &run_session_dispatch(
                &json!({ "sid": child, "task": "follow-up" }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert_eq!(dispatched["notify_deliverable"], false);
        let hint = dispatched["hint"].as_str().unwrap();
        assert!(hint.contains("poll session_collect{sid}"), "{hint}");
        assert!(!hint.contains("you will be notified"), "{hint}");
        assert!(ccteam_harness::read_delegation_watch(tmp.path(), &child).is_none());

        let off_child = parse(
            &run_session_spawn(
                &json!({ "project": "alpha", "vendor": "claude" }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let off = parse(
            &run_session_dispatch(
                &json!({ "sid": off_child, "task": "ledger only", "notify": "off" }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert_eq!(off["notify_deliverable"], false);
        let hint = off["hint"].as_str().unwrap();
        assert!(hint.contains("notifications are disabled"), "{hint}");
        assert!(!hint.contains("you will be notified"), "{hint}");
    }

    /// v0.9.1 — `session_spawn{task, wait_seconds}` with an answering child
    /// returns the answer inline (`status:completed`, `result_text`), exactly
    /// like session_dispatch's wait path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn spawn_with_task_and_wait_returns_inline_result() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let task = format!("HEAD{}TAIL", "🦀".repeat(12_000));
        let r = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "task": task, "wait_seconds": 6 }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["status"], json!("completed"), "inline: {r}");
        let result = r["result_text"].as_str().unwrap();
        assert_eq!(result.chars().count(), INLINE_RESULT_MAX_CHARS);
        assert!(result.starts_with("echo: HEAD"));
        assert!(result.ends_with("TAIL"));
        assert!(result.contains("…[+"));
        assert!(result.contains("session_collect{sid:"));
        assert!(
            r.get("status_line").is_none(),
            "spawn MCP envelope has no text status: {r}"
        );
        assert_eq!(r["context_pct"], 19, "{r}");
        assert!(r.get("model").is_none(), "{r}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn inline_wait_and_collect_surface_terminal_failure_outcome() {
        const CAPACITY_ERROR: &str = "Selected model is at capacity. Please try a different model.";
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway_opts(
            true,
            false,
            10,
            Some(ccteam_harness::ThreadErrorEvent {
                kind: "server_overloaded".into(),
                message: CAPACITY_ERROR.into(),
            }),
            tmp.path(),
        )
        .await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "codex" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();

        let waited = parse(
            &run_session_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({
                        "sid": child,
                        "task": "run the task",
                        "wait_seconds": 6,
                        "notify": "off"
                    }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(waited["status"], "failed", "{waited}");
        assert_eq!(waited["error_kind"], "server_overloaded");
        assert_eq!(waited["error"], CAPACITY_ERROR);
        assert_eq!(waited["result_text"], CAPACITY_ERROR);

        let collected = parse(
            &run_session_collect(
                &ambient(&principal, "alpha", json!({ "sid": child, "tail": true })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(collected["turns"][0]["outcome"], "failed");
        assert_eq!(collected["turns"][0]["error_kind"], "server_overloaded");
        assert_eq!(collected["turns"][0]["error"], CAPACITY_ERROR);
        assert!(
            collected.get("status_line").is_none(),
            "collect MCP envelope has no text status: {collected}"
        );
        assert_eq!(collected["context_pct"], 19, "{collected}");
        assert_eq!(collected["model"], "stub-model", "{collected}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_response_reports_total_chars_and_honest_truncation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "vendor": "claude", "model": "collect-model" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let answer = format!("HEAD{}TAIL", "界".repeat(1_200));
        ccteam_harness::execution::turns_mirror::append_turn(
            tmp.path(),
            &child,
            &ccteam_harness::execution::turns_mirror::TurnRecord {
                turn_id: "answer-1".into(),
                ts: chrono::Utc::now(),
                vendor: "claude".into(),
                role: String::new(),
                user: "question".into(),
                assistant: answer,
                usage: serde_json::Value::Null,
                status: None,
                tool_calls: Vec::new(),
                attachments: Vec::new(),
                outcome: None,
                error_kind: None,
                error: None,
            },
        )
        .unwrap();

        let response = parse(
            &run_session_collect(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": child, "max_chars": 500 }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(response["total_chars"], 1_208);
        assert_eq!(response["truncated"], true);
        assert_eq!(response["model"], "collect-model");
        let content = response["turns"][0]["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), 500);
        assert!(content.starts_with("HEAD"));
        assert!(content.ends_with("TAIL"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_idempotency_replay_returns_same_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let d = ambient(
            &principal,
            "alpha",
            json!({ "sid": child, "task": "go", "idempotency_key": "d1" }),
        );
        let t1 = parse(
            &run_session_dispatch(&d, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        let t2 = parse(
            &run_session_dispatch(&d, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        assert_eq!(t1["turn_id"], t2["turn_id"], "replay returns the same turn");
        assert_eq!(t2["idempotent_replay"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_cycle_self_and_ancestor_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        // self-dispatch.
        let e = run_session_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": principal, "task": "x" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e.contains("itself"), "self cycle: {e}");
        // child dispatching to its ancestor (principal).
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let e2 = run_session_dispatch(
            &ambient(&child, "alpha", json!({ "sid": principal, "task": "x" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e2.contains("ancestor"), "ancestor cycle: {e2}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_descendant_ok_nondescendant_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // A sibling root (not a descendant of principal).
        let sibling = {
            let mut g = gw.lock().await;
            g.create_session_api(
                "alpha".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid
        };
        let e = run_session_stop(
            &ambient(&principal, "alpha", json!({ "sid": sibling })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e.contains("not a descendant"), "non-descendant stop: {e}");
        // The real descendant stops fine.
        let ok = run_session_stop(
            &ambient(&principal, "alpha", json!({ "sid": child })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        assert_eq!(parse(&ok)["stopped"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_inline_completed_and_timeout_pending() {
        // inline completed (child answers immediately).
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let task = format!("HEAD{}TAIL", "界".repeat(12_000));
        let r = parse(
            &run_session_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": child, "task": task, "wait_seconds": 6 }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["status"], json!("completed"), "inline: {r}");
        let result = r["result_text"].as_str().unwrap();
        assert_eq!(result.chars().count(), INLINE_RESULT_MAX_CHARS);
        assert!(result.starts_with("echo: HEAD"));
        assert!(result.ends_with("TAIL"));
        assert!(result.contains("…[+"));
        assert!(
            r.get("status_line").is_none(),
            "dispatch MCP envelope has no text status: {r}"
        );
        assert_eq!(r["context_pct"], 19, "{r}");
        assert!(r.get("model").is_none(), "{r}");

        // timeout pending (child's answer is delayed past the wait).
        let tmp2 = tempfile::TempDir::new().unwrap();
        let (gw2, p2) = dispatch_gateway(true, 10_000, tmp2.path()).await;
        let child2 = parse(
            &run_session_spawn(
                &ambient(&p2, "alpha", json!({ "vendor": "claude" })),
                &gw2,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let r2 = parse(
            &run_session_dispatch(
                &ambient(
                    &p2,
                    "alpha",
                    json!({ "sid": child2, "task": "go", "wait_seconds": 1 }),
                ),
                &gw2,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r2["status"], json!("pending"), "timeout: {r2}");
        assert!(r2.get("requested_wait_seconds").is_none(), "{r2}");
        assert!(r2.get("effective_wait_seconds").is_none(), "{r2}");
        assert!(
            gw2.lock().await.session_turn_in_flight(&child2),
            "an inline timeout must not cancel the child turn"
        );
    }

    /// v0.9.5 feedback fix — a `wait_seconds` dispatch to a NARRATING child
    /// (codex posts interim messages inside one running turn) must NOT return
    /// on the first interim frame: it completes at the turn boundary with the
    /// FINAL answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_skips_interim_narration_and_returns_final_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway_opts(true, true, 0, None, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "codex" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // `notify:"off"` — the wait itself must not depend on the watch, and
        // it keeps this leg's boundary from racing a notification into the
        // async leg's assertion below.
        let r = parse(
            &run_session_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": child, "task": "do the wave", "wait_seconds": 6, "notify": "off" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r["status"], json!("completed"), "narrated: {r}");
        let result = r["result_text"].as_str().unwrap();
        assert!(
            result.contains("echo: do the wave"),
            "wait returns the FINAL answer, not the interim note: {result}"
        );
        assert!(
            !result.contains("interim narration checkpoint"),
            "the interim note must not be mistaken for the result: {result}"
        );

        // Async leg (notify path) on a FRESH narrating child: exactly ONE
        // notification at the turn boundary — idle-marked, folding the
        // interim note — proving the pump's per-turn fold end-to-end. (A
        // fresh child keeps this assertion independent of the wait leg's
        // already-consumed watch.)
        let child2 = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "codex" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        run_session_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": child2, "task": "second wave" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let project_dir = {
            let g = gw.lock().await;
            g.session_resolve(&principal).unwrap().project_dir
        };
        let mut notes = vec![];
        for _ in 0..200 {
            notes =
                ccteam_harness::execution::turns_mirror::read_all_turns(&project_dir, &principal)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| t.user.contains(" done · turn "))
                    .collect();
            if !notes.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(notes.len(), 1, "one boundary notification, no flood");
        assert!(notes[0].user.contains(" done · turn "), "{}", notes[0].user);
        assert!(notes[0].user.contains("echo: second wave"));
    }

    /// LOCK DISCIPLINE: the gateway lock is acquirable while a dispatch `wait`
    /// is parked (the wait awaits OFF the lock).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_does_not_hold_gateway_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 3_000, tmp.path()).await;
        let child = parse(
            &run_session_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // Park a dispatch wait in a task.
        let gw_w = std::sync::Arc::clone(&gw);
        let d = ambient(
            &principal,
            "alpha",
            json!({ "sid": child, "task": "go", "wait_seconds": 5 }),
        );
        let waiter =
            tokio::spawn(async move { run_session_dispatch(&d, &gw_w, McpCaller::Ambient).await });
        // Give the wait time to submit + park.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        // The gateway lock must be acquirable NOW (the wait is off-lock).
        let locked = tokio::time::timeout(std::time::Duration::from_millis(500), gw.lock()).await;
        assert!(
            locked.is_ok(),
            "gateway lock must be free while a dispatch wait is parked"
        );
        drop(locked);
        let _ = waiter.await;
    }

    // ========================================================================
    // External ledger nodes: a hand-started client that enrolled over `POST
    // /mcp` is a delegation PARENT ccteam holds no thread for. It can be
    // spawned FROM and never driven.
    // ========================================================================

    /// The enrollment a hand-started client gets over `POST /mcp` (a real sid +
    /// `meta.json`, deliberately no live-map row).
    async fn enroll_external_node(gateway: &GatewayHandle, slug: &str) -> String {
        gateway
            .lock()
            .await
            .register_external_node(slug, "user:web-api", "codex/0.144.3")
            .unwrap()
    }

    /// The ask bus is for ccteam's OWN sessions. Enrollment made "Ambient" stop
    /// meaning that — a hand-started agent now arrives Ambient too — so the
    /// refusal reads the ledger instead of the tier. Without this, an outside
    /// process could raise a prompt in the operator's IM indistinguishable from
    /// one a managed session raised on a blocked tool call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_ask_bus_refuses_an_external_node_but_serves_a_managed_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let node = enroll_external_node(&gw, "alpha").await;
        let dispatch = McpDispatch {
            paths: CcteamPaths {
                root: tmp.path().to_path_buf(),
                projects_root: tmp.path().join("projects"),
            },
            sink: None,
            pending: None,
            gateway: Some(std::sync::Arc::clone(&gw)),
        };

        for method in ["interaction/ask", "permission/ask"] {
            let from_node = dispatch
                .dispatch_as(
                    json!({"jsonrpc":"2.0","id":1,"method":method,
                           "params":{"arguments":{"_caller_sid": node}}}),
                    McpCaller::Ambient,
                )
                .await
                .expect("a request gets a response");
            assert!(
                from_node["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("not available on this transport")),
                "{method} from an external node must be refused: {from_node}"
            );

            // A managed session's own principal still reaches the bus — the
            // refusal must narrow to the new caller class, not close the door
            // HITL depends on.
            let from_managed = dispatch
                .dispatch_as(
                    json!({"jsonrpc":"2.0","id":2,"method":method,
                           "params":{"arguments":{"_caller_sid": principal}}}),
                    McpCaller::Ambient,
                )
                .await
                .expect("a request gets a response");
            let refused = from_managed["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("not available on this transport"));
            assert!(!refused, "{method} from a managed session: {from_managed}");
        }
    }

    /// All three driving tools refuse an external sid with the ONE shared
    /// message, and it says what the session IS. "not found" would be a claim
    /// the caller can immediately disprove — the sid is right there in
    /// `session_list` — so it would read as a ccteam bug instead of an answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn driving_tools_refuse_an_external_node_by_saying_what_it_is() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let node = enroll_external_node(&gw, "alpha").await;

        // The premise of the whole requirement: the caller can see it.
        let listed = parse(&run_session_list(&json!({}), &gw).await.unwrap());
        assert!(
            listed["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["sid"] == json!(node)),
            "the node is visible in session_list: {listed}"
        );

        let dispatch = run_session_dispatch(
            &ambient(&principal, "alpha", json!({ "sid": node, "task": "do it" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        let collect = run_session_collect(
            &ambient(&principal, "alpha", json!({ "sid": node })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        let stop = run_session_stop(
            &ambient(&principal, "alpha", json!({ "sid": node })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        for (tool, message) in [
            ("session_dispatch", &dispatch),
            ("session_collect", &collect),
            ("session_stop", &stop),
        ] {
            assert_eq!(
                message,
                &crate::external_nodes::not_driveable_error(tool, &node),
                "{tool}: one shared refusal, named per tool"
            );
            assert!(
                message.contains("delegation parent"),
                "{tool}: says what it can still do: {message}"
            );
            assert!(!message.contains("not found"), "{tool}: {message}");
            assert!(!message.contains("unknown session"), "{tool}: {message}");
        }

        // The ledger row is the reason, not the caller's auth tier.
        let admin = run_session_dispatch(
            &json!({ "sid": node, "task": "do it" }),
            &gw,
            McpCaller::Admin,
        )
        .await
        .unwrap_err();
        assert_eq!(
            admin,
            crate::external_nodes::not_driveable_error("session_dispatch", &node)
        );
        // A refused stop is a no-op: the node keeps its place in the ledger.
        assert!(gw.lock().await.is_external_node(&node));
    }

    /// The point of minting the node: a child spawned by a hand-started agent
    /// hangs UNDER it instead of mounting as a root. An admin-tier caller (what
    /// such a client authenticates as) declares its enrolled sid, which is
    /// validated against `session_views()` — external rows included. The
    /// guardrails then behave: a non-live parent is a legitimate depth-0 root
    /// whose LIVE children are what the fan-out ceiling counts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_declaring_an_external_parent_nests_the_child_at_depth_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let node = enroll_external_node(&gw, "alpha").await;

        let child = parse(
            &run_session_spawn(
                &json!({ "project": "alpha", "vendor": "claude", "parent_sid": node }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert_eq!(child["parent_sid"], json!(node), "{child}");
        assert_eq!(child["delegation_depth"], json!(1), "{child}");
        assert_eq!(
            child["caller"],
            json!(format!("admin:{node}")),
            "the echo names the resolved origin: {child}"
        );

        // Fan-out is counted from the live map, keyed on the parent sid — the
        // node's own absence from that map contributes nothing, so the ceiling
        // is enforced rather than silently passed.
        gw.lock()
            .await
            .set_delegation_config(ccteam_core::DelegationConfig {
                max_depth: 2,
                max_children: 1,
                max_delegated: 50,
            });
        let denied = run_session_spawn(
            &json!({ "project": "alpha", "vendor": "claude", "parent_sid": node }),
            &gw,
            McpCaller::Admin,
        )
        .await
        .unwrap_err();
        assert!(denied.contains("fan-out limit reached"), "{denied}");
        assert!(denied.contains("already has 1 active children"), "{denied}");

        // Acceptance comes from the ledger, never from faith in the declaration.
        let unknown = run_session_spawn(
            &json!({ "project": "alpha", "vendor": "claude", "parent_sid": "s999999" }),
            &gw,
            McpCaller::Admin,
        )
        .await
        .unwrap_err();
        assert!(unknown.contains("not a live session"), "{unknown}");
    }

    /// Honest notifications. MCP is client-dial-in: ccteam cannot inject a
    /// completion turn into a hand-started agent's conversation, so a task
    /// delegated from an external parent must SAY the notification will not
    /// come. Both sides are asserted — the distinction is the contract, not
    /// either half on its own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_task_is_notify_deliverable_only_under_a_managed_parent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let node = enroll_external_node(&gw, "alpha").await;

        let external = parse(
            &run_session_spawn(
                &json!({
                    "project": "alpha",
                    "vendor": "claude",
                    "parent_sid": node,
                    "task": "review the diff"
                }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            external["parent_sid"],
            json!(node),
            "the delegation edge is real: {external}"
        );
        assert_eq!(external["notify_deliverable"], json!(false), "{external}");
        let hint = external["hint"].as_str().unwrap();
        assert!(hint.contains("poll session_collect{sid}"), "{hint}");
        assert!(!hint.contains("you will be notified"), "{hint}");
        // The armed watch agrees with the answer we just gave: the edge stays
        // watched (so the child's completion keeps hitting the ledger) with no
        // impossible delivery armed on it.
        let watch =
            ccteam_harness::read_delegation_watch(tmp.path(), external["sid"].as_str().unwrap())
                .expect("the delegation edge is still watched");
        assert_eq!(watch.parent_sid, node);
        assert_eq!(watch.notify, ccteam_harness::NotifyMode::Off, "{watch:?}");

        // A managed parent has a transport, and keeps it.
        let managed = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "vendor": "claude", "task": "review the diff" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(managed["notify_deliverable"], json!(true), "{managed}");
        assert!(
            managed["hint"]
                .as_str()
                .unwrap()
                .contains("you will be notified"),
            "{managed}"
        );
        let watch =
            ccteam_harness::read_delegation_watch(tmp.path(), managed["sid"].as_str().unwrap())
                .unwrap();
        assert_eq!(watch.parent_sid, principal);
        assert_eq!(watch.notify, ccteam_harness::NotifyMode::Final, "{watch:?}");
    }

    /// v0.10.1 (issue #184) — a dispatch to a session the caller never
    /// delegated is a HANDOFF, not a delegation: the target has its own parent,
    /// or is a root with its own human. The default `notify` is a default, not a
    /// request, so it must not subscribe the caller to a peer's conversation —
    /// an edge `session_list` never draws and `session_stop` refuses to take
    /// down. Naming `notify` explicitly still opts in (for exactly one task, per
    /// the watch contract), and the caller's own children are untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_to_a_peer_root_is_ledger_only_unless_notify_is_explicit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        // An independent root in the same project — nobody's child.
        let peer = {
            let mut g = gw.lock().await;
            g.create_session_api(
                "alpha".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid
        };

        let handoff = parse(
            &run_session_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": peer, "task": "take over the P0" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(handoff["notify_deliverable"], json!(false), "{handoff}");
        let hint = handoff["hint"].as_str().unwrap();
        assert!(hint.contains("did not delegate"), "{hint}");
        assert!(hint.contains("poll session_collect{sid}"), "{hint}");
        let watch = ccteam_harness::read_delegation_watch(tmp.path(), &peer)
            .expect("the handoff edge is still recorded in the ledger");
        assert_eq!(watch.parent_sid, principal);
        assert_eq!(
            watch.notify,
            ccteam_harness::NotifyMode::Off,
            "a peer handoff is ledger-only: {watch:?}"
        );

        // Explicit opt-in still arms the notification.
        let explicit = parse(
            &run_session_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": peer, "task": "and tell me when it lands", "notify": "final" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(explicit["notify_deliverable"], json!(true), "{explicit}");
        let watch = ccteam_harness::read_delegation_watch(tmp.path(), &peer).unwrap();
        assert_eq!(watch.notify, ccteam_harness::NotifyMode::Final, "{watch:?}");

        // The caller's OWN child keeps the default notification.
        let child = parse(
            &run_session_spawn(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "vendor": "claude", "task": "do the work" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(child["notify_deliverable"], json!(true), "{child}");
        let watch =
            ccteam_harness::read_delegation_watch(tmp.path(), child["sid"].as_str().unwrap())
                .unwrap();
        assert_eq!(watch.notify, ccteam_harness::NotifyMode::Final, "{watch:?}");
    }

    /// The refusal sits BEHIND the ACL, not in front of it: a tenant who can see
    /// the node reaches the honest message (the sid→project resolver knows both
    /// indexes), while another tenant's node stays indistinguishable from an
    /// unknown sid.
    #[tokio::test]
    async fn tenant_gets_the_refusal_only_for_a_node_in_its_own_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let (own, foreign) = {
            let mut gw = gateway.lock().await;
            (
                gw.register_external_node("alice", "user:ualice", "codex/0.144.3")
                    .unwrap(),
                gw.register_external_node("bob", "user:ubob", "codex/0.144.3")
                    .unwrap(),
            )
        };
        let caller = McpCaller::User {
            user_id: "ualice".into(),
        };
        for tool in ["session_dispatch", "session_collect", "session_stop"] {
            let invoke = |sid: &str| {
                let mut args = json!({ "sid": sid });
                if tool == "session_dispatch" {
                    args["task"] = json!("do not leak");
                }
                call(tool, args)
            };
            let mine = execute_session_tool_with_paths(
                &invoke(&own),
                Some(&gateway),
                caller.clone(),
                &paths,
            )
            .await;
            assert_eq!(mine["result"]["isError"], true, "{tool}: {mine}");
            assert_eq!(
                mine["result"]["content"][0]["text"],
                json!(crate::external_nodes::not_driveable_error(tool, &own)),
                "{tool}: {mine}"
            );
            let theirs = execute_session_tool_with_paths(
                &invoke(&foreign),
                Some(&gateway),
                caller.clone(),
                &paths,
            )
            .await;
            let unknown = execute_session_tool_with_paths(
                &invoke("s999999"),
                Some(&gateway),
                caller.clone(),
                &paths,
            )
            .await;
            assert_eq!(
                theirs["result"]["content"][0]["text"], unknown["result"]["content"][0]["text"],
                "{tool}: another tenant's node must stay indistinguishable from an unknown sid"
            );
        }
    }
}
