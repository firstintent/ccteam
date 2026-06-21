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
//! - **Slash-command passthrough**: [`HarnessAdapter::handle_directive`]
//!   forwards a `/foo` directive (handled via handle_directive) as the
//!   literal string `/foo`. ccteam never filters or rewrites these.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use futures::stream::BoxStream;
use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::execution::process_inspect::pane_runs_process;
use crate::execution::progress_bridge::{
    append_event, build_chat_session_reset_event_with_reason, hooks_script_from_env,
    progress_jsonl_from_env,
};
use crate::execution::transcript_tail::{
    self, active_session_id_path, anthropic_project_dir, cursor_path, encode_project_cwd,
    PendingTools, TranscriptCursor,
};
use crate::execution::turns_mirror;
use crate::{default_backend, MuxSessionId, MuxSessionKind, MuxSessionSpec};
use crate::{
    AgentSpecBrief, AgentVendor, ExecutionMode, HarnessAdapter, HarnessError, PermissionMode,
    SpawnCtx, ThreadErrorEvent, ThreadEvent, ThreadHandle, TurnId, TurnInput, CLAUDE_BIN_ENV,
};
use crate::{ChoiceOption, ChoicePrompt, Directive, DirectiveOutcome, ThreadStatus};

/// V0.6.0 F108 [`HarnessAdapter`] for Claude Code TUI (long-running tmux
/// session, multi-turn with context reuse).
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeTuiAdapter;

impl ClaudeTuiAdapter {
    pub const fn new() -> Self {
        Self
    }
}

/// Tmux/process session-name prefix for chat-mode bots.
pub const CHAT_SESSION_PREFIX: &str = "ccteam-chat-";

/// Claude's TUI can lag behind tmux's literal text write just after
/// startup/reattach. Sending Enter immediately after `send-keys -l`
/// occasionally leaves the text in the composer without submitting it on real
/// terminals. Keep this local to the Claude TUI path; one second is below
/// normal human-perceived turn latency and avoids scraping the pane.
const SUBMIT_ENTER_SETTLE: Duration = Duration::from_millis(1000);

/// Compose the canonical tmux session name for a chat-mode bot.
///
/// v0.8.8 F1 — 第二参的【语义】由 role 改为 **sid**(`s<N>`):同一
/// `(project, role)` 可有多个独立会话,唯一键是 sid,所以 pane 名按 sid
/// 命名。函数签名(两参 `&str`)不变 —— 编译器**不会**列出调用点,改值的
/// 责任在调用方(必须传 `s.id` / `ctx.sid` 而非 role)。
pub fn chat_session_name(slug: &str, sid: &str) -> String {
    format!("{CHAT_SESSION_PREFIX}{slug}-{sid}")
}

/// Inverse of [`chat_session_name`]: parse a chat-mode session name back into
/// `(slug, sid)`. sid 是末尾 dash 分段;slug 是 prefix 与它之间的全部
/// (slug 自身可含 dash,如 `team-proj`)。`rsplit_once('-')` 在 sid=`s<N>`
/// 不含 dash 时更稳健。`name` 非 chat-mode 名或缺 slug/sid 时返回 `None`。
pub fn parse_chat_session_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(CHAT_SESSION_PREFIX)?;
    let (slug, sid) = rest.rsplit_once('-')?;
    (!slug.is_empty() && !sid.is_empty()).then(|| (slug.to_string(), sid.to_string()))
}

/// V0.6.6 F172 V2 — compose the deterministic Anthropic `--name` /
/// `--resume` argument for a chat-mode bot. Same identifier as the tmux
/// session name; kept as a separate function so the two namespaces can
/// diverge in the future without grepping callers.
///
/// v0.8.8 F1 — 第二参语义同 [`chat_session_name`](role→sid):pane 名与
/// `--name`/`--resume` 标识保持同源(按 sid),与既有先例对齐
/// (`claude_bg.rs` 的 `ccteam-bg-{slug}-{sid}` / `codex_exec.rs`)。
pub fn chat_session_id_name(slug: &str, sid: &str) -> String {
    chat_session_name(slug, sid)
}

/// v0.8.6 W2b — write / merge the chat-progress hooks into
/// `<project>/.claude/settings.local.json`. ccteam writes its managed
/// hooks to the **local** settings layer so it never dirties the user's
/// committed `.claude/settings.json` (Claude Code reads + fires hooks
/// from settings.local.json the same as settings.json). The hook command
/// invokes the per-host `~/.ccteam/hooks/hook.sh` wrapper (HTTP-to-daemon
/// fast path + CLI fallback) instead of the cold-spawn
/// `<bin> internal hook chat-progress <event>` form.
///
/// `hook_sh` is the absolute path to the dispatcher
/// (`~/.ccteam/hooks/hook.sh` in production; tests pin a fake path).
///
/// V0.8 rmux W6 — when [`hook_via_daemon_enabled`]
/// is true (operator set `CCTEAM_HOOK_VIA_DAEMON=1`), the generated hook
/// command becomes `ccteam mux hook-emit --kind chat-progress --action
/// <arg>` instead — routing the firing to the orchestrator's hook.sock
/// (single-writer path) rather than the legacy direct progress.jsonl
/// write. When the flag is unset (the default) the command string is
/// byte-for-byte the pre-W6 `{hook_sh} chat-progress {arg}` form.
///
/// v0.8.7 W2 (DB.2) — `permission_mode` controls whether the
/// `PermissionRequest` hook is installed. `Hitl` adds a `PermissionRequest`
/// entry (no `matcher` = all tools, NO `timeout` field so a long human
/// approval is not killed) that routes to `{hook_sh} permission-request`;
/// `Skip` installs no such entry (and the spawn keeps
/// `--dangerously-skip-permissions`, so the ask-path never fires anyway).
/// SMOKE-GATE GROUND TRUTH: the hook fires only when the per-tool decision
/// is "ask"; allowlist / auto-allowed tools fire no hook — so a non-hitl
/// session must NOT carry this entry.
///
/// Idempotent: existing hooks for other events (and any other keys already
/// in settings.local.json) are preserved; existing chat-progress +
/// AskUserQuestion entries are replaced. The `PermissionRequest` entry is
/// removed when `permission_mode` is `Skip` (so toggling a session back to
/// skip on the next spawn cleans it up).
pub fn ensure_chat_hooks_installed(
    project_dir: &Path,
    hook_sh: &str,
    permission_mode: PermissionMode,
) -> Result<(), HarnessError> {
    let settings_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&settings_dir)
        .map_err(|e| HarnessError::Io(format!("create {}: {e}", settings_dir.display())))?;
    let settings_path = settings_dir.join("settings.local.json");
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
    let hooks_obj = hooks.as_object_mut().ok_or_else(|| {
        HarnessError::Io("settings.local.json `hooks` field is not an object".into())
    })?;

    // (event_name, chat-progress arg)
    let chat_events: &[(&str, &str)] = &[
        ("SessionStart", "session-start"),
        ("UserPromptSubmit", "user-prompt"),
        ("Stop", "stop"),
        ("SubagentStop", "subagent-stop"),
        ("PostToolUse", "tool-use"),
        ("PreToolUse", "pre-tool-use"),
        ("SessionEnd", "session-end"),
        ("PreCompact", "pre-compact"),
        ("PostCompact", "post-compact"),
    ];
    let via_daemon = hook_via_daemon_enabled();
    for (event, arg) in chat_events {
        // DEFAULT (flag unset): byte-for-byte the pre-W6 command string.
        // mode-3 Claude depends on this exact form — do NOT change it.
        let command = if via_daemon {
            // W6 daemon-bus reroute: hook subprocess emits onto the
            // ccteam-owned hook.sock; the orchestrator is the single
            // progress.jsonl writer. session_id is derived from
            // CCTEAM_CHAT_SLUG / CCTEAM_CHAT_ROLE env (the same env the
            // legacy hook.sh path forwards), so the command stays short.
            format!("ccteam mux hook-emit --kind chat-progress --action {arg}")
        } else {
            format!("{hook_sh} chat-progress {arg}")
        };
        let entry = json!([{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": command,
            }],
        }]);
        hooks_obj.insert((*event).to_string(), entry);
    }

    // v0.8.5 D6 — additive `AskUserQuestion` PreToolUse matcher: route the
    // agent's question to the IM user (chat round-trip over the daemon
    // mcp.sock) instead of letting it block the TUI. This is a SECOND
    // PreToolUse array entry alongside the `"*"` chat-progress one above (the
    // loop wrote PreToolUse as a one-element array; append, don't replace).
    //
    // ALWAYS the `{hook_sh} intercept-ask` wrapper form (HTTP-to-daemon fast
    // path + CLI fallback) — NOT the W6 `mux hook-emit` daemon-bus reroute the
    // other chat events use. `hook-emit` is fire-and-forget (frames a HookEvent
    // to the hook.sock and exits with no stdout), but AskUserQuestion is a
    // *blocking decision* hook: Claude Code reads the `permissionDecision`
    // (allow + answer / deny) from this command's stdout, which only the
    // wrapper path returns. The hook degrades to deny-with-reason when there is
    // no chat slug or the daemon is unreachable (bg behavior preserved).
    // `timeout` is the hook-subprocess budget (slightly above the daemon's
    // 600s answer TTL, which is enforced out of band).
    let _ = via_daemon; // intentionally not used for the blocking ask hook
    let ask_entry = json!({
        "matcher": "AskUserQuestion",
        "hooks": [{
            "type": "command",
            "command": format!("{hook_sh} intercept-ask"),
            "timeout": 660,
        }],
    });
    if let Some(pre_tool_use) = hooks_obj
        .get_mut("PreToolUse")
        .and_then(|v| v.as_array_mut())
    {
        pre_tool_use.push(ask_entry);
    }

    // v0.8.7 W2 (DB.2/DB.3) — HITL: install the `PermissionRequest` hook for
    // a hitl session only. SMOKE-GATE GROUND TRUTH: this hook fires ONLY when
    // a tool's permission decision is "ask" (non-allowlist); allowlist /
    // auto-allowed tools fire no hook. The handler (`{hook_sh}
    // permission-request`) blocks on an IM approve/deny round-trip over the
    // daemon mcp.sock and prints the `{behavior:allow|deny}` decision to
    // stdout. NO `timeout` field: the human approval can take up to ~600s
    // (the daemon enforces the real TTL), so a Claude-Code-side hook timeout
    // would kill it before the user answers. `matcher` omitted = all tools.
    // For a non-hitl (skip) session we REMOVE any stale entry so the next
    // spawn matches the spawn's flag (`--dangerously-skip-permissions`).
    match permission_mode {
        PermissionMode::Hitl => {
            hooks_obj.insert(
                "PermissionRequest".to_string(),
                json!([{
                    "hooks": [{
                        "type": "command",
                        "command": format!("{hook_sh} permission-request"),
                    }],
                }]),
            );
        }
        PermissionMode::Skip => {
            hooks_obj.remove("PermissionRequest");
        }
    }

    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|e| HarnessError::Io(format!("serialize settings.local.json: {e}")))?;
    std::fs::write(&settings_path, serialized)
        .map_err(|e| HarnessError::Io(format!("write {}: {e}", settings_path.display())))?;
    Ok(())
}

