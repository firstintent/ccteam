//! V0.6.0 F108 — `ClaudeTuiAdapter` (Wave 2 real impl).
//!
//! Long-running tmux + `claude --dangerously-skip-permissions` chat
//! session, driven by `send-keys -l` literal text + the explicit Enter
//! we follow it with. The transparent passthrough flow (slash commands
//! arrive as literal `/compact` / `/clear` / `/new` strings → Claude
//! handles them natively) is the ccgram + OMC verified pattern.
//!
//! ## Event surface
//!
//! [`HarnessAdapter::events`] merges two sources:
//!
//! 1. **Fast / boundary track**: Claude Code hooks (installed by
//!    [`ensure_chat_hooks_installed`]) call `ccteam internal hook
//!    chat-progress <event>` which appends `chat_*` events to
//!    `progress.jsonl`. The orchestrator's progress.jsonl tail surfaces
//!    `ThreadEvent::TurnStarted` / `TurnCompleted` from these.
//! 2. **Content track**: [`super::transcript_tail::read_new`] polls
//!    `~/.claude/projects/<encoded-cwd>/<sid>.jsonl` for the full
//!    per-item content (assistant text, tool-use args, thinking) and
//!    emits `ThreadEvent::Item*` events.
//!
//! Track 2 also mirrors each completed turn into
//! `<project>/.ccteam/chat/<bot>/turns.jsonl` (see
//! [`super::turns_mirror`]) so [`super::session_recovery`] can rebuild
//! the bot's memory on session-id loss (F118).
//!
//! ## R4 / R2 red lines
//!
//! - **No pane scraping**: this module never invokes tmux's pane-text
//!   capture command. All output state lives in `progress.jsonl` and
//!   the transcript jsonl mirror.
//!   All state lives in `progress.jsonl` + the transcript jsonl mirror.
//! - **Slash-command passthrough**: [`HarnessAdapter::submit_turn`]
//!   forwards `SystemDirective("foo")` as the literal string `/foo`.
//!   ccteam never filters or rewrites these.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::execution::transcript_tail::{
    self, cursor_path, discover_active_session, PendingTools, TranscriptCursor,
};
use crate::execution::turns_mirror;
use crate::harness::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, SpawnCtx,
    ThreadEvent, ThreadHandle, TurnId, TurnInput, CLAUDE_BIN_ENV,
};
use crate::tmux::TmuxSession;

/// V0.6.0 F108 [`HarnessAdapter`] for Claude Code TUI (long-running tmux
/// session, multi-turn with context reuse).
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeTuiAdapter;

impl ClaudeTuiAdapter {
    pub const fn new() -> Self {
        Self
    }
}

/// Compose the canonical tmux session name for a chat-mode bot.
pub fn chat_session_name(slug: &str, role: &str) -> String {
    format!("ccteam-chat-{slug}-{role}")
}

/// V0.6.0 F108 / V0.6.1 F139 — write / merge the chat-progress hooks
/// into `<project>/.claude/settings.json`. As of F139 the hook command
/// invokes the per-host `~/.ccteam/hooks/hook.sh` wrapper (HTTP-to-
/// daemon fast path + CLI fallback) instead of the cold-spawn
/// `<bin> internal hook chat-progress <event>` form.
///
/// `hook_sh` is the absolute path to the dispatcher
/// (`~/.ccteam/hooks/hook.sh` in production; tests pin a fake path).
///
/// Idempotent: existing hooks for other events are preserved; existing
/// chat-progress entries are replaced.
pub fn ensure_chat_hooks_installed(
    project_dir: &Path,
    hook_sh: &str,
) -> Result<(), HarnessError> {
    let settings_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&settings_dir)
        .map_err(|e| HarnessError::Io(format!("create {}: {e}", settings_dir.display())))?;
    let settings_path = settings_dir.join("settings.json");
    let mut root: Value = if settings_path.exists() {
        let body = std::fs::read_to_string(&settings_path)
            .map_err(|e| HarnessError::Io(format!("read {}: {e}", settings_path.display())))?;
        serde_json::from_str(&body).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let hooks = root
        .as_object_mut()
        .expect("root was forced to an object above")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .ok_or_else(|| HarnessError::Io("settings.json `hooks` field is not an object".into()))?;

    // (event_name, chat-progress arg)
    let chat_events: &[(&str, &str)] = &[
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "user-prompt"),
        ("Stop", "stop"),
        ("SubagentStop", "subagent-stop"),
        ("PostToolUse", "tool-use"),
        ("SessionEnd", "session-end"),
        ("PreCompact", "pre-compact"),
        ("PostCompact", "post-compact"),
    ];
    for (event, arg) in chat_events {
        let entry = json!([{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": format!("{hook_sh} chat-progress {arg}"),
            }],
        }]);
        hooks_obj.insert((*event).to_string(), entry);
    }

    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|e| HarnessError::Io(format!("serialize settings.json: {e}")))?;
    std::fs::write(&settings_path, serialized)
        .map_err(|e| HarnessError::Io(format!("write {}: {e}", settings_path.display())))?;
    Ok(())
}

