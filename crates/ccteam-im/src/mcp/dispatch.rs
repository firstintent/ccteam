//! Daemon-side MCP dispatch: stateful intercepts + protocol-core fallback.
//!
//! Owns the live gateway / pending registry / event sink needed for
//! `interaction/ask`, `permission/ask`, `chat_send_file`, `session_*`, and
//! `ccteam/reload`. Both transports on top stay thin: the local `mcp.sock` loop
//! does read-line → [`McpDispatch::dispatch`] → write-line, and ccteam-web's
//! `POST /mcp` resolves the caller's credential, then calls
//! [`McpDispatch::dispatch_as`] with the tier it proved.

use std::path::PathBuf;
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
            // Discovery answers a face composed for THIS caller (leaf sessions
            // pay a fraction of an orchestrator's ambient bill); everything
            // else falls through to the protocol core unchanged.
            let face = match req.get("method").and_then(|m| m.as_str()) {
                Some("initialize") | Some("tools/list") => {
                    super::face::resolve_tool_face(
                        &req,
                        self.gateway.as_ref(),
                        &caller,
                        &self.paths,
                    )
                    .await
                }
                _ => super::face::ToolFace::full(),
            };
            protocol::handle_request(&self.paths, &req, &face).await
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
/// one. Used as the unambiguous default for an admin `agent` that
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

/// Default roster page size. FIVE: a caller asks the roster who is working
/// for it right now, and that is a handful of children — a longer page spends
/// its context on rows it did not ask about (measured on a planner reading
/// 25 rows: 38% of the metadata bytes were the titles of historical sessions
/// it had no decision to make about). More is an `n` away, and `total` +
/// `truncated` always say when the cap bit.
const AGENT_READ_DEFAULT_N: usize = 5;
/// Default turns a transcript read returns. ONE, not the roster's five: the
/// overwhelmingly common question a `sid` read asks is "what did it answer",
/// and that is the newest turn. Ten turns is transcript replay — a rarer need,
/// and one the caller says out loud. Ten of them sharing one character budget
/// was measured at 73% pointer / 27% content (issue #195); `remaining` and
/// `latest` say when there is more.
const AGENT_READ_TRANSCRIPT_DEFAULT_N: usize = 1;
/// How many delegation request rows a transcript read carries. Outstanding
/// work first, so the answer to "what does this child still owe me" is never
/// pushed off by resolved history; bounded so a busy child cannot flood the
/// reader's context with bookkeeping.
const AGENT_READ_REQUEST_ROWS: usize = 10;
/// Below this a returned turn carries more pointer than prose, so the page
/// drops whole rows (counted in `remaining`) instead of shredding every one.
const MIN_USEFUL_ROW_CHARS: usize = 200;
use crate::delegation::{DelegationOutcome, DelegationSummary};
/// Default character budget across the turns one `agent_read{sid}` returns.
use crate::delegation::{
    AGENT_READ_DEFAULT_MAX_CHARS, AGENT_READ_MAX_MAX_CHARS, AGENT_READ_MIN_MAX_CHARS,
};

/// Keep inline waits below the shortest common MCP client deadline (~300s),
/// leaving 60s for spawn/submit work around the wait itself. Requests above it
/// clamp (and still answer an honest `pending` on timeout).
const INLINE_WAIT_CEILING_SECONDS: u64 = 240;

/// The `agent{wait}` window, clamped to what the transport can survive.
fn inline_wait_seconds(args: &serde_json::Value) -> u64 {
    args.get("wait")
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
        .min(INLINE_WAIT_CEILING_SECONDS)
}

/// The `agent_read{sid,wait}` window: the same clamp, and zero for the roster
/// branch — a listing has no turn to wait for, so the parameter is ignored
/// there rather than refused (the two branches never error on each other's
/// filters).
fn read_wait_seconds(args: &serde_json::Value) -> u64 {
    if !addresses_a_session(args) {
        return 0;
    }
    inline_wait_seconds(args)
}

/// True when `args` name a session (`sid` present and non-blank) — the one
/// question `agent` and `agent_read` both branch on.
fn addresses_a_session(args: &serde_json::Value) -> bool {
    args.get("sid")
        .and_then(|sid| sid.as_str())
        .is_some_and(|sid| !sid.trim().is_empty())
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
    // discipline §7-1, mirroring run_agent_read_transcript). `None` here means no
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
        exec_turn_id: None,
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
        conclusion: None,
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

/// True for a `tools/call` naming one of the session tools (`agent`,
/// `agent_read`, `agent_stop`). Asks the ONE membership predicate rather than
/// matching a name shape, so a future tool joins the group by being listed.
fn is_session_tool_call(req: &serde_json::Value) -> bool {
    req.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && req
            .pointer("/params/name")
            .and_then(|n| n.as_str())
            .is_some_and(protocol::is_session_tool)
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

/// Daemon-aware `status`: which agents the caller's project can hire and what
/// the team spent, tiered by `detail`.
///
/// Ambient (session principal, which is every `POST /mcp` caller) is scoped to
/// its OWN project — any self-reported `project`/`_caller_slug` is ignored (the
/// body would otherwise leak another project's host). Admin (the local
/// mcp.sock admin-token tier) may name a `project`, else falls back to a
/// supplied `_caller_slug`, else answers fleet-wide. Gathering probes + reads
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

    // The bare-name beacon is the brief body byte-for-byte: it exists for
    // hosts that surface tool NAMES only, and a second shape would be a second
    // thing to keep true.
    let is_beacon = req.pointer("/params/name").and_then(|n| n.as_str())
        == Some(protocol::STATUS_BEACON_TOOL_NAME);
    let detail = if is_beacon {
        super::vendor_panel::StatusDetail::Brief
    } else {
        match super::vendor_panel::StatusDetail::parse(args.get("detail").and_then(|v| v.as_str()))
        {
            Ok(detail) => detail,
            Err(message) => return session_tool_response(id, message, true),
        }
    };

    // The caller's OWN session, when it has one (Ambient only). Drives the
    // `you` row of the usage tier — an Admin/tenant token is not a session and
    // has no context window to report.
    let mut caller_sid: Option<String> = None;
    let mut body = if let Some(user_id) = caller.user_id() {
        match user_status_body(&args, paths, user_id, detail).await {
            Ok(body) => body,
            Err(message) => return session_tool_response(id, message, true),
        }
    } else {
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
        caller_sid = ctx.as_ref().map(|ctx| ctx.sid.clone());
        let project = match super::vendor_panel::resolve_status_project(
            caller,
            args.get("project").and_then(|v| v.as_str()),
            args.get("_caller_slug").and_then(|v| v.as_str()),
            ctx.as_ref(),
        ) {
            Ok(p) => p,
            Err(note) => {
                // Ambient caller with no project scope: the fleet ledger it
                // could already read, plus an honest note — never another
                // project's host, and no account state either. An enrolled
                // binding that has simply not named a workspace is a DIFFERENT
                // fact from a bearer that failed, and the route already told
                // us which one this is by injecting `_enroll_reachable`.
                let note = match enroll_reachable_arg(&args) {
                    Some(reachable) => {
                        super::vendor_panel::enrolled_unbound_status_note(&reachable)
                    }
                    None => note,
                };
                let body = serde_json::json!({
                    "projects": protocol::status_project_rows(paths, |_| true),
                    "note": note,
                });
                return status_response(id, body);
            }
        };
        build_status_body(paths, project.clone(), detail, || {
            protocol::status_project_rows(paths, |_| true)
        })
        .await
    };
    // One decoration point for every authenticated caller shape, so a tier
    // that carries account state cannot silently skip one of them.
    append_usage_sections(&mut body, detail, gateway, caller_sid.as_deref()).await;
    status_response(id, body)
}

/// The `you` + `usage` half of the body, for the tiers that ask for it.
///
/// Two facts, deliberately together: `you.context_pct` says whether THIS
/// session still has room to keep working, and `usage` says which harness
/// accounts still have quota to hire from. A caller deciding "continue here /
/// start fresh / hand it to a different harness" needs both in one call, which
/// is the whole reason this tier exists.
///
/// No probing engine and no polling: the map is
/// [`Gateway::account_usage_snapshot`](crate::gateway::Gateway::account_usage_snapshot),
/// which asks live same-vendor adapters for state they already hold (never a
/// turn) and otherwise reads the recorded observation. A vendor nobody has
/// heard from is absent rather than zeroed.
async fn append_usage_sections(
    body: &mut serde_json::Map<String, serde_json::Value>,
    detail: super::vendor_panel::StatusDetail,
    gateway: Option<&GatewayHandle>,
    caller_sid: Option<&str>,
) {
    if !detail.wants_usage() {
        return;
    }
    let Some(gateway) = gateway else {
        return;
    };
    // Both reads under one guard: the snapshot is async (it may ask a live
    // adapter), the sid resolution is a cheap in-memory lookup.
    let (usage, own_project_dir) = {
        let gw = gateway.lock().await;
        let dir = caller_sid.and_then(|sid| gw.session_resolve_any(sid).map(|r| r.project_dir));
        (gw.account_usage_snapshot(None).await, dir)
    };
    if let Some(sid) = caller_sid {
        let mut you = serde_json::Map::new();
        you.insert("sid".into(), serde_json::json!(sid));
        // Omitted, never zeroed: a session with no turn yet has no measured
        // occupancy, and "0%" would read as "all the room in the world".
        if let Some(pct) = own_project_dir
            .as_deref()
            .and_then(|dir| crate::delegation::latest_context_pct(dir, sid))
        {
            you.insert("context_pct".into(), serde_json::json!(pct));
        }
        body.insert("you".into(), serde_json::Value::Object(you));
    }
    if !usage.is_empty() {
        let rendered: serde_json::Map<String, serde_json::Value> = usage
            .iter()
            .map(|(vendor, entry)| (vendor.clone(), crate::usage_view::vendor_usage_value(entry)))
            .collect();
        body.insert("usage".into(), serde_json::Value::Object(rendered));
    }
}

/// `_enroll_reachable`, injected by `POST /mcp` for an enrolled binding that
/// holds no principal yet: the slugs its credential's owner can name. Absent
/// for every other caller, which is what makes it a reliable discriminator
/// between "not bound yet" and "not authenticated".
fn enroll_reachable_arg(args: &serde_json::Value) -> Option<Vec<String>> {
    let list = args.get("_enroll_reachable")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|slug| slug.as_str().map(str::to_string))
            .collect(),
    )
}

/// Tenant `status` body: the same tiers, scoped by the shared owner policy.
/// `Err` carries the caller-facing refusal text.
async fn user_status_body(
    args: &serde_json::Value,
    paths: &CcteamPaths,
    user_id: &str,
    detail: super::vendor_panel::StatusDetail,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let explicit = args
        .get("project")
        .and_then(|project| project.as_str())
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .map(str::to_string);
    if let Some(project) = explicit.as_deref() {
        if !user_can_see_project(paths, user_id, project) {
            return Err("status: project not found".to_string());
        }
    }
    let rows = || {
        protocol::status_project_rows(paths, |state| {
            ccteam_core::identity::can_see_owner(user_id, false, state.owner.as_deref())
        })
    };
    let mut body = build_status_body(paths, explicit, detail, rows).await;
    if body
        .get("projects")
        .and_then(|projects| projects.as_array())
        .is_some_and(|projects| projects.is_empty())
    {
        body.insert(
            "note".into(),
            serde_json::json!(
                "no projects are visible to this user; project-scoped host, budget and routing details are withheld"
            ),
        );
    }
    Ok(body)
}

/// Build the tiered `status` body for an optional project scope. With a
/// project it is the project shape; without one it is the fleet shape (the
/// projects the caller can see + the local host's hire list).
async fn build_status_body(
    paths: &CcteamPaths,
    project: Option<String>,
    detail: super::vendor_panel::StatusDetail,
    project_rows: impl Fn() -> Vec<serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    // The hub catalog is only ever read into a `models`/`full` body; a brief
    // call must not pay for a network/disk round trip it will not print.
    let hub = if matches!(
        detail,
        super::vendor_panel::StatusDetail::Models | super::vendor_panel::StatusDetail::Full
    ) {
        crate::hub::load_models_catalog(&crate::hub::hub_base(), paths, false).await
    } else {
        crate::hub::HubModelsState::Unavailable
    };
    let paths_owned = paths.clone();
    let slug_owned = project.clone();
    let panel = tokio::task::spawn_blocking(move || {
        super::vendor_panel::gather_status_panel(&paths_owned, slug_owned.as_deref())
    })
    .await;
    let Ok(panel) = panel else {
        let mut body = serde_json::Map::new();
        body.insert(
            "error".into(),
            serde_json::json!("status: probe worker failed"),
        );
        return body;
    };
    let mut body = super::vendor_panel::status_value(&panel, &hub, detail);
    if project.is_none() {
        // Fleet scope: there is no single project, so the per-project facts
        // are replaced by the ledger of the ones the caller can see.
        body.remove("project");
        body.remove("cost_24h_usd");
        body.insert("projects".into(), serde_json::json!(project_rows()));
    }
    if detail == super::vendor_panel::StatusDetail::Full {
        // Operator data lives ONLY here: an agent that just wants to hire
        // should never carry daemon health in its context.
        body.insert(
            "daemon".into(),
            protocol::daemon_health_json(&ccteam_core::check_daemon_health(paths)),
        );
        body.entry("projects")
            .or_insert_with(|| serde_json::json!(project_rows()));
    }
    body
}

/// Serialize a `status` body compactly (no pretty-printer: indentation is
/// 30-45% of the bytes a caller pays for).
fn status_response(id: serde_json::Value, body: impl serde::Serialize) -> serde_json::Value {
    let text = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    session_tool_response(id, text, false)
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
            if let Err(message) = scope_ambient_roster(&name, &mut args, &ctx.slug) {
                return session_tool_response(id, message, true);
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
    let budget = std::time::Duration::from_secs(match name.as_str() {
        "agent" => 60 + inline_wait_seconds(&args),
        "agent_stop" => 30,
        // `agent_read{sid,wait}` deliberately holds the request open until the
        // target's turn ends, so the server deadline has to clear the wait the
        // caller asked for — a flat 15s would cut every long poll short and
        // report a busy daemon instead.
        "agent_read" => 15 + read_wait_seconds(&args),
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
                 check agent_read before assuming it failed.",
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
    // `agent` and `agent_read` each cover two shapes; which gate applies is
    // decided by the same `sid` the tool itself branches on.
    let addresses_a_session = args
        .get("sid")
        .and_then(|sid| sid.as_str())
        .is_some_and(|sid| !sid.trim().is_empty());
    let effective = match (name, addresses_a_session) {
        ("agent", false) => "hire",
        ("agent_read", false) => "roster",
        _ => "drive",
    };
    match effective {
        "hire" => {
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
                            "{name}: missing `project` — tenant MCP callers must name one of their own projects explicitly{}",
                            hint()
                        ));
                    }
                },
            };
            if !user_can_see_project(paths, user_id, &project) {
                return Err(format!("{name}: project not found{}", hint()));
            }
        }
        "drive" => {
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
                    // — a tenant who can see the node in `agent_read` must
                    // reach the honest not-driveable refusal instead of
                    // "session not found", while a node in someone else's
                    // project stays indistinguishable from an unknown sid.
                    .or_else(|| gateway.external_node(sid).map(|meta| meta.slug))
            };
            if project
                .as_deref()
                .is_none_or(|project| !user_can_see_project(paths, user_id, project))
            {
                return Err(tenant_unreachable_session_error(name));
            }
        }
        "roster" => {
            let visible = visible_user_projects(paths, user_id);
            if let Some(project) = args
                .get("project")
                .and_then(|project| project.as_str())
                .map(str::trim)
                .filter(|project| !project.is_empty())
            {
                if !visible.iter().any(|candidate| candidate == project) {
                    return Err(format!(
                        "{name}: project not found{}",
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

/// Clamp an ambient caller's ROSTER to the project its principal is bound to.
///
/// A session addresses one workspace: `agent` spawns into it, every
/// sid-addressed call is checked against it, and naming another one is
/// refused. The roster was the one asymmetry — it spanned every project the
/// project's OWNER can see, so a caller bound to `ccteam-src` was handed
/// `total: 1272` rows with its own seven pushed past the default page
/// (measured 2026-08-31). Naming the caller's own project stays legal (it is a
/// no-op filter); naming another gets the same refusal every other surface
/// gives it.
fn scope_ambient_roster(
    name: &str,
    args: &mut serde_json::Value,
    caller_slug: &str,
) -> std::result::Result<(), String> {
    if name != "agent_read" || addresses_a_session(args) {
        return Ok(());
    }
    if let Some(named) = args
        .get("project")
        .and_then(|project| project.as_str())
        .map(str::trim)
        .filter(|project| !project.is_empty())
    {
        if named != caller_slug {
            return Err(format!(
                "{name}: project not found — this session works in `{caller_slug}`"
            ));
        }
    }
    if let Some(obj) = args.as_object_mut() {
        obj.insert(
            "_caller_visible_projects".to_string(),
            serde_json::json!([caller_slug]),
        );
    }
    Ok(())
}

/// Dispatch one session tool to the gateway. Returns `Ok(body)` (a compact
/// JSON string) on success, `Err(msg)` on a tool-level error.
async fn run_session_tool(
    name: &str,
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
    paths: &CcteamPaths,
) -> std::result::Result<String, String> {
    match name {
        "agent" => run_agent(args, gateway, caller, paths).await,
        "agent_read" => run_agent_read(args, gateway, caller, paths).await,
        "agent_stop" => run_agent_stop(args, gateway, caller).await,
        other => Err(format!("unknown session tool: {other}")),
    }
}

/// Parameters that only make sense when HIRING. Naming one alongside `sid` is
/// a caller that thinks it is configuring a session it is only messaging —
/// refused rather than silently ignored, because "my model setting did
/// nothing" is a far worse bug report than "that parameter is not for this".
const AGENT_SPAWN_ONLY_PARAMS: &[&str] = &[
    "vendor",
    "model",
    "effort",
    "role",
    "mode",
    "permission_mode",
    "tools",
    "parent_sid",
];

/// A `task_file` is capped at what a brief is, not at what a file can be: the
/// text becomes one user turn in someone's context either way.
const TASK_FILE_MAX_BYTES: u64 = 256 * 1024;

/// Fold `task_file` into the `task` the rest of the call sees.
///
/// The task text is the one thing a dispatch must carry, and carrying it
/// INLINE spends it twice in the caller's own context — once building it, once
/// as this argument — for a parent that never reads it back (measured on a
/// planner: 199.7 KB across 43 dispatches, none of it re-read). Reading the
/// file here changes nothing else about the delegation: the same bytes become
/// the same verbatim user turn down the same path, and the daemon reads them
/// as the uid the caller could already read them as, so this adds no reach.
///
/// Honest scope: the path is resolved on the DAEMON's filesystem. For a
/// project bound to a satellite that is the right end anyway — the content
/// crosses the wire, a path never would (`remote_exec`, the satellite is a
/// protocol-blind byte pump) — but a caller whose file lives somewhere else
/// must pass `task` instead.
///
/// Returns the rewritten args when it fired, `None` when there was no
/// `task_file` (the overwhelmingly common call).
fn resolve_task_file(args: &serde_json::Value) -> std::result::Result<Option<Value>, String> {
    let Some(raw) = args.get("task_file") else {
        return Ok(None);
    };
    let path = raw
        .as_str()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "agent: `task_file` must be a non-empty path".to_string())?;
    // Two sources for one field is a question about which one won, and the
    // answer is never visible in the transcript. Refuse instead of picking.
    if args.get("task").is_some() {
        return Err("agent: give `task` or `task_file`, not both".to_string());
    }
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err(format!(
            "agent: `task_file` must be absolute (the daemon's cwd is not yours): `{}`",
            path.display()
        ));
    }
    let len = std::fs::metadata(path)
        .map_err(|error| format!("agent: task_file `{}`: {error}", path.display()))?
        .len();
    if len > TASK_FILE_MAX_BYTES {
        return Err(format!(
            "agent: task_file `{}` is {len} bytes, over the {TASK_FILE_MAX_BYTES} cap — send a brief, not a corpus",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("agent: task_file `{}`: {error}", path.display()))?;
    if text.trim().is_empty() {
        return Err(format!(
            "agent: task_file `{}` is empty — say what the agent should do",
            path.display()
        ));
    }
    let mut owned = args.clone();
    let Some(object) = owned.as_object_mut() else {
        return Err("agent: arguments must be an object".to_string());
    };
    object.insert("task".into(), Value::String(text));
    object.remove("task_file");
    Ok(Some(owned))
}

/// `agent` — hire a new session (`task` alone) or task one you already have
/// (`task` + `sid`). One tool, because the two are the same act with one
/// parameter of difference; they share task, wait, notify, title and
/// idempotency wholesale.
async fn run_agent(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
    paths: &CcteamPaths,
) -> std::result::Result<String, String> {
    // Normalized FIRST, so every branch below — validation, policy facts,
    // spawn-and-dispatch, dispatch-to-sid — sees one `task` and a new one
    // gets it for free.
    let resolved_task_file = resolve_task_file(args)?;
    let args = resolved_task_file.as_ref().unwrap_or(args);
    if args.get("host").is_some() {
        return Err(format!(
            "agent: {}",
            crate::remote_host::HOST_SPAWN_PARAM_REMOVED
        ));
    }
    if args.get("protocol").is_some() {
        return Err(PROTOCOL_SPAWN_PARAM_REMOVED.to_string());
    }
    if args.get("wait_seconds").is_some() {
        return Err("agent: `wait_seconds` was renamed to `wait`".to_string());
    }
    if args
        .get("task")
        .and_then(|task| task.as_str())
        .map(str::trim)
        .is_none_or(str::is_empty)
    {
        return Err(
            "agent: missing `task` — say what the agent should do (or point `task_file` at it)"
                .to_string(),
        );
    }
    let dispatching = addresses_a_session(args);
    if dispatching {
        let named: Vec<&str> = AGENT_SPAWN_ONLY_PARAMS
            .iter()
            .copied()
            .filter(|param| args.get(param).is_some_and(|value| !value.is_null()))
            .collect();
        if !named.is_empty() {
            return Err(format!(
                "agent: `sid` names an existing session — drop {} or omit `sid` to hire",
                named.join("/")
            ));
        }
    }
    // Card H — the user's own pre-flight policy runs here, on a well-formed
    // request and before either branch does anything.
    pre_agent_policy_gate(args, gateway, &caller, paths).await?;
    if dispatching {
        run_agent_dispatch(args, gateway, caller).await
    } else {
        run_agent_spawn_at(args, gateway, caller, Some(paths)).await
    }
}

/// The in-memory facts one `agent` call needs before its policy hook can run:
/// where the target project lives (which decides WHICH hook governs) and the
/// coordinates a refusal is filed under.
struct PolicyGateFacts {
    /// The target project slug — empty when the call names none resolvably.
    slug: String,
    /// Is the target project's working tree on THIS machine? A satellite-bound
    /// project keeps its hooks on the satellite, and the local tree of the same
    /// slug (if any) belongs to a different project — so its project rung is
    /// skipped entirely rather than faked from a same-named local directory.
    local: bool,
    /// That working tree, as the daemon has it mapped.
    project_dir: Option<PathBuf>,
    /// The harness the delegation would spend (hired, or the target's).
    vendor: ccteam_harness::AgentVendor,
    /// The project's bound host, for the refusal event.
    host: String,
    /// The harness of the delegating session, for the payload.
    caller_vendor: String,
    /// The caller's project tree — where its `turns.jsonl` context lives.
    caller_dir: Option<PathBuf>,
    /// The caller's active direct children.
    children: Option<u32>,
    /// Active delegated sessions in the target project.
    delegated: Option<u32>,
    /// The project cost projection, read off-lock.
    projection: Option<Arc<crate::progress_projection::ProgressProjection>>,
}

/// Card H — run the user-programmable `pre-agent` policy for one `agent` call.
///
/// WHY HERE: [`run_agent`] is the single door both delegation forms come
/// through, and the last point at which NOTHING has happened yet — no sid
/// minted, no idempotency claim, no spawn reservation — and, deliberately, no
/// gateway lock held while the script runs. A policy hook is a user program,
/// and the useful ones ask the daemon questions (`curl` its REST API, read the
/// roster); running one under the gateway mutex would deadlock the daemon on
/// its own hook. Every fact the hook is handed is therefore gathered in short
/// locks BEFORE the subprocess starts, and the refusal event is filed in
/// another one AFTER it returns (all of them bounded, like every other lock on
/// this path, by the server-side deadline `execute_session_tool` wraps the
/// whole call in).
///
/// Cost when nobody configured a policy: two `stat`s. Only after a hook file is
/// found does this pay for the account-usage snapshot, the 24h cost projection
/// and the caller's context reading — an unconfigured daemon behaves, and
/// spends, exactly as it did before.
async fn pre_agent_policy_gate(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: &McpCaller,
    paths: &CcteamPaths,
) -> std::result::Result<(), String> {
    let dispatching = addresses_a_session(args);
    let arg_str = |key: &str| {
        args.get(key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    let target_sid = arg_str("sid");
    let caller_sid = arg_str("_caller_sid");
    // A hire names no project; the spawn's own resolver picks it. Called here
    // (off the lock) because it is pure — it reads args + the catalog and
    // provisions nothing — and its refusal is the spawn branch's to report.
    let hire_slug = if dispatching {
        None
    } else {
        resolve_spawn_project(args, caller, Some(paths))
            .ok()
            .map(|resolution| resolution.slug)
    };
    let requested_vendor = parse_session_vendor(args).ok();

    // ---- Phase 1: in-memory facts + WHERE a hook would live (short lock) ----
    let facts = {
        let gw = gateway.lock().await;
        let target = if dispatching {
            gw.session_resolve_any(&target_sid)
        } else {
            None
        };
        let slug = target
            .as_ref()
            .map(|session| session.project.clone())
            .or(hire_slug)
            .unwrap_or_default();
        let host = gw.project_bound_host(&slug);
        let vendor = target
            .as_ref()
            .and_then(|session| {
                ccteam_harness::AgentVendor::ALL
                    .iter()
                    .copied()
                    .find(|candidate| candidate.wire_name() == session.vendor)
            })
            .or(requested_vendor)
            .unwrap_or(ccteam_harness::AgentVendor::Claude);
        let caller_session = (!caller_sid.is_empty())
            .then(|| gw.session_resolve_any(&caller_sid))
            .flatten();
        let local = host == ccteam_core::LOCAL_HOST && !slug.is_empty();
        PolicyGateFacts {
            local,
            project_dir: local.then(|| gw.project_dir_for(&slug)).flatten(),
            vendor,
            host,
            caller_vendor: caller_session
                .as_ref()
                .map(|session| session.vendor.clone())
                .unwrap_or_default(),
            caller_dir: caller_session.map(|session| session.project_dir),
            children: (!caller_sid.is_empty()).then(|| gw.count_active_children(&caller_sid)),
            delegated: (!slug.is_empty()).then(|| gw.count_active_delegated(&slug)),
            projection: gw.progress_projection(),
            slug,
        }
    };
    // The catalog is the fallback for a project registered after the daemon
    // loaded its map (`ccteam init` mid-run), so a fresh project's own hook is
    // honoured without a restart. Never for a remote one.
    let project_dir = facts.local.then(|| {
        facts
            .project_dir
            .clone()
            .unwrap_or_else(|| paths.project_dir(&facts.slug))
    });
    // `paths.root` is the home THIS daemon runs on (`--home` / `CCTEAM_HOME`
    // already applied) — never re-derived from the environment here.
    let Some(script) = crate::policy::resolve_hook(project_dir.as_deref(), &paths.root) else {
        return Ok(());
    };

    // ---- Phase 2: the facts that cost something, for a real hook only ----
    let usage = {
        let gw = gateway.lock().await;
        gw.account_usage_snapshot(None).await
    };
    let usage = (!usage.is_empty()).then(|| {
        serde_json::Value::Object(
            usage
                .iter()
                .map(|(vendor, entry)| {
                    (vendor.clone(), crate::usage_view::vendor_usage_value(entry))
                })
                .collect(),
        )
    });
    let payload = crate::policy::PolicyFacts {
        kind: if dispatching { "dispatch" } else { "hire" },
        caller: crate::policy::CallerFacts {
            vendor: facts.caller_vendor.clone(),
            depth: args
                .get("_caller_depth")
                .and_then(|value| value.as_u64())
                .and_then(|depth| u32::try_from(depth).ok()),
            project: arg_str("_caller_slug"),
            // Only a resolved caller has a tree to read this from, so the
            // `None` here is exactly "we could not measure it" — never a zero.
            context_pct: facts
                .caller_dir
                .as_deref()
                .and_then(|dir| crate::delegation::latest_context_pct(dir, &caller_sid)),
            sid: caller_sid.clone(),
        },
        request: crate::policy::RequestFacts {
            vendor: if dispatching {
                facts.vendor.wire_name().to_string()
            } else {
                requested_vendor
                    .map(|vendor| vendor.wire_name().to_string())
                    .unwrap_or_default()
            },
            model: arg_str("model"),
            role: arg_str("role"),
            sid: target_sid.clone(),
            wait: inline_wait_seconds(args),
            task: args
                .get("task")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
            title: arg_str("title"),
        },
        usage,
        counts: crate::policy::CountFacts {
            children: facts.children,
            delegated: facts.delegated,
            cost_24h_usd: facts.projection.as_ref().and_then(|projection| {
                (!facts.slug.is_empty())
                    .then(|| projection.project_snapshot(&facts.slug).cost.cost_24h_usd)
            }),
        },
    }
    .payload();

    let outcome = crate::policy::run_hook(&script, &payload).await;
    let Some(refusal) = outcome.refusal_text() else {
        return Ok(());
    };
    // File the refusal on the SAME journal the built-in guardrails write to,
    // under its own event name: an operator asking "why did nothing get
    // delegated for an hour" must find the policy's fingerprints there.
    if let Some(reason) = outcome.deny_reason() {
        if !facts.slug.is_empty() {
            let title = arg_str("title");
            let gw = gateway.lock().await;
            gw.emit_delegation_progress(
                &facts.slug,
                ccteam_harness::execution::progress_bridge::DELEGATION_POLICY_DENIED,
                &caller_sid,
                &target_sid,
                facts.vendor,
                &facts.host,
                None,
                (!title.is_empty()).then_some(title.as_str()),
                Some(reason.tag()),
            );
        }
    }
    Err(format!("agent: {refusal}"))
}

/// `agent_read` — the roster (no `sid`) or one session's transcript (`sid`).
/// Each branch ignores the other's filters rather than erroring: a caller that
/// leaves a stale `activity` in place while reading one transcript has made no
/// mistake worth a refusal.
async fn run_agent_read(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
    paths: &CcteamPaths,
) -> std::result::Result<String, String> {
    if args.get("limit").is_some() {
        return Err("agent_read: `limit` was renamed to `n`".to_string());
    }
    if addresses_a_session(args) {
        run_agent_read_transcript(args, gateway, caller).await
    } else {
        run_agent_read_roster_at(args, gateway, Some(paths)).await
    }
}

/// `agent` — create a session in the caller's own project and return
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
///    An Ambient caller may additionally REFINE the edge with `parent_sid`,
///    but only onto a live session in its OWN project (validated, loud on
///    miss): the flow runner is an enrolled client acting FOR the managed
///    session that launched it, and the tree should say so. Cross-project
///    declarations are refused — a child's completion notification lands on
///    its parent, so pointing the edge at someone else's session would be an
///    injection channel, not attribution. The refusal is DELIBERATELY
///    indistinguishable from an unknown sid (same wording, no foreign slug):
///    sids are monotonic, so a distinguishing error would be an enumeration
///    oracle for other projects' sessions.
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
            let declared = args
                .get("parent_sid")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != caller_sid);
            if let Some(declared) = declared {
                // Attribution within the caller's own project only (see the
                // doc comment above): owner is a project attribute, so
                // same-project IS same-owner, and the caller stamps no
                // authority it does not already hold there.
                let caller_slug = args
                    .get("_caller_slug")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let Some(gateway) = gateway else {
                    return Err(format!(
                        "agent: parent_sid `{declared}` cannot be validated (no live gateway)"
                    ));
                };
                let view = {
                    let gw = match deadline {
                        Some(deadline) => deadline
                            .lock(gateway)
                            .await
                            .map_err(|error| mcp_gateway_error("agent", &error))?,
                        None => crate::latency::gateway_lock(gateway, "mcp.spawn.resolve").await,
                    };
                    // Project scoping INSIDE the predicate: a foreign live
                    // session and a nonexistent one must be the same miss,
                    // or monotonic sids become an enumeration oracle.
                    gw.session_views()
                        .into_iter()
                        .find(|v| v.sid == declared && v.project == caller_slug)
                };
                let Some(view) = view else {
                    return Err(format!(
                        "agent: parent_sid `{declared}` is not a live session in your project — run agent_read to find it, or omit parent_sid to attribute to yourself"
                    ));
                };
                return Ok(Some(crate::gateway::DelegationParent {
                    sid: view.sid,
                    depth: view.delegation_depth,
                    role: view.role,
                }));
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
                    "agent: parent_sid `{declared}` cannot be validated (no live gateway)"
                ));
            };
            let view = {
                let gw = match deadline {
                    Some(deadline) => deadline
                        .lock(gateway)
                        .await
                        .map_err(|error| mcp_gateway_error("agent", &error))?,
                    None => crate::latency::gateway_lock(gateway, "mcp.spawn.resolve").await,
                };
                gw.session_views().into_iter().find(|v| v.sid == declared)
            };
            let Some(view) = view else {
                return Err(format!(
                    "agent: parent_sid `{declared}` is not a live session — run agent_read to find your own sid, or omit parent_sid for a root spawn"
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

async fn run_agent_spawn_at(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
    paths: Option<&CcteamPaths>,
) -> std::result::Result<String, String> {
    let deadline = crate::gateway::GatewayDeadline::start();
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
    .map_err(|e| format!("agent: {e}"))?;
    // The child's own MCP tool face, persisted in its meta so every later
    // resume serves the same one. `full` is the default and is NOT stored.
    let tool_face = match args
        .get("tools")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("full") => None,
        Some(value @ ("read" | "none")) => Some(value.to_string()),
        Some(other) => {
            return Err(format!(
                "agent: invalid `tools` `{other}` (expected `full` | `read` | `none`)"
            ))
        }
    };
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
            return Err(format!("agent: `title` too long ({n} chars; max 80)"));
        }
    }
    // Resolve only after validating the request facets: falling through to
    // scratch is a side effect and malformed spawns must not create projects.
    let project = resolve_spawn_project(args, &caller, paths)?.slug;
    // v0.9.1 delegation-ergonomics — optional FIRST task: spawn+dispatch in one
    // call (the dominant flow; saves the second round-trip and closes the
    // crash window between a spawn and its first dispatch). Identical
    // semantics to agent{sid, task}: async by default with a
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
    // everywhere (agent_read, team view, notifications). When the spawn
    // carries a first task, derive a short label from its first line. Ledger/
    // display only (task → label, never label → prompt; the injection red line
    // is untouched).
    let title = title.or_else(|| task.as_deref().map(derive_title_from_task));
    let wait_seconds = inline_wait_seconds(args);
    let notify = parse_notify_mode("agent", args)?;
    // Validated on the hire path too, so a typo is a refusal rather than a
    // silently-ignored argument. A brand-new session is idle by construction,
    // so whichever channel is named the adapter reports `started`.
    let routing = parse_routing("agent", args)?;
    // Operator/unowned projects retain the caller-derived pool. Tenant-owned
    // projects ignore this fallback in the gateway and make every
    // agent inherit the tenant principal.
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
                .map_err(|error| mcp_gateway_error("agent", &error))?;
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
                .map_err(|error| mcp_gateway_error("agent", &error))?,
        )
    } else {
        None
    };
    let replay = if let Some(key) = idem_key.as_deref() {
        let mut gw = deadline
            .lock(gateway)
            .await
            .map_err(|error| mcp_gateway_error("agent", &error))?;
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
        tool_face,
        deadline,
    )
    .await
    .map_err(|error| {
        if error
            .downcast_ref::<crate::gateway::GatewayRequestError>()
            .is_some()
        {
            mcp_gateway_error("agent", &error)
        } else {
            spawn_create_error(error, &project, &caller, paths)
        }
    })?;
    let sid = created.sid;
    // The sid is the one fact a hire ALWAYS answers with: without it the
    // caller cannot follow up, and `dispatch_task` re-inserts the same value
    // on the dispatch branch so both shapes agree.
    let mut body = serde_json::json!({ "sid": sid });
    // v0.9.1 — dispatch the optional first task through the SAME submit path
    // agent uses; its outcome (turn_id / status / inline result /
    // hint) merges into the spawn body so one call returns everything. The
    // caller's parent link doubles as the dispatcher identity (empty = admin,
    // ledger-only submit without a watch).
    if let Some(task) = task {
        let dispatcher_sid = parent_sid_for_task.as_deref().unwrap_or("");
        let frag = dispatch_task(
            gateway,
            "agent",
            dispatcher_sid,
            &sid,
            task,
            wait_seconds,
            notify,
            title.clone(),
            routing,
            idem_key.clone(),
            deadline,
        )
        .await?;
        if let Some(obj) = body.as_object_mut() {
            obj.extend(frag);
        }
    }
    let out = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    // v0.9.0 W2 (F7) — record for idempotent replay (the exact body a retry
    // returns, with a replay flag added). Keyed per-project by the client key.
    if let Some(key) = idem_key.as_deref() {
        gateway.lock().await.spawn_idem_record(&project, key, &out);
    }
    Ok(out)
}

/// Test-only 3-arg shim (the historical signature) — production goes through
/// [`run_agent_spawn_at`] with real paths for catalog-aware errors.
#[cfg(test)]
async fn run_agent_spawn(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    run_agent_spawn_at(args, gateway, caller, None).await
}

struct SpawnProjectResolution {
    slug: String,
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
                .ok_or_else(|| "agent: no project (caller slug unset)".to_string())?,
        }),
        McpCaller::User { .. } => Ok(SpawnProjectResolution {
            slug: arg("project").ok_or_else(|| {
                "agent: missing `project` — tenant MCP callers must name one of their own projects explicitly"
                    .to_string()
            })?,
        }),
        McpCaller::Admin => {
            if let Some(slug) = arg("project") {
                return Ok(SpawnProjectResolution {
                    slug,
                });
            }
            if let Some(slug) = arg("_caller_slug") {
                return Ok(SpawnProjectResolution {
                    slug,
                });
            }
            if let Some(slug) = sole_registered_project(paths) {
                return Ok(SpawnProjectResolution {
                    slug,
                });
            }
            Err(format!(
                "agent: missing `project` — name the workspace to hire into{}",
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
    let base = format!("agent: {err}");
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

/// Idempotent retries replay the original response body plus one
/// positive-space fact: `idempotent_replay:true`. Only the rare replay path
/// pays the ~25 B, and the caller that just retried after a timeout is
/// exactly the caller that needs to know nothing double-fired (an absent
/// field is not a signal an agent can be trusted to read).
fn mark_idempotent_replay(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("idempotent_replay".into(), serde_json::json!(true));
            }
            serde_json::to_string(&v).unwrap_or_else(|_| body.to_string())
        }
        Err(_) => body.to_string(),
    }
}

/// Stable error for the removed MCP `agent.protocol` input.
pub const PROTOCOL_SPAWN_PARAM_REMOVED: &str = "agent: `protocol` was removed; the channel is derived from `vendor` (claude/codex/pi = stream-json, grok/opencode/kimi/dsh = acp) — omit `protocol`";

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

/// `agent` — forward a task as a user turn to a session by sid.
/// v0.9.0 W2 (F2/F5/F7): an Ambient (agent) dispatch (a) rejects a cycle
/// (target == caller or an ancestor), (b) arms a durable completion watch on
/// the child (parent = the dispatcher) so its next turn notifies the parent,
/// (c) emits `delegation_dispatched`, and (d) optionally blocks up to
/// `wait_seconds` for the child's answer inline. `idempotency_key` makes a
/// client retry replay the original turn (never double-dispatch). `title` is
/// ledger/notification only — NEVER concatenated into the task.
async fn run_agent_dispatch(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let deadline = crate::gateway::GatewayDeadline::start();
    let sid = arg_session_sid(args)?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "agent: missing `task`".to_string())?
        .to_string();
    // R-M3 FIRST — only operate sessions in the caller's own project, and the
    // scope miss is the one non-disclosing error. State refusals below may
    // speak plainly because they only ever fire for the caller's own sids.
    assert_caller_owns_session("agent", args, gateway, &sid, &caller, Some(deadline)).await?;
    // No thread to submit into: a hand-started (external) node, live or from
    // before a restart. Then the explicit-stop contract: stopped means "hire
    // a new one", while a merely-released session cold-resumes below.
    assert_target_not_external("agent", gateway, &sid, Some(deadline)).await?;
    assert_target_not_stopped("agent", gateway, &sid).await?;

    let wait_seconds = inline_wait_seconds(args);
    let notify = parse_notify_mode("agent", args)?;
    let routing = parse_routing("agent", args)?;
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    if let Some(t) = &title {
        let n = t.chars().count();
        if n > 80 {
            return Err(format!("agent: `title` too long ({n} chars; max 80)"));
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
            .map_err(|error| mcp_gateway_error("agent", &error))?;
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
                    "agent: delegation denied: cannot dispatch a session to itself (cycle)"
                        .to_string(),
                );
            }
            if gw.ancestor_chain(&caller_sid).contains(&sid) {
                emit_cycle(&gw);
                return Err(format!(
                    "agent: delegation denied: target {sid} is an ancestor of the caller {caller_sid} (cycle)"
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
                        "agent: delegation denied: vendor `{}` has reached its 24h budget for project `{slug}` (adjust budgets or wait for the window to slide)",
                        crate::delegation::vendor_key(vendor)
                    ));
                }
            }
        }
    }

    // ---- Scope 2 + wait: the shared submit half (also used by spawn{task}) ----
    let frag = dispatch_task(
        gateway,
        "agent",
        &caller_sid,
        &sid,
        task,
        wait_seconds,
        notify,
        title,
        routing,
        idem_key.clone(),
        deadline,
    )
    .await?;
    let mut body = serde_json::json!({});
    if let Some(obj) = body.as_object_mut() {
        obj.extend(frag);
    }
    let out = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    // v0.9.0 W2 (F7) — record for idempotent replay.
    if let Some(key) = idem_key.as_deref() {
        gateway.lock().await.dispatch_idem_record(&sid, key, &out);
    }
    Ok(out)
}

