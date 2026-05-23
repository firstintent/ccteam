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
use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::execution::transcript_tail::{
    self, anthropic_project_dir, cursor_path, discover_active_session, encode_project_cwd,
    PendingTools, TranscriptCursor,
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
pub fn ensure_chat_hooks_installed(project_dir: &Path, hook_sh: &str) -> Result<(), HarnessError> {
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

/// F164 — Probe whether a tmux session's pane process looks like a
/// running `claude` process.
///
/// Algorithm (no pane content read — red line compliant):
/// 1. `tmux list-panes -F "#{pane_pid}"` → get pane PID(s).
/// 2. For each PID: `ps -p <pid> -o comm=` to read the command name.
/// 3. Accept if the `comm` field **contains** the string "claude"
///    (covers `claude`, `claude-code`, etc.).
///
/// Returns `false` when the session has no panes, all pids are gone,
/// or none match. Does **not** read pane text content.
fn is_pane_running_claude(session: &TmuxSession) -> bool {
    let pids = session.list_pane_pids();
    if pids.is_empty() {
        return false;
    }
    for pid in pids {
        if pid == 0 {
            continue;
        }
        let Ok(output) = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let comm = String::from_utf8_lossy(&output.stdout);
        if comm.trim().contains("claude") {
            return true;
        }
    }
    false
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

        // 3. Spawn (or reattach) the tmux session running
        //    `<claude> --dangerously-skip-permissions`.
        //
        //    F164 — Instead of hard-failing when the session already exists
        //    (which caused bot permanent failure after daemon restart on
        //    nas-box005, 2026-05-23), we probe liveness and either reattach
        //    or recreate:
        //
        //    a) Session exists + pane process is `claude` → reattach:
        //       skip spawning a new process, just update hooks and return a
        //       handle pointing at the existing session.
        //    b) Session exists + pane is dead (pid gone / comm ≠ "claude")
        //       → recreate: kill the stale tmux session (it's an orphan,
        //       not a running bot — not a violation of the "永不主动 kill
        //       长 session" red line), then fall through to new-session.
        //    c) Session absent → normal new-session path.
        let session_name = chat_session_name(&ctx.slug, &spec.role);
        let session = TmuxSession::from_name(session_name.clone());

        if session.exists() {
            if is_pane_running_claude(&session) {
                // (a) Alive & healthy — reattach.
                let pids = session.list_pane_pids();
                let pane_pid = pids.first().copied();
                tracing::info!(
                    event = "session_reattached",
                    session = %session_name,
                    slug = %ctx.slug,
                    role = %spec.role,
                    pane_pid = ?pane_pid,
                    "claude-tui: reattached to existing tmux session (pane claude process alive)"
                );
            } else {
                // (b) Dead pane — recreate.
                let pids = session.list_pane_pids();
                let old_pane_pid = pids.first().copied();
                tracing::info!(
                    event = "session_recreated",
                    session = %session_name,
                    slug = %ctx.slug,
                    role = %spec.role,
                    old_pane_pid = ?old_pane_pid,
                    "claude-tui: killing stale tmux session (dead pane), recreating"
                );
                session
                    .kill()
                    .map_err(|e| HarnessError::SpawnFailed(format!("tmux kill stale: {e}")))?;
                // Fall through to new-session below.
                let bin = claude_bin();
                let argv: Vec<&str> = vec![&bin, "--dangerously-skip-permissions"];
                session
                    .start(&ctx.cwd, &argv)
                    .map_err(|e| HarnessError::SpawnFailed(format!("tmux start: {e}")))?;
            }
        } else {
            // (c) Absent — normal new session.
            let bin = claude_bin();
            let argv: Vec<&str> = vec![&bin, "--dangerously-skip-permissions"];
            session
                .start(&ctx.cwd, &argv)
                .map_err(|e| HarnessError::SpawnFailed(format!("tmux start: {e}")))?;
        }

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
        let sendkeys_t0 = std::time::Instant::now();
        session
            .send_keys_literal(&text)
            .map_err(|e| HarnessError::SubmitFailed(format!("send_keys -l: {e}")))?;
        let literal_ms = sendkeys_t0.elapsed().as_millis() as u64;
        session
            .send_keys_enter()
            .map_err(|e| HarnessError::SubmitFailed(format!("send_keys Enter: {e}")))?;
        let total_ms = sendkeys_t0.elapsed().as_millis() as u64;
        // Synthesize a turn id from the wall clock + a short random
        // suffix derived from the system nanos — keeps the adapter
        // dep-light (no uuid crate) while staying unique enough for
        // the chat-mode cadence (≤ 1 turn / sec).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let turn_id = format!("turn-{nanos:x}");
        let role = h
            .raw_extras
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let slug = h
            .raw_extras
            .get("slug")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        tracing::info!(
            event = "latency",
            stage = "claude.sendkeys",
            turn_id = %turn_id,
            slug = %slug,
            role = %role,
            session = %h.identity,
            content_len = text.len(),
            literal_ms,
            total_ms,
            "latency claude.sendkeys"
        );
        Ok(TurnId::new(turn_id))
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

/// V0.6.1 — background event-driven tail of the Anthropic transcript
/// jsonl for the bot's active session. Uses `notify` (inotify on Linux,
/// FSEvents on macOS) to watch the **parent directory**
/// `~/.claude/projects/<encoded-cwd>/` for `CREATE` + `MODIFY` events,
/// so the typical wake latency drops from ~500ms (the previous poll
/// sleep) to a few ms.
///
/// Architecture:
///
/// - One watcher per `(project_dir, cwd, role)` triple, scoped to the
///   parent dir non-recursively. `CREATE(<new-sid>.jsonl)` signals
///   session rotation (`/clear` / `/compact`); `MODIFY(<sid>.jsonl)`
///   signals new content on the current session. Both reduce to one
///   `read_new` call against the affected path; mismatched sid →
///   reset cursor + clear pending tools.
/// - A 2-second safety-net poll runs in parallel via `tokio::select!`
///   to catch any missed inotify event (rare on local fs, but possible
///   when running under network mounts or some container layers).
/// - `transcript_tail::read_new` — the actual byte-cursor incremental
///   read with UTF-8 boundary safety + half-flushed line tolerance +
///   tool-pairing across cycles — is unchanged. Only the **wakeup**
///   mechanism changes here.
///
/// Cold-start (Anthropic projects dir doesn't exist yet) is handled by
/// briefly polling until the dir appears, after which the watcher is
/// installed and the loop becomes event-driven. Exits when `tx` is
/// closed (the consumer dropped the stream).
async fn tail_loop(
    project_dir: PathBuf,
    cwd: PathBuf,
    role: String,
    tx: mpsc::Sender<ThreadEvent>,
) {
    let cursor_file = cursor_path(&project_dir, &role);
    let mut cursor = TranscriptCursor::load(&cursor_file).unwrap_or_default();
    let mut pending = PendingTools::new();

    // Resolve `~/.claude/projects/<encoded-cwd>/`. Wait for it to exist
    // — Claude creates it on the first write of the first session.
    let parent_dir = match anthropic_project_dir(&cwd) {
        Some(p) => p,
        None => {
            tracing::warn!(
                cwd = %cwd.display(),
                role,
                "claude-tui tail: HOME unset; cannot resolve anthropic projects dir"
            );
            return;
        }
    };
    while !parent_dir.exists() {
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Bridge `notify` (sync callback) to a tokio mpsc the async loop
    // can `select!` on. Channel capacity 64 absorbs a burst of
    // CREATE/MODIFY events without blocking the watcher thread; the
    // safety-net poll catches anything we drop on overflow.
    let (evt_tx, mut evt_rx) = mpsc::channel::<notify::Event>(64);
    let watcher_result = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            // try_send so a saturated channel doesn't block the
            // watcher dispatcher thread; safety-net poll picks up
            // anything dropped.
            let _ = evt_tx.try_send(event);
        }
    });
    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "claude-tui tail: notify watcher creation failed; falling back to plain polling"
            );
            // Best-effort fallback to the legacy polling path —
            // shouldn't realistically happen on supported platforms.
            tail_loop_polling(project_dir, cwd, role, tx).await;
            return;
        }
    };
    if let Err(err) = watcher.watch(&parent_dir, RecursiveMode::NonRecursive) {
        tracing::warn!(
            path = %parent_dir.display(),
            error = %err,
            "claude-tui tail: watch() failed; falling back to plain polling"
        );
        drop(watcher);
        tail_loop_polling(project_dir, cwd, role, tx).await;
        return;
    }
    tracing::info!(
        path = %parent_dir.display(),
        role,
        "claude-tui tail: inotify watcher armed (parent dir CREATE+MODIFY)"
    );

    // One initial sweep — `read_new` against the most-recently-modified
    // session file. Catches any content written between Claude start
    // and our watcher arming.
    if let Some((sid, path)) = discover_active_session(&cwd) {
        if cursor.switch_session(&sid, encode_project_cwd(&cwd)) {
            pending.clear();
        }
        drain_path(&path, &mut cursor, &mut pending, &cursor_file, &tx).await;
    }

    loop {
        if tx.is_closed() {
            return;
        }
        tokio::select! {
            evt = evt_rx.recv() => {
                let Some(evt) = evt else { return };
                // Only act on CREATE (rotation) + MODIFY (content
                // append). Other event kinds (Remove, Access, Other)
                // are noise for our use case.
                if !matches!(evt.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    continue;
                }
                for affected in evt.paths.iter() {
                    if affected.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let Some(sid) = affected.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    // Switch via `TranscriptCursor::switch_session` so a
                    // sid we've seen before resumes at its prior offset
                    // — never re-reads. Closes the main-session ↔
                    // subagent-jsonl oscillation that caused 15× duplicate
                    // Telegram sends on NAS.
                    if cursor.switch_session(sid, encode_project_cwd(&cwd)) {
                        pending.clear();
                    }
                    drain_path(affected, &mut cursor, &mut pending, &cursor_file, &tx).await;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                // Safety-net poll. inotify rarely drops events on local
                // ext4/btrfs, but a half-flushed line that didn't fire
                // a MODIFY yet, or a fs that buffers writes, can leave
                // bytes on disk we haven't observed. Just re-discover +
                // read_new; usually a no-op.
                if let Some((sid, path)) = discover_active_session(&cwd) {
                    if cursor.switch_session(&sid, encode_project_cwd(&cwd)) {
                        pending.clear();
                    }
                    drain_path(&path, &mut cursor, &mut pending, &cursor_file, &tx).await;
                }
            }
        }
    }
}

