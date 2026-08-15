//! v0.8.6 W5b ResSessions — session resource endpoints over the gateway spine.
//!
//! These are the network face of the live IM gateway's session lifecycle
//! (the W5b spine: [`Gateway::session_views`] /
//! [`Gateway::create_session_api`] / [`Gateway::submit_to_sid`] /
//! [`Gateway::stop_session`]). The web server runs in the same daemon
//! process that owns the in-memory `s{n}` session map, so when a gateway
//! is attached ([`AppState::gateway`] = `Some`) these endpoints drive it
//! directly under its `Mutex`.
//!
//! Routes (all `/api/v1`, behind the shared `auth_layer`):
//!
//! - `GET    /api/v1/projects/{slug}/sessions`        → `[SessionView]` for the slug
//! - `POST   /api/v1/projects/{slug}/sessions`        → create → 201 `{sid}`
//! - `GET    /api/v1/sessions/{sid}`                  → `{sid, events}` history
//! - `POST   /api/v1/sessions/{sid}/turn`             → submit → 202 `{accepted:true}`
//! - `GET    /api/v1/sessions/{sid}/events`           → SSE (filtered by `sid`)
//! - `POST   /api/v1/sessions/{sid}/stop`             → stop → 200 `{stopped:true}`
//! - `POST   /api/v1/sessions/{sid}/interrupt`        → interrupt running turn → 200 `{interrupted:true}`
//!
//! **No-gateway contract (locked W5b)**: the standalone "internal web"
//! path runs without a daemon gateway ([`AppState::gateway`] = `None`).
//! Every session endpoint then returns **503** — there is no live session
//! map to act on. The 503 short-circuit is the first thing each handler
//! checks.
//!
//! **SSE filter key (cross-stage from the spine)**: a per-session SSE
//! handler keeps only events whose `sid` matches its target. Every
//! web-API session shares `chat_id == "web-api"`, so filtering MUST be on
//! the `sid` field — never `chat_id`. The event source is the **gateway's
//! own event stream** ([`Gateway::subscribe_events`](ccteam_im::gateway::Gateway::subscribe_events)),
//! a broadcast tee of every [`GatewayEvent`](ccteam_im::gateway::GatewayEvent)
//! the gateway emits (pump answers + progress, turn-timeout, choice prompts).
//! Each event tied to a tracked session carries `sid == Some("s{n}")`, so
//! this handler keeps the ones whose `sid` matches its target and drops the
//! rest. (The earlier file-watcher `EventBus` source only ever saw flat
//! `<slug>.jsonl` progress with `sid == None`, so a real session got nothing
//! but keep-alives — fix #2.)
//!
//! **History**: the gateway keeps no in-memory transcript, so the history
//! endpoint tails the per-session source on disk. The gateway session id
//! (`s{n}`) never appears in the flat `<slug>.jsonl` progress, so the
//! handler resolves `sid → {project_dir, sid, …}` via
//! [`Gateway::session_resolve`](ccteam_im::gateway::Gateway::session_resolve)
//! (under the gateway lock, which it drops before the blocking fs read)
//! then reads the ccteam-owned mirror
//! `<project_dir>/.ccteam/chat/<sid>/turns.jsonl` via
//! [`read_all_turns`](ccteam_harness::execution::turns_mirror::read_all_turns).
//! v0.8.8 F1 — the mirror is keyed by the session **sid**, not the role, so
//! two same-role sessions have independent histories (no cross-bleed). It is
//! best-effort: an empty `events` array is a valid 200 when nothing has been
//! written yet. A `sid` unknown to the gateway is a 404.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Extension, Json,
};
use ccteam_harness::execution::turns_mirror::{read_all_turns, TurnRecord};
use ccteam_harness::{
    AgentVendor, ChoicePrompt, HarnessAdapter, PermissionMode, SessionProtocol, ThreadHandle,
    ThreadStatus,
};
use ccteam_im::gateway::{GatewayEvent, SessionView};
use ccteam_im::transport::MessageOption;
use futures::stream::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use utoipa::ToSchema;

use super::actions::{FormOrJson, InputMode};
use crate::state::AppState;

#[derive(Debug, Default)]
struct RemovedHostField(bool);

impl<'de> Deserialize<'de> for RemovedHostField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

/// Keep-alive cadence for the per-session SSE stream. Mirrors
/// [`super::sse`]'s 15s contract (its constant is private; we restate it
/// to keep the same reverse-proxy idle-timeout defeat).
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);

/// 503 body for the no-gateway (standalone internal-web) path. Returned
/// by every session endpoint when [`AppState::gateway`] is `None`.
pub(crate) fn no_gateway() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "no live gateway: session API unavailable on standalone web"})),
    )
        .into_response()
}

/// 404 body for a `sid` the gateway does not track.
fn unknown_session(sid: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": format!("unknown session: {sid}")})),
    )
        .into_response()
}

/// Map a wire vendor token (`"claude"` / `"codex"`, case-insensitive) to
/// the harness [`AgentVendor`]. The web layer owns this mapping so it
/// never depends on the gateway's private `parse_vendor` (the spine note:
/// "map its own request string to AgentVendor before calling
/// create_session_api"). Matches `AgentVendor`'s lowercase serde form.
fn parse_vendor(raw: &str) -> Result<AgentVendor, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" => Ok(AgentVendor::Claude),
        "codex" => Ok(AgentVendor::Codex),
        "grok" => Ok(AgentVendor::Grok),
        "opencode" => Ok(AgentVendor::Opencode),
        "kimi" => Ok(AgentVendor::Kimi),
        "pi" => Ok(AgentVendor::Pi),
        "dsh" => Ok(AgentVendor::Dsh),
        other => Err(format!(
            "unknown vendor: {other} (expected claude|codex|grok|opencode|kimi|pi|dsh)"
        )),
    }
}

/// `GET /api/v1/projects/{slug}/sessions`
///
/// 200 `[{sid, project, role, vendor, current, status}]` — the gateway's
/// [`SessionView`](ccteam_im::gateway::SessionView)s filtered to this
/// project. Empty array when the project has no live sessions. 503 with no
/// gateway.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/sessions",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Live sessions `[{sid, project, role, vendor, permission_mode, current, status}]`", body = serde_json::Value),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_list_sessions(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
) -> Response {
    // v0.8.18 档1 — session visibility derives from the project (project is the
    // unit of ownership): if you can't see the project, you see none of its
    // sessions. 404 (not 403) so an unowned slug isn't revealed.
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // session_views() + live_turns() are sync after the lock (and both cheap,
    // in-memory) — no `.await` and no file read is held under it.
    let (mut views, live_turns) = {
        let guard = gw.lock().await;
        (
            guard
                .session_views()
                .into_iter()
                .filter(|v| v.project == slug)
                .collect::<Vec<_>>(),
            guard.live_turns(),
        )
    };
    apply_progress_activity_status(&app.paths, &slug, &mut views, &live_turns);
    Json(views).into_response()
}

/// 404 for a project the caller can't see (per-user isolation; 404 not 403 so
/// an unowned slug's existence isn't revealed).
pub(crate) fn project_not_visible(slug: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": format!("project not found: {slug}")})),
    )
        .into_response()
}

/// v0.8.18 档1 — gate a by-`sid` endpoint by the session's PROJECT (project is
/// the unit of ownership; a session inherits its project's owner). Resolves
/// `sid` → project via the gateway (live map first, then a stopped session's
/// on-disk `meta.json`), then checks the caller may see it. Returns `Some(404)`
/// when the sid is unknown OR its project isn't visible (the two are
/// indistinguishable, so sids in other users' projects can't be probed).
/// `None` = allowed (no-gateway → the handler does its own check, or a visible
/// project). Resolving *stopped* sessions here (not just live ones) lets an
/// authorised caller reach a since-evicted session so the turn handler can
/// cold-resume it (resume-by-sid) instead of 404-ing.
///
/// Cross-user fix (2026-07-28) — the gate runs for the ADMIN too. It used to short-circuit on
/// `is_admin`, which contradicted the very policy it delegates to:
/// `can_see_owner` deliberately keeps the admin OUT of a tenant's projects
/// (`/api/v1/projects/<tenant-slug>/…` 404s), yet every by-sid door
/// (`GET /sessions/{sid}`, `/status`, `/events`, `POST /turn`, `/stop`, …)
/// stayed wide open on the same resources. One door, one policy.
async fn gate_sid(app: &AppState, identity: &crate::auth::Identity, sid: &str) -> Option<Response> {
    // No live gateway → the handler runs its own no-gateway path; don't gate.
    let gw = app.gateway.as_ref()?;
    let project = {
        let guard = gw.lock().await;
        guard.project_slug_for_sid(sid)
    };
    match project {
        Some(p) if crate::routes::api_v1::can_see_project(app, identity, &p) => None,
        _ => Some(unknown_session(sid)),
    }
}

/// Replace the gateway's cheap `"live"` hint with the real activity
/// (`working|idle|stale|stuck`) through the SHARED resolver — the same
/// `ccteam_core::stall::classify_session_activity` IM `/status` and MCP
/// `session_list` answer through, so the SPA rail and a phone's `/status` card
/// can never tell the user different things about one session.
///
/// `live_turns` is the daemon's in-flight-turn snapshot, taken under the same
/// lock that produced `views` (cheap, no I/O) — the file read then happens here,
/// off the lock. It is what keeps a mid-turn session honest when its project's
/// progress stream cannot be read.
///
/// A project the catalog can't price for staleness no longer bails out (that
/// left every row saying `"live"`, i.e. green, on a surface whose whole job is
/// to say what a session is doing): fall back to `0` silent seconds, the same
/// fallback the IM side uses.
fn apply_progress_activity_status(
    paths: &ccteam_core::CcteamPaths,
    slug: &str,
    views: &mut [SessionView],
    live_turns: &std::collections::HashMap<String, ccteam_core::stall::LiveTurn>,
) {
    if views.is_empty() {
        return;
    }
    let silent_seconds = ccteam_core::collect_projects(paths)
        .ok()
        .and_then(|projects| {
            projects
                .into_iter()
                .find(|project| project.state.slug == slug)
                .map(|project| project.stall_silent_seconds)
        })
        .unwrap_or(0);
    let events =
        ccteam_core::progress::read_all_events(&paths.progress_jsonl(slug)).unwrap_or_default();
    let now = chrono::Utc::now();
    for view in views {
        let activity = ccteam_core::stall::classify_session_activity(
            &events,
            &view.sid,
            silent_seconds,
            live_turns.get(&view.sid).copied(),
            now,
        );
        view.last_activity_seconds = activity.last_activity_seconds;
        view.status = activity.status.activity.to_string();
    }
}

/// POST body for session creation — `role` (required), `vendor` (optional,
/// defaults `claude`), `permission_mode` (optional, `skip` default / `hitl`).
/// Accepts form or JSON via [`FormOrJson`].
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSessionForm {
    pub role: String,
    #[serde(default)]
    pub vendor: Option<String>,
    /// v0.8.7 W2 (DB.1) — `"skip"` (default) or `"hitl"`. Hitl drops the
    /// skip flag at spawn so non-allowlist tool calls prompt the IM user.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// v0.8.11 E2 — `"stream-json"` (default) or `"terminal"`. Selects the
    /// session channel; omitted → the daemon default (stream-json).
    #[serde(default)]
    pub protocol: Option<String>,
    /// Compatibility trap only: `host` was removed from the public schema in
    /// v0.9.2 and any supplied value is rejected with a migration-free error.
    #[serde(default)]
    #[serde(rename = "host")]
    #[schema(ignore)]
    removed_host: RemovedHostField,
    /// v0.8.24 A-U3 — explicit model id for the new session; overrides the
    /// role's `model:` frontmatter. Omitted/empty → vendor default. Wired
    /// vendor-natively: claude `--model`, codex `turn/start` override,
    /// grok `-m`, opencode `session/set_config_option`, kimi
    /// `session/set_model` (both best-effort).
    #[serde(default)]
    pub model: Option<String>,
    /// Explicit reasoning-effort token, forwarded to EVERY vendor verbatim.
    /// The value set is vendor-specific (claude `low|medium|high|xhigh|max`;
    /// codex `low|medium|high|xhigh`; grok `low|medium|high`; kimi
    /// `low|high|max`; opencode declares no effort axis today) and the VENDOR
    /// owns the verdict on its own values — ccteam never silently drops a
    /// caller's pick, because a dropped effort looks like a working spawn that
    /// quietly ran at the default. Omitted/empty → vendor default. Discover
    /// the live ladders with `GET /api/v1/models`.
    #[serde(default)]
    pub effort: Option<String>,
}

/// Map the create form's model/effort into a [`SpawnTuning`] — the ONE place
/// this entry point decides what reaches the vendor, and it decides nothing:
/// both facets ride through untouched for every vendor.
///
/// `_vendor` stays in the signature deliberately. It used to gate the effort
/// facet (grok/kimi were zeroed here on the theory that ccteam knew their
/// value sets better than they did), which turned an explicit caller choice
/// into a silent default — the exact failure this contract now forbids. The
/// param keeps the question "does the vendor change what we forward?" visible
/// with its permanent answer: no. Empty strings normalize to `None`
/// downstream (`SpawnTuning::normalized`); an unsupported token comes back as
/// the vendor's own spawn error, which is honest feedback the caller can act
/// on.
fn spawn_tuning_from_form(
    _vendor: AgentVendor,
    model: Option<String>,
    effort: Option<String>,
) -> ccteam_im::gateway::SpawnTuning {
    ccteam_im::gateway::SpawnTuning { model, effort }
}

