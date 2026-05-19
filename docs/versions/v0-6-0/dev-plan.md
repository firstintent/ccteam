# V0.6.0 — dev-plan

**4 wave**(按 Epic 组织,F-finding 是实施手段):

| Wave | 内容 | F | 占比 | 工期 | 并行度 |
|---|---|---|---|---|---|
| Wave 0 | **doc-first**:land 本 PRD + README + dev-plan + 5 用户面文档 + advanced 文档 | F106 + 文档套件 | 5% | 1 天(本 PR) | 主 session + 5 teammate 并行 |
| Wave 1 | **架构基石**:F107 HarnessAdapter trait 扩展(对齐 Codex ThreadManager)+ F113 `/ccteam` dispatcher skill 雏形 + F111 MCP 子前缀 | F107 + F113 + F111 | 20% | 3-4 天 | 2 subagent 并行 |
| Wave 2 | **Epic A + B 主菜**:F108 ClaudeStreamJsonAdapter + F109 ccteam-imd(统一 openhuman)+ F114 ccteam-creator 复活 + F117 ccteam-im-setup + F115 handoff + F116 supervisor + F118 session recovery | F108 + F109 + F114 + F115 + F116 + F117 + F118 | 50% | 8-10 天 | 4 subagent 并行 |
| Wave 3 | **Codex 完整集成**:F112 CodexExecAdapter + CodexAppServerAdapter + 4 用户场景 + 双 pricing | F112 | 18% | 4-5 天 | 2 subagent 并行 |
| Wave 4 | **集成 + host E2E + ship**:全链路 5 preset host probe + 文档 polish + version bump | F106 集成部分 | 7% | 2 天 | 主 session |

总:T+18-22 天(对比上版 PRD 11-15 天)。Baseline 942/1 → 预计 ~1010/1(新增 ~68 测试)。

---

## Wave 0 — 立项 PR(本 PR)

**只 land 文档** — 12 个文件:

| 文件 | 行数估 | 写者 |
|---|---|---|
| `docs/versions/v0-6-0/README.md` | ~250 | 主 session(已写) |
| `docs/versions/v0-6-0/prd.md` | ~800 | 主 session(已写) |
| `docs/versions/v0-6-0/dev-plan.md`(本文件)| ~280 | 主 session |
| `README.md`(repo 根 重写)| ≤80 | PM teammate |
| `docs/quickstart.md`(新) | ≤120 | PM teammate |
| `docs/user-manual.md`(重写 V0.5)| ≤300 | PM teammate |
| `docs/recipes.md`(新) | ≤500 | researcher teammate |
| `docs/troubleshooting.md`(新)| ≤400 | cc-expert teammate |
| `docs/advanced/customize-workflow.md`(新) | ~200 | architect teammate |
| `docs/advanced/multi-llm-codex.md`(新)| ~250 | codex-expert teammate |
| `docs/advanced/presets-reference.md`(新)| ~200 | researcher teammate |

**5 teammate 并行派单**(主 session 写完核心 3 + 调度):
1. PM → `README.md` + `quickstart.md` + `user-manual.md`(产品 voice 一致)
2. researcher → `recipes.md` + `advanced/presets-reference.md`(横向对比经验)
3. cc-expert → `troubleshooting.md`(故障 + 平台依赖知识)
4. architect → `advanced/customize-workflow.md`(高级 yaml 编辑指南)
5. codex-expert → `advanced/multi-llm-codex.md`(Codex 集成手册)

**估工**:1 天(主 session 已完 3 核心 + 调度;teammate 各 0.5-1 工时并行)

---

## Wave 1 — 架构基石(F107 + F113 + F111)

**目标**:trait 抽离 + skill dispatcher 框架 + MCP 子前缀。零行为变化(baseline 942/1 持平)。

