//! v0.9.0 W4 (F4) — team visualization: `GET /api/v1/agents/graph` (a point-in-time
//! snapshot of every session across every host, as nodes + parent→child
//! delegation edges) and `GET /api/v1/agents/events` (the global SSE feed the
//! snapshot stays live from).
//!
//! **Sources** (never a new state SoT — this reads the SAME data the rest of
//! the resource API already serves): [`ccteam_harness::list_session_metas`]
//! (every session that ever existed, on disk, per project) ⋈
//! [`ccteam_im::gateway::Gateway::session_views`] (which of those are
//! currently tracked in memory ⇒ `"live"`) ⋈
//! [`ccteam_harness::HarnessAdapter::thread_status`] per live sid (resolved
//! under the same lock, awaited after it drops — the SAME statusline source
//! `GET /sessions/{sid}/status` serves, via
//! [`super::sessions_api::resolved_thread_status`] ⇒ `nodes[].model` +
//! `nodes[].effort`) ⋈
//! [`ccteam_im::gateway::Gateway::armed_delegation_watch_sids`] (a
//! best-effort seed for `edges[].active`, corrected live by the SSE
//! `dispatched`/`completed` frames — see `AgentsView`'s reducer).
//!
//! **ACL**: every identity sees exactly the projects
//! [`crate::auth::Identity::can_see_owner`] allows (mirrors the `/projects`
//! collection filter, `api_v1::build_projects`) — both for the graph snapshot
//! AND for every SSE frame. The operator is NOT exempt: `can_see_owner` keeps
//! it out of a tenant's projects, so the 2026-07-28 cross-user fix stopped streaming those sessions'
//! live answers into the team view. The two differ only on an unattributed
//! frame (no resolvable `slug`): the operator still sees it, a tenant fails
//! closed. `?slug=` narrows the graph to one project (still gated by
//! [`super::api_v1::can_see_project`]).
//!
//! **Status honesty**: a node is `"live"` when the gateway currently tracks
//! it (in the in-memory session map) and `"idle"` otherwise (its `meta.json`
//! persists on disk but nothing is currently spawned for it). This wave does
//! not distinguish an idle-but-resumable session from one a user explicitly
//! `stop`ped — no such flag exists on `meta.json` today; documented as a
//! known scope reduction in the W4 handoff.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Extension, Json,
};
use ccteam_harness::ThreadStatus;
use ccteam_im::gateway::{GatewayEvent, SessionView};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use utoipa::ToSchema;

use super::sessions_api::{
    gateway_unavailable_event, no_gateway, parse_last_event_id, project_not_visible,
    reconnect_hint, SessionEventsQuery, KEEPALIVE_INTERVAL,
};
use crate::auth::Identity;
use crate::state::AppState;

/// One session in the team graph — the union of its durable `meta.json` and
/// (when tracked) its live [`SessionView`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentNode {
    pub sid: String,
    pub slug: String,
    pub role: String,
    pub vendor: String,
    /// The session's model: for `"live"` nodes the statusline truth (the same
    /// per-session source as `GET /api/v1/sessions/{sid}/status`, so
    /// mid-session model switches are reflected), else the durable
    /// `meta.json` facts — the requested spawn pick, or failing that the
    /// model the VENDOR last stamped on a completed turn
    /// (`SessionMeta::observed_model`, refreshed by the per-turn meta write).
    /// `null` only when nothing was ever requested OR reported — e.g. an
    /// external enrolled node ccteam never ran a thread for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The reasoning-effort level that model runs at (`low`/`medium`/`high`/
    /// `xhigh`/`max`), from the same live statusline join as `model`. `null`
    /// on idle nodes and on vendors/models with no effort axis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub host: String,
    /// `"live"` (gateway-tracked) or `"idle"` (persisted, not tracked). See
    /// the module doc's status-honesty note.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_sid: Option<String>,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Raw token ledger, present even for vendors with no USD price table
    /// (grok/codex/opencode/kimi) — the honest magnitude signal when
    /// `cost_usd` cannot exist, same contract as the session list's
    /// `tokens_total`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub last_active: String,
    pub turn_count: u64,
}