/// The official Telegram plugin id. It claims the bot-token `getUpdates`
/// long-poll, which is structurally exclusive (Telegram allows ONE consumer
/// per token) and collides with ccteam's own IM gateway — see
/// `docs/research/cc-stream-json-protocol.md` §6 (a real mid-session kick).
pub const TELEGRAM_PLUGIN_ID: &str = "telegram@claude-plugins-official";

/// v0.8.11 E2 (PRD §四 Q5) — pin-point isolate the official Telegram plugin
/// for a ccteam-spawned Claude session by writing
/// `enabledPlugins.{TELEGRAM_PLUGIN_ID} = false` into the project's
/// ccteam-managed `.claude/settings.local.json` (the layer ccteam already
/// owns; local > project > user, so this wins). ONLY this one plugin is
/// touched — every other user plugin is left exactly as the user set it
/// (vendor-native red line). Idempotent: existing `enabledPlugins` entries
/// and all other settings keys are preserved.
pub fn ensure_telegram_plugin_disabled(project_dir: &Path) -> Result<(), HarnessError> {
    let settings_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&settings_dir)
        .map_err(|e| HarnessError::Io(format!("create {}: {e}", settings_dir.display())))?;
    let settings_path = settings_dir.join("settings.local.json");
    let mut root: Value = if settings_path.exists() {
        let body = std::fs::read_to_string(&settings_path)
            .map_err(|e| HarnessError::Io(format!("read {}: {e}", settings_path.display())))?;
        serde_json::from_str(&body).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    let enabled = root
        .as_object_mut()
        .expect("root forced to object")
        .entry("enabledPlugins")
        .or_insert_with(|| json!({}));
    let enabled_obj = enabled.as_object_mut().ok_or_else(|| {
        HarnessError::Io("settings.local.json `enabledPlugins` is not an object".into())
    })?;
    // Idempotent + non-clobbering: if the user explicitly RE-enabled it we
    // still pin it false (the structural conflict makes co-running unsafe),
    // but we touch no other plugin key.
    enabled_obj.insert(TELEGRAM_PLUGIN_ID.to_string(), Value::Bool(false));

    let serialized = serde_json::to_string_pretty(&root)
        .map_err(|e| HarnessError::Io(format!("serialize settings.local.json: {e}")))?;
    std::fs::write(&settings_path, serialized)
        .map_err(|e| HarnessError::Io(format!("write {}: {e}", settings_path.display())))?;
    Ok(())
}

fn claude_bin() -> String {
    std::env::var(CLAUDE_BIN_ENV).unwrap_or_else(|_| "claude".to_string())
}

/// Build the env var pairs forwarded into the mux session at spawn so
/// the Claude Code hook subprocess can derive role/slug. The hook reads
/// `CCTEAM_CHAT_ROLE` via `std::env::var`; without these the hook's
/// `derive_role_from_payload` falls back to `None` and every chat-mode
/// progress event ships with `role=""`.
///
/// V0.8 W2c — returns owned `(String, String)` pairs to feed
/// [`MuxSessionSpec::env`] directly (the trait spec owns its env, no
/// borrow plumbing).
///
/// v0.8.7 review-fix (R-M1) — also forwards `CCTEAM_CHAT_SECRET` when a
/// non-empty per-session secret is supplied, so the in-pane stdio MCP
/// forwarder can authenticate `session_*` calls to the daemon's
/// `sid -> {role, secret}` map. An empty secret (tests / legacy) omits the
/// var entirely, preserving prior spawn env exactly.
///
/// v0.8.8 F1 — 同样转发 `CCTEAM_CHAT_SID`(非空才加,镜像 secret 的纪律),
/// 它是 hook(`PermissionRequest` / `chat-progress` marker)与 in-pane
/// `session_*` forwarder 学到 ccteam 会话 sid 的唯一通道。空 sid(测试 /
/// 旧路径)整个略过该 var,逐字保持原 spawn env。**注意**:这是 ccteam
/// 的 `s<N>` sid,绝非 Anthropic 的原生 session UUID(红线:二者不可混淆)。
fn chat_spawn_env_owned(role: &str, slug: &str, secret: &str, sid: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("CCTEAM_CHAT_ROLE".to_string(), role.to_string()),
        ("CCTEAM_CHAT_SLUG".to_string(), slug.to_string()),
    ];
    if !secret.is_empty() {
        env.push(("CCTEAM_CHAT_SECRET".to_string(), secret.to_string()));
    }
    if !sid.is_empty() {
        env.push(("CCTEAM_CHAT_SID".to_string(), sid.to_string()));
    }
    env
}