### Subagent A:F107 HarnessAdapter trait 扩展

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/harness.rs` | trait 加 `start_thread / submit_turn / events / resume_thread / close_thread` 5 方法 + `TurnInput` / `ThreadEvent` / `SessionHandle` / `AgentVendor` / `ExecutionMode` 类型 |
| `crates/ccteam-core/src/execution/claude_bg.rs`(新,迁移自 spawn.rs)| `ClaudeBgAdapter` impl trait — 行为完全等价旧 `spawn_session` |
| `crates/ccteam-core/src/execution/codex_exec.rs`(新)| `CodexExecAdapter` skeleton impl(stub 测 trait 设计,Wave 3 详填)|
| `crates/ccteam-core/src/orchestrator.rs` | 主循环改 ThreadHandle / ThreadEvent 分发 |
| `crates/ccteam-cost/src/lib.rs`(新)| `UnifiedTokenUsage` + 双 pricing table |
| `crates/ccteam-cost/pricing/{anthropic,openai}.toml`(新)| 双 pricing |
| `crates/ccteam-core/tests/harness_trait_test.rs`(扩)| 4-6 trait 行为测试 |

**估工**:2 subagent 协作 3-4 天。

### Subagent B:F113 + F111

| 文件 | 改动 |
|---|---|
| `skills/ccteam/SKILL.md`(新)| ~150 行 dispatcher skill body |
| `crates/ccteam-cli/src/mcp_serve.rs` | tool name 加子前缀;`CCTEAM_DISABLE_TOOLS` 改 group enum |
| `crates/ccteam-cli/src/mcp_chat_tools.rs`(新)| chat_* 5 工具 stub(Wave 2 接 F108)|
| `crates/ccteam-cli/src/mcp_advise_tools.rs`(新)| advise_* 2 工具 stub(Wave 3 接 F112)|
| `crates/ccteam-cli/tests/mcp_subprefix_test.rs`(新)| 4 测试 |

**估工**:1 subagent 2 天。

### Stub agreement(开 wave 前定)

trait 签名锁定(PRD §F107 接口)— wave 2/3 不许改 signature(改 = wave 1 没收敛回炉)。

### 验收

- `cargo test --workspace`: ≥942 通过(baseline 持平)
- clippy: 0 errors, ≤18 warnings
- `grep -rn "Command::new(\"claude\")" crates/ccteam-core` 命中**仅在** `execution/claude_bg.rs`
- `grep -rn "trait HarnessAdapter" crates/ccteam-core/src/harness.rs` ≥1(扩展不新建)
- `grep -rn "mcp__ccteam__" crates/ccteam-cli/src/` 命中 ≥17(server name 不变)

---

## Wave 2 — Epic A + B 主菜(F108 + F109 + F114 + F115 + F116 + F117 + F118)

**目标**:Epic A 5min IM bot 跑通(host probe TG hello world);Epic B chat 完整 IM Squad + handoff + session recovery。

### 4 subagent 并行拆

#### Subagent A:F108 ClaudeTuiAdapter + F118 session recovery

> **Amended 2026-05-19** by Wave 1 architect:F108 改 tmux 长跑 + send-keys -l +
> dual-track(prd.md F108)。文件 rename:`claude_stream_json.rs` → `claude_tui.rs`
> (Wave 1 已 STUB land);`mailbox.rs` → `attachments.rs`(只处理非文本附件,文本
> 走 send-keys -l 直送);加 `transcript_tail.rs`(byte-offset 增量读 Anthropic 内部
> session jsonl)+ `turns_mirror.rs`(写 ccteam-owned `turns.jsonl` SoT)。

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/claude_tui.rs`(Wave 1 STUB → Wave 2 填)| `ClaudeTuiAdapter` impl,~500 行 |
| `crates/ccteam-core/src/execution/transcript_tail.rs`(新)| byte-offset 增量读 `~/.claude/projects/<encoded-cwd>/<sid>.jsonl` + cursor 文件 `transcript-cursor.json`,~200 行 |
| `crates/ccteam-core/src/execution/turns_mirror.rs`(新)| 镜像 parse 后的 transcript 行到 ccteam-owned `<.ccteam/chat/<bot>/turns.jsonl>` SoT,~150 行 |
| `crates/ccteam-core/src/execution/attachments.rs`(新)| 非文本附件(图片 / 大文件)mailbox(text 走 send-keys -l 直送,无 mailbox),~100 行 |
| `crates/ccteam-core/src/execution/session_recovery.rs`(新,F118)| last-N turn 重建逻辑(读 ccteam-owned `turns.jsonl`) |
| `crates/ccteam-core/src/workflow_schema.rs` | `mode: chat` schema 字段 `#[serde(default)]` 收紧 |
| `crates/ccteam-core/src/progress_event.rs` | 加 4 chat_* event 类型 |
| `crates/ccteam-hooks/src/chat_progress.rs`(新)| Claude Code 官方 hooks(UserPromptSubmit / Stop / SubagentStop / SessionStart / PostToolUse)→ progress.jsonl chat_* 业务事件桥接 |
| `crates/ccteam-core/tests/claude_tui_test.rs`(新)| 8 测试 |

