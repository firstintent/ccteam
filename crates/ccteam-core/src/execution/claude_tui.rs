//! V0.6.0 F107 — `ClaudeTuiAdapter` STUB (Wave 1).
//!
//! Wave 2 F108 will fill this in with the tmux long-session + send-keys
//! -l + dual-track transcript polling design (see
//! `docs/v0-6-0/prd.md` F108 §A/§B/§D):
//!
//! - `start_thread`:
//!     ```text
//!     tmux new-session -d -s ccteam-chat-<slug>-<role> -c <cwd> \
//!         "claude --dangerously-skip-permissions"
//!     ```
//!   plus `ensure_hooks_installed(project_dir)` to attach
//!   UserPromptSubmit / Stop / SubagentStop / SessionStart / PostToolUse
//!   hooks that emit fast `chat_progress` events to progress.jsonl.
//! - `submit_turn`:
//!     - `UserText(s)` → `tmux send-keys -l <session> <s>` +
//!       `tmux send-keys <session> Enter` (literal mode, 0 escape
//!       eaten — ccgram + OMC verified pattern).
//!     - `Artifact(p)` → send-keys literal of `"Look at the file I
//!       just placed at <p>"` so the agent uses its built-in Read
//!       tool (sidesteps stdin escape entirely).
//!     - `SystemDirective(d)` → send-keys literal `/<d>` (slash
//!       commands flow through transparent — `/compact`, `/new`,
//!       `/clear` etc., 0 ccteam filtering).
//! - `events`:
//!     - Track 1 (fast / structured): `progress_jsonl_tail(h)` —
//!       Claude Code hooks write `chat_progress` events to
//!       `progress.jsonl` and the orchestrator emits them as
//!       [`crate::harness::ThreadEvent::ItemStarted`] /
//!       `ItemCompleted` etc.
//!     - Track 2 (full content): incremental byte-offset read of
//!       `~/.claude/projects/<encoded-cwd>/<session_id>.jsonl`,
//!       cursor file at `<.ccteam/chat/<bot>/transcript-cursor.json>`,
//!       mirror parsed entries into ccteam-owned
//!       `<.ccteam/chat/<bot>/turns.jsonl>` (R2 SoT).
//! - `close_thread`: send `/exit` + `tmux kill-session` fallback.
//!
//! Wave 1 returns `HarnessError::NotImplemented` from every async
//! method, but `name()` and `vendor()` are wired correctly so trait
//! dispatch + adapter registration tests pass.

use async_trait::async_trait;
use futures::stream::{self, BoxStream};

use crate::harness::{
    AgentSpecBrief, AgentVendor, HarnessAdapter, HarnessError, SpawnCtx, ThreadEvent,
    ThreadHandle, TurnId, TurnInput,
};

/// V0.6.0 F107 [`HarnessAdapter`] for Claude Code TUI (long-running tmux
/// session, multi-turn with context reuse). **Wave 1 STUB — Wave 2 F108
/// fills the impl.**
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeTuiAdapter;

impl ClaudeTuiAdapter {
    pub const fn new() -> Self {
        Self
    }
}

const NOT_IMPL_REASON: &str =
    "Wave 2 F108 fills tmux long-session + send-keys -l direct user-content passthrough + \
     dual-track (Claude Code hooks fast event + transcript jsonl byte-offset incremental \
     read mirror) + slash command transparent passthrough; Wave 1 ships the trait surface only";

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
        _spec: &AgentSpecBrief,
        _ctx: &SpawnCtx,
    ) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: NOT_IMPL_REASON.to_string(),
        })
    }

    async fn submit_turn(
        &self,
        _h: &ThreadHandle,
        _input: TurnInput,
    ) -> Result<TurnId, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: NOT_IMPL_REASON.to_string(),
        })
    }

    fn events(&self, _h: &ThreadHandle) -> BoxStream<'static, ThreadEvent> {
        // Stubs return an empty stream so callers can compose without
        // panicking; behavioural impl is Wave 2.
        Box::pin(stream::empty())
    }

    async fn resume_thread(&self, _persistent_id: &str) -> Result<ThreadHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: NOT_IMPL_REASON.to_string(),
        })
    }

    async fn close_thread(&self, _h: &ThreadHandle) -> Result<(), HarnessError> {
        Err(HarnessError::NotImplemented {
            reason: NOT_IMPL_REASON.to_string(),
        })
    }
}
