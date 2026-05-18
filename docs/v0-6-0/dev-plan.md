# V0.6.0 — dev-plan

**Wave 顺序** — trait 抽离打地基(零行为变化)→ 模式 3 落地(本版主菜)→ MCP 优化(顺带,独立)。每 wave 单独 PR + 单独 baseline 校验。

| Wave | 内容 | F | 占比 | 并行度 |
|---|---|---|---|---|
| Wave 1 | `ExecutionAdapter` trait 抽离 + `BgSpawnAdapter` 改造 — 零行为变化 | F107 | 20% | 单 subagent,2-3 天 |
| Wave 2 | `TmuxInteractiveAdapter` + `ccteam-im-bridge` crate — 模式 3 落地 | F108 + F109 | 55% | 2 subagent 并行,5-7 天 |
| Wave 3 | MCP namespace + 子前缀 + `CCTEAM_DISABLE_TOOLS` + 项目级 `.mcp.json` | F110 + F111 | 15% | 单 subagent,2-3 天 |
| Wave 4 | 文档同步(F106 落 tech-design / orchestration-patterns / CLAUDE.md)+ host E2E + ship | F106 + 集成 | 10% | 主 session,2 天 |

总:T+11-15 天。baseline 942/1 → 预计 ~975/1(新增 ~33 测试:trait 4 + tmux 8 + im-bridge 6 + mcp disable 5 + project mcp.json 4 + 集成 ~6)。

---

## Wave 0 — 立项 PR(本 PR)

**只 land 文档**:
- `docs/v0-6-0/README.md`
- `docs/v0-6-0/prd.md`(本文件兄弟)
- `docs/v0-6-0/dev-plan.md`(本文件)

**不动**任何代码 / 测试 / `CLAUDE.md`(version bump 走 wave 4)。

**估工**:已完成(本 session)。

---

## Wave 1 — `ExecutionAdapter` trait 抽离(F107)

**目标**:抽 trait + 改造现有 BgSpawner,行为零变化,`cargo test --workspace` 数字持平 942/1。

### 关键文件

| 文件 | 改动类型 | 说明 |
|---|---|---|
| `crates/ccteam-core/src/execution/mod.rs` | 新 | trait + 类型定义 |
| `crates/ccteam-core/src/execution/bg.rs` | 新(迁移)| `BgSpawnAdapter` impl;迁移自现 `spawn.rs` / `claude_job.rs` |
| `crates/ccteam-core/src/orchestrator.rs` | 改 | 持 `Arc<dyn ExecutionAdapter>`;主循环走 trait |
| `crates/ccteam-core/src/lib.rs` | 改 | re-export `execution::*` |
| `crates/ccteam-cli/src/commands.rs` | 改 | spawn / send / pause / resume 全改走 trait |
| `crates/ccteam-cli/src/mcp_serve.rs` | 改 | tool handler 走 trait(为 wave 3 chat tool 铺路) |
| `crates/ccteam-core/tests/execution_trait_test.rs` | 新 | 4 trait 行为测试 |

### Stub agreement(开 wave 前定)

trait 签名锁定(PRD §F107 接口)。**任何后续 wave 不许改 trait 签名**(改 = wave 1 没收敛,回炉)。

### 实施步骤

1. 写 trait + 类型 + 单测,先 `cargo build` 通过 — 0.5 天
2. 抽 `BgSpawnAdapter`,迁移 `spawn_session` 主路径 — 1 天
3. orchestrator 主循环替换 — 0.5 天
4. CLI / MCP 路径替换 — 0.5 天
5. 跑 `cargo test --workspace`,fix 退步 — 0.5 天

### 验收

- `cargo test --workspace --locked --no-fail-fast`: ≥942 通过,1 失败(同 baseline,不变)
- clippy: 0 errors, ≤18 warnings
- `grep -rn "Command::new(\"claude\")" crates/ccteam-core` 命中**仅在** `execution/bg.rs`
- host probe:V0.5.1 跑过的 dex-ui workflow.yaml 行为完全一致(progress.jsonl event 序列 diff = 0)

### 风险 / 应对

| 风险 | 应对 |
|---|---|
| trait 设计不够泛化,wave 2 发现要改签名 | wave 1 PR 包含 1 个"mock chat adapter" 测试(只为验证签名能容纳,不实现真功能) |
| BgSpawner 迁移漏掉 edge case(restart / cancel race)| 复跑现有 `crates/ccteam-core/tests/bg_spawner_test.rs` 等全套现测试,严格 0 退步 |
| orchestrator 主循环动到了红线 | code review 严守:`grep -n "fix_count\|escalate\|budget"` 在 orchestrator.rs 改动行数 ≤ 5 |

---

## Wave 2 — 模式 3 落地(F108 + F109)