/// One parent→child delegation edge (derived from `nodes[].parent_sid`, not
/// separately fetched).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentEdge {
    pub parent: String,
    pub child: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Best-effort seed: `true` when the child has an armed delegation
    /// completion watch (a dispatch not yet disarmed). The SPA corrects this
    /// live from `dispatched`/`completed` SSE frames — see the module doc.
    pub active: bool,
}

/// `GET /api/v1/agents/graph` response body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AgentsGraphResponse {
    pub nodes: Vec<AgentNode>,
    pub edges: Vec<AgentEdge>,
    /// Every host any node runs on, `"local"` first, then sorted.
    pub hosts: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AgentsGraphQuery {
    #[serde(default)]
    slug: Option<String>,
}

/// How long ONE live session gets to report its statusline before the graph
/// gives up on it and reports the node without `model`/`effort`. The snapshot
/// is a fleet overview, not a per-session probe: a node's model is a nicety,
/// while a stalled response blocks the whole team view. Per-session (not
/// global) so one slow vendor never costs the others their model.
const LIVE_STATUS_DEADLINE: std::time::Duration = std::time::Duration::from_millis(750);

/// Project slugs `identity` may see (mirrors `api_v1::build_projects`'s
/// per-tenant filter). Best-effort: a `collect_projects` failure degrades to
/// "no projects visible" rather than 500ing the whole graph.
fn visible_project_slugs(app: &AppState, identity: &Identity) -> Vec<String> {
    ccteam_core::collect_projects(&app.paths)
        .map(|summaries| {
            summaries
                .into_iter()
                .filter(|s| identity.can_see_owner(s.state.owner.as_deref()))
                .map(|s| s.state.slug)
                .collect()
        })
        .unwrap_or_default()
}

/// Sort hosts with `"local"` pinned first, everything else alphabetical.
fn sort_hosts(hosts: HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = hosts.into_iter().collect();
    out.sort_by(|a, b| match (a == "local", b == "local") {
        (true, true) | (false, false) => a.cmp(b),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
    });
    out
}

fn normalize_host(host: &str) -> String {
    if host.is_empty() {
        "local".to_string()
    } else {
        host.to_string()
    }
}

/// Build the graph snapshot for exactly the given (already ACL-filtered)
/// `slugs`, from a live-session-view lookup + armed-watch set the caller
/// resolved under the gateway lock, plus `live_status` — the per-live-sid
/// statusline the caller read AFTER dropping that lock (only live sids ever
/// appear in it, so an idle node's `model`/`effort` are honestly `None`).
/// Pure over its inputs (`project_dir` + these maps) so it's unit-testable
/// without a server.
pub(crate) fn build_agents_graph(
    project_dir_for: impl Fn(&str) -> std::path::PathBuf,
    slugs: &[String],
    live_by_sid: &HashMap<String, SessionView>,
    live_status: &HashMap<String, ThreadStatus>,
    armed_watches: &HashSet<String>,
) -> AgentsGraphResponse {
    let mut nodes = Vec::new();
    let mut hosts: HashSet<String> = HashSet::new();
    for slug in slugs {
        let dir = project_dir_for(slug);
        for m in ccteam_harness::list_session_metas(&dir) {
            let host = normalize_host(&m.host);
            hosts.insert(host.clone());
            let status = if live_by_sid.contains_key(&m.sid) {
                "live"
            } else {
                "idle"
            };
            let live = live_status.get(&m.sid);
            nodes.push(AgentNode {
                sid: m.sid.clone(),
                slug: slug.clone(),
                role: m.role.clone(),
                vendor: ccteam_im::delegation::vendor_key(m.vendor).to_string(),
                model: live
                    .and_then(|s| s.model.clone())
                    .or_else(|| m.model.clone())
                    .or_else(|| m.observed_model.clone()),
                effort: live
                    .and_then(|s| s.effort.clone())
                    .or_else(|| m.effort.clone()),
                host,
                status: status.to_string(),
                parent_sid: m.parent_sid.clone(),
                depth: m.delegation_depth,
                cost_usd: m.cost_usd,
                tokens_total: m.tokens_total,
                title: m.title.clone(),
                last_active: m.last_active.clone(),
                turn_count: m.turn_count,
            });
        }
    }
    let edges: Vec<AgentEdge> = nodes
        .iter()
        .filter_map(|n| {
            n.parent_sid.as_ref().map(|parent| AgentEdge {
                parent: parent.clone(),
                child: n.sid.clone(),
                title: n.title.clone(),
                active: armed_watches.contains(&n.sid),
            })
        })
        .collect();
    AgentsGraphResponse {
        nodes,
        edges,
        hosts: sort_hosts(hosts),
    }
}

