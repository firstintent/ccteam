//! Shared HITL "ask the user to approve/deny a tool call" core (v0.8.22 P0-2).
//!
//! Fixes review §3.1-1: the default `stream-json` protocol built its
//! [`ccteam_harness::execution::claude_stream_json::ClaudeStreamJsonAdapter`]
//! singleton WITHOUT a `CanUseToolResolver` (see
//! `daemon::default_adapter_factory_with_stream_json_handle`), so every
//! non-allowlist tool call in a `hitl` stream-json session silently denied
//! with **no IM/web prompt ever rendered** — the receipt promised "非白名单
//! 工具需要 IM 审批" but nobody was ever asked.
//!
//! Both Claude HITL surfaces now funnel through [`ask_permission`] so the
//! approval buttons, TTL, and deny semantics are identical across protocols:
//!
//! - **terminal** protocol: the `PermissionRequest` hook's `permission/ask`
//!   RPC over `mcp.sock` (`ccteam-cli`'s `execute_permission_ask` resolves a
//!   [`crate::gateway::HitlPromptContext`] then calls in here).
//! - **stream-json** protocol (the default): [`GatewayCanUseToolResolver`],
//!   constructed once the gateway + pending registry + event sink are wired
//!   (`daemon::run_daemon_with_shutdown`) and set on the ONE stream-json
//!   Claude adapter singleton via `ClaudeStreamJsonAdapter::set_resolver` —
//!   every stream-json hitl session (IM- or web-created) shares it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use ccteam_harness::execution::claude_stream_json::bridge::{
    ApprovalDecision, CanUseToolReq, CanUseToolResolver,
};
use ccteam_harness::{
    ApprovalIR, ApprovalRisk, ChoiceOption, ChoicePrompt, ChoiceSelection, PiApprovalDecision,
    PiDialogKind, PiDialogRequest, PiDialogResponse, PiInteractionResolver,
};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::gateway::{Gateway, GatewayEvent, GatewayEventKind, HitlPromptContext};
use crate::pending::{InteractionOrigin, PendingInteractions};
use crate::transport::MessageOption;

/// v0.8.7 review-fix (R-L1) — a HITL prompt blocks the whole turn, so it gets
/// a SHORTER deadline than the 600s `interaction/ask`. Fail-safe on lapse =
/// deny. Env-overridable (`CCTEAM_PERMISSION_PROMPT_TTL_SECS`) for ops/tests.
const PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT: u64 = 120;

