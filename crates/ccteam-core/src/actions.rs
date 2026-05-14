//! V0.3 M5.0 — write-action helpers shared by every channel layer.
//!
//! Promoted from `ccteam-cli/src/mcp_serve.rs` (where they lived as
//! private fns) so that:
//!
//! - the V0.3 web UI crate (`ccteam-web`) can call them without
//!   depending on `ccteam-cli` (binary-as-library is a dep-graph
//!   anti-pattern; sibling crates `ccteam-cli` / `ccteam-web` should
//!   both fan in to `ccteam-core`).
//! - the existing MCP `tool_send_to_session` / `tool_inject_decision`
//!   wrappers stay thin (args parse + JSON encode + delegate).
//! - `commands::run_resume` keeps its public surface stable but its
//!   body now lives here, callable from any crate.
//!
//! These helpers are **policy-free**:
//!
//! - they do **not** check daemon health (caller chooses gating; MCP
//!   wraps in `require_healthy_daemon`, web layer will do its own
//!   policy in M5.3 once token auth lands).
//! - they do **not** parse tmux output or kill sessions (architecture
//!   red lines, CLAUDE.md §三).
//! - they only touch the filesystem control plane (inbox files +
//!   `state.json` mutations) so the orchestrator's existing inotify +
//!   send-keys delivery picks the change up unchanged.
//!
//! Architecture refs: `docs/v0-3/prd.md` §3.2.3,
//! `docs/dev-coupling-audit.md` F45,
//! `docs/tech-design.md` §6.4 channel layer.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

use crate::inbox::{
    inbox_filename, InboxFrontMatter, InboxMessage, SessionMailbox, LATEST_SCHEMA_VERSION,
};
use crate::paths::CcteamPaths;
use crate::state::{PhaseState, ProjectState};

/// Source label every action helper stamps onto inbox front matter.
///
/// Channel-specific wrappers (the MCP tool, the web layer, telegram
/// channel, …) are free to override this via [`SendOptions::source`]
/// when their distinctness matters for downstream silence-classifier /
/// retro accounting. The default keeps existing MCP behavior backward
/// compatible (`source = "ccteam-mcp"` was the historical literal).
pub const DEFAULT_SOURCE: &str = "ccteam-core";

/// Default `source_user` label for server-originated injections.
pub const DEFAULT_SOURCE_USER: &str = "ccteam";

/// Optional knobs for [`send_to_session`]. Defaults match the legacy
/// MCP `tool_send_to_session` behavior so wrapper transparency holds.
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// Front-matter `source`. Default `ccteam-core`.
    pub source: String,
    /// Front-matter `source_user`. Default `ccteam`.
    pub source_user: String,
    /// Front-matter `content_type`. Default `text`.
    pub content_type: String,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            source: DEFAULT_SOURCE.into(),
            source_user: DEFAULT_SOURCE_USER.into(),
            content_type: "text".into(),
        }
    }
}

/// Result of [`send_to_session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendResult {
    /// Project / meta-session slug that received the message.
    pub slug: String,
    /// Filename written under `<project>/.ccteam/inbox/`.
    pub inbox_file: String,
    /// Absolute path to the inbox file.
    pub inbox_path: PathBuf,
}

/// Atomically write a free-form NL/markdown message into a session's
/// `.ccteam/inbox/`. The orchestrator's next inotify wake delivers it
/// via tmux send-keys (idle-aware).
///
/// Returns the inbox filename + absolute path of the file written.
///
/// # Errors
///
/// - `slug` does not resolve to a project directory under
///   `paths.projects_root`.
/// - inbox dir cannot be created or the write fails.
pub fn send_to_session(paths: &CcteamPaths, slug: &str, body: &str) -> Result<SendResult> {
    send_to_session_with(paths, slug, body, &SendOptions::default())
}

