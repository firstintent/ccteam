# V0.6.5 Wave 1 — Handoff

> **Status:** Wave 1 ship-complete · 7 / 7 findings merged · baseline **1528 / 1** (target 1516 / 1, +12 over) · clippy 0 warnings · F165 (organically-discovered tracing-stdout bug) inserted into V0.6.5 mid-wave, fix in flight as separate worktree.
> **Window:** 2026-05-24 (single calendar day, 5 + 2 + 1 parallel worktrees on Opus / Sonnet mix).
> **PRs merged:** [#94](https://github.com/firstintent/ccteam/pull/94) F149 · [#95](https://github.com/firstintent/ccteam/pull/95) F164 · [#96](https://github.com/firstintent/ccteam/pull/96) F163 · [#97](https://github.com/firstintent/ccteam/pull/97) F150 · [#98](https://github.com/firstintent/ccteam/pull/98) F146 · [#99](https://github.com/firstintent/ccteam/pull/99) F148+F151 · [#100](https://github.com/firstintent/ccteam/pull/100) F147 · [#101](https://github.com/firstintent/ccteam/pull/101) chore CLAUDE.md §七.

---

## Decided

| Finding | Decision |
|---|---|
| **F146** | 3 atomic MCPs (`chat_register_bot` / `chat_list_bots` / `chat_unregister_bot`) replace `chat_lifecycle` STUB. vendor lowercase enforced at 3 layers (schema enum / dispatch `to_lowercase()` / serde `rename_all`). Heartbeat sidecar `<root>/imd/registry/<slug>/<role>.heartbeat` (30 s mtime ⇒ `running:true`). New `register_bot_in` / `list_bots_in` / `unregister_bot_in` variants take explicit `ccteam_root` for tempdir-isolated tests; home-derived public API is a thin wrapper. **No alias kept** for the removed `chat_lifecycle` entry. Net tool count 27 (chat group = 6). |
| **F147** | 3 runtime MCPs. `chat_session_reset` → `chat_reset`, `chat_show_turn_log` → `chat_history` (direct rename, no alias — CLAUDE.md §五 守). `SupervisorAction::ResetSession` new variant; `tick_supervisors` signature extended with optional `bot_channels` so the daemon main loop coordinates in-memory cursor reset against the supervisor's disk wipe. V0.6.4 Bug B-prevention path verified: reset archives `turns.jsonl` to `archive/turns-<unix-ms>.jsonl` + `OutboundCursor::force_set(0)` + clears `transcript-cursor.json` and `outbound.cursor` on disk. `inbound::render_envelope` promoted to `pub` for MCP-side envelope hand-building (caller = trusted local meta-agent host, bypasses IM security pipeline). |
| **F148** | `/ccteam-creator` Phase 5.6/5.9 SKILL.md text rewritten from "call `ccteam_imd::register_bot(...)`" to "invoke `mcp__ccteam__chat_register_bot` MCP tool" with JSON-args examples. New `e2e_creator_full_path_test.rs` (2 cases: wire-contract + SKILL.md text guard) using stub TG provider + stub claude-tui adapter. Real round-trip deferred to Wave 4 nas-box005 host-probe. |
| **F149** | 6+ stale "Wave 1 fallback" / "Wave 2 not ready" / "Wave 3 未落地" phrases scrubbed from `skills/ccteam/SKILL.md`. Frontmatter + skill family table + routing table + dialog letter paths + Wave-status block all aligned to "已 ship" current state. Dispatcher routing logic itself untouched (this is doc-only). |
| **F150** | `skills/ccteam-control/SKILL.md` audited (already MCP-first; no `ccteam ctl` artifacts found). 6 admin smoke tests in `crates/ccteam-cli/tests/mcp_admin_smoke_test.rs` covering `admin_workflow_pause` / `admin_workflow_resume` / `admin_list_workflows` / `admin_cost_today` / `admin_stop_everything` / `admin_change_persona`. `docs/user-manual.md` §4 Admin 操作参考 added with MCP tool names for all 6. |
| **F151** | `cmd_remove::purge` now cleans `~/.ccteam/imd/registry/<slug>/` entire directory (heartbeat sidecars included). Prefer MCP `chat_unregister_bot` path (added in F146); fall back to filesystem delete only when MCP unreachable (daemon down). Dry-run output shows `would purge imd/registry/<slug>/ (N JSON file(s))`. Default `remove` (no `--purge`) leaves `imd/registry/` intact. |
| **F163** | Real bug differed from PRD: `wait_for_shutdown_signal()` SIGINT/SIGTERM handler already existed; the actual blocker was unbounded `web_handle.await` / `imd_handle.await` after orchestrator drain (axum + IMD long-poll didn't wake up promptly after shutdown channel fired → process hung forever). Fix: `TASK_DRAIN_TIMEOUT = Duration::from_secs(5)` wraps both await points; timeout branch logs WARN and proceeds to pidfile cleanup + port release. Added `tracing::info!` noting "tmux sessions left running intentionally" (CLAUDE.md §三 守). `graceful_shutdown_test.rs` 4 cases all green: SIGTERM / SIGINT / trigger-file shutdown / tmux sessions survive. `docs/interfaces.md` §CLI lifecycle gained `stop` 行为契约 row. |
| **F164** | `claude_tui::start_thread` rewritten with 3-path decision: (a) session exists + pane comm contains "claude" → reattach (no new process, update hooks + return existing handle); (b) session exists + dead pane → `tmux kill-session` then new session; (c) session absent → new session. New `is_pane_running_claude(session) -> bool` helper using `ps -o comm=` only (no pane content read). `TmuxSession::list_pane_pids() -> Vec<u32>` added via `tmux list-panes -F "#{pane_pid}"`. **Did not touch `resume_thread`.** Bonus: idempotency bug surfaced (two `start_thread` calls on alive session both succeed instead of returning `SpawnFailed`). |
| **CLAUDE.md §七** (chore PR #101) | Replaced `cargo fmt -- <files>` direction with `rustfmt --edition 2021 <files>` direct, after W1-T3 confirmed cargo fmt silently ignores positional file arguments and runs workspace-wide. Direction now lives in [[feedback_no_cargo_fmt]] auto-memory too. |

---

## Rejected (this wave)

- **`chat_register_bot` multi-tenant `im_chat_ids: Vec<String>`** — held for V0.7 (per `dev-plan.md` Wave 1 note); registry stays single-tenant `im_chat_id: String`.
- **`chat_lifecycle` deprecated alias** — explicitly removed without back-compat shim (CLAUDE.md §五: pre-v1.0 no backwards-compat).
- **F163 killing tmux child sessions on shutdown** — explicitly rejected; CLAUDE.md §三 红线 "永不主动 kill 长 session" + "tmux session 由 user 决定". Tmux session survives daemon stop; F164 reattach picks it up cleanly on next start.
- **F148 SKILL.md mentioning Rust fn name in body** — replaced with the user-facing MCP tool name only; meta-agent should never see Rust API surface.

---

## Risks

| ID | Risk | Mitigation |
|---|---|---|
| R1 | **F148 e2e is stub** (TG + claude-tui both mocked). Real fresh-machine `/ccteam-creator → TG round-trip` is unverified until host-probe. | Wave 4 nas-box005 host-probe with fresh wipe (`docs/versions/v0-6-5/README.md` §5 #3 ship gate). |
| R2 | **F163 deviation from PRD** (drain timeout vs new signal handler). Future maintainers reading the PRD may expect a fresh signal listener. | `docs/interfaces.md` §CLI lifecycle row documents the actual contract. This handoff doc captures the deviation. PR #96 description explicit "corrected from PRD". |
| R3 | **F147 reset path coupling**: `tick_supervisors` now takes optional `bot_channels` arg to coordinate in-memory cursor reset against disk wipe. Callers in tests + main daemon updated, but any future caller forgetting to pass channels will get a *partial* reset (disk clean, in-memory cursor stale) → V0.6.4 Bug B regression risk. | Type-level: argument is `Option<&BotChannelsMap>`; missing → no in-memory reset attempted (no false positive). Integration test `reset_session_archives_turns_jsonl_and_clears_transcript_cursor` asserts both sides cleaned. |
| R4 | **F164 alive-session reattach health check** trusts `ps -o comm=` to identify claude. A user-spawned `claude` process in a hijacked tmux session would pass the check and be silently adopted. | Acceptable: ccteam-managed tmux session names follow `ccteam-chat-<slug>-<role>` convention; hijack would already require explicit user `tmux new-session -s ccteam-chat-foo-bar`. Documented in claude_tui.rs comment. |
| R5 | **F165 organically discovered**: `ccteam mcp-serve` `tracing::info!` writes to stdout, collides with JSON-RPC frame channel. F147/F148/F150 worked around it via `RUST_LOG=error`. | F165 worktree in flight; fix = `init_tracing` `with_writer(io::stderr)` for stdio MCP mode, plus removal of `RUST_LOG=error` workaround in tests + new stdout-cleanliness smoke test. Will merge before Wave 2 dispatch (advise_* MCP tests would hit the same坑). |

---

## Files (changed across PRs #94–#100 + chore #101)

**Code (`crates/`):**
- `ccteam-cli/src/main.rs` (F163 drain timeout)
- `ccteam-cli/src/commands.rs` + `src/cmd_remove.rs` (F151)
- `ccteam-cli/src/mcp_serve.rs` + new `src/mcp_chat_tools.rs` (F146 + F147)
- `ccteam-imd/src/{registry,supervisor,outbound,bot_mpsc,inbound,daemon,lib}.rs` (F146 register_bot/heartbeat; F147 reset/inbox/turns_jsonl path helpers)
- `ccteam-core/src/execution/{claude_tui,transcript_tail}.rs` (F164 reattach + F147 cursor coordination)
- `ccteam-core/src/tmux.rs` (F164 list_pane_pids)

**Tests (new):**
- `ccteam-cli/tests/mcp_admin_smoke_test.rs` (F150, +6)
- `ccteam-cli/tests/graceful_shutdown_test.rs` (F163, +4)
- `ccteam-cli/tests/remove_test.rs` (F151, +2)
- `ccteam-cli/tests/e2e_creator_full_path_test.rs` (F148, +2)
- `ccteam-core/tests/claude_tui_reattach_test.rs` (F164, +6)
- `ccteam-imd/tests/chat_register_mcp_test.rs` + `chat_send_input_test.rs` + `chat_reset_signal_test.rs` + others (F146 + F147 integration, +~20)

**Docs:**
- `skills/ccteam/SKILL.md` (F149)
- `skills/ccteam-creator/SKILL.md` (F148)
- `skills/ccteam-control/SKILL.md` (F150 audit — already clean)
- `docs/interfaces.md` (F146 MCP table + F147 MCP table + F163 §CLI lifecycle)
- `docs/user-manual.md` §4 Admin (F150)
- `CLAUDE.md` §七 (chore #101)

---

## Remaining (organically discovered → not original Wave 1 scope)

- **F165** `ccteam mcp-serve` tracing → stderr — discovered W1-T3, fix in flight as separate Opus worktree; will merge **before** Wave 2 dispatch (advise_* MCP tests will hit the same stdout/JSON-RPC collision otherwise).
- **F148 / F157 / F162 / F163 / F164 nas-box005 真机 host-probe** — Wave 4 single subagent runs them, signs off in `docs/versions/v0-6-5/host-probe.md` (per ship gate #10).
- **chat MCP multi-bot per `chat_id`** support — V0.7 candidate, already noted in `dev-plan.md` Rejected.

**Strict no-leftover audit:** every Wave 1 finding (F146 / F147 / F148 / F149 / F150 / F151 / F163 / F164) shipped in this wave; **zero** items pushed to V0.6.6 / V0.7. F165 is a new finding, not a Wave 1 leftover.

---

## Next: Wave 2 gating

Wave 2 (Epic F — advise + Codex critic, 3 worktrees → target 1534/1) is **blocked on F165 merge** (advise_* MCP test infrastructure will fail under live tracing otherwise). Main session dispatches Wave 2 batch the moment F165 PR auto-merges.
