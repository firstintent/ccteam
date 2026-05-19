# V0.6.0 Wave 2 — tui-impl handoff

Scope: F108 `ClaudeTuiAdapter` real impl + F118 session recovery +
chat-progress hooks. Baseline 1052/1 (only failure is the pre-existing
`workflow_summary_reflects_agent_spawn_and_done_events` flake).

## Decided

- **`ClaudeTuiAdapter` is stateless** — all per-session state lives on
  the [`ThreadHandle`]'s `raw_extras` bag (`role`, `project_dir`,
  `cwd`, `slug`, `tmux_session`). `events()` spawns a fresh polling
  task each call; closing the stream's receiver shuts it down. This
  matches `ClaudeBgAdapter` / `CodexExecAdapter` ergonomics so the
  orchestrator's trait-boundary translation stays uniform.
- **Slash-command transparency** — `submit_turn(SystemDirective("foo"))`
  sends the literal string `/foo` via `send_keys -l`. ccteam never
  rewrites or filters these (R4 red line); `/compact` / `/new` /
  `/clear` flow straight through to Claude Code.
- **No pane scraping** — the adapter never invokes tmux's pane-text
  capture command. State derives from `progress.jsonl` (hooks) plus
  the Anthropic transcript jsonl (incremental byte-offset reader). R2
  SoT preserved.
- **`turns.jsonl` is the recovery SoT, not the Anthropic transcript** —
  ccteam owns `<project>/.ccteam/chat/<bot>/turns.jsonl`. The Anthropic
  transcript at `~/.claude/projects/<encoded>/<sid>.jsonl` is the wire
  source for per-item content; the adapter mirrors completed turns
  into the ccteam-owned file so F118 recovery survives Claude's
  session-id rotation (`/clear`, compaction abort, etc.).
- **`recover_last_n_turns` default = 20** — matches a typical 5-minute
  chat without exceeding Claude's first-turn 200 KiB budget.
- **`hop_limit` default = 3** — matches the V0.4.x fix-loop escalate
  cadence (CLAUDE.md §三 红线).