/// v0.9.1 delegation-ergonomics — the shared submit half of a dispatch, used
/// Parse the optional `notify` arg shared by `agent`/`agent`:
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

/// Parse the optional `routing` arg of `agent`: `"inject"` (default — the task
/// joins the turn the child is already running, the same channel a human IM
/// message takes) or `"queue"` (a distinct FIFO follow-up turn of its own).
///
/// The default is a STEER because that is what a parent tasking a child is
/// (AGENTS.md: `agent{task,sid}` and an IM `@handle` are one route). What the
/// vendor actually did comes back as the response's `status`, never as an echo
/// of this argument: an adapter with no injection channel degrades to a queued
/// turn and says `queued`.
fn parse_routing(
    tool: &str,
    args: &serde_json::Value,
) -> std::result::Result<ccteam_harness::TurnRouting, String> {
    match args.get("routing") {
        None | Some(serde_json::Value::Null) => Ok(ccteam_harness::TurnRouting::Inject),
        Some(serde_json::Value::String(raw)) => match raw.trim().to_ascii_lowercase().as_str() {
            "inject" => Ok(ccteam_harness::TurnRouting::Inject),
            "queue" => Ok(ccteam_harness::TurnRouting::Queue),
            other => Err(format!(
                "{tool}: invalid routing `{other}` (expected `inject` | `queue`)"
            )),
        },
        Some(other) => Err(format!(
            "{tool}: invalid routing {other} (expected `inject` | `queue`)"
        )),
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
    /// No `notify` arg — the harness default (`brief`), but only because nobody
    /// said otherwise.
    const fn defaulted() -> Self {
        Self {
            mode: ccteam_harness::NotifyMode::Brief,
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

    /// The mode this dispatch actually runs under: an explicit argument wins,
    /// otherwise this parent's most recent outstanding request on this child
    /// sets the precedent, otherwise the wire default (`brief`).
    ///
    /// THE one place that decides it (issue #201 F.1). Every dispatch path —
    /// a caller's own child, a peer handoff, an external parent — asks here, so
    /// a parent's deliberate `final` cannot be silently downgraded on the next
    /// message by a path that forgot to look. It used to be: the parent made a
    /// fifteen-minute decision off the 443-character excerpt that came back.
    fn effective(self, precedent: Option<ccteam_harness::NotifyMode>) -> Self {
        if self.explicit {
            return self;
        }
        Self {
            mode: precedent.unwrap_or(self.mode),
            // Inherited, not named: a later follow-up inherits it in turn.
            explicit: false,
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

    fn is_deliverable(self) -> bool {
        self == Self::ParentSession
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

/// by BOTH `agent` and `agent{task}` (one-call
/// spawn+dispatch, the dominant delegation flow). Accept + persist the REQUEST
/// (before anything reaches the vendor) → submit the task as a verbatim user
/// turn → bind the request to the execution turn the adapter gave it → emit
/// `delegation_dispatched` → optionally block inline for THIS request's answer.
/// Returns the response FRAGMENT (`request_id`/`status`/`delivery`/result
/// fields) the caller merges into its own body; `tool` prefixes error strings.
#[allow(clippy::too_many_arguments)]
async fn dispatch_task(
    gateway: &GatewayHandle,
    tool: &str,
    caller_sid: &str,
    sid: &str,
    task: String,
    wait_seconds: u64,
    notify: NotifyRequest,
    title: Option<String>,
    routing: ccteam_harness::TurnRouting,
    idempotency_key: Option<String>,
    deadline: crate::gateway::GatewayDeadline,
) -> std::result::Result<serde_json::Map<String, serde_json::Value>, String> {
    let is_delegation = !caller_sid.is_empty();
    // Claimed BEFORE anything is read or written, and held through the submit
    // and the bind (issue #201). A child that answers in the microsecond after
    // its line reached the vendor used to find its request still `Accepted`,
    // resolve nothing, and lose the completion until a daemon restart; the
    // notifier plans under this same claim, so it waits for the binding.
    let store_claim = if is_delegation {
        Some(
            crate::gateway::Gateway::claim_delegation_store_within(gateway, sid, deadline)
                .await
                .map_err(|error| mcp_gateway_error(tool, &error))?,
        )
    } else {
        None
    };
    let (rx, parent_is_external, peer_unsubscribed, precedent) = {
        let gw = deadline
            .lock(gateway)
            .await
            .map_err(|error| mcp_gateway_error(tool, &error))?;
        // Whether a completion turn is deliverable is a property of the PARENT's
        // ledger row, not of the caller's auth tier: a hand-started client dials
        // in over MCP, so there is no thread to steer and no session to resume.
        // Asked once, here, so the recorded request and the response fragment
        // can never disagree about it.
        let parent_is_external = is_delegation && gw.is_external_node(caller_sid);
        // Subscribe BEFORE submitting so a fast child can't answer before we
        // start listening (the wait races the child's own turn).
        let rx = if wait_seconds > 0 {
            Some(gw.subscribe_events())
        } else {
            None
        };
        // What this caller last asked for on this child, if anything is still
        // outstanding — the precedent an omitted `notify` inherits.
        let precedent = if is_delegation && !notify.explicit {
            gw.delegation_notify_precedent(sid, caller_sid)
        } else {
            None
        };
        // v0.10.1 — is the target one of the caller's OWN sessions? A dispatch
        // to a session the caller never delegated is a HANDOFF: the target has
        // its own parent, or is a root with its own human. `agent_read` draws
        // no edge for it (that tree is spawn lineage) and `agent_stop` refuses
        // it, so a subscription made here is an edge nobody can see or take
        // down. The default `notify` is a default, not a request — only an
        // explicit one subscribes the caller to a session it does not own.
        //
        // A precedent IS such a request: this caller already has outstanding
        // work on this peer under a mode it chose, so the follow-up inherits
        // that mode instead of being silenced (issue #201 F.1 — the peer path
        // is a dispatch path like any other).
        let peer_unsubscribed = is_delegation
            && !notify.explicit
            && precedent.is_none()
            && !gw.lineage_reaches(sid, caller_sid);
        (rx, parent_is_external, peer_unsubscribed, precedent)
    };
    // ONE decision, used by the recorded request AND by the response's
    // notification route below, so the two can never disagree.
    let notify = notify.effective(precedent);
    let effective_notify = notify.mode;
    let request_id = if is_delegation {
        // The request is recorded either way — the completion edge belongs in
        // the ledger (`delegation_completed` fires off it, whatever the notify
        // mode). Durable IO is explicitly outside the gateway mutex; a
        // generation fence rejects a concurrently replaced child. An external
        // parent gets it with notifications OFF: left on, the first completion
        // would submit into a session ccteam must never re-spawn, fail, and
        // drop the request — silently ending that child's completion
        // accounting. A peer handoff gets the same treatment for the opposite
        // reason: the edge is real and worth recording, the subscription was
        // never asked for.
        let watch_notify = if parent_is_external || peer_unsubscribed {
            ccteam_harness::NotifyMode::Off
        } else {
            effective_notify
        };
        let accepted = crate::gateway::Gateway::accept_delegation_request_shared(
            Arc::clone(gateway),
            store_claim
                .as_ref()
                .expect("a delegation holds its store claim"),
            caller_sid,
            watch_notify,
            title.clone(),
            // What the DISPATCHER asked for; what the adapter did with it is
            // the request's state + bound turn (issue #197 D). A ccteam-authored
            // completion notification is a different path entirely and stays
            // queued (issue #194).
            routing,
            idempotency_key,
            deadline,
        )
        .await
        .map_err(|error| mcp_gateway_error(tool, &error))?;
        // No record means no completion edge and no notification. Submitting
        // anyway hands the caller a normal-looking dispatch and then waits
        // forever for an answer that can never be delivered (issue #7), so the
        // failure belongs here, before the task goes out.
        let Some(accepted) = accepted else {
            return Err(format!(
                "{tool}: no completion watch could be registered for {sid} (unknown session)"
            ));
        };
        // Before the task is even submitted: this caller will block on THIS
        // request's answer, so the completion is its to take inline and the
        // notifier must not ALSO push it (issue #195 — the parent paid a whole
        // extra turn for the second copy). Per request: a sibling task
        // finishing meanwhile is still pushed, because the caller is not
        // holding that one.
        if wait_seconds > 0 {
            gateway
                .lock()
                .await
                .claim_request_wait(sid, &accepted, wait_seconds);
        }
        Some(accepted)
    } else {
        None
    };
    let mut receipt = match crate::gateway::Gateway::submit_to_sid_receipt_shared(
        Arc::clone(gateway),
        sid,
        task,
        routing,
        deadline,
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(error) => {
            if let Some(request_id) = request_id.as_deref() {
                crate::gateway::Gateway::drop_delegation_request_shared(
                    Arc::clone(gateway),
                    store_claim
                        .as_ref()
                        .expect("a delegation holds its store claim"),
                    request_id,
                )
                .await;
            }
            return Err(mcp_gateway_error(tool, &error));
        }
    };
    let mut bind_error: Option<String> = None;
    if let Some(request_id) = request_id.as_deref() {
        // Bind BEFORE the caller is told anything, and DURABLY before the
        // adapter's completion fence is released below: a boundary that lands
        // in the next millisecond must already know whose answer it is, and one
        // that lands before the binding is on disk would leave an executed
        // request unbindable by every later life (issue #201).
        // The text goes with it: `state:"unknown"` says a correlation is not
        // promised, and this says why — the same string the request carries
        // into `agent_read`.
        bind_error = crate::gateway::Gateway::bind_delegation_request_shared(
            Arc::clone(gateway),
            store_claim
                .as_ref()
                .expect("a delegation holds its store claim"),
            request_id,
            &receipt,
        )
        .await
        .err()
        .map(|error| format!("{error:#}"));
        let gw = gateway.lock().await;
        if let Some((vendor, host, slug)) = gw.session_vendor_host_slug(sid) {
            gw.emit_delegation_progress(
                &slug,
                ccteam_harness::execution::progress_bridge::DELEGATION_DISPATCHED,
                caller_sid,
                sid,
                vendor,
                &host,
                Some(&receipt.turn_id),
                title.as_deref(),
                None,
            );
        }
    }
    // The handover is complete: the request is bound, so the notifier may plan
    // against it and the vendor's own turn boundary may land. Both released
    // BEFORE the inline wait below — a wait that held either would block the
    // very boundary it is waiting for.
    receipt.release_completion();
    drop(store_claim);
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
            turn_id = %receipt.turn_id,
            notify = effective_notify.as_str(),
            parent_is_external,
            "ccteam MCP completion notification unavailable: caller has no managed parent session; poll agent_read"
        );
    } else if notification_route == CompletionNotificationRoute::PeerUnsubscribed {
        tracing::info!(
            tool,
            caller_sid,
            child_sid = %sid,
            turn_id = %receipt.turn_id,
            "ccteam MCP handoff to a session the caller did not delegate: ledger-only, no completion watch armed"
        );
    }

    // ---- wait branch (OFF the gateway lock) ----
    if let Some(rx) = rx {
        Ok(dispatch_wait_for_completion(
            gateway,
            sid,
            &receipt,
            request_id.as_deref(),
            InlineWaitWindow {
                effective_seconds: wait_seconds,
            },
            rx,
            notification_route,
        )
        .await)
    } else {
        let mut m = serde_json::Map::new();
        m.insert("sid".to_string(), serde_json::json!(sid));
        if let Some(request_id) = request_id.as_deref() {
            m.insert("request_id".to_string(), serde_json::json!(request_id));
        }
        insert_delivery_facts(&mut m, &receipt);
        if let Some(error) = bind_error.as_deref() {
            // The task went out; the record of WHICH turn answers it did not.
            // This process may still resolve it, no later one can, and saying
            // `queued` would promise a correlation a restart cannot keep.
            m.insert("state".to_string(), serde_json::json!("unknown"));
            m.insert("error".to_string(), serde_json::json!(error));
            m.remove("queue_position");
            m.insert(
                "delivery".to_string(),
                serde_json::json!({
                    "accepted": true,
                    "queued": "unknown",
                    "written": "unknown",
                    "executing": "unknown",
                }),
            );
        }
        if receipt.queued_behind_body {
            // One sid, one body: the child's process from before a ccteam
            // restart is still finishing its turn; the task is queued behind
            // it and runs the moment that body exits (the notification then
            // arrives as usual). The only surviving `hint`: nothing else in
            // the response says why nothing is happening yet.
            m.insert(
                "hint".to_string(),
                serde_json::json!(
                    "queued behind the session body still finishing after restart; agent_stop \
                     ends it now"
                ),
            );
        }
        if !notification_route.is_deliverable() {
            // The one fact a caller cannot infer: no notification is coming,
            // so poll. Only ever present when it is FALSE.
            m.insert("notify_deliverable".into(), serde_json::json!(false));
        }
        Ok(m)
    }
}

/// The delivery facts a dispatch response carries. `status` is what the adapter
/// DID — `started` / `injected` / `queued` — not the flat `pending` every
/// dispatch used to answer (issue #201: a parent that could not tell "running"
/// from "third in a queue" re-sent the same instruction three times and then
/// stopped a 400k-context child it believed was ignoring it).
///
/// `delivery` keeps the four facts apart. ccteam accepting a request, ccteam
/// retaining it, the bytes reaching the harness, and the harness being observed
/// running it are four different claims; a stdin flush is not proof the model
/// read anything, so `executing` is `unknown` until a turn is observed to open.
fn insert_delivery_facts(
    m: &mut serde_json::Map<String, serde_json::Value>,
    receipt: &crate::gateway::TurnReceipt,
) {
    m.insert("turn_id".to_string(), serde_json::json!(receipt.turn_id));
    m.insert("status".to_string(), serde_json::json!(receipt.status()));
    if let Some(position) = receipt.queue_position {
        m.insert("queue_position".to_string(), serde_json::json!(position));
    }
    let queued = receipt.status() == "queued";
    m.insert(
        "delivery".to_string(),
        serde_json::json!({
            "accepted": true,
            "queued": queued,
            "written": !queued,
            "executing": "unknown",
        }),
    );
}

/// An inline wait that ran out: the child is still working (a timeout NEVER
/// cancels it), so the honest answer is that the task has not answered yet —
/// with the delivery facts, so the caller can tell "running" from "still third
/// in the queue" instead of reading one flat `pending` for both (issue #201).
fn pending_dispatch_response(
    sid: &str,
    receipt: &crate::gateway::TurnReceipt,
    request_id: Option<&str>,
    delivery: Option<serde_json::Value>,
    notification_route: CompletionNotificationRoute,
) -> serde_json::Map<String, serde_json::Value> {
    let mut response = serde_json::Map::new();
    response.insert("sid".to_string(), serde_json::json!(sid));
    if let Some(request_id) = request_id {
        response.insert("request_id".to_string(), serde_json::json!(request_id));
    }
    insert_delivery_facts(&mut response, receipt);
    // A named request with no row left in the store: it was dropped out from
    // under the caller (a stop, or an unreachable parent). Everything ccteam
    // knew about its fate went with it, so the state is `unknown` — not
    // `queued`, not `answered`, and never a sibling's text.
    if delivery.is_none() && request_id.is_some() {
        response.insert("state".to_string(), serde_json::json!("unknown"));
        response.remove("queue_position");
    }
    // The live row wins over the submit-time guess: a task that was third in
    // the queue when it was accepted may be running by now.
    if let Some(row) = delivery {
        if let Some(state) = row.get("state").and_then(|s| s.as_str()) {
            response.insert("state".to_string(), serde_json::json!(state));
        }
        if let Some(position) = row.get("queue_position") {
            response.insert("queue_position".to_string(), position.clone());
        } else {
            response.remove("queue_position");
        }
        if let Some(facts) = row.get("delivery") {
            response.insert("delivery".to_string(), facts.clone());
        }
    }
    // The task has not answered: distinct from the delivery `status` above,
    // which says where the message got to.
    response.insert("answered".to_string(), serde_json::json!(false));
    if !notification_route.is_deliverable() {
        response.insert("notify_deliverable".into(), serde_json::json!(false));
    }
    response
}

/// Wait until `sid`'s vendor turn is no longer in flight, or `deadline`.
/// Returns whether a turn boundary was reached.
///
/// ONE waiter for both long-poll shapes — an `agent{wait}` dispatch (which
/// starts before its own task is submitted, so `armed` is false: it must see
/// this child answer something first) and an `agent_read{sid,wait}` (which has
/// already established the target is mid-turn, so `armed` is true). Sharing it
/// is the point: the trap below was found once and must not be re-derived.
///
/// **A frame alone is NOT completion.** Codex mirrors interim narration as
/// separate answers inside one still-running vendor turn, so returning on the
/// first frame hands back a checkpoint note as the result (and, for a
/// dispatch, disarms the real completion's watch). After a frame arrives,
/// completion additionally requires the turn to have left flight
/// (`session_turn_in_flight`, the same cell the pump clears on completion or
/// failure), re-checked on a short poll tick — the terminal boundary emits no
/// frame of its own.
///
/// NEVER holds the gateway lock across an await.
async fn await_turn_boundary(
    gateway: &GatewayHandle,
    sid: &str,
    deadline: tokio::time::Instant,
    rx: &mut tokio::sync::broadcast::Receiver<crate::gateway::GatewayEvent>,
    armed: bool,
) -> bool {
    // Re-check cadence for "answer seen, is the turn still in flight?".
    const BOUNDARY_POLL: std::time::Duration = std::time::Duration::from_millis(200);
    let mut saw_answer = armed;
    loop {
        if saw_answer {
            let in_flight = {
                let gw = gateway.lock().await;
                gw.session_turn_in_flight(sid)
            };
            if !in_flight {
                return true;
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        // While mid-turn with an answer already seen, wake at least every
        // BOUNDARY_POLL to re-check the in-flight cell.
        let wait_slice = if saw_answer {
            remaining.min(BOUNDARY_POLL)
        } else {
            remaining
        };
        match tokio::time::timeout(wait_slice, rx.recv()).await {
            Ok(Ok(ev)) => {
                let hit = ev.sid.as_deref() == Some(sid)
                    && matches!(ev.kind, crate::gateway::GatewayEventKind::Answer)
                    && ev.status.is_some();
                if hit {
                    saw_answer = true;
                }
            }
            // Broadcast lag → keep waiting (we may have missed unrelated frames).
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            // Sender gone (daemon shutdown) → give up on the boundary.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return false,
            // Poll tick (or deadline) → loop re-checks in-flight / remaining.
            Err(_) => {}
        }
    }
}

/// v0.9.0 W2 (F2) — the OFF-lock half of a `wait_seconds>0` dispatch. Awaits
/// THIS REQUEST's own completion on the gateway broadcast until the deadline.
/// NEVER holds the gateway lock across the await (lock discipline).
///
/// "Its own" is the whole point (issue #201). A parent that dispatched A and
/// then waited on B used to be handed A's answer the moment A finished — the
/// wait returned on the first turn boundary of the child, whichever task it
/// belonged to. Here the boundary is only a wake-up: the wait ends when the
/// caller's request reaches a terminal state, and the answer it returns is the
/// transcript row that request was resolved against, not "the newest row".
///
/// The caller placed an inline claim before the submit, so the notifier hands
/// this boundary over instead of pushing a second copy of the answer; the claim
/// is released here either way. On timeout it returns the delivery facts and
/// leaves the request outstanding (the child is not cancelled).
///
/// v0.9.5 feedback fix — an `Answer` frame alone is NOT completion; see
/// [`await_turn_boundary`], which owns that rule for every long poll.
#[allow(clippy::too_many_arguments)]
async fn dispatch_wait_for_completion(
    gateway: &GatewayHandle,
    child_sid: &str,
    receipt: &crate::gateway::TurnReceipt,
    request_id: Option<&str>,
    wait: InlineWaitWindow,
    mut rx: tokio::sync::broadcast::Receiver<crate::gateway::GatewayEvent>,
    notification_route: CompletionNotificationRoute,
) -> serde_json::Map<String, serde_json::Value> {
    // MCP-DX-1 — elapsed telemetry: the wait starts right after the submit, so
    // submit→completion is an honest task-duration approximation for a wait
    // that covers the whole task.
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(wait.effective_seconds);
    let Some(request_id) = request_id else {
        // No request of our own (an admin caller): the first boundary is the
        // only thing there is to wait for.
        let observed = await_turn_boundary(gateway, child_sid, deadline, &mut rx, false).await;
        return finish_dispatch_wait(
            gateway,
            child_sid,
            receipt,
            None,
            observed,
            notification_route,
        )
        .await;
    };
    // Wait in short slices and re-check OUR request each time. The boundary
    // event is only a hint: the notifier resolves the request off the pump a
    // moment later, and a wait that decided on the event alone would either
    // return holding a sibling's answer or sit out its whole timeout for one
    // that had already arrived.
    const REQUEST_POLL: std::time::Duration = std::time::Duration::from_millis(100);
    let mut answered = false;
    loop {
        let slice = deadline.min(tokio::time::Instant::now() + REQUEST_POLL);
        let _ = await_turn_boundary(gateway, child_sid, slice, &mut rx, false).await;
        let state = {
            let gw = gateway.lock().await;
            gw.delegation_request_state(child_sid, request_id)
        };
        match state {
            Some(state) if state.is_terminal() => {
                answered = true;
                break;
            }
            // The row has LEFT the store: an `agent_stop` dropped it, or the
            // notifier could not reach its parent. Nothing will resolve it now,
            // so the wait ends — but "gone" is not "answered" (issue #201).
            // Treating it as answered sent the caller to the transcript tail,
            // where the newest row is a SIBLING's answer. It reports unknown.
            None => break,
            Some(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    finish_dispatch_wait(
        gateway,
        child_sid,
        receipt,
        Some(request_id),
        answered,
        notification_route,
    )
    .await
}

/// Release the inline claim and render what the wait is holding: this
/// request's own answer, or the delivery facts that say why it is not here
/// yet.
async fn finish_dispatch_wait(
    gateway: &GatewayHandle,
    child_sid: &str,
    receipt: &crate::gateway::TurnReceipt,
    request_id: Option<&str>,
    answered: bool,
    notification_route: CompletionNotificationRoute,
) -> serde_json::Map<String, serde_json::Value> {
    // Release the claim, and pick up a boundary the notifier suppressed in our
    // favour: that happens when the child finished in the same instant our
    // deadline expired, and the answer is then ours to report rather than
    // nobody's (the notification was already skipped).
    let (suppressed, delivery_row) = if let Some(request_id) = request_id {
        let mut gw = gateway.lock().await;
        let suppressed = gw.release_request_wait(child_sid, request_id);
        let row = gw
            .delegation_request_rows(child_sid, usize::MAX)
            .into_iter()
            .find(|row| row.get("request_id").and_then(|id| id.as_str()) == Some(request_id));
        (suppressed, row)
    } else {
        (false, None)
    };
    if !answered && !suppressed {
        return pending_dispatch_response(
            child_sid,
            receipt,
            request_id,
            delivery_row,
            notification_route,
        );
    }

    // Resolve the child (sync) under a brief lock, then read its transcript
    // tail OFF the lock for a clean, unprefixed result.
    let (resolved, answered_turn) = {
        let gw = gateway.lock().await;
        let answered_turn = request_id.and_then(|id| gw.delegation_request_answer(child_sid, id));
        (gw.session_resolve(child_sid), answered_turn)
    };
    let (result_record, cost_usd, meta_turn) = resolved
        .as_ref()
        .map(|r| {
            let all =
                ccteam_harness::execution::turns_mirror::read_all_turns(&r.project_dir, &r.sid)
                    .unwrap_or_default();
            // THE row this request was resolved against — never the newest
            // one. Between the boundary and this read the child may already
            // have finished a queued follow-up, and returning that as the
            // answer is exactly the mix-up this wait exists to prevent (issue
            // #201). With a request identity there is NO substitute: an answer
            // we cannot name is reported as absent, not as somebody else's.
            // Without one — an admin caller waiting on the child's next
            // boundary — the newest row is what was waited for.
            let last = match answered_turn.as_deref() {
                Some(turn_id) => all.into_iter().find(|t| t.turn_id == turn_id),
                None if request_id.is_none() => {
                    all.into_iter().rev().find(|t| !t.assistant.is_empty())
                }
                None => None,
            };
            // Session-ledger telemetry (MCP-DX-1): cumulative cost + raw
            // tokens, same semantics as agent_read/collect (tokens present
            // even for vendors with no USD price table).
            let (cost, turn) =
                ccteam_harness::execution::session_meta::read_session_meta(&r.project_dir, &r.sid)
                    .ok()
                    .map(|m| (m.cost_usd, m.turn_count))
                    .unwrap_or((None, 0));
            (last, cost, turn)
        })
        .unwrap_or((None, None, 0));

    let record = result_record.as_ref();
    // The child's harness rides the mirrored turn (`TurnRecord.vendor`), the
    // same source the gateway's completion header uses; an unmirrored result
    // simply omits the field rather than guessing.
    let vendor = record.map(|turn| turn.vendor.clone()).unwrap_or_default();
    let answer_turn_id = record
        .map(|turn| turn.turn_id.as_str())
        .unwrap_or(&receipt.turn_id);
    DelegationSummary {
        sid: child_sid,
        vendor: &vendor,
        turn_id: answer_turn_id,
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
        conclusion: record.and_then(|turn| turn.conclusion.as_deref()),
        request_id,
        // The header of an inline result names the task the caller is holding;
        // it asked for this one by id, so the label adds nothing and the queue
        // count belongs to the push path.
        title: None,
        remaining_queued: 0,
    }
    .inline_result()
}

/// One page of a session's transcript, as `agent_read{sid}` answers it.
struct TranscriptPage {
    rows: Vec<serde_json::Value>,
    /// `turn_id` of the last row on the page — the paging cursor. `None` when
    /// the page is empty.
    cursor: Option<String>,
    /// Matching turns NOT on this page: the still-unread newer ones for a
    /// forward (`since`) read, the older ones for a tail read. Said out loud
    /// because "I gave you one of three" must never look like "that is all"
    /// (issue #194: a `since` + `n:1` read returned the oldest unread turn and
    /// the caller took it for the newest).
    remaining: usize,
    /// `turn_id` of the newest matching turn in the whole transcript, whatever
    /// the page shows — the answer to "has it said anything new?" on a
    /// status-only (`n:0`) read.
    latest: Option<String>,
}

/// Rows `agent_read{sid}` shows: assistant-side turns, plus every row that
/// carries a terminal `outcome`, whose text is routinely empty. TWO writers
/// produce such rows: a vendor turn that failed before it said anything
/// (`outcome:"failed"`), and the boot reconcile's `"unobserved"` row for a turn
/// the restarted daemon never watched end — both are facts a poller must see
/// instead of the PREVIOUS successful answer. Dropping those pages the FAILURE
/// out: the session then reads "not working, here is the last row", and the
/// newest row left is the PREVIOUS successful answer, which a polling caller
/// returns as the result of the task that just failed. An empty `content` is
/// the documented "no answer yet" shape and costs nothing to carry; the
/// failure it comes with is the whole point of the row.
fn is_transcript_row(turn: &ccteam_harness::execution::turns_mirror::TurnRecord) -> bool {
    !turn.assistant.is_empty() || turn.outcome.is_some()
}

/// `agent_read{sid,turn}` — the one page holding exactly that turn. Nothing
/// is `remaining`: the caller asked for one row by name and got it, or an
/// error naming the turn the transcript does not hold.
fn exact_collected_turn(
    all: &[ccteam_harness::execution::turns_mirror::TurnRecord],
    turn_id: &str,
) -> Option<TranscriptPage> {
    let latest = all
        .iter()
        .rev()
        .find(|turn| is_transcript_row(turn))
        .map(|turn| turn.turn_id.clone());
    let row = all.iter().find(|turn| turn.turn_id == turn_id)?;
    Some(TranscriptPage {
        rows: vec![collected_turn_row(row)],
        cursor: Some(row.turn_id.clone()),
        remaining: 0,
        latest,
    })
}

/// v0.8.7 review-fix (R-L3) — pure paging core of [`run_agent_read_transcript`],
/// extracted so the cursor/paging contract is unit-testable without a gateway
/// or filesystem. Given ALL mirrored turns, an optional `since` turn-id
/// cursor, a page size `n` and the direction, it:
///
/// - keeps only transcript rows AFTER `since` (or all when `since` is `None` /
///   not found — never silently lose turns on a stale cursor),
/// - returns the OLDEST `n` of those (so repeated polls page forward in
///   order), or the NEWEST `n` when `tail` is set,
/// - counts what it withheld in `remaining` and names the newest turn in
///   `latest`, so the caller can tell "one of three" from "that is all"
///   (issue #194) and page on with `cursor`.
fn page_collected_turns(
    all: &[ccteam_harness::execution::turns_mirror::TurnRecord],
    since: Option<&str>,
    n: usize,
    tail: bool,
) -> TranscriptPage {
    let latest = all
        .iter()
        .rev()
        .find(|turn| is_transcript_row(turn))
        .map(|turn| turn.turn_id.clone());
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
        .filter(|t| is_transcript_row(t))
        .map(|t| collected_turn_row(t))
        .collect();
    let remaining = rows.len().saturating_sub(n);
    if tail {
        // v0.9.1 — the "final answer" shape: keep the NEWEST n (chronological
        // order preserved inside the page).
        rows.drain(..remaining);
    } else {
        rows.truncate(n);
    }
    let cursor = rows
        .last()
        .and_then(|r| r.get("turn_id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    TranscriptPage {
        rows,
        cursor,
        remaining,
        latest,
    }
}

/// One transcript row as `agent_read{sid}` publishes it.
fn collected_turn_row(
    t: &ccteam_harness::execution::turns_mirror::TurnRecord,
) -> serde_json::Value {
    let mut row = serde_json::json!({"turn_id": t.turn_id, "content": t.assistant});
    // Steers this row's own cut only (a cut row shows the conclusion,
    // not the narration's head — issue #196); `bound_collected_turns`
    // strips it, so the wire never carries a second copy of the text.
    if let Some(conclusion) = t.conclusion.as_deref() {
        row["conclusion"] = serde_json::json!(conclusion);
    }
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
}

/// The exact one-call read of a whole turn — what a truncated transcript row
/// points at instead of a vague "read more".
///
/// It names the TURN, not a position. The cheapest form used to be "the newest
/// turn" (`n:1`), which is what a truncated row was at the instant it was
/// written and stops being the moment the child finishes anything else — a
/// parent that followed the recipe minutes later read a queued confirmation's
/// answer and took it for the verdict it had asked for (issue #201). A cursor
/// (`since:<previous>`) has the same defect one row further back: it depends
/// on nothing being appended in between.
fn whole_turn_recipe(sid: &str, turn_id: &str, total_chars: usize) -> String {
    let budget = crate::delegation::read_budget_for(total_chars);
    format!("agent_read{{sid:{sid},turn:{turn_id},max_chars:{budget}}}")
}

fn collect_max_chars(args: &serde_json::Value) -> usize {
    let Some(value) = args.get("max_chars") else {
        return AGENT_READ_DEFAULT_MAX_CHARS;
    };
    if let Some(n) = value.as_u64() {
        return n.clamp(
            AGENT_READ_MIN_MAX_CHARS as u64,
            AGENT_READ_MAX_MAX_CHARS as u64,
        ) as usize;
    }
    value
        .as_i64()
        .map(|n| {
            n.clamp(
                AGENT_READ_MIN_MAX_CHARS as i64,
                AGENT_READ_MAX_MAX_CHARS as i64,
            ) as usize
        })
        .unwrap_or(AGENT_READ_DEFAULT_MAX_CHARS)
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

/// Shed rows from a page that cannot afford them. A row is unaffordable when
/// the page overflows `max_chars` AND the equal share left per row has fallen
/// under [`MIN_USEFUL_ROW_CHARS`] — at that point every row would come back as
/// mostly pointer, and a caller is better served by fewer whole turns plus an
/// honest `remaining`. Returns how many rows were dropped; a page that fits is
/// never touched, and one row always survives.
///
/// WHICH END matters. A `tail` page is "the latest answers", so the oldest go
/// and the newest turn always survives. A FORWARD page is a caller walking a
/// cursor, so the newest go instead: dropping from the front there would move
/// the cursor past turns the caller never saw, and a short page is far better
/// than one that silently skips.
fn drop_unaffordable_rows(
    rows: &mut Vec<serde_json::Value>,
    max_chars: usize,
    tail: bool,
) -> usize {
    let row_chars = |row: &serde_json::Value| {
        row.get("content")
            .and_then(|value| value.as_str())
            .map(|text| text.chars().count())
            .unwrap_or(0)
    };
    let mut total: usize = rows.iter().map(row_chars).sum();
    let mut dropped = 0;
    while rows.len() > 1 && total > max_chars && max_chars / rows.len() < MIN_USEFUL_ROW_CHARS {
        let victim = if tail { 0 } else { rows.len() - 1 };
        total -= row_chars(&rows[victim]);
        rows.remove(victim);
        dropped += 1;
    }
    dropped
}

/// Apply the collect character budget to the already-selected turn page.
/// `recipe(turn_id, total_chars)` names the exact read of one whole turn, and
/// rides on every excerpt's marker. Returns `(original_total_chars,
/// any_content_truncated)`.
fn bound_collected_turns(
    rows: &mut [serde_json::Value],
    max_chars: usize,
    recipe: &dyn Fn(&str, usize) -> String,
) -> (usize, bool) {
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
        strip_bounding_fields(rows);
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
        let conclusion = row.get("conclusion").and_then(|v| v.as_str());
        let turn_id = row
            .get("turn_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let mark =
            |omitted: usize| format!("…[+{omitted} chars: {}]…", recipe(turn_id, original_chars));
        // A pointer that costs more than the text it withholds makes the answer
        // both bigger and worse — return the turn whole instead. The overspend
        // is bounded by one marker per row (issue #195: turns of 131-197 chars
        // were cut to save 40-94 while paying 86 to say where the rest was).
        if original_chars <= budget + mark(original_chars - budget).chars().count() {
            continue;
        }
        // The same excerpt rule as a completion notification: a cut row shows
        // the turn's conclusion, not the head of its narration (issue #196).
        let bounded = crate::delegation::answer_excerpt(content, conclusion, budget, mark);
        row["content"] = serde_json::json!(bounded.text);
        truncated |= bounded.truncated;
    }
    strip_bounding_fields(rows);
    (total_chars, truncated)
}

/// A row's `conclusion` exists to steer its own cut; the wire never carries a
/// second copy of a turn's text.
fn strip_bounding_fields(rows: &mut [serde_json::Value]) {
    for row in rows.iter_mut() {
        if let Some(object) = row.as_object_mut() {
            object.remove("conclusion");
        }
    }
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

/// One decimal place. A ledger total is read for its magnitude, and the f64
/// the vendor accrued into (`326.49616805000005`) spends twenty characters of
/// the caller's context to say `326.5`.
fn round_cost_usd(cost: f64) -> f64 {
    (cost * 10.0).round() / 10.0
}

/// `12345` -> `12k`, `146752597` -> `147m`. A token total is read as a SIZE,
/// never as an exact figure — `context_pct` is what a caller actually decides
/// on — so nine raw digits are nine characters of noise.
fn abbreviate_tokens(total: u64) -> String {
    match total {
        n if n < 1_000 => n.to_string(),
        n if n < 1_000_000 => format!("{}k", (n as f64 / 1_000.0).round() as u64),
        n if n < 1_000_000_000 => format!("{}m", (n as f64 / 1_000_000.0).round() as u64),
        n => format!("{:.1}b", n as f64 / 1_000_000_000.0),
    }
}

#[derive(serde::Serialize)]
struct SessionRow {
    activity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_pct: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_total: Option<String>,
}

fn session_row_fields(row: SessionRow) -> serde_json::Map<String, serde_json::Value> {
    serde_json::to_value(row)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// Hold an `agent_read{sid,wait}` open until the target's in-flight turn ends.
///
/// Returns whether it returned AT a turn boundary (a boundary the caller is now
/// holding the result of), which is `false` for `wait:0`, for a target that is
/// not mid-turn, and for a timeout. Subscribes BEFORE it looks at the
/// in-flight cell so a turn that ends in between cannot be missed, and holds no
/// gateway lock across the await.
async fn read_wait_for_turn_boundary(
    gateway: &GatewayHandle,
    sid: &str,
    args: &serde_json::Value,
) -> bool {
    let wait = read_wait_seconds(args);
    if wait == 0 {
        return false;
    }
    let (mut rx, in_flight, resolved) = {
        let gw = gateway.lock().await;
        (
            gw.subscribe_events(),
            gw.session_turn_in_flight(sid),
            gw.session_resolve_any(sid),
        )
    };
    if !in_flight {
        // Nothing to wait for: the answer (or the emptiness) is already final.
        return false;
    }
    let before = resolved.as_ref().and_then(last_mirrored_turn_id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
    if !await_turn_boundary(gateway, sid, deadline, &mut rx, true).await {
        return false;
    }
    if let Some(resolved) = resolved.as_ref() {
        settle_until_the_final_turn_lands(resolved, before.as_deref()).await;
    }
    true
}

/// The turn_id of the newest row in a session's durable mirror.
fn last_mirrored_turn_id(resolved: &crate::gateway::SessionResolve) -> Option<String> {
    ccteam_harness::execution::turns_mirror::read_all_turns(&resolved.project_dir, &resolved.sid)
        .ok()?
        .pop()
        .map(|turn| turn.turn_id)
}

/// Wait (briefly, bounded) for the notifier to record which of `waiting_on`
/// this boundary resolved, and return them. The notifier runs off the pump
/// while a reader reaches the same boundary through an event broadcast, so
/// asking immediately is asking too early.
async fn settle_until_a_request_resolves(
    gateway: &GatewayHandle,
    sid: &str,
    waiting_on: &[String],
) -> ReadResolution {
    const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
    const POLL: std::time::Duration = std::time::Duration::from_millis(20);
    let mut outcome = ReadResolution::default();
    if waiting_on.is_empty() {
        return outcome;
    }
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        outcome = ReadResolution::default();
        {
            use ccteam_harness::RequestState;
            let gw = gateway.lock().await;
            for id in waiting_on {
                match gw.delegation_request_state(sid, id) {
                    // ANSWERED at an observed boundary — the only thing
                    // "resolved" may mean. The caller acts on this: it is
                    // holding the answer to exactly these tasks.
                    Some(RequestState::Answered | RequestState::Failed) => {
                        outcome.resolved.push(id.clone())
                    }
                    // Cut short, confirmed undelivered, or gone from the store
                    // altogether (an `agent_stop`, an unreachable parent).
                    // These stopped being resolvable WITHOUT being answered,
                    // and reporting them as resolved told a caller it was
                    // holding an answer that does not exist (issue #201).
                    Some(RequestState::Interrupted | RequestState::Undelivered) | None => {
                        outcome.unknown.push(id.clone())
                    }
                    Some(_) => outcome.still_waiting = true,
                }
            }
        }
        // An answer in hand is what this read came for, so it returns on the
        // first one. An `unknown` alone is not worth cutting the wait short
        // for: something else may still be about to answer.
        let decided = !outcome.resolved.is_empty() || !outcome.still_waiting;
        if decided || tokio::time::Instant::now() >= deadline {
            return outcome;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What a long-poll read can honestly say about the requests it was waiting on.
#[derive(Default)]
struct ReadResolution {
    /// Answered or failed at an OBSERVED boundary — the caller holds these.
    resolved: Vec<String>,
    /// No longer resolvable and never answered: dropped from the store, cut
    /// short, or confirmed undelivered. Named separately because "I do not
    /// know" and "here is your answer" are different things to act on.
    unknown: Vec<String>,
    /// At least one request is still outstanding — nothing to report for it.
    still_waiting: bool,
}

/// Wait (briefly, bounded) for the boundary's own row to reach `turns.jsonl`.
///
/// The event pump clears the in-flight cell in its terminal-accounting step,
/// which runs BEFORE it mirrors that turn's answer — so "no longer in flight"
/// leads the durable write by a hair. Returning on the cell alone handed back a
/// body missing the very turn the caller waited for (observed as a flaky read
/// under load). This closes the window by watching for the row itself instead
/// of sleeping a guessed amount: it returns the moment the row lands, and gives
/// up after [`BOUNDARY_SETTLE`] for a boundary that appends nothing at all (a
/// completion with no answer text).
async fn settle_until_the_final_turn_lands(
    resolved: &crate::gateway::SessionResolve,
    before: Option<&str>,
) {
    const BOUNDARY_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
    const POLL: std::time::Duration = std::time::Duration::from_millis(20);
    let deadline = tokio::time::Instant::now() + BOUNDARY_SETTLE;
    while tokio::time::Instant::now() < deadline {
        if last_mirrored_turn_id(resolved).as_deref() != before {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// `agent_read{sid}` — tail one session's `turns.jsonl` (assistant turns).
/// Resolve sid → role + project_dir under the lock, drop the guard, then read
/// the ccteam-owned mirror. `since` is a turn_id cursor.
///
/// Default is NEWEST-first: the overwhelmingly common read is "what did it
/// answer", and the old oldest-first default made a caller page through a
/// transcript it had already seen to reach it. Passing `since` flips the
/// default to forward paging, which is the shape that cursor is for — and the
/// body then says what it withheld (`remaining`) and what the newest turn is
/// (`latest`), so an oldest-unread page can never pass for the newest answer.
/// `n:0` is the status-only read: the same body with no turn text at all —
/// activity, `latest`, and how many turns are unread past `since` — for the
/// caller whose question is "is it done?" (issue #194).
async fn run_agent_read_transcript(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    // Same gate as dispatch/stop: ccteam mirrors no transcript for a client it
    // never spawned, so the honest answer is what the session is — not an empty
    // page or an "unknown session" from the resolve below.
    let since = args.get("since").and_then(|v| v.as_str()).map(String::from);
    // The EXACT selector. `since` is a cursor — "what came after this" — and a
    // truncated excerpt used to point at `n:1`, "the newest turn", which is
    // only what the excerpt showed at the instant it was written: fifteen
    // minutes and one queued confirmation later that read returns a different
    // answer and says nothing about it (issue #201). `turn:<turn_id>` names
    // the one row, forever.
    let exact_turn = args.get("turn").and_then(|v| v.as_str()).map(String::from);
    // `n:0` is legal here (status only); the roster keeps its floor of 1.
    let n = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|x| (x as usize).min(500))
        .unwrap_or(AGENT_READ_TRANSCRIPT_DEFAULT_N);
    let tail = args
        .get("tail")
        .and_then(|v| v.as_bool())
        .unwrap_or(since.is_none());
    let max_chars = collect_max_chars(args);
    // R-M3 — only collect from sessions in the caller's own project.
    assert_caller_owns_session("agent_read", args, gateway, &sid, &caller, None).await?;
    // External nodes have no ccteam-held thread OR transcript mirror to read;
    // after the scope gate so the wording cannot probe foreign sids.
    assert_target_not_external("agent_read", gateway, &sid, None).await?;
    // ---- the long poll (off the gateway lock, after every gate) ----
    //
    // The missing primitive was "wait for the turn that is in flight". Without
    // it a parent that had to sit out a 25-minute child either burned a
    // dispatch `wait` it could not extend or — measured 2026-08-31 — tailed the
    // child's private `turns.jsonl` and grepped line shapes, which is the
    // caller-built side channel this surface exists to make unnecessary. The
    // read that follows is the ordinary one: every filter still applies, the
    // body carries no new field, and a timeout just answers
    // `activity:"working"`.
    //
    // D4 — a boundary this read returns is one the caller now HOLDS, so the
    // completion notification would be a second copy. The claim goes in BEFORE
    // the poll (the notifier reaches the boundary first otherwise, issue #195),
    // and only for the parent the watch names: a third party reading someone
    // else's child must never take that child's notification away from the
    // session that hired it.
    let read_wait = read_wait_seconds(args);
    let caller_sid = args
        .get("_caller_sid")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let claimed = read_wait > 0 && !caller_sid.is_empty() && {
        let mut gw = gateway.lock().await;
        gw.parent_holds_delegation_request(&sid, &caller_sid)
            && gw.claim_read_wait(&sid, &caller_sid, read_wait)
    };
    // WHICH of the caller's tasks were outstanding when the wait began. The
    // ones that are gone afterwards are the ones this read resolved — a
    // question the reader could not answer at all before, and the reason a
    // parent took a queued confirmation's answer for its verdict (issue #201).
    let waiting_on: Vec<String> = if claimed {
        gateway
            .lock()
            .await
            .outstanding_request_ids(&sid, &caller_sid)
    } else {
        Vec::new()
    };
    let reached = read_wait_for_turn_boundary(gateway, &sid, args).await;
    let mut resolution = ReadResolution::default();
    if claimed {
        if reached {
            // The boundary landed. Give the notifier the moment it needs to
            // record WHICH requests it answered — the claim is still held, so
            // it cannot push a copy of what this read is about to return, and
            // reporting "resolved nothing" while the bookkeeping is still in
            // flight would be the vaguest possible answer to the one question
            // this field exists for.
            resolution = settle_until_a_request_resolves(gateway, &sid, &waiting_on).await;
        }
        // The notifier suppressed this boundary in our favour (or we reached
        // it first); either way the caller now holds whatever it answered.
        let _suppressed = gateway.lock().await.release_read_wait(&sid, &caller_sid);
    }

    // Resolve under the lock (sync) — with the child's in-flight turn, which is
    // a cheap in-memory peek — then DROP the guard before the fs read.
    // Residency comes from the SAME lock hold as the resolve: two acquisitions
    // could disagree about a session that was released in between.
    let (resolved, live, projection, residency) = {
        let gw = gateway.lock().await;
        (
            gw.session_resolve_any(&sid),
            gw.live_turn_for(&sid),
            gw.progress_projection(),
            gw.session_residency(&sid),
        )
    };
    let resolved = resolved.ok_or_else(|| format!("agent_read: unknown session {sid}"))?;

    // Tail the ccteam-owned transcript mirror.
    // v0.8.8 F1 — the mirror is keyed by `sid` (`.ccteam/chat/<sid>/turns.jsonl`),
    // not role, so read by `resolved.sid` (role is a content label only).
    let all = ccteam_harness::execution::turns_mirror::read_all_turns(
        &resolved.project_dir,
        &resolved.sid,
    )
    .map_err(|e| format!("agent_read: read turns.jsonl for {sid}: {e}"))?;

    // v0.9.0 W2 (F2) — surface the vendor resume key + accrued cost from meta.
    let meta = ccteam_harness::execution::session_meta::read_session_meta(
        &resolved.project_dir,
        &resolved.sid,
    )
    .ok();
    let cost_usd = meta.as_ref().and_then(|m| m.cost_usd).map(round_cost_usd);
    let tokens_total = meta
        .as_ref()
        .and_then(|m| m.tokens_total)
        .map(abbreviate_tokens);
    let latest_status = all.iter().rev().find_map(|turn| turn.status.clone());
    // Apply the `since` cursor + page forward (R-L3 — oldest-first, no silent
    // drop of a > `n` burst; `tail:true` flips to newest-first). Pure logic in
    // `page_collected_turns`.
    let page = match exact_turn.as_deref() {
        Some(turn_id) => exact_collected_turn(&all, turn_id)
            .ok_or_else(|| format!("agent_read: {sid} has no turn {turn_id}"))?,
        None => page_collected_turns(&all, since.as_deref(), n, tail),
    };
    let TranscriptPage {
        mut rows,
        mut cursor,
        mut remaining,
        latest,
    } = page;
    // Fewer whole turns beat a page of stubs: while the page cannot fit and a
    // row's share has fallen under what a turn needs to say anything, drop the
    // OLDEST row and count it as unread rather than shredding every row down to
    // its pointer (issue #195).
    let dropped = drop_unaffordable_rows(&mut rows, max_chars, tail);
    if dropped > 0 {
        remaining += dropped;
        cursor = rows
            .last()
            .and_then(|row| row.get("turn_id"))
            .and_then(|value| value.as_str())
            .map(String::from);
    }
    let recipe =
        |turn_id: &str, total_chars: usize| whole_turn_recipe(&resolved.sid, turn_id, total_chars);
    let (_total_chars, content_truncated) = bound_collected_turns(&mut rows, max_chars, &recipe);

    let activity = classify_session_activity(
        projection.as_deref(),
        &resolved.project,
        &resolved.sid,
        live,
    )
    .unwrap_or_else(|| "idle".into());
    let context_pct = crate::delegation::context_pct(latest_status.as_ref());
    let mut body = session_row_fields(SessionRow {
        activity,
        context_pct,
        cost_usd,
        tokens_total,
    });
    body.insert("turns".into(), serde_json::json!(rows));
    if let Some(cursor) = cursor.as_deref() {
        body.insert("cursor".into(), serde_json::json!(cursor));
    }
    // What the page did NOT show, stated as numbers and ids: `remaining`
    // matching turns off the page, and `latest` when the newest turn is not the
    // one the cursor points at (always on a status-only read).
    if remaining > 0 {
        body.insert("remaining".into(), serde_json::json!(remaining));
    }
    if let Some(latest) = latest.filter(|latest| Some(latest.as_str()) != cursor.as_deref()) {
        body.insert("latest".into(), serde_json::json!(latest));
    }
    // `truncated` is about TEXT: a returned turn was cut to fit `max_chars`
    // (its marker carries the exact read of the whole turn).
    if content_truncated {
        body.insert("truncated".into(), serde_json::json!(true));
    }
    // What this child OWES, and to whom. A dispatcher could not see its own
    // queue at all: told only `pending`, it re-sent the same instruction three
    // times and then stopped a child it believed was ignoring it (issue #201).
    // Outstanding rows first, then the most recent resolved ones, bounded.
    let requests = gateway
        .lock()
        .await
        .delegation_request_rows(&sid, AGENT_READ_REQUEST_ROWS);
    if !requests.is_empty() {
        body.insert("requests".into(), serde_json::json!(requests));
    }
    // Which of the caller's own tasks this read resolved — the answer it is
    // now holding, named, so it is never mistaken for another one.
    if !resolution.resolved.is_empty() {
        body.insert(
            "resolved_requests".into(),
            serde_json::json!(resolution.resolved),
        );
    }
    // …and which of them stopped being resolvable WITHOUT being answered.
    // Listing those as resolved told a caller it was holding an answer that
    // never existed (issue #201).
    if !resolution.unknown.is_empty() {
        body.insert(
            "unknown_requests".into(),
            serde_json::json!(resolution.unknown),
        );
    }
    // `status: "stopped"` used to mean nothing more than "not live", which
    // read as "this session is over" for a session that was merely between
    // processes — the caller's next move (spawn a replacement vs dispatch to
    // this one) turns on that difference. Say which it is.
    if let Some(residency) = residency.filter(|r| *r != crate::gateway::RESIDENCY_RESIDENT) {
        body.insert("residency".into(), serde_json::json!(residency));
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
                cursor.as_deref(),
                None,
                None,
            );
        }
    }
    Ok(serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string()))
}

/// `agent_read` (no `sid`) — the roster of sessions the caller can reach.
///
/// A fleet of tens of live sessions dumped verbatim floods the caller's
/// context, so the listing accepts `project` / `activity` filters, caps at
/// [`AGENT_READ_DEFAULT_N`] most-recently-active rows (explicit
/// `truncated`/`total` fields say when a cap bit), and omits null/empty
/// fields.
#[cfg(test)]
async fn run_agent_read_roster(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
) -> std::result::Result<String, String> {
    run_agent_read_roster_at(args, gateway, None).await
}

async fn run_agent_read_roster_at(
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
                "agent_read: invalid `activity` filter `{a}` (expected `working` | `idle` | `stale` | `stuck` | `all`)"
            ));
        }
    }
    let limit = args
        .get("n")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, 500))
        .unwrap_or(AGENT_READ_DEFAULT_N);
    let include_tree = args.get("tree").and_then(|v| v.as_bool()).unwrap_or(false);

    // Both halves of the activity answer come from under ONE lock hold, and
    // both are cheap in-memory reads. Projection catch-ups happen below, after
    // the guard drops (a fleet's streams are far too big to touch under the
    // gateway mutex).
    let (views, live_turns, projection, status_roots) = {
        let gw = gateway.lock().await;
        // RESIDENT (+ external + detached) rows, then the RELEASED ones: a
        // session ccteam is not holding a process for is not gone — it resumes
        // by sid on the next dispatch — so a caller that could not see it here
        // would keep spawning duplicates of work it already has.
        let mut views = gw.session_views();
        views.extend(gw.released_session_views(caller_visible_projects.as_ref()));
        let status_roots = views
            .iter()
            .filter_map(|view| {
                gw.session_resolve_any(&view.sid)
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
    let classified: Vec<(&crate::gateway::SessionView, String)> = views
        .iter()
        .map(|v| {
            // A detached body (alive from before a daemon restart, not driven
            // from here) is its own state: neither working nor idle.
            if v.detached.is_some() {
                return (v, "detached".to_string());
            }
            let activity = activity_ctx
                .get(&v.project)
                .map(|snapshot| {
                    let silent = snapshot
                        .last_valid
                        .as_ref()
                        .and_then(|event| {
                            ccteam_core::stall::progress_event_age_seconds(event, now)
                        })
                        .unwrap_or(0);
                    snapshot
                        .session_activity(&v.sid, silent, live_turns.get(&v.sid).copied(), now)
                        .status
                        .activity
                        .to_string()
                })
                .unwrap_or_else(|| "idle".to_string());
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
                return activity == want;
            }
            true
        })
        .collect();
    let total = classified.len();
    let truncated = total > limit;
    // Context % costs a `turns.jsonl` tail read per session, so it is paid ONLY
    // for the rows actually emitted — not for the whole fleet before the cut.
    let context_pcts = classified
        .iter()
        .take(limit)
        .filter_map(|(v, _)| {
            let dir = status_roots.get(&v.sid)?;
            crate::delegation::latest_context_pct(dir, &v.sid).map(|pct| (v.sid.clone(), pct))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let rows: Vec<serde_json::Value> = classified
        .iter()
        .take(limit)
        .map(|(v, activity)| {
            // Slim rows: null/empty/default fields are omitted rather than
            // spelled out (the caller reads these into its context).
            let mut row = session_row_fields(SessionRow {
                activity: activity.clone(),
                context_pct: context_pcts.get(&v.sid).copied(),
                cost_usd: v.cost_usd.map(round_cost_usd),
                tokens_total: v.tokens_total.map(abbreviate_tokens),
            });
            row.insert("sid".into(), serde_json::json!(v.sid));
            if !v.role.is_empty() {
                row.insert("role".into(), serde_json::json!(v.role));
            }
            row.insert("vendor".into(), serde_json::json!(v.vendor));
            // Residency only when it is NOT the default: a resident row would
            // spend the caller's context saying nothing.
            if v.residency != crate::gateway::RESIDENCY_RESIDENT {
                row.insert("residency".into(), serde_json::json!(v.residency));
            }
            // The caller's OWN row (see `caller_sid`). Named nothing like
            // `current` on purpose: the two answer different questions, and
            // reading one as the other is the failure this ends.
            if caller_sid == Some(v.sid.as_str()) {
                row.insert("is_self".into(), serde_json::json!(true));
            }
            if v.waiting_approval {
                row.insert("waiting_approval".into(), serde_json::json!(true));
            }
            // v0.9.0 W2 (F2) — delegation topology + attribution.
            if let Some(p) = &v.parent_sid {
                row.insert("parent_sid".into(), serde_json::json!(p));
            }
            if v.host != "local" {
                row.insert("host".into(), serde_json::json!(v.host));
            }
            if let Some(t) = &v.title {
                row.insert("title".into(), serde_json::json!(t));
            }
            serde_json::Value::Object(row)
        })
        .collect();
    // A `tree` view (roots → children by `parent_sid`) so a caller sees the
    // delegation topology without recomputing it. Laid over exactly the rows
    // RETURNED: a topology that names sids the response does not carry costs
    // context to describe sessions the caller cannot look at, and the caller
    // widens `n` when it wants more. Roots = returned rows whose parent is not
    // among them (a true root, a parent past the cut, or one in a project the
    // caller cannot see).
    let returned: Vec<crate::gateway::SessionView> = classified
        .iter()
        .take(limit)
        .map(|(v, _)| (*v).clone())
        .collect();
    let sids: std::collections::HashSet<&str> = returned.iter().map(|v| v.sid.as_str()).collect();
    let mut body = serde_json::json!({"sessions": rows});
    if include_tree {
        let tree: Vec<serde_json::Value> = returned
            .iter()
            .filter(|v| {
                v.parent_sid
                    .as_deref()
                    .map(|p| !sids.contains(p))
                    .unwrap_or(true)
            })
            .map(|v| session_tree_node(v, &returned))
            .collect();
        body["tree"] = serde_json::json!(tree);
    }
    if truncated {
        body["truncated"] = serde_json::json!(true);
        body["total"] = serde_json::json!(total);
    }
    Ok(serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string()))
}

/// Build one node of the roster's delegation tree: `{sid, vendor, role?,
/// children:[...]}` recursively (children = sessions whose `parent_sid` is this
/// sid). Depth is bounded by the returned set, so the recursion terminates. An
/// empty `role` is omitted — roleless is the DEFAULT, and spelling `""` out on
/// every node costs the caller bytes to learn nothing.
fn session_tree_node(
    v: &crate::gateway::SessionView,
    all: &[crate::gateway::SessionView],
) -> serde_json::Value {
    let children: Vec<serde_json::Value> = all
        .iter()
        .filter(|c| c.parent_sid.as_deref() == Some(v.sid.as_str()) && c.sid != v.sid)
        .map(|c| session_tree_node(c, all))
        .collect();
    let mut node = serde_json::Map::new();
    node.insert("sid".into(), serde_json::json!(v.sid));
    node.insert("vendor".into(), serde_json::json!(v.vendor));
    if !v.role.is_empty() {
        node.insert("role".into(), serde_json::json!(v.role));
    }
    node.insert("children".into(), serde_json::json!(children));
    serde_json::Value::Object(node)
}

/// `agent_stop` — deregister + close a session by sid (explicit command).
async fn run_agent_stop(
    args: &serde_json::Value,
    gateway: &GatewayHandle,
    caller: McpCaller,
) -> std::result::Result<String, String> {
    let sid = arg_session_sid(args)?;
    // R-M3 — only stop sessions in the caller's own project (explicit command,
    // never a proactive kill; the scope check just prevents cross-project stop).
    assert_caller_owns_session("agent_stop", args, gateway, &sid, &caller, None).await?;
    // Before the descendant walk: a hand-started client's process belongs to
    // its operator, and the walk would otherwise reject it as "not a
    // descendant" — true, but not the reason. Stopped-again is the same
    // refusal it always was.
    assert_target_not_external("agent_stop", gateway, &sid, None).await?;
    assert_target_not_stopped("agent_stop", gateway, &sid).await?;
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
    let gw = gateway.lock().await;
    if !caller_sid.is_empty() && !gw.ancestor_chain(&sid).contains(&caller_sid) {
        // The rule is right and stays; what was missing is the way out. A
        // hand-started client that reconnects is a NEW ledger node, so the
        // sessions its previous node hired are nobody's descendants and it
        // could not stop its own work at all (measured 2026-08-31).
        return Err(format!(
            "agent_stop: permission denied — session {sid} is not a descendant of the caller {caller_sid} (an agent may only stop the sessions it delegated). A reconnected client is a new ledger node, so its earlier hires are not its descendants: stop it from the web console, or POST /api/v1/sessions/{sid}/stop with a web token"
        ));
    }
    // Capture the delegation event fields BEFORE the stop removes the session
    // from the live map.
    let stopped_meta = gw.session_vendor_host_slug(&sid);
    drop(gw);
    // The stop itself goes through the shared door: it takes the child's
    // delegation claim in the right order, records the turn it cut short in
    // `turns.jsonl`, settles every request the child still owed, and makes
    // that durable. The child's requests are NOT dropped — a stop ends a
    // process, and a task ccteam is still holding replays on the next resume
    // (issue #197 E; dropping them left a parent with no way to learn that its
    // instruction had never been delivered).
    let outcome = crate::gateway::Gateway::stop_session_shared(gateway, &sid)
        .await
        .map_err(|e| format!("agent_stop failed: {e}"))?;
    if !caller_sid.is_empty() {
        if let Some((vendor, host, slug)) = stopped_meta {
            gateway.lock().await.emit_delegation_progress(
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
    let mut body = serde_json::Map::new();
    body.insert("sid".into(), serde_json::json!(sid));
    body.insert("stopped".into(), serde_json::json!(true));
    if let Some(cut) = outcome.interrupted.as_ref() {
        // What the stop actually ended. `turn` is the transcript row the
        // narration is in, so the caller reads it with one exact call instead
        // of paying for it in every stop response.
        let mut interrupted = serde_json::Map::new();
        interrupted.insert("turn".into(), serde_json::json!(cut.row_turn_id));
        interrupted.insert("exec_turn".into(), serde_json::json!(cut.exec_turn_id));
        interrupted.insert(
            "narration".into(),
            serde_json::json!(cut.narration.as_str()),
        );
        if !cut.request_ids.is_empty() {
            interrupted.insert("requests".into(), serde_json::json!(cut.request_ids));
        }
        body.insert("interrupted".into(), serde_json::Value::Object(interrupted));
    }
    if !outcome.undelivered.is_empty() {
        let rows: Vec<serde_json::Value> = outcome
            .undelivered
            .iter()
            .map(|request| {
                let mut row = serde_json::Map::new();
                row.insert("request_id".into(), serde_json::json!(request.request_id));
                if let Some(title) = request.title.as_deref() {
                    row.insert("title".into(), serde_json::json!(title));
                }
                row.insert("state".into(), serde_json::json!(request.state));
                row.insert("delivery".into(), serde_json::json!(request.delivery));
                if let Some(file) = request.retained_in {
                    row.insert("retained_in".into(), serde_json::json!(file));
                }
                serde_json::Value::Object(row)
            })
            .collect();
        body.insert("undelivered".into(), serde_json::json!(rows));
    }
    if outcome.has_retained() {
        // Only when something is actually held: a policy nobody's task is
        // subject to is noise in every other stop response.
        body.insert(
            "resume_policy".into(),
            serde_json::json!(crate::gateway::RESUME_POLICY_REPLAY_AFTER_FIRST_RESULT),
        );
    }
    Ok(
        serde_json::to_string(&serde_json::Value::Object(body))
            .unwrap_or_else(|_| "{}".to_string()),
    )
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
/// caller can SEE in `agent_read` as unknown — a correct refusal that reads as
/// a ccteam bug. One shared message
/// ([`crate::external_nodes::not_driveable_error`]) says what the session IS: a
/// process its own operator drives, usable as a delegation parent.
/// Refuse a hand-started (external) target. The live index answers for
/// current bindings; the on-disk meta answers for nodes from before a daemon
/// restart — an external node must stay refusable across restarts, or the
/// engine would try to drive a process it never held. Runs AFTER the
/// ownership gate so the (state-revealing) wording never leaks a foreign
/// sid's existence.
async fn assert_target_not_external(
    tool: &str,
    gateway: &GatewayHandle,
    sid: &str,
    deadline: Option<crate::gateway::GatewayDeadline>,
) -> std::result::Result<(), String> {
    let (is_external, resolved) = match deadline {
        Some(deadline) => {
            let gw = deadline
                .lock(gateway)
                .await
                .map_err(|error| mcp_gateway_error(tool, &error))?;
            (gw.is_external_node(sid), gw.session_resolve_any(sid))
        }
        None => {
            let gw = gateway.lock().await;
            (gw.is_external_node(sid), gw.session_resolve_any(sid))
        }
    };
    if is_external {
        return Err(crate::external_nodes::not_driveable_error(tool, sid));
    }
    if let Some(resolved) = resolved {
        let external_on_disk = ccteam_harness::execution::session_meta::read_session_meta(
            &resolved.project_dir,
            &resolved.sid,
        )
        .is_ok_and(|meta| !meta.managed_by.is_driveable());
        if external_on_disk {
            return Err(crate::external_nodes::not_driveable_error(tool, sid));
        }
    }
    Ok(())
}

/// Refuse dispatch/stop of an explicitly STOPPED session (the MCP contract:
/// "hire a new one"; the transcript stays readable). Read-only surfaces skip
/// this — a stopped session's history is exactly what they exist for.
async fn assert_target_not_stopped(
    tool: &str,
    gateway: &GatewayHandle,
    sid: &str,
) -> std::result::Result<(), String> {
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve_any(sid)
    };
    if let Some(resolved) = resolved {
        let stopped_at = ccteam_harness::execution::session_meta::read_session_meta(
            &resolved.project_dir,
            &resolved.sid,
        )
        .ok()
        .and_then(|meta| meta.stopped_at);
        if let Some(at) = stopped_at {
            return Err(format!(
                "{tool}: {sid} was stopped at {at}; agent_read still reads it — hire a new one to continue"
            ));
        }
    }
    Ok(())
}

/// v0.8.7 review-fix (R-M3) — project-scope a sid-addressed session call:
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
        // ONE resolver for every tool: live map first, on-disk meta second.
        // The old live-only branch for agent/agent_stop conflated "stopped"
        // with "not in the live map", so after a daemon restart every
        // pre-restart released session answered "unknown" — breaking
        // resume-by-sid (issue #8). Stopped/external refusals are separate,
        // meta-driven gates that run AFTER this scope check.
        gw.session_resolve_any(sid)
    };
    match caller {
        McpCaller::Admin => Ok(()),
        McpCaller::User { user_id } => {
            let allowed = resolved
                .as_ref()
                .map(|resolved| CcteamPaths::project_state_in(&resolved.project_dir))
                .and_then(|state_path| ccteam_core::ProjectState::load(&state_path).ok())
                .is_some_and(|state| {
                    ccteam_core::identity::can_see_owner(user_id, false, state.owner.as_deref())
                });
            if allowed {
                Ok(())
            } else {
                Err(tenant_unreachable_session_error(name))
            }
        }
        McpCaller::Ambient => {
            let caller_slug = args
                .get("_caller_slug")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{name}: no project scope (ambient slug unset)"))?
                .to_string();
            // Unknown and unreachable answer the SAME text, from the same
            // builder: naming the project a sid runs in told a caller that
            // `s1` exists in somebody else's workspace, which is an
            // enumerable cross-tenant leak (measured 2026-08-31 — `agent_read`
            // said "s1 runs in project pm" for a sid `agent` and `agent_stop`
            // called unknown).
            match resolved {
                Some(resolved) if resolved.project == caller_slug => Ok(()),
                _ => Err(unreachable_session_error(name, gateway, sid, &caller_slug).await),
            }
        }
    }
}

/// The ONE refusal for a sid an ambient caller cannot reach — `agent`,
/// `agent_read` and `agent_stop` all answer through it, and unknown,
/// invisible and another tenant's are deliberately the same bytes.
///
/// "Unknown" and "you stopped it" are the same absence from the live map but
/// two different next moves — hire a replacement vs. read what it already
/// said — and collapsing them into "unknown session" sent callers hunting for
/// a bug in ccteam. The stopped answer is only ever given for a session in the
/// CALLER'S OWN project: elsewhere, existence itself stays undisclosed.
async fn unreachable_session_error(
    tool: &str,
    gateway: &GatewayHandle,
    sid: &str,
    caller_slug: &str,
) -> String {
    let resolved = {
        let gw = gateway.lock().await;
        gw.session_resolve_any(sid)
    };
    if let Some(resolved) = resolved.filter(|resolved| resolved.project == caller_slug) {
        let stopped_at = ccteam_harness::execution::session_meta::read_session_meta(
            &resolved.project_dir,
            &resolved.sid,
        )
        .ok()
        .and_then(|meta| meta.stopped_at);
        if let Some(at) = stopped_at {
            return format!(
                "{tool}: {sid} was stopped at {at}; agent_read still reads it — hire a new one to continue"
            );
        }
    }
    format!("{tool}: unknown session {sid} in project {caller_slug}")
}

/// The tenant tier's half of the same rule: one text for a sid this user
/// cannot see, whatever the reason. Deliberately shorter than the ambient
/// one — a tenant has no single bound project to name.
fn tenant_unreachable_session_error(tool: &str) -> String {
    format!("{tool}: session not found")
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
                "agent: invalid vendor `{other}`: expected `claude`, `codex`, `grok`, `opencode`, `kimi`, `pi`, or `dsh`"
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

    pub(super) fn call(name: &str, args: serde_json::Value) -> serde_json::Value {
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

    /// `wait` is clamped to what the transport can survive, and the retired
    /// `wait_seconds` spelling is a hard error rather than a silent no-op.
    #[test]
    fn inline_wait_clamps_to_the_transport_safe_ceiling() {
        for (requested, expected) in [(600, 240), (240, 240), (0, 0), (30, 30)] {
            assert_eq!(inline_wait_seconds(&json!({ "wait": requested })), expected);
        }
        assert_eq!(inline_wait_seconds(&json!({})), 0);
    }

    /// `agent_read{sid,wait}` clamps like `agent{wait}` and is inert on the
    /// roster branch, which has no turn to wait for.
    #[test]
    fn read_wait_clamps_and_only_applies_to_a_named_session() {
        for (requested, expected) in [(600, 240), (240, 240), (0, 0), (30, 30)] {
            assert_eq!(
                read_wait_seconds(&json!({ "sid": "s5", "wait": requested })),
                expected
            );
        }
        assert_eq!(read_wait_seconds(&json!({ "wait": 240 })), 0, "roster");
        assert_eq!(read_wait_seconds(&json!({ "sid": " ", "wait": 240 })), 0);
        assert_eq!(read_wait_seconds(&json!({ "sid": "s5" })), 0);
    }

    /// D1 — the primitive that was missing: hold the read open until the
    /// target's in-flight turn ENDS, then answer the ordinary transcript.
    /// Without it a parent waiting on a long child had to tail the child's
    /// private `turns.jsonl` (measured 2026-08-31).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn agent_read_wait_holds_until_the_turn_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gw, principal, _drx) =
            build_dispatch_gateway(true, false, 150, None, tmp.path()).await;
        let child = parse(
            &run_agent(
                &ambient(&principal, "alpha", json!({ "task": "long job" })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            gw.lock().await.session_turn_in_flight(&child),
            "the child is mid-turn when the read starts"
        );

        let body = parse(
            &run_agent_read(
                &ambient(&principal, "alpha", json!({ "sid": &child, "wait": 20 })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        assert!(
            !gw.lock().await.session_turn_in_flight(&child),
            "the read returned at the turn boundary: {body}"
        );
        let turns = body["turns"].as_array().unwrap();
        assert!(
            turns
                .last()
                .and_then(|turn| turn["content"].as_str())
                .is_some_and(|content| content.contains("echo: long job")),
            "the final turn is in the body the wait returned: {body}"
        );
        // A long poll answers the SAME body shape as an ordinary read — plus
        // the request bookkeeping every `sid` read carries (issue #201).
        for key in body.as_object().unwrap().keys() {
            assert!(
                [
                    "activity",
                    "context_pct",
                    "cost_usd",
                    "cursor",
                    "latest",
                    "remaining",
                    "requests",
                    "residency",
                    "resolved_requests",
                    "tokens_total",
                    "truncated",
                    "turns"
                ]
                .contains(&key.as_str()),
                "unexpected field `{key}` in a waited read: {body}"
            );
        }
    }

    /// The two branches that must NOT hold: nothing in flight answers now, and
    /// a wait that runs out answers the ordinary body with `activity:working`
    /// (the turn is untouched — a timeout never cancels anything).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn agent_read_wait_returns_at_once_when_idle_and_says_working_on_timeout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        // 30s before the first event: long enough that only the wait can end.
        let (gw, principal, _drx) =
            build_dispatch_gateway(true, false, 30_000, None, tmp.path()).await;
        // The activity resolver is the same one the web list uses, and it needs
        // the daemon's progress projection to answer anything but "idle".
        gw.lock().await.enable_project_creation(paths.clone());
        let child = parse(
            &run_agent_spawn(
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

        // Idle target: a 240s wait costs nothing at all.
        let started = std::time::Instant::now();
        let idle = parse(
            &run_agent_read(
                &ambient(&principal, "alpha", json!({ "sid": &child, "wait": 240 })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "an idle target must answer immediately: {idle}"
        );
        assert!(idle["turns"].as_array().unwrap().is_empty(), "{idle}");

        // Mid-turn target, wait exhausted: the ordinary body, honestly working.
        dispatch_task(
            &gw,
            "agent",
            &principal,
            &child,
            "slow job".to_string(),
            0,
            NotifyRequest::defaulted(),
            None,
            ccteam_harness::TurnRouting::Inject,
            None,
            crate::gateway::GatewayDeadline::start(),
        )
        .await
        .unwrap();
        let timed_out = parse(
            &run_agent_read(
                &ambient(&principal, "alpha", json!({ "sid": &child, "wait": 1 })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        assert_eq!(timed_out["activity"], "working", "{timed_out}");
        assert!(timed_out.get("waited").is_none(), "{timed_out}");
        assert!(
            gw.lock().await.session_turn_in_flight(&child),
            "a lapsed wait never cancels the turn"
        );
    }

    /// The codex trap, mirrored from the dispatch-wait side: an interim
    /// assistant frame arrives INSIDE the still-running turn, and returning on
    /// it would hand back a checkpoint note as the answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn agent_read_wait_does_not_return_on_an_interim_frame() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gw, principal, _drx) = build_dispatch_gateway(true, true, 150, None, tmp.path()).await;
        let child = parse(
            &run_agent(
                &ambient(&principal, "alpha", json!({ "task": "narrated job" })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let body = parse(
            &run_agent_read(
                &ambient(&principal, "alpha", json!({ "sid": &child, "wait": 20 })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        let last = body["turns"]
            .as_array()
            .unwrap()
            .last()
            .and_then(|turn| turn["content"].as_str())
            .unwrap_or_default();
        assert!(
            last.contains("echo: narrated job"),
            "the wait must run past the narration to the real answer: {body}"
        );
    }

    /// D4 — a long poll that returns AT the boundary leaves the parent holding
    /// the answer, so the completion notification would be a second copy: the
    /// reader's own request is resolved by the boundary it returned at, and it
    /// is told WHICH request that was. A reader that is not the request's
    /// parent must not touch it.
    ///
    /// The notifier runs, as it does in production: it is the single writer of
    /// request resolution, and the reader's claim is what stops it pushing a
    /// copy of the answer this read is returning (issue #201).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn agent_read_wait_disarms_only_the_watching_parents_own_notification() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gw, principal) = dispatch_gateway_opts(true, false, 150, None, tmp.path()).await;
        let child = parse(
            &run_agent(
                &ambient(&principal, "alpha", json!({ "task": "first job" })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            gw.lock()
                .await
                .parent_holds_delegation_request(&child, &principal),
            "the dispatch recorded the parent's request"
        );
        let read = parse(
            &run_agent_read(
                &ambient(&principal, "alpha", json!({ "sid": &child, "wait": 20 })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        // The read names WHICH of the caller's tasks it resolved, so the
        // answer it is holding is never mistaken for another one (issue #201).
        assert_eq!(
            read["resolved_requests"].as_array().map(Vec::len),
            Some(1),
            "{read}"
        );
        // The notifier resolves off the pump, so give it the moment it needs.
        let mut released = false;
        for _ in 0..200 {
            if !gw
                .lock()
                .await
                .parent_holds_delegation_request(&child, &principal)
            {
                released = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            released,
            "the parent read the answer inline — no redundant notification"
        );

        // Re-arm, then let a THIRD session read the same child: its own answer
        // is none of that reader's business to suppress.
        let stranger = parse(
            &run_agent_spawn(
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
        dispatch_task(
            &gw,
            "agent",
            &principal,
            &child,
            "second job".to_string(),
            0,
            NotifyRequest::defaulted(),
            None,
            ccteam_harness::TurnRouting::Inject,
            None,
            crate::gateway::GatewayDeadline::start(),
        )
        .await
        .unwrap();
        run_agent_read(
            &ambient(&stranger, "alpha", json!({ "sid": &child, "wait": 20 })),
            &gw,
            McpCaller::Ambient,
            &paths,
        )
        .await
        .unwrap();
        // The invariant is the NOTIFICATION, not the outstanding row: the
        // boundary resolves the request either way (that is what a boundary
        // does), and what a third party must not do is take the parent's copy
        // of the answer. The parent read its FIRST task inline, so this is the
        // only notification it can ever have been owed.
        let notes = await_notifications(tmp.path(), &principal, 1).await;
        assert_eq!(
            notes.len(),
            1,
            "a third-party reader must not take the parent's notification away: {notes:?}"
        );
    }

    /// The server-side deadline has to clear the wait the caller asked for, or
    /// a long poll is cut short by a "the daemon is busy" timeout at 15s.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_long_read_wait_is_not_cut_short_by_the_server_deadline() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _drx) =
            build_dispatch_gateway(true, false, 60_000, None, tmp.path()).await;
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let child = parse(
            &run_agent(
                &ambient(&principal, "alpha", json!({ "task": "very slow" })),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // The read tool's budget is 15s + the requested wait; a 240s wait must
        // therefore not resolve to the flat 15s the read used to get.
        assert_eq!(
            read_wait_seconds(&json!({ "sid": &child, "wait": 240 })),
            240
        );
        // Proof at the dispatch layer: a 3s wait survives past 15s of nothing
        // happening only because the budget moved with it (the wait itself
        // still lapses at 3s and answers the ordinary working body).
        let secret = gw
            .lock()
            .await
            .principals()
            .credential_for_managed_attach(&principal)
            .expect("the caller holds a minted principal")
            .0;
        let started = std::time::Instant::now();
        let response = execute_session_tool_with_paths(
            &call(
                "agent_read",
                json!({
                    "sid": &child,
                    "wait": 3,
                    "_caller_sid": &principal,
                    "_caller_secret": secret,
                }),
            ),
            Some(&gw),
            McpCaller::Ambient,
            &paths,
        )
        .await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_secs(3),
            "the wait was honoured: {elapsed:?}"
        );
        let body = parse(response["result"]["content"][0]["text"].as_str().unwrap());
        assert!(body["turns"].as_array().is_some(), "{body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn capped_dispatch_wait_returns_honest_pending_and_keeps_child_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gateway, principal) = dispatch_gateway(true, 10_000, tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
                "agent",
                &principal,
                &child,
                "slow task".to_string(),
                1,
                NotifyRequest::defaulted(),
                None,
                ccteam_harness::TurnRouting::Inject,
                None,
                crate::gateway::GatewayDeadline::start(),
            )
            .await
            .expect("a capped inline timeout is a normal pending response"),
        );
        // A lapsed wait says what it knows: the task has not answered, and
        // where the message actually got to. One flat `pending` for both
        // "running" and "third in a queue" is what made a parent re-send the
        // same instruction three times (issue #201).
        assert_eq!(response["answered"], serde_json::json!(false));
        assert_eq!(response["status"], "started");
        assert_eq!(response["state"], "submitted");
        assert_eq!(response["sid"], child);
        assert!(response["request_id"].as_str().is_some());
        assert!(response["turn_id"].as_str().is_some());
        assert!(response.get("requested_wait_seconds").is_none());
        assert!(response.get("effective_wait_seconds").is_none());
        // Still no prose: the caller already knows what to do with a sid, and
        // a hint is bytes it did not ask for.
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
        let req = call("agent_read", json!({ "_caller_admin_token": "tok-abc123" }));
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
        let req = call("agent_read", json!({ "_caller_admin_token": "wrong" }));
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
        assert!(req
            .pointer("/params/arguments/_caller_admin_token")
            .is_none());

        // No token arg → Ambient, request untouched.
        let req = call("agent_read", json!({ "_caller_sid": "s1" }));
        let (req, caller) = d.promote_local_admin(req);
        assert_eq!(caller, McpCaller::Ambient);
        assert_eq!(
            req.pointer("/params/arguments/_caller_sid"),
            Some(&json!("s1"))
        );

        // Token file absent on the daemon → Ambient even with an arg.
        let tmp2 = tempfile::TempDir::new().unwrap();
        let d2 = dispatch_with_root(tmp2.path());
        let req = call("agent_read", json!({ "_caller_admin_token": "anything" }));
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

    /// A stub that owns a real turn FIFO: one turn runs, the rest wait, and a
    /// test releases them one at a time. What that buys the request tests
    /// (issue #201): three tasks are genuinely outstanding at once, each with
    /// its own execution-turn identity, and the boundary of one is a fact the
    /// test controls rather than a race with the pump.
    #[derive(Default)]
    struct StubTurnQueue {
        seq: std::sync::atomic::AtomicUsize,
        /// Per thread identity, so a parent and its child do not share a FIFO.
        state: std::sync::Mutex<std::collections::HashMap<String, StubTurnQueueState>>,
    }

    #[derive(Default)]
    struct StubTurnQueueState {
        active: Option<String>,
        waiting: std::collections::VecDeque<String>,
        parked: std::collections::HashMap<String, Vec<ccteam_harness::ThreadEvent>>,
    }

    impl StubTurnQueue {
        /// Reserve the turn a message will run in — started when idle, and
        /// mid-turn whatever the ROUTING asked for: joined to the turn already
        /// running (`Inject`, so several messages share one execution turn, as
        /// they do on claude / grok) or queued with a 1-based position behind
        /// whatever is already waiting (`Queue`).
        fn claim(
            &self,
            identity: &str,
            routing: ccteam_harness::TurnRouting,
        ) -> (String, ccteam_harness::TurnDisposition, Option<usize>) {
            let n = self
                .seq
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .saturating_add(1);
            let id = format!("x{n}");
            let mut states = self.state.lock().unwrap();
            let state = states.entry(identity.to_string()).or_default();
            match state.active.clone() {
                None => {
                    state.active = Some(id.clone());
                    (id, ccteam_harness::TurnDisposition::Started, None)
                }
                Some(active) if routing == ccteam_harness::TurnRouting::Inject => {
                    (active, ccteam_harness::TurnDisposition::Injected, None)
                }
                Some(_) => {
                    state.waiting.push_back(id.clone());
                    let position = state.waiting.len();
                    (id, ccteam_harness::TurnDisposition::Queued, Some(position))
                }
            }
        }

        fn park(&self, identity: &str, turn_id: &str, events: Vec<ccteam_harness::ThreadEvent>) {
            self.state
                .lock()
                .unwrap()
                .entry(identity.to_string())
                .or_default()
                .parked
                .insert(turn_id.to_string(), events);
        }

        /// Run the turn in flight to its boundary and hand the queue to the
        /// next one. Returns its events, in order.
        fn run_active(&self, identity: &str) -> Vec<ccteam_harness::ThreadEvent> {
            let mut states = self.state.lock().unwrap();
            let Some(state) = states.get_mut(identity) else {
                return Vec::new();
            };
            let Some(active) = state.active.take() else {
                return Vec::new();
            };
            state.active = state.waiting.pop_front();
            state.parked.remove(&active).unwrap_or_default()
        }
    }

    #[derive(Clone, Default)]
    struct StubAdapter {
        spawns: std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
        /// When set, submissions take a real FIFO ([`StubTurnQueue`]) instead
        /// of completing the moment they are accepted.
        queue: Option<std::sync::Arc<StubTurnQueue>>,
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
        /// Model a vendor with no injection channel: a mid-turn `Inject` is
        /// safely served as a distinct queued turn, and the receipt says
        /// `queued` — the disposition is what the adapter DID, never an echo
        /// of what was asked for (issue #197 D).
        degrade_inject: bool,
        /// Run a STARTED turn to its boundary from inside `submit_turn_routed`,
        /// then hold the call open long enough for the pump and the notifier to
        /// have processed it. The pathological ordering behind issue #197: a
        /// child that answers before the dispatcher has bound its request.
        complete_inside_submit: bool,
        events: std::sync::Arc<
            tokio::sync::Mutex<std::collections::VecDeque<(String, ccteam_harness::ThreadEvent)>>,
        >,
        notify: std::sync::Arc<tokio::sync::Notify>,
        spawn_barrier: Option<std::sync::Arc<StubSpawnBarrier>>,
    }

    impl StubAdapter {
        /// Run the identity's in-flight turn to its boundary: its parked events
        /// reach the pump, and the next queued turn takes over.
        async fn run_next_turn(&self, identity: &str) {
            let events = self
                .queue
                .as_ref()
                .expect("a queueing stub")
                .run_active(identity);
            let mut queued = self.events.lock().await;
            for event in events {
                queued.push_back((identity.to_string(), event));
            }
            drop(queued);
            // `notify_waiters` (not `notify_one`): one shared cell serves every
            // session's pump here, and a permit handed to the wrong waiter
            // stalls the pump the event was for.
            self.notify.notify_waiters();
        }
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
                            conclusion: None,
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
            routing: ccteam_harness::TurnRouting,
        ) -> std::result::Result<ccteam_harness::TurnSubmission, ccteam_harness::HarnessError>
        {
            let Some(queue) = self.queue.clone() else {
                return self
                    .submit_turn(h, input)
                    .await
                    .map(ccteam_harness::TurnSubmission::started);
            };
            let text = match input {
                ccteam_harness::TurnInput::UserText(t) => t,
                _ => String::new(),
            };
            let routing = if self.degrade_inject {
                ccteam_harness::TurnRouting::Queue
            } else {
                routing
            };
            let (turn_id, disposition, position) = queue.claim(&h.identity, routing);
            // An injected message joins a turn whose events are already parked;
            // re-parking would overwrite the answer that turn is going to give.
            if disposition != ccteam_harness::TurnDisposition::Injected {
                queue.park(
                    &h.identity,
                    &turn_id,
                    vec![
                        ccteam_harness::ThreadEvent::TurnStarted {
                            turn_id: turn_id.clone(),
                        },
                        ccteam_harness::ThreadEvent::ItemCompleted {
                            item: ccteam_harness::ThreadItem {
                                id: format!("msg-{turn_id}"),
                                details: ccteam_harness::ThreadItemDetails::AgentMessage(format!(
                                    "echo: {text}"
                                )),
                            },
                        },
                        ccteam_harness::ThreadEvent::TurnCompleted {
                            turn_id: turn_id.clone(),
                            usage: Default::default(),
                            model: None,
                            conclusion: None,
                        },
                    ],
                );
            }
            if self.complete_inside_submit
                && disposition == ccteam_harness::TurnDisposition::Started
            {
                self.run_next_turn(&h.identity).await;
                // The submit has not returned yet, so the caller cannot have
                // bound anything. Long enough for the pump to translate the
                // boundary and the notifier to reach it.
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            let turn_id = ccteam_harness::TurnId::new(turn_id);
            Ok(match disposition {
                ccteam_harness::TurnDisposition::Queued => {
                    ccteam_harness::TurnSubmission::queued_at(
                        turn_id,
                        position.expect("a queued claim reports its position"),
                    )
                }
                ccteam_harness::TurnDisposition::Injected => {
                    ccteam_harness::TurnSubmission::injected(turn_id)
                }
                ccteam_harness::TurnDisposition::Started => {
                    ccteam_harness::TurnSubmission::started(turn_id)
                }
            })
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
            "agent_read",
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
    /// the gate and the call reaches the gateway (agent_read returns rows).
    #[tokio::test]
    async fn execute_session_tool_allows_correct_principal() {
        let (gw, cto_sid, _beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "agent_read",
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
    /// (cross-project). The scope comes from the SERVER-resolved CallerCtx.slug,
    /// and the refusal says only what the caller may know: the sid is not one
    /// of ITS sessions.
    #[tokio::test]
    async fn execute_session_tool_rejects_cross_project_sid() {
        let (gw, cto_sid, beta_sid, cto_secret) = gateway_with_cto_and_cross_project().await;
        for tool in ["agent", "agent_read", "agent_stop"] {
            let mut args = json!({
                "_caller_sid": cto_sid.clone(),
                "_caller_secret": cto_secret.clone(),
                "sid": beta_sid.clone(),
            });
            if tool == "agent" {
                args["task"] = json!("do something");
            }
            let resp = execute_session_tool(&call(tool, args), Some(&gw), McpCaller::Ambient).await;
            assert_eq!(resp["result"]["isError"], true, "{tool} must reject");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert_eq!(
                text,
                format!("{tool}: unknown session {beta_sid} in project alpha"),
                "{tool}: a foreign sid is refused without naming its project"
            );
            assert!(!text.contains("beta"), "{tool} leaked the target project");
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
                "agent_read",
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
            text.ends_with("in project alpha"),
            "server must use CallerCtx.slug (alpha), not the spoofed `beta`, got: {text}"
        );
        assert!(
            !text.contains("beta"),
            "the spoofed slug is never echoed back"
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
                "agent_read",
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
        assert!(is_session_tool_call(&call("agent", json!({}))));
        assert!(is_session_tool_call(&call(
            "agent_read",
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
            "params": { "name": "agent" }
        })));
    }

    /// v0.9.0 W1 (F1) — an Ambient caller whose `(sid, secret)` principal
    /// resolves to no live session is REJECTED (needs a gateway to check).
    #[tokio::test]
    async fn execute_session_tool_ambient_denies_unknown_principal() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let req = call(
            "agent_read",
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
            "agent_read",
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
        let req = call("agent_read", json!({}));
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

    /// v0.9 T4 — admin `agent_read` works with NO ambient args and reaches the
    /// live gateway (fleet-wide semantics, same as the web admin Identity).
    #[tokio::test]
    async fn execute_session_tool_admin_lists_sessions_fleet_wide() {
        let (gw, _cto_sid, _beta_sid, _cto_secret) = gateway_with_cto_and_cross_project().await;
        let resp =
            execute_session_tool(&call("agent_read", json!({})), Some(&gw), McpCaller::Admin).await;
        assert_eq!(
            resp["result"]["isError"], false,
            "admin bypasses the principal gate: {resp}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"sessions\""), "got: {text}");
    }

    /// `task_file` is one field folded into another before anything else looks
    /// at the call. What it must never do is change WHAT the child receives.
    #[test]
    fn a_task_file_becomes_the_task_and_refuses_every_ambiguity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let brief_path = tmp.path().join("brief-MM-298.md");
        let brief = "【派发词·MM-298·checker】\n工位（只读）：/home/ubuntu/wt/MM-298\n";
        std::fs::write(&brief_path, brief).unwrap();
        let at = |value: serde_json::Value| resolve_task_file(&value);

        // The overwhelmingly common call: untouched, not even cloned.
        assert!(at(json!({"task": "inline"})).unwrap().is_none());

        let rewritten = at(json!({"sid": "s7", "task_file": brief_path.to_str().unwrap()}))
            .unwrap()
            .expect("the file became the task");
        assert_eq!(rewritten["task"], json!(brief), "verbatim, byte for byte");
        assert!(
            rewritten.get("task_file").is_none(),
            "the pointer does not travel with the task"
        );
        assert_eq!(rewritten["sid"], json!("s7"), "other arguments survive");

        // Two sources for one field: refused, never silently picked — which of
        // the two won would not be visible in any transcript afterwards.
        assert!(
            at(json!({"task": "inline", "task_file": brief_path.to_str().unwrap()}))
                .unwrap_err()
                .contains("not both")
        );
        // A relative path would resolve against the DAEMON's cwd, not the
        // caller's, and quietly read the wrong file.
        assert!(at(json!({"task_file": "brief-MM-298.md"}))
            .unwrap_err()
            .contains("absolute"));
        assert!(at(json!({"task_file": ""}))
            .unwrap_err()
            .contains("non-empty"));
        assert!(at(json!({"task_file": tmp.path().join("nope.md").to_str().unwrap()})).is_err());

        let empty = tmp.path().join("empty.md");
        std::fs::write(&empty, "   \n\t\n").unwrap();
        assert!(at(json!({"task_file": empty.to_str().unwrap()}))
            .unwrap_err()
            .contains("empty"));

        let fat = tmp.path().join("corpus.md");
        std::fs::write(&fat, vec![b'x'; TASK_FILE_MAX_BYTES as usize + 1]).unwrap();
        assert!(at(json!({"task_file": fat.to_str().unwrap()}))
            .unwrap_err()
            .contains("cap"));
    }

    /// End to end: the bytes reach the child's transcript exactly as an inline
    /// `task` would have, while the caller's own context carried a path.
    #[tokio::test]
    async fn a_task_file_lands_in_the_child_exactly_as_an_inline_task_would() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let brief =
            "【派发词·MM-298·checker】\n工位（只读）：/home/ubuntu/wt/MM-298\n回报 ≤8 行。\n";
        let brief_path = tmp.path().join("brief-MM-298.md");
        std::fs::write(&brief_path, brief).unwrap();

        let hire = |args: serde_json::Value| {
            let gateway = &gateway;
            let paths = &paths;
            async move {
                let response = execute_session_tool_with_paths(
                    &call("agent", args),
                    Some(gateway),
                    McpCaller::User {
                        user_id: "ualice".into(),
                    },
                    paths,
                )
                .await;
                assert_eq!(response["result"]["isError"], false, "{response}");
                let body: serde_json::Value = serde_json::from_str(
                    response["result"]["content"][0]["text"].as_str().unwrap(),
                )
                .unwrap();
                body["sid"].as_str().unwrap().to_string()
            }
        };

        let by_file = hire(json!({
            "project": "alice",
            "vendor": "claude",
            "task_file": brief_path.to_str().unwrap(),
        }))
        .await;
        let by_inline = hire(json!({
            "project": "alice",
            "vendor": "claude",
            "task": brief,
        }))
        .await;

        let alice_dir = paths.projects_root.join("alice");
        let first_user = |sid: &str| {
            ccteam_harness::execution::turns_mirror::read_all_turns(&alice_dir, sid)
                .unwrap()
                .into_iter()
                .find_map(|turn| (!turn.user.is_empty()).then_some(turn.user))
        };
        assert_eq!(
            first_user(&by_file).as_deref(),
            Some(brief.trim()),
            "the child got the file's bytes, through the SAME trim an inline \
             task goes through — one normalization, not two"
        );
        assert_eq!(
            first_user(&by_file),
            first_user(&by_inline),
            "a path and an inline brief are the same delegation"
        );
    }

    #[tokio::test]
    async fn user_spawn_is_root_owned_by_tenant_and_spoofed_caller_fields_are_ignored() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, _alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let req = call(
            "agent",
            json!({
                "project": "alice",
                "vendor": "claude",
                "task": "hello",
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
        assert!(body["sid"].is_string());

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
                "agent",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                    "task": "hello",
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
                "agent",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                    "task": "hello",
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
        assert!(body["sid"].is_string(), "{body}");
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
                "agent",
                json!({
                    "project": "alice",
                    "vendor": "claude",
                    "task": "hello",
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
        let response = run_agent_spawn_at(
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

    /// An Ambient caller (the flow runner's shape: an enrolled node acting
    /// FOR a managed session) may attribute its hire to a live session in
    /// its own project — the tree mounts under the declared parent.
    #[tokio::test]
    async fn ambient_declared_parent_in_own_project_takes_the_edge() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let response = run_agent_spawn_at(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "vendor": "claude", "parent_sid": alice_sid }),
            ),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&response).unwrap();
        let first = body["sid"].as_str().unwrap().to_string();

        // A sibling enrolled-style caller now declares the FIRST child as
        // parent: the edge lands on the declared session, not the caller.
        let response = run_agent_spawn_at(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "vendor": "claude", "parent_sid": first }),
            ),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&response).unwrap();
        let child_meta = ccteam_harness::execution::session_meta::read_session_meta(
            &paths.projects_root.join("alice"),
            body["sid"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(child_meta.parent_sid.as_deref(), Some(first.as_str()));
    }

    /// Unknown declared parent: loud error, never a silent fallback.
    #[tokio::test]
    async fn ambient_declared_parent_unknown_is_a_loud_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, _bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let err = run_agent_spawn_at(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "vendor": "claude", "parent_sid": "s404" }),
            ),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap_err();
        assert!(err.contains("not a live session in your project"), "{err}");
    }

    /// Cross-project attribution is refused with the SAME error an unknown
    /// sid gets: a child's completion notification lands on its parent (an
    /// injection channel), and a distinguishing refusal would let monotonic
    /// sids enumerate other projects' sessions.
    #[tokio::test]
    async fn ambient_declared_parent_cross_project_is_an_indistinguishable_miss() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let err = run_agent_spawn_at(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "vendor": "claude", "parent_sid": bob_sid }),
            ),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap_err();
        assert!(err.contains("not a live session in your project"), "{err}");
        assert!(
            !err.contains("bob"),
            "must not name the foreign project: {err}"
        );
    }

    /// issue #8 — the red line "resume-by-sid survives restarts": a released
    /// session must stay dispatchable after the daemon process is rebuilt
    /// over the same state (the old live-only gate answered "unknown").
    #[tokio::test]
    async fn released_sessions_stay_dispatchable_across_daemon_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, _bob, _admin) =
            gateway_with_tenant_projects(tmp.path()).await;
        let spawned = run_agent_spawn_at(
            &ambient(&alice_sid, "alice", json!({ "vendor": "claude" })),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap();
        let child: serde_json::Value = serde_json::from_str(&spawned).unwrap();
        let child_sid = child["sid"].as_str().unwrap().to_string();
        drop(gateway);

        let gateway2 = rebuilt_gateway_over(tmp.path());
        let response = run_agent_dispatch(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "sid": child_sid, "task": "carry on" }),
            ),
            &gateway2,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let body: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(body["sid"], json!(child_sid), "{body}");
    }

    /// Explicitly STOPPED stays refused across a restart — with its own
    /// words, never "unknown".
    #[tokio::test]
    async fn stopped_targets_refuse_after_restart_with_their_own_words() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, _bob, _admin) =
            gateway_with_tenant_projects(tmp.path()).await;
        let spawned = run_agent_spawn_at(
            &ambient(&alice_sid, "alice", json!({ "vendor": "claude" })),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap();
        let child: serde_json::Value = serde_json::from_str(&spawned).unwrap();
        let child_sid = child["sid"].as_str().unwrap().to_string();
        {
            let mut gw = gateway.lock().await;
            gw.stop_session(&child_sid).await.unwrap();
        }
        drop(gateway);

        let gateway2 = rebuilt_gateway_over(tmp.path());
        let err = run_agent_dispatch(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "sid": child_sid, "task": "carry on" }),
            ),
            &gateway2,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(err.contains("was stopped at"), "{err}");
    }

    /// A hand-started (external) node stays refused across a restart, from
    /// its persisted meta — the live index is empty then.
    #[tokio::test]
    async fn external_targets_refuse_after_restart_from_their_meta() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, _bob, _admin) =
            gateway_with_tenant_projects(tmp.path()).await;
        let spawned = run_agent_spawn_at(
            &ambient(&alice_sid, "alice", json!({ "vendor": "claude" })),
            &gateway,
            McpCaller::Ambient,
            Some(&paths),
        )
        .await
        .unwrap();
        let child: serde_json::Value = serde_json::from_str(&spawned).unwrap();
        let child_sid = child["sid"].as_str().unwrap().to_string();
        let project_dir = paths.projects_root.join("alice");
        let mut meta =
            ccteam_harness::execution::session_meta::read_session_meta(&project_dir, &child_sid)
                .unwrap();
        meta.managed_by = ccteam_harness::execution::session_meta::ManagedBy::External;
        ccteam_harness::execution::session_meta::write_session_meta(&project_dir, &meta).unwrap();
        drop(gateway);

        let gateway2 = rebuilt_gateway_over(tmp.path());
        let err = run_agent_dispatch(
            &ambient(
                &alice_sid,
                "alice",
                json!({ "sid": child_sid, "task": "carry on" }),
            ),
            &gateway2,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(err.contains("hand-started"), "{err}");
    }

    /// Restart double: a fresh Gateway over the same on-disk state, exactly
    /// what a daemon restart is (processes are not respawned; state is).
    fn rebuilt_gateway_over(tmp: &std::path::Path) -> GatewayHandle {
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
        let mut gateway = Gateway::new_with_factory(factory, "alice", tmp.join("projects/alice"));
        mark_stub_vendors_installed(&mut gateway);
        gateway.register_project("bob", tmp.join("projects/bob"));
        gateway.register_project("admin", tmp.join("projects/admin"));
        std::sync::Arc::new(tokio::sync::Mutex::new(gateway))
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
                "agent",
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
            &call("agent", json!({"vendor": "claude"})),
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
                &call("agent", json!({"project": project})),
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
            assert!(text.starts_with("agent: project not found"), "{text}");
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
            &call("agent", json!({"vendor": "claude", "task": "hello"})),
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
        assert!(body["sid"].is_string(), "spawn response: {body}");
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
        let err = run_agent_spawn_at(
            &json!({"project": "mychat", "vendor": "claude"}),
            &gw,
            McpCaller::Admin,
            Some(&paths),
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("agent: unknown project: mychat"), "{err}");
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

        let err = run_agent_spawn_at(
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
            &run_agent_spawn_at(
                &json!({"vendor": "claude"}),
                &gw,
                McpCaller::Admin,
                Some(&paths),
            )
            .await
            .unwrap(),
        );
        assert!(body["sid"].is_string(), "{body}");
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

        for (args, caller, _source, _project) in [
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
                &run_agent_spawn_at(&args, &gw, caller, Some(&paths))
                    .await
                    .unwrap(),
            );
            assert!(body["sid"].is_string(), "{body}");
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
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        for args in [
            json!({"vendor": "grok", "task": "x", "protocol": "acp"}),
            json!({"vendor": "grok", "task": "x", "protocol": "stream-json"}),
            json!({"vendor": "claude", "task": "x", "protocol": "terminal"}),
            json!({"vendor": "claude", "task": "x", "protocol": "bogus"}),
            json!({"vendor": "claude", "task": "x", "protocol": null}),
        ] {
            let err = run_agent(&args, &gw, McpCaller::Admin, &paths)
                .await
                .unwrap_err();
            assert_eq!(err, PROTOCOL_SPAWN_PARAM_REMOVED);
        }
    }

    /// The retired parameter names are HARD errors that say what replaced
    /// them: a silently-ignored `wait_seconds` is a caller that thinks it is
    /// blocking and is not.
    #[tokio::test]
    async fn agent_rejects_retired_parameters_and_requires_a_task() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let run = |args: serde_json::Value| {
            let gw = gw.clone();
            let paths = paths.clone();
            async move { run_agent(&args, &gw, McpCaller::Admin, &paths).await }
        };
        assert!(run(json!({"task": "x", "host": "sat"}))
            .await
            .unwrap_err()
            .contains("host is bound to the project"));
        assert_eq!(
            run(json!({"task": "x", "wait_seconds": 30}))
                .await
                .unwrap_err(),
            "agent: `wait_seconds` was renamed to `wait`"
        );
        assert_eq!(
            run(json!({"vendor": "claude"})).await.unwrap_err(),
            "agent: missing `task` — say what the agent should do (or point `task_file` at it)"
        );
        assert_eq!(
            run(json!({"task": "  "})).await.unwrap_err(),
            "agent: missing `task` — say what the agent should do (or point `task_file` at it)"
        );
        // A follow-up may not reconfigure the session it is only messaging.
        assert_eq!(
            run(json!({"task": "x", "sid": "s9", "vendor": "codex", "model": "o"}))
                .await
                .unwrap_err(),
            "agent: `sid` names an existing session — drop vendor/model or omit `sid` to hire"
        );
        // `agent_read` renamed `limit` to `n` for the same reason.
        assert_eq!(
            run_agent_read(&json!({"limit": 5}), &gw, McpCaller::Admin, &paths)
                .await
                .unwrap_err(),
            "agent_read: `limit` was renamed to `n`"
        );
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
            &run_agent_spawn(
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
            "agent",
            &principal,
            &child,
            "quick question".to_string(),
            6,
            NotifyRequest::defaulted(),
            None,
            ccteam_harness::TurnRouting::Inject,
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
                "request_id",
                "result_text",
                "sid",
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

        for tool in ["agent", "agent_read", "agent_stop"] {
            let invoke = |sid: &str| {
                let mut args = json!({"sid": sid});
                if tool == "agent" {
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

    /// P1-2 — an ambient caller must not learn that a sid it cannot reach
    /// exists, let alone which project it runs in: `agent_read` answered
    /// "s1 runs in project pm" for a sid `agent` and `agent_stop` called
    /// unknown, which made every sid enumerable across tenants (2026-08-31).
    /// All three tools now answer from one builder.
    #[tokio::test]
    async fn ambient_foreign_and_unknown_sid_errors_are_identical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, bob_sid, _admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let secret = gateway
            .lock()
            .await
            .principals()
            .credential_for_managed_attach(&alice_sid)
            .expect("alice holds a minted principal")
            .0;

        for tool in ["agent", "agent_read", "agent_stop"] {
            let invoke = |sid: &str| {
                let mut args = json!({
                    "sid": sid,
                    "_caller_sid": alice_sid.clone(),
                    "_caller_secret": secret.clone(),
                });
                if tool == "agent" {
                    args["task"] = json!("do not leak");
                }
                call(tool, args)
            };
            let foreign = execute_session_tool_with_paths(
                &invoke(&bob_sid),
                Some(&gateway),
                McpCaller::Ambient,
                &paths,
            )
            .await;
            let unknown = execute_session_tool_with_paths(
                &invoke("s999999"),
                Some(&gateway),
                McpCaller::Ambient,
                &paths,
            )
            .await;
            assert_eq!(foreign["result"]["isError"], true, "{tool}: {foreign}");
            assert_eq!(unknown["result"]["isError"], true, "{tool}: {unknown}");
            let foreign_text = foreign["result"]["content"][0]["text"].as_str().unwrap();
            let unknown_text = unknown["result"]["content"][0]["text"].as_str().unwrap();
            assert_eq!(
                foreign_text.replace(&bob_sid, "<sid>"),
                unknown_text.replace("s999999", "<sid>"),
                "{tool}: a foreign sid and one that never existed must read alike"
            );
            assert!(
                !foreign_text.contains("bob"),
                "{tool} named another tenant's project: {foreign_text}"
            );
        }
    }

    /// P1-3 — an ambient caller's roster is its OWN project. It used to span
    /// every project the owner can see (measured `total: 1272`, the caller's
    /// own seven rows pushed past the default page), while NAMING another
    /// project was refused: unasked-for breadth plus a refusal for the honest
    /// request.
    #[tokio::test]
    async fn ambient_roster_is_scoped_to_the_callers_own_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, bob_sid, admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let secret = gateway
            .lock()
            .await
            .principals()
            .credential_for_managed_attach(&alice_sid)
            .expect("alice holds a minted principal")
            .0;
        let roster = |project: Option<&str>| {
            let mut args = json!({
                "_caller_sid": alice_sid.clone(),
                "_caller_secret": secret.clone(),
            });
            if let Some(project) = project {
                args["project"] = json!(project);
            }
            let request = call("agent_read", args);
            let gateway = std::sync::Arc::clone(&gateway);
            let paths = paths.clone();
            async move {
                execute_session_tool_with_paths(
                    &request,
                    Some(&gateway),
                    McpCaller::Ambient,
                    &paths,
                )
                .await
            }
        };

        for scope in [None, Some("alice")] {
            let response = roster(scope).await;
            assert_eq!(response["result"]["isError"], false, "{response}");
            let body = parse(response["result"]["content"][0]["text"].as_str().unwrap());
            let sids: Vec<&str> = body["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|session| session["sid"].as_str())
                .collect();
            assert_eq!(sids, vec![alice_sid.as_str()], "{scope:?}: {body}");
            assert!(!sids.contains(&bob_sid.as_str()));
            assert!(!sids.contains(&admin_sid.as_str()));
            assert!(body.get("total").is_none(), "nothing was cut: {body}");
        }

        // Naming somebody else's project keeps the refusal it always had.
        let refused = roster(Some("bob")).await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert_eq!(
            refused["result"]["content"][0]["text"],
            json!("agent_read: project not found — this session works in `alice`")
        );
    }

    #[tokio::test]
    async fn user_agent_read_filters_to_owned_projects_and_overwrites_spoofed_scope() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (paths, gateway, alice_sid, bob_sid, admin_sid) =
            gateway_with_tenant_projects(tmp.path()).await;
        let response = execute_session_tool_with_paths(
            &call(
                "agent_read",
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
        assert!(body.get("total").is_none());
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
        assert!(!is_permission_ask_call(&call("agent", json!({}))));
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

    // ---------- v0.8.7 review-fix (R-L3) agent_read paging ----------

    pub(super) fn turn(id: &str) -> ccteam_harness::execution::turns_mirror::TurnRecord {
        ccteam_harness::execution::turns_mirror::TurnRecord {
            exec_turn_id: None,
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
            conclusion: None,
        }
    }

    /// A burst of MORE than `n` turns after the cursor must NOT silently drop
    /// the middle: `page_collected_turns` returns the OLDEST `n`, sets the
    /// cursor to that page's boundary, and counts the `remaining` so a
    /// follow-up poll fetches the rest. Walking the cursor returns EVERY turn
    /// in order.
    #[test]
    fn page_collected_turns_pages_a_burst_without_loss() {
        let all: Vec<_> = (0..25).map(|i| turn(&format!("t{i}"))).collect();
        // First poll, no cursor, page size 10.
        let page = page_collected_turns(&all, None, 10, false);
        assert_eq!(page.rows.len(), 10);
        assert_eq!(page.remaining, 15, "25 − 10 still to read");
        assert_eq!(
            page.rows[0]["turn_id"], "t0",
            "oldest-first (not the newest 10)"
        );
        assert_eq!(page.rows[9]["turn_id"], "t9");
        assert_eq!(page.cursor.as_deref(), Some("t9"), "cursor = boundary turn");
        assert_eq!(
            page.latest.as_deref(),
            Some("t24"),
            "the newest turn is named whatever the page shows"
        );

        // Second poll from the boundary.
        let page2 = page_collected_turns(&all, Some("t9"), 10, false);
        assert_eq!(page2.rows.len(), 10);
        assert_eq!(page2.remaining, 5);
        assert_eq!(
            page2.rows[0]["turn_id"], "t10",
            "no gap — resumes right after t9"
        );
        assert_eq!(page2.cursor.as_deref(), Some("t19"));

        // Third poll drains the remainder.
        let page3 = page_collected_turns(&all, Some("t19"), 10, false);
        assert_eq!(page3.rows.len(), 5);
        assert_eq!(page3.remaining, 0, "final page withholds nothing");
        assert_eq!(page3.rows[0]["turn_id"], "t20");
        assert_eq!(page3.rows[4]["turn_id"], "t24");
        assert_eq!(
            page3.cursor, page3.latest,
            "the last page ends at the newest turn"
        );

        // The three pages reconstruct the full ordered set — zero loss.
        let mut seen: Vec<String> = Vec::new();
        for rows in [&page.rows, &page2.rows, &page3.rows] {
            for r in rows {
                seen.push(r["turn_id"].as_str().unwrap().to_string());
            }
        }
        let expected: Vec<String> = (0..25).map(|i| format!("t{i}")).collect();
        assert_eq!(seen, expected, "every turn returned exactly once, in order");
    }

    /// A short backlog (≤ `n`) returns everything, nothing remaining, cursor =
    /// last turn. An unknown cursor returns everything (never silently lose).
    #[test]
    fn page_collected_turns_short_and_unknown_cursor() {
        let all: Vec<_> = (0..3).map(|i| turn(&format!("t{i}"))).collect();
        let page = page_collected_turns(&all, None, 20, false);
        assert_eq!(page.rows.len(), 3);
        assert_eq!(page.remaining, 0);
        assert_eq!(page.cursor.as_deref(), Some("t2"));
        // Unknown cursor → all turns (defensive, no loss).
        let unknown = page_collected_turns(&all, Some("ghost"), 20, false);
        assert_eq!(unknown.rows.len(), 3);
        assert_eq!(unknown.remaining, 0);
    }

    /// v0.9.1 — `tail:true` returns the NEWEST `n` (chronological inside the
    /// page), the "just give me the final answer" shape; cursor = newest turn.
    #[test]
    fn page_collected_turns_tail_returns_newest() {
        let all: Vec<_> = (0..25).map(|i| turn(&format!("t{i}"))).collect();
        let page = page_collected_turns(&all, None, 3, true);
        assert_eq!(page.rows.len(), 3);
        assert_eq!(page.remaining, 22, "the older 22 are off the page");
        assert_eq!(
            page.rows[0]["turn_id"], "t22",
            "newest 3, oldest of them first"
        );
        assert_eq!(page.rows[2]["turn_id"], "t24", "ends at the newest turn");
        assert_eq!(page.cursor.as_deref(), Some("t24"));
        assert_eq!(
            page.latest, page.cursor,
            "a tail page always ends at the newest turn"
        );
        // `since` still applies before the tail cut.
        let page2 = page_collected_turns(&all, Some("t22"), 5, true);
        assert_eq!(page2.rows.len(), 2, "only t23/t24 exist after t22");
        assert_eq!(page2.remaining, 0);
        assert_eq!(page2.rows[0]["turn_id"], "t23");
    }

    /// issue #194 — the field report: a `since` + `n:1` read is the OLDEST
    /// unread turn, and the body now says so — `remaining` counts what is
    /// still unread and `latest` names the newest turn — so it can never pass
    /// for the newest answer. `n:0` is the body-free status read.
    #[test]
    fn page_collected_turns_says_what_it_withheld() {
        let all: Vec<_> = (7..10).map(|i| turn(&format!("s1587-{i}"))).collect();
        let page = page_collected_turns(&all, Some("s1587-7"), 1, false);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0]["turn_id"], "s1587-8", "oldest unread first");
        assert_eq!(page.cursor.as_deref(), Some("s1587-8"));
        assert_eq!(page.remaining, 1, "one newer turn was withheld");
        assert_eq!(page.latest.as_deref(), Some("s1587-9"));

        let status = page_collected_turns(&all, Some("s1587-7"), 0, false);
        assert!(status.rows.is_empty(), "n:0 carries no text");
        assert_eq!(status.cursor, None);
        assert_eq!(status.remaining, 2, "unread count since the cursor");
        assert_eq!(status.latest.as_deref(), Some("s1587-9"));

        let caught_up = page_collected_turns(&all, Some("s1587-9"), 0, false);
        assert_eq!(caught_up.remaining, 0);
        assert_eq!(caught_up.latest.as_deref(), Some("s1587-9"));
    }

    /// A vendor turn that failed before it said anything writes kind/error and
    /// no text. Paging that row out is not a cosmetic loss: the session then
    /// reads "not working, newest row = the PREVIOUS successful answer", and a
    /// caller polling for the failed task's result gets that answer instead.
    /// The row stays, with `content` as the documented empty string.
    #[test]
    fn page_collected_turns_keeps_a_failed_turn_that_said_nothing() {
        let mut failed = turn("t2");
        failed.assistant = String::new();
        failed.outcome = Some("failed".into());
        failed.error_kind = Some("server_overloaded".into());
        failed.error = Some("Selected model is at capacity.".into());
        let all = vec![turn("t1"), failed];

        let page = page_collected_turns(&all, None, 10, true);
        let (rows, cursor) = (page.rows, page.cursor);
        assert_eq!(rows.len(), 2, "the failure is a row, not a gap: {rows:?}");
        assert_eq!(rows[1]["turn_id"], "t2");
        assert_eq!(rows[1]["content"], "", "empty text, not a dropped row");
        assert_eq!(rows[1]["outcome"], "failed");
        assert_eq!(rows[1]["error_kind"], "server_overloaded");
        assert_eq!(rows[1]["error"], "Selected model is at capacity.");
        assert_eq!(
            cursor.as_deref(),
            Some("t2"),
            "and it is the cursor, so the next poll pages past it",
        );

        // A turn with neither text nor an outcome is still not a row: the
        // user-side half of an exchange has no business in an answer page.
        let mut silent = turn("t3");
        silent.assistant = String::new();
        let quiet = page_collected_turns(&[silent], None, 10, true).rows;
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    /// The same fact through the tool itself: `agent_read` shows the failure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_read_shows_a_failed_turn_that_carried_no_text() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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

        for (turn_id, assistant, failure) in [
            ("answer-1", "the previous answer", None),
            ("answer-2", "", Some(("transport", "connection reset"))),
        ] {
            ccteam_harness::execution::turns_mirror::append_turn(
                tmp.path(),
                &child,
                &ccteam_harness::execution::turns_mirror::TurnRecord {
                    exec_turn_id: None,
                    turn_id: turn_id.into(),
                    ts: chrono::Utc::now(),
                    vendor: "claude".into(),
                    role: String::new(),
                    user: "question".into(),
                    assistant: assistant.into(),
                    usage: serde_json::Value::Null,
                    status: None,
                    tool_calls: Vec::new(),
                    attachments: Vec::new(),
                    outcome: failure.map(|_| "failed".to_string()),
                    error_kind: failure.map(|(kind, _)| kind.to_string()),
                    error: failure.map(|(_, error)| error.to_string()),
                    conclusion: None,
                },
            )
            .unwrap();
        }

        let response = parse(
            &run_agent_read_transcript(
                &ambient(&principal, "alpha", json!({ "sid": child, "tail": true })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        let last = response["turns"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last["turn_id"], "answer-2",
            "the newest row is the failure, not the answer before it: {response}",
        );
        assert_eq!(last["outcome"], "failed");
        assert_eq!(last["error_kind"], "transport");
        assert_eq!(last["error"], "connection reset");
    }

    #[test]
    fn collect_max_chars_defaults_and_clamps() {
        assert_eq!(collect_max_chars(&json!({})), 1_000);
        assert_eq!(collect_max_chars(&json!({ "max_chars": 1 })), 100);
        assert_eq!(collect_max_chars(&json!({ "max_chars": -10 })), 100);
        // The small polling window the field report reached for is honoured,
        // not silently widened.
        assert_eq!(collect_max_chars(&json!({ "max_chars": 300 })), 300);
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
        let recipe = |turn_id: &str, total: usize| format!("read {turn_id} whole ({total})");
        let (total_chars, truncated) = bound_collected_turns(&mut rows, 500, &recipe);
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
        // The marker carries the recipe for THAT turn with ITS full length.
        assert!(excerpt.contains("read t2 whole (908)"), "{excerpt}");
    }

    /// issue #196 — a cut transcript row shows the turn's conclusion (behind
    /// the marker for its narration), and the steering field never reaches
    /// the wire — cut or not.
    #[test]
    fn a_cut_transcript_row_shows_the_conclusion_and_hides_the_steering_field() {
        let narration = "narration ".repeat(80);
        let receipt = "RECEIPT: all green".to_string();
        let long = format!("{narration}\n\n{receipt}");
        let mut rows = vec![
            json!({ "turn_id": "t1", "content": "short", "conclusion": "short" }),
            json!({ "turn_id": "t2", "content": long, "conclusion": receipt }),
        ];
        let recipe = |turn_id: &str, total: usize| format!("read {turn_id} whole ({total})");
        let (_total, truncated) = bound_collected_turns(&mut rows, 300, &recipe);
        assert!(truncated);
        let excerpt = rows[1]["content"].as_str().unwrap();
        assert!(excerpt.ends_with("RECEIPT: all green"), "{excerpt}");
        assert!(excerpt.starts_with("…[+"), "{excerpt}");
        assert!(excerpt.contains("read t2 whole (820)"), "{excerpt}");
        assert!(!excerpt.contains("narration narration"), "{excerpt}");
        for row in &rows {
            assert!(row.get("conclusion").is_none(), "{row}");
        }

        // A page that fits is untouched — except that the steering field is
        // still stripped.
        let mut fits = vec![json!({ "turn_id": "t1", "content": "ok", "conclusion": "ok" })];
        let (_total, truncated) = bound_collected_turns(&mut fits, 300, &recipe);
        assert!(!truncated);
        assert_eq!(fits[0], json!({ "turn_id": "t1", "content": "ok" }));
    }

    /// The recipe on a truncated transcript row names the exact turn, so it
    /// still reads THAT answer after the child has finished others.
    #[test]
    fn whole_turn_recipe_names_the_exact_read() {
        assert_eq!(
            whole_turn_recipe("s5", "t1", 908),
            "agent_read{sid:s5,turn:t1,max_chars:908}"
        );
        // The budget is clamped to what the parameter accepts, never 7.
        assert_eq!(
            whole_turn_recipe("s5", "t0", 7),
            "agent_read{sid:s5,turn:t0,max_chars:100}"
        );
    }

    /// `agent_read{turn}` returns that one row whatever has happened since,
    /// and refuses a turn the transcript does not hold rather than paging to
    /// something else.
    #[test]
    fn exact_turn_selector_returns_only_that_row() {
        let all: Vec<_> = ["t0", "t1", "t2"].iter().map(|id| turn(id)).collect();
        let page = exact_collected_turn(&all, "t1").expect("t1 is in the transcript");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0]["turn_id"], "t1");
        assert_eq!(page.remaining, 0);
        assert_eq!(page.latest.as_deref(), Some("t2"));
        assert!(exact_collected_turn(&all, "ghost").is_none());
    }

    /// issue #195 — a pointer that costs more than the text it withholds makes
    /// the answer bigger AND worse, so the turn comes back whole.
    #[test]
    fn a_pointer_that_costs_more_than_it_saves_is_not_emitted() {
        // 131 chars whole, 103 of budget: the pointer would withhold 28 chars
        // and cost 51 to say so — the exact shape measured in the field.
        let recipe = |_: &str, _: usize| "agent_read{sid:s5,n:1,max_chars:131}".to_string();
        let mut rows = vec![json!({ "turn_id": "t1", "content": "x".repeat(131) })];
        let (_total, truncated) = bound_collected_turns(&mut rows, 103, &recipe);
        assert!(
            !truncated,
            "131 chars whole beats 103 chars of mostly marker"
        );
        assert_eq!(rows[0]["content"].as_str().unwrap().chars().count(), 131);

        // Far past the marker's own cost, truncation is the smaller answer again.
        let mut long = vec![json!({ "turn_id": "t1", "content": "x".repeat(4_000) })];
        let (_total, truncated) = bound_collected_turns(&mut long, 500, &recipe);
        assert!(truncated);
        assert_eq!(long[0]["content"].as_str().unwrap().chars().count(), 500);
    }

    /// issue #195 — a tight budget spread over many rows returns fewer WHOLE
    /// turns (the newest ones) and counts the rest as unread, instead of ten
    /// stubs that are mostly pointer. A page that fits is never touched.
    #[test]
    fn a_tight_budget_drops_whole_rows_instead_of_shredding_every_one() {
        let mut rows: Vec<serde_json::Value> = (0..10)
            .map(|i| json!({ "turn_id": format!("t{i}"), "content": "x".repeat(700) }))
            .collect();
        let dropped = drop_unaffordable_rows(&mut rows, 1_000, true);
        assert_eq!(dropped, 5, "1000/5 = 200 chars a row is the useful floor");
        assert_eq!(rows.len(), 5);
        assert_eq!(
            rows[0]["turn_id"], "t5",
            "the OLDEST rows are the ones shed"
        );
        assert_eq!(rows[4]["turn_id"], "t9", "the newest row always survives");

        // A FORWARD page sheds from the other end: dropping the oldest would
        // walk the caller's cursor past turns it never saw.
        let mut forward: Vec<serde_json::Value> = (0..10)
            .map(|i| json!({ "turn_id": format!("t{i}"), "content": "x".repeat(700) }))
            .collect();
        assert_eq!(drop_unaffordable_rows(&mut forward, 1_000, false), 5);
        assert_eq!(forward[0]["turn_id"], "t0", "the cursor keeps its place");
        assert_eq!(forward[4]["turn_id"], "t4");

        // Ten short turns fit inside the budget: nothing is dropped.
        let mut short: Vec<serde_json::Value> = (0..10)
            .map(|i| json!({ "turn_id": format!("t{i}"), "content": "ok" }))
            .collect();
        assert_eq!(drop_unaffordable_rows(&mut short, 1_000, true), 0);
        assert_eq!(short.len(), 10);
    }

    /// The transcript branch answers "what did it say" with ONE turn; the
    /// roster answers "who is working for me" with a handful of rows.
    #[test]
    fn transcript_and_roster_defaults_are_not_the_same_number() {
        assert_eq!(AGENT_READ_TRANSCRIPT_DEFAULT_N, 1);
        assert_eq!(AGENT_READ_DEFAULT_N, 5);
    }

    /// A ledger figure is read for its magnitude. Full f64 precision and nine
    /// raw token digits are characters the caller pays for and decides nothing
    /// with (`context_pct` is what it steers on).
    #[test]
    fn a_ledger_figure_is_stated_at_reading_precision() {
        // Values measured off real sessions (excore s1480 / s1641 / s1617).
        assert_eq!(round_cost_usd(326.49616805000005), 326.5);
        assert_eq!(round_cost_usd(3.6579032499999995), 3.7);
        assert_eq!(round_cost_usd(0.0), 0.0);
        // What lands in the caller's context, not just what the f64 equals.
        assert_eq!(
            serde_json::to_string(&round_cost_usd(326.49616805000005)).unwrap(),
            "326.5"
        );

        assert_eq!(abbreviate_tokens(999), "999");
        assert_eq!(abbreviate_tokens(12_345), "12k");
        assert_eq!(abbreviate_tokens(174_558), "175k");
        assert_eq!(abbreviate_tokens(146_752_597), "147m");
        assert_eq!(abbreviate_tokens(723_973_606), "724m");
        assert_eq!(abbreviate_tokens(1_500_000_000), "1.5b");
    }

    // ========================================================================
    // v0.9.0 W2 (F2/F7) — dispatch-handler: idempotency, cycle, stop, wait.
    // The handlers are called directly with the `_caller_*` context that
    // `execute_session_tool` injects (so no secret dance).
    // ========================================================================

    /// Inject the server-resolved caller identity `execute_session_tool` sets.
    pub(super) fn ambient(
        caller_sid: &str,
        slug: &str,
        mut args: serde_json::Value,
    ) -> serde_json::Value {
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
        let (handle, principal, drx) =
            build_dispatch_gateway(answer, narrate, delay_ms, turn_failure, project_dir).await;
        tokio::spawn(Gateway::run_delegation_notifier(
            std::sync::Arc::clone(&handle),
            drx,
        ));
        (handle, principal)
    }

    /// The same wiring with the notifier NOT running, and the signal receiver
    /// handed back. What that buys a test: nothing but the code under test can
    /// spend a delegation watch, so "did the long poll disarm it?" is a fact
    /// rather than a race with the notifier's own boundary delivery.
    async fn build_dispatch_gateway(
        answer: bool,
        narrate: bool,
        delay_ms: u64,
        turn_failure: Option<ccteam_harness::ThreadErrorEvent>,
        project_dir: &std::path::Path,
    ) -> (
        GatewayHandle,
        String,
        tokio::sync::mpsc::UnboundedReceiver<crate::delegation::DelegationPulse>,
    ) {
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
        (handle, principal, drx)
    }

    /// A dispatch gateway whose child holds a REAL turn FIFO: one turn runs,
    /// the rest wait, and the test releases them one at a time. Every adapter
    /// shares the queue, the event log and the wakeup, so the returned handle
    /// drives whichever session the test names. The notifier runs, as it does
    /// in production. Returns `(gateway, parent sid, adapter)`.
    async fn queueing_dispatch_gateway(
        project_dir: &std::path::Path,
    ) -> (GatewayHandle, String, StubAdapter) {
        queueing_dispatch_gateway_with(project_dir, false).await
    }

    /// [`queueing_dispatch_gateway`], with the option of a stub that answers a
    /// started turn from INSIDE the submit call.
    async fn queueing_dispatch_gateway_with(
        project_dir: &std::path::Path,
        complete_inside_submit: bool,
    ) -> (GatewayHandle, String, StubAdapter) {
        queueing_dispatch_gateway_built(
            project_dir,
            StubAdapter {
                answer: true,
                queue: Some(std::sync::Arc::new(StubTurnQueue::default())),
                complete_inside_submit,
                ..Default::default()
            },
        )
        .await
    }

    /// The shared body: one stub, one gateway, one principal session.
    async fn queueing_dispatch_gateway_built(
        project_dir: &std::path::Path,
        shared: StubAdapter,
    ) -> (GatewayHandle, String, StubAdapter) {
        let handed = shared.clone();
        let factory: std::sync::Arc<
            dyn Fn(
                    ccteam_harness::AgentVendor,
                    ccteam_harness::SessionProtocol,
                )
                    -> std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
                + Send
                + Sync,
        > = std::sync::Arc::new(move |_, _| {
            std::sync::Arc::new(handed.clone())
                as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
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
        (handle, principal, shared)
    }

    /// [`queueing_dispatch_gateway`] whose stub has no injection channel.
    async fn queueing_dispatch_gateway_without_inject(
        project_dir: &std::path::Path,
    ) -> (GatewayHandle, String, StubAdapter) {
        queueing_dispatch_gateway_built(
            project_dir,
            StubAdapter {
                answer: true,
                queue: Some(std::sync::Arc::new(StubTurnQueue::default())),
                degrade_inject: true,
                ..Default::default()
            },
        )
        .await
    }

    /// The thread identity a `StubAdapter` gave `sid` (its FIFO key).
    fn stub_identity(sid: &str) -> String {
        format!("alpha--{sid}")
    }

    /// Poll a session's mirrored USER rows — the notification turns a parent
    /// received — until `want` of them land, or give up.
    async fn await_notifications(
        project_dir: &std::path::Path,
        parent_sid: &str,
        want: usize,
    ) -> Vec<String> {
        for _ in 0..400 {
            let rows: Vec<String> =
                ccteam_harness::execution::turns_mirror::read_all_turns(project_dir, parent_sid)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|turn| turn.user.contains(" done · ") || turn.user.contains(" FAILED "))
                    .map(|turn| turn.user)
                    .collect();
            if rows.len() >= want {
                return rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        ccteam_harness::execution::turns_mirror::read_all_turns(project_dir, parent_sid)
            .unwrap_or_default()
            .into_iter()
            .filter(|turn| turn.user.contains(" done · ") || turn.user.contains(" FAILED "))
            .map(|turn| turn.user)
            .collect()
    }

    pub(super) fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap()
    }

    /// The recorded `(sid, secret)` pairs a [`StubAdapter`] saw, so a test can
    /// authenticate as a session the gateway actually spawned.
    pub(super) type SpawnSecrets = std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>;

    pub(super) async fn secret_for(secrets: &SpawnSecrets, sid: &str) -> String {
        secrets
            .lock()
            .await
            .iter()
            .find(|(recorded, _)| recorded == sid)
            .map(|(_, secret)| secret.clone())
            .unwrap_or_else(|| panic!("no minted secret recorded for {sid}"))
    }

    /// A gateway whose adapters share ONE spawn ledger, so every session's
    /// minted principal is recoverable — what the tool-face tests need to call
    /// `initialize` as a specific child. Returns `(gateway, root sid, secrets)`.
    pub(super) async fn face_gateway(
        project_dir: &std::path::Path,
    ) -> (GatewayHandle, String, SpawnSecrets) {
        let secrets: SpawnSecrets = Default::default();
        let shared = std::sync::Arc::clone(&secrets);
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
                spawns: std::sync::Arc::clone(&shared),
                ..Default::default()
            }) as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let mut gw = Gateway::new_with_factory(factory, "alpha", project_dir);
        mark_stub_vendors_installed(&mut gw);
        let (etx, mut erx) = tokio::sync::mpsc::unbounded_channel::<GatewayEvent>();
        gw.set_event_sink(etx);
        tokio::spawn(async move { while erx.recv().await.is_some() {} });
        let root = gw
            .create_session_api(
                "alpha".into(),
                String::new(),
                ccteam_harness::AgentVendor::Claude,
                ccteam_harness::PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        (
            std::sync::Arc::new(tokio::sync::Mutex::new(gw)),
            root,
            secrets,
        )
    }

    // ====================================================================
    // GitHub #197 (B/C/F) — a dispatch is a REQUEST, and its answer knows
    // whose it is. The three failures these cover were measured on
    // s932→s933/s936 (a parent that could not see its own queue re-sent one
    // instruction three times and then stopped a 400k-context child) and
    // s1688→s1689 (a 15-minute decision made off the wrong answer).
    // ====================================================================

    /// A running, B and C queued: each dispatch is told its own request id and
    /// where in the queue it actually sits. Before this, all three answered a
    /// flat `pending` with no way to tell "running" from "third in line".
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn queued_dispatches_report_their_own_identity_and_position() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _stub) = queueing_dispatch_gateway(tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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

        let dispatch = |task: &str, title: &str| {
            let args = json!({ "sid": &child, "task": task, "title": title, "routing": "queue" });
            let gw = &gw;
            let principal = principal.clone();
            async move {
                parse(
                    &run_agent_dispatch(
                        &ambient(&principal, "alpha", args),
                        gw,
                        McpCaller::Ambient,
                    )
                    .await
                    .unwrap(),
                )
            }
        };
        let a = dispatch("verdict please", "verdict").await;
        let b = dispatch("then the cleanup", "cleanup").await;
        let c = dispatch("and the release note", "release-note").await;

        assert_eq!(a["status"], json!("started"), "{a}");
        assert!(a.get("queue_position").is_none(), "nothing is queued: {a}");
        assert_eq!(b["status"], json!("queued"), "{b}");
        assert_eq!(b["queue_position"], json!(1), "1-based, oldest first: {b}");
        assert_eq!(c["status"], json!("queued"), "{c}");
        assert_eq!(c["queue_position"], json!(2), "{c}");

        // Three distinct identities, and three distinct execution turns.
        let ids: Vec<&str> = [&a, &b, &c]
            .iter()
            .map(|r| {
                r["request_id"]
                    .as_str()
                    .expect("every dispatch is a request")
            })
            .collect();
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "{a} {b} {c}"
        );
        let turns: Vec<&str> = [&a, &b, &c]
            .iter()
            .map(|r| r["turn_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            turns.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "a queued task names the turn it WILL run in: {a} {b} {c}"
        );

        // The four delivery facts, kept apart. A queued task has not reached
        // the harness; a started one has, but that is not proof it was read.
        assert_eq!(a["delivery"]["written"], json!(true), "{a}");
        assert_eq!(a["delivery"]["executing"], json!("unknown"), "{a}");
        assert_eq!(b["delivery"]["queued"], json!(true), "{b}");
        assert_eq!(b["delivery"]["written"], json!(false), "{b}");
    }

    /// GitHub #197 (E) — an explicit stop ends a PROCESS, and says what that
    /// cost: the turn it cut (recorded in the transcript), the tasks ccteam is
    /// still holding and where, and the policy that decides their fate.
    ///
    /// It used to answer `{stopped:true}` and DROP the child's requests, so a
    /// parked instruction — measured on s932→s936, a "do not open a public
    /// port" constraint that sat in the queue and was never delivered — left
    /// no trace anywhere and nobody could learn it had not arrived.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_stop_records_the_turn_it_cut_and_names_what_is_still_held() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, _stub) = queueing_dispatch_gateway(&project_dir).await;
        let child = parse(
            &run_agent_spawn(
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
        let dispatch = |args: serde_json::Value| {
            let gw = &gw;
            let principal = principal.clone();
            async move {
                parse(
                    &run_agent_dispatch(
                        &ambient(&principal, "alpha", args),
                        gw,
                        McpCaller::Ambient,
                    )
                    .await
                    .unwrap(),
                )
            }
        };
        let running = dispatch(json!({ "sid": &child, "task": "the migration" })).await;
        let queued = dispatch(
            json!({ "sid": &child, "task": "do not open a public port", "routing": "queue" }),
        )
        .await;
        assert_eq!(queued["status"], json!("queued"), "{queued}");
        // The adapter's own mirror is what makes a queued line RETAINED. The
        // stub has no such file, so the test writes what the stream-json
        // adapter writes: the parked line under the turn it will open.
        let chat_dir = project_dir.join(".ccteam").join("chat").join(&child);
        std::fs::create_dir_all(&chat_dir).unwrap();
        std::fs::write(
            chat_dir.join("deferred-input.json"),
            serde_json::to_vec(&json!({
                "schema": 2,
                "parked": [{"turn_id": queued["turn_id"], "text": "do not open a public port"}],
            }))
            .unwrap(),
        )
        .unwrap();

        let stopped = parse(
            &run_agent_stop(
                &ambient(&principal, "alpha", json!({ "sid": &child })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(stopped["stopped"], json!(true), "{stopped}");

        // What it cut. The stub cannot report an in-flight turn's narration, so
        // the honest word is `unknown` — never an empty string that reads as
        // "it said nothing".
        let cut = &stopped["interrupted"];
        assert_eq!(cut["exec_turn"], running["turn_id"], "{stopped}");
        assert_eq!(cut["narration"], json!("unknown"), "{stopped}");
        assert_eq!(
            cut["requests"],
            json!([running["request_id"].as_str().unwrap()]),
            "the request bound to the cut turn, and only it: {stopped}"
        );

        // What is still held, and under which policy.
        let held = stopped["undelivered"].as_array().expect("{stopped}");
        assert_eq!(held.len(), 1, "{stopped}");
        assert_eq!(held[0]["request_id"], queued["request_id"], "{stopped}");
        assert_eq!(held[0]["delivery"], json!("undelivered"), "{stopped}");
        assert_eq!(held[0]["retained_in"], json!("deferred-input.json"));
        assert_eq!(
            held[0]["state"],
            json!("queued"),
            "still outstanding: it replays: {stopped}"
        );
        assert_eq!(
            stopped["resume_policy"],
            json!("replay_after_first_result"),
            "{stopped}"
        );

        // The transcript keeps the record — this is what `agent_read` shows
        // instead of the `turns:[]` a stopped child used to read back as.
        let rows = ccteam_harness::execution::turns_mirror::read_all_turns(&project_dir, &child)
            .unwrap_or_default();
        let record = rows
            .iter()
            .find(|row| row.outcome.as_deref() == Some("interrupted"))
            .expect("the cut turn leaves a record");
        assert_eq!(
            record.exec_turn_id.as_deref(),
            running["turn_id"].as_str(),
            "{record:?}"
        );
        assert!(record.error.as_deref().unwrap_or_default().contains(&child));

        // …and the durable store agrees with the response.
        let store = ccteam_harness::read_delegation_requests(&project_dir, &child)
            .expect("the stop keeps the child's requests");
        let by_id = |id: &str| {
            store
                .get(id)
                .unwrap_or_else(|| panic!("request {id} survives the stop"))
                .state
        };
        assert_eq!(
            by_id(running["request_id"].as_str().unwrap()),
            ccteam_harness::RequestState::Interrupted
        );
        assert_eq!(
            by_id(queued["request_id"].as_str().unwrap()),
            ccteam_harness::RequestState::Queued
        );
    }

    /// A stop with nothing running and nothing owed says exactly that: no
    /// `interrupted`, no `undelivered`, no policy nobody is subject to.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stop_of_an_idle_child_reports_nothing_cut_short() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _stub) = queueing_dispatch_gateway(tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
        let stopped = parse(
            &run_agent_stop(
                &ambient(&principal, "alpha", json!({ "sid": &child })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(stopped["stopped"], json!(true), "{stopped}");
        assert!(stopped.get("interrupted").is_none(), "{stopped}");
        assert!(stopped.get("undelivered").is_none(), "{stopped}");
        assert!(stopped.get("resume_policy").is_none(), "{stopped}");
    }

    /// GitHub #197 (D) — a parent tasking a BUSY child steers the turn it is
    /// already running, exactly as a human's IM message does; a caller that
    /// wants its own turn boundary asks for one.
    ///
    /// Every `agent{sid,task}` used to be queued, because routing was derived
    /// from the turn's ORIGIN and an A2A submit is internal. So a correction
    /// sent to a working child sat behind it: measured on s932→s933, a ruling
    /// sent at 17:38 reached claude at 17:52, and the parent — told only
    /// `pending` — re-sent it twice more and then stopped the child.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_follow_up_steers_the_running_turn_unless_its_own_turn_is_asked_for() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _stub) = queueing_dispatch_gateway(tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
        let dispatch = |args: serde_json::Value| {
            let gw = &gw;
            let principal = principal.clone();
            async move {
                parse(
                    &run_agent_dispatch(
                        &ambient(&principal, "alpha", args),
                        gw,
                        McpCaller::Ambient,
                    )
                    .await
                    .unwrap(),
                )
            }
        };
        let running = dispatch(json!({ "sid": &child, "task": "the long job" })).await;
        assert_eq!(running["status"], json!("started"), "{running}");

        // No `routing` named: the default is a steer.
        let steer = dispatch(json!({ "sid": &child, "task": "one correction" })).await;
        assert_eq!(
            steer["status"],
            json!("injected"),
            "a follow-up on a busy child steers by default: {steer}"
        );
        assert_eq!(
            steer["turn_id"], running["turn_id"],
            "an injected task runs in the turn it JOINED: {steer}"
        );
        assert!(
            steer.get("queue_position").is_none(),
            "nothing is queued: {steer}"
        );
        assert_eq!(steer["delivery"]["queued"], json!(false), "{steer}");
        assert_eq!(steer["delivery"]["written"], json!(true), "{steer}");
        assert_ne!(
            steer["request_id"], running["request_id"],
            "sharing a turn is not sharing an identity: {steer}"
        );

        // …and the other channel is still one argument away.
        let queued =
            dispatch(json!({ "sid": &child, "task": "afterwards", "routing": "queue" })).await;
        assert_eq!(queued["status"], json!("queued"), "{queued}");
        assert_eq!(queued["queue_position"], json!(1), "{queued}");
        assert_ne!(
            queued["turn_id"], running["turn_id"],
            "a queued task names the turn it WILL open: {queued}"
        );
    }

    /// GitHub #197 (D) — `routing` says what the caller WANTS; `status` says
    /// what the vendor did. A harness with no injection channel degrades to a
    /// distinct turn and the response says so, so a parent never reads
    /// `injected` for a task the model has not been shown.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_vendor_that_cannot_inject_reports_queued_rather_than_claiming_injected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _stub) = queueing_dispatch_gateway_without_inject(tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
        for task in ["the long job", "one correction"] {
            let response = parse(
                &run_agent_dispatch(
                    &ambient(
                        &principal,
                        "alpha",
                        json!({ "sid": &child, "task": task, "routing": "inject" }),
                    ),
                    &gw,
                    McpCaller::Ambient,
                )
                .await
                .unwrap(),
            );
            let want = if task == "the long job" {
                "started"
            } else {
                "queued"
            };
            assert_eq!(response["status"], json!(want), "{task}: {response}");
        }
    }

    /// GitHub #197 (D) — an unknown routing word is a refusal, not a silently
    /// ignored argument: the two channels behave differently enough that
    /// guessing one for the caller is worse than saying no.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unknown_routing_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _stub) = queueing_dispatch_gateway(tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
        let error = run_agent_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": &child, "task": "x", "routing": "urgent" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("invalid routing `urgent`") && error.contains("`inject` | `queue`"),
            "{error}"
        );
    }

    /// A finishes: only A's parent is woken, the header names A's request and
    /// title, and it says how much of that child's work is still owed. B and C
    /// stay outstanding — the boundary that ended A is not their answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_boundary_notifies_only_the_request_it_answered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, stub) = queueing_dispatch_gateway(&project_dir).await;
        let child = parse(
            &run_agent_spawn(
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
        let mut sent = Vec::new();
        for (task, title) in [
            ("verdict please", "verdict"),
            ("then the cleanup", "cleanup"),
            ("and the release note", "release-note"),
        ] {
            sent.push(parse(
                &run_agent_dispatch(
                    &ambient(
                        &principal,
                        "alpha",
                        json!({ "sid": &child, "task": task, "title": title, "routing": "queue" }),
                    ),
                    &gw,
                    McpCaller::Ambient,
                )
                .await
                .unwrap(),
            ));
        }

        stub.run_next_turn(&stub_identity(&child)).await;
        let notes = await_notifications(&project_dir, &principal, 1).await;
        assert_eq!(notes.len(), 1, "one boundary wakes one request: {notes:?}");
        let header = notes[0].lines().next().unwrap_or_default().to_string();
        assert!(
            header.contains(sent[0]["request_id"].as_str().unwrap()),
            "the header names the request that was answered: {header}"
        );
        assert!(
            header.contains("«verdict»"),
            "…and its OWN title, not a later dispatch's: {header}"
        );
        assert!(
            header.contains("2 still queued"),
            "…and what the child still owes: {header}"
        );
        assert!(
            !header.contains("cleanup") && !header.contains("release-note"),
            "{header}"
        );

        // B and C are untouched: their turns have not run.
        let read = parse(
            &run_agent_read(
                &ambient(&principal, "alpha", json!({ "sid": &child, "n": 0 })),
                &gw,
                McpCaller::Ambient,
                &CcteamPaths {
                    root: tmp.path().join("home"),
                    projects_root: tmp.path().join("projects"),
                },
            )
            .await
            .unwrap(),
        );
        let outstanding: Vec<&str> = read["requests"]
            .as_array()
            .expect("a read carries the child's request rows")
            .iter()
            .filter(|row| row["state"] != json!("answered"))
            .map(|row| row["title"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(outstanding, ["cleanup", "release-note"], "{read}");

        // B's turn runs next: ITS request is the one that gets reported.
        stub.run_next_turn(&stub_identity(&child)).await;
        let notes = await_notifications(&project_dir, &principal, 2).await;
        assert_eq!(notes.len(), 2, "{notes:?}");
        let second = notes[1].lines().next().unwrap_or_default().to_string();
        assert!(
            second.contains(sent[1]["request_id"].as_str().unwrap()),
            "the queued task's flush bound it to its own turn: {second}"
        );
        assert!(second.contains("«cleanup»"), "{second}");
        assert!(second.contains("1 still queued"), "{second}");
    }

    /// issue #201 R6 — the ordinal on a completion header counts turns that
    /// FINISHED, not messages that were accepted. Three tasks are handed to a
    /// child that has completed one, and its first answer used to arrive
    /// labelled `turn 3`: the parent read the number as "you are on your third
    /// reply" and trusted it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn the_turn_ordinal_counts_completed_turns_not_accepted_messages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, stub) = queueing_dispatch_gateway(&project_dir).await;
        let child = parse(
            &run_agent_spawn(
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
        for task in ["first", "second", "third"] {
            run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": &child, "task": task, "title": task, "routing": "queue" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        }
        stub.run_next_turn(&stub_identity(&child)).await;
        let notes = await_notifications(&project_dir, &principal, 1).await;
        let header = notes[0].lines().next().unwrap_or_default().to_string();
        assert!(
            header.contains(" · turn 1 ·") || header.ends_with(" · turn 1"),
            "three messages were accepted, ONE turn finished: {header}"
        );

        stub.run_next_turn(&stub_identity(&child)).await;
        let notes = await_notifications(&project_dir, &principal, 2).await;
        let header = notes[1].lines().next().unwrap_or_default().to_string();
        assert!(
            header.contains(" · turn 2 ·") || header.ends_with(" · turn 2"),
            "{header}"
        );
    }

    /// `agent{wait}` waits for ITS request. A sibling finishing first is not
    /// the answer, and the wait does not return holding it (issue #201: the
    /// first boundary of the child used to end every wait on that child).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wait_never_returns_a_sibling_tasks_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, stub) = queueing_dispatch_gateway(&project_dir).await;
        let child = parse(
            &run_agent_spawn(
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
        // A runs; B waits behind it.
        run_agent_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": &child, "task": "task A", "title": "A", "routing": "queue" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();

        let waiter = {
            let gw = std::sync::Arc::clone(&gw);
            let principal = principal.clone();
            let child = child.clone();
            tokio::spawn(async move {
                parse(
                    &run_agent_dispatch(
                        &ambient(
                            &principal,
                            "alpha",
                            json!({ "sid": &child, "task": "task B", "title": "B", "wait": 20, "routing": "queue" }),
                        ),
                        &gw,
                        McpCaller::Ambient,
                    )
                    .await
                    .unwrap(),
                )
            })
        };
        // Let B be accepted and queued before A's boundary lands.
        for _ in 0..200 {
            let queued = gw
                .lock()
                .await
                .outstanding_request_ids(&child, &principal)
                .len();
            if queued == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        stub.run_next_turn(&stub_identity(&child)).await;
        // A's answer belongs to A's (async) dispatch, so the parent is woken
        // for it — and the waiter is still waiting.
        let notes = await_notifications(&project_dir, &principal, 1).await;
        assert!(notes[0].contains("«A»"), "{notes:?}");
        assert!(!waiter.is_finished(), "B's wait must not take A's answer");

        stub.run_next_turn(&stub_identity(&child)).await;
        let answer = waiter.await.expect("the waiter finishes");
        assert_eq!(answer["status"], json!("completed"), "{answer}");
        assert!(
            answer["result_text"]
                .as_str()
                .unwrap_or_default()
                .contains("task B"),
            "the wait returns ITS OWN task's answer: {answer}"
        );
    }

    /// A follow-up that names no `notify` keeps the mode this parent chose for
    /// its outstanding work on this child; an explicit one overrides. Reverting
    /// to the default mid-conversation is how a deliberate `final` became a
    /// 443-character `brief` and a parent decided off the excerpt.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn an_omitted_notify_inherits_this_parents_own_precedent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal, _stub) = queueing_dispatch_gateway(tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
        let dispatch = |args: serde_json::Value| {
            let gw = &gw;
            let principal = principal.clone();
            async move {
                run_agent_dispatch(&ambient(&principal, "alpha", args), gw, McpCaller::Ambient)
                    .await
                    .unwrap()
            }
        };
        dispatch(json!({ "sid": &child, "task": "…", "title": "the verdict", "notify": "final" }))
            .await;
        dispatch(json!({ "sid": &child, "task": "…", "title": "a follow-up" })).await;
        dispatch(json!({ "sid": &child, "task": "…", "title": "and one more", "notify": "brief" }))
            .await;

        let store = ccteam_harness::read_delegation_requests(tmp.path(), &child)
            .expect("the child holds its requests");
        let modes: Vec<&str> = store
            .requests
            .iter()
            .map(|request| request.notify.as_str())
            .collect();
        assert_eq!(
            modes,
            ["final", "final", "brief"],
            "an omitted notify inherits, an explicit one overrides: {store:?}"
        );
        // Titles are per request and never rewritten by a later dispatch.
        let titles: Vec<&str> = store
            .requests
            .iter()
            .map(|request| request.title.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(titles, ["the verdict", "a follow-up", "and one more"]);
    }

    /// The full-read recipe on a truncated excerpt still reads THAT answer
    /// after the child has finished a later turn. `n:1` was only correct at
    /// the instant of delivery (issue #201).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn an_old_excerpts_recipe_still_reads_its_own_answer() {
        use ccteam_harness::execution::turns_mirror::{append_turn, TurnRecord};
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
        let verdict = format!("VERDICT{}END", "v".repeat(4_000));
        for (id, text) in [
            (format!("{child}-1"), verdict.clone()),
            (format!("{child}-2"), "confirmed, ready to ship".to_string()),
        ] {
            append_turn(
                tmp.path(),
                &child,
                &TurnRecord {
                    exec_turn_id: None,
                    turn_id: id,
                    ts: chrono::Utc::now(),
                    vendor: "claude".into(),
                    role: String::new(),
                    user: String::new(),
                    assistant: text,
                    usage: serde_json::Value::Null,
                    status: None,
                    tool_calls: vec![],
                    attachments: vec![],
                    outcome: None,
                    error_kind: None,
                    error: None,
                    conclusion: None,
                },
            )
            .unwrap();
        }
        // The excerpt a truncated read of the verdict hands back.
        let cut = parse(
            &run_agent_read(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": &child, "turn": format!("{child}-1"), "max_chars": 300 }),
                ),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        let content = cut["turns"][0]["content"].as_str().unwrap().to_string();
        let recipe = content
            .split("agent_read{")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("a truncated row carries the exact read")
            .to_string();
        assert!(
            recipe.contains(&format!("turn:{child}-1")),
            "the recipe names the turn, not a position: {recipe}"
        );
        assert!(!recipe.contains("n:1"), "{recipe}");

        // Follow it AFTER a newer turn exists: it still reads the verdict.
        let whole = parse(
            &run_agent_read(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": &child, "turn": format!("{child}-1"), "max_chars": 50_000 }),
                ),
                &gw,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        assert_eq!(whole["turns"].as_array().map(Vec::len), Some(1), "{whole}");
        assert_eq!(whole["turns"][0]["content"], json!(verdict), "{whole}");
        // …and a turn the transcript does not hold is an error, never a page
        // of something else.
        let missing = run_agent_read(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": &child, "turn": "s0-ghost" }),
            ),
            &gw,
            McpCaller::Ambient,
            &paths,
        )
        .await
        .unwrap_err();
        assert!(missing.contains("no turn s0-ghost"), "{missing}");
    }

    fn assert_exact_keys(value: &serde_json::Value, expected: &[&str]) {
        let mut actual = value
            .as_object()
            .expect("response must be an object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        actual.sort_unstable();
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        assert_eq!(actual, expected, "unexpected response shape: {value}");
    }

    #[tokio::test]
    async fn agent_rejects_removed_host_parameter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let args = ambient(&principal, "alpha", json!({"task": "x", "host": "sat-a"}));
        let err = run_agent(&args, &gw, McpCaller::Ambient, &paths)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            format!("agent: {}", crate::remote_host::HOST_SPAWN_PARAM_REMOVED)
        );
        assert_eq!(gw.lock().await.session_views().len(), 1);
    }

    #[tokio::test]
    async fn agent_reports_vendor_not_installed_from_empty_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gateway, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        gateway
            .lock()
            .await
            .set_local_vendor_availability_for_tests(stub_vendor_availability(false));

        let error = run_agent_spawn(
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
        let r1 = parse(&run_agent_spawn(&a, &gw, McpCaller::Ambient).await.unwrap());
        let r2 = parse(&run_agent_spawn(&a, &gw, McpCaller::Ambient).await.unwrap());
        assert_exact_keys(&r1, &["sid"]);
        assert_exact_keys(&r2, &["idempotent_replay", "sid"]);
        assert_eq!(r1["sid"], r2["sid"], "replay returns the original sid");
        assert_eq!(r2["idempotent_replay"], true, "a replay says so");
        // Exactly ONE child was created (principal + 1 child = 2 sessions).
        let list = parse(
            &run_agent_read_roster(&serde_json::json!({}), &gw)
                .await
                .unwrap(),
        );
        let children = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["parent_sid"] == json!(principal))
            .count();
        assert_eq!(children, 1, "no double-spawn: {list}");
    }

    /// Two independent MCP agent calls must reach vendor startup at
    /// the same time; only the post-spawn admission seam is serialized.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn agent_fanout_reaches_two_vendor_spawns_concurrently() {
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
                run_agent_spawn(
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

    /// v0.9.5 feedback fix — `agent_read` accepts `project`/`activity`/
    /// `limit` filters, caps rows (flagging `truncated` + `total`), slims
    /// null/empty fields out of each row, and rejects a bogus activity value.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_read_filters_limit_and_slim_rows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        for _ in 0..3 {
            run_agent_spawn(
                &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        }
        // Unfiltered: principal + 3 children.
        let all = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
        assert_exact_keys(&all, &["sessions"]);
        assert!(all.get("total").is_none());
        assert_eq!(all["sessions"].as_array().unwrap().len(), 4);
        assert!(all.get("truncated").is_none(), "under the cap: {all}");
        // Slim rows: empty role / absent title / false current are omitted.
        let row = &all["sessions"].as_array().unwrap()[0];
        assert!(row.get("role").is_none(), "empty role omitted: {row}");
        assert!(row.get("status").is_none(), "static status omitted: {row}");

        // limit=2 → truncated + hint, total still 4.
        let capped = parse(&run_agent_read_roster(&json!({"n": 2}), &gw).await.unwrap());
        assert_eq!(capped["sessions"].as_array().unwrap().len(), 2);
        assert_eq!(capped["total"], json!(4));
        assert_eq!(capped["truncated"], json!(true));
        // `truncated` + `total` say everything the caller needs; a prose hint
        // repeating the schema is bytes it already paid for.
        assert!(capped.get("hint").is_none(), "{capped}");

        // project filter: a non-existent slug matches nothing.
        let none = parse(
            &run_agent_read_roster(&json!({"project": "nope"}), &gw)
                .await
                .unwrap(),
        );
        assert_eq!(none["sessions"].as_array().unwrap().len(), 0);

        // bogus activity value → readable error.
        let err = run_agent_read_roster(&json!({"activity": "busy"}), &gw)
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
    async fn agent_read_marks_the_calling_session_not_the_current_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let mut children = Vec::new();
        for _ in 0..2 {
            let spawned = parse(
                &run_agent_spawn(
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
            &run_agent_read_roster(&ambient(&principal, "alpha", json!({})), &gw)
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
        assert!(as_principal["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.get("current").is_none()));

        // The marker follows the CALLER, not the fleet: same gateway, same
        // rows, a child asking sees the mark move onto its own row.
        let as_child = parse(
            &run_agent_read_roster(&ambient(&children[0], "alpha", json!({})), &gw)
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
    async fn agent_read_marks_nothing_when_the_caller_has_no_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        run_agent_spawn(
            &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();

        for args in [json!({}), json!({ "_caller_sid": "" })] {
            let list = parse(&run_agent_read_roster(&args, &gw).await.unwrap());
            assert_exact_keys(&list, &["sessions"]);
            let rows = list["sessions"].as_array().unwrap();
            assert_eq!(rows.len(), 2, "both sessions listed: {list}");
            for row in rows {
                if row.get("parent_sid").is_some() {
                    assert_exact_keys(row, &["activity", "parent_sid", "sid", "vendor"]);
                } else {
                    assert_exact_keys(row, &["activity", "sid", "vendor"]);
                }
                assert!(row.get("activity").is_some());
            }
            assert!(
                rows.iter().all(|row| row.get("is_self").is_none()),
                "a sid-less caller owns no row: {list}"
            );
        }

        let with_tree = parse(
            &run_agent_read_roster(&json!({"tree": true}), &gw)
                .await
                .unwrap(),
        );
        assert_exact_keys(&with_tree, &["sessions", "tree"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_read_surfaces_requested_model_and_omits_vendor_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let spawned = parse(
            &run_agent_spawn(
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
                exec_turn_id: None,
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
                conclusion: None,
            },
        )
        .unwrap();

        let list = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
        let child = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sid"] == child_sid)
            .unwrap();
        assert_eq!(child["context_pct"], json!(19));
        let parent = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["sid"] == principal)
            .unwrap();
        assert!(parent.get("model").is_none());
    }

    /// A session ccteam is not holding a process for is still REAL: it must be
    /// listed (so a caller reuses it instead of spawning a duplicate) and
    /// marked `residency:"released"`, while `activity` keeps saying only what
    /// the session is DOING. A resident row carries no `residency` at all —
    /// the agent's context is not spent on a field that says nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn agent_read_shows_released_sessions_and_marks_their_residency() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let child = run_agent_spawn(
            &ambient(&principal, "alpha", json!({ "vendor": "claude" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let child = parse(&child)["sid"].as_str().unwrap().to_string();

        // Resident rows say nothing about residency.
        let list = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
        let row = |list: &serde_json::Value, sid: &str| -> serde_json::Value {
            list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["sid"] == sid)
                .cloned()
                .unwrap_or_else(|| panic!("{sid} must be listed: {list}"))
        };
        assert!(row(&list, &child).get("residency").is_none());

        // Release the child's process; the row survives, now marked.
        {
            let mut guard = gw.lock().await;
            guard.set_sessions_config(ccteam_core::SessionsConfig {
                idle_release_secs: 1,
                ..Default::default()
            });
            guard.settle_turn_for_tests(&child);
            guard.backdate_residency_for_tests(&child, std::time::Duration::from_secs(600));
        }
        assert_eq!(Gateway::idle_release_tick(&gw).await, vec![child.clone()]);

        let list = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
        assert_eq!(row(&list, &child)["residency"], "released");
        assert!(
            row(&list, &child).get("activity").is_some(),
            "activity is still answered from the file verdict"
        );
        // The caller's own (still resident) row is unchanged.
        assert!(row(&list, &principal).get("residency").is_none());

        // `agent_read` says the same thing, in place of the old
        // `status:"stopped"` (which could not tell "asleep" from "over").
        let collected = parse(
            &run_agent_read_transcript(
                &ambient(&principal, "alpha", json!({ "sid": child.clone() })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(collected["residency"], "released");
        assert!(collected.get("status").is_none());

        // An explicit stop flips the word — the difference the caller acts on.
        gw.lock().await.stop_session(&child).await.unwrap();
        let collected = parse(
            &run_agent_read_transcript(
                &ambient(&principal, "alpha", json!({ "sid": child.clone() })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(collected["residency"], "stopped");
        let list = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
        assert!(
            !list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["sid"] == child.as_str()),
            "a stopped session leaves the listing: {list}"
        );
    }

    /// v0.9.5 feedback fix — a title-less `agent{task}` derives a
    /// short display label from the task's first line (ledger only), and the
    /// `notify` arg accepts the mode strings while rejecting garbage.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_derives_title_from_task_and_notify_modes_parse() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let long_line = format!("Refactor the harness layer {}", "x".repeat(80));
        run_agent_spawn(
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
        let list = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
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
        run_agent_spawn(
            &ambient(
                &principal,
                "alpha",
                json!({ "vendor": "claude", "task": "some task", "title": "my label", "notify": "brief" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let list = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
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
        let err = run_agent_spawn(
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

        // …and the retired `all` says so by name rather than reading as a typo.
        let removed = run_agent_spawn(
            &ambient(
                &principal,
                "alpha",
                json!({ "vendor": "claude", "task": "t", "notify": "all" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert_eq!(removed, "agent: notify `all` was removed; use final");
        assert_eq!(gw.lock().await.session_views().len(), before);
    }

    /// v0.9.1 — `agent{task}`: one call spawns AND dispatches (the
    /// dominant flow). The response merges the dispatch outcome (`turn_id` +
    /// `status:pending`) into the spawn body, and the delegation lineage
    /// is intact (`parent_sid` = the caller).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_with_task_dispatches_in_one_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let r = parse(
            &run_agent_spawn(
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
        assert_exact_keys(&r, &["delivery", "request_id", "sid", "status", "turn_id"]);
        // The disposition the adapter reported, not a flat `pending` (#201).
        assert_eq!(r["status"], json!("started"), "dispatch merged: {r}");
        assert_eq!(r["delivery"]["accepted"], json!(true), "{r}");
        assert_eq!(r["delivery"]["executing"], json!("unknown"), "{r}");
        assert!(
            r["turn_id"].as_str().is_some_and(|t| !t.is_empty()),
            "turn_id present: {r}"
        );
        assert!(r.get("notify_deliverable").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_fallback_async_calls_report_notification_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, _principal) = dispatch_gateway(false, 0, tmp.path()).await;

        let spawned = parse(
            &run_agent_spawn(
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
        // No managed parent to wake: say so on the HIRE too, not only on a
        // follow-up. The caller's next move (poll) is the same either way.
        assert_eq!(spawned["notify_deliverable"], false, "{spawned}");
        assert!(
            ccteam_harness::read_delegation_requests(tmp.path(), spawned_sid).is_none(),
            "an admin fallback caller has no parent watch"
        );

        let child = parse(
            &run_agent_spawn(
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
            &run_agent_dispatch(
                &json!({ "sid": child, "task": "follow-up" }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert_eq!(dispatched["notify_deliverable"], false);
        assert!(ccteam_harness::read_delegation_requests(tmp.path(), &child).is_none());

        let off_child = parse(
            &run_agent_spawn(
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
            &run_agent_dispatch(
                &json!({ "sid": off_child, "task": "ledger only", "notify": "off" }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert_eq!(off["notify_deliverable"], false);
    }

    /// `agent{task, wait}` with an answering child returns the answer inline
    /// (`status:completed`, `result_text`), exactly like the follow-up path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn spawn_with_task_and_wait_returns_inline_result() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let task = format!("HEAD{}TAIL", "🦀".repeat(12_000));
        let r = parse(
            &run_agent_spawn(
                &ambient(&principal, "alpha", json!({ "task": task, "wait": 6 })),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_exact_keys(
            &r,
            &[
                "context_pct",
                "request_id",
                "result_text",
                "sid",
                "status",
                "turn",
                "turn_id",
            ],
        );
        assert_eq!(r["status"], json!("completed"), "inline: {r}");
        let result = r["result_text"].as_str().unwrap();
        assert_eq!(result.chars().count(), INLINE_RESULT_MAX_CHARS);
        assert!(result.starts_with("echo: HEAD"));
        assert!(result.ends_with("TAIL"));
        assert!(result.contains("…[+"));
        assert!(result.contains("agent_read{sid:"));
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
            &run_agent_spawn(
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
            &run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({
                        "sid": child,
                        "task": "run the task",
                        "wait": 6,
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
            &run_agent_read_transcript(
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
        assert!(collected.get("model").is_none(), "{collected}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_response_reports_total_chars_and_honest_truncation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
                exec_turn_id: None,
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
                conclusion: None,
            },
        )
        .unwrap();

        let response = parse(
            &run_agent_read_transcript(
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
        assert_exact_keys(&response, &["activity", "cursor", "truncated", "turns"]);
        assert!(response.get("total_chars").is_none());
        assert_eq!(response["truncated"], true);
        assert!(response.get("model").is_none());
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
            &run_agent_spawn(
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
            &run_agent_dispatch(&d, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        let t2 = parse(
            &run_agent_dispatch(&d, &gw, McpCaller::Ambient)
                .await
                .unwrap(),
        );
        assert_exact_keys(
            &t2,
            &[
                "delivery",
                "idempotent_replay",
                "request_id",
                "sid",
                "status",
                "turn_id",
            ],
        );
        assert_eq!(t1["turn_id"], t2["turn_id"], "replay returns the same turn");
        assert_eq!(t2["idempotent_replay"], true, "a replay says so");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_cycle_self_and_ancestor_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        // self-dispatch.
        let e = run_agent_dispatch(
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
            &run_agent_spawn(
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
        let e2 = run_agent_dispatch(
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
            &run_agent_spawn(
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
        let e = run_agent_stop(
            &ambient(&principal, "alpha", json!({ "sid": sibling })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(e.contains("not a descendant"), "non-descendant stop: {e}");
        // The real descendant stops fine.
        let ok = run_agent_stop(
            &ambient(&principal, "alpha", json!({ "sid": child })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let stopped = parse(&ok);
        assert_exact_keys(&stopped, &["sid", "stopped"]);
        assert_eq!(stopped["stopped"], json!(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_inline_completed_and_timeout_pending() {
        // inline completed (child answers immediately).
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 0, tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
            &run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": child, "task": task, "wait": 6 }),
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
            &run_agent_spawn(
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
            &run_agent_dispatch(
                &ambient(
                    &p2,
                    "alpha",
                    json!({ "sid": child2, "task": "go", "wait": 1 }),
                ),
                &gw2,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_eq!(r2["answered"], json!(false), "timeout: {r2}");
        assert_eq!(r2["status"], json!("started"), "timeout: {r2}");
        assert!(r2["request_id"].as_str().is_some(), "{r2}");
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
            &run_agent_spawn(
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
            &run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": child, "task": "do the wave", "wait": 6, "notify": "off" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert_exact_keys(
            &r,
            &[
                "context_pct",
                "request_id",
                "result_text",
                "sid",
                "status",
                "turn",
                "turn_id",
            ],
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
            &run_agent_spawn(
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
        run_agent_dispatch(
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
                    .filter(|t| t.user.contains(" done · "))
                    .collect();
            if !notes.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(notes.len(), 1, "one boundary notification, no flood");
        assert!(notes[0].user.contains(" done · "), "{}", notes[0].user);
        assert!(notes[0].user.contains("echo: second wave"));
    }

    /// LOCK DISCIPLINE: the gateway lock is acquirable while a dispatch `wait`
    /// is parked (the wait awaits OFF the lock).
    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn dispatch_wait_does_not_hold_gateway_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(true, 3_000, tmp.path()).await;
        let child = parse(
            &run_agent_spawn(
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
            json!({ "sid": child, "task": "go", "wait": 5 }),
        );
        let waiter =
            tokio::spawn(async move { run_agent_dispatch(&d, &gw_w, McpCaller::Ambient).await });
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
    /// `agent_read` — so it would read as a ccteam bug instead of an answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn driving_tools_refuse_an_external_node_by_saying_what_it_is() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
        let node = enroll_external_node(&gw, "alpha").await;

        // The premise of the whole requirement: the caller can see it.
        let listed = parse(&run_agent_read_roster(&json!({}), &gw).await.unwrap());
        assert!(
            listed["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["sid"] == json!(node)),
            "the node is visible in agent_read: {listed}"
        );

        let dispatch = run_agent_dispatch(
            &ambient(&principal, "alpha", json!({ "sid": node, "task": "do it" })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        let collect = run_agent_read_transcript(
            &ambient(&principal, "alpha", json!({ "sid": node })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        let stop = run_agent_stop(
            &ambient(&principal, "alpha", json!({ "sid": node })),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        for (tool, message) in [
            ("agent", &dispatch),
            ("agent_read", &collect),
            ("agent_stop", &stop),
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
        let admin = run_agent_dispatch(
            &json!({ "sid": node, "task": "do it" }),
            &gw,
            McpCaller::Admin,
        )
        .await
        .unwrap_err();
        assert_eq!(
            admin,
            crate::external_nodes::not_driveable_error("agent", &node)
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
            &run_agent_spawn(
                &json!({ "project": "alpha", "vendor": "claude", "parent_sid": node }),
                &gw,
                McpCaller::Admin,
            )
            .await
            .unwrap(),
        );
        assert!(child["sid"].is_string(), "{child}");

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
        let denied = run_agent_spawn(
            &json!({ "project": "alpha", "vendor": "claude", "parent_sid": node }),
            &gw,
            McpCaller::Admin,
        )
        .await
        .unwrap_err();
        assert!(denied.contains("fan-out limit reached"), "{denied}");
        assert!(denied.contains("already has 1 active children"), "{denied}");

        // Acceptance comes from the ledger, never from faith in the declaration.
        let unknown = run_agent_spawn(
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
            &run_agent_spawn(
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
        assert!(external["sid"].is_string(), "{external}");
        // The armed watch agrees with the answer we just gave: the edge stays
        // watched (so the child's completion keeps hitting the ledger) with no
        // impossible delivery armed on it.
        let watch =
            ccteam_harness::read_delegation_requests(tmp.path(), external["sid"].as_str().unwrap())
                .expect("the delegation edge is still watched");
        assert_eq!(watch.requests[0].parent_sid, node);
        assert_eq!(
            watch.requests[0].notify,
            ccteam_harness::NotifyMode::Off,
            "{watch:?}"
        );

        // A managed parent has a transport, and keeps it.
        let managed = parse(
            &run_agent_spawn(
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
        assert!(managed.get("notify_deliverable").is_none(), "{managed}");
        let watch =
            ccteam_harness::read_delegation_requests(tmp.path(), managed["sid"].as_str().unwrap())
                .unwrap();
        assert_eq!(watch.requests[0].parent_sid, principal);
        // issue #194 — frugal by default: nobody asked for the 2000-char tier.
        assert_eq!(
            watch.requests[0].notify,
            ccteam_harness::NotifyMode::Brief,
            "{watch:?}"
        );
    }

    /// GitHub #197 (B) — the child answers BEFORE the dispatcher has bound its
    /// request. The submit path used to release the child's store between the
    /// accept and the bind, so a boundary that fast found the request still
    /// `Accepted`, resolved nothing, and the completion was lost until a daemon
    /// restart. Accept, submit and bind now run under one per-child claim that
    /// the notifier also takes before it plans, so the boundary waits for the
    /// binding — and the answer is delivered exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_child_that_answers_inside_the_submit_is_still_answered_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, _stub) = queueing_dispatch_gateway_with(&project_dir, true).await;
        let child = parse(
            &run_agent_spawn(
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
        let dispatched = parse(
            &run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": &child, "task": "answer instantly", "title": "instant" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        let request_id = dispatched["request_id"].as_str().unwrap().to_string();
        let notes = await_notifications(&project_dir, &principal, 1).await;
        assert_eq!(notes.len(), 1, "delivered, and delivered once: {notes:?}");
        assert!(notes[0].contains("«instant»"), "{}", notes[0]);
        assert!(notes[0].contains(&request_id), "{}", notes[0]);
        // And the request is closed, not left outstanding for a restart to find.
        assert!(
            gw.lock()
                .await
                .outstanding_request_ids(&child, &principal)
                .is_empty(),
            "the request resolved on its own boundary"
        );
    }

    /// GitHub #197 (B) — a request that LEFT the store while its dispatcher was
    /// blocked on it is `unknown`, never `answered`. The wait used to treat a
    /// missing row as done and then read the transcript tail, so a caller
    /// waiting on B whose request was dropped (an `agent_stop`, an unreachable
    /// parent) was handed sibling A's answer as if it were B's.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wait_on_a_request_the_store_lost_reports_unknown_not_a_siblings_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, stub) = queueing_dispatch_gateway(&project_dir).await;
        let child = parse(
            &run_agent_spawn(
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
        run_agent_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": &child, "task": "task A", "title": "A", "routing": "queue" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let waiter = {
            let gw = std::sync::Arc::clone(&gw);
            let principal = principal.clone();
            let child = child.clone();
            tokio::spawn(async move {
                parse(
                    &run_agent_dispatch(
                        &ambient(
                            &principal,
                            "alpha",
                            json!({ "sid": &child, "task": "task B", "title": "B", "wait": 20, "routing": "queue" }),
                        ),
                        &gw,
                        McpCaller::Ambient,
                    )
                    .await
                    .unwrap(),
                )
            })
        };
        for _ in 0..200 {
            if gw
                .lock()
                .await
                .outstanding_request_ids(&child, &principal)
                .len()
                == 2
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // A finishes and leaves its answer as the newest transcript row.
        stub.run_next_turn(&stub_identity(&child)).await;
        await_notifications(&project_dir, &principal, 1).await;
        // B's row is dropped out from under the waiter, exactly as a stop does.
        // A's resolution is committed off the notifier's own thread, so watch
        // for it rather than assuming the notification implied it.
        let mut outstanding = Vec::new();
        for _ in 0..400 {
            outstanding = gw.lock().await.outstanding_request_ids(&child, &principal);
            if outstanding.len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(outstanding.len(), 1, "{outstanding:?}");
        let claim = Gateway::claim_delegation_store(&gw, &child).await;
        Gateway::drop_delegation_request_shared(
            std::sync::Arc::clone(&gw),
            &claim,
            &outstanding[0],
        )
        .await;
        drop(claim);

        let answer = tokio::time::timeout(std::time::Duration::from_secs(15), waiter)
            .await
            .expect("the wait ends when its request can no longer resolve")
            .expect("the waiter finishes");
        assert_eq!(answer["answered"], json!(false), "{answer}");
        assert_eq!(answer["state"], json!("unknown"), "{answer}");
        assert!(
            !serde_json::to_string(&answer).unwrap().contains("task A"),
            "a lost request must never come back holding a sibling's answer: {answer}"
        );
    }

    /// v0.10.1 (issue #184) — a dispatch to a session the caller never
    /// delegated is a HANDOFF, not a delegation: the target has its own parent,
    /// or is a root with its own human. The default `notify` is a default, not a
    /// request, so it must not subscribe the caller to a peer's conversation —
    /// an edge `agent_read` never draws and `agent_stop` refuses to take
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
            &run_agent_dispatch(
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
        assert_exact_keys(
            &handoff,
            &[
                "delivery",
                "notify_deliverable",
                "request_id",
                "sid",
                "status",
                "turn_id",
            ],
        );
        assert_eq!(handoff["notify_deliverable"], false, "{handoff}");
        // `notify_deliverable:false` IS the instruction to poll; a prose hint
        // saying so again is the manual this surface stopped being.
        assert!(handoff.get("hint").is_none(), "{handoff}");
        let watch = ccteam_harness::read_delegation_requests(tmp.path(), &peer)
            .expect("the handoff edge is still recorded in the ledger");
        assert_eq!(watch.requests[0].parent_sid, principal);
        assert_eq!(
            watch.requests[0].notify,
            ccteam_harness::NotifyMode::Off,
            "a peer handoff is ledger-only: {watch:?}"
        );

        // Explicit opt-in still arms the notification.
        let explicit = parse(
            &run_agent_dispatch(
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
        assert!(explicit.get("notify_deliverable").is_none(), "{explicit}");
        let watch = ccteam_harness::read_delegation_requests(tmp.path(), &peer).unwrap();
        // The opt-in is a NEW request. It does not reach back and subscribe
        // the earlier handoff, which stays the ledger-only edge it was asked
        // to be — one dispatch never rewrites another's terms (issue #201).
        assert_eq!(watch.requests.len(), 2, "{watch:?}");
        assert_eq!(
            watch.requests[0].notify,
            ccteam_harness::NotifyMode::Off,
            "{watch:?}"
        );
        assert_eq!(
            watch.requests[1].notify,
            ccteam_harness::NotifyMode::Final,
            "{watch:?}"
        );

        // The caller's OWN child keeps the default notification.
        let child = parse(
            &run_agent_spawn(
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
        assert!(child.get("notify_deliverable").is_none(), "{child}");
        let watch =
            ccteam_harness::read_delegation_requests(tmp.path(), child["sid"].as_str().unwrap())
                .unwrap();
        assert_eq!(
            watch.requests[0].notify,
            ccteam_harness::NotifyMode::Brief,
            "{watch:?}"
        );
    }

    /// GitHub #197 — `agent_read{sid,wait}` names what it RESOLVED and what it
    /// merely lost track of, separately. `resolved_requests` used to mean "no
    /// longer outstanding", which lumped a request an `agent_stop` dropped in
    /// with one the boundary answered — telling the caller it was holding an
    /// answer that does not exist.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_read_wait_separates_the_requests_it_answered_from_the_ones_it_lost() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gw, principal, stub) = queueing_dispatch_gateway(&project_dir).await;
        let child = parse(
            &run_agent_spawn(
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
        for (task, title) in [("task A", "A"), ("task B", "B")] {
            run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": &child, "task": task, "title": title }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        }
        let outstanding = gw.lock().await.outstanding_request_ids(&child, &principal);
        assert_eq!(outstanding.len(), 2, "{outstanding:?}");
        let (answered_id, lost_id) = (outstanding[0].clone(), outstanding[1].clone());
        let reader = {
            let gw = std::sync::Arc::clone(&gw);
            let principal = principal.clone();
            let child = child.clone();
            let paths = paths.clone();
            tokio::spawn(async move {
                parse(
                    &run_agent_read(
                        &ambient(&principal, "alpha", json!({ "sid": &child, "wait": 20 })),
                        &gw,
                        McpCaller::Ambient,
                        &paths,
                    )
                    .await
                    .unwrap(),
                )
            })
        };
        // Both requests are what the read is waiting on. B is then dropped out
        // from under it — exactly what an `agent_stop` does — and A is answered
        // by the boundary the read returns at.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let claim = Gateway::claim_delegation_store(&gw, &child).await;
        Gateway::drop_delegation_request_shared(std::sync::Arc::clone(&gw), &claim, &lost_id).await;
        drop(claim);
        stub.run_next_turn(&stub_identity(&child)).await;
        let read = tokio::time::timeout(std::time::Duration::from_secs(20), reader)
            .await
            .expect("the read returns at the boundary")
            .expect("the reader finishes");
        assert_eq!(
            read["resolved_requests"],
            json!([answered_id]),
            "only the request an observed boundary answered: {read}"
        );
        assert_eq!(
            read["unknown_requests"],
            json!([lost_id]),
            "a request that stopped being resolvable without being answered is \
             named as unknown, never as resolved: {read}"
        );
    }

    /// GitHub #197 — an `agent_stop` and a dispatch to the same child race for
    /// its request store. Both go through that child's one claim, so the
    /// durable record is never a half-written mixture of the two: either the
    /// dispatch's request is there whole, or the stop removed everything.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stop_racing_a_dispatch_leaves_a_whole_store_or_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let (gw, principal, _stub) = queueing_dispatch_gateway(&project_dir).await;
        for round in 0..6u32 {
            let child = parse(
                &run_agent_spawn(
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
            let dispatch = {
                let gw = std::sync::Arc::clone(&gw);
                let (principal, child) = (principal.clone(), child.clone());
                tokio::spawn(async move {
                    run_agent_dispatch(
                        &ambient(
                            &principal,
                            "alpha",
                            json!({ "sid": &child, "task": "work", "title": "T" }),
                        ),
                        &gw,
                        McpCaller::Ambient,
                    )
                    .await
                })
            };
            let stop = {
                let gw = std::sync::Arc::clone(&gw);
                let (principal, child) = (principal.clone(), child.clone());
                tokio::spawn(async move {
                    run_agent_stop(
                        &ambient(&principal, "alpha", json!({ "sid": &child })),
                        &gw,
                        McpCaller::Ambient,
                    )
                    .await
                })
            };
            let (dispatched, _stopped) = tokio::join!(dispatch, stop);
            let dispatched = dispatched.expect("the dispatch task does not panic");
            // Whatever survives on disk must be READABLE and complete: a
            // half-written store is how one writer's request lost its parent.
            if let Some(store) = ccteam_harness::read_delegation_requests(&project_dir, &child) {
                for request in &store.requests {
                    assert_eq!(request.parent_sid, principal, "round {round}: {store:?}");
                    assert_eq!(
                        request.title.as_deref(),
                        Some("T"),
                        "round {round}: {store:?}"
                    );
                }
                if let Ok(body) = dispatched.as_ref() {
                    let body = parse(body);
                    if let Some(id) = body["request_id"].as_str() {
                        assert!(
                            store.get(id).is_some() || store.requests.is_empty(),
                            "round {round}: the accepted request is neither recorded nor \
                             removed: {store:?}"
                        );
                    }
                }
            }
        }
    }

    /// GitHub #197 (F.1) — notify inheritance is decided in ONE place and holds
    /// on EVERY dispatch path, the peer handoff included. A caller that named
    /// `final` on a peer has subscribed to that peer; the follow-up that names
    /// no mode inherits it. The peer rule silences a first contact nobody asked
    /// to be notified about — not a conversation already under way.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_follow_up_to_a_peer_inherits_the_mode_its_dispatcher_chose() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gw, principal) = dispatch_gateway(false, 0, tmp.path()).await;
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
        run_agent_dispatch(
            &ambient(
                &principal,
                "alpha",
                json!({ "sid": &peer, "task": "take over the P0", "notify": "final" }),
            ),
            &gw,
            McpCaller::Ambient,
        )
        .await
        .unwrap();
        let follow_up = parse(
            &run_agent_dispatch(
                &ambient(
                    &principal,
                    "alpha",
                    json!({ "sid": &peer, "task": "and the rollback note" }),
                ),
                &gw,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        );
        assert!(
            follow_up.get("notify_deliverable").is_none(),
            "the follow-up is subscribed, so the response says nothing about polling: {follow_up}"
        );
        let watch = ccteam_harness::read_delegation_requests(tmp.path(), &peer).unwrap();
        assert_eq!(watch.requests.len(), 2, "{watch:?}");
        assert_eq!(
            watch.requests[1].notify,
            ccteam_harness::NotifyMode::Final,
            "an omitted notify keeps the mode this dispatcher chose for its \
             outstanding work on this peer: {watch:?}"
        );
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
        for tool in ["agent", "agent_read", "agent_stop"] {
            let invoke = |sid: &str| {
                let mut args = json!({ "sid": sid });
                if tool == "agent" {
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

    // ========================================================================
    // Card H — the user-programmable `pre-agent` policy hook, driven through
    // the real `agent` door (both forms) with fixture scripts. Every root is
    // injected (tempdir home + tempdir project tree), so no test can read or
    // write the developer's own `~/.ccteam`.
    // ========================================================================
    #[cfg(unix)]
    mod policy_gate_tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        const POLICY_DENIED: &str =
            ccteam_harness::execution::progress_bridge::DELEGATION_POLICY_DENIED;

        /// A gateway plus the two roots the policy layer resolves hooks from.
        struct Fixture {
            paths: CcteamPaths,
            gateway: GatewayHandle,
            /// The root session every delegation here comes from.
            principal: String,
            /// `alpha`'s working tree — its hook rung is `.ccteam/hooks/`.
            project_dir: PathBuf,
            /// Held so delegation signals have somewhere to land.
            _signals: tokio::sync::mpsc::UnboundedReceiver<crate::delegation::DelegationPulse>,
        }

        async fn fixture(tmp: &std::path::Path) -> Fixture {
            let paths = CcteamPaths {
                root: tmp.join("home"),
                projects_root: tmp.join("projects"),
            };
            let project_dir = tmp.join("alpha");
            std::fs::create_dir_all(&project_dir).unwrap();
            let (gateway, principal, signals) =
                build_dispatch_gateway(true, false, 0, None, &project_dir).await;
            // Journal + catalog + cost projection, all under the tempdir home.
            gateway.lock().await.enable_project_creation(paths.clone());
            Fixture {
                paths,
                gateway,
                principal,
                project_dir,
                _signals: signals,
            }
        }

        fn write_hook(path: &std::path::Path, body: &str, mode: u32) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        fn project_hook(project_dir: &std::path::Path) -> PathBuf {
            project_dir
                .join(".ccteam")
                .join("hooks")
                .join(crate::policy::PRE_AGENT_HOOK_FILE)
        }

        fn global_hook(paths: &CcteamPaths) -> PathBuf {
            paths
                .root
                .join("hooks")
                .join(crate::policy::PRE_AGENT_HOOK_FILE)
        }

        fn deny_hook(words: &str) -> String {
            format!("#!/bin/sh\necho '{words}' >&2\nexit 2\n")
        }

        async fn hire(fx: &Fixture, args: serde_json::Value) -> Result<String, String> {
            run_agent(
                &ambient(&fx.principal, "alpha", args),
                &fx.gateway,
                McpCaller::Ambient,
                &fx.paths,
            )
            .await
        }

        /// The journal append runs off-task; poll until `want` refusals land.
        async fn policy_events(paths: &CcteamPaths, want: usize) -> Vec<serde_json::Value> {
            let path = paths.progress_jsonl("alpha");
            for _ in 0..150 {
                let rows: Vec<serde_json::Value> = ccteam_core::progress::read_all_events(&path)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|row| row["event"] == POLICY_DENIED)
                    .collect();
                if rows.len() >= want {
                    return rows;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!(
                "fewer than {want} {POLICY_DENIED} rows in {}",
                path.display()
            );
        }

        /// The headline: one script, both delegation forms, its own words
        /// relayed verbatim — and the refusal recorded where an operator
        /// looking for "why did nothing get delegated" will find it.
        #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
        async fn a_project_policy_denies_both_delegation_forms_in_its_own_words() {
            let tmp = tempfile::TempDir::new().unwrap();
            let fx = fixture(tmp.path()).await;
            // Unconfigured: the hire behaves exactly as it always has.
            let child = parse(&hire(&fx, json!({ "task": "warm up" })).await.unwrap())["sid"]
                .as_str()
                .unwrap()
                .to_string();

            // A policy appears. No registration, no restart.
            write_hook(
                &project_hook(&fx.project_dir),
                &deny_hook("quota low, use codex"),
                0o755,
            );

            let denied_hire = hire(&fx, json!({ "task": "write the docs", "vendor": "codex" }))
                .await
                .unwrap_err();
            assert!(
                denied_hire.contains("quota low, use codex"),
                "the script's own sentence reaches the caller: {denied_hire}"
            );
            let denied_dispatch = hire(&fx, json!({ "sid": &child, "task": "one more thing" }))
                .await
                .unwrap_err();
            assert!(
                denied_dispatch.contains("quota low, use codex"),
                "dispatch is governed by the same hook: {denied_dispatch}"
            );

            let rows = policy_events(&fx.paths, 2).await;
            assert!(
                rows.iter().all(|row| row["reason"] == "policy"),
                "a verdict is tagged as a verdict: {rows:?}"
            );
            assert!(
                rows.iter()
                    .all(|row| row["parent_sid"] == fx.principal.as_str()),
                "the caller is named: {rows:?}"
            );
            assert!(
                rows.iter().any(|row| row["child_sid"] == ""),
                "the hire has no child to name yet: {rows:?}"
            );
            assert!(
                rows.iter().any(|row| row["child_sid"] == child.as_str()),
                "the dispatch names its target: {rows:?}"
            );
        }

        /// The file IS the registration: rewriting it changes the next call.
        #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
        async fn editing_the_policy_takes_effect_on_the_next_call() {
            let tmp = tempfile::TempDir::new().unwrap();
            let fx = fixture(tmp.path()).await;
            let hook = project_hook(&fx.project_dir);
            write_hook(&hook, &deny_hook("not right now"), 0o755);
            assert!(hire(&fx, json!({ "task": "first" }))
                .await
                .unwrap_err()
                .contains("not right now"));

            write_hook(&hook, "#!/bin/sh\nexit 0\n", 0o755);
            let allowed = parse(&hire(&fx, json!({ "task": "second" })).await.unwrap());
            assert!(
                allowed["sid"].as_str().is_some_and(|sid| !sid.is_empty()),
                "no caching: the edited policy allows the very next call: {allowed}"
            );
        }

        /// A broken script is fail-closed like a deny, but never WORDED like
        /// one: the refusal names the file and how it failed, so the person who
        /// has to fix it knows there is nothing to argue with.
        #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
        async fn a_broken_policy_is_a_script_error_never_a_verdict() {
            let tmp = tempfile::TempDir::new().unwrap();
            let fx = fixture(tmp.path()).await;
            let hook = project_hook(&fx.project_dir);
            let cases = [
                ("#!/bin/sh\nexit 3\n", 0o755, "exited 3"),
                ("#!/bin/sh\nsleep 30\n", 0o755, "timed out"),
                ("#!/bin/sh\nexit 0\n", 0o644, "cannot run it"),
            ];
            for (body, mode, expected) in cases {
                write_hook(&hook, body, mode);
                let error = hire(&fx, json!({ "task": "anything" })).await.unwrap_err();
                assert!(
                    error.contains("policy_script_error"),
                    "{expected}: a fault is labelled a fault: {error}"
                );
                assert!(
                    error.contains(&hook.display().to_string()),
                    "{expected}: the broken file is named: {error}"
                );
                assert!(error.contains(expected), "{expected}: got {error}");
                assert!(
                    !error.contains("denied by policy"),
                    "{expected}: a fault must not read as a verdict: {error}"
                );
            }
            let rows = policy_events(&fx.paths, 3).await;
            assert!(
                rows.iter()
                    .all(|row| row["reason"] == "policy_script_error"),
                "faults are tagged apart from verdicts on the journal: {rows:?}"
            );
        }

        /// The fallback chain: nothing anywhere allows, the global rung governs
        /// a project that states no policy, and a project policy REPLACES it.
        #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
        async fn the_global_hook_governs_a_project_that_states_no_policy() {
            let tmp = tempfile::TempDir::new().unwrap();
            let fx = fixture(tmp.path()).await;
            assert!(
                hire(&fx, json!({ "task": "no policy anywhere" }))
                    .await
                    .is_ok(),
                "an unconfigured daemon delegates exactly as before"
            );

            write_hook(&global_hook(&fx.paths), &deny_hook("global says no"), 0o755);
            assert!(hire(&fx, json!({ "task": "second" }))
                .await
                .unwrap_err()
                .contains("global says no"));

            write_hook(
                &project_hook(&fx.project_dir),
                &deny_hook("project says no"),
                0o755,
            );
            let error = hire(&fx, json!({ "task": "third" })).await.unwrap_err();
            assert!(error.contains("project says no"), "{error}");
            assert!(
                !error.contains("global says no"),
                "the rungs replace, never merge: {error}"
            );
        }

        /// A satellite-bound project's hooks live on the satellite. The local
        /// tree that happens to carry the same slug is a DIFFERENT machine's
        /// files, so its script must not be mistaken for the project's policy.
        #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
        async fn a_remote_projects_hook_is_never_run_from_the_local_tree() {
            let tmp = tempfile::TempDir::new().unwrap();
            let fx = fixture(tmp.path()).await;
            ccteam_core::config::upsert_project(
                &fx.paths.root,
                ccteam_core::ProjectEntry {
                    slug: "alpha".into(),
                    path: fx.project_dir.clone(),
                    host: "sat-a".into(),
                    remote_slug: Some("alpha".into()),
                    remote_path: None,
                    team: "dev".into(),
                    installed_at: chrono::Utc::now(),
                },
            )
            .unwrap();
            assert_eq!(
                fx.gateway.lock().await.project_bound_host("alpha"),
                "sat-a",
                "the fixture must really be satellite-bound"
            );
            write_hook(
                &project_hook(&fx.project_dir),
                &deny_hook("local tree says no"),
                0o755,
            );
            write_hook(&global_hook(&fx.paths), &deny_hook("global says no"), 0o755);

            let error = hire(&fx, json!({ "task": "remote work" }))
                .await
                .unwrap_err();
            assert!(error.contains("global says no"), "{error}");
            assert!(
                !error.contains("local tree says no"),
                "the local tree is not the remote project's policy: {error}"
            );
        }

        /// The payload is the whole point: a policy gets the quota facts, the
        /// caller's own state and the shape of the request handed to it, so it
        /// never needs a token or a round trip back into the daemon.
        #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
        async fn the_hook_is_handed_the_facts_a_policy_decides_on() {
            let tmp = tempfile::TempDir::new().unwrap();
            let fx = fixture(tmp.path()).await;
            // The account-usage snapshot reads the daemon's own catalog; point
            // it at the tempdir home and seed one observation.
            fx.gateway
                .lock()
                .await
                .enable_persistence(&fx.paths.root)
                .unwrap();
            ccteam_harness::usage_catalog::record_vendor_usage_in(
                &fx.paths.root,
                "claude",
                "status card",
                &ccteam_harness::AccountUsage {
                    five_hour_pct: Some(8),
                    ..Default::default()
                },
            )
            .unwrap();
            // One existing delegation, so the counts are not all zero.
            hire(&fx, json!({ "task": "warm up" })).await.unwrap();

            let dump = tmp.path().join("stdin.json");
            write_hook(
                &project_hook(&fx.project_dir),
                &format!("#!/bin/sh\ncat > '{}'\nexit 0\n", dump.display()),
                0o755,
            );
            let task = "R".repeat(900);
            hire(
                &fx,
                json!({
                    "task": task,
                    "vendor": "codex",
                    "model": "gpt-5.1-codex-max",
                    "role": "reviewer",
                    "title": "review the diff",
                }),
            )
            .await
            .unwrap();

            let raw = std::fs::read_to_string(&dump).unwrap();
            assert_eq!(raw.lines().count(), 1, "one compact line: {raw}");
            let payload: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(payload["kind"], "hire");
            assert_eq!(payload["caller"]["sid"], fx.principal.as_str());
            assert_eq!(payload["caller"]["vendor"], "claude");
            assert_eq!(payload["caller"]["depth"], 0);
            assert_eq!(payload["caller"]["project"], "alpha");
            assert_eq!(payload["request"]["vendor"], "codex");
            assert_eq!(payload["request"]["model"], "gpt-5.1-codex-max");
            assert_eq!(payload["request"]["role"], "reviewer");
            assert_eq!(payload["request"]["title"], "review the diff");
            assert_eq!(payload["request"]["wait"], 0);
            assert_eq!(payload["request"]["task_chars"], 900);
            assert_eq!(
                payload["request"]["task_head"]
                    .as_str()
                    .unwrap()
                    .chars()
                    .count(),
                crate::policy::TASK_HEAD_MAX_CHARS,
                "the head is capped, the full prompt never leaves the daemon: {payload}"
            );
            assert_eq!(
                payload["usage"]["claude"]["windows"][0],
                json!({"w": "5h", "pct": 8}),
                "the quota facts are handed in: {payload}"
            );
            assert_eq!(payload["counts"]["children"], 1, "{payload}");
            assert_eq!(payload["counts"]["delegated"], 1, "{payload}");
        }
    }
}

// ============================================================================
// The per-caller tool face + the byte budgets it exists to hit, end to end
// through `dispatch_as` (the face rules themselves are unit-tested in
// `super::face`).
// ============================================================================
#[cfg(test)]
mod tool_face_tests {
    use super::session_tool_tests::*;
    use super::*;
    use serde_json::json;

    fn compact_len(value: &serde_json::Value) -> usize {
        serde_json::to_string(value).unwrap().len()
    }

    fn discovery(method: &str, sid: &str, secret: &str) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": { "arguments": { "_caller_sid": sid, "_caller_secret": secret } }
        })
    }

    fn dispatcher(paths: &CcteamPaths, gateway: &GatewayHandle) -> McpDispatch {
        McpDispatch {
            paths: paths.clone(),
            sink: None,
            pending: None,
            gateway: Some(std::sync::Arc::clone(gateway)),
        }
    }

    /// G1, end to end — a session AT the delegation depth cap is served the
    /// leaf face and the leaf instructions: one read tool, the depth-cap fact,
    /// and none of the orchestration policy it could not act on anyway.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_depth_capped_child_is_served_the_leaf_face_and_no_orchestration_policy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, secrets) = face_gateway(tmp.path()).await;
        gateway
            .lock()
            .await
            .set_delegation_config(ccteam_core::DelegationConfig {
                max_depth: 1,
                ..Default::default()
            });

        let child = parse(
            &run_agent(
                &ambient(&root, "alpha", json!({ "task": "leaf work" })),
                &gateway,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();

        let dispatch = dispatcher(&paths, &gateway);
        let child_secret = secret_for(&secrets, &child).await;
        let listed = dispatch
            .dispatch_as(
                discovery("tools/list", &child, &child_secret),
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["status", "chat_send_file", "agent_read"],
            "a capped child may see the board, read the team and answer its chat"
        );

        let init = dispatch
            .dispatch_as(
                discovery("initialize", &child, &child_secret),
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        let instructions = init["result"]["instructions"].as_str().unwrap();
        assert!(
            instructions.contains(&format!("You are {child} in project alpha.")),
            "{instructions}"
        );
        assert!(
            instructions.contains("at the delegation depth cap and cannot hire agents"),
            "{instructions}"
        );
        assert!(
            !instructions.contains("never shell out to a vendor CLI"),
            "a session that cannot hire must not be taught how: {instructions}"
        );

        // The ROOT, by contrast, gets the orchestrating face — and pays for it.
        let root_secret = secret_for(&secrets, &root).await;
        let root_list = dispatch
            .dispatch_as(
                discovery("tools/list", &root, &root_secret),
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        let root_names: Vec<&str> = root_list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(root_names.contains(&"agent"), "{root_names:?}");
        assert!(
            root_names.contains(&"chat_send_file"),
            "a root answers its own human: {root_names:?}"
        );
        let root_init = dispatch
            .dispatch_as(
                discovery("initialize", &root, &root_secret),
                McpCaller::Ambient,
            )
            .await
            .unwrap();

        // G1 — the point of the whole exercise: the leaf's ambient bill is a
        // fraction of the orchestrator's. (The absolute leaf budget is pinned
        // in `protocol::tests::leaf_ambient_cost_stays_under_two_kilobytes`.)
        let leaf_cost = compact_len(&listed["result"]) + instructions.len();
        let root_cost = compact_len(&root_list["result"])
            + root_init["result"]["instructions"].as_str().unwrap().len();
        assert!(
            leaf_cost * 2 < root_cost,
            "a leaf must pay less than half an orchestrator's ambient tax \
             (leaf {leaf_cost} B vs root {root_cost} B)"
        );
    }

    /// `agent{tools}` is persisted, so a resume serves the SAME face — and an
    /// unknown value is refused rather than silently widened.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_tools_argument_is_persisted_and_validated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, secrets) = face_gateway(tmp.path()).await;

        for (requested, stored) in [
            ("read", Some("read")),
            ("none", Some("none")),
            ("full", None),
        ] {
            let sid = parse(
                &run_agent(
                    &ambient(&root, "alpha", json!({ "task": "x", "tools": requested })),
                    &gateway,
                    McpCaller::Ambient,
                    &paths,
                )
                .await
                .unwrap(),
            )["sid"]
                .as_str()
                .unwrap()
                .to_string();
            let meta = ccteam_harness::execution::session_meta::read_session_meta(tmp.path(), &sid)
                .unwrap();
            assert_eq!(meta.tool_face.as_deref(), stored, "tools:{requested}");
        }

        let err = run_agent(
            &ambient(&root, "alpha", json!({ "task": "x", "tools": "readonly" })),
            &gateway,
            McpCaller::Ambient,
            &paths,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err,
            "agent: invalid `tools` `readonly` (expected `full` | `read` | `none`)"
        );

        // A `tools:"none"` child lists nothing, and its instructions say so.
        let muted = parse(
            &run_agent(
                &ambient(&root, "alpha", json!({ "task": "x", "tools": "none" })),
                &gateway,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let dispatch = dispatcher(&paths, &gateway);
        let muted_secret = secret_for(&secrets, &muted).await;
        let listed = dispatch
            .dispatch_as(
                discovery("tools/list", &muted, &muted_secret),
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        assert_eq!(listed["result"], json!({ "tools": [] }));
        let init = dispatch
            .dispatch_as(
                discovery("initialize", &muted, &muted_secret),
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        assert!(init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("(no ccteam tools)"));
    }

    /// P1-1 — the face is a property of the PRINCIPAL, which exists before the
    /// vendor process does. The child's MCP client fetches `tools/list` once,
    /// at startup, and that request beat `meta.json` to disk: a `tools:"none"`
    /// child was served all six tools and later really called `status`
    /// (daemon.log, 2026-08-31). Deleting the meta file is the proof that
    /// nothing on this path reads it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_tool_face_is_resolved_without_reading_meta_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, secrets) = face_gateway(tmp.path()).await;
        let dispatch = dispatcher(&paths, &gateway);

        for (requested, expected) in [
            ("none", Vec::<&str>::new()),
            // A hired child that a chat is wired to keeps `chat_send_file`;
            // the READ part of the face is `status` + `agent_read`.
            ("read", vec!["status", "chat_send_file", "agent_read"]),
        ] {
            let child = parse(
                &run_agent(
                    &ambient(&root, "alpha", json!({ "task": "x", "tools": requested })),
                    &gateway,
                    McpCaller::Ambient,
                    &paths,
                )
                .await
                .unwrap(),
            )["sid"]
                .as_str()
                .unwrap()
                .to_string();
            let child_secret = secret_for(&secrets, &child).await;

            // The face answers the moment the create call returns…
            let names = |listed: &serde_json::Value| -> Vec<String> {
                listed["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| tool["name"].as_str().unwrap().to_string())
                    .collect()
            };
            let listed = dispatch
                .dispatch_as(
                    discovery("tools/list", &child, &child_secret),
                    McpCaller::Ambient,
                )
                .await
                .unwrap();
            assert_eq!(names(&listed), expected, "tools:{requested} at create time");

            // …and keeps answering with the file gone, which is what a child
            // asking before the write lands amounts to.
            let meta_path = ccteam_harness::execution::turns_mirror::chat_dir(tmp.path(), &child)
                .join("meta.json");
            assert!(meta_path.exists(), "the audit copy is still written");
            std::fs::remove_file(&meta_path).unwrap();
            let listed = dispatch
                .dispatch_as(
                    discovery("tools/list", &child, &child_secret),
                    McpCaller::Ambient,
                )
                .await
                .unwrap();
            assert_eq!(
                names(&listed),
                expected,
                "tools:{requested} must not depend on meta.json"
            );
        }
    }

    /// P1-1, the earliest moment there is: the child's MCP client asks for its
    /// tool list DURING the spawn, while its principal is still `Spawning` and
    /// nothing about the session is on disk. That request used to fail the
    /// authorization-grade check, leave the resolver with no caller to narrow
    /// by, and degrade to the full face — the measured `tools=Some(6)` a
    /// `tools:"none"` child then held for its whole life.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spawning_childs_face_is_already_narrowed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, _root, _secrets) = face_gateway(tmp.path()).await;
        gateway.lock().await.principals().reserve(
            "s99",
            "spawning-secret",
            crate::principals::PrincipalFacts {
                tool_face: Some("none".into()),
                parent_sid: Some("s1".into()),
                ..crate::principals::PrincipalFacts::new("alpha", "", 1)
            },
        );
        let dispatch = dispatcher(&paths, &gateway);
        let listed = dispatch
            .dispatch_as(
                discovery("tools/list", "s99", "spawning-secret"),
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        assert_eq!(
            listed["result"],
            json!({ "tools": [] }),
            "a mid-spawn child hired with no tools must never see the full face"
        );

        // Listing is not authority: the same principal still cannot ACT.
        let denied = execute_session_tool_with_paths(
            &call(
                "agent_read",
                json!({ "_caller_sid": "s99", "_caller_secret": "spawning-secret" }),
            ),
            Some(&gateway),
            McpCaller::Ambient,
            &paths,
        )
        .await;
        assert_eq!(denied["result"]["isError"], true, "{denied}");
    }

    /// P2-5 — the descendant rule is right; the refusal was a dead end. A
    /// reconnected hand-started client is a NEW ledger node, so the sessions
    /// its previous node hired are nobody's descendants, and it could not stop
    /// its own work at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_non_descendant_stop_refusal_names_the_way_out() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (gateway, root, _secrets) = face_gateway(tmp.path()).await;
        let stranger = parse(
            &run_agent_spawn(
                &ambient(&root, "alpha", json!({ "vendor": "claude" })),
                &gateway,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        // `stranger` is the ROOT's child, so a sibling caller is not its ancestor.
        let sibling = parse(
            &run_agent_spawn(
                &ambient(&root, "alpha", json!({ "vendor": "claude" })),
                &gateway,
                McpCaller::Ambient,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        let refusal = run_agent_stop(
            &ambient(&sibling, "alpha", json!({ "sid": &stranger })),
            &gateway,
            McpCaller::Ambient,
        )
        .await
        .unwrap_err();
        assert!(
            refusal.contains("is not a descendant of the caller"),
            "{refusal}"
        );
        assert!(
            refusal.contains(&format!("POST /api/v1/sessions/{stranger}/stop")),
            "the refusal must name the way out: {refusal}"
        );
        assert!(refusal.contains("web console"), "{refusal}");
    }

    /// G2 — the default `status` body, and the beacon that must be the same
    /// bytes (it exists for hosts that see names only; a second shape would be
    /// a second thing to keep true)."""
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_brief_is_tiny_and_the_beacon_is_byte_identical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, secrets) = face_gateway(tmp.path()).await;
        let secret = secret_for(&secrets, &root).await;
        let call_status = |name: &'static str, detail: Option<&'static str>| {
            let gateway = std::sync::Arc::clone(&gateway);
            let paths = paths.clone();
            let root = root.clone();
            let secret = secret.clone();
            async move {
                let mut args = json!({ "_caller_sid": root, "_caller_secret": secret });
                if let Some(detail) = detail {
                    args["detail"] = json!(detail);
                }
                let req = json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": name, "arguments": args }
                });
                let resp = execute_status(&req, Some(&gateway), McpCaller::Ambient, &paths).await;
                resp["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_string()
            }
        };

        let brief = call_status("status", None).await;
        let beacon = call_status(protocol::STATUS_BEACON_TOOL_NAME, None).await;
        assert_eq!(beacon, brief, "the beacon IS the brief body");
        assert!(
            brief.len() <= 300,
            "status brief is {} B: {brief}",
            brief.len()
        );
        let body: serde_json::Value = serde_json::from_str(&brief).unwrap();
        assert_eq!(body["project"], "alpha");
        assert_eq!(body["host"], "local");
        assert!(body["hire"].is_array());
        assert!(body["cost_24h_usd"].is_number());
        // Operator data lives only in `full`.
        assert!(body.get("daemon").is_none(), "{brief}");
        assert!(body.get("projects").is_none(), "{brief}");

        // The beacon takes no `detail`: it is the brief body or nothing.
        let beacon_full = call_status(protocol::STATUS_BEACON_TOOL_NAME, Some("full")).await;
        assert_eq!(beacon_full, brief);

        let full: serde_json::Value =
            serde_json::from_str(&call_status("status", Some("full")).await).unwrap();
        for key in ["models", "vendors", "routing", "daemon", "projects"] {
            assert!(full.get(key).is_some(), "full must carry `{key}`: {full}");
        }
        let bad = call_status("status", Some("everything")).await;
        assert!(bad.contains("invalid `detail`"), "{bad}");
    }

    /// U1 — `status{detail:"usage"}`: the caller's own context headroom plus
    /// every harness account ccteam has an unexpired observation for. The two
    /// scheduling numbers, one call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_usage_reports_your_context_and_every_observed_account() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let paths = CcteamPaths {
            root: home.clone(),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, secrets) = face_gateway(tmp.path()).await;
        // The catalog lives under the gateway's injected state root.
        gateway
            .lock()
            .await
            .enable_persistence(home.clone())
            .unwrap();
        let secret = secret_for(&secrets, &root).await;

        let now = chrono::Utc::now();
        let iso = |hours: i64| (now + chrono::Duration::hours(hours)).to_rfc3339();
        // claude: every window, including a per-model one.
        ccteam_harness::usage_catalog::record_vendor_usage_in(
            &home,
            "claude",
            "status card",
            &ccteam_harness::AccountUsage {
                subscription: Some("max".into()),
                five_hour_pct: Some(8),
                five_hour_resets_at: Some(iso(2)),
                weekly_pct: Some(23),
                weekly_resets_at: Some(iso(48)),
                weekly_severity: Some("warning".into()),
                credits_pct: Some(3),
                model_windows: vec![ccteam_harness::ModelWindow {
                    model: "Fable".into(),
                    pct: Some(16),
                    resets_at: Some(iso(48)),
                }],
            },
        )
        .unwrap();
        // codex: one live window plus one the vendor's own reset already
        // passed — the expired one must not be rendered as current.
        ccteam_harness::usage_catalog::record_vendor_usage_in(
            &home,
            "codex",
            "session release",
            &ccteam_harness::AccountUsage {
                five_hour_pct: Some(90),
                five_hour_resets_at: Some(iso(-1)),
                weekly_pct: Some(12),
                weekly_resets_at: Some(iso(72)),
                ..Default::default()
            },
        )
        .unwrap();
        // grok: proves the map is vendor-generic, not a claude/codex pair.
        ccteam_harness::usage_catalog::record_vendor_usage_in(
            &home,
            "grok",
            "status card",
            &ccteam_harness::AccountUsage {
                subscription: Some("SuperGrok Heavy".into()),
                weekly_pct: Some(42),
                weekly_resets_at: Some(iso(24)),
                ..Default::default()
            },
        )
        .unwrap();

        // The caller's own session has reported its context once.
        let resolved = gateway.lock().await.session_resolve_any(&root).unwrap();
        ccteam_harness::execution::turns_mirror::append_turn(
            &resolved.project_dir,
            &root,
            &ccteam_harness::execution::turns_mirror::TurnRecord {
                exec_turn_id: None,
                turn_id: format!("{root}-1"),
                ts: chrono::Utc::now(),
                vendor: "claude".into(),
                role: String::new(),
                user: String::new(),
                assistant: "ok".into(),
                usage: serde_json::Value::Null,
                status: Some(ccteam_harness::TurnStatus {
                    model: None,
                    context: Some(ccteam_harness::ContextUsage::known(
                        63,
                        100,
                        ccteam_harness::ContextSource::Reported,
                    )),
                    turn: 1,
                    cost_usd: None,
                    tokens_total: None,
                }),
                tool_calls: Vec::new(),
                attachments: Vec::new(),
                outcome: None,
                error_kind: None,
                error: None,
                conclusion: None,
            },
        )
        .unwrap();

        let call = |caller: McpCaller, args: serde_json::Value| {
            let gateway = std::sync::Arc::clone(&gateway);
            let paths = paths.clone();
            async move {
                let req = json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": { "name": "status", "arguments": args }
                });
                let resp = execute_status(&req, Some(&gateway), caller, &paths).await;
                serde_json::from_str::<serde_json::Value>(
                    resp["result"]["content"][0]["text"].as_str().unwrap(),
                )
                .unwrap()
            }
        };

        let body = call(
            McpCaller::Ambient,
            json!({ "_caller_sid": root, "_caller_secret": secret, "detail": "usage" }),
        )
        .await;

        // The brief body is still underneath — `usage` ADDS, never replaces.
        assert_eq!(body["project"], "alpha");
        assert!(body["hire"].is_array());

        // `you`: the caller's own sid + its latest context percentage.
        assert_eq!(body["you"]["sid"], json!(root));
        assert_eq!(body["you"]["context_pct"], json!(63));

        // One entry per harness with a live observation, keyed by harness.
        let usage = body["usage"].as_object().expect("a usage map");
        assert_eq!(
            usage.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["claude", "codex", "grok"],
            "only observed harnesses, and every one of them: {body}"
        );
        assert_eq!(usage["claude"]["subscription"], json!("max"));
        assert_eq!(usage["claude"]["source"], json!("status card"));
        assert!(usage["claude"]["observed"].is_string());
        let claude: Vec<&serde_json::Value> = usage["claude"]["windows"]
            .as_array()
            .unwrap()
            .iter()
            .collect();
        assert_eq!(claude[0]["w"], json!("5h"));
        assert_eq!(claude[0]["pct"], json!(8));
        assert_eq!(claude[1]["w"], json!("7d"));
        assert_eq!(claude[1]["severity"], json!("warning"));
        assert_eq!(claude[2]["w"], json!("7d"));
        assert_eq!(claude[2]["model"], json!("Fable"));
        assert_eq!(claude[2]["pct"], json!(16));
        assert_eq!(claude[3], &json!({"w": "credits", "pct": 3}));

        // The codex 5h window's own reset has passed: gone, not stale.
        let codex = usage["codex"]["windows"].as_array().unwrap();
        assert_eq!(codex.len(), 1, "expired windows are dropped: {body}");
        assert_eq!(codex[0]["w"], json!("7d"));
        assert!(usage["codex"].get("subscription").is_none());

        // A harness nobody has heard from is ABSENT — never a zeroed row.
        assert!(usage.get("kimi").is_none(), "{body}");

        // `full` carries the same two sections.
        let full = call(
            McpCaller::Ambient,
            json!({ "_caller_sid": root, "_caller_secret": secret, "detail": "full" }),
        )
        .await;
        assert_eq!(full["you"]["context_pct"], json!(63));
        assert_eq!(full["usage"]["claude"]["subscription"], json!("max"));

        // The quieter tiers pay nothing for it.
        let brief = call(
            McpCaller::Ambient,
            json!({ "_caller_sid": root, "_caller_secret": secret }),
        )
        .await;
        assert!(brief.get("usage").is_none(), "{brief}");
        assert!(brief.get("you").is_none(), "{brief}");

        // An Admin token is not a session: the account map still answers, but
        // there is no `you` to report.
        let admin = call(McpCaller::Admin, json!({ "detail": "usage" })).await;
        assert!(admin.get("you").is_none(), "{admin}");
        assert_eq!(
            admin["usage"]["grok"]["subscription"],
            json!("SuperGrok Heavy")
        );
    }

    /// U1 — a session that has never reported its context says only WHO it is:
    /// a fabricated `0` would read as "all the room in the world".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_usage_omits_an_unobserved_context_rather_than_zeroing_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, secrets) = face_gateway(tmp.path()).await;
        let secret = secret_for(&secrets, &root).await;
        let req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "status", "arguments": {
                "_caller_sid": root, "_caller_secret": secret, "detail": "usage"
            }}
        });
        let resp = execute_status(&req, Some(&gateway), McpCaller::Ambient, &paths).await;
        let body: serde_json::Value =
            serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["you"], json!({ "sid": root }));
        // No observation anywhere ⇒ no `usage` key at all, not an empty object
        // dressed up as an answer.
        assert!(body.get("usage").is_none(), "{body}");
    }

    /// G2 — a ten-row roster stays inside its budget, and `tree` describes
    /// exactly the rows that were returned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_ten_row_roster_fits_its_budget_and_the_tree_covers_the_rows_returned() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, _secrets) = face_gateway(tmp.path()).await;
        gateway
            .lock()
            .await
            .set_delegation_config(ccteam_core::DelegationConfig {
                max_children: 20,
                ..Default::default()
            });
        // Hired straight through the spawn branch (no first task), so the
        // rows carry no derived title — the worst case for the budget is a
        // roster of anonymous workers.
        for _ in 0..12 {
            run_agent_spawn(
                &ambient(&root, "alpha", json!({})),
                &gateway,
                McpCaller::Ambient,
            )
            .await
            .unwrap();
        }

        let roster = parse(
            &run_agent_read(
                &ambient(&root, "alpha", json!({ "tree": true })),
                &gateway,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        );
        let rows = roster["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), AGENT_READ_DEFAULT_N, "default page is 10 rows");
        assert_eq!(roster["truncated"], json!(true));
        assert_eq!(roster["total"], json!(13));
        let bytes = compact_len(&roster);
        assert!(bytes <= 1200, "10-row roster is {bytes} B: {roster}");

        // Every returned row appears exactly once in the topology, and a
        // roleless session spells no empty `role`.
        fn count(node: &serde_json::Value) -> usize {
            assert!(
                node.get("role").is_none(),
                "an empty role must be omitted: {node}"
            );
            1 + node["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(count)
                .sum::<usize>()
        }
        let tree_nodes: usize = roster["tree"].as_array().unwrap().iter().map(count).sum();
        assert_eq!(
            tree_nodes,
            rows.len(),
            "the tree covers the returned rows and nothing else: {roster}"
        );
        for row in rows {
            assert!(row.get("role").is_none(), "empty role omitted: {row}");
            assert!(row.get("title").is_none(), "{row}");
        }
    }

    /// `agent_read{sid}` answers NEWEST-first by default (the overwhelmingly
    /// common read is "what did it say"), and `since` flips it to forward
    /// paging without needing `tail:false` spelled out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn transcript_reads_newest_first_unless_since_pages_forward() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, _secrets) = face_gateway(tmp.path()).await;
        let child = parse(
            &run_agent(
                &ambient(&root, "alpha", json!({ "task": "x", "notify": "off" })),
                &gateway,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        for i in 0..15u32 {
            ccteam_harness::execution::turns_mirror::append_turn(
                tmp.path(),
                &child,
                &turn(&format!("t{i}")),
            )
            .unwrap();
        }

        let read = |args: serde_json::Value| {
            let gateway = std::sync::Arc::clone(&gateway);
            let paths = paths.clone();
            let args = ambient(&root, "alpha", args);
            async move {
                parse(
                    &run_agent_read(&args, &gateway, McpCaller::Ambient, &paths)
                        .await
                        .unwrap(),
                )
            }
        };

        // A bare read answers "what did it say": the newest turn, alone.
        let newest = read(json!({ "sid": child })).await;
        let turns = newest["turns"].as_array().unwrap();
        assert_eq!(turns.len(), AGENT_READ_TRANSCRIPT_DEFAULT_N);
        assert_eq!(
            turns.last().unwrap()["turn_id"],
            "t14",
            "the default page ends at the newest turn: {newest}"
        );
        assert_eq!(
            newest["remaining"],
            json!(14),
            "the rest is counted, not shown"
        );

        // Asking for ten still pages ten, newest last.
        let ten = read(json!({ "sid": child, "n": 10 })).await;
        let turns = ten["turns"].as_array().unwrap();
        assert_eq!(
            turns.len(),
            10,
            "an explicit `n` is the page, not a default"
        );
        assert_eq!(turns[0]["turn_id"], "t5");

        let forward = read(json!({ "sid": child, "since": "t1" })).await;
        let forward_turns = forward["turns"].as_array().unwrap();
        assert_eq!(
            forward_turns[0]["turn_id"], "t2",
            "`since` pages FORWARD from the cursor: {forward}"
        );

        // An explicit `tail` still wins over the `since`-derived default.
        let tailed = read(json!({ "sid": child, "since": "t1", "tail": true })).await;
        assert_eq!(
            tailed["turns"].as_array().unwrap().last().unwrap()["turn_id"],
            "t14"
        );
    }

    /// "Never seen here" and "you stopped it" are the same absence from the
    /// live map but two different next moves, so they must not share an error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stopped_session_is_not_reported_as_unknown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        let (gateway, root, _secrets) = face_gateway(tmp.path()).await;
        let child = parse(
            &run_agent(
                &ambient(&root, "alpha", json!({ "task": "x", "notify": "off" })),
                &gateway,
                McpCaller::Ambient,
                &paths,
            )
            .await
            .unwrap(),
        )["sid"]
            .as_str()
            .unwrap()
            .to_string();
        run_agent_stop(
            &ambient(&root, "alpha", json!({ "sid": child })),
            &gateway,
            McpCaller::Ambient,
        )
        .await
        .unwrap();

        let stopped = run_agent(
            &ambient(&root, "alpha", json!({ "sid": child, "task": "more" })),
            &gateway,
            McpCaller::Ambient,
            &paths,
        )
        .await
        .unwrap_err();
        assert!(
            stopped.contains(&format!("{child} was stopped at")),
            "{stopped}"
        );
        assert!(
            stopped.contains("agent_read still reads it — hire a new one to continue"),
            "{stopped}"
        );

        let unknown = run_agent(
            &ambient(&root, "alpha", json!({ "sid": "s999", "task": "more" })),
            &gateway,
            McpCaller::Ambient,
            &paths,
        )
        .await
        .unwrap_err();
        assert_eq!(unknown, "agent: unknown session s999 in project alpha");
    }
}
