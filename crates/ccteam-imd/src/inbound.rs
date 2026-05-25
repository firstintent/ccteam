//! Inbound pipeline: IM event → security check → router → bot mailbox.
//!
//! The daemon owns one [`tokio::sync::mpsc::Receiver<ChannelMessage>`]
//! per active Channel. Each received message runs through
//! [`process_inbound`] which composes three-layer security, the
//! router, and the per-bot mailbox writer.
//!
//! The actual `HarnessAdapter::submit_turn` call is performed by the
//! daemon's supervisor (it owns the active [`ccteam_core::harness::ThreadHandle`]
//! per bot); this module's responsibility ends at writing the
//! mailbox file the adapter watches.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::nl_admin::{self, AdminExecutor, AdminReply};
use crate::router::{
    self, available_handles_for_chat, format_unknown_handle_reply, HandleMap, Route,
};
use crate::three_layer_sec::{SecOutcome, ThreeLayerSec};
use crate::transport::{Channel, ChannelMessage, SendMessage};

/// One mailbox envelope dropped into
/// `<project>/.ccteam/chat/<bot>/inbox/msg-<ts>-<seq>.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEnvelope {
    /// IM platform name.
    pub platform: String,
    /// Sender id (post-ACL).
    pub sender: String,
    /// Hop counter (bot-to-bot loop guard).
    pub hop: u8,
    /// RFC3339 receipt timestamp.
    pub received_at: chrono::DateTime<chrono::Utc>,
    /// Where to send the reply.
    pub reply_target: String,
    /// Sanitized payload.
    pub payload: String,
    /// Original platform message id (echo suppression).
    pub message_id: String,
}

/// Result of one inbound processing pass.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundOutcome {
    /// Wrote a mailbox file for a bot session; daemon should kick the
    /// per-bot supervisor.
    DroppedToBot {
        /// project slug.
        slug: String,
        /// bot role.
        role: String,
        /// path of the written envelope.
        path: PathBuf,
        /// V0.6.1 fast-path — the router-stripped payload, carried out
        /// alongside the file path so the daemon's per-bot inbox
        /// dispatcher doesn't have to re-read + parse the envelope on
        /// every message. The on-disk envelope still holds the same
        /// payload for safety-net `drain_inboxes` recovery.
        payload: String,
        /// V0.6.1 fast-path — the originating IM platform's message id
        /// (e.g. `tg-{message_id}`). Used as the latency-log `cid` field
        /// in the downstream mpsc dispatcher.
        cid: String,
        /// V0.6.8 F193 — hop counter for the bot-to-bot loop guard.
        /// Echoes `process_inbound`'s `hop` argument so the downstream
        /// fast-path `InboxItem` carries the same value the on-disk
        /// `InboxEnvelope.hop` records.
        hop: u8,
    },
    /// Routed to admin handler.
    Admin {
        /// `@ccteam <verb_and_args>` payload.
        verb_and_args: String,
    },
    /// `@handle` parsed but unknown to the registry. The admin-aware
    /// wrapper replies to the originating chat with a helpful list of
    /// available bots.
    UnknownHandle {
        /// The handle the user typed (without the leading `@`).
        handle: String,
    },
    /// Dropped by router (no mention, hop budget exceeded).
    Dropped {
        /// reason.
        reason: String,
    },
    /// Rejected by the three-layer security.
    Rejected {
        /// which layer.
        layer: String,
    },
}

/// Per-bot mailbox path resolver. The daemon owns this; it's a
/// closure-style trait so tests can substitute a tempdir-based
/// implementation.
///
/// F185 — takes the full [`crate::BotRegistration`] so resolvers can
/// honor `reg.project_dir` (absolute path written at registration
/// time). Resolvers fall back to the historical
/// `<projects_root>/<workflow_slug>/.ccteam/chat/<role>/inbox/` layout
/// when `reg.project_dir = None` (legacy registrations).
pub trait MailboxResolver: Send + Sync {
    /// Return `<project>/.ccteam/chat/<role>/inbox/` for the given
    /// bot. The resolver decides how to interpret
    /// `reg.project_dir` vs the fallback `projects_root` layout.
    fn inbox_dir(&self, reg: &crate::BotRegistration) -> Result<PathBuf>;
}