/// [`send_to_session`] with channel-specific front-matter overrides.
///
/// Used by the MCP wrapper to keep `source = "ccteam-mcp"` for backward
/// compat with retro / silence-classifier consumers that grep on
/// `source`.
pub fn send_to_session_with(
    paths: &CcteamPaths,
    slug: &str,
    body: &str,
    opts: &SendOptions,
) -> Result<SendResult> {
    let project_dir = paths.project_dir(slug);
    if !project_dir.exists() {
        return Err(anyhow!(
            "no project / session named `{}` (looked under {})",
            slug,
            project_dir.display(),
        ));
    }
    let ccteam_dir = paths.project_ccteam_dir(slug);
    let mailbox = SessionMailbox::for_ccteam_dir(&ccteam_dir);
    mailbox.ensure_dirs()?;
    let now = Utc::now();
    let filename = inbox_filename(now, next_inbox_seq(&mailbox)?);
    let inbox_path = mailbox.inbox.join(&filename);
    let msg = InboxMessage {
        front: InboxFrontMatter {
            schema_version: LATEST_SCHEMA_VERSION,
            source: opts.source.clone(),
            source_chat_id: None,
            source_msg_id: None,
            source_user: opts.source_user.clone(),
            created_at: now,
            ingested_at: now,
            content_type: opts.content_type.clone(),
            attachments: Vec::new(),
        },
        body: format!("{}\n", body.trim_end_matches('\n')),
    };
    msg.save(&inbox_path)?;
    Ok(SendResult {
        slug: slug.to_string(),
        inbox_file: filename,
        inbox_path,
    })
}

/// Parameters for [`inject_decision`] — a path-aware primitive.
///
/// `path` is the absolute file path to write; `body` is the raw
/// message content (no front-matter wrapping is performed — callers
/// that want the full inbox shape should use [`send_to_session`]
/// instead).
///
/// The shape is deliberately primitive so the V0.3 web layer (M5.3)
/// can hand the user a free-form decision file without going through
/// the inbox path-naming convention. The MCP wrapper still goes
/// through the inbox path because the MCP tool exposes the
/// `escalate_kind` enum, which the wrapper translates into a
/// structured body before calling [`inject_decision`].
#[derive(Debug, Clone)]
pub struct DecisionInput {
    /// Absolute path to write the decision file at.
    pub path: PathBuf,
    /// File body (channel-specific shape; e.g. inbox front-matter
    /// markdown, plain markdown, etc.).
    pub body: String,
}

/// Atomically write a decision file at the requested path.
///
/// Creates parent directories as needed. Uses `<path>.tmp` + rename
/// for atomicity (consistent with `InboxMessage::save`).
pub fn inject_decision(_paths: &CcteamPaths, _slug: &str, decision: DecisionInput) -> Result<()> {
    if let Some(parent) = decision.path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = decision.path.with_extension({
        let mut ext = decision
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        ext.push_str(".tmp");
        ext
    });
    std::fs::write(&tmp, decision.body.as_bytes())
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &decision.path)
        .with_context(|| format!("rename {} → {}", tmp.display(), decision.path.display()))?;
    Ok(())
}

/// Pause auto-dispatch for one project. Sets `user_pause_pending=true`
/// and bumps `last_user_interaction_at`. Does **not** kill the tmux
/// session (CLAUDE.md §三 red line).
///
/// Idempotent — pausing an already-paused project is a no-op state
/// rewrite (the timestamp updates, which is the desired effect for
/// fresh-attention accounting).
pub fn pause(paths: &CcteamPaths, slug: &str) -> Result<()> {
    let state_path = paths.project_state(slug);
    let mut state =
        ProjectState::load(&state_path).with_context(|| format!("load state for {slug}"))?;
    state.user_pause_pending = true;
    state.last_user_interaction_at = Utc::now();
    state.save(&state_path)?;
    Ok(())
}