/// `GET /api/v1/agents/graph`
///
/// Snapshot of every session across every visible project, as nodes + parent
/// → child delegation edges. `?slug=` narrows to one project. ACL: admin
/// sees everything; a tenant sees only projects it owns (404 for an
/// unowned/unknown `?slug=`, matching `project_not_visible`'s "don't reveal
/// existence" convention). 503 with no live gateway (the same no-gateway
/// contract every session endpoint has).
#[utoipa::path(
    get,
    path = "/api/v1/agents/graph",
    tag = "agents",
    params(("slug" = Option<String>, Query, description = "Narrow to one project slug")),
    responses(
        (status = 200, description = "Team graph snapshot", body = AgentsGraphResponse),
        (status = 404, description = "`slug` given but not visible/unknown"),
        (status = 503, description = "No live gateway (standalone web)"),
    ),
)]
pub(crate) async fn handle_agents_graph(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(q): Query<AgentsGraphQuery>,
) -> Response {
    if let Some(slug) = q.slug.as_deref() {
        if !super::api_v1::can_see_project(&app, &identity, slug) {
            return project_not_visible(slug);
        }
    }
    let Some(gw) = app.gateway.as_ref() else {
        return no_gateway();
    };
    let slugs: Vec<String> = match &q.slug {
        Some(slug) => vec![slug.clone()],
        None => visible_project_slugs(&app, &identity),
    };
    let (live_by_sid, armed_watches, live_handles) = {
        let guard = gw.lock().await;
        let live: HashMap<String, SessionView> = guard
            .session_views()
            .into_iter()
            .map(|v| (v.sid.clone(), v))
            .collect();
        let armed = guard.armed_delegation_watch_sids();
        // `(adapter, thread)` clones for every live sid, resolved under this
        // same single lock acquisition — the `thread_status` I/O below runs
        // only after the guard drops (the status endpoint's lock-drop
        // discipline).
        let handles: Vec<_> = live
            .keys()
            .filter_map(|sid| {
                guard
                    .session_status_handle(sid)
                    .map(|(adapter, thread)| (sid.clone(), adapter, thread))
            })
            .collect();
        (live, armed, handles)
    };
    // The live-statusline join (model + effort): one per-live-sid read per
    // snapshot, through the SAME helper `GET /sessions/{sid}/status` serves —
    // one source of truth for what a session is running.
    //
    // BOUNDED BY CONSTRUCTION (2026-08-02). This used to be a sequential loop
    // with no deadline, so the endpoint's latency was the SUM over live
    // sessions of whatever each vendor took to answer — unbounded, and one
    // stuck adapter held the whole team view hostage. Now the reads run
    // concurrently and each carries its own deadline: a session that cannot
    // answer in time is simply reported without `model`/`effort` (already a
    // valid, honest response — an idle node reports the same). Worst-case
    // latency is therefore one deadline regardless of fleet size, and any
    // FUTURE adapter is covered without touching this code.
    let live_status: HashMap<String, ThreadStatus> =
        futures::future::join_all(live_handles.into_iter().map(
            |(sid, adapter, thread)| async move {
                match tokio::time::timeout(
                    LIVE_STATUS_DEADLINE,
                    super::sessions_api::resolved_thread_status(adapter, thread, &sid),
                )
                .await
                {
                    Ok(status) => Some((sid, status)),
                    Err(_) => {
                        tracing::warn!(
                            %sid,
                            "thread_status exceeded the graph deadline; reporting no model"
                        );
                        None
                    }
                }
            },
        ))
        .await
        .into_iter()
        .flatten()
        .collect();
    let paths = app.paths.clone();
    let graph = build_agents_graph(
        |slug| paths.project_dir(slug),
        &slugs,
        &live_by_sid,
        &live_status,
        &armed_watches,
    );
    Json(graph).into_response()
}