**估工**:3-4 天。

#### Subagent B:F109 ccteam-imd + F116 supervisor

| 文件 | 改动 |
|---|---|
| `crates/ccteam-imd/Cargo.toml`(新)| deps openhuman + feature gate;ccteam-core trait dep |
| `crates/ccteam-imd/src/main.rs`(新)| daemon 入口 |
| `crates/ccteam-imd/src/{supervisor,inbound,outbound,router,hop_tracker,nl_admin,credentials,sanitize,rate_limit,acl}.rs`(新)| 6 模块 ~1500 行 |
| `crates/ccteam-imd/systemd/ccteam-imd.service`(新)| Linux systemd unit |
| `Cargo.toml`(workspace)| 加 member + openhuman dep |
| `crates/ccteam-imd/tests/router_test.rs`(新)| 6 测试 |
| `crates/ccteam-imd/tests/dep_graph_test.rs`(新)| 守 ccteam-core 0 openhuman dep |
| `crates/ccteam-cli/src/commands.rs::daemon_start` | chat workflow 检测 → spawn ccteam-imd |

**估工**:3-4 天。

#### Subagent C:F114 ccteam-creator + F117 ccteam-im-setup

| 文件 | 改动 |
|---|---|
| `skills/ccteam-creator/SKILL.md`(复活 + 重写)| ~400 行 |
| `skills/ccteam-creator/personas/<10 prefab>`(新)| 5-10 个 prefab persona(每个 zh + en 版)|
| `skills/ccteam-im-setup/SKILL.md`(新)| ~200 行 onboarding dialog |
| `crates/ccteam-core/src/templates/workflow_templates/`(新)| 5 个 workflow.yaml 模板(对应 5 preset) |
| `crates/ccteam-core/src/mode_inferrer.rs`(新)| NL → mode 推断(rule-based + LLM 兜底)|
| `crates/ccteam-core/src/agent_naming.rs`(新)| 50 scientist nickname 池 |
| `crates/ccteam-imd/src/onboarding.rs`(新)| getMe + getUpdates auto-detect chat_id |
| `skills/ccteam-creator/tests/`(新)| persona / mode_inferrer 测试 |

**估工**:3 天。