/// Resolve the HITL permission-prompt TTL: the env override when set + valid,
/// else [`PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT`]. Clamped to ≥1s so a
/// misconfig can't make the prompt expire instantly (which would deny every
/// tool before the user can possibly click).
pub fn permission_prompt_timeout_secs() -> u64 {
    std::env::var("CCTEAM_PERMISSION_PROMPT_TTL_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT)
}

/// Render a rich, human-readable summary of a tool call for the approval
/// prompt (v0.8.22 P1, review §3.1-2). The prior version showed only the
/// tool name + the first matching field truncated at 160 chars, so a
/// `Write`/`Edit` approval showed nothing but the file path — "见文件路径
/// 不见写入内容,等于盲批" (you see the path, never the content: a blind
/// approval). Tool-aware:
///
/// - `Write` → file path + a content preview (~200 chars).
/// - `Edit`/`MultiEdit` → path + an old→new snippet (~100 chars each side).
/// - `Bash`/`KillShell`/`KillBash` → the command string (~200 chars).
/// - anything else (Read, Glob, MCP tools, …) → a compact `key: value`
///   digest of the top-level params, length-capped — NEVER a raw JSON dump.
pub fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String {
    match tool_name {
        "Write" => summarize_write(tool_input),
        "Edit" => summarize_edit(tool_input),
        "MultiEdit" => summarize_multi_edit(tool_input),
        "Bash" | "KillShell" | "KillBash" => summarize_bash(tool_name, tool_input),
        _ => summarize_generic(tool_name, tool_input),
    }
}

const CONTENT_PREVIEW_MAX: usize = 200;
const SNIPPET_MAX: usize = 100;
const COMMAND_MAX: usize = 200;
const GENERIC_DIGEST_MAX: usize = 200;
const GENERIC_VALUE_MAX: usize = 60;

fn summarize_write(tool_input: &serde_json::Value) -> String {
    let path = tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("(no file_path)");
    let content = tool_input
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if content.is_empty() {
        format!("Write {path}")
    } else {
        format!(
            "Write {path}\n  + {}",
            truncate_chars(content, CONTENT_PREVIEW_MAX)
        )
    }
}

fn summarize_edit(tool_input: &serde_json::Value) -> String {
    let path = tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("(no file_path)");
    let old = tool_input
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new = tool_input
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!(
        "Edit {path}\n  - {}\n  + {}",
        truncate_chars(old, SNIPPET_MAX),
        truncate_chars(new, SNIPPET_MAX),
    )
}

fn summarize_multi_edit(tool_input: &serde_json::Value) -> String {
    let path = tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("(no file_path)");
    let edits = tool_input.get("edits").and_then(|v| v.as_array());
    let Some(first) = edits.and_then(|e| e.first()) else {
        return format!("MultiEdit {path}");
    };
    let old = first
        .get("old_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new = first
        .get("new_string")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let extra_count = edits.map(|e| e.len()).unwrap_or(1).saturating_sub(1);
    let extra = if extra_count > 0 {
        format!(
            " (+{extra_count} more edit{})",
            if extra_count == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    format!(
        "MultiEdit {path}{extra}\n  - {}\n  + {}",
        truncate_chars(old, SNIPPET_MAX),
        truncate_chars(new, SNIPPET_MAX),
    )
}

fn summarize_bash(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let command = tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("(no command)");
    format!("{tool_name} {}", truncate_chars(command, COMMAND_MAX))
}

/// Fallback for every tool without a dedicated renderer (Read/Glob/Grep,
/// MCP tools, …): a compact `key: value` digest of the top-level params,
/// length-capped. Collapses nested arrays/objects to a size marker instead
/// of recursing, so a generic tool call can never dump a raw JSON blob into
/// the approval prompt.
fn summarize_generic(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let Some(obj) = tool_input.as_object() else {
        return tool_name.to_string();
    };
    if obj.is_empty() {
        return tool_name.to_string();
    }
    let mut parts = Vec::new();
    let mut total = 0usize;
    for (key, value) in obj {
        let piece = format!("{key}: {}", compact_value(value));
        total += piece.chars().count();
        parts.push(piece);
        if total >= GENERIC_DIGEST_MAX {
            break;
        }
    }
    let digest = truncate_chars(&parts.join(", "), GENERIC_DIGEST_MAX);
    format!("{tool_name} {digest}")
}

/// A JSON value collapsed to a short, single-line preview — arrays/objects
/// become a size marker (`[3 items]` / `{2 keys}`), never their full
/// recursive contents.
fn compact_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => truncate_chars(s, GENERIC_VALUE_MAX),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Array(a) => {
            format!("[{} item{}]", a.len(), if a.len() == 1 { "" } else { "s" })
        }
        serde_json::Value::Object(o) => {
            format!("{{{} key{}}}", o.len(), if o.len() == 1 { "" } else { "s" })
        }
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        s.to_string()
    }
}

/// Coarse severity bucket for a tool call (v0.8.22 P1, review §3.1-2),
/// reusing the [`ApprovalRisk`] enum ccteam-harness already defined but no
/// approval surface actually populated. Best-effort classification, NOT a
/// security boundary: it exists so the human clicking Approve/Deny can tell
/// "reading a file" from "rewriting one" from "rm -rf" at a glance instead
/// of blind-approving every prompt identically. Errs toward the pessimistic
/// tier when unsure — an unrecognized tool is `Unknown`, never silently
/// treated as `Low`.
pub fn classify_tool_risk(tool_name: &str, tool_input: &serde_json::Value) -> ApprovalRisk {
    match tool_name {
        "Read"
        | "Glob"
        | "Grep"
        | "LS"
        | "NotebookRead"
        | "TodoRead"
        | "WebSearch"
        | "WebFetch"
        | "BashOutput"
        | "read"
        | "grep"
        | "find"
        | "ls"
        | "ccteam_status"
        | "ccteam_grok_claude_codex_kimi"
        | "ccteam_session_list"
        | "ccteam_session_collect" => ApprovalRisk::Low,
        "Write"
        | "Edit"
        | "MultiEdit"
        | "NotebookEdit"
        | "TodoWrite"
        | "write"
        | "edit"
        | "ccteam_chat_send_file"
        | "ccteam_session_spawn"
        | "ccteam_session_dispatch"
        | "ccteam_session_stop" => ApprovalRisk::Medium,
        "Bash" | "KillShell" | "KillBash" | "bash" => {
            let command = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if is_destructive_command(command) {
                ApprovalRisk::High
            } else {
                ApprovalRisk::Medium
            }
        }
        _ => ApprovalRisk::Unknown,
    }
}

/// Coarse substring match for shell commands that destroy data or hand over
/// the machine — `rm -rf`, force-pushes, disk-level writes, fork bombs,
/// pipe-to-shell installers, … Intentionally broad/best-effort: a false
/// positive (flagging a harmless command `High`) costs nothing; the
/// review's complaint was the false NEGATIVE (an `rm -rf` rendering
/// identical to a `Read`), so this deliberately errs toward over-flagging.
fn is_destructive_command(command: &str) -> bool {
    let lower = command.to_lowercase();
    const DESTRUCTIVE_SUBSTRINGS: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "rm -r -f",
        "rm -f -r",
        "sudo rm",
        "mkfs",
        "dd if=",
        "dd of=",
        "git push --force",
        "git push -f",
        "git reset --hard",
        "chmod -r 777",
        "drop table",
        "drop database",
        "truncate table",
        "shutdown",
        "reboot",
        ":(){ :|:& };:",
        "curl | sh",
        "curl | bash",
        "wget | sh",
        "wget | bash",
        "> /dev/sd",
    ];
    DESTRUCTIVE_SUBSTRINGS.iter().any(|p| lower.contains(p))
}

/// Map a risk tier to its emoji prefix for the approval prompt (review
/// §3.1-2's "风险等级映射颜色/emoji" — IM/web render emoji, not CSS color,
/// so this is the portable choice for both surfaces). 🟢 read-only /
/// 🟡 mutating / 🔴 destructive / ⚪ unknown (an unrecognized tool — never
/// silently treated as safe).
pub fn risk_badge(risk: ApprovalRisk) -> &'static str {
    match risk {
        ApprovalRisk::Low => "🟢",
        ApprovalRisk::Medium => "🟡",
        ApprovalRisk::High => "🔴",
        ApprovalRisk::Unknown => "⚪",
    }
}