/// Shared between the inotify-driven path and the safety-net branch:
/// one `read_new` call, persist cursor, forward events. Returns when
/// `tx.send` errors (consumer dropped).
async fn drain_path(
    transcript_path: &Path,
    cursor: &mut TranscriptCursor,
    pending: &mut PendingTools,
    cursor_file: &Path,
    tx: &mpsc::Sender<ThreadEvent>,
) {
    match transcript_tail::read_new(transcript_path, cursor, std::mem::take(pending)).await {
        Ok(Some(delta)) => {
            *pending = delta.pending_tools;
            cursor.byte_offset = delta.new_offset;
            cursor.last_event_id = delta.last_event_id;
            let _ = cursor.save(cursor_file);
            for ev in delta.events {
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
        }
        Ok(None) => {
            // file vanished mid-read (e.g. test cleanup) — no-op
        }
        Err(err) => {
            tracing::debug!(
                path = %transcript_path.display(),
                error = %err,
                "claude-tui tail: read_new failed; retry on next event"
            );
        }
    }
}

/// Polling-only fallback if `notify` fails to arm (e.g. unsupported
/// kernel / sandboxed environment). Same shape as the pre-V0.6.1
/// polling loop with a tighter post-success interval (50ms instead of
/// the legacy 500ms) so even the fallback path has lower latency.
async fn tail_loop_polling(
    project_dir: PathBuf,
    cwd: PathBuf,
    role: String,
    tx: mpsc::Sender<ThreadEvent>,
) {
    let cursor_file = cursor_path(&project_dir, &role);
    let mut cursor = TranscriptCursor::load(&cursor_file).unwrap_or_default();
    let mut pending = PendingTools::new();
    let mut sleep_ms: u64 = 200;

    loop {
        if tx.is_closed() {
            return;
        }
        let (sid, transcript_path) = match discover_active_session(&cwd) {
            Some(pair) => pair,
            None => {
                tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                sleep_ms = (sleep_ms * 2).min(2000);
                continue;
            }
        };

        if cursor.switch_session(&sid, transcript_tail::encode_project_cwd(&cwd)) {
            pending.clear();
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
                // V0.6.1: tighter post-success interval (was 500ms).
                sleep_ms = 50;
            }
            Ok(None) => {
                sleep_ms = (sleep_ms * 2).min(2000);
            }
            Err(_) => {
                sleep_ms = (sleep_ms * 2).min(5000);
            }
        }
        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        let _ = role;
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