/// Which live frames an SSE subscriber may receive, resolved once per stream.
///
/// Cross-user fix (2026-07-28) — the admin used to subscribe with NO filter, so the team view
/// streamed every tenant's answers/progress verbatim to the operator, on
/// exactly the projects `can_see_owner` refuses to even list. Both identities
/// now filter on the same visible-project set; they differ only in what to do
/// with an UNATTRIBUTED frame (no `slug`, e.g. a HITL prompt whose context
/// carries no project): the operator keeps seeing those, a tenant fails closed.
#[derive(Clone)]
struct EventAcl {
    visible: HashSet<String>,
    allow_unattributed: bool,
}

impl EventAcl {
    fn resolve(app: &AppState, identity: &Identity) -> Self {
        Self {
            visible: visible_project_slugs(app, identity).into_iter().collect(),
            allow_unattributed: identity.is_admin,
        }
    }
}

fn event_visible(ev: &GatewayEvent, acl: &EventAcl) -> bool {
    match ev.slug.as_deref() {
        Some(slug) => acl.visible.contains(slug),
        None => acl.allow_unattributed,
    }
}

/// Render one global-ring entry as the SSE frame `useAgentsEvents` consumes:
/// event name `"delegation"` for a delegation lifecycle transition, else the
/// existing per-sid `"progress"` name (reusing
/// [`super::sessions_api::session_event_payload`] for the JSON body — same
/// shape the per-sid stream sends, now carrying `slug`).
fn agents_event(ev: &GatewayEvent, seq: u64) -> Event {
    let event_name = match ev.kind {
        ccteam_im::gateway::GatewayEventKind::Delegation { .. } => "delegation",
        ccteam_im::gateway::GatewayEventKind::SessionLifecycle { .. } => "session_lifecycle",
        _ => "progress",
    };
    Event::default()
        .id(seq.to_string())
        .event(event_name)
        .data(super::sessions_api::session_event_payload(ev).to_string())
}