#### Subagent D:F115 handoff

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/handoff.rs`(新)| handoff hook trigger + markdown 模板 |
| `crates/ccteam-core/src/orchestrator.rs` | `stage_done` event 触发 handoff prompt 注入 |
| `crates/ccteam-core/src/spawn_brief.rs` | spawn prompt 模板加 `{{include_prev_handoffs}}` |
| `crates/ccteam-core/tests/handoff_test.rs`(新)| 4 测试 |

**估工**:1.5 天。

### Stub agreement

- A 给 B/C:`HarnessAdapter::submit_turn(TurnInput::UserText)` signature 确认(Wave 1 已 lock)
- B 给 C:`ccteam-imd::register_bot(workflow_slug, vendor, role)` 接口
- C 给 B:`workflow_templates/chat.yaml` template format(bot_roster 字段)
- D 不依赖其他

### Wave 2 集成节点(并行结束后)

1. workflow.yaml fixture(`crates/ccteam-core/tests/fixtures/chat-pocket-assistant.yaml` + `chat-im-squad.yaml`)
2. 集成测试:mock IM 输入 → ccteam-imd → ClaudeStreamJsonAdapter(`CCTEAM_CLAUDE_BIN` env 覆盖)→ assert progress.jsonl 含 `chat_turn` event
3. **host TG probe**(开发者 own TG bot token + chat_id 走 `/ccteam-im-setup`):
   - Pocket Assistant DM hello world(目标:5 min 内 wow)
   - IM Squad group + bot-to-bot @ + hop_limit 触发
   - session recovery:删 Anthropic session jsonl → bot 在 IM 主动通知重建
   - `@ccteam pause helpful-bot` NL admin
4. **video / GIF**:30s 录屏 user 5min hello world,放 `docs/versions/v0-6-0/demos/30s-tg-bot-team.gif`

### 验收

- `cargo test --workspace`: ≥980 通过
- TG host probe pass(视频 + log 留存 `docs/versions/v0-6-0/host-probe.md`)
- `cargo tree -p ccteam-core | grep -E "openhuman|teloxide|tgbot"` 命中 0
- ccteam-imd crash 重启 ≤2s,无丢 turn
- `claude -p --resume` cache hit:turn 2 起 `cache_read_input_tokens > 0`
- backup transport 切换(`/ccteam-im-setup --transport official-telegram`)生效
- NL admin `@ccteam pause` 在 TG 群中工作

---

## Wave 3 — Codex 完整集成(F112)

**目标**:Codex Option B 完整落地。4 用户场景全跑通。30 cell 矩阵中 Codex 列 ✓ 11 + ⚠ 2 / ✗ 4(mode 1 跨 vendor)host probe 验。

### 2 subagent 并行拆

#### Subagent A:F112 CodexExecAdapter + CodexAppServerAdapter

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/codex_exec.rs` | 从 Wave 1 stub 填完整 impl,~400 行 |
| `crates/ccteam-core/src/execution/codex_app_server.rs`(新)| `CodexAppServerAdapter` impl(走 UDS JSON-RPC v2)~600 行 |
| `crates/ccteam-core/src/execution/codex_jsonrpc.rs`(新)| `rmcp` client 复用 or 自造 thin client |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | `--check-codex-version` + `--check-codex-auth` |
| `crates/ccteam-cost/src/budget.rs` | per-vendor budget caps |
| `crates/ccteam-core/tests/codex_exec_test.rs`(新)| 6 测试 |
| `crates/ccteam-core/tests/codex_app_server_test.rs`(新)| 5 测试 |

**估工**:3-4 天。

#### Subagent B:F112 4 用户场景 skill

| 文件 | 改动 |
|---|---|
| `skills/ccteam-advise/SKILL.md`(新)| ~100 行 parallel voting skill |
| `skills/ccteam-creator/SKILL.md` | Phase 2 加 auto-critic 路径(检测 codex binary + auth → critic role 自动 `vendor: codex`)|
| `skills/ccteam-team/SKILL.md` | 加 "Codex critic teammate(if available)" 路径 |
| `crates/ccteam-core/src/preferences.rs`(新)| `~/.ccteam/preferences.toml` 读写(fallback.on_claude_quota = "codex|off")|
| `crates/ccteam-cli/src/commands.rs::prefs_*` | `ccteam prefs <key> <value>` admin CLI(opt-in)|
| `skills/tests/`(新)| skill dialogue test |

**估工**:2 天。

### Wave 3 集成节点

1. host probe(开发者装 codex + auth):
   - `/ccteam-advise "what's the best approach to X"` → Claude + Codex 并行 → 合成 verdict
   - `ccteam-creator` task 含 "review" → critic role 自动 `vendor: codex`(progress.jsonl 记 `vendor: codex`)
   - Codex `agent_max_depth` 递归保护:链 5 层后 escalate
2. cost 双 vendor 聚合验证:`ccteam-control show-cost` 显示聚合 + 分 vendor
3. 模式 3 codex(`CodexAppServerAdapter`)host probe:`codex app-server` UDS thread/start + turn/start + 流式 items
4. backup transport + Codex 混合场景验证

### 验收

- `cargo test --workspace`: ≥1010 通过
- Codex host probe pass
- 30 cell 矩阵 host probe(关键 ⚠/✗ cell 至少 mock test;✓ cell 至少 1 host)

---

## Wave 4 — 集成 + host E2E + ship(F106 集成 + 文档 polish)

### 任务

