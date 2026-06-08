# v0.8.9 Wave 0 Handoff — clean prompt content + dead-links + zero-prompt red-line

> Phase 0 of the v0.8.9 run (see `dev-prompt.md`). Three sub-commits on `dev`: `7d9e86c` (0a), `40db1d1` (0b), `8162a35` (0c).

## Decided
- **Split Phase 0 into 3 file-disjoint sub-commits**, each a sequential foreground opus agent with a gate between (the v0.8.8-proven pattern; mutating workflows are fragile):
  - **0a** — delete the dead `BotSupervisor + filesystem-queue outbound/inbound + cross-bot mention` chain (ccteam-im). Dead since the v0.8.2 gateway rewrite + v0.8.8 F1 (gateway is the sole live turns writer; daemon logs "no supervisor tick").
  - **0b** — remove the two dead chat MCP tools `chat_send_input` (wrote a mailbox the deleted supervisor never drained) + `chat_history` (read a role-keyed turns.jsonl F1 stopped writing). MCP surface **17 → 15** (chat group **6 → 4**).
  - **0c** — retire the legacy agent-team init mode + meta-agent + delete the remaining prompt templates; add the zero-prompt-content red-line.
- **Full retire of `InitMode::AgentTeam`** (Option A, NOT inline placeholders). The dev-prompt's literal "delete 5 files" was under-specified: the prompt files were load-bearing in the agent-team init mode (2 `include_str!` → compile breaks). Since agent-team mode is retired-legacy and the zero-prompt red-line is the point, removed the `InitMode` enum + `--mode` flag + agent-team scaffold/start paths + their CLI/flow tests. `ccteam init` now always scaffolds the artifact-driven `workflow.yaml`.
- `meta_agent.rs` deleted (dead end-to-end; only its own tests called it); `meta_slug()` (literally `"meta"`) inlined into `watchdog.rs`.
- **CLAUDE.md §三**: added the `ccteam repo 零提示词类型插件` red-line (only `cto_role.md` excepted); MCP count 17→15 / chat 6→4.

## Rejected
- **Option B** (inline placeholder yaml/`__lead.md` to keep agent-team init mode alive): leaves prompt-ish content + a retired mode alive — violates the red-line spirit.
- **Deleting `inbound_wiring_test.rs`** (the recon mis-listed it): it is mostly LIVE gateway/durable-outbound coverage → KEPT (agent 0a caught the recon error).
- **Ripping out** the harness `marker_reporter` registry/trait + the `inbound::process_inbound*` / `nl_admin::AdminExecutor` dead fns: those are dead fns inside LIVE modules — out of Phase 0 scope, deferred as separate surgical passes (avoid widening the diff into the execution adapter / live inbound module).

## Risks
- **`leave-running` is now a half-feature**: its purpose (enable a later `--restart-team`) is gone. The user-facing hint was fixed (`--restart-team` → `claude attach`), and `run_stop_slug`/`leave_running` stay coherent, but the whole agent-team CLI lifecycle (stop/attach/leave-running) is arguably retire-able alongside the deferred ccteam-flow orchestrator. **Low severity** — agent-team projects are no longer creatable via `init`.
- **Deferred pub-inert dead code** (left clippy-safe, flag for a future deep dead-code pass): `write_project_settings_agent_team` + `settings.agent-team.json`, `push_alert_to_meta_outbox` (watchdog meta-outbox, consumer was the meta-agent), `inbound::{render_envelope,parse_envelope,InboxEnvelope}` (now unused after `chat_send_input` removal but `pub`).

## Files
- **Deleted**: `ccteam-im/src/{supervisor,bot_mpsc,outbound}.rs` + 11 dead-chain test files; `ccteam-core/src/meta_agent.rs` + `templates/{meta_agent_role.md,workflow.agent-team.yaml,squad_roster.rs}`; root `agents/` + `workflows/dev-flow/`; `ccteam-flow/tests/dev_flow_template_parses.rs`; the `chat_history`+`chat_send_input` MCP tool surface.
- **Edited**: `ccteam-im/src/{lib,daemon,nl_admin}.rs` + pruned 2 mixed test files; `ccteam-cli/src/{commands,main,mcp_chat_tools,mcp_serve,mcp_session_tools,mcp_admin_tools,mcp_advise_tools,mcp_tool_groups}.rs` + mcp tests; `ccteam-core/src/{lib,watchdog,projects,templates/mod}.rs`; `CLAUDE.md`.

## Remaining (later phases / follow-ups)
- **Phase 2** removes `agency_agents_catalog.json` + the direct-github role-import (after the hub marketplace backend lands).
- **Phase 5** docs: `docs/tech-design.md` + `docs/usage.md` still say 17 MCP tools → update to 15 / chat 4.
- **Deep dead-code pass** (deferred): harness `marker_reporter` + trait; `inbound::process_inbound*` / `nl_admin::AdminExecutor`; the pub-dead agent-team settings helpers + watchdog meta-outbox; the agent-team CLI lifecycle (stop/leave-running) if the ccteam-flow orchestrator is retired.
- **`--restart-team` residue in the (unreachable) ccteam-flow orchestrator** (review P2): `orchestrator.rs:1444` runtime hint + doc-comments in `orchestrator.rs`/`workflow.rs`/`state.rs` still name the removed `--restart-team` flag. The daemon does NOT construct the `Orchestrator` (deferred), so unreachable in production — clean it when/if the orchestrator is revived.
- **`memory_bridge_{dev,research}.md`** (review P2, FYI): the only other `include_str!`'d `.md` bodies besides `cto_role.md` — cross-project memory-file *scaffolds* (not personas, so outside the red-line's persona-body scope) that reference the old `teams/` retro subsystem. A future phase may retire the dead `teams/` retro system + these scaffolds.

## Gate
`cargo test --workspace --exclude ccteam-web` **1894/0** (was 1999 — 105 deleted dead tests, fail=0, all net-decrease from intentional deletion); `cargo clippy --workspace --all-targets -D warnings` **0**; `cargo fmt --all --check` clean; `ccteam-web` 224 + 4 env-gated `ws_*`; `doctor --verify-mcp` would report 15. **Adversarial review: SHIP-READY, no P0/P1** — 3 P2 residues all accepted/deferred (the agent-team `--install-meta-agent`/`--restart-team` doc residue in `main.rs` was cleaned into `8162a35`; orchestrator + memory_bridge residue deferred above).