/// `POST /api/v1/projects/{slug}/sessions`
///
/// Creates a new session via the spine. v0.8.8 F1 — ALWAYS mints a fresh sid
/// (no `(project, role)` dedup), so a project can run multiple same-role
/// sessions side by side. v0.8.8 F2-web — an empty `role` is a valid roleless
/// session (bare claude). 201 `{sid}` on success. 400 on a bad vendor token or
/// bad permission_mode. 422 when a NAMED role has no `.claude/agents/<role>.md`
/// (a caller mistake, R-M6; an empty role is NOT this case). 503 with no
/// gateway. 500 if the gateway create fails for a genuine internal reason
/// (project not registered / adapter spawn error).
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/sessions",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    request_body(content = CreateSessionForm, description = "Session to create (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 201, description = "Created; `{sid}`", body = serde_json::Value),
        (status = 400, description = "Bad vendor / bad permission_mode"),
        (status = 422, description = "Unknown NAMED role (no `.claude/agents/<role>.md`); empty role is allowed (roleless)"),
        (status = 503, description = "No live gateway (standalone web)"),
        (status = 500, description = "Gateway create failed (internal)"),
    ),
)]
pub(crate) async fn handle_create_session(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
    FormOrJson(form, mode): FormOrJson<CreateSessionForm>,
) -> Response {
    if form.removed_host.0 {
        return create_error(
            StatusCode::BAD_REQUEST,
            ccteam_im::remote_host::HOST_SPAWN_PARAM_REMOVED.to_string(),
            mode,
        );
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // v0.8.18 档1 — can't create a session in a project you can't see (the
    // session would inherit that project's ownership).
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    // v0.8.8 F2-web — an EMPTY role is now a valid "roleless" session (bare
    // claude that self-reads the project CLAUDE.md): pass the trimmed string
    // through verbatim. The gateway's `create_session_api` accepts "" (its
    // `ensure_role_exists` short-circuits on empty), so we no longer 400 here.
    let role = form.role.trim().to_string();
    let vendor_raw = form.vendor.as_deref().unwrap_or("claude");
    let vendor = match parse_vendor(vendor_raw) {
        Ok(v) => v,
        Err(msg) => return create_error(StatusCode::BAD_REQUEST, msg, mode),
    };
    // v0.8.7 W2 (DB.1) — optional `permission_mode` body field; default skip.
    let permission_mode = match PermissionMode::parse_opt(form.permission_mode.as_deref()) {
        Ok(m) => m,
        Err(msg) => return create_error(StatusCode::BAD_REQUEST, msg, mode),
    };
    // v0.8.11 E2 — optional `protocol` body field; default stream-json.
    // Grok always ACP (v0.8.23) — honest meta, ignore conflicting body.
    let protocol = match SessionProtocol::parse_opt(form.protocol.as_deref()) {
        Ok(p) => p,
        Err(msg) => return create_error(StatusCode::BAD_REQUEST, msg, mode),
    };
    let protocol = match vendor {
        AgentVendor::Grok | AgentVendor::Opencode | AgentVendor::Kimi | AgentVendor::Dsh => {
            SessionProtocol::Acp
        }
        AgentVendor::Pi => SessionProtocol::StreamJson,
        AgentVendor::Claude | AgentVendor::Codex => protocol,
    };

    // v0.8.24 A-U3 — explicit model/effort from the composer menu.
    let tuning = spawn_tuning_from_form(vendor, form.model.clone(), form.effort.clone());
    let created = {
        let mut guard = gw.lock().await;
        guard
            .create_session_api_tuned(
                slug.clone(),
                role.clone(),
                vendor,
                permission_mode,
                protocol,
                // v0.8.20 web↔IM convergence — own the session by the caller's
                // identity (`user:<tenant>` / `user:web-api`) so the tenant's own
                // IM bot sees it too.
                identity.web_chat_id(),
                tuning,
            )
            .await
    };
    match created {
        Ok(created) => (StatusCode::CREATED, Json(json!({"sid": created.sid}))).into_response(),
        // v0.8.7 review-fix (R-M6) — distinguish a caller mistake (the named
        // role has no `.claude/agents/<role>.md`) from a real internal failure
        // (adapter spawn / fs error). A bad role is a client error → 422
        // Unprocessable Entity with the clear hint, NOT a 500.
        Err(err) => {
            if let Some(missing) = err.downcast_ref::<ccteam_im::gateway::RoleNotFound>() {
                tracing::info!(%slug, %role, "create_session_api: unknown role -> 422");
                return create_error(StatusCode::UNPROCESSABLE_ENTITY, missing.to_string(), mode);
            }
            tracing::warn!(%slug, %role, %err, "create_session_api failed");
            create_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format_create_session_error(&err),
                mode,
            )
        }
    }
}

/// `GET /api/v1/sessions/{sid}`
///
/// History for one session. The gateway keeps no in-memory transcript, and
/// the gateway session id (`s{n}`) is *not* the `session_id` that ever
/// appears in the flat `<slug>.jsonl` progress — so we resolve the sid to
/// its `{role, project_dir}` via [`Gateway::session_resolve`] (404 if the
/// sid is unknown to the gateway) and read the ccteam-owned per-session
/// mirror `<project_dir>/.ccteam/chat/<role>/turns.jsonl`. Best-effort:
/// returns `{sid, events: []}` (200) when no turn has been mirrored yet (or
/// the file read fails). 503 with no gateway.
///
/// Lock discipline: `session_resolve` is sync (no `.await`) and only clones
/// scalar fields, so we run it under the gateway guard, then **drop the
/// guard** before the blocking `read_all_turns` fs read.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{sid}",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "History `{sid, events:[{turn_id, ts, role, user, assistant}]}`", body = serde_json::Value),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_history(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // Resolve sid → sid + project_dir under the lock (also our 404 gate),
    // then drop the guard before touching the filesystem. Live OR stopped: a
    // stopped session's transcript outlives the live map by design, and this
    // endpoint is what the team panel's 最近对话 reads for exactly those rows.
    let resolved = {
        let guard = gw.lock().await;
        guard.session_resolve_any(&sid)
    };
    let Some(resolved) = resolved else {
        return unknown_session(&sid);
    };
    let events = collect_session_turns(&resolved.project_dir, &resolved.sid);
    Json(json!({ "sid": sid, "events": events })).into_response()
}

/// PATCH body for `PATCH /api/v1/sessions/{sid}` — the only mutable field is
/// `title` (v0.8.22 P1 session-title system).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RenameSessionRequest {
    pub title: String,
}

/// `PATCH /api/v1/sessions/{sid}`
///
/// Rename a session's user-facing title — live **or stopped** (a history row
/// in the rail is renameable exactly like a live one; `meta.json` outlives the
/// live map). The title is rule-based truncated server-side
/// (whitespace-collapsed, capped ~40 chars — never an LLM call) and recorded
/// as `TitleSource::User`, which is STICKY: it is never later overwritten by
/// the first-message auto-title or a vendor `ai-title` (see
/// [`ccteam_harness::apply_title`]'s precedence).
///
/// 200 `{sid, title, previous, vendor, vendor_sync:{state, detail?}}` — the
/// cleaned title actually stored plus what the VENDOR's own title surface did
/// with it (`pushed` | `deferred` | `unsupported`), so the UI can tell the
/// user honestly whether the rename crossed the vendor boundary instead of
/// implying it always does. 400 for a blank title. 404 unknown/inaccessible
/// session (project-ACL'd via `gate_sid`, same as every other
/// `/sessions/{sid}/*` route). 503 no gateway.
#[utoipa::path(
    patch,
    path = "/api/v1/sessions/{sid}",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    request_body = RenameSessionRequest,
    responses(
        (status = 200, description = "Renamed `{sid, title, previous, vendor, vendor_sync}`", body = serde_json::Value),
        (status = 400, description = "Blank title"),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_patch_session(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
    Json(body): Json<RenameSessionRequest>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    if body.title.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "title must not be blank"})),
        )
            .into_response();
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let result = {
        let guard = gw.lock().await;
        // Held across the vendor push, like `interrupt_session`'s adapter call:
        // the push is one file append / one RPC, and serializing renames keeps
        // meta.json's read-modify-write atomic against a concurrent rename.
        guard.rename_session(&sid, &body.title).await
    };
    match result {
        Ok(renamed) => Json(rename_payload(&renamed)).into_response(),
        Err(err) => {
            tracing::warn!(%sid, %err, "rename_session failed");
            unknown_session(&sid)
        }
    }
}

/// Serialize a [`SessionRename`] for the PATCH response. Split out so the
/// wire shape — including the vendor-sync report the SPA renders — is
/// unit-testable without a live gateway.
fn rename_payload(renamed: &ccteam_im::gateway::SessionRename) -> serde_json::Value {
    let (state, detail) = match &renamed.vendor_sync {
        ccteam_harness::TitleSync::Pushed => ("pushed", None),
        ccteam_harness::TitleSync::Deferred(reason) => ("deferred", Some(reason.clone())),
        ccteam_harness::TitleSync::Unsupported => ("unsupported", None),
    };
    let mut sync = json!({ "state": state });
    if let Some(detail) = detail {
        sync["detail"] = json!(detail);
    }
    json!({
        "sid": renamed.sid,
        "title": renamed.title,
        "previous": renamed.previous,
        "vendor": renamed.vendor,
        "vendor_sync": sync,
    })
}

/// Reconstruct a session's history from its ccteam-owned transcript mirror
/// `<project_dir>/.ccteam/chat/<sid>/turns.jsonl` (the same file the W1
/// `session_collect` path reads). Each [`TurnRecord`] becomes one event
/// object; any read error folds to an empty list — a best-effort history
/// view (an absent file is the legitimate first-turn case, which
/// [`read_all_turns`] already returns as `Ok(empty)`). Split out from the
/// handler so the disk → events mapping is unit-testable without a live
/// gateway.
///
/// v0.8.8 F1 — keyed by `sid` (the transcript directory is
/// `.ccteam/chat/<sid>/`, not role): two same-role sessions therefore have
/// independent histories that do not bleed into each other.
fn collect_session_turns(project_dir: &std::path::Path, sid: &str) -> Vec<serde_json::Value> {
    match read_all_turns(project_dir, sid) {
        Ok(turns) => turns.iter().map(turn_to_event).collect(),
        Err(_) => Vec::new(),
    }
}

/// Map one mirrored [`TurnRecord`] to the history event shape the SPA
/// renders. Keeps the user prompt + assistant reply + turn id/ts so a
/// reopened per-session page can seed its transcript before the live SSE
/// takes over.
fn turn_to_event(turn: &TurnRecord) -> serde_json::Value {
    let mut event = json!({
        "turn_id": turn.turn_id,
        "ts": turn.ts,
        "role": turn.role,
        "user": turn.user,
        "assistant": turn.assistant,
    });
    if !turn.attachments.is_empty() {
        event["attachments"] =
            serde_json::to_value(&turn.attachments).unwrap_or_else(|_| json!([]));
    }
    event
}

/// One live session's [`ThreadStatus`], awaited AFTER the caller has dropped
/// the gateway lock (resolve `(adapter, thread)` via
/// [`session_status_handle`](ccteam_im::gateway::Gateway::session_status_handle)
/// under the lock first — `thread_status` does fs/transport I/O and must
/// never run under the gateway mutex). A `thread_status` error degrades to
/// the empty (all-null) status, never a 5xx. This is the single source of
/// truth for a session's live statusline: shared by `GET
/// /sessions/{sid}/status` and the team graph's per-live-node model join
/// ([`super::agents::handle_agents_graph`]), so both report the same model.
pub(crate) async fn resolved_thread_status(
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    thread: ThreadHandle,
    sid: &str,
) -> ThreadStatus {
    match adapter.thread_status(&thread).await {
        Ok(s) => s,
        Err(err) => {
            // A statusless answer is valid — degrade to empty, never a 5xx.
            tracing::warn!(%sid, %err, "thread_status failed; reporting empty status");
            ThreadStatus::default()
        }
    }
}

/// `GET /api/v1/sessions/{sid}/status`
///
/// The session's live statusline — model + context-window usage — for the
/// SPA's per-session top bar (the web peer of the IM `/sessions` model·ctx
/// suffix). Resolves the sid → `(adapter, thread)` under the gateway lock
/// (also the 404 gate), **drops the lock**, then awaits
/// [`HarnessAdapter::thread_status`](ccteam_harness::HarnessAdapter::thread_status)
/// via [`resolved_thread_status`] — fs/transport I/O that must never run
/// under the gateway mutex (same lock-drop discipline as the history
/// endpoint). 200 `{sid, model, context, status_line}`; any field is `null`
/// until there is something to report (a fresh session before its first
/// turn). 404 unknown sid. 503 no gateway. A `thread_status` error degrades
/// to the empty (all-null) status, never a 5xx.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{sid}/status",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "Live statusline `{sid, model, effort, context:{used_tokens, window_tokens, pct}, status_line}` (fields null until the first turn; `effort` null on models/builds with no effort axis)", body = serde_json::Value),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_status(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // Resolve (adapter, thread) under the lock, then DROP the guard before the
    // async thread_status I/O.
    let resolved = {
        let guard = gw.lock().await;
        guard.session_status_handle(&sid)
    };
    let Some((adapter, thread)) = resolved else {
        return unknown_session(&sid);
    };
    let status = resolved_thread_status(adapter, thread, &sid).await;
    let context = status.context.map(|c| {
        json!({
            // `used_tokens` / `pct` are null when no channel reports occupancy
            // — the SPA renders a dash; a zero would claim an empty context.
            "used_tokens": c.used_tokens,
            "window_tokens": c.window_tokens,
            "pct": c.pct(),
            "source": c.source,
        })
    });
    Json(json!({
        "sid": sid,
        "model": status.model,
        "effort": status.effort,
        "goal": status.goal,
        "context": context,
        "status_line": status.status_suffix(),
    }))
    .into_response()
}

