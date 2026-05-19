# V0.6.0 Wave 1 — Handoff

> **Status**: Shipped 2026-05-19 via #69 squash-merge to main(commit `887b750`)。
> **Baseline**: 990 / 1 — V0.5.1 baseline 942/1 + 48 net new tests;1 fail = pre-existing SSE flake `workflow_summary_reflects_agent_spawn_and_done_events`(CLAUDE.md §一 已挂账)。
> **Clippy**: 0 errors,17 warnings(-1 vs V0.5.1 baseline 18,全 pre-existing doc-list drift)。
> **Wall time**: 3 teammate 并行 worktree ~30 min;主 session 整合 + PR ~10 min。

## Decided

- **F107 Option C** — `HarnessAdapter` trait 旧 5 方法全删,改 5-method thread/turn 接口对齐 Codex `ThreadManager::{submit, next_event}`。`SessionHandle` 保留作 orchestrator-internal 数据类型(state.json + web SSE 兼容),`SessionHandle::from_thread_handle` 在 adapter 边界翻译。
- **ccteam-cost 抽 workspace crate** — `crates/ccteam-cost/` 含 pricing/level/budget + `UnifiedTokenUsage`(canonical home)+ Anthropic / OpenAI 双 vendor pricing TOML + `Vendor` enum。`classify(cost, soft_warn, hard_kill)` 签名改 primitives 使 `ccteam-cost` zero-dep on `ccteam-core`(`cargo tree -p ccteam-cost | grep ccteam-core` = 0)。V0.5 callers 通过 `ccteam_core::lib.rs` 的 `pub use ccteam_cost::*` re-export 零代码改动。
- **MCP namespace 保留 `ccteam`** — F110 上版的 ccteam→ct rename 取消,只加 group 子前缀(`workflow_/chat_/advise_/admin_/screenshot`)。V0.5 用户 `~/.claude.json` 配置零 break。
- **Mode 3 实施路径 flip** — 弃 `claude -p --resume` + stream-json + stdin pipe;改 tmux long-session + `tmux send-keys -l` 直送 user content + dual-track output(Claude Code 官方 hooks 写 progress.jsonl 业务事件 + transcript jsonl byte-offset incremental read → mirror 到 ccteam-owned `<bot>/turns.jsonl`)+ slash 命令(`/compact /new /clear`)透明透传。基于 ccgram + OMC 已在 production 验证的模式。
- **bot-to-bot 100% IM group** — IM history = 完整对话链,no in-process IPC,no FleetView SendMessage cross-tmux(那是 in-proc only)。hop_limit 在 group msg 链上数。
- **`ClaudeTuiAdapter` Wave 1 STUB** — 5 方法全返 `HarnessError::NotImplemented`,Wave 2 F108 填实。
- **`CodexExecAdapter` Wave 1 = 等价 V0.5.1 行为** — `start_thread` + `close_thread` 等价旧 `spawn_session`/`shutdown_session`;`submit_turn` / `resume_thread` 返 NotImplemented,Wave 3 F112 填。
- **root README.md 重写** — pitch 从"Claude session 召唤 AI 团队 接进 IM"(mode 3 偏置)→ "Claude Code 之上的 multi-agent 编排器 — 一个工具,三档能力"(3 形态平等)。3 形态 section 上移到 5-min install 之前,install snippet 含 3 invocation pattern(per mode)。

## Rejected

- ~~Option B(trait 留旧加新 10 方法)~~ — user 选 Option C(纯替换)。理由:Pre-v1.0 不留兼容 shim,旧 surface 在 Wave 2/3 接力使用前 deprecate 增加 mental load。
- ~~不抽 ccteam-cost crate,扩 pricing.rs~~ — user 选抽。理由:dual vendor + per-vendor budget 让 cost 已经是独立 domain。
- ~~mailbox 文件 + send-keys 短 trigger(input)~~ — 用户指出 ccgram + OMC 直接 send-keys content(literal mode -l 0 escape),mailbox 仅 attachment 用。
- ~~只 hooks single track output~~ — 用户指出 ccgram 也读 Anthropic internal transcript jsonl 走 dual-track,我们 mirror 到 ccteam-owned turns.jsonl 让 ccteam 控 schema + 缓冲 Anthropic 格式漂移。
- ~~ccteam 不主动调 /compact /new /clear + 不过滤用户调~~ → 现:**完全透明透传**(ccteam 不主动调也不过滤),通过 SessionStart hook 观察副作用 emit `chat_session_reset` event。
- ~~`ClaudeStreamJsonAdapter` / `ClaudeChatAdapter`~~ → `ClaudeTuiAdapter`(强调 transport,与 ClaudeBg / CodexExec / CodexAppServer 风格一致)。

## Risks(待 Wave 2/3/4 兜)