**目标**:`TmuxInteractiveAdapter` impl + `ccteam-im-bridge` crate + workflow.yaml `mode: chat`;host TG 实测多轮 chat 跑通。

### 并行拆分(2 subagent)

#### Subagent A:F108 `TmuxInteractiveAdapter` + 配套 schema

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/tmux_interactive.rs` | 新,~600 行 |
| `crates/ccteam-core/src/execution/session_jsonl_tail.rs` | 新,~300 行 |
| `crates/ccteam-core/src/execution/send_keys.rs` | 新,~200 行 + fuzzing test |
| `crates/ccteam-core/src/workflow_schema.rs` | 加 `mode: chat` 枚举 + 字段 |
| `crates/ccteam-core/src/progress_event.rs` | 加 4 chat_* event 类型 |
| `crates/ccteam-core/tests/tmux_interactive_test.rs` | 新,8 测试 |

**估工**:3-4 天。

#### Subagent B:F109 `ccteam-im-bridge` crate

| 文件 | 改动 |
|---|---|
| `crates/ccteam-im-bridge/Cargo.toml` | 新 crate |
| `crates/ccteam-im-bridge/src/lib.rs` | 新,trait + types |
| `crates/ccteam-im-bridge/src/telegram.rs` | 新,teloxide impl |
| `crates/ccteam-im-bridge/src/router.rs` | 新,@-mention parse + 路由 |
| `crates/ccteam-im-bridge/src/hop_tracker.rs` | 新,R6 hop_limit |
| `crates/ccteam-im-bridge/src/session_link.rs` | 新,TG user ↔ bot session 持久化 |
| `crates/ccteam-cli/src/commands.rs::start` | chat mode 检测 → spawn bridge |
| `crates/ccteam-cli/src/main.rs` | 加 `internal im-bridge` 子命令 |
| `Cargo.toml`(workspace 根)| 加 member |
| `crates/ccteam-im-bridge/tests/router_test.rs` | 新,6 测试 |
| `crates/ccteam-im-bridge/tests/dep_graph_test.rs` | 新,守 core 不依赖 teloxide |

**估工**:3-4 天。

### Stub agreement(开 wave 前定)

A 给 B 提供的 stub:
- `ExecutionAdapter::send_input(handle, Input::User { ... })` signature 确认(F107 wave 1 已固化)
- `ObservedEvent::ChatTurn` 字段:`{ speaker: Speaker, content: String, ts: SystemTime, raw_jsonl_offset: u64 }`
- session-id 持久化路径:`~/.ccteam/im/<workflow_slug>/<bot_name>/session-id`

A 不依赖 B(可在 mock IM 输入下独立测试)。
B 不依赖 A 真实(用 mock ExecutionAdapter 单测 router / hop_tracker)。

### Wave 2 集成节点(并行结束后)

1. workflow.yaml fixture(`crates/ccteam-core/tests/fixtures/chat-2-bot.yaml`)
2. 集成测试:mock IM 输入 → TmuxInteractive(用 echo 替代 `claude` 二进制,`CCTEAM_CLAUDE_BIN` env 覆盖)→ assert progress.jsonl 含 `chat_turn` event
3. host TG probe(开发者 own TG bot token):双 bot 群组 @ 链式互动,实测 prompt cache hit + hop_limit 触发

### 验收

- `cargo test --workspace`: ≥970 通过
- TG host probe pass(有视频 / log 留存放进 `docs/v0-6-0/host-probe.md`,主 session 写)
- `cargo tree -p ccteam-core | grep teloxide` 命中 0
- `claude --resume` cache hit:turn 2 起 transcript jsonl 的 `cache_read_input_tokens > 0`

### 风险 / 应对

| 风险 | 应对 |
|---|---|
| `claude --resume` 在新 Claude Code 版本失效 | feature-gate 检测:`ccteam doctor --check-claude` 测试 resume 能力,失败则报错让用户升 |
| send-keys 在 unicode / emoji 输入 corrupt pane | fuzzing test 200 sample;`tmux send-keys -l`(literal mode)覆盖 risky 字符 |
| session jsonl 格式 Anthropic 改 | session_jsonl_tail 用 `#[serde(other)]` + 容错 parse;格式变化 graceful 退化为 turn-count-only(失对话细节,不失业务事件) |
| teloxide breaking upgrade | pin 0.13.x;每月 dependabot scan |
| TG bot token 被仓库泄露 | 红线:`workflow.yaml` 只写 env var **名字**,token 必须从环境拿;CI 加 trufflehog 扫 |

---

## Wave 3 — MCP 优化(F110 + F111)

**目标**:server rename + 子前缀 + disable env + 项目级 `.mcp.json`。

### 文件清单