/// Resume a paused project.
///
/// Clears `user_pause_pending`, lifts `user_attached`, re-arms
/// `phase_state=Idle` so the F66 workflow loop's next tick can
/// re-evaluate. Archives any sibling `escalation.md` to
/// `escalation.<context_reset_count>.md` so future ESCALATE writes
/// don't collide.
///
/// V0.4.0 F60: the legacy `phase_history` resume marker is gone with
/// the rest of the phase machinery. F66 reintroduces resume tracking
/// on the new workflow event log.
pub fn resume(paths: &CcteamPaths, slug: &str) -> Result<()> {
    let state_path = paths.project_state(slug);
    let mut state =
        ProjectState::load(&state_path).with_context(|| format!("load state for {slug}"))?;
    state.user_pause_pending = false;
    state.user_attached = false;
    state.phase_state = PhaseState::Idle;
    state.last_user_interaction_at = Utc::now();
    state.save(&state_path)?;

    let esc = paths.project_ccteam_dir(slug).join("escalation.md");
    if esc.exists() {
        let archive = paths
            .project_ccteam_dir(slug)
            .join(format!("escalation.{}.md", state.context_reset_count));
        let _ = std::fs::rename(&esc, &archive);
    }
    Ok(())
}

/// Compute the next 1-based sequence number for inbox writes within
/// the same wall-clock second. Scans existing files in the inbox dir
/// and picks `max + 1`. Tolerates a corrupted dir by defaulting to 1.
///
/// Promoted from `ccteam-cli/src/mcp_serve.rs` along with
/// [`send_to_session`] — both the MCP wrapper and any future channel
/// (web, telegram) building inbox files need it.
pub fn next_inbox_seq(mailbox: &SessionMailbox) -> Result<u32> {
    let entries = mailbox.list_inbox()?;
    let mut max = 0u32;
    for path in entries {
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            // Filename: msg-<ts>-<NNN>.md
            if let Some(seq) = name
                .strip_prefix("msg-")
                .and_then(|rest| rest.rsplit_once('-'))
                .map(|(_, last)| last.trim_end_matches(".md"))
                .and_then(|n| n.parse::<u32>().ok())
            {
                if seq > max {
                    max = seq;
                }
            }
        }
    }
    Ok(max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::bootstrap_project;
    use crate::tool_surface::disable_tool_surface_bootstrap_for_tests;

    fn isolated_paths() -> (tempfile::TempDir, CcteamPaths) {
        disable_tool_surface_bootstrap_for_tests();
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = CcteamPaths {
            root: tmp.path().join("home"),
            projects_root: tmp.path().join("projects"),
        };
        (tmp, paths)
    }

    #[test]
    fn send_to_session_writes_one_inbox_file_with_front_matter_and_body() {
        let (_tmp, paths) = isolated_paths();
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();

        let result = send_to_session(&paths, "demo", "hello from actions").unwrap();

        assert_eq!(result.slug, "demo");
        assert!(result.inbox_file.starts_with("msg-"));
        assert!(result.inbox_path.exists());

        let body = std::fs::read_to_string(&result.inbox_path).unwrap();
        // Front matter envelope intact.
        assert!(body.starts_with("---\n"));
        assert!(body.contains("schema_version: 1"));
        assert!(body.contains("content_type: text"));
        // Body restored with single trailing newline.
        assert!(body.contains("hello from actions\n"));

        // Round-trip parse so the YAML schema is enforced.
        let parsed = InboxMessage::load(&result.inbox_path).unwrap();
        assert_eq!(parsed.front.schema_version, LATEST_SCHEMA_VERSION);
        assert_eq!(parsed.front.content_type, "text");
        assert!(parsed.body.contains("hello from actions"));
    }

    #[test]
    fn send_to_session_errors_when_slug_does_not_exist() {
        let (_tmp, paths) = isolated_paths();
        let err = send_to_session(&paths, "no-such-slug", "ignored").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no project"), "got: {msg}");
    }

    #[test]
    fn send_to_session_with_overrides_source_and_source_user() {
        let (_tmp, paths) = isolated_paths();
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();

        let opts = SendOptions {
            source: "ccteam-mcp".into(),
            source_user: "mcp".into(),
            content_type: "markdown".into(),
        };
        let result = send_to_session_with(&paths, "demo", "body", &opts).unwrap();
        let parsed = InboxMessage::load(&result.inbox_path).unwrap();
        assert_eq!(parsed.front.source, "ccteam-mcp");
        assert_eq!(parsed.front.source_user, "mcp");
        assert_eq!(parsed.front.content_type, "markdown");
    }

    #[test]
    fn send_to_session_increments_seq_within_same_second() {
        let (_tmp, paths) = isolated_paths();
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();

        let r1 = send_to_session(&paths, "demo", "first").unwrap();
        let r2 = send_to_session(&paths, "demo", "second").unwrap();
        assert_ne!(r1.inbox_file, r2.inbox_file);
        // Ordered: r2 > r1 (lexicographic == chronological by design).
        assert!(r2.inbox_file > r1.inbox_file);
    }

    #[test]
    fn inject_decision_writes_body_atomically_at_path() {
        let (tmp, paths) = isolated_paths();
        let target = tmp.path().join("inject").join("decision.md");
        let decision = DecisionInput {
            path: target.clone(),
            body: "**META-AGENT DECISION**: ship it.\n".into(),
        };
        inject_decision(&paths, "ignored", decision).unwrap();
        assert!(target.exists());
        let body = std::fs::read_to_string(&target).unwrap();
        assert_eq!(body, "**META-AGENT DECISION**: ship it.\n");
        // `.tmp` sibling must be cleaned up (rename completed).
        let mut entries: Vec<String> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["decision.md".to_string()]);
    }

    #[test]
    fn inject_decision_creates_missing_parent_dirs() {
        let (tmp, paths) = isolated_paths();
        let target = tmp.path().join("a").join("b").join("c").join("d.md");
        inject_decision(
            &paths,
            "ignored",
            DecisionInput {
                path: target.clone(),
                body: "deep".into(),
            },
        )
        .unwrap();
        assert!(target.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "deep");
    }

    #[test]
    fn pause_sets_user_pause_pending_and_bumps_interaction_ts() {
        let (_tmp, paths) = isolated_paths();
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let state_path = paths.project_state("demo");
        let before = ProjectState::load(&state_path).unwrap();
        assert!(!before.user_pause_pending);

        pause(&paths, "demo").unwrap();

        let after = ProjectState::load(&state_path).unwrap();
        assert!(after.user_pause_pending);
        assert!(after.last_user_interaction_at >= before.last_user_interaction_at);
    }

    #[test]
    fn resume_clears_pause_and_resets_phase_state_to_idle() {
        let (_tmp, paths) = isolated_paths();
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let state_path = paths.project_state("demo");

        // Set up a paused state.
        {
            let mut s = ProjectState::load(&state_path).unwrap();
            s.user_pause_pending = true;
            s.user_attached = true;
            s.save(&state_path).unwrap();
        }

        resume(&paths, "demo").unwrap();

        let after = ProjectState::load(&state_path).unwrap();
        assert!(!after.user_pause_pending);
        assert!(!after.user_attached);
        assert_eq!(after.phase_state, PhaseState::Idle);
    }

    #[test]
    fn resume_archives_escalation_md_when_present() {
        let (_tmp, paths) = isolated_paths();
        bootstrap_project(&paths, "demo", "demo request", "dev").unwrap();
        let esc = paths.project_ccteam_dir("demo").join("escalation.md");
        std::fs::write(&esc, "the reason").unwrap();
        resume(&paths, "demo").unwrap();
        assert!(!esc.exists(), "escalation.md must be archived");
        // Archived sibling exists (suffix tied to context_reset_count).
        let entries: Vec<String> = std::fs::read_dir(paths.project_ccteam_dir("demo"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("escalation.") && n.ends_with(".md"))
            .collect();
        assert_eq!(entries.len(), 1, "expected exactly one archived escalation");
    }

    #[test]
    fn next_inbox_seq_starts_at_one_for_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mb = SessionMailbox::for_ccteam_dir(tmp.path());
        mb.ensure_dirs().unwrap();
        assert_eq!(next_inbox_seq(&mb).unwrap(), 1);
    }
}