/// F164 — Probe whether a chat session's pane process looks like a
/// running `claude` process.
///
/// V0.8 W2c — thin async wrapper over the shared
/// [`pane_runs_process`] helper with needle `"claude"`. Goes through the
/// `ProcessBackend` trait for pane PID enumeration; the `ps -o comm=` read
/// stays OS-level. Red-line compliant (reads process command name, never
/// pane text content). Probe errors (backend query failure) degrade to
/// `false` — a session we can't probe is treated as not-alive, which
/// routes start_thread to the safe recreate path.
async fn pane_runs_claude(backend: &dyn crate::PaneBackend, id: &MuxSessionId) -> bool {
    pane_runs_process(backend, id, "claude")
        .await
        .unwrap_or(false)
}

/// V0.8 W2c — `MuxSessionSpec` for the dead-pane recreate path: relaunch
/// `claude --agent <role> --resume <session_id_name>` so Claude reloads the
/// prior session jsonl (lossless context restore via Anthropic's own CLI)
/// AND re-binds the role persona from `.claude/agents/<role>.md` — the
/// session-is-the-role keystone (v0.8.6 W1; resume must carry `--agent`
/// because Anthropic does not persist the agent binding into the jsonl).
/// v0.8.7 W2 (DB.2) — the permission-posture argv segment for a chat spawn.
///
/// - `Skip` (default) → `["--dangerously-skip-permissions"]`: today's
///   behavior, every tool runs without prompting.
/// - `Hitl` → `["--permission-mode", "default"]` and DROP the skip flag, so
///   Claude's native permission ask-path stays alive. SMOKE-GATE GROUND
///   TRUTH: passing `--permission-mode default` is MANDATORY because a
///   user-global `defaultMode: "auto"` would otherwise mask prompts and the
///   `PermissionRequest` hook would never fire.
///
/// Sits between the `--agent <role>` pair and the `--name`/`--resume` pair
/// in the argv. Returned as a `Vec` so it can be one or two elements.
fn permission_args(mode: PermissionMode) -> Vec<String> {
    match mode {
        PermissionMode::Skip => vec!["--dangerously-skip-permissions".to_string()],
        PermissionMode::Hitl => vec!["--permission-mode".to_string(), "default".to_string()],
    }
}

#[derive(Clone, Copy)]
struct ClaudeTuiSpecInput<'a> {
    role: &'a str,
    slug: &'a str,
    sid: &'a str,
    cwd: &'a Path,
    session_id_name: &'a str,
    model_id: Option<&'a str>,
    permission_mode: PermissionMode,
    secret: &'a str,
}

impl<'a> ClaudeTuiSpecInput<'a> {
    fn new(
        role: &'a str,
        slug: &'a str,
        sid: &'a str,
        cwd: &'a Path,
        session_id_name: &'a str,
    ) -> Self {
        Self {
            role,
            slug,
            sid,
            cwd,
            session_id_name,
            model_id: None,
            permission_mode: PermissionMode::Skip,
            secret: "",
        }
    }

    fn with_model_id(mut self, model_id: Option<&'a str>) -> Self {
        self.model_id = model_id;
        self
    }

    fn with_permission_mode(mut self, permission_mode: PermissionMode) -> Self {
        self.permission_mode = permission_mode;
        self
    }

    fn with_secret(mut self, secret: &'a str) -> Self {
        self.secret = secret;
        self
    }
}

fn claude_spawn_argv_base(input: ClaudeTuiSpecInput<'_>) -> Vec<String> {
    // v0.8.8 F2 — 空 role = roleless,不加 `--agent`(裸 claude 自读项目
    // CLAUDE.md);非空 role 仍绑 `.claude/agents/<role>.md` persona。
    let mut argv = vec![claude_bin()];
    if !input.role.is_empty() {
        argv.push("--agent".to_string());
        argv.push(input.role.to_string());
    }
    if let Some(model) = input.model_id.map(str::trim).filter(|m| !m.is_empty()) {
        argv.push("--model".to_string());
        argv.push(model.to_string());
    }
    argv.extend(permission_args(input.permission_mode));
    argv
}

fn spec_for_resume(input: ClaudeTuiSpecInput<'_>) -> MuxSessionSpec {
    let mut argv = claude_spawn_argv_base(input);
    argv.push("--resume".to_string());
    argv.push(input.session_id_name.to_string());
    // v0.8.8 F1 — pane 名按 sid(`--agent` 仍按 role 绑 persona)。
    MuxSessionSpec::new(
        chat_session_name(input.slug, input.sid),
        argv,
        input.cwd.to_path_buf(),
    )
    .with_env(chat_spawn_env_owned(
        input.role,
        input.slug,
        input.secret,
        input.sid,
    ))
    .with_kind(MuxSessionKind::LongLived)
}

/// V0.8 W2c — `MuxSessionSpec` for the `--resume` failure fallback: fresh
/// `claude [--agent <role>] --name <session_id_name>` (no context carry-over;
/// pairs with the `chat_session_reset` event the caller emits). Delegates to
/// [`spec_for_new`], so it inherits the same `--agent <role>` persona binding
/// (v0.8.6 W1 session-is-the-role keystone) — or, when `role` is empty
/// (v0.8.8 F2 roleless), the same `--agent`-omitted bare-claude shape.
fn spec_for_fresh(input: ClaudeTuiSpecInput<'_>) -> MuxSessionSpec {
    spec_for_new(input)
}

/// V0.8 W2c — `MuxSessionSpec` for the brand-new (session-absent) path:
/// `claude [--agent <role>] --name <session_id_name>` so Anthropic files the
/// session jsonl under a deterministic name (enabling future recreate-path
/// `--resume`) AND binds the role persona from `.claude/agents/<role>.md` —
/// the session-is-the-role keystone (v0.8.6 W1). v0.8.8 F2 — an empty `role`
/// (roleless) OMITS `--agent` so bare claude reads the project's own
/// `CLAUDE.md`; the `--name`/sid segment is unconditional.
fn spec_for_new(input: ClaudeTuiSpecInput<'_>) -> MuxSessionSpec {
    let mut argv = claude_spawn_argv_base(input);
    argv.push("--name".to_string());
    argv.push(input.session_id_name.to_string());
    // v0.8.8 F1 — pane 名按 sid(`--agent` 仍按 role 绑 persona)。
    MuxSessionSpec::new(
        chat_session_name(input.slug, input.sid),
        argv,
        input.cwd.to_path_buf(),
    )
    .with_env(chat_spawn_env_owned(
        input.role,
        input.slug,
        input.secret,
        input.sid,
    ))
    .with_kind(MuxSessionKind::LongLived)
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
    hooks_script_from_env()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "ccteam".to_string())
}