/// The outcome of [`ask_permission`]. All non-`Allow` outcomes are a deny —
/// callers translate them into whatever shape their protocol expects
/// (`ApprovalDecision::deny` for stream-json, a JSON-RPC `{"behavior":"deny"}`
/// / `{"timeout":true}` for the terminal hook).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAnswer {
    /// The user clicked Approve.
    Allow,
    /// The user clicked Deny.
    Deny,
    /// No click within [`permission_prompt_timeout_secs`] — fail-safe deny.
    Timeout,
    /// Couldn't even render the prompt (the event sink is closed) — fail-safe
    /// deny.
    Unavailable,
}

enum ExternalChoiceAnswer {
    Selected(ChoiceSelection),
    Timeout,
    Unavailable,
}

struct PermissionProgress<'a> {
    path: &'a std::path::Path,
    role: &'a str,
    tool_name: &'a str,
    summary: &'a str,
    ttl_secs: u64,
}

fn mint_prompt_token(prefix: char) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{prefix}{:x}", (nanos as u64) & 0xff_ffff_ffff)
}

async fn ask_external_choice(
    sink: &mpsc::UnboundedSender<GatewayEvent>,
    pending: &Arc<Mutex<PendingInteractions>>,
    ctx: &HitlPromptContext,
    sid: &str,
    prompt: ChoicePrompt,
    ttl: Duration,
    progress: Option<PermissionProgress<'_>>,
) -> ExternalChoiceAnswer {
    let token = prompt.token.clone();
    let message_options = prompt
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| MessageOption {
            data: format!("{token}:{index}"),
            label: option.label.clone(),
            id: option.id.clone(),
        })
        .collect();
    let (reply, answer) = oneshot::channel::<ChoiceSelection>();
    {
        let mut guard = pending.lock().await;
        guard.register(
            token.clone(),
            prompt.clone(),
            InteractionOrigin::External { reply },
            Instant::now() + ttl,
        );
        guard.tag_sid(&token, sid.to_string());
    }
    if sink
        .send(GatewayEvent {
            id: format!("permission-{token}"),
            channel: ctx.channel.clone(),
            chat_id: ctx.chat_id.clone(),
            thread_ts: None,
            content: prompt.title,
            kind: GatewayEventKind::Answer,
            attachments: Vec::new(),
            options: message_options,
            status: None,
            sid: Some(sid.to_string()),
            slug: None,
        })
        .is_err()
    {
        pending.lock().await.take_by_token(&token);
        return ExternalChoiceAnswer::Unavailable;
    }
    if let Some(progress) = progress {
        let event = ccteam_core::progress::build_chat_permission_prompt_outstanding_event(
            progress.role,
            progress.tool_name,
            progress.summary,
            progress.ttl_secs,
        );
        if let Err(error) = ccteam_core::progress::append_event(progress.path, &event) {
            tracing::warn!(
                sid,
                path = %progress.path.display(),
                %error,
                "hitl: failed to append permission-prompt-outstanding progress line"
            );
        }
    }
    match tokio::time::timeout(ttl, answer).await {
        Ok(Ok(selection)) => ExternalChoiceAnswer::Selected(selection),
        _ => {
            pending.lock().await.take_by_token(&token);
            ExternalChoiceAnswer::Timeout
        }
    }
}

