//! v8.1 IM gateway route table.
//!
//! This module owns the chat-local `project ⇄ session` state that sits
//! above the older `@handle -> mailbox` router. It is deliberately
//! daemon-agnostic: tests drive it with a fake [`HarnessAdapter`], and
//! the daemon can wire the same state machine into real transports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use ccteam_core::config::{upsert_project, ProjectEntry};
use ccteam_core::projects::{bootstrap_project_at_dir, validate_slug_format};
use ccteam_core::CcteamPaths;
use ccteam_harness::{
    chat_session_name, parse_chat_session_name, AgentSpecBrief, AgentVendor, HarnessAdapter,
    ProcessBackend, SpawnCtx, ThreadEvent, ThreadHandle, ThreadItemDetails, TurnInput,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::BotRegistration;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ChatKey {
    channel: String,
    chat_id: String,
    user_id: String,
}

impl ChatKey {
    fn new(channel: &str, chat_id: &str, user_id: &str) -> Self {
        Self {
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
        }
    }
}

#[derive(Clone)]
struct GatewaySession {
    id: String,
    owner: ChatKey,
    project: String,
    role: String,
    vendor: AgentVendor,
    handle: String,
    thread: ThreadHandle,
    adapter: Arc<dyn HarnessAdapter + Send + Sync>,
    visible_events: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct GatewayRouteTemplate {
    channel: String,
    chat_id: String,
    project: String,
    role: String,
    vendor: AgentVendor,
    handle: String,
}

/// In-memory v8.1 route table for one daemon process.
pub struct Gateway {
    adapter_factory:
        Arc<dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync>,
    default_project: String,
    state_path: Option<PathBuf>,
    projects: BTreeMap<String, PathBuf>,
    current_project: BTreeMap<ChatKey, String>,
    current_session: BTreeMap<ChatKey, String>,
    sessions: BTreeMap<String, GatewaySession>,
    templates: Vec<GatewayRouteTemplate>,
    next_session: u64,
    event_sink: Option<tokio::sync::mpsc::UnboundedSender<GatewayEvent>>,
    event_pumps: BTreeMap<String, tokio::task::JoinHandle<()>>,
    /// Path context for `/newproject` (scaffold + config-registry write).
    /// `None` in unit tests that don't exercise project creation; the
    /// daemon sets it via [`Gateway::enable_project_creation`].
    project_paths: Option<CcteamPaths>,
}

/// User-visible text emitted asynchronously from a harness event stream.
///
/// The daemon owns delivery: it maps `channel` to a live [`Channel`],
/// appends the durable outbound ledger row, and sends to `chat_id`.
#[derive(Debug, Clone)]
pub struct GatewayEvent {
    /// Stable outbound id prefix used by the durable ledger.
    pub id: String,
    /// IM channel name (`telegram`, `ws`, ...).
    pub channel: String,
    /// Platform chat/recipient id.
    pub chat_id: String,
    /// Optional platform thread id.
    pub thread_ts: Option<String>,
    /// User-visible message content.
    pub content: String,
}

/// A live `ccteam-chat-*` process with no matching tracked gateway session —
/// a survivor of a prior daemon. The process name carries only slug+role (not
/// the owning chat), so orphans are a global concern and are never attributed
/// to a single chat's `/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanSession {
    /// Full process/tmux session name (`ccteam-chat-<slug>-<role>`).
    pub name: String,
    /// Project slug parsed from the name.
    pub slug: String,
    /// Role parsed from the name.
    pub role: String,
}

/// Reconciliation of live chat-mode processes against this gateway's tracked
/// sessions. See [`Gateway::reconcile_chat_sessions`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionInventory {
    /// Live `ccteam-chat-*` names that map to a tracked gateway session.
    pub tracked: Vec<String>,
    /// Live `ccteam-chat-*` names with no tracked session (orphans).
    pub orphans: Vec<OrphanSession>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewayState {
    default_project: String,
    current_project: Vec<SavedGatewayRoute>,
    current_session: Vec<SavedGatewayRoute>,
    sessions: Vec<SavedGatewaySession>,
    next_session: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewayRoute {
    chat: ChatKey,
    value: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedGatewaySession {
    id: String,
    owner: ChatKey,
    project: String,
    role: String,
    vendor: AgentVendor,
    handle: String,
    thread: ThreadHandle,
}

impl Gateway {
    /// Create a gateway with one default project.
    pub fn new(
        adapter: Arc<dyn HarnessAdapter + Send + Sync>,
        default_project: impl Into<String>,
        default_dir: impl Into<PathBuf>,
    ) -> Self {
        Self::new_with_factory(
            {
                let adapter = Arc::clone(&adapter);
                Arc::new(move |_vendor| Arc::clone(&adapter))
            },
            default_project,
            default_dir,
        )
    }

    /// Create a gateway with per-vendor adapter selection.
    pub fn new_with_factory(
        adapter_factory: Arc<
            dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync,
        >,
        default_project: impl Into<String>,
        default_dir: impl Into<PathBuf>,
    ) -> Self {
        let default_project = default_project.into();
        let mut projects = BTreeMap::new();
        projects.insert(default_project.clone(), default_dir.into());
        Self {
            adapter_factory,
            default_project,
            state_path: None,
            projects,
            current_project: BTreeMap::new(),
            current_session: BTreeMap::new(),
            sessions: BTreeMap::new(),
            templates: Vec::new(),
            next_session: 0,
            event_sink: None,
            event_pumps: BTreeMap::new(),
            project_paths: None,
        }
    }

    /// Enable async delivery of [`HarnessAdapter::events`] back to IM.
    ///
    /// When enabled, `handle_text` returns a quick submit ACK and the
    /// daemon sends later assistant/error events via this sink. Calling
    /// this after `enable_persistence` also re-subscribes restored
    /// sessions, which is the daemon-restart path.
    pub fn set_event_sink(&mut self, tx: tokio::sync::mpsc::UnboundedSender<GatewayEvent>) {
        self.event_sink = Some(tx);
        let ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            self.spawn_event_pump(&id);
        }
    }

    /// Enable `/newproject <slug> <path>` by giving the gateway the path
    /// context it needs to scaffold + register a project. The daemon
    /// wires this; unit tests that don't create projects leave it unset
    /// (the command then reports it's unavailable).
    pub fn enable_project_creation(&mut self, paths: CcteamPaths) {
        self.project_paths = Some(paths);
    }

    /// Load and persist route/session state at `path`.
    ///
    /// The daemon uses this for v8.1 spawn-on-demand continuity across
    /// restarts. Unit tests keep the default in-memory mode.
    pub fn enable_persistence(&mut self, path: impl Into<PathBuf>) -> Result<()> {
        self.state_path = Some(path.into());
        self.load_state()
    }

    /// Reconnect persisted sessions after daemon restart.
    ///
    /// Claude TUI sessions first use the live tmux `resume_thread`
    /// path, then merge persisted transcript-tail context back in.
    /// If the pane is gone, real Claude handles fall through to
    /// `start_thread` so the adapter can reattach/recreate. Codex
    /// app-server sessions use the native `thread/resume` RPC.
    pub async fn resume_restored_sessions(&mut self) {
        let ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let Some(snapshot) = self.sessions.get(&id).cloned() else {
                continue;
            };
            let Some(cwd) = self.projects.get(&snapshot.project).cloned() else {
                tracing::warn!(
                    session = %id,
                    project = %snapshot.project,
                    "ccteam-im: restored gateway session skipped; project root missing"
                );
                continue;
            };
            let adapter = (self.adapter_factory)(snapshot.vendor);
            let resumed = match snapshot.vendor {
                AgentVendor::Claude => {
                    match adapter.resume_thread(&snapshot.thread.identity).await {
                        Ok(mut thread) => {
                            thread.raw_extras = merge_thread_extras(
                                snapshot.thread.raw_extras.clone(),
                                thread.raw_extras,
                            );
                            Ok(thread)
                        }
                        Err(err) if is_real_claude_tui_handle(&snapshot.thread) => {
                            tracing::warn!(
                                session = %id,
                                error = %err,
                                "ccteam-im: Claude restored-session resume failed; trying start_thread reattach/recreate"
                            );
                            adapter
                                .start_thread(
                                    &AgentSpecBrief {
                                        role: snapshot.role.clone(),
                                    },
                                    &SpawnCtx {
                                        slug: snapshot.project.clone(),
                                        sid: snapshot.id.clone(),
                                        cwd: cwd.clone(),
                                        project_dir: cwd,
                                        extra_args: vec![],
                                        model_id: None,
                                    },
                                )
                                .await
                        }
                        Err(err) => Err(err),
                    }
                }
                AgentVendor::Codex => adapter.resume_thread(&snapshot.thread.identity).await,
            };
            match resumed {
                Ok(thread) => {
                    if let Some(session) = self.sessions.get_mut(&id) {
                        session.thread = thread;
                        session.adapter = adapter;
                        session.visible_events = Arc::new(AtomicU64::new(0));
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        session = %id,
                        vendor = ?snapshot.vendor,
                        error = %err,
                        "ccteam-im: restored gateway session resume failed; keeping persisted handle"
                    );
                }
            }
        }
        if let Err(err) = self.persist_state() {
            tracing::warn!(
                error = %err,
                "ccteam-im: failed to persist resumed gateway sessions"
            );
        }
    }

    /// Register or update a project root addressable by `/cd <slug>`.
    pub fn register_project(&mut self, slug: impl Into<String>, dir: impl Into<PathBuf>) {
        self.projects.insert(slug.into(), dir.into());
    }

    /// Register a persisted bot as a spawn-on-demand gateway session template.
    pub fn register_bot_template(
        &mut self,
        bot: &BotRegistration,
        project_dir: impl Into<PathBuf>,
    ) {
        self.register_project(bot.workflow_slug.clone(), project_dir);
        let template = GatewayRouteTemplate {
            channel: bot.im_platform.clone(),
            chat_id: bot.im_chat_id.clone(),
            project: bot.workflow_slug.clone(),
            role: bot.role.clone(),
            vendor: bot.vendor,
            handle: bot.effective_handle().to_string(),
        };
        if let Some(existing) = self.templates.iter_mut().find(|entry| {
            entry.channel == template.channel
                && entry.chat_id == template.chat_id
                && entry.project == template.project
                && entry.role == template.role
        }) {
            *existing = template;
        } else {
            self.templates.push(template);
        }
    }

    /// True when `text` is one of the gateway-owned slash commands.
    pub fn is_gateway_command(text: &str) -> bool {
        matches!(
            text.split_whitespace().next(),
            Some("/pair" | "/new" | "/use" | "/cd" | "/sessions" | "/projects" | "/newproject")
        )
    }

    /// True when this chat/user already has a current gateway session.
    pub fn has_current_session(&self, channel: &str, chat_id: &str, user_id: &str) -> bool {
        self.current_session
            .contains_key(&ChatKey::new(channel, chat_id, user_id))
    }

    /// Route one inbound text message and return outbound replies.
    pub async fn handle_text(
        &mut self,
        channel: &str,
        chat_id: &str,
        user_id: &str,
        text: &str,
    ) -> Result<Vec<String>> {
        let chat = ChatKey::new(channel, chat_id, user_id);
        if let Some(reply) = self.handle_command(&chat, text).await? {
            return Ok(vec![reply]);
        }
        if let Some((handle, payload)) = crate::router::parse_first_mention(text) {
            if let Some(session_id) = self.session_by_handle(&chat, &handle) {
                self.current_session.insert(chat.clone(), session_id);
                if payload.is_empty() {
                    return Ok(vec![format!("using @{handle}")]);
                }
                return self.submit_to_current(&chat, payload).await;
            }
            if let Some(template) = self.template_by_handle(&chat, &handle) {
                let session_id = self.start_template_session(chat.clone(), template).await?;
                self.current_session.insert(chat.clone(), session_id);
                if payload.is_empty() {
                    return Ok(vec![format!("using @{handle}")]);
                }
                return self.submit_to_current(&chat, payload).await;
            }
        }
        let templates = self.templates_for_chat(&chat);
        if templates.len() > 1 {
            let mut handles: Vec<String> = templates.iter().map(|t| t.handle.clone()).collect();
            handles.sort();
            handles.dedup();
            return Ok(vec![crate::inbound::format_ambiguous_dm_reply(&handles)]);
        }
        self.ensure_current_session(&chat).await?;
        self.submit_to_current(&chat, text.to_string()).await
    }

    async fn handle_command(&mut self, chat: &ChatKey, text: &str) -> Result<Option<String>> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return Ok(None);
        }
        let mut parts = trimmed.split_whitespace();
        let cmd = parts.next().unwrap_or_default();
        match cmd {
            "/pair" => {
                let code = parts
                    .next()
                    .ok_or_else(|| anyhow!("/pair requires a code"))?;
                self.ensure_current_session(chat).await?;
                self.persist_state()?;
                Ok(Some(format!("paired {code}")))
            }
            "/new" => {
                let vendor = parse_vendor(parts.next().unwrap_or("claude"))?;
                let role = parts.next().unwrap_or("assistant").to_string();
                let project = self.current_project_for(chat);
                let handle = role.clone();
                let session_id = self
                    .start_session(chat.clone(), project, vendor, role, handle)
                    .await?;
                Ok(Some(format!("created session {session_id}")))
            }
            "/use" => {
                let id = parts
                    .next()
                    .ok_or_else(|| anyhow!("/use requires a session id"))?;
                let session = self
                    .sessions
                    .get(id)
                    .filter(|s| s.owner == *chat)
                    .ok_or_else(|| anyhow!("unknown session for this chat: {id}"))?;
                self.current_session
                    .insert(chat.clone(), session.id.clone());
                self.persist_state()?;
                Ok(Some(format!("using session {}", session.id)))
            }
            "/cd" => {
                let project = parts
                    .next()
                    .ok_or_else(|| anyhow!("/cd requires a project"))?;
                if !self.projects.contains_key(project) {
                    return Err(anyhow!("unknown project: {project}"));
                }
                self.current_project
                    .insert(chat.clone(), project.to_string());
                // The active session must follow the project switch, otherwise
                // messages keep landing in the previous project's session while
                // the receipt claims we moved. Adopt an existing session owned by
                // this chat in the target project (deterministic: smallest id);
                // otherwise clear the active session so the next message spawns
                // one on demand in the target project via `ensure_current_session`.
                let adopted = self.adopt_session_in_project(chat, project);
                self.persist_state()?;
                Ok(Some(match adopted {
                    Some(sid) => format!("project set to {project} (switched to {sid})"),
                    None => {
                        format!("project set to {project} (next message starts a session there)")
                    }
                }))
            }
            "/newproject" => {
                // `/newproject <slug> <path>` — the path is the remainder
                // of the line so it may contain spaces. Splitting on the
                // first two whitespace runs keeps the path intact.
                let mut it = trimmed.splitn(3, char::is_whitespace);
                let _cmd = it.next();
                let slug = it
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow!("用法: /newproject <slug> <项目路径>"))?;
                let path = it
                    .next()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| anyhow!("用法: /newproject <slug> <项目路径>"))?;
                self.create_project(slug, path).map(Some)
            }
            "/sessions" => Ok(Some(self.render_sessions(chat))),
            "/projects" => Ok(Some(self.render_projects())),
            _ => Ok(None),
        }
    }

    /// Scaffold a ccteam project at `raw_path`, register it in
    /// `config.yaml`, and make it addressable by `/cd <slug>` in this
    /// running daemon. `raw_path` may be `~`-relative; it must resolve to
    /// an absolute directory (existing repos are adopted in place, empty
    /// dirs are created — `bootstrap_project_at_dir` leaves user files
    /// alone). Requires [`Gateway::enable_project_creation`].
    fn create_project(&mut self, slug: &str, raw_path: &str) -> Result<String> {
        let paths = self
            .project_paths
            .clone()
            .ok_or_else(|| anyhow!("project creation is not configured on this daemon"))?;
        let slug = validate_slug_format(slug)?;
        if self.projects.contains_key(&slug) {
            return Err(anyhow!("project already exists: {slug}"));
        }
        let abs = expand_project_path(raw_path)?;
        bootstrap_project_at_dir(&paths, &abs, &slug, "(created from web/IM chat)", "dev")
            .with_context(|| format!("scaffold project {slug} at {}", abs.display()))?;
        upsert_project(
            &paths.root,
            ProjectEntry {
                slug: slug.clone(),
                path: abs.clone(),
                team: "dev".to_string(),
                installed_at: chrono::Utc::now(),
            },
        )
        .with_context(|| format!("register project {slug} in config.yaml"))?;
        self.register_project(slug.clone(), abs.clone());
        if let Err(err) = self.persist_state() {
            tracing::warn!(error = %err, "ccteam-im: persist after /newproject failed");
        }
        Ok(format!("created project {slug} at {}", abs.display()))
    }

    async fn ensure_current_session(&mut self, chat: &ChatKey) -> Result<()> {
        if self.current_session.contains_key(chat) {
            return Ok(());
        }
        let templates = self.templates_for_chat(chat);
        if templates.len() == 1 {
            // A single registered bot template spawns on demand — UNLESS the
            // user explicitly `/cd`'d to a different project. An explicit `/cd`
            // target wins over the template so the project switch is honoured:
            // fall through to a generic assistant in the requested project
            // rather than silently dragging the message back into the bot's
            // project. (Tradeoff: the bot's role/vendor are not reused once you
            // `/cd` off its project.)
            let template = &templates[0];
            let cd_elsewhere = self
                .current_project
                .get(chat)
                .is_some_and(|p| *p != template.project);
            if !cd_elsewhere {
                self.start_template_session(chat.clone(), template.clone())
                    .await?;
                return Ok(());
            }
        }
        if templates.len() > 1 {
            let mut handles: Vec<String> = templates.iter().map(|t| t.handle.clone()).collect();
            handles.sort();
            handles.dedup();
            return Err(anyhow!(crate::inbound::format_ambiguous_dm_reply(&handles)));
        }
        let project = self.current_project_for(chat);
        self.start_session(
            chat.clone(),
            project,
            AgentVendor::Claude,
            "assistant".to_string(),
            "assistant".to_string(),
        )
        .await?;
        Ok(())
    }

    async fn start_template_session(
        &mut self,
        owner: ChatKey,
        template: GatewayRouteTemplate,
    ) -> Result<String> {
        self.current_project
            .insert(owner.clone(), template.project.clone());
        self.start_session(
            owner,
            template.project,
            template.vendor,
            template.role,
            template.handle,
        )
        .await
    }

    async fn start_session(
        &mut self,
        owner: ChatKey,
        project: String,
        vendor: AgentVendor,
        role: String,
        handle: String,
    ) -> Result<String> {
        self.next_session += 1;
        let id = format!("s{}", self.next_session);
        let cwd = self
            .projects
            .get(&project)
            .cloned()
            .ok_or_else(|| anyhow!("unknown project: {project}"))?;
        let adapter = (self.adapter_factory)(vendor);
        let thread = adapter
            .start_thread(
                &AgentSpecBrief { role: role.clone() },
                &SpawnCtx {
                    slug: project.clone(),
                    sid: id.clone(),
                    cwd: cwd.clone(),
                    project_dir: cwd,
                    extra_args: vec![],
                    model_id: None,
                },
            )
            .await?;
        self.sessions.insert(
            id.clone(),
            GatewaySession {
                id: id.clone(),
                owner: owner.clone(),
                project,
                role,
                vendor,
                handle,
                thread,
                adapter,
                visible_events: Arc::new(AtomicU64::new(0)),
            },
        );
        self.current_session.insert(owner, id.clone());
        self.persist_state()?;
        self.spawn_event_pump(&id);
        Ok(id)
    }

    fn spawn_event_pump(&mut self, session_id: &str) {
        if self.event_pumps.contains_key(session_id) {
            return;
        }
        let Some(tx) = self.event_sink.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(session_id).cloned() else {
            return;
        };
        let session_id = session.id.clone();
        let pump_key = session_id.clone();
        let handle = tokio::spawn(async move {
            let mut events = session.adapter.events(&session.thread);
            let mut seq: u64 = 0;
            while let Some(evt) = events.next().await {
                let Some(text) = async_event_text(&evt) else {
                    continue;
                };
                seq = seq.saturating_add(1);
                session.visible_events.fetch_add(1, Ordering::SeqCst);
                if tx
                    .send(GatewayEvent {
                        id: format!("gateway-event-{session_id}-{seq}"),
                        channel: session.owner.channel.clone(),
                        chat_id: session.owner.chat_id.clone(),
                        thread_ts: None,
                        content: text,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.event_pumps.insert(pump_key, handle);
    }

    fn load_state(&mut self) -> Result<()> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(path)?;
        let saved: SavedGatewayState = serde_json::from_str(&raw)?;
        self.default_project = saved.default_project;
        self.current_project = saved
            .current_project
            .into_iter()
            .map(|route| (route.chat, route.value))
            .collect();
        self.current_session = saved
            .current_session
            .into_iter()
            .map(|route| (route.chat, route.value))
            .collect();
        self.next_session = saved.next_session;
        self.sessions.clear();
        for saved_session in saved.sessions {
            let adapter = (self.adapter_factory)(saved_session.vendor);
            self.sessions.insert(
                saved_session.id.clone(),
                GatewaySession {
                    id: saved_session.id,
                    owner: saved_session.owner,
                    project: saved_session.project,
                    role: saved_session.role,
                    vendor: saved_session.vendor,
                    handle: saved_session.handle,
                    thread: saved_session.thread,
                    adapter,
                    visible_events: Arc::new(AtomicU64::new(0)),
                },
            );
        }
        Ok(())
    }

    fn persist_state(&self) -> Result<()> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let saved = SavedGatewayState {
            default_project: self.default_project.clone(),
            current_project: self
                .current_project
                .iter()
                .map(|(chat, value)| SavedGatewayRoute {
                    chat: chat.clone(),
                    value: value.clone(),
                })
                .collect(),
            current_session: self
                .current_session
                .iter()
                .map(|(chat, value)| SavedGatewayRoute {
                    chat: chat.clone(),
                    value: value.clone(),
                })
                .collect(),
            sessions: self
                .sessions
                .values()
                .map(|session| SavedGatewaySession {
                    id: session.id.clone(),
                    owner: session.owner.clone(),
                    project: session.project.clone(),
                    role: session.role.clone(),
                    vendor: session.vendor,
                    handle: session.handle.clone(),
                    thread: session.thread.clone(),
                })
                .collect(),
            next_session: self.next_session,
        };
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    async fn submit_to_current(&self, chat: &ChatKey, payload: String) -> Result<Vec<String>> {
        let session_id = self
            .current_session
            .get(chat)
            .ok_or_else(|| anyhow!("no current session for chat"))?;
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("current session missing: {session_id}"))?;
        let start_visible_events = session.visible_events.load(Ordering::SeqCst);
        let submit_wait = gateway_submit_timeout_duration();
        let turn_id = tokio::time::timeout(submit_wait, async {
            session
                .adapter
                .submit_turn(
                    &session.thread,
                    turn_input_for_session(session.vendor, payload),
                )
                .await
        })
        .await
        .map_err(|_| anyhow!("submit timed out after {submit_wait:?} for {session_id}"))??;
        let mut replies = Vec::new();
        if let Some(tx) = self.event_sink.clone() {
            spawn_turn_timeout_watchdog(tx, session, start_visible_events, &turn_id.0);
        } else {
            let mut events = session.adapter.events(&session.thread);
            let wait = gateway_reply_wait_duration();
            while let Ok(Some(evt)) = tokio::time::timeout(wait, events.next()).await {
                if let Some(text) = event_text(&evt) {
                    replies.push(text);
                    break;
                }
            }
        }
        if replies.is_empty() {
            replies.push(format!("submitted {session_id} turn {}", turn_id.0));
        }
        Ok(replies)
    }

    fn current_project_for(&self, chat: &ChatKey) -> String {
        self.current_project
            .get(chat)
            .cloned()
            .unwrap_or_else(|| self.default_project.clone())
    }

    /// Point the chat's active session at an existing session owned by this
    /// chat in `project` (deterministic: smallest session index), returning its
    /// id. When none exists, clear the active session so the next message spawns
    /// one on demand in `project`. Backs `/cd` so the project switch is real.
    fn adopt_session_in_project(&mut self, chat: &ChatKey, project: &str) -> Option<String> {
        let adopted = self
            .sessions
            .values()
            .filter(|s| s.owner == *chat && s.project == project)
            .min_by_key(|s| session_index(&s.id))
            .map(|s| s.id.clone());
        match &adopted {
            Some(id) => {
                self.current_session.insert(chat.clone(), id.clone());
            }
            None => {
                self.current_session.remove(chat);
            }
        }
        adopted
    }

    fn session_by_handle(&self, chat: &ChatKey, handle: &str) -> Option<String> {
        self.sessions
            .values()
            .find(|s| s.owner == *chat && s.handle == handle)
            .map(|s| s.id.clone())
    }

    fn template_by_handle(&self, chat: &ChatKey, handle: &str) -> Option<GatewayRouteTemplate> {
        self.templates
            .iter()
            .find(|t| t.channel == chat.channel && t.chat_id == chat.chat_id && t.handle == handle)
            .cloned()
    }

    fn templates_for_chat(&self, chat: &ChatKey) -> Vec<GatewayRouteTemplate> {
        self.templates
            .iter()
            .filter(|t| t.channel == chat.channel && t.chat_id == chat.chat_id)
            .cloned()
            .collect()
    }

    fn render_sessions(&self, chat: &ChatKey) -> String {
        let rows: Vec<String> = self
            .sessions
            .values()
            .filter(|s| s.owner == *chat)
            .map(|s| format!("{}:{}:{:?}:{}", s.id, s.project, s.vendor, s.role))
            .collect();
        if rows.is_empty() {
            "no sessions".to_string()
        } else {
            rows.join("\n")
        }
    }

    fn render_projects(&self) -> String {
        self.projects.keys().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Reconcile live `ccteam-chat-*` process names against tracked sessions.
    ///
    /// A live name equal to some tracked session's canonical name
    /// ([`chat_session_name`]) is `tracked`; the rest are `orphans` — processes
    /// that outlived a prior daemon and were never recorded by this one.
    /// Matching is by *computed* canonical name (not by parsing the live name),
    /// so dash-containing slugs are unambiguous; parsing is only used to
    /// describe orphans for display.
    pub fn reconcile_chat_sessions(&self, live_chat_names: &[String]) -> SessionInventory {
        let tracked_names: std::collections::BTreeSet<String> = self
            .sessions
            .values()
            .map(|s| chat_session_name(&s.project, &s.role))
            .collect();
        // Bare-path call binds to the free function below, not this method.
        reconcile_chat_sessions(&tracked_names, live_chat_names)
    }

    /// Enumerate live chat sessions from `backend` and reconcile them against
    /// tracked sessions. Production entry for daemon startup / a global session
    /// view. Read-only — never kills (the "never auto-kill a long session"
    /// redline; reclaim stays an explicit, opt-in action).
    pub async fn inventory_via_backend(
        &self,
        backend: &dyn ProcessBackend,
    ) -> Result<SessionInventory> {
        let live = ccteam_harness::list_chat_sessions(backend).await?;
        Ok(self.reconcile_chat_sessions(&live))
    }

    /// Render a global session inventory for an operator: every tracked session
    /// (`id:project:vendor:role`) plus any orphaned `ccteam-chat-*` processes,
    /// each flagged for explicit reclaim. Global (not per-chat): orphan names
    /// don't carry an owning chat, so this is intentionally not part of the
    /// per-chat `/sessions` view.
    pub fn render_all_sessions(&self, live_chat_names: &[String]) -> String {
        let inventory = self.reconcile_chat_sessions(live_chat_names);
        let mut lines: Vec<String> = self
            .sessions
            .values()
            .map(|s| format!("{}:{}:{:?}:{}", s.id, s.project, s.vendor, s.role))
            .collect();
        lines.sort();
        for orphan in &inventory.orphans {
            lines.push(format!(
                "orphan {} (slug={} role={}) — untracked, reclaim explicitly",
                orphan.name, orphan.slug, orphan.role
            ));
        }
        if lines.is_empty() {
            "no sessions".to_string()
        } else {
            lines.join("\n")
        }
    }
}

/// Reconcile live `ccteam-chat-*` process names against a set of *tracked*
/// canonical session names. A live name present in `tracked_names` is
/// `tracked`; any other (parseable) live name is an `orphan` — a process that
/// outlived the daemon that spawned it. Matching is by the *computed* canonical
/// name, so dash-containing slugs stay unambiguous; the live name is only
/// parsed to describe an orphan for display.
///
/// This is the daemon-independent core behind [`Gateway::reconcile_chat_sessions`].
/// The read-only `ccteam sessions` CLI view calls it directly, passing tracked
/// names loaded from the persisted registry via [`tracked_chat_session_names`].
pub fn reconcile_chat_sessions(
    tracked_names: &std::collections::BTreeSet<String>,
    live_chat_names: &[String],
) -> SessionInventory {
    let mut inventory = SessionInventory::default();
    for name in live_chat_names {
        if tracked_names.contains(name) {
            inventory.tracked.push(name.clone());
        } else if let Some((slug, role)) = parse_chat_session_name(name) {
            inventory.orphans.push(OrphanSession {
                name: name.clone(),
                slug,
                role,
            });
        }
    }
    inventory.tracked.sort();
    inventory.tracked.dedup();
    inventory.orphans.sort_by(|a, b| a.name.cmp(&b.name));
    inventory
}

/// Load the set of canonical chat-session names (`ccteam-chat-<slug>-<role>`)
/// the gateway has tracked, from its persisted route table at `state_path`
/// (see [`default_gateway_state_path`](crate::default_gateway_state_path)).
///
/// Returns an empty set when the file is absent — no daemon has persisted a
/// registry yet, so every live chat session is by definition an orphan. This
/// is the daemon-independent registry source the `ccteam sessions` CLI view
/// reconciles against; it is strictly read-only and never mutates the file.
pub fn tracked_chat_session_names(state_path: &Path) -> Result<std::collections::BTreeSet<String>> {
    if !state_path.exists() {
        return Ok(std::collections::BTreeSet::new());
    }
    let raw = std::fs::read_to_string(state_path)
        .with_context(|| format!("read gateway state {}", state_path.display()))?;
    let saved: SavedGatewayState = serde_json::from_str(&raw)
        .with_context(|| format!("parse gateway state {}", state_path.display()))?;
    Ok(saved
        .sessions
        .into_iter()
        .map(|s| chat_session_name(&s.project, &s.role))
        .collect())
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for (_, handle) in std::mem::take(&mut self.event_pumps) {
            handle.abort();
        }
    }
}

fn spawn_turn_timeout_watchdog(
    tx: tokio::sync::mpsc::UnboundedSender<GatewayEvent>,
    session: &GatewaySession,
    start_visible_events: u64,
    turn_id: &str,
) {
    let timeout = gateway_turn_timeout_duration();
    if timeout.is_zero() {
        return;
    }
    let visible_events = Arc::clone(&session.visible_events);
    let session_id = session.id.clone();
    let channel = session.owner.channel.clone();
    let chat_id = session.owner.chat_id.clone();
    let turn_id = turn_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        if visible_events.load(Ordering::SeqCst) != start_visible_events {
            return;
        }
        let _ = tx.send(GatewayEvent {
            id: format!("gateway-timeout-{session_id}-{turn_id}"),
            channel,
            chat_id,
            thread_ts: None,
            content: format!(
                "gateway error: turn timed out after {timeout:?} for {session_id} turn {turn_id}"
            ),
        });
    });
}