/// Default resolver: rooted at the bot's chat dir
/// (`<project>/.ccteam/chat/<role>/inbox/`). Resolution honors the
/// three-tier F190 priority chain (see
/// [`crate::resolve_project_dir`]):
///
/// 1. `reg.project_dir` (F185 explicit field).
/// 2. `config_projects[reg.workflow_slug]` (F190 —
///    `~/.ccteam/config.yaml::projects[]` slug → path SoT). Daemon
///    loads this once at startup and passes it in via
///    [`Self::with_config_projects`]; legacy registrations without an
///    explicit `project_dir` route correctly even when the project
///    lives outside `~/projects/<slug>/` (NAS share, dir basename ≠
///    workflow slug).
/// 3. `<projects_root>/<workflow_slug>/` (historical layout).
///
/// `projects_root` is normally `~/projects` (production) but tests
/// pass an override via [`Self::with_projects_root`]. Tests that want
/// to assert the config-yaml tier construct with
/// [`Self::with_config_projects`].
pub struct DefaultMailboxResolver {
    projects_root: PathBuf,
    /// V0.6.8 F190 — slug → absolute project path map sourced from
    /// `~/.ccteam/config.yaml::projects[]`. Empty when the resolver was
    /// constructed without config (legacy code path / unit tests that
    /// don't exercise the F190 tier).
    config_projects: HashMap<String, PathBuf>,
}

impl DefaultMailboxResolver {
    /// Build pointing at `~/projects` (or `/` if HOME is unset). No
    /// config-yaml tier — callers that want F190 behavior should use
    /// [`Self::with_config_projects`].
    pub fn new() -> Self {
        let projects_root = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join("projects");
        Self {
            projects_root,
            config_projects: HashMap::new(),
        }
    }

    /// Build pointing at an explicit projects root (tests). No
    /// config-yaml tier — see [`Self::with_config_projects`].
    pub fn with_projects_root(projects_root: impl Into<PathBuf>) -> Self {
        Self {
            projects_root: projects_root.into(),
            config_projects: HashMap::new(),
        }
    }

    /// V0.6.8 F190 — build with both `projects_root` and the
    /// `~/.ccteam/config.yaml::projects[]` slug → path map. Daemon
    /// startup wires this so legacy registrations with
    /// `project_dir = None` resolve through the config-yaml tier
    /// before falling through to the projects_root tier.
    pub fn with_config_projects(
        projects_root: impl Into<PathBuf>,
        config_projects: HashMap<String, PathBuf>,
    ) -> Self {
        Self {
            projects_root: projects_root.into(),
            config_projects,
        }
    }
}

impl Default for DefaultMailboxResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl MailboxResolver for DefaultMailboxResolver {
    fn inbox_dir(&self, reg: &crate::BotRegistration) -> Result<PathBuf> {
        Ok(reg
            .chat_dir_with_config(&self.projects_root, &self.config_projects)
            .join("inbox"))
    }
}

/// V0.6.1 F135 — DM auto-route preprocessor.
///
/// Mutates `msg.content` in-place by prepending `@<role> ` when:
/// 1. The content has no existing `@<handle>` mention, AND
/// 2. Exactly one registered bot's `(im_platform, im_chat_id)` matches
///    `(msg.channel, msg.reply_target)`.
///
/// The router contract drops "no @mention" messages; without this
/// preprocessor a user DMing a single bot would have to type
/// `@<role> hi` to be heard, which defeats the "natural DM" UX
/// promise. The 2+ bot case (group with multiple bots sharing a
/// chat_id) still falls through to the router's drop path so explicit
/// @ mentions remain the disambiguator there.
///
/// Helper [`has_at_mention`] does the @ detection; kept public so
/// unit tests can probe the boundary cases (e.g. bare "@" with no
/// handle char).
pub fn auto_route_dm_mention(msg: &mut ChannelMessage, bots: &[crate::BotRegistration]) {
    if has_at_mention(&msg.content) {
        return;
    }
    let matches: Vec<&crate::BotRegistration> = bots
        .iter()
        .filter(|b| b.im_platform == msg.channel && b.im_chat_id == msg.reply_target)
        .collect();
    match matches.len() {
        1 => {
            let role = matches[0].role.clone();
            let new_content = format!("@{} {}", role, msg.content.trim_start());
            tracing::info!(
                slug = %matches[0].workflow_slug,
                role = %role,
                platform = %msg.channel,
                chat_id = %msg.reply_target,
                "imd: F135 DM auto-route prepended @{}",
                role
            );
            msg.content = new_content;
        }
        0 => {
            tracing::debug!(
                platform = %msg.channel,
                chat_id = %msg.reply_target,
                "imd: F135 DM auto-route skipped (no bot bound to this chat)"
            );
        }
        n => {
            tracing::debug!(
                count = n,
                platform = %msg.channel,
                chat_id = %msg.reply_target,
                "imd: F135 DM auto-route skipped (multiple bots share chat_id; group-style @ required)"
            );
        }
    }
}