/// POST body for a turn submission — `text` (required unless `attachments`
/// is non-empty). Form or JSON; `attachments` rides the JSON shape only.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TurnForm {
    #[serde(default)]
    pub text: String,
    /// Composer attachments to weave into the turn text (web parity with the
    /// IM inbound attachment grammar). Empty for a plain text turn.
    #[serde(default)]
    pub attachments: Vec<TurnAttachment>,
}

/// One composer attachment named in a turn POST.
///
/// - `kind: "image" | "file"` → `path` names a file previously stored by
///   `POST /projects/{slug}/uploads` (validated to live under THAT session's
///   project `.ccteam/uploads/` — the API accepts no arbitrary paths).
/// - `kind: "skill"` → `name` is a skill id. Omitted / `scope: "project"`
///   resolves the project-local skill; `scope: "global"` resolves a possibly
///   nested id in the admin's user-level library. The turn gains a
///   self-describing "read this skill file and follow it" line, which works
///   identically for every vendor (a skill is prompt-layer markdown any agent
///   can `Read`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct TurnAttachment {
    /// `"image"` / `"file"` / `"skill"`.
    pub kind: String,
    /// Stored upload path (image/file kinds).
    #[serde(default)]
    pub path: Option<String>,
    /// Display name; for `kind: "skill"` the skill id (required).
    #[serde(default)]
    pub name: Option<String>,
    /// Skill source: omitted / `"project"` keeps the project-local behavior;
    /// `"global"` resolves a nested id in the user-level global library.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Render the skill-attachment line for the session's vendor — the ONE seam
/// where vendor-native skill invocation syntaxes are unified. The wire/API
/// stays vendor-neutral (`{kind:"skill", name}`); this rendering picks the
/// richest path each vendor understands:
/// - **claude** — the native loader path is the Skill tool (`/name`); a
///   mid-text `/name` does NOT auto-trigger it, so the line names the tool
///   explicitly (Claude then invokes it natively) with the read-path fallback.
/// - **codex** — `$name`: Codex's plaintext `TOOL_MENTION_SIGIL` (`'$'`)
///   resolves skill mentions natively anywhere in a user message.
/// - **grok / opencode / kimi** (ACP; no native skill loader) — the neutral
///   read-and-follow line (a skill is prompt-layer markdown any agent can
///   `Read`).
fn skill_attachment_line(vendor: &str, id: &str, md: &std::path::Path) -> String {
    match vendor {
        "claude" => format!(
            "[skill \"{id}\" attached — invoke /{id} with your Skill tool \
             (fallback: read {}) and follow it for this request]",
            md.display()
        ),
        "codex" => format!(
            "[skill \"{id}\" attached — use ${id} for this request (definition: {})]",
            md.display()
        ),
        _ => format!(
            "[skill \"{id}\" attached — read {} and follow it for this request]",
            md.display()
        ),
    }
}

/// Weave validated attachments into the outgoing turn text — the web peer of
/// the IM `wrap_inbound` extra-attachment lines, emitting the SAME
/// `[attachment image_path|file_path="…"]` grammar (shared helper, so the
/// format every vendor session is taught to `Read` never drifts) plus the
/// per-vendor skill line ([`skill_attachment_line`]). Pure aside from
/// existence checks; returns a readable error string for the 400 path.
fn build_turn_text_with_attachments(
    text: &str,
    attachments: &[TurnAttachment],
    project_dir: &std::path::Path,
    skills_dir: &std::path::Path,
    vendor: &str,
) -> Result<String, String> {
    use ccteam_im::transport::{attachment_line, AttachmentKind, ChannelAttachment};
    let mut lines: Vec<String> = Vec::new();
    for att in attachments {
        let scope = match att.scope.as_deref() {
            None | Some("project") => "project",
            Some("global") => "global",
            Some(other) => return Err(format!("invalid attachment scope: {other}")),
        };
        match att.kind.as_str() {
            "image" | "file" => {
                if scope == "global" {
                    return Err("attachment scope `global` is only valid for skills".into());
                }
                let Some(path) = att.path.as_deref().filter(|p| !p.trim().is_empty()) else {
                    return Err(format!("attachment kind `{}` requires `path`", att.kind));
                };
                let canon = super::uploads::canonical_project_upload(
                    project_dir,
                    std::path::Path::new(path),
                )
                .map_err(|err| match err {
                    super::uploads::UploadPathError::NotFound => {
                        format!("attachment not found: {path}")
                    }
                    super::uploads::UploadPathError::OutsideUploads => {
                        format!("attachment path is outside this project's uploads dir: {path}")
                    }
                })?;
                let kind = if att.kind == "image" {
                    AttachmentKind::Image
                } else {
                    AttachmentKind::File
                };
                lines.push(attachment_line(&ChannelAttachment {
                    kind,
                    file_name: att.name.clone().unwrap_or_default(),
                    local_path: canon.to_string_lossy().into_owned(),
                    mime: None,
                    size: None,
                }));
            }
            "skill" => {
                let Some(id) = att.name.as_deref().filter(|n| !n.trim().is_empty()) else {
                    return Err("attachment kind `skill` requires `name` (the skill id)".into());
                };
                if scope == "global" {
                    ccteam_core::validate_skill_library_id(id)
                        .map_err(|_| format!("invalid skill id: {id}"))?;
                    let md = skills_dir.join(id).join("SKILL.md");
                    if !md.is_file() {
                        return Err(format!("skill not in library: {id}"));
                    }
                    lines.push(skill_attachment_line(vendor, id, &md));
                    continue;
                }
                // Same charset rule as the skill write path — subsumes any
                // path traversal (`/`, `\`, `..` all rejected).
                if !id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                {
                    return Err(format!("invalid skill id: {id}"));
                }
                let md = ccteam_core::skill_md_path(project_dir, id);
                if !md.is_file() {
                    return Err(format!("skill not installed in this project: {id}"));
                }
                lines.push(skill_attachment_line(vendor, id, &md));
            }
            other => return Err(format!("unknown attachment kind: {other}")),
        }
    }
    let trimmed = text.trim();
    Ok(if trimmed.is_empty() {
        lines.join("\n")
    } else {
        format!("{trimmed}\n\n{}", lines.join("\n"))
    })
}

/// `POST /api/v1/sessions/{sid}/turn`
///
/// Submits a user-text turn to the session via the spine. The turn
/// executes asynchronously — the reply + progress arrive over
/// `GET /api/v1/sessions/{sid}/events` — so success is 202
/// `{accepted:true}`. 404 only for a genuinely unknown sid (no live session
/// AND no `meta.json`); a session that was evicted for capacity, dropped by a
/// daemon restart whose rebuild failed, or explicitly stopped is COLD-RESUMED
/// on demand (resume-by-sid — the same contract IM `/use` + the sidebar resume
/// button honour) instead of 404-ing. 503 with no gateway, 400 on empty text,
/// 502 when a resumable session fails to re-spawn.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/turn",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    request_body(content = TurnForm, description = "Turn text (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 202, description = "Accepted; reply/progress arrive over `/events`. `{accepted:true}`", body = serde_json::Value),
        (status = 400, description = "Empty text"),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_turn(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
    FormOrJson(form, mode): FormOrJson<TurnForm>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let has_global_attachment = form
        .attachments
        .iter()
        .any(|att| att.scope.as_deref() == Some("global"));
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    // Attachments make a bare send meaningful ("look at this file"), so text
    // is only required when nothing is attached.
    if form.text.trim().is_empty() && form.attachments.is_empty() {
        return create_error(
            StatusCode::BAD_REQUEST,
            "text must not be empty".into(),
            mode,
        );
    }
    let result = {
        let mut guard = gw.lock().await;
        // Resume-by-sid (红线「会话 = resume-by-session-id」): a session evicted
        // for capacity, dropped by a daemon restart whose rebuild failed, or
        // explicitly stopped is not in the live map — but its `meta.json`
        // persists on disk. Cold-resume it on demand rather than 404, so a chat
        // whose session "disappeared" out from under it accepts the next turn
        // (the same path the sidebar resume button + IM `/use` take). A
        // genuinely unknown sid (no live session, no meta) still 404s below.
        if !guard.is_session_live(&sid) {
            let Some(slug) = guard.project_slug_for_sid(&sid) else {
                return unknown_session(&sid);
            };
            // `gate_sid` already project-gated the caller; bind the resume to
            // the resolved owning project so `resume_stopped_session`'s own ACL
            // guard is satisfied (`exp == slug` holds).
            let caller_identity = identity.owner_tag();
            if let Err(err) = guard
                .resume_stopped_session(&sid, &caller_identity, Some(&slug))
                .await
            {
                tracing::warn!(%sid, %err, "auto-resume on web turn failed");
                return create_error(
                    StatusCode::BAD_GATEWAY,
                    format!("session {sid} could not be resumed: {err}"),
                    mode,
                );
            }
        }
        let Some(view) = guard
            .session_views()
            .into_iter()
            .find(|session| session.sid == sid)
        else {
            return unknown_session(&sid);
        };
        // Weave attachments into the turn text (the web peer of the IM
        // inbound attachment grammar). Project uploads and the global library
        // live on the daemon host, so a remote-host session can't see them —
        // readable error, no silent rot.
        let text = if form.attachments.is_empty() {
            form.text
        } else {
            let Some(resolved) = guard.session_resolve(&sid) else {
                return unknown_session(&sid);
            };
            if has_global_attachment {
                let catalog_host = super::uploads::project_host(&app, &resolved.project);
                let execution_host = if catalog_host != "local" {
                    Some(catalog_host.as_str())
                } else if !view.host.is_empty() && view.host != "local" {
                    Some(view.host.as_str())
                } else {
                    None
                };
                if let Some(host) = execution_host {
                    return create_error(
                        StatusCode::BAD_REQUEST,
                        format!(
                            "project `{}` runs on remote host `{host}` — maintain \
                             ~/.ccteam/skills on the execution host before attaching global skills",
                            resolved.project
                        ),
                        mode,
                    );
                }
            }
            if !view.host.is_empty() && view.host != "local" {
                return create_error(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "session {sid} runs on remote host `{}` — attachments are not yet \
                         supported for remote sessions",
                        view.host
                    ),
                    mode,
                );
            }
            match build_turn_text_with_attachments(
                &form.text,
                &form.attachments,
                &resolved.project_dir,
                &app.paths.skills_dir(),
                &view.vendor,
            ) {
                Ok(text) => text,
                Err(msg) => return create_error(StatusCode::BAD_REQUEST, msg, mode),
            }
        };
        // Web interactive path: run the gateway control-command face (/status,
        // /sessions, …) first — parity with IM — then fall back to a turn. The
        // fleet renders are admin-only (see `submit_web_sid`).
        guard.submit_web_sid(&sid, text, identity.is_admin).await
    };
    match result {
        Ok(_turn_id) => (StatusCode::ACCEPTED, Json(json!({"accepted": true}))).into_response(),
        Err(err) => {
            tracing::warn!(%sid, %err, "submit_to_sid failed");
            create_error(StatusCode::BAD_GATEWAY, format_submit_error(&err), mode)
        }
    }
}

/// JSON body for a one-shot delayed user turn. `send_at` and `when` are
/// aliases for the same strict daemon-local parser; exactly one is required.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateScheduledRequest {
    pub text: String,
    #[serde(default)]
    pub send_at: Option<String>,
    #[serde(default)]
    pub when: Option<String>,
}

/// `GET /api/v1/sessions/{sid}/scheduled`.
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{sid}/scheduled",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "Pending and retained failed scheduled messages", body = serde_json::Value),
        (status = 404, description = "Unknown or invisible session"),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_list_scheduled(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let result = gw.lock().await.scheduled_items_for_sid(&sid);
    match result {
        Ok(items) => Json(items).into_response(),
        Err(_) => unknown_session(&sid),
    }
}

/// `POST /api/v1/sessions/{sid}/scheduled` — create a one-shot delayed normal
/// user turn. Time parsing is identical to IM `/inbox` and uses daemon local
/// time while persistence/wire values are UTC RFC3339.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/scheduled",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    request_body = CreateScheduledRequest,
    responses(
        (status = 201, description = "Scheduled item", body = serde_json::Value),
        (status = 400, description = "Empty body or invalid/past/out-of-range time"),
        (status = 404, description = "Unknown or invisible session"),
        (status = 409, description = "Pending-message limit reached"),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_create_scheduled(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
    Json(request): Json<CreateScheduledRequest>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    if request.text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "scheduled message text cannot be empty"})),
        )
            .into_response();
    }
    let when = match (request.send_at.as_deref(), request.when.as_deref()) {
        (Some(value), None) | (None, Some(value)) => value,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "provide exactly one of `send_at` or `when`"})),
            )
                .into_response()
        }
    };
    let send_at = match ccteam_im::scheduled::parse_send_time(when) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    };
    let visible_projects = if identity.is_admin {
        None
    } else {
        Some(
            ccteam_core::collect_projects(&app.paths)
                .unwrap_or_default()
                .into_iter()
                .filter(|project| identity.can_see_owner(project.state.owner.as_deref()))
                .map(|project| project.state.slug)
                .collect::<std::collections::HashSet<_>>(),
        )
    };
    let result = gw.lock().await.create_scheduled_message(
        &sid,
        request.text,
        send_at,
        identity.owner_tag(),
        visible_projects.as_ref(),
    );
    match result {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(err) => {
            let message = err.to_string();
            let status = if message.contains("limit") || message.contains("already has") {
                StatusCode::CONFLICT
            } else if message.contains("unknown session") {
                StatusCode::NOT_FOUND
            } else if message.contains("cannot be empty")
                || message.contains("future")
                || message.contains("within 7 days")
            {
                StatusCode::BAD_REQUEST
            } else {
                tracing::warn!(%sid, error = %err, "create scheduled message failed");
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(json!({"error": message}))).into_response()
        }
    }
}