fn claude_bin() -> String {
    std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string())
}

fn ccteam_bin_for_hooks() -> String {
    // V0.6.1 F139 — chat-mode hooks now invoke the wrapper script
    // (`~/.ccteam/hooks/hook.sh`) rather than the ccteam binary. The
    // wrapper itself handles the HTTP-to-daemon round-trip + CLI
    // fallback. Tests pin a fake path via `CCTEAM_HOOK_SH`; otherwise
    // resolve from `CcteamPaths::from_env()`.
    if let Ok(path) = std::env::var("CCTEAM_HOOK_SH") {
        return path;
    }
    crate::CcteamPaths::from_env()
        .map(|paths| paths.hooks_script().display().to_string())
        .unwrap_or_else(|_| "ccteam".to_string())
}

#[async_trait]
impl HarnessAdapter for ClaudeTuiAdapter {
    fn name(&self) -> &'static str {
        "claude-tui"
    }

    fn vendor(&self) -> AgentVendor {
        AgentVendor::Claude
    }

    async fn start_thread(
        &self,
        spec: &AgentSpecBrief,
        ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        if spec.role.is_empty() {
            return Err(HarnessError::SpawnFailed(
                "claude-tui requires a non-empty role (AgentSpecBrief::role)".into(),
            ));
        }
        // 1. Install chat-progress hooks into <project>/.claude/settings.json.
        ensure_chat_hooks_installed(&ctx.project_dir, &ccteam_bin_for_hooks())?;
        // 2. Make sure the bot's chat dir exists (turns.jsonl + cursor).
        turns_mirror::ensure_dir(&ctx.project_dir, &spec.role)
            .map_err(|e| HarnessError::Io(e.to_string()))?;

        // 3. Spawn tmux session running `<claude> --dangerously-skip-permissions`.
        let session_name = chat_session_name(&ctx.slug, &spec.role);
        let session = TmuxSession::from_name(session_name.clone());
        if session.exists() {
            return Err(HarnessError::SpawnFailed(format!(
                "tmux session already exists: {session_name} (call resume_thread instead)"
            )));
        }
        let bin = claude_bin();
        let argv: Vec<&str> = vec![&bin, "--dangerously-skip-permissions"];
        session
            .start(&ctx.cwd, &argv)
            .map_err(|e| HarnessError::SpawnFailed(format!("tmux start: {e}")))?;

        // 4. Heartbeat file — lightweight liveness marker the imd watch
        //    + meta-agent dashboard can poll.
        let heartbeat = ctx
            .project_dir
            .join(".ccteam/chat")
            .join(&spec.role)
            .join("heartbeat");
        if let Some(parent) = heartbeat.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&heartbeat, Utc::now().to_rfc3339());

        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: session_name.clone(),
            started_at: Utc::now(),
            raw_extras: json!({
                "tmux_session": session_name,
                "role": spec.role,
                "project_dir": ctx.project_dir.to_string_lossy(),
                "cwd": ctx.cwd.to_string_lossy(),
                "slug": ctx.slug,
            }),
        })
    }

    async fn submit_turn(
        &self,
        h: &ThreadHandle,
        input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        let session = TmuxSession::from_name(h.identity.clone());
        if !session.exists() {
            return Err(HarnessError::SubmitFailed(format!(
                "tmux session missing: {} (resume_thread first)",
                h.identity
            )));
        }
        let text: String = match input {
            TurnInput::UserText(s) => s,
            TurnInput::Artifact(p) => {
                format!("Look at the file I just placed at {}", p.display())
            }
            TurnInput::Image(p) => {
                format!("Look at the image I just placed at {}", p.display())
            }
            TurnInput::SystemDirective(d) => {
                // Slash-command passthrough — never filter, never rewrite.
                // R4 red line: ccteam does not know team-specific slashes;
                // Claude handles `/compact` / `/new` / `/clear` natively.
                format!("/{d}")
            }
            TurnInput::ToolResult { call_id, content } => {
                let body = match content {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                format!("Tool result for {call_id}: {body}")
            }
        };
        session
            .send_keys_literal(&text)
            .map_err(|e| HarnessError::SubmitFailed(format!("send_keys -l: {e}")))?;
        session
            .send_keys_enter()
            .map_err(|e| HarnessError::SubmitFailed(format!("send_keys Enter: {e}")))?;
        // Synthesize a turn id from the wall clock + a short random
        // suffix derived from the system nanos — keeps the adapter
        // dep-light (no uuid crate) while staying unique enough for
        // the chat-mode cadence (≤ 1 turn / sec).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(TurnId::new(format!("turn-{nanos:x}")))
    }

    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        let role = h
            .raw_extras
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let project_dir = h
            .raw_extras
            .get("project_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let cwd = h
            .raw_extras
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let (tx, rx) = mpsc::channel::<ThreadEvent>(64);

        if let (Some(pdir), Some(cwd)) = (project_dir, cwd) {
            if !role.is_empty() {
                tokio::spawn(tail_loop(pdir, cwd, role, tx));
            }
        }

        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|ev| (ev, rx))
        }))
    }

    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        // The persistent_id is the tmux session name
        // (`ccteam-chat-<slug>-<role>`). If it's live, hand back a
        // handle pointing at it; otherwise we cannot rebuild without
        // the SpawnCtx (caller falls back to start_thread + recovery).
        let session = TmuxSession::from_name(persistent_id.to_string());
        if !session.exists() {
            return Err(HarnessError::NotImplemented {
                reason: format!(
                    "resume_thread requires a live tmux session ({persistent_id} not found); \
                     caller must invoke start_thread with the original SpawnCtx and seed the \
                     fresh session via session_recovery::build_recovery_prompt"
                ),
            });
        }
        // Best-effort: parse `<slug>-<role>` out of the name so the
        // returned handle still carries identity. raw_extras stays
        // minimal because we don't know project_dir / cwd here.
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: persistent_id.to_string(),
            started_at: Utc::now(),
            raw_extras: json!({"tmux_session": persistent_id}),
        })
    }

    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError> {
        let session = TmuxSession::from_name(h.identity.clone());
        if !session.exists() {
            return Ok(());
        }
        // Send `/exit` so Claude shuts down cleanly + writes any pending
        // transcript line; then SIGTERM via tmux kill-session.
        let _ = session.send_keys_literal("/exit");
        let _ = session.send_keys_enter();
        tokio::time::sleep(Duration::from_millis(500)).await;
        session
            .kill()
            .map_err(|e| HarnessError::ShutdownFailed(format!("tmux kill-session: {e}")))?;
        Ok(())
    }
}

