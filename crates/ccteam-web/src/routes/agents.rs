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
//! [`ccteam_im::gateway::Gateway::armed_delegation_watch_sids`] (a
//! best-effort seed for `edges[].active`, corrected live by the SSE
//! `dispatched`/`completed` frames — see `AgentsView`'s reducer).
//!
//! **ACL**: admin sees every project; a tenant sees only the projects
//! [`crate::auth::Identity::can_see_owner`] allows (mirrors the `/projects`
//! collection filter, `api_v1::build_projects`) — both for the graph snapshot
//! AND for every SSE frame (a frame with no resolvable `slug` is dropped for
//! a tenant, fail-closed; only an admin sees it). `?slug=` narrows the graph
//! to one project (still gated by [`super::api_v1::can_see_project`]).
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
    /// Not tracked on `meta.json`/`SessionView` today (spawn-time model
    /// override isn't persisted) — always `null`. Reserved wire shape for
    /// when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub host: String,
    /// `"live"` (gateway-tracked) or `"idle"` (persisted, not tracked). See
    /// the module doc's status-honesty note.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_sid: Option<String>,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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
/// resolved under the gateway lock. Pure over its inputs (`project_dir` +
/// these maps) so it's unit-testable without a server.
pub(crate) fn build_agents_graph(
    project_dir_for: impl Fn(&str) -> std::path::PathBuf,
    slugs: &[String],
    live_by_sid: &HashMap<String, SessionView>,
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
            nodes.push(AgentNode {
                sid: m.sid.clone(),
                slug: slug.clone(),
                role: m.role.clone(),
                vendor: ccteam_im::compare::vendor_label(m.vendor).to_string(),
                model: None,
                host,
                status: status.to_string(),
                parent_sid: m.parent_sid.clone(),
                depth: m.delegation_depth,
                cost_usd: m.cost_usd,
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
    let (live_by_sid, armed_watches): (HashMap<String, SessionView>, HashSet<String>) = {
        let guard = gw.lock().await;
        let live = guard
            .session_views()
            .into_iter()
            .map(|v| (v.sid.clone(), v))
            .collect();
        let armed = guard.armed_delegation_watch_sids();
        (live, armed)
    };
    let paths = app.paths.clone();
    let graph = build_agents_graph(
        |slug| paths.project_dir(slug),
        &slugs,
        &live_by_sid,
        &armed_watches,
    );
    Json(graph).into_response()
}

/// Whether `ev` is visible to `identity` given its already-resolved visible
/// slug set (`None` for admin ⇒ everything visible; `Some(set)` for a tenant
/// ⇒ only a frame whose `slug` is in `set` — fail-closed: a frame with no
/// `slug` at all is dropped for a tenant).
fn event_visible(ev: &GatewayEvent, visible: &Option<HashSet<String>>) -> bool {
    match visible {
        None => true,
        Some(set) => ev.slug.as_deref().is_some_and(|s| set.contains(s)),
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
    let visible: Option<HashSet<String>> = if identity.is_admin {
        None
    } else {
        Some(visible_project_slugs(&app, &identity).into_iter().collect())
    };
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
            sid: sid.to_string(),
            slug: "demo".to_string(),
            vendor,
            protocol: ccteam_harness::SessionProtocol::StreamJson,
            role: role.to_string(),
            permission_mode: ccteam_harness::PermissionMode::Skip,
            owner: "user:web".to_string(),
            vendor_uuid: String::new(),
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
            compare_group: None,
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
                waiting_approval: false,
                parent_sid: None,
                delegation_depth: 0,
            },
        );
        let graph = build_agents_graph(
            |_| dir.clone(),
            &["demo".to_string()],
            &live,
            &HashSet::new(),
        );
        assert_eq!(graph.nodes[0].status, "live");
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
    fn event_visible_admin_sees_everything_including_no_slug() {
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
        assert!(event_visible(&ev, &None));
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
        let visible = Some(["demo".to_string()].into_iter().collect());
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
        let visible = Some(["demo".to_string()].into_iter().collect());
        assert!(event_visible(&ev, &visible));
        ev.slug = Some("other".to_string());
        assert!(!event_visible(&ev, &visible));
    }
}