fn async_event_text(evt: &ThreadEvent) -> Option<String> {
    match evt {
        ThreadEvent::ItemCompleted { item } | ThreadEvent::ItemUpdated { item } => {
            match &item.details {
                ThreadItemDetails::AgentMessage(text) if !text.is_empty() => Some(text.clone()),
                _ => None,
            }
        }
        ThreadEvent::TurnFailed { err, .. } | ThreadEvent::Error(err) => Some(err.message.clone()),
        ThreadEvent::ThreadStarted { .. }
        | ThreadEvent::TurnStarted { .. }
        | ThreadEvent::TurnCompleted { .. }
        | ThreadEvent::ItemStarted { .. } => None,
    }
}

fn gateway_reply_wait_duration() -> std::time::Duration {
    const DEFAULT_MS: u64 = 5;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_REPLY_WAIT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn gateway_submit_timeout_duration() -> std::time::Duration {
    const DEFAULT_MS: u64 = 5_000;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn gateway_turn_timeout_duration() -> std::time::Duration {
    const DEFAULT_MS: u64 = 120_000;
    let ms = std::env::var("CCTEAM_IM_GATEWAY_TURN_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MS);
    std::time::Duration::from_millis(ms)
}

fn is_real_claude_tui_handle(thread: &ThreadHandle) -> bool {
    thread
        .raw_extras
        .get("tmux_session")
        .and_then(|v| v.as_str())
        .is_some()
        && thread
            .raw_extras
            .get("cwd")
            .and_then(|v| v.as_str())
            .is_some()
        && thread
            .raw_extras
            .get("project_dir")
            .and_then(|v| v.as_str())
            .is_some()
}

fn merge_thread_extras(
    persisted: serde_json::Value,
    resumed: serde_json::Value,
) -> serde_json::Value {
    let mut merged = persisted.as_object().cloned().unwrap_or_default();
    if let Some(resumed) = resumed.as_object() {
        for (key, value) in resumed {
            merged.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(merged)
}

fn turn_input_for_session(vendor: AgentVendor, payload: String) -> TurnInput {
    let trimmed = payload.trim();
    if let Some(command) = trimmed.strip_prefix('/') {
        let name = command.split_whitespace().next().unwrap_or_default();
        if vendor == AgentVendor::Claude || matches!(name, "compact" | "review") {
            return TurnInput::SystemDirective(command.to_string());
        }
    }
    TurnInput::UserText(payload)
}

fn parse_vendor(raw: &str) -> Result<AgentVendor> {
    match raw {
        "claude" => Ok(AgentVendor::Claude),
        "codex" => Ok(AgentVendor::Codex),
        other => Err(anyhow!("unknown vendor: {other}")),
    }
}

/// Resolve a chat-supplied project path: expand a leading `~`, then
/// require the result to be absolute (the daemon's cwd is not a
/// meaningful base for a path typed into a chat / web form).
fn expand_project_path(raw: &str) -> Result<PathBuf> {
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("cannot resolve home directory for ~"))?
            .join(rest)
    } else if raw == "~" {
        dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve home directory for ~"))?
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_absolute() {
        return Err(anyhow!("项目路径必须是绝对路径(或 ~ 开头): {raw}"));
    }
    Ok(expanded)
}

/// Numeric ordering key for a `s{n}` session id; unparseable ids sort last so
/// session adoption stays deterministic.
fn session_index(id: &str) -> u64 {
    id.strip_prefix('s')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

fn event_text(evt: &ThreadEvent) -> Option<String> {
    match evt {
        ThreadEvent::ItemCompleted { item } | ThreadEvent::ItemUpdated { item } => {
            match &item.details {
                ThreadItemDetails::AgentMessage(text) if !text.is_empty() => Some(text.clone()),
                _ => None,
            }
        }
        ThreadEvent::TurnCompleted { turn_id, .. } => Some(format!("turn completed {turn_id}")),
        ThreadEvent::TurnFailed { err, .. } | ThreadEvent::Error(err) => Some(err.message.clone()),
        ThreadEvent::ThreadStarted { .. }
        | ThreadEvent::TurnStarted { .. }
        | ThreadEvent::ItemStarted { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccteam_harness::{ExecutionMode, HarnessError, ThreadItem, TurnId};
    use futures::stream::BoxStream;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex as StdMutex, OnceLock};
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[derive(Debug)]
    struct FakeAdapter {
        vendor: AgentVendor,
        starts: AtomicUsize,
        submissions: Arc<Mutex<Vec<(String, String)>>>,
        events: Arc<Mutex<VecDeque<(String, ThreadEvent)>>>,
        event_delay: std::time::Duration,
    }

    impl Default for FakeAdapter {
        fn default() -> Self {
            Self::new(AgentVendor::Claude)
        }
    }

    impl FakeAdapter {
        fn new(vendor: AgentVendor) -> Self {
            Self {
                vendor,
                starts: AtomicUsize::new(0),
                submissions: Arc::new(Mutex::new(Vec::new())),
                events: Arc::new(Mutex::new(VecDeque::new())),
                event_delay: std::time::Duration::ZERO,
            }
        }

        fn new_with_event_delay(vendor: AgentVendor, event_delay: std::time::Duration) -> Self {
            Self {
                event_delay,
                ..Self::new(vendor)
            }
        }
    }

    #[async_trait::async_trait]
    impl HarnessAdapter for FakeAdapter {
        fn name(&self) -> &'static str {
            "fake-gateway"
        }

        fn vendor(&self) -> AgentVendor {
            self.vendor
        }

        async fn start_thread(
            &self,
            spec: &AgentSpecBrief,
            ctx: &SpawnCtx,
        ) -> Result<ThreadHandle, HarnessError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(ThreadHandle {
                vendor: self.vendor,
                mode: ExecutionMode::Chat,
                identity: format!("{}-{}-{}", ctx.slug, spec.role, ctx.sid),
                started_at: chrono::Utc::now(),
                raw_extras: serde_json::json!({}),
            })
        }

        async fn submit_turn(
            &self,
            h: &ThreadHandle,
            input: TurnInput,
        ) -> Result<TurnId, HarnessError> {
            let text = match input {
                TurnInput::UserText(text) => text,
                TurnInput::SystemDirective(directive) => format!("system:{directive}"),
                _ => String::new(),
            };
            self.submissions
                .lock()
                .await
                .push((h.identity.clone(), text.clone()));
            self.events.lock().await.push_back((
                h.identity.clone(),
                ThreadEvent::ItemCompleted {
                    item: ThreadItem {
                        id: "msg-1".to_string(),
                        details: ThreadItemDetails::AgentMessage(format!(
                            "{} echo: {text}",
                            h.identity
                        )),
                    },
                },
            ));
            Ok(TurnId::new(format!("turn-{}", h.identity)))
        }

        fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
            let events = Arc::clone(&self.events);
            let wanted = h.identity.clone();
            let delay = self.event_delay;
            Box::pin(futures::stream::unfold((), move |_| {
                let events = Arc::clone(&events);
                let wanted = wanted.clone();
                let delay = delay;
                async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let mut guard = events.lock().await;
                    let idx = guard.iter().position(|(thread, _)| thread == &wanted)?;
                    let (_, evt) = guard.remove(idx)?;
                    Some((evt, ()))
                }
            }))
        }

        async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
            Err(HarnessError::NotImplemented {
                reason: "fake".to_string(),
            })
        }

        async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn gateway_plain_message_submits_to_current_session_and_echoes() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let created = gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        assert_eq!(created, vec!["created session s1"]);

        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "hi")
            .await
            .unwrap();
        assert_eq!(replies, vec!["alpha-reviewer-s1 echo: hi"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            fake.submissions.lock().await.as_slice(),
            &[("alpha-reviewer-s1".to_string(), "hi".to_string())]
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn gateway_reply_wait_can_capture_realistic_delayed_event() {
        let _guard = env_lock();
        std::env::set_var("CCTEAM_IM_GATEWAY_REPLY_WAIT_MS", "100");
        let fake = Arc::new(FakeAdapter::new_with_event_delay(
            AgentVendor::Claude,
            std::time::Duration::from_millis(25),
        ));
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "hi after delay")
            .await
            .unwrap();
        std::env::remove_var("CCTEAM_IM_GATEWAY_REPLY_WAIT_MS");

        assert_eq!(replies, vec!["alpha-reviewer-s1 echo: hi after delay"]);
    }

    #[tokio::test]
    async fn gateway_pair_starts_default_session() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        let paired = gateway
            .handle_text("mock", "chat-1", "alice", "/pair 4821-77")
            .await
            .unwrap();
        assert_eq!(paired, vec!["paired 4821-77"]);

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "after pair")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-assistant-s1 echo: after pair"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gateway_commands_switch_project_and_session() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        let projects = gateway
            .handle_text("mock", "chat-1", "alice", "/projects")
            .await
            .unwrap();
        assert_eq!(projects, vec!["alpha\nbeta"]);

        let cd = gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        assert_eq!(
            cd,
            vec!["project set to beta (next message starts a session there)"]
        );

        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let sessions = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["s1:beta:Codex:api\ns2:beta:Claude:reviewer"]);

        let use_first = gateway
            .handle_text("mock", "chat-1", "alice", "/use s1")
            .await
            .unwrap();
        assert_eq!(use_first, vec!["using session s1"]);
        let replies = gateway
            .handle_text("mock", "chat-1", "alice", "ping")
            .await
            .unwrap();
        assert_eq!(replies, vec!["beta-api-s1 echo: ping"]);
    }

    #[tokio::test]
    async fn gateway_routes_two_projects_and_sessions_matrix() {
        let claude = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let codex = Arc::new(FakeAdapter::new(AgentVendor::Codex));
        let factory = {
            let claude = Arc::clone(&claude);
            let codex = Arc::clone(&codex);
            Arc::new(move |vendor| -> Arc<dyn HarnessAdapter + Send + Sync> {
                match vendor {
                    AgentVendor::Claude => claude.clone(),
                    AgentVendor::Codex => codex.clone(),
                }
            })
        };
        let mut gateway = Gateway::new_with_factory(factory, "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex docs")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude qa")
            .await
            .unwrap();

        let sessions = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            sessions,
            vec![
                "s1:alpha:Claude:reviewer\ns2:alpha:Codex:docs\ns3:beta:Codex:api\ns4:beta:Claude:qa"
            ]
        );
        let projects = gateway
            .handle_text("mock", "chat-1", "alice", "/projects")
            .await
            .unwrap();
        assert_eq!(projects, vec!["alpha\nbeta"]);

        let alpha_reply = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer alpha ping")
            .await
            .unwrap();
        assert_eq!(alpha_reply, vec!["alpha-reviewer-s1 echo: alpha ping"]);
        let beta_reply = gateway
            .handle_text("mock", "chat-1", "alice", "@api beta ping")
            .await
            .unwrap();
        assert_eq!(beta_reply, vec!["beta-api-s3 echo: beta ping"]);

        gateway
            .handle_text("mock", "chat-2", "bob", "/new claude reviewer")
            .await
            .unwrap();
        let isolated = gateway
            .handle_text("mock", "chat-2", "bob", "same text")
            .await
            .unwrap();
        assert_eq!(isolated, vec!["alpha-reviewer-s5 echo: same text"]);
    }

    #[tokio::test]
    async fn gateway_dual_vendor_sessions_route_slash_commands_by_vendor() {
        let claude = Arc::new(FakeAdapter::new(AgentVendor::Claude));
        let codex = Arc::new(FakeAdapter::new(AgentVendor::Codex));
        let factory = {
            let claude = Arc::clone(&claude);
            let codex = Arc::clone(&codex);
            Arc::new(move |vendor| -> Arc<dyn HarnessAdapter + Send + Sync> {
                match vendor {
                    AgentVendor::Claude => claude.clone(),
                    AgentVendor::Codex => codex.clone(),
                }
            })
        };
        let mut gateway = Gateway::new_with_factory(factory, "alpha", "/tmp/alpha");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();

        let compact = gateway
            .handle_text("mock", "chat-1", "alice", "/compact")
            .await
            .unwrap();
        assert_eq!(compact, vec!["alpha-api-s2 echo: system:compact"]);
        let review = gateway
            .handle_text("mock", "chat-1", "alice", "/review")
            .await
            .unwrap();
        assert_eq!(review, vec!["alpha-api-s2 echo: system:review"]);
        let claude_clear = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer /clear")
            .await
            .unwrap();
        assert_eq!(claude_clear, vec!["alpha-reviewer-s1 echo: system:clear"]);

        assert_eq!(
            codex.submissions.lock().await.as_slice(),
            &[
                ("alpha-api-s2".to_string(), "system:compact".to_string()),
                ("alpha-api-s2".to_string(), "system:review".to_string())
            ]
        );
        assert_eq!(
            claude.submissions.lock().await.as_slice(),
            &[("alpha-reviewer-s1".to_string(), "system:clear".to_string())]
        );
    }

    #[tokio::test]
    async fn gateway_at_bot_switches_session_without_cross_chat_leakage() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-2", "bob", "/new claude reviewer")
            .await
            .unwrap();

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer check this")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-reviewer-s1 echo: check this"]);

        let other = gateway
            .handle_text("mock", "chat-2", "bob", "same text")
            .await
            .unwrap();
        assert_eq!(other, vec!["alpha-reviewer-s3 echo: same text"]);
    }

    #[tokio::test]
    async fn gateway_persistence_restores_routes_and_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("gateway-state.json");
        let fake = Arc::new(FakeAdapter::default());

        {
            let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
            gateway.register_project("beta", "/tmp/beta");
            gateway.enable_persistence(&state_path).unwrap();
            gateway
                .handle_text("mock", "chat-1", "alice", "/cd beta")
                .await
                .unwrap();
            gateway
                .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
                .await
                .unwrap();
        }

        let mut restored = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        restored.register_project("beta", "/tmp/beta");
        restored.enable_persistence(&state_path).unwrap();

        let sessions = restored
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(sessions, vec!["s1:beta:Claude:reviewer"]);

        let reply = restored
            .handle_text("mock", "chat-1", "alice", "after restart")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta-reviewer-s1 echo: after restart"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reconcile_chat_sessions_free_fn_splits_tracked_and_orphans() {
        let tracked: std::collections::BTreeSet<String> =
            [ccteam_harness::chat_session_name("dev-foo", "alice")]
                .into_iter()
                .collect();
        let live = vec![
            ccteam_harness::chat_session_name("dev-foo", "alice"), // tracked
            ccteam_harness::chat_session_name("ghost-proj", "zombie"), // orphan
            "ccteam-chat-".to_string(),                            // unparseable → dropped
        ];
        let inv = reconcile_chat_sessions(&tracked, &live);
        assert_eq!(inv.tracked, vec!["ccteam-chat-dev-foo-alice".to_string()]);
        assert_eq!(inv.orphans.len(), 1);
        assert_eq!(inv.orphans[0].slug, "ghost-proj");
        assert_eq!(inv.orphans[0].role, "zombie");
        assert_eq!(inv.orphans[0].name, "ccteam-chat-ghost-proj-zombie");
    }

    #[test]
    fn tracked_chat_session_names_empty_when_state_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.json");
        assert!(tracked_chat_session_names(&missing).unwrap().is_empty());
    }

    #[tokio::test]
    async fn tracked_chat_session_names_reads_persisted_canonical_names() {
        let tmp = tempfile::tempdir().unwrap();
        let state_path = tmp.path().join("gateway-state.json");
        let fake = Arc::new(FakeAdapter::default());

        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");
        gateway.enable_persistence(&state_path).unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();

        let names = tracked_chat_session_names(&state_path).unwrap();
        assert!(
            names.contains("ccteam-chat-beta-reviewer"),
            "expected canonical chat-session name, got {names:?}"
        );
    }

    #[tokio::test]
    async fn gateway_registered_bot_template_spawns_on_demand() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "default", "/tmp/default");
        gateway.register_bot_template(
            &BotRegistration {
                workflow_slug: "alpha".to_string(),
                role: "lead".to_string(),
                vendor: AgentVendor::Claude,
                persona_id: None,
                im_platform: "mock".to_string(),
                im_chat_id: "chat-1".to_string(),
                chat_handle: None,
                project_dir: None,
                created_at: chrono::Utc::now(),
            },
            "/tmp/alpha",
        );

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();

        assert_eq!(reply, vec!["alpha-lead-s1 echo: hello"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gateway_registered_bot_templates_keep_ambiguous_dm_out_of_sessions() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "default", "/tmp/default");
        for role in ["lead", "reviewer"] {
            gateway.register_bot_template(
                &BotRegistration {
                    workflow_slug: format!("alpha-{role}"),
                    role: role.to_string(),
                    vendor: AgentVendor::Claude,
                    persona_id: None,
                    im_platform: "mock".to_string(),
                    im_chat_id: "chat-1".to_string(),
                    chat_handle: None,
                    project_dir: None,
                    created_at: chrono::Utc::now(),
                },
                format!("/tmp/alpha-{role}"),
            );
        }

        let ambiguous = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();
        assert_eq!(
            ambiguous,
            vec!["Multiple bots in this chat. Specify one: @lead @reviewer"]
        );

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "@reviewer hello")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-reviewer-reviewer-s1 echo: hello"]);
        assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gateway_cd_switches_active_session_to_target_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        // Active session s1 lives in project alpha.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        let before = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(before, vec!["s1:alpha:Claude:reviewer"]);

        // /cd to beta, where no session exists yet, clears the active session.
        let cd = gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        assert_eq!(
            cd,
            vec!["project set to beta (next message starts a session there)"]
        );

        // The next plain message must route into a beta session, not back s1.
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "where am i")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta-assistant-s2 echo: where am i"]);

        let after = gateway
            .handle_text("mock", "chat-1", "alice", "/sessions")
            .await
            .unwrap();
        assert_eq!(
            after,
            vec!["s1:alpha:Claude:reviewer\ns2:beta:Claude:assistant"]
        );
    }

    #[tokio::test]
    async fn gateway_cd_adopts_existing_session_in_target_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");
        gateway.register_project("beta", "/tmp/beta");

        // s1 in alpha; then /cd beta + /new makes s2 in beta.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude reviewer")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        gateway
            .handle_text("mock", "chat-1", "alice", "/new codex api")
            .await
            .unwrap();

        // /cd back to alpha must deterministically re-adopt the existing s1.
        let cd_back = gateway
            .handle_text("mock", "chat-1", "alice", "/cd alpha")
            .await
            .unwrap();
        assert_eq!(cd_back, vec!["project set to alpha (switched to s1)"]);

        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "ping")
            .await
            .unwrap();
        assert_eq!(reply, vec!["alpha-reviewer-s1 echo: ping"]);
    }

    #[tokio::test]
    async fn gateway_cd_overrides_single_template_project() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "default", "/tmp/default");
        gateway.register_project("beta", "/tmp/beta");
        gateway.register_bot_template(
            &BotRegistration {
                workflow_slug: "alpha".to_string(),
                role: "lead".to_string(),
                vendor: AgentVendor::Claude,
                persona_id: None,
                im_platform: "mock".to_string(),
                im_chat_id: "chat-1".to_string(),
                chat_handle: None,
                project_dir: None,
                created_at: chrono::Utc::now(),
            },
            "/tmp/alpha",
        );

        // /cd to a different project than the bot's: the explicit target wins,
        // so the next message spawns a generic assistant in beta, not the bot.
        gateway
            .handle_text("mock", "chat-1", "alice", "/cd beta")
            .await
            .unwrap();
        let reply = gateway
            .handle_text("mock", "chat-1", "alice", "hello")
            .await
            .unwrap();
        assert_eq!(reply, vec!["beta-assistant-s1 echo: hello"]);
    }

    #[tokio::test]
    async fn gateway_reconciles_orphan_chat_sessions() {
        use ccteam_harness::{InProcBackend, MuxSessionSpec};
        use std::path::PathBuf;

        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake.clone(), "alpha", "/tmp/alpha");

        // One tracked session: s1 = alpha/lead → ccteam-chat-alpha-lead.
        gateway
            .handle_text("mock", "chat-1", "alice", "/new claude lead")
            .await
            .unwrap();

        // Two live ccteam-chat-* processes injected via a fake ProcessBackend:
        // one matches the tracked session, the other is an orphan that outlived
        // a prior daemon (dashed slug to exercise the parser).
        let backend = InProcBackend::new();
        let spec =
            |name: &str| MuxSessionSpec::new(name, vec!["true".into()], PathBuf::from("/tmp"));
        backend
            .spawn(spec(&chat_session_name("alpha", "lead")))
            .await
            .unwrap();
        backend
            .spawn(spec("ccteam-chat-ghost-proj-zombie"))
            .await
            .unwrap();

        let inventory = gateway.inventory_via_backend(&backend).await.unwrap();
        assert_eq!(
            inventory.tracked,
            vec!["ccteam-chat-alpha-lead".to_string()]
        );
        assert_eq!(
            inventory.orphans,
            vec![OrphanSession {
                name: "ccteam-chat-ghost-proj-zombie".to_string(),
                slug: "ghost-proj".to_string(),
                role: "zombie".to_string(),
            }]
        );

        // The global display entry lists the tracked session and flags the orphan.
        let live = ccteam_harness::list_chat_sessions(&backend).await.unwrap();
        let rendered = gateway.render_all_sessions(&live);
        assert!(
            rendered.contains("s1:alpha:Claude:lead"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains("orphan ccteam-chat-ghost-proj-zombie (slug=ghost-proj role=zombie)"),
            "rendered: {rendered}"
        );
    }

    #[tokio::test]
    async fn gateway_newproject_validates_args_and_requires_path_context() {
        let fake = Arc::new(FakeAdapter::default());
        let mut gateway = Gateway::new(fake, "alpha", "/tmp/alpha");
        // Missing path → usage error (parsed before any path-context check).
        let usage = gateway
            .handle_text("mock", "chat-1", "alice", "/newproject demo")
            .await;
        assert!(format!("{:#}", usage.unwrap_err()).contains("用法"));
        // Valid args, but project creation is not configured on this gateway.
        let err = gateway
            .handle_text("mock", "chat-1", "alice", "/newproject demo /tmp/demo")
            .await
            .expect_err("expected not-configured error");
        assert!(format!("{err:#}").contains("not configured"));
        assert!(Gateway::is_gateway_command("/newproject demo /x"));
    }

    #[test]
    fn expand_project_path_requires_absolute_and_expands_tilde() {
        assert_eq!(
            expand_project_path("/srv/code/app").unwrap(),
            std::path::PathBuf::from("/srv/code/app")
        );
        assert!(expand_project_path("relative/dir").is_err());
        let home = expand_project_path("~/code/app").unwrap();
        assert!(home.is_absolute());
        assert!(home.ends_with("code/app"));
    }
}
