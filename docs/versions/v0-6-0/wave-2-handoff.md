# V0.6.0 Wave 2 — Handoff

> **Status**: Shipping(integration branch `wave2-integration`,4-way merge done,baseline + clippy + 15 红线 全绿)。
> **Baseline**: 1201 / 1 — Wave 1 baseline 990/1 + 211 net new tests;1 fail = pre-existing SSE flake `workflow_summary_reflects_agent_spawn_and_done_events`(CLAUDE.md §一 已挂账)。
> **Clippy**: 0 errors,19 warnings(+2 vs Wave 1 17,doc-list drift category;非新逻辑 warning)。
> **Wall time**: 4 teammate 并行 worktree ~45 min(含 creator/imd 自调 2 轮 peer DM);主 session 整合 + 4-way merge ~15 min。

## Decided

- **F108 ClaudeTuiAdapter 真 impl** — tmux long-session + `tmux send-keys -l` 直送 user content + dual-track output(Claude Code 官方 hooks → progress.jsonl 业务事件 + Anthropic internal transcript jsonl byte-offset 增量读 → mirror 到 ccteam-owned `<bot>/turns.jsonl`)+ slash 命令(`/compact /new /clear` etc.)透明透传。
- **F118 session recovery** — `session_recovery::build_recovery_prompt` 从 `<bot>/turns.jsonl` 重建 last-N turn 作 system prompt 注入新 tmux + claude(默认 N=20)。
- **F109 ccteam-imd daemon(new crate `crates/ccteam-imd/`)** — Reply Listener 风格 single daemon(boot + heartbeat + supervisor + graceful shutdown via tokio::signal + max_runtime watchdog);Option C vendor-in-crate TG/Slack/Discord 3 provider(`src/transport/providers/`),避免拉 openhuman 整 crate 巨型 transitive deps(whisper-rs / tauri / AI libs)。
- **F116 supervisor + 三层 OMC 安全** — `three_layer_sec.rs` 集成 sig auth + rate limit(per-user token bucket)+ content validation(sanitize.rs:backtick/`$()`/`${}`/control/bidi 两层剥)。per-bot tmux heartbeat + signal files(`shutdown.signal` / `drain.signal`)+ crash 自动重启。
- **F114 ccteam-creator skill 复活** — V0.5.0 F100 砍掉的 dialogue skill 复活;6-phase NL flow(intent → mode inference → persona match → output PROJECT PLAN → on 'go' execute → fallback)。`mode_inferrer::infer_mode` rule-based + 置信度 + NeedsClarification 兜底。`agent_naming::SCIENTIST_NAMES` 50+ pool(借 Codex)。
- **5 workflow.yaml 模板**(`crates/ccteam-core/src/templates/workflow_templates/{inproc-solo, inproc-team, bg-overnight, chat-pocket, chat-squad}.yaml`)对应 5 preset。
- **7 prefab persona zh+en**(14 role.md)— tech-helper / writing-assistant / translator / tutor / project-lead / customer-support / code-critic;manifest.toml registry。
- **F117 `/ccteam-im-setup` skill** — 4-step TG onboarding(BotFather URL → token → `onboarding::telegram_setup` getMe verify + getUpdates auto-detect chat_id → write `~/.ccteam/im/credentials.json` 0600)。
- **F115 `.ccteam/handoffs/<slug>/stage-<N>-<role>.md`** — handoff hook(write_handoff atomic + list_handoffs sorted + read_concat last-N + sanitize_component path-traversal hardening)+ `{{include_prev_handoffs}}` template directive(spawn_brief.rs 渲染)+ fix-loop 3-strike escalation 追加 handoff trace 到 meta-agent inbox + agent .md 模板加"When you're done with a stage"指引段。
- **6 chat_* progress event 类型**(`chat_session_started, chat_turn_user_prompt, chat_turn_completed, chat_session_reset, chat_session_reset_with_recovery, chat_compact_done, chat_hop_escalate`)+ `ChatSpec` schema(`bot_name, compact_every_turns, hop_limit=3, recover_last_n_turns=20, chat_acl`)。
- **`ccteam internal hook chat-progress <event>` subcommand** — 装到 .claude/settings.json hooks(UserPromptSubmit/Stop/SubagentStop/SessionStart/PostToolUse/pre-compact/post-compact)写 progress.jsonl + 触发 turns.jsonl mirror。

## Rejected

- ~~openhuman 整 crate 作 path dep(Option A)~~ — imd 选 Option C(vendor TG/Slack/Discord in-crate)避免 whisper-rs/tauri 等无关 transitive deps + 编译时间爆炸。详 `docs/versions/v0-6-0/wave-2-decisions.md` §3。
- ~~`crates/ccteam-channels` 抽 openhuman channels module(Option B)~~ — 3 个 provider slim 化后总共 ~600 行,新建 crate 的 ceremony 成本不抵。Option C 直接 in-crate `src/transport/providers/{telegram,slack,discord}.rs`。
- ~~creator 单独写 `register.rs`~~ — imd 的 `lib.rs::register_bot()` 是 canonical impl(签名 `register_bot(slug, role, AgentVendor, im_platform, im_chat_id) -> Result<PathBuf>`);creator 的 stub 合并时按 merge plan 删除。
- ~~creator 单独写 `tests/credentials_roundtrip_test.rs`~~ — 与 imd 的 `tests/credentials_test.rs` 功能 dup,merge 时删。
- ~~`-D clippy warnings` gate~~ — main 已有 17 pre-existing doc-list drift warnings(V0.5.x 遗留);Wave 2 加 2 warning(总 19)。Wave 4 doc-sweep chore PR 一次性清。