/// `GET /api/v1/agents/events`
///
/// Global SSE for the team view: every session's `Progress`/`Activity`/
/// `Answer` events PLUS every delegation lifecycle transition, across every
/// visible project. Same replay contract as the per-sid stream (`GET
/// /api/v1/sessions/{sid}/events`): a 256-frame ring keyed by
/// `Last-Event-ID` (header or `?last_event_id=` query), 15s keep-alive, a
/// `reconnect_hint` frame on `Lagged`. No-gateway emits one
/// `gateway_unavailable` frame then keep-alives (never 503 — an
/// `EventSource` would retry-loop on that).
#[utoipa::path(
    get,
    path = "/api/v1/agents/events",
    tag = "agents",
    params(
        ("last_event_id" = Option<String>, Query, description = "Reconnect watermark (query fallback for the `Last-Event-ID` header)"),
    ),
    responses(
        (status = 200, description = "SSE stream (text/event-stream). Frames: `event: progress` (answer/progress/activity, `data` per session_event_payload) and `event: delegation` (a delegation lifecycle transition, `data` additionally carries relation/parent_sid/child_sid/title?/reason?).", content_type = "text/event-stream"),
    ),
)]
pub(crate) async fn handle_agents_events(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<SessionEventsQuery>,
    headers: HeaderMap,
) -> Response {
    let last_id = parse_last_event_id(&headers, &query);
    let rx = app.gateway.as_ref().map(|_| app.global_ring.subscribe());
    let visible = EventAcl::resolve(&app, &identity);
    let stream = match rx {
        Some(rx) => {
            let catchup: Vec<Event> = match last_id {
                Some(since) => app
                    .global_ring
                    .replay_since(since)
                    .into_iter()
                    .filter(|entry| event_visible(&entry.event, &visible))
                    .map(|entry| agents_event(&entry.event, entry.seq))
                    .collect(),
                None => Vec::new(),
            };
            let tap_visible = visible.clone();
            futures::stream::iter(catchup.into_iter().map(Ok::<Event, Infallible>))
                .chain(BroadcastStream::new(rx).filter_map(move |item| {
                    let visible = tap_visible.clone();
                    async move {
                        match item {
                            Ok(entry) if event_visible(&entry.event, &visible) => {
                                Some(Ok(agents_event(&entry.event, entry.seq)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(
        dir: &std::path::Path,
        sid: &str,
        role: &str,
        vendor: ccteam_harness::AgentVendor,
        parent_sid: Option<&str>,
        depth: u32,
    ) {
        let mut m = ccteam_harness::SessionMeta {
            mode: None,
            managed_by: Default::default(),
            sid: sid.to_string(),
            slug: "demo".to_string(),
            vendor,
            protocol: ccteam_harness::SessionProtocol::StreamJson,
            role: role.to_string(),
            permission_mode: ccteam_harness::PermissionMode::Skip,
            owner: "user:web".to_string(),
            vendor_uuid: String::new(),
            model: None,
            observed_model: None,
            effort: None,
            host: String::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_active: "2026-01-01T00:00:00Z".to_string(),
            origin: ccteam_harness::SessionOrigin::Ccteam,
            title: None,
            title_source: None,
            turn_count: 0,
            cost_usd: None,
            tokens_total: None,
            role_sha: None,
            skills_sha: None,
            trigger: None,
            parent_sid: parent_sid.map(str::to_string),
            spawned_by_role: None,
            delegation_depth: depth,
        };
        m.sid = sid.to_string();
        ccteam_harness::write_session_meta(dir, &m).unwrap();
    }

    #[test]
    fn build_agents_graph_derives_edges_from_parent_sid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        meta(
            &dir,
            "s1",
            "brain",
            ccteam_harness::AgentVendor::Claude,
            None,
            0,
        );
        meta(
            &dir,
            "s2",
            "worker",
            ccteam_harness::AgentVendor::Grok,
            Some("s1"),
            1,
        );

        let live: HashMap<String, SessionView> = HashMap::new();
        let armed: HashSet<String> = ["s2".to_string()].into_iter().collect();
        let graph = build_agents_graph(
            |slug| {
                assert_eq!(slug, "demo");
                dir.clone()
            },
            &["demo".to_string()],
            &live,
            &HashMap::new(),
            &armed,
        );
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].parent, "s1");
        assert_eq!(graph.edges[0].child, "s2");
        assert!(graph.edges[0].active, "s2 has an armed watch");
        assert_eq!(graph.hosts, vec!["local".to_string()]);
        // Neither sid is in the live map ⇒ both idle.
        assert!(graph.nodes.iter().all(|n| n.status == "idle"));
    }

    #[test]
    fn build_agents_graph_marks_live_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        meta(
            &dir,
            "s1",
            "brain",
            ccteam_harness::AgentVendor::Claude,
            None,
            0,
        );
        let mut live: HashMap<String, SessionView> = HashMap::new();
        live.insert(
            "s1".to_string(),
            SessionView {
                driveable: true,
                sid: "s1".to_string(),
                project: "demo".to_string(),
                role: "brain".to_string(),
                vendor: "claude".to_string(),
                permission_mode: "skip".to_string(),
                protocol: "stream-json".to_string(),
                host: "local".to_string(),
                current: false,
                status: "live".to_string(),
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
        );
        let graph = build_agents_graph(
            |_| dir.clone(),
            &["demo".to_string()],
            &live,
            &HashMap::new(),
            &HashSet::new(),
        );
        assert_eq!(graph.nodes[0].status, "live");
    }

    /// TEAM-4 — `nodes[].model` + `nodes[].effort` come from the caller's
    /// post-lock statusline join (`live_status`), never from `meta.json`: the
    /// live sid carries its reported model/effort, the idle sid stays honestly
    /// `None`.
    #[test]
    fn build_agents_graph_joins_live_model_only_for_live_nodes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        meta(
            &dir,
            "s1",
            "brain",
            ccteam_harness::AgentVendor::Claude,
            None,
            0,
        );
        meta(
            &dir,
            "s2",
            "worker",
            ccteam_harness::AgentVendor::Grok,
            Some("s1"),
            1,
        );
        let mut live: HashMap<String, SessionView> = HashMap::new();
        live.insert(
            "s1".to_string(),
            SessionView {
                driveable: true,
                sid: "s1".to_string(),
                project: "demo".to_string(),
                role: "brain".to_string(),
                vendor: "claude".to_string(),
                permission_mode: "skip".to_string(),
                protocol: "stream-json".to_string(),
                host: "local".to_string(),
                current: false,
                status: "live".to_string(),
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
        );
        let live_status: HashMap<String, ThreadStatus> = [(
            "s1".to_string(),
            ThreadStatus {
                model: Some("fable-5".to_string()),
                effort: Some("high".to_string()),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();
        let graph = build_agents_graph(
            |_| dir.clone(),
            &["demo".to_string()],
            &live,
            &live_status,
            &HashSet::new(),
        );
        let by_sid = |sid: &str| graph.nodes.iter().find(|n| n.sid == sid).unwrap();
        assert_eq!(by_sid("s1").model.as_deref(), Some("fable-5"));
        assert_eq!(by_sid("s1").effort.as_deref(), Some("high"));
        assert_eq!(
            by_sid("s2").model,
            None,
            "an idle node has nothing live to report"
        );
        assert_eq!(by_sid("s2").effort, None);
    }

    #[test]
    fn sort_hosts_pins_local_first() {
        let hosts: HashSet<String> = ["zeta".to_string(), "local".to_string(), "alpha".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            sort_hosts(hosts),
            vec!["local".to_string(), "alpha".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn event_visible_admin_keeps_unattributed_but_not_a_tenants_project() {
        let mut ev = GatewayEvent {
            id: "e".into(),
            channel: String::new(),
            chat_id: String::new(),
            thread_ts: None,
            content: String::new(),
            kind: ccteam_im::gateway::GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: None,
            slug: None,
        };
        // The operator's ACL: its own visible projects + unattributed frames
        // (a HITL prompt carries no slug).
        let admin = EventAcl {
            visible: ["adminproj".to_string()].into_iter().collect(),
            allow_unattributed: true,
        };
        assert!(
            event_visible(&ev, &admin),
            "unattributed frames still shown"
        );
        ev.slug = Some("adminproj".to_string());
        assert!(event_visible(&ev, &admin));
        // Cross-user fix (2026-07-28) — but NOT a tenant's project. The admin used to subscribe
        // unfiltered, so the team view streamed every tenant's live answers.
        ev.slug = Some("tenantproj".to_string());
        assert!(
            !event_visible(&ev, &admin),
            "a tenant's session events must not reach the operator's team view"
        );
    }

    #[test]
    fn event_visible_tenant_fails_closed_on_missing_slug() {
        let ev = GatewayEvent {
            id: "e".into(),
            channel: String::new(),
            chat_id: String::new(),
            thread_ts: None,
            content: String::new(),
            kind: ccteam_im::gateway::GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: None,
            slug: None,
        };
        let visible = EventAcl {
            visible: ["demo".to_string()].into_iter().collect(),
            allow_unattributed: false,
        };
        assert!(!event_visible(&ev, &visible));
    }

    #[test]
    fn event_visible_tenant_matches_own_slug_only() {
        let mut ev = GatewayEvent {
            id: "e".into(),
            channel: String::new(),
            chat_id: String::new(),
            thread_ts: None,
            content: String::new(),
            kind: ccteam_im::gateway::GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: Vec::new(),
            sid: None,
            slug: Some("demo".to_string()),
        };
        let visible = EventAcl {
            visible: ["demo".to_string()].into_iter().collect(),
            allow_unattributed: false,
        };
        assert!(event_visible(&ev, &visible));
        ev.slug = Some("other".to_string());
        assert!(!event_visible(&ev, &visible));
    }
}
