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
use ccteam_harness::{ChoiceOption, ChoicePrompt, ChoiceSelection};
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

/// Render a short, human-readable one-liner of a tool call for the approval
/// prompt. Picks the most useful field per common tool (`Bash` → command,
/// file tools → path) and truncates so the IM message stays compact. Falls
/// back to the tool name when no obvious field exists.
pub fn summarize_tool_input(tool_name: &str, tool_input: &serde_json::Value) -> String {
    const MAX: usize = 160;
    let pick = |key: &str| tool_input.get(key).and_then(|v| v.as_str());
    let detail = pick("command")
        .or_else(|| pick("file_path"))
        .or_else(|| pick("path"))
        .or_else(|| pick("url"))
        .or_else(|| pick("pattern"));
    let body = match detail {
        Some(d) => format!("{tool_name} {d}"),
        None => tool_name.to_string(),
    };
    if body.chars().count() > MAX {
        let truncated: String = body.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        body
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
    let title = format!("{session_desc} wants to run: {summary}");

    // Mint a short token (≤16B ASCII, no `:` — the ChoicePrompt contract).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("p{:x}", (nanos as u64) & 0xff_ffff_ffff);

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
    let message_options: Vec<MessageOption> = prompt
        .options
        .iter()
        .enumerate()
        .map(|(i, opt)| MessageOption {
            data: format!("{token}:{i}"),
            label: opt.label.clone(),
            id: opt.id.clone(),
        })
        .collect();

    // Register the External-origin pending (token-keyed); release the guard
    // BEFORE the long await (lock discipline §7-1).
    let (tx, rx) = oneshot::channel::<ChoiceSelection>();
    let ttl_secs = permission_prompt_timeout_secs();
    let ttl = Duration::from_secs(ttl_secs);
    {
        let mut guard = pending.lock().await;
        guard.register(
            token.clone(),
            prompt.clone(),
            InteractionOrigin::External { reply: tx },
            Instant::now() + ttl,
        );
    }

    // Render the approve/deny buttons in IM/web.
    let sent = sink.send(GatewayEvent {
        id: format!("permission-{token}"),
        channel: ctx.channel.clone(),
        chat_id: ctx.chat_id.clone(),
        thread_ts: None,
        content: title,
        kind: GatewayEventKind::Answer,
        attachments: Vec::new(),
        options: message_options,
        // sid set so a per-session web UI stream can show the approval
        // (None would route to IM fine but be filtered out of SSE).
        sid: Some(sid_label.to_string()),
    });
    if sent.is_err() {
        pending.lock().await.take_by_token(&token);
        return PermissionAnswer::Unavailable;
    }

    // Best-effort operator-visibility line: PARKED awaiting approval, not
    // silently stuck. Never blocks/affects the approval flow.
    if let Some(progress_path) = ctx.progress_path.as_ref() {
        let ev = ccteam_core::progress::build_chat_permission_prompt_outstanding_event(
            &ctx.role, tool_name, &summary, ttl_secs,
        );
        if let Err(err) = ccteam_core::progress::append_event(progress_path, &ev) {
            tracing::warn!(
                sid = %sid_label,
                path = %progress_path.display(),
                error = %err,
                "hitl: failed to append permission-prompt-outstanding progress line"
            );
        }
    }

    // Block on the click, holding NO lock. TTL enforced here; on lapse the
    // caller degrades to deny.
    match tokio::time::timeout(ttl, rx).await {
        Ok(Ok(selection)) => match selection.ids.first().map(String::as_str) {
            Some("allow") => PermissionAnswer::Allow,
            _ => PermissionAnswer::Deny,
        },
        _ => {
            pending.lock().await.take_by_token(&token);
            PermissionAnswer::Timeout
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::GatewayEventKind;
    use serde_json::json;

    #[test]
    fn summarize_tool_input_picks_the_useful_field() {
        assert_eq!(
            summarize_tool_input("Bash", &json!({"command": "ls -la"})),
            "Bash ls -la"
        );
        assert_eq!(
            summarize_tool_input("Read", &json!({"file_path": "/tmp/x"})),
            "Read /tmp/x"
        );
        assert_eq!(summarize_tool_input("Glob", &json!({})), "Glob");
    }

    #[test]
    fn summarize_tool_input_truncates_long_detail() {
        let long = "x".repeat(300);
        let out = summarize_tool_input("Bash", &json!({"command": long}));
        assert!(
            out.chars().count() <= 161,
            "got {} chars",
            out.chars().count()
        );
        assert!(out.ends_with('…'));
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