/// Render an Approve/Deny prompt to `ctx`'s bound chat and block (bounded by
/// [`permission_prompt_timeout_secs`]) until the user clicks or the prompt
/// lapses. THE shared core both Claude HITL surfaces call so buttons / TTL /
/// deny semantics never drift between the terminal and stream-json protocols
/// (P0-2). Registers the prompt as an `External`-origin [`PendingInteractions`]
/// entry — the SAME machinery an IM inline-button click or a web
/// `POST …/resolve` already resolves (`Gateway::resolve_web_selection`), so a
/// stream-json hitl session's approval prompt is indistinguishable, from the
/// user's seat, from a terminal-protocol one.
pub async fn ask_permission(
    sink: &mpsc::UnboundedSender<GatewayEvent>,
    pending: &Arc<Mutex<PendingInteractions>>,
    ctx: &HitlPromptContext,
    sid_label: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> PermissionAnswer {
    let session_desc = if ctx.role.is_empty() {
        format!("session {sid_label}")
    } else {
        format!("session {sid_label} ({role})", role = ctx.role)
    };
    let summary = summarize_tool_input(tool_name, tool_input);
    let risk = classify_tool_risk(tool_name, tool_input);
    let title = format!(
        "{badge} {session_desc} wants to run: {summary}",
        badge = risk_badge(risk),
    );

    // Mint a short token (≤16B ASCII, no `:` — the ChoicePrompt contract).
    let token = mint_prompt_token('p');

    let prompt = ChoicePrompt {
        token: token.clone(),
        title: title.clone(),
        options: vec![
            ChoiceOption {
                id: "allow".to_string(),
                label: "✅ Approve".to_string(),
            },
            ChoiceOption {
                id: "deny".to_string(),
                label: "⛔ Deny".to_string(),
            },
        ],
        multi: false,
    };
    let ttl_secs = permission_prompt_timeout_secs();
    let ttl = Duration::from_secs(ttl_secs);

    let progress = ctx.progress_path.as_deref().map(|path| PermissionProgress {
        path,
        role: &ctx.role,
        tool_name,
        summary: &summary,
        ttl_secs,
    });
    match ask_external_choice(sink, pending, ctx, sid_label, prompt, ttl, progress).await {
        ExternalChoiceAnswer::Selected(selection) => {
            match selection.ids.first().map(String::as_str) {
                Some("allow") => PermissionAnswer::Allow,
                _ => PermissionAnswer::Deny,
            }
        }
        ExternalChoiceAnswer::Timeout => PermissionAnswer::Timeout,
        ExternalChoiceAnswer::Unavailable => PermissionAnswer::Unavailable,
    }
}

/// Production [`CanUseToolResolver`] for the stream-json protocol (v0.8.22
/// P0-2). Funnels each session's non-allowlist `can_use_tool` reverse-RPC
/// into the SAME gateway pending-approval machinery
/// [`crate::gateway::Gateway::resolve_web_selection`] and an IM inline-button
/// click already resolve, via [`ask_permission`].
///
/// Constructed once daemon startup has wired the gateway handle, pending
/// registry, and event sink (`daemon::run_daemon_with_shutdown`), and set on
/// the ONE `ClaudeStreamJsonAdapter` singleton via `set_resolver` — every
/// stream-json hitl session (spawned from IM `/new … hitl` or the web
/// `POST …/sessions {"permission_mode":"hitl"}` API) shares it, since both
/// paths spawn through the same adapter singleton.
pub struct GatewayCanUseToolResolver {
    gateway: Arc<Mutex<Gateway>>,
    pending: Arc<Mutex<PendingInteractions>>,
    sink: mpsc::UnboundedSender<GatewayEvent>,
}

impl GatewayCanUseToolResolver {
    /// Build the resolver from the daemon's already-wired gateway handle,
    /// pending registry, and event sink.
    pub fn new(
        gateway: Arc<Mutex<Gateway>>,
        pending: Arc<Mutex<PendingInteractions>>,
        sink: mpsc::UnboundedSender<GatewayEvent>,
    ) -> Self {
        Self {
            gateway,
            pending,
            sink,
        }
    }
}

#[async_trait]
impl CanUseToolResolver for GatewayCanUseToolResolver {
    /// Decide one tool-use approval for the stream-json session `sid`. Fails
    /// safe to deny (never panics, never hangs indefinitely beyond the TTL)
    /// when `sid` is not tracked or the event sink has closed — the SAME safe
    /// direction the adapter's own "no resolver wired" default took, so
    /// wiring this resolver can only ever ADD approval capability, never
    /// remove the pre-existing fail-safe.
    async fn resolve(&self, sid: &str, req: &CanUseToolReq) -> ApprovalDecision {
        let ctx = {
            let guard = self.gateway.lock().await;
            guard.hitl_prompt_context_for(sid)
        };
        let Some(ctx) = ctx else {
            return ApprovalDecision::deny(
                "HITL approval unavailable: session not tracked by the gateway (denied)",
            );
        };
        match ask_permission(
            &self.sink,
            &self.pending,
            &ctx,
            sid,
            &req.tool_name,
            &req.input,
        )
        .await
        {
            PermissionAnswer::Allow => ApprovalDecision::allow(),
            PermissionAnswer::Deny => {
                ApprovalDecision::deny("用户未批准该工具调用。Tool call not approved by the user.")
            }
            PermissionAnswer::Timeout => ApprovalDecision::deny(
                "审批超时（未在时限内响应），已拒绝。Approval timed out — denied.",
            ),
            PermissionAnswer::Unavailable => {
                ApprovalDecision::deny("HITL approval channel unavailable — denied.")
            }
        }
    }
}

#[async_trait]
impl PiInteractionResolver for GatewayCanUseToolResolver {
    fn classify_tool_risk(&self, tool_name: &str, input: &serde_json::Value) -> ApprovalRisk {
        classify_tool_risk(tool_name, input)
    }

    async fn resolve_approval(&self, sid: &str, request: &ApprovalIR) -> PiApprovalDecision {
        let tool_name = request
            .raw
            .get("toolName")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let input = request
            .raw
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if tool_name.is_empty() {
            return PiApprovalDecision::Deny(
                "Pi approval request omitted its tool identity".to_string(),
            );
        }
        let ctx = {
            let guard = self.gateway.lock().await;
            guard.hitl_prompt_context_for(sid)
        };
        let Some(ctx) = ctx else {
            return PiApprovalDecision::Deny(
                "HITL approval unavailable: session not tracked by the gateway".to_string(),
            );
        };
        match ask_permission(&self.sink, &self.pending, &ctx, sid, tool_name, &input).await {
            PermissionAnswer::Allow => PiApprovalDecision::Allow,
            PermissionAnswer::Deny => {
                PiApprovalDecision::Deny("Tool call not approved by the user".to_string())
            }
            PermissionAnswer::Timeout => PiApprovalDecision::Deny("Approval timed out".to_string()),
            PermissionAnswer::Unavailable => {
                PiApprovalDecision::Deny("HITL approval channel unavailable".to_string())
            }
        }
    }

    async fn resolve_dialog(&self, sid: &str, request: &PiDialogRequest) -> PiDialogResponse {
        let ctx = {
            let guard = self.gateway.lock().await;
            guard.hitl_prompt_context_for(sid)
        };
        let Some(ctx) = ctx else {
            return PiDialogResponse::Cancelled;
        };
        let mut prompt = ccteam_harness::execution::pi_rpc::bridge::dialog_prompt(
            mint_prompt_token('d'),
            request,
        );
        match &request.kind {
            PiDialogKind::Confirm { message } if !message.is_empty() => {
                prompt.title = format!("{}\n{}", prompt.title, message);
            }
            PiDialogKind::Input {
                placeholder: Some(placeholder),
            } if !placeholder.is_empty() => {
                prompt.title = format!("{}\nReply with text ({placeholder})", prompt.title);
            }
            PiDialogKind::Editor {
                prefill: Some(prefill),
            } if !prefill.is_empty() => {
                prompt.title = format!("{}\nReply with edited text:\n{prefill}", prompt.title);
            }
            PiDialogKind::Input { .. }
            | PiDialogKind::Editor { .. }
            | PiDialogKind::Confirm { .. }
            | PiDialogKind::Select { .. } => {}
        }
        match ask_external_choice(
            &self.sink,
            &self.pending,
            &ctx,
            sid,
            prompt,
            Duration::from_secs(permission_prompt_timeout_secs()),
            None,
        )
        .await
        {
            ExternalChoiceAnswer::Selected(selection) => match &request.kind {
                PiDialogKind::Confirm { .. } => PiDialogResponse::Confirmed(
                    selection.ids.first().map(String::as_str) == Some("true"),
                ),
                PiDialogKind::Select { .. } => selection
                    .ids
                    .into_iter()
                    .next()
                    .map(PiDialogResponse::Value)
                    .unwrap_or(PiDialogResponse::Cancelled),
                PiDialogKind::Input { .. } | PiDialogKind::Editor { .. } => selection
                    .free_text
                    .or_else(|| selection.ids.into_iter().next())
                    .map(PiDialogResponse::Value)
                    .unwrap_or(PiDialogResponse::Cancelled),
            },
            ExternalChoiceAnswer::Timeout | ExternalChoiceAnswer::Unavailable => {
                PiDialogResponse::Cancelled
            }
        }
    }

    async fn cancel_sid(&self, sid: &str) {
        self.pending.lock().await.drain_sid(sid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::GatewayEventKind;
    use serde_json::json;

    #[test]
    fn summarize_tool_input_bash_shows_the_command() {
        assert_eq!(
            summarize_tool_input("Bash", &json!({"command": "ls -la"})),
            "Bash ls -la"
        );
    }

    #[test]
    fn summarize_tool_input_generic_fallback_is_a_key_value_digest() {
        // Read has no dedicated renderer — falls to the generic digest, not
        // a raw path echo (still legible: `key: value`).
        assert_eq!(
            summarize_tool_input("Read", &json!({"file_path": "/tmp/x"})),
            "Read file_path: /tmp/x"
        );
        // Empty params → just the tool name, no dangling digest.
        assert_eq!(summarize_tool_input("Glob", &json!({})), "Glob");
    }

    /// review §3.1-2 — a `Write` approval must show WHAT is being written,
    /// not just the path (the old "见文件路径不见写入内容,等于盲批" gap).
    #[test]
    fn summarize_tool_input_write_shows_path_and_content_preview() {
        let out = summarize_tool_input(
            "Write",
            &json!({"file_path": "/a/b.rs", "content": "fn main() {}"}),
        );
        assert_eq!(out, "Write /a/b.rs\n  + fn main() {}");
    }

    /// A `Write` with no content field (rare, but shouldn't panic/garble)
    /// degrades to just the path.
    #[test]
    fn summarize_tool_input_write_without_content_shows_just_the_path() {
        assert_eq!(
            summarize_tool_input("Write", &json!({"file_path": "/a/b.rs"})),
            "Write /a/b.rs"
        );
    }

    /// review §3.1-2 — an `Edit` approval must show the old→new snippet, not
    /// just the path.
    #[test]
    fn summarize_tool_input_edit_shows_old_and_new_snippets() {
        let out = summarize_tool_input(
            "Edit",
            &json!({"file_path": "/a/b.rs", "old_string": "foo()", "new_string": "bar()"}),
        );
        assert_eq!(out, "Edit /a/b.rs\n  - foo()\n  + bar()");
    }

    #[test]
    fn summarize_tool_input_multi_edit_shows_first_snippet_and_extra_count() {
        let out = summarize_tool_input(
            "MultiEdit",
            &json!({
                "file_path": "/a/b.rs",
                "edits": [
                    {"old_string": "a", "new_string": "b"},
                    {"old_string": "c", "new_string": "d"},
                ],
            }),
        );
        assert_eq!(out, "MultiEdit /a/b.rs (+1 more edit)\n  - a\n  + b");
    }

    #[test]
    fn summarize_tool_input_truncates_long_bash_command() {
        let long = "x".repeat(500);
        let out = summarize_tool_input("Bash", &json!({"command": long}));
        assert!(out.starts_with("Bash "));
        assert!(out.ends_with('…'));
        // "Bash " (5) + COMMAND_MAX (200) + the ellipsis char.
        assert!(
            out.chars().count() <= 207,
            "got {} chars",
            out.chars().count()
        );
    }

    #[test]
    fn summarize_tool_input_truncates_long_write_content() {
        let long = "y".repeat(1000);
        let out = summarize_tool_input("Write", &json!({"file_path": "/x", "content": long}));
        assert!(out.ends_with('…'));
        assert!(out.len() < 1000, "content preview must be capped");
    }

    /// review §3.1-2 — the generic fallback must never dump a raw JSON
    /// blob: nested arrays/objects collapse to a size marker.
    #[test]
    fn summarize_tool_input_generic_never_dumps_raw_json() {
        let out = summarize_tool_input(
            "mcp__example__do_thing",
            &json!({"items": [1, 2, 3], "nested": {"a": 1, "b": 2}, "name": "x"}),
        );
        assert!(
            !out.contains("1, 2, 3"),
            "the array's raw elements must not appear: {out}"
        );
        assert!(
            out.contains("items: [3 items]"),
            "expected a size marker, got: {out}"
        );
        assert!(
            out.contains("nested: {2 keys}"),
            "expected a size marker, got: {out}"
        );
    }

    #[test]
    fn summarize_tool_input_generic_caps_total_length() {
        let mut fields = serde_json::Map::new();
        for i in 0..30 {
            fields.insert(format!("field_{i}"), json!("x".repeat(50)));
        }
        let out =
            summarize_tool_input("mcp__example__do_thing", &serde_json::Value::Object(fields));
        assert!(
            out.chars().count() < 400,
            "digest must stay capped, got {} chars",
            out.chars().count()
        );
    }

    // ---- risk classification (review §3.1-2) -------------------------------

    #[test]
    fn classify_tool_risk_read_only_tools_are_low() {
        for tool in ["Read", "Glob", "Grep", "LS", "WebFetch"] {
            assert_eq!(
                classify_tool_risk(tool, &json!({})),
                ApprovalRisk::Low,
                "{tool} should be Low"
            );
        }
    }

    #[test]
    fn pi_tool_names_use_the_shared_risk_classifier() {
        for tool in [
            "read",
            "grep",
            "find",
            "ls",
            "ccteam_status",
            "ccteam_grok_claude_codex_kimi",
            "ccteam_session_list",
            "ccteam_session_collect",
        ] {
            assert_eq!(classify_tool_risk(tool, &json!({})), ApprovalRisk::Low);
        }
        for tool in [
            "write",
            "edit",
            "ccteam_chat_send_file",
            "ccteam_session_spawn",
            "ccteam_session_dispatch",
            "ccteam_session_stop",
        ] {
            assert_eq!(classify_tool_risk(tool, &json!({})), ApprovalRisk::Medium);
        }
        assert_eq!(
            classify_tool_risk("unknown_custom", &json!({})),
            ApprovalRisk::Unknown
        );
    }

    #[test]
    fn classify_tool_risk_file_mutations_are_medium() {
        for tool in ["Write", "Edit", "MultiEdit", "NotebookEdit"] {
            assert_eq!(
                classify_tool_risk(tool, &json!({})),
                ApprovalRisk::Medium,
                "{tool} should be Medium"
            );
        }
    }

    #[test]
    fn classify_tool_risk_plain_bash_is_medium_destructive_bash_is_high() {
        assert_eq!(
            classify_tool_risk("Bash", &json!({"command": "ls -la"})),
            ApprovalRisk::Medium
        );
        assert_eq!(
            classify_tool_risk("Bash", &json!({"command": "rm -rf /tmp/x"})),
            ApprovalRisk::High
        );
        assert_eq!(
            classify_tool_risk("Bash", &json!({"command": "git push --force origin main"})),
            ApprovalRisk::High
        );
    }

    #[test]
    fn classify_tool_risk_unknown_tool_is_unknown_not_low() {
        assert_eq!(
            classify_tool_risk("mcp__example__do_thing", &json!({})),
            ApprovalRisk::Unknown,
            "an unrecognized tool must never be silently treated as safe"
        );
    }

    /// review §3.1-2's explicit ask: "rm -rf 级命令必须视觉上区别于 Read".
    #[test]
    fn destructive_bash_and_read_have_visually_distinct_badges() {
        let read_risk = classify_tool_risk("Read", &json!({"file_path": "/tmp/x"}));
        let bash_risk = classify_tool_risk("Bash", &json!({"command": "rm -rf /"}));
        assert_ne!(read_risk, bash_risk);
        assert_ne!(risk_badge(read_risk), risk_badge(bash_risk));
        assert_eq!(risk_badge(read_risk), "🟢");
        assert_eq!(risk_badge(bash_risk), "🔴");
    }

    #[test]
    fn permission_prompt_timeout_defaults_and_clamps() {
        std::env::remove_var("CCTEAM_PERMISSION_PROMPT_TTL_SECS");
        assert_eq!(
            permission_prompt_timeout_secs(),
            PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT
        );
        std::env::set_var("CCTEAM_PERMISSION_PROMPT_TTL_SECS", "0");
        assert_eq!(
            permission_prompt_timeout_secs(),
            PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT,
            "0 is clamped away (would expire instantly)"
        );
        std::env::set_var("CCTEAM_PERMISSION_PROMPT_TTL_SECS", "5");
        assert_eq!(permission_prompt_timeout_secs(), 5);
        std::env::remove_var("CCTEAM_PERMISSION_PROMPT_TTL_SECS");
    }

    fn ctx() -> HitlPromptContext {
        HitlPromptContext {
            channel: "mock".to_string(),
            chat_id: "chat-1".to_string(),
            role: "alice".to_string(),
            progress_path: None,
        }
    }

    /// `ask_permission` registers a pending + renders the prompt; approving
    /// (via the pending registry's oneshot, exactly like a resolved IM/web
    /// click) resolves `Allow`.
    #[tokio::test(flavor = "current_thread")]
    async fn ask_permission_allow_round_trip() {
        let pending = Arc::new(Mutex::new(PendingInteractions::new()));
        let (tx, mut rx) = mpsc::unbounded_channel::<GatewayEvent>();
        let pending2 = Arc::clone(&pending);
        let handle = tokio::spawn(async move {
            ask_permission(
                &tx,
                &pending2,
                &ctx(),
                "s1",
                "Bash",
                &json!({"command": "ls"}),
            )
            .await
        });

        // The prompt was rendered with Approve/Deny buttons.
        let evt = rx.recv().await.expect("prompt event sent");
        assert!(matches!(evt.kind, GatewayEventKind::Answer));
        assert_eq!(evt.sid.as_deref(), Some("s1"));
        assert_eq!(evt.options.len(), 2);
        assert!(evt.content.contains("session s1 (alice)"));
        assert!(evt.content.contains("Bash ls"));

        // Simulate the click: take the pending by token and deliver "allow"
        // over its oneshot — exactly what `Gateway::resolve_web_selection` /
        // an IM inline-button click do.
        let token = evt.options[0].data.split(':').next().unwrap().to_string();
        let taken = {
            let mut guard = pending.lock().await;
            guard.take_by_token(&token).expect("pending registered")
        };
        match taken.origin {
            InteractionOrigin::External { reply } => reply
                .send(ChoiceSelection {
                    token,
                    ids: vec!["allow".to_string()],
                    free_text: None,
                })
                .expect("resolver still waiting"),
            _ => panic!("expected External origin"),
        }

        assert_eq!(handle.await.unwrap(), PermissionAnswer::Allow);
    }

    /// The deny click resolves `Deny` — the SAME machinery, opposite click.
    #[tokio::test(flavor = "current_thread")]
    async fn ask_permission_deny_round_trip() {
        let pending = Arc::new(Mutex::new(PendingInteractions::new()));
        let (tx, mut rx) = mpsc::unbounded_channel::<GatewayEvent>();
        let pending2 = Arc::clone(&pending);
        let handle = tokio::spawn(async move {
            ask_permission(
                &tx,
                &pending2,
                &ctx(),
                "s1",
                "Bash",
                &json!({"command": "rm -rf /"}),
            )
            .await
        });

        let evt = rx.recv().await.expect("prompt event sent");
        let token = evt.options[0].data.split(':').next().unwrap().to_string();
        let taken = {
            let mut guard = pending.lock().await;
            guard.take_by_token(&token).expect("pending registered")
        };
        match taken.origin {
            InteractionOrigin::External { reply } => reply
                .send(ChoiceSelection {
                    token,
                    ids: vec!["deny".to_string()],
                    free_text: None,
                })
                .expect("resolver still waiting"),
            _ => panic!("expected External origin"),
        }

        assert_eq!(handle.await.unwrap(), PermissionAnswer::Deny);
    }

    /// No click within the TTL → fail-safe `Timeout` (never hangs forever,
    /// never silently allows).
    #[tokio::test(flavor = "current_thread")]
    async fn ask_permission_times_out_when_nobody_answers() {
        std::env::set_var("CCTEAM_PERMISSION_PROMPT_TTL_SECS", "1");
        let pending = Arc::new(Mutex::new(PendingInteractions::new()));
        let (tx, rx) = mpsc::unbounded_channel::<GatewayEvent>();
        let answer = ask_permission(&tx, &pending, &ctx(), "s1", "Bash", &json!({})).await;
        assert_eq!(answer, PermissionAnswer::Timeout);
        assert!(
            pending.lock().await.is_empty(),
            "lapsed pending is cleaned up"
        );
        drop(rx);
        std::env::remove_var("CCTEAM_PERMISSION_PROMPT_TTL_SECS");
    }

    /// A closed sink (event consumer gone) fails safe to `Unavailable`
    /// instead of hanging or panicking.
    #[tokio::test(flavor = "current_thread")]
    async fn ask_permission_unavailable_when_sink_closed() {
        let pending = Arc::new(Mutex::new(PendingInteractions::new()));
        let (tx, rx) = mpsc::unbounded_channel::<GatewayEvent>();
        drop(rx); // close the sink before anyone renders a prompt
        let answer = ask_permission(&tx, &pending, &ctx(), "s1", "Bash", &json!({})).await;
        assert_eq!(answer, PermissionAnswer::Unavailable);
        assert!(pending.lock().await.is_empty());
    }

    /// A `HarnessAdapter` that is never actually invoked — [`Gateway::new`]
    /// requires one, but `resolver_denies_when_session_untracked` never
    /// spawns a session (the gateway starts with an empty session map).
    struct NoopAdapter;

    #[async_trait]
    impl ccteam_harness::HarnessAdapter for NoopAdapter {
        fn name(&self) -> &'static str {
            "noop-test-adapter"
        }
        fn vendor(&self) -> ccteam_harness::AgentVendor {
            ccteam_harness::AgentVendor::Claude
        }
        async fn start_thread(
            &self,
            _spec: &ccteam_harness::AgentSpecBrief,
            _ctx: &ccteam_harness::SpawnCtx,
        ) -> Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by this test")
        }
        async fn submit_turn(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _input: ccteam_harness::TurnInput,
        ) -> Result<ccteam_harness::TurnId, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by this test")
        }
        async fn submit_turn_routed(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _input: ccteam_harness::TurnInput,
            _routing: ccteam_harness::TurnRouting,
        ) -> Result<ccteam_harness::TurnSubmission, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by this test")
        }
        async fn rebuild_tool_surface(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> Result<ccteam_harness::ToolSurfaceRebuild, ccteam_harness::HarnessError> {
            // Test double: no tool face to rebuild.
            Ok(ccteam_harness::ToolSurfaceRebuild::RespawnRequired {
                reason: "test double".to_string(),
            })
        }

        fn event_attachment(&self) -> ccteam_harness::EventAttachment {
            // Scripted test stream: one-shot. Re-attaching would replay
            // the script, which is exactly what `Rebuildable` forbids.
            ccteam_harness::EventAttachment::OneShot
        }

        fn events(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> futures::stream::BoxStream<'static, ccteam_harness::ThreadEvent> {
            Box::pin(futures::stream::empty())
        }
        async fn resume_thread(
            &self,
            _persistent_id: &str,
        ) -> Result<ccteam_harness::ThreadHandle, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by this test")
        }
        async fn close_thread(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> Result<(), ccteam_harness::HarnessError> {
            Ok(())
        }
        async fn handle_directive(
            &self,
            _h: &ccteam_harness::ThreadHandle,
            _d: ccteam_harness::Directive,
        ) -> Result<ccteam_harness::DirectiveOutcome, ccteam_harness::HarnessError> {
            unimplemented!("not exercised by this test")
        }
        async fn thread_status(
            &self,
            _h: &ccteam_harness::ThreadHandle,
        ) -> Result<ccteam_harness::ThreadStatus, ccteam_harness::HarnessError> {
            Ok(ccteam_harness::ThreadStatus::default())
        }
    }

    /// `GatewayCanUseToolResolver` denies fail-safe when the gateway has no
    /// tracked session for `sid` (never panics on a stale/unknown sid) — the
    /// resolver only ever ADDS approval capability, it never removes the
    /// pre-existing "no resolver wired" fail-safe direction.
    #[tokio::test(flavor = "current_thread")]
    async fn resolver_denies_when_session_untracked() {
        let gateway = Arc::new(Mutex::new(Gateway::new(
            Arc::new(NoopAdapter),
            "alpha",
            "/tmp/alpha-hitl-test",
        )));
        let pending = Arc::new(Mutex::new(PendingInteractions::new()));
        let (tx, _rx) = mpsc::unbounded_channel::<GatewayEvent>();
        let resolver = GatewayCanUseToolResolver::new(gateway, pending, tx);
        let req = CanUseToolReq {
            request_id: "r1".to_string(),
            tool_name: "Bash".to_string(),
            input: json!({"command": "ls"}),
            tool_use_id: None,
        };
        let decision = resolver.resolve("s404-never-tracked", &req).await;
        assert!(!decision.allow, "untracked sid must fail safe to deny");
    }
}