fn hook_via_daemon_enabled() -> bool {
    std::env::var("CCTEAM_HOOK_VIA_DAEMON")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// v0.8.5 D5 — local-jsx popup commands that apply directly when given an arg
/// but pop a ccteam picker when bare (effort: ApplyEffortAndClose). bare →
/// NeedsChoice; arg/choice → arg-form passthrough.
///
/// v0.8.10 — `model` was REMOVED: claude's real `/model` picker + "Switch
/// model?" confirmation drift from any hardcoded list, so `/model` now passes
/// straight through to claude's native TUI (see the dedicated `/model` arm in
/// `handle_directive`); the user drives it via `/screen` + number replies.
const CLAUDE_ARG_POPUPS: &[&str] = &["effort"];

/// v0.8.5 D5 — local-jsx commands that open a TUI panel/picker with no
/// chat-drivable arg form; sent bare they stick the modal + swallow input,
/// so the gate Rejects them (never blind-send a popup). Curated
/// (clearly-blocking subset) from references/claude-code/src/commands/*;
/// re-sync when bumping the claude reference. `/esc` recovers a stuck one.
const CLAUDE_PANEL_POPUPS: &[&str] = &[
    "config",
    "agents",
    "permissions",
    "mcp",
    "hooks",
    "plan",
    "ide",
    "login",
    "logout",
    "theme",
    "vault",
    "privacy-settings",
    "output-style",
    "terminal-setup",
    "sandbox-toggle",
    "rate-limit-options",
];

/// Reasoning-effort levels offered for a bare `/effort` choice.
const CLAUDE_EFFORT_LEVELS: &[&str] = &["low", "medium", "high"];

/// Build the [`ChoicePrompt`] for a bare arg-applicable popup (D5). The
/// token is a per-prompt unique id (≤16B ASCII, no `:`): the gateway
/// resolves callbacks token-globally, so a name-based token would collide
/// when two sessions raise the same command's picker at once.
fn claude_popup_prompt(name: &str) -> ChoicePrompt {
    let (title, opts): (&str, &[&str]) = match name {
        "effort" => ("Pick a reasoning effort", CLAUDE_EFFORT_LEVELS),
        _ => ("Pick an option", &[]),
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    ChoicePrompt {
        token: format!("cj{:x}", (nanos as u64) & 0xff_ffff_ffff),
        title: title.to_string(),
        options: opts
            .iter()
            .map(|o| ChoiceOption {
                id: (*o).to_string(),
                label: (*o).to_string(),
            })
            .collect(),
        multi: false,
    }
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
        // v0.8.8 F2 — roleless session 合法:空 role → spawn 不加 `--agent`
        // (vendor 原生裸 claude 自读项目 CLAUDE.md)。这是红线允许的(红线只禁
        // 注入 system prompt,不禁省略 `--agent`),故移除原先的非空 role 硬挡。
        // 1. Install chat-progress hooks into
        //    <project>/.claude/settings.local.json (ccteam-managed local
        //    layer; never dirties the user's committed settings.json).
        ensure_chat_hooks_installed(
            &ctx.project_dir,
            &ccteam_bin_for_hooks(),
            ctx.permission_mode,
        )?;
        // v0.8.11 E2 — pin-point isolate the official Telegram plugin (its
        // bot-token getUpdates poll structurally collides with ccteam's IM
        // gateway). Only this one plugin; every other stays as the user set it.
        ensure_telegram_plugin_disabled(&ctx.project_dir)?;
        // 2. Make sure the session's chat dir exists (turns.jsonl + cursor).
        //    v0.8.8 F1 — 按 sid 建目录(同 turns / marker / cursor 维度),
        //    所以同 (project, role) 多会话各自独立、不互相覆盖。
        turns_mirror::ensure_dir(&ctx.project_dir, &ctx.sid)
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
        // v0.8.8 F1 — pane 名按 ctx.sid(`--agent` 仍按 spec.role 绑 persona)。
        // /role 切换不变 sid → pane 名不变 → 命中 dead-pane recreate 路径
        // (carry-context 语义,决策 1A)。
        let session_name = chat_session_name(&ctx.slug, &ctx.sid);
        // V0.8 W2c — route all session lifecycle through the ProcessBackend
        // trait (default = TmuxBackend, behavior unchanged vs V0.6.x).
        // Hold the backend once; pass `&*backend` to the liveness probe.
        let backend = default_backend();
        let id = MuxSessionId::new(session_name.clone());

        // V0.6.6 F172 V2 — deterministic Anthropic `--name` / `--resume`
        // identifier so the dead-pane recreate path can ask Claude itself
        // to reload the prior session jsonl (lossless context restore
        // via Anthropic's own CLI surface; R10 守).
        let session_id_name = chat_session_id_name(&ctx.slug, &ctx.sid);
        let tui_spec_input =
            ClaudeTuiSpecInput::new(&spec.role, &ctx.slug, &ctx.sid, &ctx.cwd, &session_id_name)
                .with_model_id(ctx.model_id.as_deref())
                .with_permission_mode(ctx.permission_mode)
                .with_secret(&ctx.secret);
        if backend
            .exists(&id)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("mux exists: {e}")))?
        {
            if pane_runs_claude(&*backend, &id).await {
                // (a) Alive & healthy — reattach. F164 path; F172 V2 must
                // **not** touch this code path (no spawn → no argv change).
                let pids = backend.list_pane_pids(&id).await.unwrap_or_default();
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
                // (b) Dead pane — recreate via F172 V2 `--resume <name>`
                // route. If --resume fails (jsonl absent / corrupt /
                // user wiped ~/.claude/projects/) we detect a fast pane
                // death and fall through to a fresh `--name` spawn,
                // emitting `chat_session_reset` with a reason so the
                // user-visible bot context loss is explicit (no silent
                // synthesis — R3 / R10 守).
                let pids = backend.list_pane_pids(&id).await.unwrap_or_default();
                let old_pane_pid = pids.first().copied();
                tracing::info!(
                    event = "session_recreated",
                    session = %session_name,
                    slug = %ctx.slug,
                    role = %spec.role,
                    old_pane_pid = ?old_pane_pid,
                    "claude-tui: killing stale tmux session (dead pane), recreating via --resume"
                );
                backend
                    .kill(&id)
                    .await
                    .map_err(|e| HarnessError::SpawnFailed(format!("mux kill stale: {e}")))?;
                backend
                    .spawn(spec_for_resume(tui_spec_input))
                    .await
                    .map_err(|e| HarnessError::SpawnFailed(format!("mux spawn resume: {e}")))?;

                // Detect `--resume` failure: claude exits non-zero quickly
                // if the named session jsonl can't be loaded. Give the
                // pane a short window to either survive (success) or die
                // (failure). 400ms is enough for the OS to schedule
                // claude's startup + first jsonl read + exit on failure,
                // while staying well under user-perceptible latency.
                tokio::time::sleep(Duration::from_millis(400)).await;
                if !pane_runs_claude(&*backend, &id).await {
                    tracing::info!(
                        event = "session_resume_failed_fallback",
                        session = %session_name,
                        slug = %ctx.slug,
                        role = %spec.role,
                        "claude-tui: `claude --resume <name>` failed; falling back to fresh `--name` spawn"
                    );
                    // Kill the dead pane's tmux session shell (still
                    // exists with remain-on-exit / pane-dead state) and
                    // re-spawn fresh.
                    let _ = backend.kill(&id).await;
                    backend
                        .spawn(spec_for_fresh(tui_spec_input))
                        .await
                        .map_err(|e| HarnessError::SpawnFailed(format!("mux spawn fresh: {e}")))?;
                    // Best-effort emit `chat_session_reset` with
                    // explicit reason so IM / web surfaces show the
                    // user "context was lost". Path resolution honours
                    // CCTEAM_HOME so tests land in their tempdir layout.
                    if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
                        let ev = build_chat_session_reset_event_with_reason(
                            &spec.role,
                            &ctx.sid,
                            "resume_failed_fallback_to_fresh",
                        );
                        if let Err(err) = append_event(&progress_path, &ev) {
                            tracing::warn!(
                                error = %err,
                                "claude-tui: failed to append chat_session_reset event"
                            );
                        }
                    }
                }
            }
        } else {
            // (c) Absent — first spawn for this (slug, sid). F172 V2:
            // add `--name <session_id_name>` so Anthropic's session jsonl
            // is filed under a deterministic name, enabling future
            // recreate-path `--resume`. F118 brand-new spawn recovery
            // path is unchanged (operates on turns.jsonl, not Anthropic
            // session jsonl).
            backend
                .spawn(spec_for_new(tui_spec_input))
                .await
                .map_err(|e| HarnessError::SpawnFailed(format!("mux spawn new: {e}")))?;

            // v0.8.8 F1(决策 1)— fresh-spawn 也加 death/liveness probe
            // (镜像 branch (b) 的 `--resume` 探测)。/role 切换在 pane 已
            // 死时走的就是这条 fresh-spawn 路径(同 sid → 同 --name → 复用
            // 既有 session jsonl,carry-context 语义);若 `claude --name`
            // 因 jsonl 名冲突 / 启动失败而快速退出,不能误报成功 → 探测
            // 到 pane 死则 kill 残壳并 SpawnFailed,而非假装会话健在。
            // 400ms 与 branch (b) 同窗口:够 OS 调度 claude 启动 + 首读 +
            // 失败退出,又远低于用户可感延迟。
            tokio::time::sleep(Duration::from_millis(400)).await;
            if !pane_runs_claude(&*backend, &id).await {
                tracing::warn!(
                    event = "session_fresh_spawn_died",
                    session = %session_name,
                    slug = %ctx.slug,
                    sid = %ctx.sid,
                    role = %spec.role,
                    "claude-tui: fresh `claude --name <name>` died on startup; reporting spawn failure"
                );
                let _ = backend.kill(&id).await;
                return Err(HarnessError::SpawnFailed(format!(
                    "claude --name {session_id_name} died on startup (fresh spawn)"
                )));
            }
        }

        // V0.8 rmux Slice 1 — all three branches above (reattach /
        // recreate / fresh) converge here with a confirmed-live session.
        // Start a typed-event tap that mirrors no-enrichment pattern
        // detections into progress.jsonl. No-op unless CCTEAM_TYPED_EVENTS
        // is set, so the flag-OFF path is behavior-neutral (one env check).
        if let Some(progress_path) = progress_jsonl_from_env(&ctx.slug) {
            crate::execution::typed_events::maybe_start_typed_event_tap(
                backend.clone(),
                id.clone(),
                crate::Vendor::Claude,
                // Registry key == HookEvent::session_id (`{slug}-{role}`,
                // from CCTEAM_CHAT_SLUG/ROLE) so the orchestrator's hook sink
                // can route Stop-hook enrichment to this session's tap.
                format!("{}-{}", ctx.slug, spec.role),
                progress_path,
            );
        }

        // 4. Heartbeat file — lightweight liveness marker the imd watch
        //    + meta-agent dashboard can poll.
        //    v0.8.8 F1 — heartbeat 目录按 sid,与 turns / cursor / marker
        //    同维度,使同 (project, role) 多会话各自独立。
        let heartbeat = ctx
            .project_dir
            .join(".ccteam/chat")
            .join(&ctx.sid)
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
                // v0.8.8 F1 — sid 是 turns / cursor / marker 的真实键;
                // events() 从此处取出传给 tail_loop,使 tail 按 sid 定位
                // (而非按 role 推断,后者在同 role 多会话下会串台)。
                "sid": ctx.sid,
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
        let backend = default_backend();
        let id = MuxSessionId::new(h.identity.clone());
        if !backend
            .exists(&id)
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("mux exists: {e}")))?
        {
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
            TurnInput::ToolResult { call_id, content } => {
                let body = match content {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_default(),
                };
                format!("Tool result for {call_id}: {body}")
            }
        };
        let sendkeys_t0 = std::time::Instant::now();
        backend
            .send_text(&id, &text)
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("send_keys -l: {e}")))?;
        let literal_ms = sendkeys_t0.elapsed().as_millis() as u64;
        tokio::time::sleep(SUBMIT_ENTER_SETTLE).await;
        backend
            .send_enter(&id)
            .await
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
        // v0.8.8 F1 — sid 是 turns / cursor / marker 的真实键。tail_loop
        // 用它算 cursor_path / active_session_id_path,使同 (project, role)
        // 多会话各跟各自的 jsonl,绝不串台。
        let sid = h
            .raw_extras
            .get("sid")
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
            // v0.8.9 — roleless sessions (empty role) ALSO need the transcript
            // tail loop: answer-forwarding reads the transcript regardless of
            // persona, so gate on the sid, NOT on a non-empty role. The
            // previous `if !role.is_empty()` guard was a
            // "session = role"-era leftover that v0.8.8-F2's roleless spawn
            // missed — it silently dropped EVERY roleless reply: `events()`
            // spawned no tail loop, so its stream stayed empty and the gateway
            // pump never observed an ANSWER. The tail key is the sid (turns /
            // cursor / marker 真实键).
            if !sid.is_empty() {
                let dispatch = tracing::dispatcher::get_default(Clone::clone);
                tokio::spawn(tail_loop(pdir, cwd, role, sid, tx, dispatch));
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
        let backend = default_backend();
        let id = MuxSessionId::new(persistent_id.to_string());
        if !backend
            .exists(&id)
            .await
            .map_err(|e| HarnessError::SpawnFailed(format!("mux exists: {e}")))?
        {
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
        let backend = default_backend();
        let id = MuxSessionId::new(h.identity.clone());
        if !backend
            .exists(&id)
            .await
            .map_err(|e| HarnessError::ShutdownFailed(format!("mux exists: {e}")))?
        {
            return Ok(());
        }
        // Send `/exit` so Claude shuts down cleanly + writes any pending
        // transcript line; then SIGTERM via tmux kill-session.
        let _ = backend.send_text(&id, "/exit").await;
        let _ = backend.send_enter(&id).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        backend
            .kill(&id)
            .await
            .map_err(|e| HarnessError::ShutdownFailed(format!("tmux kill-session: {e}")))?;
        Ok(())
    }

    async fn handle_directive(
        &self,
        h: &ThreadHandle,
        d: Directive,
    ) -> Result<DirectiveOutcome, HarnessError> {
        // D5 four-channel gate (arch-refactor §2.1 / D5; §5.1 smoke PASS →
        // main path). Channels: (1) prompt open-set + (2) BRIDGE_SAFE local
        // → zero-knowledge passthrough; (3) local-jsx popups: arg-applicable
        // (model/effort) apply directly with an arg, bare → NeedsChoice
        // (never blind-send a bare popup); panel-only → Rejected; (4) agent
        // popups → D6 (the hook path, not here). Plus `/esc` → Escape.
        let name = d.name.trim().trim_start_matches('/').to_ascii_lowercase();

        // `/esc` escape hatch — cancel a stuck picker (write-only Escape).
        if name == "esc" || name == "escape" {
            default_backend()
                .send_escape(&MuxSessionId::new(h.identity.clone()))
                .await
                .map_err(|e| HarnessError::SubmitFailed(format!("send Escape: {e}")))?;
            return Ok(DirectiveOutcome::Done {
                receipt: "sent Escape to the Claude session".to_string(),
            });
        }

        // v0.8.10 — `/model` ALWAYS passes through to claude's NATIVE picker /
        // confirmation, never a ccteam hardcoded list (which drifts from
        // claude's real, evolving model set). claude's `/model` is an
        // interactive TUI (a picker when bare; a "Switch model?" confirmation
        // for a cache-invalidating switch) with no hook event, so ccteam can't
        // forward or auto-drive it — the user drives it from chat: `/screen` to
        // see the pane, then reply the number / Enter / `s` / `/esc`.
        if name == "model" {
            let line = if d.args.trim().is_empty() {
                "/model".to_string()
            } else {
                format!("/model {}", d.args.trim())
            };
            self.submit_turn(h, TurnInput::UserText(line)).await?;
            return Ok(DirectiveOutcome::Done {
                receipt: "已发送 /model —— claude 在 pane 里弹了它自己的选单/确认框。用 /screen 看，回数字选（回车=设为默认 · 输 s=仅本会话 · /esc=取消）。".to_string(),
            });
        }

        // Channel 3a — arg-applicable popup (effort). A resolved choice (from a
        // prior NeedsChoice) or inline args both apply directly with no picker.
        if CLAUDE_ARG_POPUPS.contains(&name.as_str()) {
            let applied = d
                .choice
                .as_ref()
                .and_then(|c| {
                    c.ids
                        .first()
                        .cloned()
                        .or_else(|| c.free_text.clone().filter(|s| !s.trim().is_empty()))
                })
                .or_else(|| {
                    let a = d.args.trim();
                    (!a.is_empty()).then(|| a.to_string())
                });
            match applied {
                Some(arg) => {
                    let turn = self
                        .submit_turn(h, TurnInput::UserText(format!("/{name} {}", arg.trim())))
                        .await?;
                    return Ok(DirectiveOutcome::Turn(turn));
                }
                // bare → offer the choice; the gateway renders + re-enters.
                None => return Ok(DirectiveOutcome::NeedsChoice(claude_popup_prompt(&name))),
            }
        }

        // Channel 3b — panel-only popup: no chat-drivable arg form.
        if CLAUDE_PANEL_POPUPS.contains(&name.as_str()) {
            return Ok(DirectiveOutcome::Rejected {
                reason: format!(
                    "/{name} opens a Claude TUI panel that can't be driven from chat — \
                     run it in a direct `claude` session (use /esc if a picker is stuck)."
                ),
            });
        }

        // Channels 1 + 2 (+ unknown) — zero-knowledge passthrough.
        let line = if d.args.trim().is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {}", d.args.trim())
        };
        let turn = self.submit_turn(h, TurnInput::UserText(line)).await?;
        Ok(DirectiveOutcome::Turn(turn))
    }

    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError> {
        // P3 — read the session transcript tail (never full-parse: it can
        // be tens of MB). model = `message.model`; context = last
        // `message.usage` row's input+cache_creation+cache_read; window =
        // `[1m]` suffix → 1M else the 200k baseline (the only constant).
        // The transcript path is resolved from `raw_extras` (cwd + role)
        // the same way the tail loop does — marker sid first, then the
        // most-recently-modified main-session jsonl.
        let Some(transcript) = self.resolve_transcript_path(h) else {
            // No project context (resumed handle / malformed extras) or no
            // transcript on disk yet → nothing to report. `Default`, not
            // an error (statusless is a valid answer).
            return Ok(ThreadStatus::default());
        };
        let (model, context) = transcript_tail::read_status_tail(&transcript)
            .await
            .map_err(|e| HarnessError::SubmitFailed(format!("read transcript status: {e}")))?;
        // TUI sessions don't surface a reasoning-effort axis (no stream-json
        // get_settings tap); leave it None so the statusline omits it.
        // Goal display is stream-json-only for now (read from the transcript
        // there); the TUI path leaves it None.
        Ok(ThreadStatus {
            model,
            context,
            effort: None,
            goal: None,
        })
    }
}