## Risks(进 Wave 3 / Wave 4 兜)

- **imd↔tui e2e wiring 未通**:imd daemon ships 完整 skeleton(inbound 写 mailbox 文件 + outbound 读 turns.jsonl),但 `daemon::supervisor` 内 `HarnessAdapter::submit_turn` 调用是 conceptual comment 不是实际代码 — orchestrator 还没把 chat workflow 路由到 ClaudeTuiAdapter。**真 e2e mode 3 跑通需要 Wave 3 follow-up commit**(orchestrator 加 mode:chat 分发 + 给 chat agent 起 ClaudeTuiAdapter 不是 ClaudeBgAdapter)。当前状态:所有单元测试 + skeleton 测试 ✓;真 IM 来回单元测试 mocked(Channel mock impl + fake claude)。
- **真 TG host probe 未跑** — TG bot token + chat_id 在 ~/.ccteam/im/credentials.json 0600 已落,但本 wave 没起真 tmux + 真 claude TUI + 真 TG webhook。Wave 4 host probe 会跑(user 已 paste token + `/start`'d bot)。
- **OpenAI pricing 数据 stale 风险** — Wave 1 cost-crater verify @ 2026-05-19;Wave 4 加 `ccteam doctor --check-pricing-version` 检查脚手架。
- **doc-list drift 17 → 19** — Wave 2 加了 chat_* event docs 引入 2 个新 doc-overindented-list warnings。Wave 4 doc-sweep 一次性清。

## Files

新文件(40+):
- `crates/ccteam-core/src/{handoff,spawn_brief,agent_naming,mode_inferrer}.rs`
- `crates/ccteam-core/src/execution/{transcript_tail,turns_mirror,session_recovery}.rs`
- `crates/ccteam-core/src/templates/workflow_templates/{mod.rs, 5 yaml}`
- `crates/ccteam-hooks/src/chat_progress.rs`
- `crates/ccteam-imd/` 完整新 crate(15 src files + 5 test files + systemd unit + README)
- `skills/ccteam-creator/SKILL.md` + `skills/ccteam-creator/personas/{7 dir × zh+en}/role.md` + manifest.toml
- `skills/ccteam-im-setup/SKILL.md`
- `crates/ccteam-core/tests/{agent_naming, mode_inferrer, persona, workflow_templates, claude_tui, transcript_tail, turns_mirror, session_recovery, handoff, spawn_brief}_test.rs`(10 new)
- `crates/ccteam-hooks/tests/chat_progress_test.rs`
- `docs/versions/v0-6-0/{wave-2-decisions, wave-2-tui-impl-handoff, wave-2-handoff}.md`

修改:
- `crates/ccteam-core/src/{lib.rs, orchestrator.rs, progress.rs, workflow.rs, tmux.rs, execution/{mod.rs, claude_tui.rs}, templates/mod.rs}`
- `crates/ccteam-cli/src/{main.rs, commands.rs}`(`ccteam internal hook chat-progress` + `ccteam daemon {start|stop|status}` 路由)
- `crates/ccteam-hooks/{Cargo.toml, src/lib.rs}`
- `agents/{__lead, explorer}.md`(handoff 指引段)
- Workspace `Cargo.toml`(加 `ccteam-imd` member)
- `Cargo.lock`(regenerated)

## Remaining(Wave 3 接力)

- **F112 Codex Option B 完整**:
  - `CodexExecAdapter::{submit_turn, resume_thread}` 真 impl(codex exec --json stdin + `codex resume <UUID>` — codex CLI 0.131.0 + ChatGPT auth 已就位)
  - 新 `crates/ccteam-core/src/execution/codex_app_server.rs` — `CodexAppServerAdapter` UDS JSON-RPC v2 client
  - 4 用户场景:
    - `/ccteam-advise <hard question>` parallel voting skill(`skills/ccteam-advise/SKILL.md` — Wave 1 已 stub)
    - Auto-critic in `ccteam-creator`(检测 codex binary + auth → critic role 自动 `vendor: codex`)
    - `/ccteam-team` 内置 Codex critic teammate
    - opt-in fallback prefs(`~/.ccteam/preferences.toml` `fallback.on_claude_quota = "codex|off"`)
  - per-vendor budget UI + cost 聚合显示(`/ccteam-control show-cost`)
- **imd↔tui orchestrator wiring**(本来计划 Wave 2 follow-up commit,但范围比预期大,推 Wave 3):
  - orchestrator 加 mode:chat 分发逻辑(chat workflow 起 `ClaudeTuiAdapter` 不是 `ClaudeBgAdapter`)
  - imd supervisor 内真 `HarnessAdapter::{start_thread, submit_turn, events, close_thread}` 调用 + ThreadHandle ownership per-bot
  - 真 TG e2e mock test(Channel mock + fake claude + verify 完整 IM ↔ bot ↔ IM 单元链路)

## 给 Wave 3 的接力

- Codex CLI 已装 + ChatGPT auth ok(`codex login status` 已验)→ Wave 3 host probe 可以跑真 codex(不 mock)
- 所有 Wave 1 + Wave 2 红线已守;Wave 3 trait sig 仍是 Wave 1 lock 的 5-method(start_thread/submit_turn/events/resume_thread/close_thread)
- imd↔tui wiring 是 Wave 3 architect teammate 第一活
- per-vendor budget cap 已在 `ccteam-cost::budget::Budgets` 准备好,Wave 3 接 UI 显示