/// `DELETE /api/v1/sessions/{sid}/scheduled/{id}` — cancel a pending row or
/// dismiss a retained failure.
#[utoipa::path(
    delete,
    path = "/api/v1/sessions/{sid}/scheduled/{id}",
    tag = "sessions",
    params(
        ("sid" = String, Path, description = "Gateway session id (`s{n}`)"),
        ("id" = String, Path, description = "Scheduled id (`d{n}`)"),
    ),
    responses(
        (status = 200, description = "Cancelled/dismissed", body = serde_json::Value),
        (status = 404, description = "Unknown item, session, or invisible session"),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_cancel_scheduled(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path((sid, id)): Path<(String, String)>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    match gw.lock().await.cancel_scheduled_message(&sid, &id) {
        Ok(_) => Json(json!({"cancelled": true, "id": id})).into_response(),
        Err(err) if err.to_string().contains("unknown scheduled") => unknown_session(&id),
        Err(err) => {
            tracing::warn!(%sid, %id, error = %err, "cancel scheduled message failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// POST body for resolving a pending choice (the HITL approve/deny path) —
/// `token` (the pending-resolution token carried on the SSE choice frame) +
/// `selection` (the chosen option `id`, e.g. `"allow"` / `"deny"`). Form or
/// JSON. v0.8.7 review-fix (R-H1).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveForm {
    /// Pending-resolution token (from the SSE choice frame's `token`).
    pub token: String,
    /// Chosen option id (the SSE choice frame's `options[].id`).
    pub selection: String,
}

/// `POST /api/v1/sessions/{sid}/resolve`
///
/// v0.8.7 review-fix (R-H1) — resolve a token-keyed pending choice (the web
/// HITL approve/deny click) through the SAME gateway machinery an IM option
/// click uses ([`Gateway::resolve_web_selection`] → `take_by_token` →
/// `apply_pending`). This is **not** a turn: it delivers the decision to the
/// blocked `permission/ask` hook so `[Approve]` makes the tool actually run
/// and `[Deny]` denies immediately (no 600s timeout). 200 `{resolved:true}`
/// on success. 400 on empty token/selection. 404 (clean 4xx, never a turn)
/// when the token is unknown/expired or the selection is not a valid option
/// for that prompt. 503 with no gateway.
///
/// The `{sid}` is the addressing namespace for parity with the other session
/// endpoints; resolution itself is token-global (a pending is keyed by its
/// token, unique per outstanding prompt), so the token is the authority.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/resolve",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    request_body(content = ResolveForm, description = "Pending resolution (JSON or x-www-form-urlencoded)"),
    responses(
        (status = 200, description = "Resolved; the decision was delivered to the waiting hook. `{resolved:true}`", body = serde_json::Value),
        (status = 400, description = "Empty token or selection"),
        (status = 404, description = "Unknown/expired token or invalid selection (NOT submitted as a turn)"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_resolve(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
    FormOrJson(form, mode): FormOrJson<ResolveForm>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let token = form.token.trim();
    let selection = form.selection.trim();
    if token.is_empty() || selection.is_empty() {
        return create_error(
            StatusCode::BAD_REQUEST,
            "token and selection must not be empty".into(),
            mode,
        );
    }
    let result = {
        // resolve_web_selection takes &self (the pending registry is behind its
        // own inner lock), so a shared guard suffices.
        let guard = gw.lock().await;
        guard.resolve_web_selection(token, selection).await
    };
    match result {
        Ok(()) => Json(json!({"resolved": true})).into_response(),
        Err(err) => {
            // Unknown/expired token or a bad option id — a clean 4xx, never a
            // turn (the whole point of R-H1). 404 mirrors the unknown-session
            // shape the other session endpoints return.
            tracing::warn!(%sid, %err, "resolve_web_selection failed");
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("resolve failed: {err}")})),
            )
                .into_response()
        }
    }
}

/// Query-string fallback for `Last-Event-ID` (v0.8.22 P1, review §3.1-3).
/// `EventSource` cannot set arbitrary request headers, and
/// [`handle_session_events`] deliberately opens a BRAND NEW `EventSource` on
/// every reconnect (see `useSessionEvents.ts`'s `connect()` — it explicitly
/// closes + reopens rather than relying on the browser's own auto-retry, so
/// the browser's native "resend `Last-Event-ID` on its own reconnect"
/// behavior never applies here). The SPA therefore threads the watermark
/// through this query param; the standard header is ALSO honored (useful
/// for curl / future non-browser SSE clients, and it wins if both are
/// somehow present).
#[derive(Debug, Deserialize)]
pub(crate) struct SessionEventsQuery {
    #[serde(default)]
    pub(crate) last_event_id: Option<String>,
}

/// Resolve the reconnect watermark from the standard `Last-Event-ID` header
/// or the `?last_event_id=` query fallback. `None` for a fresh connect (no
/// watermark at all) or an unparseable value — defensive: a bad query
/// string degrades to "no replay", never a 4xx.
pub(crate) fn parse_last_event_id(headers: &HeaderMap, query: &SessionEventsQuery) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .or(query.last_event_id.as_deref())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Rebuild the exact [`GatewayEvent`] shape the HITL approval flow
/// (`ccteam_im::hitl::ask_permission` / `ccteam-cli`'s
/// `execute_permission_ask`) originally broadcast, from the
/// [`ccteam_im::pending::PendingInteractions`] entry alone — used to
/// re-seed a still-outstanding approval on SSE (re)connect (v0.8.22 P1,
/// review §3.1-3). The `id` mirrors the original (`"permission-{token}"`)
/// so [`session_event_payload`] renders the identical `token`/`options`
/// shape the live broadcast used.
fn synthetic_approval_event(sid: &str, prompt: &ChoicePrompt) -> GatewayEvent {
    use ccteam_im::gateway::GatewayEventKind;
    GatewayEvent {
        id: format!("permission-{}", prompt.token),
        channel: String::new(),
        chat_id: String::new(),
        thread_ts: None,
        content: prompt.title.clone(),
        kind: GatewayEventKind::Answer,
        attachments: Vec::new(),
        options: prompt
            .options
            .iter()
            .enumerate()
            .map(|(i, opt)| MessageOption {
                data: format!("{}:{i}", prompt.token),
                label: opt.label.clone(),
                id: opt.id.clone(),
            })
            .collect(),
        sid: Some(sid.to_string()),
        // Not resolvable from a bare `(sid, prompt)` pair; the reseed just
        // won't ACL-filter into the team view's global SSE for a tenant
        // (admin-visible only). Known scope reduction (W4).
        slug: None,
    }
}

/// Build the catch-up batch a (re)connecting SSE client gets BEFORE the live
/// tail (v0.8.22 P1, review §3.1-3): the ring's best-effort backlog replay
/// (only when the caller sent a `last_id` watermark) PLUS an unconditional
/// pending-approval reseed. The reseed is independent of ring contents/
/// `last_id` on purpose — a brand-new tab with no watermark at all must
/// still see a still-outstanding approval, not just a reconnecting one whose
/// gap happens to be covered by the ring. Skips the reseed when the ring
/// replay already carries that same approval's token (avoids an
/// always-visible double-render on a plain reconnect).
///
/// Returns `(seq, event)` pairs rather than rendered [`Event`]s — split out
/// so this is unit-testable with plain assertions (mirrors why
/// [`session_event_payload`] is split from [`session_event`]: asserting on
/// an axum `Event`'s rendered body is awkward). [`build_catchup_events`] is
/// the thin wrapper the handler actually calls.
async fn build_catchup_entries(
    app: &AppState,
    sid: &str,
    last_id: Option<u64>,
) -> Vec<(u64, GatewayEvent)> {
    let Some(gw) = app.gateway.as_ref() else {
        return Vec::new();
    };
    let ring_entries: Vec<crate::ring::RingEntry> = match last_id {
        Some(since) => app
            .session_ring
            .replay_since(sid, since)
            .into_iter()
            .filter(|e| !is_im_only_event(&e.event))
            .collect(),
        None => Vec::new(),
    };
    let already_seeded_token = ring_entries
        .iter()
        .rev()
        .find_map(|e| approval_token(&e.event));

    let mut entries: Vec<(u64, GatewayEvent)> =
        ring_entries.into_iter().map(|e| (e.seq, e.event)).collect();

    let pending_prompt = {
        let pending = gw.lock().await.pending_handle();
        let guard = pending.lock().await;
        guard.pending_for_sid(sid).map(|p| p.prompt.clone())
    };
    if let Some(prompt) = pending_prompt {
        if already_seeded_token.as_deref() != Some(prompt.token.as_str()) {
            let synthetic = synthetic_approval_event(sid, &prompt);
            let seq = app.session_ring.record(sid, synthetic.clone());
            entries.push((seq, synthetic));
        }
    }
    entries
}

/// Render [`build_catchup_entries`]'s output as SSE [`Event`]s — the form
/// [`handle_session_events`] actually streams.
async fn build_catchup_events(app: &AppState, sid: &str, last_id: Option<u64>) -> Vec<Event> {
    build_catchup_entries(app, sid, last_id)
        .await
        .into_iter()
        .map(|(seq, ev)| session_event(&ev, seq))
        .collect()
}

/// `GET /api/v1/sessions/{sid}/events`
///
/// SSE stream for one session. Subscribes to the SSE replay ring's live tap
/// ([`crate::ring::SessionEventRing::subscribe`], fed by the ONE persistent
/// feeder off the gateway's event broadcast — see `crate::ring`'s module
/// doc) and keeps only entries whose `sid` matches this session id — the
/// cross-stage filter key. 15s keep-alive; a lagging consumer (broadcast
/// `Lagged`) gets a synthetic `reconnect_hint` then the stream closes for the
/// SPA's `EventSource` to auto-reconnect.
///
/// v0.8.22 P1 (review §3.1-3) — before the live tail, a (re)connecting
/// client gets a catch-up batch ([`build_catchup_events`]): the ring's
/// best-effort backlog replay when `Last-Event-ID` (header or
/// `?last_event_id=` query fallback) names a watermark, PLUS an
/// unconditional pending-approval reseed so an outstanding HITL approval
/// renders again on ANY (re)connect — including a brand-new tab that never
/// had a watermark. Every emitted frame now carries an SSE `id:` (the
/// ring's per-sid monotonic seq) so the client can dedupe a replayed/
/// reseeded frame against one it already rendered.
///
/// No-gateway: a 503 here would close the `EventSource` and the SPA would
/// retry-loop, so we instead emit a single `gateway_unavailable` SSE frame
/// and keep an (empty) keep-alive stream open — the SPA shows "no live
/// gateway" without hammering reconnects.
///
/// Unknown sid (gateway present but no session by that id) is *not* a 404
/// here: the stream simply never matches, so only keep-alives flow. A
/// session created concurrently then starts matching live — closing the
/// stream on a momentarily-unknown sid would race that.
///
/// OpenAPI note: this is a **Server-Sent Events** stream, which OpenAPI
/// cannot fully model as a JSON response body. The response is declared
/// as `text/event-stream`; each `event: progress` frame's `data` is a
/// JSON line `{id, sid, kind:"answer"|"progress", content, done?, options?}`,
/// and the SSE frame itself carries an `id:` (the replay-ring seq).
#[utoipa::path(
    get,
    path = "/api/v1/sessions/{sid}/events",
    tag = "sessions",
    params(
        ("sid" = String, Path, description = "Gateway session id (`s{n}`)"),
        ("last_event_id" = Option<String>, Query, description = "Reconnect watermark (query fallback for the `Last-Event-ID` header — EventSource can't set custom headers)"),
    ),
    responses(
        (status = 200, description = "SSE stream (text/event-stream). Frames: `event: progress` with `data` = `{id, sid, kind, content, done?, options?}`, each carrying an SSE `id:` seq. Never 503 — a no-gateway path emits one `gateway_unavailable` frame then keep-alives.", content_type = "text/event-stream"),
    ),
)]
pub(crate) async fn handle_session_events(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
    Query(query): Query<SessionEventsQuery>,
    headers: HeaderMap,
) -> Response {
    // v0.8.18 档1 — gate by the session's project (per-user isolation); a
    // tenant can't subscribe to a session in a project it can't see.
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let last_id = parse_last_event_id(&headers, &query);
    // Subscribe to the ring's live tap BEFORE computing the catchup batch:
    // any event recorded in the small window between the two lands in the
    // LIVE tail (a possible, harmless at-most-once duplicate the client's
    // seq-based dedup swallows) rather than a silent gap. `None` gateway
    // keeps the standalone no-gateway contract.
    let rx = app.gateway.as_ref().map(|_| app.session_ring.subscribe());
    let target_sid = sid.clone();
    // Unify both arms into one stream type (`Either`) so the function has a
    // single `impl Stream` return. With a gateway: the catchup batch chained
    // into the filtered live tap. Without: a one-shot `gateway_unavailable`
    // notice (then only keep-alives).
    let stream = match rx {
        Some(rx) => {
            let catchup = build_catchup_events(&app, &sid, last_id).await;
            futures::stream::iter(catchup.into_iter().map(Ok::<Event, Infallible>))
                .chain(BroadcastStream::new(rx).filter_map(move |item| {
                    let target_sid = target_sid.clone();
                    async move {
                        match item {
                            Ok(entry) if event_matches_sid(&entry.event, &target_sid) => {
                                Some(Ok(session_event(&entry.event, entry.seq)))
                            }
                            Ok(_) => None,
                            Err(BroadcastStreamRecvError::Lagged(n)) => {
                                Some(Ok(reconnect_hint(&format!("lagged {n} events"))))
                            }
                        }
                    }
                }))
                .left_stream()
        }
        None => futures::stream::iter(vec![Ok::<Event, Infallible>(gateway_unavailable_event())])
            .right_stream(),
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::default().interval(KEEPALIVE_INTERVAL))
        .into_response()
}