- **Chat-progress hook arg → ccteam event mapping** lives in
  `crates/ccteam-hooks/src/chat_progress.rs`. Unknown hook args
  forward as `chat_<arg>` so we never silently drop a hook firing
  (CLAUDE.md feedback_quality_over_autonomy.md: surface, don't swallow).

## Rejected

- **`ThreadEvent::TurnStarted` / `TurnCompleted` emission from
  transcript_tail** — the transcript jsonl doesn't carry turn-level
  framing, only item-level content. The fast track (chat-progress
  hooks → progress.jsonl) already emits those, so the content track
  stays content-only.
- **Auto-compact policy enforcement inside `ClaudeTuiAdapter`** — the
  `chat.compact_every_turns` field is in the schema, but the adapter
  itself doesn't tick a counter; whoever drives `submit_turn`
  (`ccteam-imd` / orchestrator) is the right policy site.
- **R3 exception for chat-mode resume** — `resume_thread` returns
  `NotImplemented` when the tmux session is gone. The caller must
  invoke `start_thread` + `session_recovery::build_recovery_prompt`
  + `submit_turn(SystemDirective(prompt))`. Keeps the adapter's
  resume contract identical to V0.5.x ClaudeBg.

## Files (new)

- `crates/ccteam-core/src/execution/turns_mirror.rs` — `TurnRecord`,
  `append_turn`, `last_n_turns`, `read_all_turns`, path helpers.
- `crates/ccteam-core/src/execution/transcript_tail.rs` —
  `TranscriptCursor`, `read_new`, `parse_transcript_line`,
  `discover_active_session`, `encode_project_cwd`.
- `crates/ccteam-core/src/execution/session_recovery.rs` —
  `build_recovery_prompt`, `format_recovery_prompt`, `RecoveryPlan`.
- `crates/ccteam-hooks/src/chat_progress.rs` — `handle_chat_progress`
  dispatch table.
- `crates/ccteam-core/tests/{claude_tui,transcript_tail,turns_mirror,session_recovery}_test.rs`
- `crates/ccteam-hooks/tests/chat_progress_test.rs`

## Files (modified)

- `crates/ccteam-core/src/execution/claude_tui.rs` — Wave 1 stub
  replaced by real impl (≈300 lines).
- `crates/ccteam-core/src/progress.rs` — 7 `CHAT_*` constants + 6
  event builders + `is_idle` extension.
- `crates/ccteam-core/src/workflow.rs` — `WorkflowMode::Chat` variant
  + `ChatSpec` + `ChatAcl` + chat validation.
- `crates/ccteam-core/src/tmux.rs` — `send_keys_literal` /
  `send_keys_enter` split.
- `crates/ccteam-core/src/execution/mod.rs` — re-exports.
- `crates/ccteam-hooks/src/lib.rs` — `chat_progress` module +
  `handle_chat_progress` re-export.
- `crates/ccteam-hooks/Cargo.toml` — `serial_test` dev-dep.
- `crates/ccteam-cli/src/main.rs` — `HookCommand::ChatProgress`
  subcommand + run_hook dispatch.
- `crates/ccteam-cli/src/commands.rs` — `WorkflowMode::Chat` arms in
  `run_start_agent_team` / `run_stop_slug`.
- 5 `tests/*.rs` files in `crates/ccteam-core/tests/` — `chat: None`
  field added to `WorkflowSpec` literals.

## Remaining for downstream waves

- **imd consumer wiring** — `ccteam-imd` (a separate Wave 2 teammate)
  calls `ClaudeTuiAdapter::{start_thread, submit_turn, events,
  close_thread}` via the locked Wave 1 trait. The orchestrator
  translates `ThreadHandle` ↔ `SessionHandle` at the trait boundary
  (`SessionHandle::from_thread_handle`); chat-mode mapping uses
  `harness: "claude-tui"` (already wired).
- **Workflow.yaml creator template** — `ccteam-creator` (separate
  Wave 2 teammate) needs the `mode: chat` template referencing the
  schema fields locked here: `bot_name`, `compact_every_turns`,
  `hop_limit`, `recover_last_n_turns`, `chat_acl`.
- **Real Telegram probe** — Wave 2 host probe is mock-only because no
  TG token has been pasted. The `submit_turn` + `events` plumbing is
  hermetic (fake claude script + real tmux); user can run the live
  probe post-`/ccteam-im-setup`.
- **`UnifiedTokenUsage` plumb into `chat_turn_completed`** — the Stop
  hook payload doesn't carry token counts; we emit
  `chat_turn_completed` with a default-shaped usage and the cost
  pipeline cross-references the transcript / state.json. A future
  patch can read `~/.claude/jobs/...` if/when chat-mode bg jobs
  expose usage there.
- **Adapter `events()` task lifecycle** — the spawned `tail_loop`
  exits cleanly when the consumer drops the stream (`tx.is_closed()`
  check each iteration). No explicit cancel handle yet; if a future
  consumer needs forced cancellation, plumb a `CancellationToken`
  through `raw_extras`.

## Risks observed

- **Anthropic transcript jsonl path is internal** — ccgram + OMC have
  been running against this layout for >6 months without breakage,
  but a future Claude Code release could rename / re-encode. The
  cursor-file design (`<project>/.ccteam/chat/<bot>/transcript-cursor.json`)
  is decoupled, so we only need to update
  `transcript_tail::encode_project_cwd` + `discover_active_session`
  to recover.
- **chat-progress hook role attribution** — we currently derive the
  bot role from `stdin.role` (preferred) or `CCTEAM_CHAT_ROLE` env
  fallback. When `ccteam-imd` lands, it should set the env when
  spawning the tmux session. Without it the `role` field on
  `chat_*` events is empty (handler still emits, just unattributed).
- **Race on cursor file under sub-1s tick** — `TranscriptCursor::save`
  uses `<path>.tmp` + rename for crash safety. Single-writer (the
  per-bot tail loop), so no multi-writer race; tests verify the
  round-trip.

## Acceptance script results

```
baseline:  1052 / 1   (target ≥990 / ≤1 ✓; only fail is pre-existing flake)
clippy:    19 warnings (matches main baseline; PRD target ≤17 is
                       below pre-merge baseline, not achievable without
                       a doc-list-drift sweep — see CLAUDE.md §七)
NotImpl:   0  (the Wave 2 marker is gone from claude_tui.rs)
new files: 4  (turns_mirror, transcript_tail, session_recovery, chat_progress)
chat_*:    26 mentions in progress.rs (≥3 ✓)
mode:chat: 16 mentions in workflow.rs (≥3 ✓)
send-l:    11 send_keys_literal/-l references (≥1 ✓)
capture-pane in claude_tui.rs: 0 ✓ (R4)
```