- **`SessionHandle::from_thread_handle` 翻译层** — Wave 1 留作 `#[allow(dead_code)]` helper。Wave 2 起 `translate_thread_events` 翻译 task 会真用;若 ThreadHandle.raw_extras 字段不全可能引入 panic。Wave 2 architect 接手时跑 host probe 验。
- **orchestrator 主循环仍用 F80 `claude_job::probe_job` poll** — Wave 1 没切到 `events()` stream;Wave 2 切换时需谨防 progress.jsonl `agent_spawn` / `agent_done` 顺序漂移。
- **Tier-1 docs unprefixed tool name drift** — `docs/interfaces.md` / `docs/claude-code-tool-surface.md` / `docs/v0-1/user-quickstart.md` / `skills/ccteam-control/SKILL.md` 仍引用 V0.5 `mcp__ccteam__admin_ls` 等。Wave 4 doc-sweep finding 必清。
- **OpenAI pricing 数据源** — cost-crater Plan 阶段 WebSearch 2026-05-19 openai.com + pricepertoken.com mirrors。3-6 个月需 verify 一次;Wave 4 加 `ccteam doctor --check-pricing-version` 警告(已有 schema_version 字段,doctor 检查脚手架就行)。
- **TG bot token 还没就位** — Wave 2 IM bridge host probe 走 mock-only;real host probe 待 user paste token 后单跑(`/ccteam-im-setup` 一次性 onboarding)。已在 `host-probe.md` 标 TODO。

## Files

- 仓 root README.md(3-mode rewrite)
- crates/ccteam-core/src/harness.rs(trait Option C + 类型)
- crates/ccteam-core/src/execution/{mod,claude_bg,claude_tui,codex_exec}.rs(新)
- crates/ccteam-core/src/{cost.rs,pricing.rs,pricing.json}(删,搬 ccteam-cost)
- crates/ccteam-cost/{Cargo.toml, src/{lib,pricing,level,budget}.rs, pricing/{anthropic,openai}.toml}(新 workspace member)
- crates/ccteam-cli/src/{mcp_serve,mcp_workflow_tools,mcp_chat_tools,mcp_advise_tools,mcp_tool_groups}.rs(MCP 子前缀 + 7 新 stub + 5 group enum)
- crates/ccteam-core/src/templates/{mod,project_mcp_json}.rs(`.mcp.json` 生成 + merge helper)
- skills/ccteam/SKILL.md(NL dispatcher 雏形)
- crates/ccteam-core/tests/harness_trait_test.rs(新,18 测试)
- crates/ccteam-cli/tests/{mcp_subprefix_test,mcp_disable_groups_test}.rs(新,12 测试)
- docs/v0-6-0/{README,prd,dev-plan}.md(amendment:三轴 / F108 / F109 §D / §九决策 flip / Wave 2 §A rename)

## Remaining(进 Wave 2/3/4)

- **Wave 2**(F108 + F109 + F114 + F115 + F116 + F117 + F118):
  - F108 `ClaudeTuiAdapter` STUB → 实 impl(tmux long-session + send-keys -l + dual-track)
  - F109 `ccteam-imd` daemon binary(`crates/ccteam-imd/` 新 crate;openhuman/channels deps;OMC Reply Listener 风格)
  - F114 `ccteam-creator` skill 复活 + 5-10 persona templates(中英 zh+en)
  - F115 `.ccteam/handoffs/<workflow>/<stage>.md` 决策摘要 hook
  - F116 supervisor + 三层安全 + heartbeat + signal files(OMC 借)
  - F117 `/ccteam-im-setup` skill + TG getMe + getUpdates auto-detect chat_id
  - F118 chat session 失效 last-N turn 重建(从 turns.jsonl)
- **Wave 3**(F112 Codex 完整):
  - `CodexExecAdapter::submit_turn / resume_thread` 填实(codex exec --json + codex resume <UUID>)
  - `CodexAppServerAdapter`(`crates/ccteam-core/src/execution/codex_app_server.rs`)走 UDS JSON-RPC v2
  - 4 用户场景 skill:`/ccteam-advise` + auto-critic in `ccteam-creator` + `/ccteam-team` Codex critic teammate + opt-in fallback preferences
  - per-vendor budget cap UI + cost 聚合显示
- **Wave 4**(集成 + ship):
  - 5 preset host E2E(Solo / Team Sprint / Overnight Builder / Pocket Assistant / IM Squad)+ 录 5 demo GIF
  - `CLAUDE.md` §一 version → 0.6.0 + baseline 数字回填 + §三红线表加 vendor 列
  - `docs/tech-design.md` + `docs/interfaces.md` + `docs/dev-coupling-audit.md` (F106-F118 各 1 条) 同步
  - Tier-1 docs MCP tool name 子前缀同步(`interfaces.md` + `claude-code-tool-surface.md` + `v0-1/user-quickstart.md` + `skills/ccteam-control/SKILL.md`)
  - workspace version bump 0.5.1 → 0.6.0 + git tag v0.6.0