详 PRD §F110 + §F111。

### 实施步骤(单 subagent)

1. mcp_serve.rs server name + tool registry 重命名 — 0.5 天
2. 新 `mcp_chat_tools.rs`(5 工具)注册 — 0.5 天
3. CCTEAM_DISABLE_TOOLS env + glob 过滤 — 0.5 天
4. `ccteam init` 落项目 `.mcp.json` + merge 逻辑 — 0.5 天
5. skill `SKILL.md` + meta-agent template 全文 replace — 0.5 天
6. 测试新增 + host probe — 0.5 天

**估工**:2-3 天。

### Stub agreement

- F110 tool name 表(PRD §F110 完整表)在 wave 3 启动前再次确认(给 user 最后一次反悔机会)
- F111 disable glob 语义:short name 不含 prefix,`workflow_*` / `chat_*` / exact name 都支持

### 验收

- `~/.claude.json` 含 `"ct"` server,**无** `"ccteam"` server
- `/mcp` 列表 22 工具按 workflow_/chat_ 分组(Claude Code 不强制分组渲染,但工具名前缀让 user 一眼看到归属)
- `CCTEAM_DISABLE_TOOLS=chat_*` 后 tool list 17 工具(模式 2-only 用户场景)
- `ccteam init` 新项目自动生成 `.mcp.json`,既有项目 merge 不破坏
- meta-agent / `ccteam-control` skill 跑过 V0.5.1 全部 host probe

### 风险 / 应对

| 风险 | 应对 |
|---|---|
| 用户 V0.5 session 中调用旧名字 → method-not-found | Claude 看到错误自然重试新名字;skill SKILL.md 升级文档显著标"V0.6.0 breaking" |
| `.mcp.json` 跟用户已有 OMC / serena 等 server 冲突 | merge 逻辑保留其他 server,只加 `ct`;ct 已存在则跳过 |
| 项目级 `.mcp.json` 进 git,泄露 / 团队成员强制配 | doctor 输出明确提示"`.mcp.json` 推荐 commit,但 `CCTEAM_DISABLE_TOOLS` 个人偏好别写进去 — 用 shell rc" |

---

## Wave 4 — 文档同步 + host E2E + ship(F106 + 集成)

**目标**:三模式 + 红线表正式 land tier-1 文档;主 session 跑完整 host E2E;`workspace.package.version` bump → V0.6.0;ship。

### 任务

1. **F106 文档落地**(主 session):
   - `CLAUDE.md` §一表格 version → 0.6.0,baseline 数字回填(wave 1/2/3 ship 后实测)
   - `CLAUDE.md` §三红线表加"模式"列
   - `docs/tech-design.md` §2.1(或 §0)三模式定义表 + 红线 × 模式矩阵 + `ExecutionAdapter` trait section
   - `docs/orchestration-patterns.md` §一 5×3 适用矩阵
   - `docs/interfaces.md` MCP 工具表 + workflow.yaml `mode: chat` schema
   - `docs/dev-coupling-audit.md` 加 F106-F111

2. **`docs/v0-6-0/user-manual.md`**:chat mode 教程(从 `ccteam init --mode chat` 到 TG @ bot 跑通,~150 行)

3. **host E2E**:
   - 现 V0.5.1 host E2E 全部复跑(模式 2 不退步)
   - 新 mode-3 E2E:启 chat workflow + TG 真实 bot + 多 bot 链式 @ + hop_limit + `/compact` 触发

4. **commit + tag**:`vX.Y.Z:` 前缀,`workspace.package.version = "0.6.0"`,git tag `v0.6.0`

### 验收

- `cargo test --workspace --locked --no-fail-fast`: ≥975 / 1
- clippy: 0 errors, ≤18 warnings
- host E2E 全过
- 文档 self-consistency:`grep -rn "每次 spawn.*fresh"` 全仓命中处都有 `[模式 2]` 标记 / EOL dir / 删除

---

## Cross-wave invariants

整个 V0.6.0 周期守:

1. **现有模式 2 行为不退步** — 任何 wave PR 都跑现 V0.5.1 host E2E,fail 即 block
2. **trait 签名 wave 1 后冻结** — wave 2/3 改 signature 必须打回 wave 1 重做
3. **`ccteam-core` 不依赖 IM SDK** — `cargo tree -p ccteam-core | grep -i "telox\|slack\|discord"` 命中 0
4. **测试 fixture 不依赖真 Claude binary / 真 TG** — `CCTEAM_CLAUDE_BIN` / `CCTEAM_TG_BOT_API_URL` env 覆盖 + mock server
5. **每 wave PR 描述**带:`requirements.md` 痛点 link + `prd.md` F-finding link + `dev-coupling-audit.md` 条目;**测试 baseline 数字** before/after