/// `POST /api/v1/sessions/{sid}/stop`
///
/// Stops (deregisters) the session via the spine. 200 `{stopped:true}`.
/// 404 for an unknown sid. 503 with no gateway. Never file-purges — the
/// spine's `stop_session` is deregister-only.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/stop",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "Stopped (deregistered). `{stopped:true}`", body = serde_json::Value),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_stop(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let result = {
        let mut guard = gw.lock().await;
        guard.stop_session(&sid).await
    };
    match result {
        Ok(()) => Json(json!({"stopped": true})).into_response(),
        Err(err) => {
            tracing::warn!(%sid, %err, "stop_session failed");
            unknown_session(&sid)
        }
    }
}

/// `POST /api/v1/sessions/{sid}/interrupt`
///
/// Interrupts the session's CURRENTLY-RUNNING turn WITHOUT destroying it — the
/// non-destructive twin of `/stop`. The session stays live + idle (its context
/// survives), so the client can immediately `/model` switch or send a
/// follow-up. The spine's [`Gateway::interrupt_session`] reaches the adapter
/// OUT-OF-BAND (stream-json `interrupt` control_request / TUI ESC / codex
/// `turn/interrupt`), so the interrupt is NOT queued behind the running turn.
/// 200 `{interrupted:true}`. 404 for an unknown sid. 503 with no gateway. Same
/// auth + project ACL (`gate_sid`) as the stop route.
#[utoipa::path(
    post,
    path = "/api/v1/sessions/{sid}/interrupt",
    tag = "sessions",
    params(("sid" = String, Path, description = "Gateway session id (`s{n}`)")),
    responses(
        (status = 200, description = "Interrupted the running turn (session kept). `{interrupted:true}`", body = serde_json::Value),
        (status = 404, description = "Unknown session"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_session_interrupt(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(sid): Path<String>,
) -> Response {
    if let Some(deny) = gate_sid(&app, &identity, &sid).await {
        return deny;
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let result = {
        let mut guard = gw.lock().await;
        guard.interrupt_session(&sid).await
    };
    match result {
        Ok(()) => Json(json!({"interrupted": true})).into_response(),
        Err(err) => {
            tracing::warn!(%sid, %err, "interrupt_session failed");
            unknown_session(&sid)
        }
    }
}

// ── v0.8.21 history / resume / external-import ───────────────────────────────

/// `GET /api/v1/projects/{slug}/sessions/history`
///
/// Lists *stopped* ccteam sessions for this project — sessions with a
/// `meta.json` on disk that are NOT currently in the gateway live map.
/// Caller-lazy: only invoked when the user expands "more history" in the UI.
/// 200 `[HistorySessionView]` sorted by `last_active` desc.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/sessions/history",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Stopped sessions list", body = serde_json::Value),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_session_history_list(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let metas = {
        let guard = gw.lock().await;
        guard.list_history_sessions(&slug)
    };
    let views: Vec<serde_json::Value> = metas
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "sid": m.sid,
                "slug": m.slug,
                "vendor": format!("{:?}", m.vendor).to_lowercase(),
                "protocol": format!("{:?}", m.protocol).to_lowercase().replace("json", "-json"),
                "role": m.role,
                "permission_mode": format!("{:?}", m.permission_mode).to_lowercase(),
                "owner": m.owner,
                "vendor_uuid": m.vendor_uuid,
                "created_at": m.created_at,
                "last_active": m.last_active,
                "origin": format!("{:?}", m.origin).to_lowercase(),
                "transcript_present": !m.vendor_uuid.is_empty(),
                // v0.8.22 P1 — session-title system: surface the title (if
                // any) + the casually-added turn_count/cost_usd bookkeeping.
                "title": m.title,
                "turn_count": m.turn_count,
                "cost_usd": m.cost_usd,
            })
        })
        .collect();
    Json(serde_json::json!(views)).into_response()
}