/// V0.6.1 F135 — true iff `text` contains an `@` followed by at least
/// one alphanumeric / `_` / `-` char. Mirrors the router's
/// `parse_first_mention` handle-char rules so we don't double-@ on
/// content that already starts with a mention.
pub fn has_at_mention(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '@' {
            if let Some(&next_ch) = chars.peek() {
                if next_ch.is_ascii_alphanumeric() || next_ch == '_' || next_ch == '-' {
                    return true;
                }
            }
        }
    }
    false
}

/// Process one inbound IM event end-to-end.
///
/// `bots` is the registry slice the router resolves against; F185 uses
/// it to look up the [`crate::BotRegistration`] matching the routed
/// `(slug, role)` so the mailbox resolver can honor the bot's
/// `project_dir`. When no matching registration is found (race
/// window: registration removed between route resolution and mailbox
/// resolution), we synthesize a transient `BotRegistration` with
/// `project_dir = None` so the fallback layout still works — better
/// than erroring out the inbound pipeline.
pub async fn process_inbound(
    msg: &ChannelMessage,
    sec: &Arc<Mutex<ThreeLayerSec>>,
    handles: &HandleMap,
    bots: &[crate::BotRegistration],
    mailbox: &dyn MailboxResolver,
    hop: u8,
    seq: u64,
) -> Result<InboundOutcome> {
    // Layer 1 + 2 + 3 (ACL → rate limit → sanitize).
    let outcome = {
        let mut s = sec.lock().await;
        s.evaluate(&msg.channel, &msg.sender, &msg.content)
    };
    let payload = match outcome {
        SecOutcome::Accept { payload } => payload,
        SecOutcome::AclDenied => {
            return Ok(InboundOutcome::Rejected {
                layer: "acl".into(),
            })
        }
        SecOutcome::RateLimited => {
            return Ok(InboundOutcome::Rejected {
                layer: "rate_limit".into(),
            })
        }
        SecOutcome::BadSignature(reason) => {
            return Ok(InboundOutcome::Rejected {
                layer: format!("signature:{reason}"),
            })
        }
        SecOutcome::EmptyAfterSanitize => {
            return Ok(InboundOutcome::Rejected {
                layer: "sanitize_empty".into(),
            })
        }
    };

    let route = router::route(&payload, handles, hop);
    match route {
        Route::Drop { reason } => Ok(InboundOutcome::Dropped { reason }),
        Route::UnknownHandle { handle } => Ok(InboundOutcome::UnknownHandle { handle }),
        Route::Admin { verb_and_args } => {
            tracing::info!(verb = %verb_and_args, "admin command parsed: {:?}", nl_admin::parse(&verb_and_args));
            Ok(InboundOutcome::Admin { verb_and_args })
        }
        Route::Bot {
            slug,
            role,
            payload: stripped,
        } => {
            let t0 = std::time::Instant::now();
            // F185 — find the live BotRegistration so the mailbox
            // resolver can honor its `project_dir`. Synthesize a
            // fallback reg (project_dir = None) when the routed
            // (slug, role) isn't present in `bots` so a race between
            // unregister and route doesn't error out the pipeline —
            // the fallback layout keeps the inbound path live.
            let owned_reg;
            let reg_ref: &crate::BotRegistration = match bots
                .iter()
                .find(|b| b.workflow_slug == slug && b.role == role)
            {
                Some(b) => b,
                None => {
                    owned_reg = crate::BotRegistration {
                        workflow_slug: slug.clone(),
                        role: role.clone(),
                        vendor: ccteam_core::harness::AgentVendor::Claude,
                        persona_id: None,
                        im_platform: msg.channel.clone(),
                        im_chat_id: msg.reply_target.clone(),
                        chat_handle: None,
                        project_dir: None,
                        created_at: chrono::Utc::now(),
                    };
                    &owned_reg
                }
            };
            let dir = mailbox.inbox_dir(reg_ref)?;
            fs::create_dir_all(&dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
            let ts = Utc::now().format("%Y%m%dT%H%M%S").to_string();
            let path = dir.join(format!("msg-{ts}-{seq:03}.md"));
            let env = InboxEnvelope {
                platform: msg.channel.clone(),
                sender: msg.sender.clone(),
                hop,
                received_at: Utc::now(),
                reply_target: msg.reply_target.clone(),
                payload: stripped.clone(),
                message_id: msg.id.clone(),
            };
            let body = render_envelope(&env);
            fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
            tracing::info!(
                event = "latency",
                stage = "imd.mailbox.write",
                cid = %msg.id,
                slug = %slug,
                role = %role,
                elapsed_ms = t0.elapsed().as_millis() as u64,
                path = %path.display(),
                "latency imd.mailbox.write"
            );
            Ok(InboundOutcome::DroppedToBot {
                slug,
                role,
                path,
                payload: stripped,
                cid: msg.id.clone(),
                hop,
            })
        }
    }
}

/// Admin-aware wrapper around [`process_inbound`].
///
/// Reply behaviour:
///
/// - `InboundOutcome::Admin` → parse the NL via [`nl_admin::parse`],
///   execute it through `executor`, and `channel.send` the result so
///   the user sees the outcome in the same chat.
/// - `InboundOutcome::UnknownHandle` → format an
///   `Unknown handle '@xxx'. Available bots in this chat: @alice @bob`
///   reply from `bots` and send it. Replaces the V0.6.x silent-drop UX.
/// - Bot routing, plain drops, and security rejections pass through
///   with `admin_reply = None`.
///
/// Admin and unknown-handle replies do **not** bump the hop counter —
/// neither path re-enters the bot mailbox so the bot-to-bot hop budget
/// stays untouched.
#[allow(clippy::too_many_arguments)]
pub async fn process_inbound_admin_aware(
    msg: &ChannelMessage,
    sec: &Arc<Mutex<ThreeLayerSec>>,
    handles: &HandleMap,
    bots: &[crate::BotRegistration],
    mailbox: &dyn MailboxResolver,
    executor: &AdminExecutor,
    channel: &dyn Channel,
    hop: u8,
    seq: u64,
) -> Result<(InboundOutcome, Option<AdminReply>)> {
    let outcome = process_inbound(msg, sec, handles, bots, mailbox, hop, seq).await?;
    let admin_reply = match &outcome {
        InboundOutcome::Admin { verb_and_args } => {
            let cmd = nl_admin::parse(verb_and_args);
            let reply = executor
                .execute_for_chat(cmd, &msg.reply_target, &msg.channel, bots)
                .await;
            let out = SendMessage::new(reply.message.clone(), msg.reply_target.clone())
                .in_thread(msg.thread_ts.clone());
            if let Err(err) = channel.send(&out).await {
                tracing::warn!(error = %err, "admin reply send failed");
            }
            Some(reply)
        }
        InboundOutcome::UnknownHandle { handle } => {
            let available = available_handles_for_chat(bots, &msg.channel, &msg.reply_target);
            let text = format_unknown_handle_reply(handle, &available);
            let out =
                SendMessage::new(text, msg.reply_target.clone()).in_thread(msg.thread_ts.clone());
            if let Err(err) = channel.send(&out).await {
                tracing::warn!(error = %err, "unknown-handle reply send failed");
            }
            None
        }
        _ => None,
    };
    Ok((outcome, admin_reply))
}

/// Render a human-readable + machine-parseable Markdown envelope.
/// Front-matter YAML header carries the structured metadata; body is
/// the sanitized payload.
///
/// V0.6.5 F147 — `pub` so the MCP `chat_send_input` tool can hand-build
/// an envelope and drop it into the mailbox directly (bypassing the IM
/// security pipeline, since the caller is the meta-agent host process
/// already running with full local trust).
pub fn render_envelope(env: &InboxEnvelope) -> String {
    let yaml = serde_yaml::to_string(env).unwrap_or_default();
    format!("---\n{yaml}---\n\n{}\n", env.payload)
}

/// Parse an envelope back out (round-trip helper for tests + the
/// outbound echo-suppression module).
pub fn parse_envelope(text: &str) -> Result<InboxEnvelope> {
    let body = text
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow::anyhow!("missing front-matter prefix"))?;
    let end = body
        .find("\n---\n")
        .ok_or_else(|| anyhow::anyhow!("missing front-matter terminator"))?;
    let yaml = &body[..end];
    let env: InboxEnvelope = serde_yaml::from_str(yaml).context("parse front-matter")?;
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::AclPolicy;
    use tempfile::TempDir;

    fn sample_msg(platform: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: "x".into(),
            sender: "alice".into(),
            reply_target: "alice".into(),
            content: content.into(),
            channel: platform.into(),
            timestamp: 0,
            thread_ts: None,
        }
    }

    #[tokio::test]
    async fn drops_when_no_mention() {
        let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
        let tmp = TempDir::new().unwrap();
        let mailbox = DefaultMailboxResolver::with_projects_root(tmp.path());
        let res = process_inbound(
            &sample_msg("telegram", "hello world"),
            &sec,
            &HandleMap::new(),
            &[],
            &mailbox,
            0,
            1,
        )
        .await
        .unwrap();
        assert!(matches!(res, InboundOutcome::Dropped { .. }));
    }

    #[tokio::test]
    async fn drops_envelope_to_mailbox() {
        let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
        let tmp = TempDir::new().unwrap();
        let mailbox = DefaultMailboxResolver::with_projects_root(tmp.path());
        let mut handles = HandleMap::new();
        handles.insert("lead", "dev-foo", "lead");
        let res = process_inbound(
            &sample_msg("telegram", "@lead please plan"),
            &sec,
            &handles,
            &[],
            &mailbox,
            0,
            7,
        )
        .await
        .unwrap();
        match res {
            InboundOutcome::DroppedToBot {
                slug, role, path, ..
            } => {
                assert_eq!(slug, "dev-foo");
                assert_eq!(role, "lead");
                assert!(path.exists());
                let body = fs::read_to_string(&path).unwrap();
                let env = parse_envelope(&body).unwrap();
                assert_eq!(env.payload, "please plan");
                assert_eq!(env.platform, "telegram");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn admin_route_short_circuits() {
        let sec = Arc::new(Mutex::new(ThreeLayerSec::new(AclPolicy::default())));
        let tmp = TempDir::new().unwrap();
        let mailbox = DefaultMailboxResolver::with_projects_root(tmp.path());
        let res = process_inbound(
            &sample_msg("telegram", "@ccteam status"),
            &sec,
            &HandleMap::new(),
            &[],
            &mailbox,
            0,
            0,
        )
        .await
        .unwrap();
        match res {
            InboundOutcome::Admin { verb_and_args } => assert_eq!(verb_and_args, "status"),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rate_limit_layer_rejects() {
        let sec = Arc::new(Mutex::new(ThreeLayerSec {
            acl: AclPolicy::default(),
            rate: crate::rate_limit::RateLimiter::new(1, std::time::Duration::from_secs(60)),
        }));
        let tmp = TempDir::new().unwrap();
        let mailbox = DefaultMailboxResolver::with_projects_root(tmp.path());
        let mut handles = HandleMap::new();
        handles.insert("lead", "dev-foo", "lead");
        let m = sample_msg("telegram", "@lead one");
        let _ = process_inbound(&m, &sec, &handles, &[], &mailbox, 0, 1)
            .await
            .unwrap();
        let res = process_inbound(&m, &sec, &handles, &[], &mailbox, 0, 2)
            .await
            .unwrap();
        match res {
            InboundOutcome::Rejected { layer } => assert_eq!(layer, "rate_limit"),
            other => panic!("got {other:?}"),
        }
    }
}