/// Background polling loop: walks the Anthropic transcript jsonl for
/// the bot's most recent session and pushes parsed events through `tx`.
/// Exits when `tx` is closed (the consumer dropped the stream).
async fn tail_loop(
    project_dir: PathBuf,
    cwd: PathBuf,
    role: String,
    tx: mpsc::Sender<ThreadEvent>,
) {
    let cursor_file = cursor_path(&project_dir, &role);
    let mut cursor = TranscriptCursor::load(&cursor_file).unwrap_or_default();
    let mut pending = PendingTools::new();
    // Defensive backoff if Anthropic projects dir is missing (e.g.
    // hermetic test env without ~/.claude).
    let mut sleep_ms: u64 = 200;

    loop {
        if tx.is_closed() {
            return;
        }
        // Re-discover the active session-id every iteration so a
        // `/clear` that rotates the file picks up the new sid.
        let (sid, transcript_path) = match discover_active_session(&cwd) {
            Some(pair) => pair,
            None => {
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                sleep_ms = (sleep_ms * 2).min(2000);
                continue;
            }
        };

        if cursor.session_id != sid {
            // Rotation — start fresh on the new file.
            cursor.session_id = sid.clone();
            cursor.byte_offset = 0;
            cursor.last_event_id = None;
            pending.clear();
            cursor.project_encoded = transcript_tail::encode_project_cwd(&cwd);
        }

        match transcript_tail::read_new(&transcript_path, &cursor, std::mem::take(&mut pending))
            .await
        {
            Ok(Some(delta)) => {
                pending = delta.pending_tools;
                cursor.byte_offset = delta.new_offset;
                cursor.last_event_id = delta.last_event_id;
                let _ = cursor.save(&cursor_file);
                for ev in delta.events {
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
                sleep_ms = 500;
            }
            Ok(None) => {
                sleep_ms = (sleep_ms * 2).min(2000);
            }
            Err(_) => {
                sleep_ms = (sleep_ms * 2).min(5000);
            }
        }
        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
    }
}

// Re-export to keep `anthropic_project_dir` reachable for the
// session_recovery / imd consumers without bloating the module surface.
pub use crate::execution::transcript_tail::anthropic_project_dir as resolve_anthropic_project_dir;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_session_name_uses_chat_prefix() {
        assert_eq!(
            chat_session_name("dev-foo", "alice"),
            "ccteam-chat-dev-foo-alice"
        );
    }

    #[test]
    fn vendor_and_name_match_wave2_contract() {
        let a = ClaudeTuiAdapter::new();
        assert_eq!(a.name(), "claude-tui");
        assert_eq!(a.vendor(), AgentVendor::Claude);
    }
}
