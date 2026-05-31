//! v8.1 IM gateway route table.
//!
//! This module owns the chat-local `project ⇄ session` state that sits
//! above the older `@handle -> mailbox` router. It is deliberately
//! daemon-agnostic: tests drive it with a fake [`HarnessAdapter`], and
//! the daemon can wire the same state machine into real transports.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use ccteam_harness::{
    AgentSpecBrief, AgentVendor, HarnessAdapter, SpawnCtx, ThreadEvent, ThreadHandle,
    ThreadItemDetails, TurnInput,
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

    /// Load and persist route/session state at `path`.
    ///
    /// The daemon uses this for v8.1 spawn-on-demand continuity across
    /// restarts. Unit tests keep the default in-memory mode.
    pub fn enable_persistence(&mut self, path: impl Into<PathBuf>) -> Result<()> {
        self.state_path = Some(path.into());
        self.load_state()
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
            Some("/pair" | "/new" | "/use" | "/cd" | "/sessions" | "/projects")
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
                self.persist_state()?;
                Ok(Some(format!("project set to {project}")))
            }
            "/sessions" => Ok(Some(self.render_sessions(chat))),
            "/projects" => Ok(Some(self.render_projects())),
            _ => Ok(None),
        }
    }

    async fn ensure_current_session(&mut self, chat: &ChatKey) -> Result<()> {
        if self.current_session.contains_key(chat) {
            return Ok(());
        }
        let templates = self.templates_for_chat(chat);
        if templates.len() == 1 {
            self.start_template_session(chat.clone(), templates[0].clone())
                .await?;
            return Ok(());
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
        if self.event_sink.is_none() {
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
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for (_, handle) in std::mem::take(&mut self.event_pumps) {
            handle.abort();
        }
    }
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
        assert_eq!(cd, vec!["project set to beta"]);

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
}