impl ClaudeTuiAdapter {
    /// Resolve the active transcript jsonl for a handle (P3), reusing the
    /// tail loop's discovery: read `cwd` + `sid` from `raw_extras`, prefer
    /// the session's `active-session-id` marker, fall back to the
    /// most-recently-modified main-session jsonl under
    /// `~/.claude/projects/<encoded-cwd>/`. `None` when there is no project
    /// context (e.g. a bare resumed handle) or no transcript exists yet.
    fn resolve_transcript_path(&self, h: &ThreadHandle) -> Option<PathBuf> {
        let cwd = h
            .raw_extras
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)?;
        let project_dir = h
            .raw_extras
            .get("project_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        // v0.8.8 F1 — marker 现按 sid 存储,与 events()/tail_loop 同键。
        let sid = h.raw_extras.get("sid").and_then(|v| v.as_str());
        let marker_key = sid.filter(|s| !s.is_empty()).unwrap_or("");
        let parent_dir = anthropic_project_dir(&cwd)?;
        // Marker sid first (deterministic per-session target, set by the
        // chat-progress hook); only trust it if the file actually exists.
        if let Some(pdir) = project_dir.as_ref() {
            if !marker_key.is_empty() {
                if let Some(sid) = read_marker_sid(&active_session_id_path(pdir, marker_key)) {
                    let p = parent_dir.join(format!("{sid}.jsonl"));
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
        // Fall back to discovery (skips subagent jsonls).
        transcript_tail::discover_active_session(&cwd).map(|(_, p)| p)
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
    // v0.8.8 F1 — cursor / marker 的存储键(`s<N>`),非 Anthropic 原生
    // UUID。`role` 仍保留给日志上下文;cursor_path /
    // active_session_id_path 一律按 sid。
    sid: String,
    tx: mpsc::Sender<ThreadEvent>,
    dispatch: tracing::Dispatch,
) {
    let cursor_file = cursor_path(&project_dir, &sid);
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
            tail_loop_polling(project_dir, cwd, role, sid, tx, dispatch).await;
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
        tail_loop_polling(project_dir, cwd, role, sid, tx, dispatch).await;
        return;
    }
    tracing::info!(
        path = %parent_dir.display(),
        role,
        "claude-tui tail: inotify watcher armed (parent dir CREATE+MODIFY)"
    );

    // F177 — initial sweep targets the marker's sid. The marker is
    // written by the chat-progress hook on SessionStart and carries
    // Anthropic's real session_id. v0.8.8 F1 — 每个会话有自己的 marker
    // `<project>/.ccteam/chat/<sid>/active-session-id`(键由 role 改为
    // sid),所以同 (project, role) 多会话也不会串台(各 tail 各跟各的
    // jsonl)。marker 内容仍是 Anthropic 原生 session_id(内层不动)。
    let marker_file = active_session_id_path(&project_dir, &sid);
    // F187 — surface a stuck loop with one WARN at ~60s; suppressed
    // once the marker appears (resets on first marker-found read).
    let mut silence = MarkerSilenceWatch::from_env();
    let initial = read_marker_target(&marker_file, &parent_dir);
    observe_marker(
        &mut silence,
        initial.is_some(),
        &marker_file,
        &role,
        &project_dir,
        &dispatch,
        &tx,
    )
    .await;
    if let Some((sid, path)) = initial {
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
                // Re-read the marker on every fs event — `/clear` /
                // `/compact` rotates the sid and the hook overwrites
                // the marker. Skip if missing (hook hasn't fired yet
                // → wait for next event). F187: account missing markers
                // toward the silence WARN clock.
                let target_sid = match read_marker_sid(&marker_file) {
                    Some(sid) => {
                        observe_marker(
                            &mut silence,
                            true,
                            &marker_file,
                            &role,
                            &project_dir,
                            &dispatch,
                            &tx,
                        )
                        .await;
                        sid
                    }
                    None => {
                        observe_marker(
                            &mut silence,
                            false,
                            &marker_file,
                            &role,
                            &project_dir,
                            &dispatch,
                            &tx,
                        )
                        .await;
                        continue;
                    }
                };
                for affected in evt.paths.iter() {
                    if affected.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let Some(sid) = affected.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    // F177 — only drain the bot's target jsonl. Other
                    // bots in the same project dir write their own
                    // jsonls; we must NOT read them here or we fan-
                    // out the same content to every bot's IM channel.
                    if sid != target_sid {
                        continue;
                    }
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
                // bytes on disk we haven't observed. Re-read marker +
                // drain the targeted jsonl. F187: this is the cadence
                // that gates the silence WARN — at 2s/tick we hit the
                // 60s threshold after ~30 consecutive misses.
                let pair = read_marker_target(&marker_file, &parent_dir);
                observe_marker(
                    &mut silence,
                    pair.is_some(),
                    &marker_file,
                    &role,
                    &project_dir,
                    &dispatch,
                    &tx,
                )
                .await;
                if let Some((sid, path)) = pair {
                    if cursor.switch_session(&sid, encode_project_cwd(&cwd)) {
                        pending.clear();
                    }
                    drain_path(&path, &mut cursor, &mut pending, &cursor_file, &tx).await;
                }
            }
        }
    }
}

/// Single observation point for the F187 in-process WARN gate
/// ([`MarkerSilenceWatch`]).
///
/// Called once per tail-loop tick (initial sweep, every inotify
/// CREATE/MODIFY event, and every 2-second safety-net tick in
/// `tail_loop`; every iteration of the poll-only fallback).
async fn observe_marker(
    silence: &mut MarkerSilenceWatch,
    marker_present: bool,
    marker_file: &Path,
    role: &str,
    project_dir: &Path,
    dispatch: &tracing::Dispatch,
    tx: &mpsc::Sender<ThreadEvent>,
) {
    let user_message = tracing::dispatcher::with_default(dispatch, || {
        silence.observe(marker_present, marker_file, role, project_dir)
    });
    if let Some(message) = user_message {
        let _ = tx
            .send(ThreadEvent::Error(ThreadErrorEvent {
                kind: "tail_marker_missing".to_string(),
                message,
            }))
            .await;
    }
}

/// F187 — surface tail loops silently waiting forever for the F176
/// active-session-id marker. The hook writes the marker on the
/// SessionStart hook firing; when that fails (most likely cause:
/// F186-style env-propagation failure leaving `role=""` so the hook
/// targets the wrong marker path), the loop quietly sleeps and the
/// user sees a bot that never replies. Fire one WARN at ~60s with
/// role/slug context so the failure mode is grep-able in logs;
/// suppress further WARNs until the marker appears at least once so
/// long-running loops don't spam.
///
/// V0.6.8 F196 — the WARN gate naturally resets across heals because
/// the supervisor's `reset_session` aborts the events task and
/// `ensure_started` respawns a fresh `events()` stream → fresh
/// `tail_loop` → fresh `MarkerSilenceWatch`. No per-heal-cycle reset
/// logic is plumbed here; the F196 escalation owns the reset, and
/// the WARN simply re-arms on the new task.
struct MarkerSilenceWatch {
    first_missing: Option<Instant>,
    warned: bool,
    warn_after: Duration,
}

impl Default for MarkerSilenceWatch {
    fn default() -> Self {
        Self {
            first_missing: None,
            warned: false,
            warn_after: Self::DEFAULT_WARN_AFTER,
        }
    }
}

impl MarkerSilenceWatch {
    /// Threshold after which we WARN. ~60s covers the cold-start grace
    /// period (the hook can take a beat to fire on first prompt) but
    /// surfaces a stuck loop well before the user reports "bot dead".
    /// `CCTEAM_TAIL_MARKER_WARN_MS` overrides this for tests so they
    /// don't have to sleep a full minute to exercise the WARN path.
    const DEFAULT_WARN_AFTER: Duration = Duration::from_secs(60);

    fn from_env() -> Self {
        let warn_after = std::env::var("CCTEAM_TAIL_MARKER_WARN_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Self::DEFAULT_WARN_AFTER);
        Self {
            first_missing: None,
            warned: false,
            warn_after,
        }
    }

    /// Call on every loop iteration with the current marker-present
    /// status. `marker_file` is only used for the WARN message.
    fn observe(
        &mut self,
        marker_present: bool,
        marker_file: &Path,
        role: &str,
        project_dir: &Path,
    ) -> Option<String> {
        if marker_present {
            self.first_missing = None;
            self.warned = false;
            return None;
        }
        let started = *self.first_missing.get_or_insert_with(Instant::now);
        if !self.warned && started.elapsed() >= self.warn_after {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            tracing::warn!(
                event = "tail_marker_missing",
                role = %role,
                project_dir = %project_dir.display(),
                marker = %marker_file.display(),
                elapsed_ms,
                "chat-mode tail waiting for SessionStart hook — likely env-propagation failure if this persists"
            );
            self.warned = true;
            return Some(
                "会话暂时没有产出: ccteam 还没看到 Claude 的 SessionStart 标记，可能是 hook 没有启动或环境变量没有传到会话。下一步: 请先重试发送；如果仍无回复，运行 `ccteam doctor` 检查 hook，或重启 `ccteam start`。"
                    .to_string(),
            );
        }
        None
    }
}

/// Read just the sid from `<project>/.ccteam/chat/<sid>/active-session-id`.
/// Returns `None` when the marker is absent (hook hasn't fired yet) or
/// unreadable. Trims whitespace because the hook writes the raw sid
/// without a trailing newline but a future writer might.
fn read_marker_sid(marker: &Path) -> Option<String> {
    let body = std::fs::read_to_string(marker).ok()?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve the marker to a `(sid, transcript_path)` pair the tail loop
/// can drain. Returns `None` when the marker is missing OR when the
/// targeted `<sid>.jsonl` doesn't exist yet under `<parent_dir>` — both
/// are transient cold-start conditions; caller waits for the next fs
/// event / safety-net tick.
fn read_marker_target(marker: &Path, parent_dir: &Path) -> Option<(String, PathBuf)> {
    let sid = read_marker_sid(marker)?;
    let path = parent_dir.join(format!("{sid}.jsonl"));
    if !path.exists() {
        return None;
    }
    Some((sid, path))
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
    // v0.8.8 F1 — 同 tail_loop:cursor / marker 按 sid;role 仅供
    // 日志。
    sid: String,
    tx: mpsc::Sender<ThreadEvent>,
    dispatch: tracing::Dispatch,
) {
    let cursor_file = cursor_path(&project_dir, &sid);
    let marker_file = active_session_id_path(&project_dir, &sid);
    let parent_dir = match anthropic_project_dir(&cwd) {
        Some(p) => p,
        None => {
            tracing::warn!(
                cwd = %cwd.display(),
                role,
                "claude-tui tail (polling): HOME unset; cannot resolve anthropic projects dir"
            );
            return;
        }
    };
    let mut cursor = TranscriptCursor::load(&cursor_file).unwrap_or_default();
    let mut pending = PendingTools::new();
    let mut sleep_ms: u64 = 200;
    // F187 — same WARN gate as `tail_loop`. The exponential backoff
    // (200ms → 2s) means count-based thresholds drift; the Instant
    // form fires on wall-clock elapsed instead, matching the
    // event-driven loop's 60s threshold.
    let mut silence = MarkerSilenceWatch::from_env();

    loop {
        if tx.is_closed() {
            return;
        }
        // F177 — marker-driven instead of `discover_active_session`.
        // No fallback to most-recently-modified jsonl; if the hook
        // hasn't published a marker yet, we wait.
        let (sid, transcript_path) = match read_marker_target(&marker_file, &parent_dir) {
            Some(pair) => {
                observe_marker(
                    &mut silence,
                    true,
                    &marker_file,
                    &role,
                    &project_dir,
                    &dispatch,
                    &tx,
                )
                .await;
                pair
            }
            None => {
                observe_marker(
                    &mut silence,
                    false,
                    &marker_file,
                    &role,
                    &project_dir,
                    &dispatch,
                    &tx,
                )
                .await;
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

    fn test_spec_input<'a>(
        role: &'a str,
        slug: &'a str,
        sid: &'a str,
        cwd: &'a Path,
        session_id_name: &'a str,
    ) -> ClaudeTuiSpecInput<'a> {
        ClaudeTuiSpecInput::new(role, slug, sid, cwd, session_id_name)
    }

    #[test]
    fn permission_args_skip_is_the_skip_flag() {
        // Default (skip) → exactly the single skip flag, unchanged behavior.
        assert_eq!(
            permission_args(PermissionMode::Skip),
            vec!["--dangerously-skip-permissions".to_string()]
        );
    }

    #[test]
    fn permission_args_hitl_drops_skip_for_permission_mode_default() {
        // Hitl → `--permission-mode default` (two elements) and NO skip flag.
        let args = permission_args(PermissionMode::Hitl);
        assert_eq!(
            args,
            vec!["--permission-mode".to_string(), "default".to_string()]
        );
        assert!(!args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn spec_for_new_argv_reflects_permission_mode() {
        let cwd = std::path::Path::new("/tmp/cc-permmode");
        // Skip: carries the skip flag, not --permission-mode.
        // v0.8.8 F1 — 第三参为 sid。
        let skip = spec_for_new(test_spec_input("dev", "slug", "s1", cwd, "sid-1"));
        assert!(skip
            .argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
        assert!(!skip.argv.iter().any(|a| a == "--permission-mode"));
        // Still the keystone --agent + --name (v0.8.8 F2: non-empty role path
        // is unchanged — `--agent` is present and immediately followed by the
        // role token "dev", so roleless's `--agent` omission never regresses a
        // named-role spawn).
        let agent_at = skip.argv.iter().position(|a| a == "--agent");
        assert_eq!(
            skip.argv.get(agent_at.unwrap() + 1).map(String::as_str),
            Some("dev")
        );
        assert!(skip.argv.iter().any(|a| a == "--name"));
        // Hitl: drops the skip flag, carries --permission-mode default.
        let hitl = spec_for_new(
            test_spec_input("dev", "slug", "s1", cwd, "sid-1")
                .with_permission_mode(PermissionMode::Hitl),
        );
        assert!(!hitl
            .argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
        assert!(hitl.argv.iter().any(|a| a == "--permission-mode"));
        assert!(hitl.argv.iter().any(|a| a == "default"));
        assert!(hitl.argv.iter().any(|a| a == "--name"));
    }

    #[test]
    fn spec_for_resume_argv_reflects_permission_mode() {
        let cwd = std::path::Path::new("/tmp/cc-permmode");
        // v0.8.8 F1 — 第三参为 sid。
        let hitl = spec_for_resume(
            test_spec_input("dev", "slug", "s1", cwd, "sid-1")
                .with_permission_mode(PermissionMode::Hitl),
        );
        assert!(!hitl
            .argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
        assert!(hitl.argv.iter().any(|a| a == "--permission-mode"));
        // Resume path keeps --resume (not --name).
        assert!(hitl.argv.iter().any(|a| a == "--resume"));
    }

    /// v0.8.8 F2 — roleless: 空 role 的 spawn argv **不含** `--agent`(裸 claude
    /// 自读项目 CLAUDE.md),但 `--name`+sid+skip 段恒在(确定性 session jsonl
    /// 名供后续 resume)。
    #[test]
    fn spec_for_new_roleless_omits_agent() {
        let cwd = std::path::Path::new("/tmp/cc-roleless");
        let spec = spec_for_new(test_spec_input("", "slug", "s1", cwd, "sid-1"));
        assert!(
            !spec.argv.iter().any(|a| a == "--agent"),
            "roleless spawn must omit --agent, got: {:?}",
            spec.argv
        );
        // The `--name`/sid + skip segment stays put.
        assert!(spec.argv.iter().any(|a| a == "--name"));
        assert!(spec.argv.iter().any(|a| a == "sid-1"));
        assert!(spec
            .argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
    }

    /// v0.8.8 F2 — roleless resume:空 role 的 resume argv 同样 **不含**
    /// `--agent`,且保留 `--resume`+sid。
    #[test]
    fn spec_for_resume_roleless_omits_agent() {
        let cwd = std::path::Path::new("/tmp/cc-roleless");
        let spec = spec_for_resume(test_spec_input("", "slug", "s1", cwd, "sid-1"));
        assert!(
            !spec.argv.iter().any(|a| a == "--agent"),
            "roleless resume must omit --agent, got: {:?}",
            spec.argv
        );
        assert!(spec.argv.iter().any(|a| a == "--resume"));
        assert!(spec.argv.iter().any(|a| a == "sid-1"));
    }

    /// v0.8.7 review-fix (R-M1) — a non-empty per-session secret is injected
    /// into the pane env as `CCTEAM_CHAT_SECRET`; an empty secret omits the var
    /// entirely (preserving prior spawn env for tests / legacy callers).
    ///
    /// v0.8.8 F1 — 同理 `CCTEAM_CHAT_SID`:非空 sid 注入、空 sid 略过。
    #[test]
    fn spec_env_carries_secret_only_when_present() {
        let cwd = std::path::Path::new("/tmp/cc-secret");
        let with = spec_for_new(
            test_spec_input("dev", "slug", "s1", cwd, "sid-1").with_secret("deadbeef"),
        );
        let pairs: std::collections::HashMap<&str, &str> = with
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(pairs.get("CCTEAM_CHAT_SECRET"), Some(&"deadbeef"));
        assert_eq!(pairs.get("CCTEAM_CHAT_ROLE"), Some(&"dev"));
        assert_eq!(pairs.get("CCTEAM_CHAT_SLUG"), Some(&"slug"));
        assert_eq!(pairs.get("CCTEAM_CHAT_SID"), Some(&"s1"));

        // 空 secret 略过 CCTEAM_CHAT_SECRET;空 sid 略过 CCTEAM_CHAT_SID。
        let without = spec_for_new(test_spec_input("dev", "slug", "", cwd, "sid-1"));
        assert!(
            !without.env.iter().any(|(k, _)| k == "CCTEAM_CHAT_SECRET"),
            "empty secret must omit CCTEAM_CHAT_SECRET, got: {:?}",
            without.env
        );
        assert!(
            !without.env.iter().any(|(k, _)| k == "CCTEAM_CHAT_SID"),
            "empty sid must omit CCTEAM_CHAT_SID, got: {:?}",
            without.env
        );
    }

    #[test]
    fn spec_argv_carries_role_model_when_present() {
        let cwd = std::path::Path::new("/tmp/cc-model");
        let spec = spec_for_new(
            test_spec_input("dev", "slug", "s1", cwd, "sid-1")
                .with_model_id(Some("deepseek-via-claude")),
        );
        let model_at = spec
            .argv
            .iter()
            .position(|a| a == "--model")
            .expect("role model must be passed through to claude argv");
        assert_eq!(
            spec.argv.get(model_at + 1).map(String::as_str),
            Some("deepseek-via-claude")
        );

        let resume = spec_for_resume(
            test_spec_input("dev", "slug", "s1", cwd, "sid-1")
                .with_model_id(Some("sonnet"))
                .with_permission_mode(PermissionMode::Hitl),
        );
        let resume_model_at = resume
            .argv
            .iter()
            .position(|a| a == "--model")
            .expect("resume must preserve role model");
        assert_eq!(
            resume.argv.get(resume_model_at + 1).map(String::as_str),
            Some("sonnet")
        );

        let no_model = spec_for_new(
            test_spec_input("dev", "slug", "s1", cwd, "sid-1").with_model_id(Some("  ")),
        );
        assert!(
            !no_model.argv.iter().any(|a| a == "--model"),
            "blank model must not add a claude --model argv pair: {:?}",
            no_model.argv
        );
    }

    #[test]
    fn chat_session_name_uses_chat_prefix() {
        assert_eq!(
            chat_session_name("dev-foo", "alice"),
            "ccteam-chat-dev-foo-alice"
        );
    }

    #[test]
    fn parse_chat_session_name_round_trips_with_dashed_slug() {
        // Role is the last segment; the slug keeps its internal dashes.
        assert_eq!(
            parse_chat_session_name("ccteam-chat-dev-foo-alice"),
            Some(("dev-foo".to_string(), "alice".to_string()))
        );
        let (slug, role) = ("team-proj", "reviewer");
        assert_eq!(
            parse_chat_session_name(&chat_session_name(slug, role)),
            Some((slug.to_string(), role.to_string()))
        );
        // Non-chat names and malformed inputs are rejected.
        assert_eq!(parse_chat_session_name("some-other-tmux"), None);
        assert_eq!(parse_chat_session_name("ccteam-chat-noseprole"), None);
    }

    #[test]
    fn vendor_and_name_match_wave2_contract() {
        let a = ClaudeTuiAdapter::new();
        assert_eq!(a.name(), "claude-tui");
        assert_eq!(a.vendor(), AgentVendor::Claude);
    }

    // F187 — state machine smoke. The `observe` method is the
    // load-bearing logic; an integration test
    // (`claude_tui_silence_warn_test.rs`) covers the actual WARN
    // emission against a live tail loop, this unit asserts the
    // first-missing tracking + reset on marker-found resets the
    // warned flag.
    #[test]
    fn marker_silence_watch_arms_and_resets() {
        let marker = std::path::PathBuf::from("/tmp/nope");
        let project = std::path::PathBuf::from("/tmp/proj");
        let mut s = MarkerSilenceWatch {
            first_missing: None,
            warned: false,
            warn_after: Duration::from_millis(0),
        };
        // First missing: arms the clock + (since threshold is 0)
        // immediately fires + flips warned to true.
        s.observe(false, &marker, "alice", &project);
        assert!(
            s.warned,
            "WARN should latch on first missing with 0 threshold"
        );
        // Second missing: warned latched → no re-fire (no observable
        // way to count from outside, but `first_missing` stays set).
        assert!(s.first_missing.is_some());
        // Marker found resets: next missing should re-arm the clock.
        s.observe(true, &marker, "alice", &project);
        assert!(!s.warned);
        assert!(s.first_missing.is_none());
    }
}