/// `POST /api/v1/projects/{slug}/sessions/{sid}/resume`
///
/// Re-activate a stopped ccteam session: read its `meta.json`, re-insert
/// into the gateway live map, spawn the child. 200 `{sid}` on success.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/sessions/{sid}/resume",
    tag = "sessions",
    params(
        ("slug" = String, Path, description = "Project slug"),
        ("sid"  = String, Path, description = "Session id"),
    ),
    responses(
        (status = 200, description = "Resumed `{sid}`", body = serde_json::Value),
        (status = 404, description = "Session or project not found"),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_session_resume(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path((slug, sid)): Path<(String, String)>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let caller_identity = identity.owner_tag();
    let result = {
        let mut guard = gw.lock().await;
        // Bind the resume to the authorised project slug: the sid must belong to
        // `slug` (gated by `can_see_project` above). Without this a tenant could
        // resume another project's session by passing its sid under a slug they
        // own.
        guard
            .resume_stopped_session(&sid, &caller_identity, Some(&slug))
            .await
    };
    match result {
        Ok(resumed_sid) => Json(json!({"sid": resumed_sid})).into_response(),
        Err(err) => {
            tracing::warn!(%sid, %err, "resume_stopped_session failed");
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/projects/{slug}/external-sessions`
///
/// Discover Claude sessions in `~/.claude/projects/` whose recorded `cwd`
/// matches this project, excluding already-adopted ones.
/// 200 `[{vendor_uuid, title, last_active, adoptable}]`.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/external-sessions",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "External sessions list", body = serde_json::Value),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_external_sessions(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let sessions = {
        let guard = gw.lock().await;
        guard.list_external_claude_sessions(&slug)
    };
    let views: Vec<serde_json::Value> = sessions
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "vendor": "claude",
                "vendor_uuid": s.vendor_uuid,
                "title": s.title,
                "last_active": s.last_active,
                "cwd": s.cwd,
                "adoptable": true,
            })
        })
        .collect();
    Json(serde_json::json!(views)).into_response()
}

/// Request body for `POST /api/v1/projects/{slug}/sessions/import`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct ImportSessionRequest {
    pub vendor: String,
    pub vendor_uuid: String,
}

/// `POST /api/v1/projects/{slug}/sessions/import`
///
/// Adopt an external Claude session: mint a new ccteam `sid`, write
/// `meta.json`, resume via fidelity ladder. 201 `{sid}`.
/// v1: Claude only (`vendor == "claude"`); Codex deferred to v0.9.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{slug}/sessions/import",
    tag = "sessions",
    params(("slug" = String, Path, description = "Project slug")),
    request_body = ImportSessionRequest,
    responses(
        (status = 201, description = "Imported session `{sid}`", body = serde_json::Value),
        (status = 400, description = "Unsupported vendor or missing uuid"),
        (status = 503, description = "No live gateway"),
    ),
)]
pub(crate) async fn handle_import_session(
    State(app): State<AppState>,
    Extension(identity): Extension<crate::auth::Identity>,
    Path(slug): Path<String>,
    Json(body): Json<ImportSessionRequest>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    if body.vendor != "claude" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "only claude vendor supported for import in v1"})),
        )
            .into_response();
    }
    if body.vendor_uuid.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "vendor_uuid required"})),
        )
            .into_response();
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let caller_identity = identity.owner_tag();
    let result = {
        let mut guard = gw.lock().await;
        guard
            .import_external_session(&slug, &body.vendor_uuid, &caller_identity)
            .await
    };
    match result {
        Ok(sid) => (StatusCode::CREATED, Json(json!({"sid": sid}))).into_response(),
        Err(err) => {
            tracing::warn!(%slug, %err, "import_external_session failed");
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

/// The per-session SSE filter key (cross-stage from the spine): keep a
/// [`GatewayEvent`] iff its `sid` is exactly `Some(target)`. Events with a
/// different `sid` — or none at all (for example the D6 `interaction/ask`
/// hook prompt or an IM-only file send) — are dropped. Web-bound outbound
/// files carry their server-resolved caller sid.
fn event_matches_sid(ev: &GatewayEvent, target: &str) -> bool {
    ev.sid.as_deref() == Some(target)
}

/// True for [`GatewayEvent`]s the web SSE must never emit because they have no
/// web representation (v0.8.19): the 👀 ack `Reaction` is an IM-only affordance
/// (Telegram/Lark message reaction). The web chat has its own UI, so these are
/// dropped at the stream filter, keeping the SSE contract unchanged. Delegates
/// to [`crate::ring::is_im_only_event`] — v0.8.22 P1 moved the canonical
/// definition there so the ring feeder can skip recording them at all (they
/// never occupy a ring slot / never reach the live tap), while this local
/// name stays stable for every existing call site + test in this module.
fn is_im_only_event(ev: &GatewayEvent) -> bool {
    crate::ring::is_im_only_event(ev)
}

/// Build the `event: progress` SSE frame for one [`GatewayEvent`]. The
/// payload is a single-line JSON object carrying the event `id`, its `sid`,
/// a `kind` label (`"answer"` / `"progress"`, with `done` for a finalizing
/// progress update), and the user-visible `content`. Choice prompts arrive
/// as `Answer` events whose `options` are non-empty; those are surfaced too
/// so the SPA can render them. The SSE event name stays `progress` so the
/// SPA's existing per-session parser handles it unchanged.
///
/// v0.8.22 P1 (review §3.1-3) — also sets the SSE-protocol `id:` field to
/// `seq` (the replay ring's per-sid monotonic sequence number), which is
/// what a client's `Last-Event-ID` reconnect watermark is built from and
/// what its dedup logic compares against a replayed/reseeded frame's
/// `MessageEvent.lastEventId`.
fn session_event(ev: &GatewayEvent, seq: u64) -> Event {
    Event::default()
        .id(seq.to_string())
        .event("progress")
        .data(session_event_payload(ev).to_string())
}

/// The JSON payload [`session_event`] serializes (split out for unit tests —
/// asserting on an `axum` `Event`'s rendered body is awkward).
///
/// v0.8.7 review-fix (R-H1): a choice prompt (e.g. the HITL approve/deny
/// bubble) now also carries the resolution `token` plus, per option, its
/// stable `id` — so the SPA can resolve the pending via
/// `POST /api/v1/sessions/{sid}/resolve {token, selection=id}` (the SAME
/// token-keyed pending the IM click resolves), instead of misfiring the
/// option index as a brand-new turn (which never resolved the pending and
/// let the blocked permission hook time out to deny). The `options` array
/// stays backward-friendly: each entry is `{label, id}`. The token is parsed
/// from the option callback `data` (`"{token}:{idx}"`), the single source of
/// the token on the wire.
///
/// v0.9.0 W4 (F4) — also carries `slug` (unconditionally, `null` when
/// unknown) and, for a [`GatewayEventKind::Delegation`], the
/// `relation`/`parent_sid`/`child_sid`/`title?`/`reason?` fields — so
/// `GET /api/v1/agents/events` (`crate::routes::agents`) can reuse this exact
/// serializer for the team view's global SSE frames instead of duplicating
/// the shape. `pub(crate)` for that cross-module reuse.
pub(crate) fn session_event_payload(ev: &GatewayEvent) -> serde_json::Value {
    use ccteam_im::gateway::GatewayEventKind;
    let (kind, done) = match &ev.kind {
        GatewayEventKind::Answer => ("answer", false),
        GatewayEventKind::Progress { done, .. } => ("progress", *done),
        // v0.8.19 — structured per-step activity (web-only; IM drops it).
        GatewayEventKind::Activity { .. } => ("activity", false),
        // v0.8.19 — the 👀 ack `Reaction` is IM-only and is filtered out by
        // `is_im_only_event` before this serializer ever runs; the arm exists
        // only to keep the match exhaustive (a no-op label, never emitted).
        GatewayEventKind::Reaction { .. } => ("reaction", false),
        // v0.9.0 W4 — a delegation lifecycle transition (team view only; a
        // per-sid stream never sees one since `sid` is always `None` on a
        // `Delegation` event, so `event_matches_sid` filters it out upstream
        // of this call — the arm exists for the global agents SSE + match
        // exhaustiveness).
        GatewayEventKind::Delegation { .. } => ("delegation", false),
        GatewayEventKind::SessionLifecycle { .. } => ("session_lifecycle", false),
        GatewayEventKind::ScheduledChanged => ("scheduled_changed", false),
    };
    let mut payload = json!({
        "id": ev.id,
        "sid": ev.sid,
        "slug": ev.slug,
        "kind": kind,
        "content": ev.content,
    });
    if done {
        payload["done"] = serde_json::Value::Bool(true);
    }
    let attachments = ev
        .attachments
        .iter()
        .filter(|file| !file.id.is_empty())
        .filter_map(|file| file.attachment_ref().ok())
        .collect::<Vec<_>>();
    if !attachments.is_empty() {
        payload["attachments"] = serde_json::to_value(attachments).unwrap_or_else(|_| json!([]));
    }
    // v0.8.19 — attach the structured activity form so the web chat can render
    // it as an activity card (the shared `progress::activity_for` summary +
    // kind + item_id for start↔complete dedup). IM never sees this branch.
    if let GatewayEventKind::Activity { activity, .. } = &ev.kind {
        payload["activity"] = serde_json::to_value(activity).unwrap_or(serde_json::Value::Null);
    }
    // v0.9.0 W4 — the team view's edge/status reducer input.
    if let GatewayEventKind::Delegation {
        relation,
        parent_sid,
        child_sid,
        title,
        reason,
    } = &ev.kind
    {
        payload["relation"] = json!(relation);
        payload["parent_sid"] = json!(parent_sid);
        payload["child_sid"] = json!(child_sid);
        if let Some(title) = title {
            payload["title"] = json!(title);
        }
        if let Some(reason) = reason {
            payload["reason"] = json!(reason);
        }
    }
    if let GatewayEventKind::SessionLifecycle { state, reason } = &ev.kind {
        payload["state"] = json!(state);
        payload["reason"] = json!(reason);
    }
    if !ev.options.is_empty() {
        payload["options"] = serde_json::Value::Array(
            ev.options
                .iter()
                .map(|o| json!({ "label": o.label, "id": o.id }))
                .collect(),
        );
        if let Some(token) = approval_token(ev) {
            payload["token"] = serde_json::Value::String(token);
        }
    }
    payload
}

/// Extract the pending-resolution token from a choice-prompt event. Every
/// option's callback `data` is `"{token}:{idx}"` (the single on-wire carrier
/// of the token), so the token is the prefix before the first `:` of the
/// first option. `None` when there are no options or the shape is unexpected
/// (the SPA then omits the resolve affordance rather than guess).
fn approval_token(ev: &GatewayEvent) -> Option<String> {
    let data = &ev.options.first()?.data;
    data.split_once(':').map(|(token, _)| token.to_string())
}

/// Synthetic lag/close frame — mirrors [`super::sse`]'s `reconnect_hint`.
/// `pub(crate)` — also reused by `crate::routes::agents`'s global SSE.
pub(crate) fn reconnect_hint(reason: &str) -> Event {
    Event::default()
        .event("reconnect_hint")
        .data(json!({ "type": "reconnect_hint", "reason": reason }).to_string())
}

/// One-shot notice emitted on the no-gateway SSE path. `pub(crate)` — also
/// reused by `crate::routes::agents`'s global SSE.
pub(crate) fn gateway_unavailable_event() -> Event {
    Event::default()
        .event("gateway_unavailable")
        .data(json!({ "type": "gateway_unavailable", "reason": "no live gateway" }).to_string())
}

/// Shared POST error responder honoring the [`FormOrJson`] mode
/// convention: form ⇒ plain text, JSON ⇒ `{ "ok": false, "error": ... }`.
fn create_error(status: StatusCode, msg: String, mode: InputMode) -> Response {
    match mode {
        InputMode::Form => (status, msg).into_response(),
        InputMode::Json => (status, Json(json!({"ok": false, "error": msg}))).into_response(),
    }
}

fn format_create_session_error(err: &anyhow::Error) -> String {
    let raw = err.to_string();
    if let Some(project) = raw.strip_prefix("unknown project: ") {
        return format!(
            "项目不存在: {project}。下一步: 先在项目目录运行 ccteam init，或从已有项目列表重新选择。"
        );
    }
    if let Some(rest) = raw.strip_prefix("spawn failed: ") {
        return format!(
            "会话启动失败: {rest}。下一步: 请检查项目和角色后重试，或重启 ccteam start。"
        );
    }
    format!("会话启动失败: {raw}。下一步: 请检查项目和角色后重试，或重启 ccteam start。")
}

fn format_submit_error(err: &anyhow::Error) -> String {
    let raw = err.to_string();
    let detail = raw.strip_prefix("submit failed: ").unwrap_or(&raw);
    format!("发送失败: {detail}。下一步: 请重试；如果仍失败，刷新会话列表或重新 /new。")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vendor_accepts_both_case_insensitive() {
        assert_eq!(parse_vendor("claude").unwrap(), AgentVendor::Claude);
        assert_eq!(parse_vendor("Codex").unwrap(), AgentVendor::Codex);
        assert_eq!(parse_vendor("grok").unwrap(), AgentVendor::Grok);
        assert_eq!(parse_vendor("opencode").unwrap(), AgentVendor::Opencode);
        assert_eq!(parse_vendor("pi").unwrap(), AgentVendor::Pi);
        assert_eq!(parse_vendor("dsh").unwrap(), AgentVendor::Dsh);
        assert_eq!(parse_vendor("  CLAUDE ").unwrap(), AgentVendor::Claude);
    }

    #[test]
    fn parse_vendor_rejects_unknown() {
        assert!(parse_vendor("gemini").is_err());
        assert!(parse_vendor("").is_err());
    }

    #[test]
    fn create_session_form_parses_optional_permission_mode() {
        // v0.8.7 W2 (DB.1) — JSON body with permission_mode → parsed; absent
        // field → None → default skip at the handler.
        let with: CreateSessionForm =
            serde_json::from_str(r#"{"role":"r","permission_mode":"hitl"}"#).unwrap();
        assert_eq!(with.permission_mode.as_deref(), Some("hitl"));
        assert_eq!(
            PermissionMode::parse_opt(with.permission_mode.as_deref()).unwrap(),
            PermissionMode::Hitl
        );

        let without: CreateSessionForm = serde_json::from_str(r#"{"role":"r"}"#).unwrap();
        assert!(without.permission_mode.is_none());
        assert_eq!(
            PermissionMode::parse_opt(without.permission_mode.as_deref()).unwrap(),
            PermissionMode::Skip,
            "absent permission_mode ⇒ skip"
        );

        // A bad token is rejected at the API edge (→ 400).
        let bad: CreateSessionForm =
            serde_json::from_str(r#"{"role":"r","permission_mode":"nope"}"#).unwrap();
        assert!(PermissionMode::parse_opt(bad.permission_mode.as_deref()).is_err());
    }

    #[test]
    fn collect_session_turns_reads_mirrored_turns() {
        // v0.8.8 F1 — the history handler reads the ccteam-owned mirror
        // `<project_dir>/.ccteam/chat/<sid>/turns.jsonl` (resolved from the
        // gateway sid), NOT the flat progress.jsonl and NOT a role-keyed dir.
        // Seed two turns and one garbage line; expect two well-formed history
        // events in order.
        use ccteam_harness::execution::turns_mirror::append_turn;
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        // Key the mirror by the session sid (the trailing `s<N>`), not the role.
        let sid = "s1";
        let mk = |id: &str, user: &str, assistant: &str| TurnRecord {
            turn_id: id.into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: "reviewer".into(),
            user: user.into(),
            assistant: assistant.into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
            attachments: vec![],
            outcome: None,
            error_kind: None,
            error: None,
        };
        append_turn(project_dir, sid, &mk("t1", "review the diff", "LGTM")).unwrap();
        append_turn(project_dir, sid, &mk("t2", "and the tests?", "all green")).unwrap();
        // A half-flushed / garbage line must be skipped (read_all_turns drops it).
        let path = ccteam_harness::execution::turns_mirror::turns_jsonl_path(project_dir, sid);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "not-json").unwrap();
        }
        let events = collect_session_turns(project_dir, sid);
        assert_eq!(events.len(), 2, "two parseable turns → two events");
        assert_eq!(events[0]["turn_id"], "t1");
        assert_eq!(events[0]["user"], "review the diff");
        assert_eq!(events[0]["assistant"], "LGTM");
        assert_eq!(events[1]["turn_id"], "t2");
        assert_eq!(events[1]["assistant"], "all green");
    }

    #[test]
    fn collect_session_turns_two_same_role_sids_do_not_bleed() {
        // v0.8.8 F1 (acceptance d) — the BUG-3 root: two sessions of the SAME
        // role keep INDEPENDENT histories because the mirror is keyed by sid,
        // not role. Write a turn under sid1 + a turn under sid2; collecting by
        // sid1 must see ONLY sid1's turn (no cross-bleed).
        use ccteam_harness::execution::turns_mirror::append_turn;
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        let mk = |id: &str, assistant: &str| TurnRecord {
            turn_id: id.into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: "reviewer".into(), // SAME role for both sessions
            user: String::new(),
            assistant: assistant.into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
            attachments: vec![],
            outcome: None,
            error_kind: None,
            error: None,
        };
        append_turn(project_dir, "s1", &mk("t1", "from-s1")).unwrap();
        append_turn(project_dir, "s2", &mk("t2", "from-s2")).unwrap();

        let only_s1 = collect_session_turns(project_dir, "s1");
        assert_eq!(
            only_s1.len(),
            1,
            "sid1 history must not include sid2's turn"
        );
        assert_eq!(only_s1[0]["turn_id"], "t1");
        assert_eq!(only_s1[0]["assistant"], "from-s1");

        let only_s2 = collect_session_turns(project_dir, "s2");
        assert_eq!(
            only_s2.len(),
            1,
            "sid2 history must not include sid1's turn"
        );
        assert_eq!(only_s2[0]["turn_id"], "t2");
        assert_eq!(only_s2[0]["assistant"], "from-s2");
    }

    #[test]
    fn turn_to_event_carries_user_and_assistant() {
        let turn = TurnRecord {
            turn_id: "t9".into(),
            ts: chrono::Utc::now(),
            vendor: "claude".into(),
            role: "cto".into(),
            user: "spawn a reviewer".into(),
            assistant: "done — s2".into(),
            usage: serde_json::Value::Null,
            tool_calls: vec![],
            attachments: vec![ccteam_harness::execution::turns_mirror::AttachmentRef {
                id: "1780000000000-chart.png".into(),
                name: "chart.png".into(),
                kind: ccteam_harness::execution::turns_mirror::AttachmentRefKind::Image,
                size: 42,
            }],
            outcome: None,
            error_kind: None,
            error: None,
        };
        let ev = turn_to_event(&turn);
        assert_eq!(ev["turn_id"], "t9");
        assert_eq!(ev["role"], "cto");
        assert_eq!(ev["user"], "spawn a reviewer");
        assert_eq!(ev["assistant"], "done — s2");
        assert_eq!(ev["attachments"][0]["id"], "1780000000000-chart.png");
        assert_eq!(ev["attachments"][0]["kind"], "image");
        assert!(ev["attachments"][0].get("path").is_none());
    }

    /// Build a minimal [`GatewayEvent`] with the given `sid` for filter tests.
    fn gw_event(sid: Option<&str>) -> GatewayEvent {
        use ccteam_im::gateway::GatewayEventKind;
        GatewayEvent {
            id: "e1".into(),
            channel: "web".into(),
            chat_id: "web-api".into(),
            thread_ts: None,
            content: "hi".into(),
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: sid.map(str::to_string),
            slug: None,
        }
    }

    #[test]
    fn event_matches_sid_keeps_target_drops_others() {
        // Target sid passes; a different sid and a None sid are dropped — the
        // cross-stage SSE filter key.
        assert!(event_matches_sid(&gw_event(Some("s1")), "s1"));
        assert!(!event_matches_sid(&gw_event(Some("s2")), "s1"));
        assert!(!event_matches_sid(&gw_event(None), "s1"));
    }

    /// v0.8.19 — the 👀 ack `Reaction` is IM-only: even when it carries a
    /// matching `sid`, the SSE stream drops it via `is_im_only_event` so the
    /// web SSE contract is unchanged (web has its own UI). Answer/Progress/
    /// Activity are NOT IM-only and still flow.
    #[test]
    fn reaction_event_is_im_only_and_dropped_from_sse() {
        use ccteam_im::gateway::GatewayEventKind;
        let mut reaction = gw_event(Some("s1"));
        reaction.kind = GatewayEventKind::Reaction {
            message_id: "tg-555".into(),
            on: true,
        };
        assert!(
            is_im_only_event(&reaction),
            "a Reaction event must be IM-only (dropped from web SSE)"
        );
        // The SSE filter combines sid-match AND not-im-only: a reaction with a
        // matching sid is still dropped.
        assert!(event_matches_sid(&reaction, "s1"));
        assert!(
            event_matches_sid(&reaction, "s1") && is_im_only_event(&reaction),
            "matches sid but is im-only → the filter drops it"
        );
        // A normal Answer is NOT im-only (still emitted).
        assert!(!is_im_only_event(&gw_event(Some("s1"))));
    }

    #[test]
    fn session_event_maps_answer_and_progress() {
        use ccteam_im::gateway::GatewayEventKind;
        // Answer → kind "answer", sid + content carried, no `done` key.
        let answer = session_event_payload(&gw_event(Some("s1")));
        assert_eq!(answer["kind"], "answer");
        assert_eq!(answer["sid"], "s1");
        assert_eq!(answer["content"], "hi");
        assert!(answer.get("done").is_none());
        // No options ⇒ no token / no options key.
        assert!(answer.get("options").is_none());
        assert!(answer.get("token").is_none());

        // Finalizing progress → kind "progress" + done:true.
        let mut prog = gw_event(Some("s1"));
        prog.kind = GatewayEventKind::Progress {
            status_key: "s1-0".into(),
            done: true,
        };
        let prog = session_event_payload(&prog);
        assert_eq!(prog["kind"], "progress");
        assert_eq!(prog["done"], true);
    }

    #[test]
    fn session_event_carries_only_attachment_reference_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("chart.png");
        std::fs::write(&source, b"png").unwrap();
        let mut event = gw_event(Some("s1"));
        event.content.clear();
        event.attachments.push(ccteam_im::transport::OutboundFile {
            id: "1780000000000-chart.png".into(),
            size: 3,
            path: source.to_string_lossy().into_owned(),
            caption: None,
            kind: ccteam_im::transport::OutboundFileKind::Photo,
        });

        let payload = session_event_payload(&event);
        assert_eq!(payload["attachments"][0]["id"], "1780000000000-chart.png");
        assert_eq!(payload["attachments"][0]["name"], "chart.png");
        assert_eq!(payload["attachments"][0]["kind"], "image");
        assert_eq!(payload["attachments"][0]["size"], 3);
        let wire = payload.to_string();
        assert!(!wire.contains(source.to_string_lossy().as_ref()));
        assert!(!wire.contains("base64"));
    }

    #[test]
    fn session_event_serializes_capacity_eviction_lifecycle() {
        use ccteam_im::gateway::GatewayEventKind;
        let mut ev = gw_event(Some("s4"));
        ev.kind = GatewayEventKind::SessionLifecycle {
            state: "evicted".into(),
            reason: "capacity".into(),
        };
        let payload = session_event_payload(&ev);
        assert_eq!(payload["kind"], "session_lifecycle");
        assert_eq!(payload["state"], "evicted");
        assert_eq!(payload["reason"], "capacity");
    }

    #[test]
    fn session_event_serializes_lightweight_scheduled_invalidation() {
        let mut ev = gw_event(Some("s4"));
        ev.content.clear();
        ev.kind = ccteam_im::gateway::GatewayEventKind::ScheduledChanged;
        let payload = session_event_payload(&ev);
        assert_eq!(payload["kind"], "scheduled_changed");
        assert_eq!(payload["sid"], "s4");
        assert_eq!(payload["content"], "");
        assert!(payload.get("items").is_none());
    }

    /// v0.8.19 — a structured `Activity` event serializes `kind:"activity"`
    /// plus the nested `activity` object (kind / name / summary / status /
    /// item_id) the web chat renders as an activity card. IM never reaches
    /// this branch (strict no-op arm), so this is purely the web wire shape.
    #[test]
    fn session_event_serializes_activity() {
        use ccteam_im::gateway::{ActivityKind, ActivityStatus, GatewayEventKind, SessionActivity};
        let mut ev = gw_event(Some("s3"));
        ev.content = "Bash(ls -la)".into();
        ev.kind = GatewayEventKind::Activity {
            status_key: "s3-0".into(),
            activity: SessionActivity {
                kind: ActivityKind::ToolCall,
                name: "Bash".into(),
                summary: "Bash(ls -la)".into(),
                status: ActivityStatus::Started,
                item_id: "t1".into(),
            },
        };
        let payload = session_event_payload(&ev);
        assert_eq!(payload["kind"], "activity");
        assert_eq!(payload["sid"], "s3");
        // The human content line mirrors the summary.
        assert_eq!(payload["content"], "Bash(ls -la)");
        // No `done` key on an activity frame.
        assert!(payload.get("done").is_none());
        // The nested structured activity (snake_case enums via serde).
        let act = &payload["activity"];
        assert_eq!(act["kind"], "tool_call");
        assert_eq!(act["name"], "Bash");
        assert_eq!(act["summary"], "Bash(ls -la)");
        assert_eq!(act["status"], "started");
        assert_eq!(act["item_id"], "t1");
    }

    /// v0.8.7 review-fix (R-H1) — an approval ChoicePrompt event serializes its
    /// resolution `token` plus, per option, `{label, id}`, so the SPA can
    /// resolve via `POST /resolve {token, selection=id}` (NOT misfire the index
    /// as a turn). The token is parsed from the option callback `data`
    /// (`"{token}:{idx}"`), the single on-wire carrier.
    #[test]
    fn session_event_carries_token_and_option_ids_for_approval() {
        use ccteam_im::transport::MessageOption;
        let mut ev = gw_event(Some("s7"));
        ev.content = "session s7 (cto) wants to run: Bash rm -rf /".into();
        ev.options = vec![
            MessageOption {
                data: "pcafef00d:0".into(),
                label: "✅ Approve".into(),
                id: "allow".into(),
            },
            MessageOption {
                data: "pcafef00d:1".into(),
                label: "⛔ Deny".into(),
                id: "deny".into(),
            },
        ];
        let payload = session_event_payload(&ev);
        // token lifted from the option `data` prefix.
        assert_eq!(payload["token"], "pcafef00d");
        // each option carries its stable id (the decision value) + label.
        let opts = payload["options"].as_array().expect("options array");
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["label"], "✅ Approve");
        assert_eq!(opts[0]["id"], "allow");
        assert_eq!(opts[1]["id"], "deny");
    }

    #[test]
    fn approval_token_parses_prefix_and_handles_empty() {
        use ccteam_im::transport::MessageOption;
        let mut ev = gw_event(Some("s1"));
        ev.options = vec![MessageOption {
            data: "ptok:0".into(),
            label: "x".into(),
            id: "allow".into(),
        }];
        assert_eq!(approval_token(&ev).as_deref(), Some("ptok"));
        // No options ⇒ None (the payload then omits the resolve affordance).
        let bare = gw_event(Some("s1"));
        assert!(approval_token(&bare).is_none());
    }

    /// No session is mid-turn as far as the daemon knows — the file verdict
    /// stands alone (what a daemonless reader sees too).
    fn no_live_turns() -> std::collections::HashMap<String, ccteam_core::stall::LiveTurn> {
        std::collections::HashMap::new()
    }

    fn one_live_turn(
        sid: &str,
        live: ccteam_core::stall::LiveTurn,
    ) -> std::collections::HashMap<String, ccteam_core::stall::LiveTurn> {
        std::collections::HashMap::from([(sid.to_string(), live)])
    }

    fn view(sid: &str) -> SessionView {
        SessionView {
            driveable: true,
            sid: sid.into(),
            project: "demo".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
            protocol: "stream-json".into(),
            host: "local".into(),
            current: true,
            status: "live".into(),
            last_activity_seconds: None,
            created_at: String::new(),
            last_active: String::new(),
            title: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            model: None,
            waiting_approval: false,
            parent_sid: None,
            delegation_depth: 0,
        }
    }

    /// The reported bug, from the web end: a session whose turn is in flight
    /// must not read `idle` just because the project's progress stream told the
    /// reader nothing (unreadable / rotated / no event for this sid yet). Green
    /// here is what made the SPA rail and IM `/status` disagree.
    #[test]
    fn live_turn_keeps_a_mid_turn_session_honest_when_the_stream_says_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        write_project_state(&paths, "demo");
        let mut views = vec![view("s1")];

        // No progress stream at all → the file verdict is a bare "idle".
        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "idle");

        views[0].status = "live".into();
        apply_progress_activity_status(
            &paths,
            "demo",
            &mut views,
            &one_live_turn(
                "s1",
                ccteam_core::stall::LiveTurn {
                    silent_seconds: 3,
                    elapsed_seconds: 42,
                    stuck_after_seconds: 300,
                },
            ),
        );
        assert_eq!(views[0].status, "working");
        assert_eq!(views[0].last_activity_seconds, None);
    }

    /// A project missing from the catalog used to short-circuit the whole
    /// replacement, leaving the gateway's `"live"` hint — a green dot on a
    /// surface whose job is to say what the session is doing.
    #[test]
    fn unknown_project_still_reports_a_real_activity_not_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        let mut views = vec![view("s1")];
        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "idle");
    }

    #[test]
    fn progress_activity_status_uses_core_file_backed_classifier() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        write_project_state(&paths, "demo");
        let mut views = vec![SessionView {
            driveable: true,
            sid: "s1".into(),
            project: "demo".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
            protocol: "stream-json".into(),
            host: "local".into(),
            current: true,
            status: "live".into(),
            last_activity_seconds: None,
            created_at: String::new(),
            last_active: String::new(),
            title: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            model: None,
            waiting_approval: false,
            parent_sid: None,
            delegation_depth: 0,
        }];

        let stale_completed = serde_json::json!({
            "event": ccteam_core::progress::CHAT_TURN_COMPLETED,
            "ts": (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339(),
        });
        ccteam_core::progress::append_event(&paths.progress_jsonl("demo"), &stale_completed)
            .unwrap();
        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "idle");
        assert!(views[0].last_activity_seconds.is_some());

        let timeout = serde_json::json!({
            "event": ccteam_core::progress::CHAT_TURN_TIMEOUT,
            "stuck": true,
            "ts": chrono::Utc::now().to_rfc3339(),
        });
        ccteam_core::progress::append_event(&paths.progress_jsonl("demo"), &timeout).unwrap();
        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "stuck");
    }

    #[test]
    fn progress_activity_status_prefers_sid_specific_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        write_project_state(&paths, "demo");
        let mut views = vec![
            SessionView {
                driveable: true,
                sid: "s1".into(),
                project: "demo".into(),
                role: "cto".into(),
                vendor: "claude".into(),
                permission_mode: "skip".into(),
                protocol: "stream-json".into(),
                host: "local".into(),
                current: true,
                status: "live".into(),
                last_activity_seconds: None,
                created_at: String::new(),
                last_active: String::new(),
                title: None,
                turn_count: 0,
                cost_usd: None,
                tokens_total: None,
                model: None,
                waiting_approval: false,
                parent_sid: None,
                delegation_depth: 0,
            },
            SessionView {
                driveable: true,
                sid: "s2".into(),
                project: "demo".into(),
                role: "qa".into(),
                vendor: "claude".into(),
                permission_mode: "skip".into(),
                protocol: "stream-json".into(),
                host: "local".into(),
                current: false,
                status: "live".into(),
                last_activity_seconds: None,
                created_at: String::new(),
                last_active: String::new(),
                title: None,
                turn_count: 0,
                cost_usd: None,
                tokens_total: None,
                model: None,
                waiting_approval: false,
                parent_sid: None,
                delegation_depth: 0,
            },
        ];

        ccteam_core::progress::append_event(
            &paths.progress_jsonl("demo"),
            &serde_json::json!({
                "event": ccteam_core::progress::CHAT_TURN_COMPLETED,
                "sid": "s1",
                "ts": (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339(),
            }),
        )
        .unwrap();
        ccteam_core::progress::append_event(
            &paths.progress_jsonl("demo"),
            &serde_json::json!({
                "event": ccteam_core::progress::CHAT_TURN_TIMEOUT,
                "sid": "s2",
                "stuck": true,
                "ts": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .unwrap();

        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "idle");
        assert!(views[0].last_activity_seconds.unwrap() >= 60);
        assert_eq!(views[1].status, "stuck");
        assert!(views[1].last_activity_seconds.unwrap() < 60);
    }

    #[test]
    fn progress_activity_status_hides_age_while_working() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        write_project_state(&paths, "demo");
        let mut views = vec![SessionView {
            driveable: true,
            sid: "s1".into(),
            project: "demo".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
            protocol: "stream-json".into(),
            host: "local".into(),
            current: true,
            status: "live".into(),
            last_activity_seconds: None,
            created_at: String::new(),
            last_active: String::new(),
            title: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            model: None,
            waiting_approval: false,
            parent_sid: None,
            delegation_depth: 0,
        }];

        ccteam_core::progress::append_event(
            &paths.progress_jsonl("demo"),
            &serde_json::json!({
                "event": ccteam_core::progress::CHAT_TURN_USER_PROMPT,
                "sid": "s1",
                "ts": chrono::Utc::now().to_rfc3339(),
            }),
        )
        .unwrap();

        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "working");
        assert_eq!(views[0].last_activity_seconds, None);
    }

    #[test]
    fn progress_activity_status_marks_old_non_idle_event_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(tmp.path());
        write_project_state(&paths, "demo");
        let mut views = vec![SessionView {
            driveable: true,
            sid: "s1".into(),
            project: "demo".into(),
            role: "cto".into(),
            vendor: "claude".into(),
            permission_mode: "skip".into(),
            protocol: "stream-json".into(),
            host: "local".into(),
            current: true,
            status: "live".into(),
            last_activity_seconds: None,
            created_at: String::new(),
            last_active: String::new(),
            title: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            model: None,
            waiting_approval: false,
            parent_sid: None,
            delegation_depth: 0,
        }];

        ccteam_core::progress::append_event(
            &paths.progress_jsonl("demo"),
            &serde_json::json!({
                "event": ccteam_core::progress::CHAT_TURN_USER_PROMPT,
                "sid": "s1",
                "ts": (chrono::Utc::now()
                    - chrono::Duration::seconds(ccteam_core::stall::STALL_WARN_SECONDS as i64))
                .to_rfc3339(),
            }),
        )
        .unwrap();

        apply_progress_activity_status(&paths, "demo", &mut views, &no_live_turns());
        assert_eq!(views[0].status, "stale");
        assert!(
            views[0].last_activity_seconds.unwrap() >= ccteam_core::stall::STALL_WARN_SECONDS - 1
        );
    }

    #[test]
    fn collect_session_turns_missing_file_is_empty() {
        // Absent turns.jsonl is the legitimate first-turn case → empty (200),
        // not an error. read_all_turns returns Ok(empty) for a missing file.
        // v0.8.8 F1 — keyed by sid (the never-spawned `s99` has no mirror yet).
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(collect_session_turns(tmp.path(), "s99").is_empty());
    }

    fn test_paths(root: &std::path::Path) -> ccteam_core::CcteamPaths {
        ccteam_core::CcteamPaths {
            root: root.join(".ccteam"),
            projects_root: root.join("projects"),
        }
    }

    fn write_project_state(paths: &ccteam_core::CcteamPaths, slug: &str) {
        let state_path = paths.project_state(slug);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let state = ccteam_core::ProjectState::initial(slug.to_string());
        std::fs::write(state_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    }

    // ── v0.8.22 P1 (review §3.1-3) — SSE Last-Event-ID / reseed ────────────

    #[test]
    fn parse_last_event_id_prefers_header_over_query() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", "42".parse().unwrap());
        let query = SessionEventsQuery {
            last_event_id: Some("7".to_string()),
        };
        assert_eq!(parse_last_event_id(&headers, &query), Some(42));
    }

    #[test]
    fn parse_last_event_id_falls_back_to_query() {
        let headers = HeaderMap::new();
        let query = SessionEventsQuery {
            last_event_id: Some("7".to_string()),
        };
        assert_eq!(parse_last_event_id(&headers, &query), Some(7));
    }

    #[test]
    fn parse_last_event_id_absent_or_unparseable_is_none() {
        let headers = HeaderMap::new();
        assert_eq!(
            parse_last_event_id(
                &headers,
                &SessionEventsQuery {
                    last_event_id: None
                }
            ),
            None
        );
        assert_eq!(
            parse_last_event_id(
                &headers,
                &SessionEventsQuery {
                    last_event_id: Some("not-a-number".to_string())
                }
            ),
            None
        );
    }

    fn test_choice_prompt(token: &str) -> ChoicePrompt {
        ChoicePrompt {
            token: token.to_string(),
            title: format!("session s1 wants to run: Bash rm -rf /tmp/{token}"),
            options: vec![
                ccteam_harness::ChoiceOption {
                    id: "allow".into(),
                    label: "✅ Approve".into(),
                },
                ccteam_harness::ChoiceOption {
                    id: "deny".into(),
                    label: "⛔ Deny".into(),
                },
            ],
            multi: false,
        }
    }

    #[test]
    fn synthetic_approval_event_mirrors_the_original_broadcast_shape() {
        let prompt = test_choice_prompt("ptok");
        let ev = synthetic_approval_event("s1", &prompt);
        assert_eq!(ev.id, "permission-ptok");
        assert_eq!(ev.sid.as_deref(), Some("s1"));
        assert_eq!(ev.content, prompt.title);

        let payload = session_event_payload(&ev);
        assert_eq!(payload["token"], "ptok");
        let opts = payload["options"].as_array().expect("options array");
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["id"], "allow");
        assert_eq!(opts[1]["id"], "deny");
    }

    /// A minimal `HarnessAdapter` never actually invoked by these tests —
    /// `Gateway::new_with_factory` requires a factory, but none of the
    /// `build_catchup_entries` tests spawn a session through it.
    struct RingTestAdapter;

    #[async_trait::async_trait]
    impl ccteam_harness::HarnessAdapter for RingTestAdapter {
        fn name(&self) -> &'static str {
            "ring-test-adapter"
        }
        fn vendor(&self) -> AgentVendor {
            AgentVendor::Claude
        }
        async fn start_thread(
            &self,
            _spec: &ccteam_harness::AgentSpecBrief,
            ctx: &ccteam_harness::SpawnCtx,
        ) -> Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError> {
            Ok(ccteam_harness::ThreadHandle {
                vendor: AgentVendor::Claude,
                mode: ccteam_harness::ExecutionMode::Chat,
                identity: format!("test-{}", ctx.sid),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({}),
            })
        }
        async fn submit_turn(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _input: ccteam_harness::TurnInput,
        ) -> Result<ccteam_harness::TurnId, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by these tests")
        }
        async fn submit_turn_routed(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _input: ccteam_harness::TurnInput,
            _routing: ccteam_harness::TurnRouting,
        ) -> Result<ccteam_harness::TurnSubmission, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by these tests")
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
            _h: &ccteam_harness::ThreadHandle,
        ) -> futures::stream::BoxStream<'static, ccteam_harness::ThreadEvent> {
            Box::pin(futures::stream::empty())
        }
        async fn resume_thread(
            &self,
            _persistent_id: &str,
        ) -> Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by these tests")
        }
        async fn close_thread(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> Result<(), ccteam_harness::HarnessError> {
            Ok(())
        }
        async fn handle_directive(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _d: ccteam_harness::Directive,
        ) -> Result<ccteam_harness::DirectiveOutcome, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by these tests")
        }
        async fn thread_status(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> Result<ccteam_harness::ThreadStatus, ccteam_harness::HarnessError> {
            Ok(ccteam_harness::ThreadStatus::default())
        }
    }

    /// An `AppState` with a real (but session-less) gateway attached, so
    /// `build_catchup_entries`'s `app.gateway`/`app.session_ring` paths are
    /// exercised for real (spawns the ring feeder too, harmlessly idle since
    /// nothing ever turns through this gateway).
    fn test_app_with_gateway(tmp: &std::path::Path) -> AppState {
        let paths = test_paths(tmp);
        let factory = std::sync::Arc::new(|_vendor, _protocol| {
            std::sync::Arc::new(RingTestAdapter)
                as std::sync::Arc<dyn ccteam_harness::HarnessAdapter + Send + Sync>
        });
        let gateway = ccteam_im::gateway::Gateway::new_with_factory(
            factory,
            "demo",
            paths.projects_root.join("demo"),
        );
        AppState::new(paths).with_gateway_owned(gateway)
    }

    #[tokio::test]
    async fn scheduled_rest_create_list_and_cancel() {
        let tmp = tempfile::TempDir::new().unwrap();
        let app = test_app_with_gateway(tmp.path());
        let sid = app
            .gateway
            .as_ref()
            .unwrap()
            .lock()
            .await
            .create_session_api(
                "demo".into(),
                String::new(),
                AgentVendor::Claude,
                PermissionMode::Skip,
            )
            .await
            .unwrap()
            .sid;
        let identity = crate::auth::Identity::admin();

        let created = handle_create_scheduled(
            State(app.clone()),
            Extension(identity.clone()),
            Path(sid.clone()),
            Json(CreateScheduledRequest {
                text: "run the checks".into(),
                send_at: None,
                when: Some("+30m".into()),
            }),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);

        let listed = handle_list_scheduled(
            State(app.clone()),
            Extension(identity.clone()),
            Path(sid.clone()),
        )
        .await;
        assert_eq!(listed.status(), StatusCode::OK);
        assert_eq!(
            app.gateway
                .as_ref()
                .unwrap()
                .lock()
                .await
                .scheduled_items_for_sid(&sid)
                .unwrap()
                .len(),
            1
        );

        let cancelled = handle_cancel_scheduled(
            State(app.clone()),
            Extension(identity),
            Path((sid.clone(), "d1".into())),
        )
        .await;
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert!(app
            .gateway
            .as_ref()
            .unwrap()
            .lock()
            .await
            .scheduled_items_for_sid(&sid)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn scheduled_request_rejects_attachment_fields() {
        let parsed = serde_json::from_value::<CreateScheduledRequest>(serde_json::json!({
            "text": "later",
            "when": "+30m",
            "attachments": [{"kind": "file", "path": "/tmp/nope"}]
        }));
        assert!(
            parsed.is_err(),
            "scheduled v1 must not silently accept attachments"
        );
    }

    /// Register + sid-tag a pending approval directly against the app's
    /// gateway's shared pending registry — the same steps
    /// `ccteam_im::hitl::ask_permission` takes, without needing a live HITL
    /// turn.
    async fn seed_pending_approval(app: &AppState, sid: &str, token: &str) {
        let gw = app.gateway.as_ref().unwrap();
        let pending = gw.lock().await.pending_handle();
        let mut guard = pending.lock().await;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        guard.register(
            token.to_string(),
            test_choice_prompt(token),
            ccteam_im::pending::InteractionOrigin::External { reply: tx },
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        guard.tag_sid(token, sid.to_string());
    }

    /// review §3.1-3's explicit ask: "a fresh page load must also see a
    /// pending approval, not just reconnects" — no `last_id` at all (a
    /// brand-new tab) still gets the outstanding approval.
    #[tokio::test]
    async fn build_catchup_entries_fresh_connect_seeds_pending_even_without_last_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let app = test_app_with_gateway(tmp.path());
        seed_pending_approval(&app, "s1", "ptok").await;

        let entries = build_catchup_entries(&app, "s1", None).await;
        assert_eq!(entries.len(), 1, "the pending approval must still surface");
        assert_eq!(entries[0].1.id, "permission-ptok");
    }

    /// No pending, no ring data ⇒ nothing to catch up on.
    #[tokio::test]
    async fn build_catchup_entries_empty_when_nothing_pending_or_buffered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let app = test_app_with_gateway(tmp.path());
        assert!(build_catchup_entries(&app, "s1", None).await.is_empty());
        assert!(build_catchup_entries(&app, "s1", Some(0)).await.is_empty());
    }

    /// A reconnect with `last_id` replays the ring gap for that sid only.
    #[tokio::test]
    async fn build_catchup_entries_replays_the_ring_gap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let app = test_app_with_gateway(tmp.path());
        let ans = |id: &str, sid: &str| GatewayEvent {
            id: id.to_string(),
            channel: "web".into(),
            chat_id: "web-api".into(),
            thread_ts: None,
            content: id.to_string(),
            kind: ccteam_im::gateway::GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: Some(sid.to_string()),
            slug: None,
        };
        app.session_ring.record("s1", ans("a", "s1"));
        let since = app.session_ring.record("s1", ans("b", "s1"));
        app.session_ring.record("s1", ans("c", "s1"));
        // A different sid's traffic must not leak into s1's replay.
        app.session_ring.record("s2", ans("z", "s2"));

        let entries = build_catchup_entries(&app, "s1", Some(since)).await;
        let ids: Vec<&str> = entries.iter().map(|(_, ev)| ev.id.as_str()).collect();
        assert_eq!(ids, vec!["c"]);
    }

    /// If the ring replay ALREADY carries the outstanding approval's token
    /// (a plain reconnect whose gap the ring still covers), the reseed must
    /// not ALSO append a duplicate entry.
    #[tokio::test]
    async fn build_catchup_entries_skips_reseed_when_ring_already_covers_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let app = test_app_with_gateway(tmp.path());
        seed_pending_approval(&app, "s1", "ptok").await;
        // The SAME token's original broadcast, already in the ring (as if
        // ask_permission's own `sink.send` had recorded it moments ago).
        let original = synthetic_approval_event("s1", &test_choice_prompt("ptok"));
        app.session_ring.record("s1", original);

        let entries = build_catchup_entries(&app, "s1", Some(0)).await;
        let matching: Vec<_> = entries
            .iter()
            .filter(|(_, ev)| approval_token(ev).as_deref() == Some("ptok"))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "must not double-render the same outstanding approval"
        );
    }

    /// A pending approval for a DIFFERENT sid must never leak into this
    /// sid's catchup.
    #[tokio::test]
    async fn build_catchup_entries_ignores_other_sids_pending() {
        let tmp = tempfile::TempDir::new().unwrap();
        let app = test_app_with_gateway(tmp.path());
        seed_pending_approval(&app, "s2", "ptok").await;

        let entries = build_catchup_entries(&app, "s1", None).await;
        assert!(entries.is_empty());
    }

    /// The form → SpawnTuning contract: BOTH facets reach every vendor
    /// verbatim. Grok and kimi are the regression anchors — this entry point
    /// used to zero their effort, so an explicit `effort` looked accepted
    /// (201 + a live sid) while the session actually ran at the vendor
    /// default. The per-vendor loop keeps a future "just this one vendor"
    /// exception from being re-added quietly.
    #[test]
    fn spawn_tuning_from_form_passes_model_and_effort_through_for_every_vendor() {
        for (vendor, model, effort) in [
            (AgentVendor::Claude, "opus-4.8", "max"),
            (AgentVendor::Codex, "gpt-5.2-codex", "xhigh"),
            (AgentVendor::Grok, "grok-code", "high"),
            (AgentVendor::Opencode, "anthropic/claude-opus-4-5", "high"),
            (AgentVendor::Kimi, "kimi-code/k3", "max"),
            (AgentVendor::Pi, "anthropic/claude-sonnet-4-5", "high"),
        ] {
            let t = spawn_tuning_from_form(vendor, Some(model.into()), Some(effort.into()));
            assert_eq!(t.model.as_deref(), Some(model), "{vendor:?} dropped model");
            assert_eq!(
                t.effort.as_deref(),
                Some(effort),
                "{vendor:?} dropped the caller's explicit effort — the vendor owns \
                 that verdict, ccteam must not pre-empt it"
            );
        }

        // Omitted stays omitted: absence is how a caller asks for the vendor
        // default, and it must never be back-filled here either.
        let t = spawn_tuning_from_form(AgentVendor::Grok, None, None);
        assert_eq!(t.model, None);
        assert_eq!(t.effort, None);
    }
}

#[cfg(test)]
mod skill_line_tests {
    use super::skill_attachment_line;
    use std::path::Path;

    /// The vendor-native invocation seam: claude names its Skill tool
    /// (`/name`), codex gets the plaintext `$name` mention, and the ACP trio
    /// (no native loader) keeps the neutral read-and-follow line.
    #[test]
    fn skill_line_renders_per_vendor_native_syntax() {
        let md = Path::new("/p/.claude/skills/deep-research/SKILL.md");
        let claude = skill_attachment_line("claude", "deep-research", md);
        assert!(claude.contains("invoke /deep-research"), "{claude}");
        assert!(claude.contains("SKILL.md"), "{claude}");

        let codex = skill_attachment_line("codex", "deep-research", md);
        assert!(codex.contains("$deep-research"), "{codex}");

        for acp in ["grok", "opencode", "kimi"] {
            let line = skill_attachment_line(acp, "deep-research", md);
            assert!(
                line.contains("read /p/.claude/skills/deep-research/SKILL.md and follow it"),
                "{acp}: {line}"
            );
        }
    }
}
