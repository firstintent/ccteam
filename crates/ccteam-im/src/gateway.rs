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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

/// In-memory v8.1 route table for one daemon process.
pub struct Gateway {
    adapter_factory:
        Arc<dyn Fn(AgentVendor) -> Arc<dyn HarnessAdapter + Send + Sync> + Send + Sync>,
    default_project: String,
    projects: BTreeMap<String, PathBuf>,
    current_project: BTreeMap<ChatKey, String>,
    current_session: BTreeMap<ChatKey, String>,
    sessions: BTreeMap<String, GatewaySession>,
    next_session: u64,
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
            projects,
            current_project: BTreeMap::new(),
            current_session: BTreeMap::new(),
            sessions: BTreeMap::new(),
            next_session: 0,
        }
    }

    /// Register or update a project root addressable by `/cd <slug>`.
    pub fn register_project(&mut self, slug: impl Into<String>, dir: impl Into<PathBuf>) {
        self.projects.insert(slug.into(), dir.into());
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
            "/new" => {
                let vendor = parse_vendor(parts.next().unwrap_or("claude"))?;
                let role = parts.next().unwrap_or("assistant").to_string();
                let project = self.current_project_for(chat);
                let session_id = self
                    .start_session(chat.clone(), project, vendor, role)
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
        let project = self.current_project_for(chat);
        self.start_session(
            chat.clone(),
            project,
            AgentVendor::Claude,
            "assistant".to_string(),
        )
        .await?;
        Ok(())
    }

    async fn start_session(
        &mut self,
        owner: ChatKey,
        project: String,
        vendor: AgentVendor,
        role: String,
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
        let handle = role.clone();
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
        Ok(id)
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
        let turn_id = session
            .adapter
            .submit_turn(&session.thread, TurnInput::UserText(payload))
            .await?;
        let mut events = session.adapter.events(&session.thread);
        let mut replies = Vec::new();
        while let Ok(Some(evt)) =
            tokio::time::timeout(std::time::Duration::from_millis(5), events.next()).await
        {
            if let Some(text) = event_text(&evt) {
                replies.push(text);
                break;
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

    #[derive(Debug, Default)]
    struct FakeAdapter {
        starts: AtomicUsize,
        submissions: Arc<Mutex<Vec<(String, String)>>>,
        events: Arc<Mutex<VecDeque<(String, ThreadEvent)>>>,
    }

    #[async_trait::async_trait]
    impl HarnessAdapter for FakeAdapter {
        fn name(&self) -> &'static str {
            "fake-gateway"
        }

        fn vendor(&self) -> AgentVendor {
            AgentVendor::Claude
        }

        async fn start_thread(
            &self,
            spec: &AgentSpecBrief,
            ctx: &SpawnCtx,
        ) -> Result<ThreadHandle, HarnessError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(ThreadHandle {
                vendor: AgentVendor::Claude,
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
            Box::pin(futures::stream::unfold((), move |_| {
                let events = Arc::clone(&events);
                let wanted = wanted.clone();
                async move {
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
}
