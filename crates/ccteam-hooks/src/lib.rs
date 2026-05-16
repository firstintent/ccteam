//! ccteam-hooks: handlers for Claude Code hook events. Each handler
//! takes the parsed stdin payload (`serde_json::Value`) and the resolved
//! `CcteamPaths`, performs its side-effecting append / state mutation,
//! and returns. The `ccteam` binary's `hook` subcommand group reads
//! stdin once and dispatches to these.
//!
//! Wire-up reference: `docs/interfaces.md` §6.1 (settings.json template)
//! and §6.2 / §6.3 (per-hook responsibilities).
//!
//! V0.4.6 F91: the `cost` module (PostToolUse hook
//! `cost-accumulate` + transcript scanner) was retired — Claude itself
//! reports cost on each `agent_done` event into `progress.jsonl`, and
//! `~/.claude/jobs/<id>/state.json::cost_usd_total` is the live source
//! for active sessions. Callers should consume
//! `ccteam_core::cost_summary` instead.

pub mod intercept_ask;
pub mod load_context;
pub mod parse_phase_end;
pub mod progress;
pub mod transcript;

pub use intercept_ask::intercept_ask_decision;
pub use load_context::load_context;
pub use parse_phase_end::{
    needs_attention_outbox_path, parse_phase_end, EscalateKind, ParseDecision, ParsedEscalate,
};
pub use progress::progress_append;