1. **F106 文档落地**(主 session):
   - `CLAUDE.md` §一 version → 0.6.0;baseline 数字回填;§三红线表加 vendor 列(模式 × vendor 双轴)
   - `docs/tech-design.md` §0 + §2.1 + §3.3 改动 land
   - `docs/architecture/orchestration-patterns.md` §一加 30 cell 矩阵
   - `docs/interfaces.md` MCP 子前缀 + workflow.yaml schema + handoff format
   - `docs/dev-coupling-audit.md` F106-F118 各 1 条

2. **5 preset 全链路 host E2E**:
   - Solo Sidekick(V0.5 既有,verify 不退步)
   - Team Sprint(V0.5 既有,verify 不退步 + 加 Codex critic 测试)
   - Overnight Builder(mode 2 既有 + 加 Codex critic role)
   - Pocket Assistant(Wave 2 落,完整 5min hello world)
   - IM Squad(Wave 2 落,完整 group + bot-to-bot @ + hop_limit)

3. **5 demo GIF**(每 preset 30s 录屏)放 `docs/versions/v0-6-0/demos/`;README + quickstart 头条引用

4. **commit + tag**:`v0.6.0:` 前缀,`workspace.package.version = "0.6.0"`,git tag `v0.6.0`

### 验收

- `cargo test --workspace --locked --no-fail-fast`: ≥1010 / 1
- clippy: 0 errors, ≤18 warnings(允许减少)
- 5 host E2E 全过(每个 5 preset 一个 GIF + 一个 host-probe.md 段落)
- 文档 self-consistency:
  - `grep -rn "每次 spawn.*fresh"` 全仓命中处都标 [模式 2 / Claude vendor];EOL dir 内不动
  - `grep -rn "mcp__ccteam__chat_" crates/ skills/ docs/` 命中 ≥5(子前缀工具命名生效)
  - `grep -rn "send-keys" crates/ccteam-core/src/execution/` 命中 0(tmux send-keys 路径已弃)
  - `grep -rn "mode 1\|mode 2\|mode 3" docs/{quickstart,user-manual,recipes,troubleshooting,README}.md` 命中 0(用户面 0 内部术语)

---

## Cross-wave invariants

整个 V0.6.0 周期守:

1. **现有模式 2 行为不退步** — 任何 wave PR 都跑现 V0.5.1 host E2E,fail 即 block
2. **trait 签名 wave 1 后冻结** — wave 2/3 改 signature 必须打回 wave 1 重做
3. **`ccteam-core` 不依赖 openhuman / IM SDK** — `cargo tree -p ccteam-core | grep -E "openhuman|teloxide|slack|discord|tgbot"` 命中 0
4. **测试 fixture 不依赖真 binary**:`CCTEAM_CLAUDE_BIN` / `CCTEAM_CODEX_BIN` env 覆盖;TG / Slack 用 mock server
5. **每 wave PR 描述**带:`requirements.md` 痛点 link + `prd.md` F-finding link + `dev-coupling-audit.md` 条目;**测试 baseline 数字** before/after
6. **用户面文档**(README / quickstart / user-manual / recipes / troubleshooting)13 项内部术语 0 命中(grep 守红线)
7. **vendor 双轴**:任何新 finding / 新代码加入,都答"Claude / Codex 各自如何?";只覆盖 Claude 不算完成

---

## 范围回滚 / 紧急 ship plan

如 Wave 2 host probe 发现重大平台不可控(如 Anthropic 改 `claude -p --resume` 协议),fallback 顺序:

1. **保 Epic A 完整 ship**(`ccteam-creator` skill + Pocket Assistant + 单 vendor Claude):优先级 1
2. **Epic B 砍 IM Squad,只保 DM**:Epic B 完整体验降级,V0.6.1 补 group + bot-to-bot
3. **Epic C 砍** persona 中文版,等 V0.7:Epic C 影响国际化但不阻塞 ship
4. **F112 Codex 砍到 Option A 最小**(mode 2 codex only):若 Codex app-server UDS 协议变,Wave 3 紧急降级
5. **F108 紧急回退 tmux send-keys**:若 Agent SDK / `claude -p --resume` 不可用,F108 走原 PRD tmux 路径(技术债,但保 ship)— 标记 V0.6.1 必偿还

任何 fallback 触发 → PR 描述明示 + `docs/versions/v0-6-0/host-probe.md` 详记 + `dev-coupling-audit.md` 加 F-finding 入下版必清单。
