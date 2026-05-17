# V0.5.0 — dev-plan

3 wave 并行;F92 是 wave 0 prerequisite,wave 1+2 可并发 subagent 派工;wave 3 在 wave 1+2 stub 合并后启动。

---

## Wave 0(prerequisite, 必须先 ship)

### F92 — 真 cost 数据源

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/cost_summary.rs` | 重写主入口:从 state.json 切到 transcript jsonl;保留 state.json fallback |
| `crates/ccteam-core/src/pricing.rs`(新)| 内嵌 Anthropic pricing table(`include_str!("../pricing.json")`)+ token → dollar 计算 |
| `crates/ccteam-core/src/pricing.json`(新)| 当前 model 列表:sonnet-4 / sonnet-4-6 / opus-4 / opus-4-6 / opus-4-7 / haiku-4-5;字段:input / output / cache_creation / cache_read 每 1M token 美元 |
| `crates/ccteam-core/src/transcript_scanner.rs`(新)| 读 transcript jsonl,提取 `message.usage`;按 mtime + length memoize 避免重扫 |
| `crates/ccteam-core/tests/cost_summary_test.rs` | 新增 6 测试:linkScanPath 正常 / linkScanPath 缺失 fallback / pricing 4 model 覆盖 / memoize 命中 / WARN log 路径 / F84 budget cap 触发 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | `--check-pricing-version` 子选项:检查 pricing.json 内嵌 schema_version 距今 ≤180 天 |

测试矩阵:
- 真 host data:把 dex-ui 4h $1.10 session 的 transcript jsonl 拷成 fixture → assert cost ≈ $1.10
- 跨 model:fixture 含 sonnet + opus 混跑 → assert 总 cost 是两个 model 子 cost 之和
- linkScanPath 缺失:删 fixture 的 linkScanPath 字段 → assert 退 state.json,WARN 日志一次
- price drift:仿 Anthropic 半年后改价,本地不动 pricing.json → assert 计算用本地 pricing,`doctor --check-pricing-version` 报 stale

**估工**:1 个 subagent,1-2 天。

---

## Wave 1 — agent-team mode 核心(F93 + F94)

### F93 — workflow.yaml schema + `__lead` role

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs::WorkflowSpec` | 加 `pub mode: WorkflowMode`(enum `ArtifactDriven` / `AgentTeam`)+ `pub agent_team: Option<AgentTeamSpec>` |
| `crates/ccteam-core/src/workflow.rs::AgentTeamSpec`(新)| `lead_seed: String` / `teammate_mode: TeammateMode` / `default_model: Option<String>` / `require_plan_approval: bool` / `cleanup_on_stop: CleanupOnStop` |
| `crates/ccteam-core/src/templates/workflow.agent-team.yaml`(新)| 默认模板,`ccteam init --mode agent-team` 用;含 `suggested_teammates` 注释示例(definition + ad-hoc 各一个) |
| `agents/__lead.md`(新)| Anthropic agent spec 模板,沿用 F89 explorer.md pattern;include_str! 嵌入 binary;**body 必须包含 Anthropic 两类 teammate spawn pattern 指导**(definition: Task tool with subagent_type=role / ad-hoc: Task with subagent_type=general-purpose + 完整 inline prompt)+ Worker Preamble boilerplate |
| `crates/ccteam-core/src/workflow.rs::SuggestedTeammate`(新结构)| `{ role: String, kind: enum {Definition, AdHoc}, spawn_brief: String, adhoc_model: Option<String>, adhoc_color: Option<String>, adhoc_tools: Option<Vec<String>> }` |
| `crates/ccteam-cli/src/commands.rs::run_init` | 加 `--mode <artifact-driven\|agent-team>` flag;agent-team 走新 init 路径,写 `__lead.md` + agent-team workflow.yaml 模板 |
| `crates/ccteam-cli/src/commands.rs::DEFAULT_AGENT_SCAFFOLDS` | 加 `("__lead.md", include_str!("../../../agents/__lead.md"))` 条目(F89 数组) |
| `crates/ccteam-core/src/orchestrator.rs` | 加 `spawn_agent_team_lead(slug, spec)` 函数;agent-team mode 项目跳过 ArtifactWatcher 装,只装 lead session;lead spawn 时 env 注入 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` |
| `crates/ccteam-core/tests/agent_team_spawn_test.rs`(新)| 8 测试覆盖:schema parse / 缺 mode 默认 artifact-driven / lead spawn env / lead_seed 写到 inputs / __lead.md 用户改 body 警告 |

### F94 — Agent Teams 3 hook 镜像 + 5 新 event

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/templates/settings.json` | 加 `TeammateIdle` / `TaskCreated` / `TaskCompleted` 三 hook(条件渲染:agent-team mode 项目才装) |
| `crates/ccteam-core/src/templates/settings.agent-team.json`(新)| 或者:agent-team 独立 settings 模板,init 时按 mode 选择 |
| `crates/ccteam-cli/src/main.rs::HookCommand` | `ProgressAppend` 接受 5 新 event_type 字符串(`team_task_created` 等),复用现有 dispatch |
| `crates/ccteam-hooks/src/progress.rs` | 已支持任意 event_type 透传;agent_team 5 个 event 验证 payload schema(`team_task_created` 必须有 `task_id` 字段等) |
| `crates/ccteam-core/src/orchestrator.rs::Event` enum | 加 5 变体(`#[serde(rename = "team_*")]`)+ `From<serde_json::Value>` 转换 |
| `crates/ccteam-core/tests/event_team_test.rs`(新)| 5 event 序列化 / 反序列化 / payload 校验 |
| `docs/interfaces.md §6.4` | 7 类 event 表扩到 12 类;附 agent-team mode 专属说明 |

**估工**:F93+F94 1 wave 2-3 个 subagent 并行,3-5 天。

---

## Wave 2 — observation(F95)

### F95 — ArtifactWatcher 扩展(实测 schema 后简化)

实测发现 mailbox 是 `inboxes/<teammate>.json` **单文件** per 收件人,不是目录;message 已带 `from/text/timestamp/color/read` 字段。**不需要扫 teammate transcript jsonl**,**不需要 Haiku summarize** — 直接读文件 + 截前 200 char 即可。

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/artifact_watcher.rs` | 加 `add_agent_teams_discovery()` — daemon 启动 + 60s 周期扫 `~/.claude/teams/*/config.json` |
| `crates/ccteam-core/src/teams_config_parser.rs`(新)| 解析 `~/.claude/teams/<>/config.json`,提取 `members[]` + 每 member 全字段(agentId/name/color/agentType/model/cwd/tmuxPaneId/subscriptions/backendType/planModeRequired);**计算 `definition_backed: bool`** = `agentType` ∉ `{"general-purpose", "team-lead"}`;diff snapshot → emit `team_member_joined` / `team_member_left` |
| `crates/ccteam-core/src/teams_inbox_parser.rs`(新)| 解析 `~/.claude/teams/<>/inboxes/<teammate>.json` 数组;diff snapshot by timestamp → 新 message → emit `team_message_sent`(`text` 截 200 char);**识别 `text` 是 JSON-stringified `idle_notification` 类系统消息 → 分流到 F94 event(不进 mailbox stream)** |
| `crates/ccteam-core/src/teams_task_parser.rs`(新)| 解析 `~/.claude/tasks/<>/<id>.json`;监 dir 新文件 + modify;diff `status` 状态机(pending/in_progress/completed)→ emit `team_task_created` / `team_task_completed` |
| `crates/ccteam-core/tests/agent_teams_watcher_test.rs`(新)| 全用 host fixture(`references/claude-code/teams-samples/`)— config.json 加 member / inbox 加 message / task status 流转 / schema 解析失败 graceful |

**估工**:1 个 subagent,1-2 天(Haiku 不需要 + 文件 schema 已 probe → 比原计划简单)。

---

## Wave 3 — UI(F96)

### F96 — Web SPA Teams tab + 3 面板

| 文件 | 改动 |
|---|---|
| `crates/ccteam-web/src/api_v1.rs` | 加 5 endpoint:`GET /api/v1/teams`(列表)/ `/teams/<name>`(单 team)/ `/teams/<name>/tasks` / `/teams/<name>/inbox?teammate=&since=` / `/teams/<name>/member/<n>/definition`(definition-backed 返回 .md 解析,ad-hoc 404) |
| `crates/ccteam-web/src/subagent_resolver.rs`(新)| 按 Claude Code subagent scope 顺序(project `.claude/agents/` → user `~/.claude/agents/` → plugin → managed)解析 `<agentType>.md` 路径 + 解析 frontmatter + 标注 `skills` / `mcpServers` "not applied as teammate" |
| `crates/ccteam-web/src/sse.rs` | 加 `/api/v1/teams/<name>/events` SSE channel,推 6 类 team event(F95 5 + F94 1) |
| `crates/ccteam-web/web/spa/src/pages/TeamsListPage.tsx`(新)| `/teams` 顶级 tab,列出 host 所有 team 卡片(name / 描述 / member 数 / 最新活动 ts) |
| `crates/ccteam-web/web/spa/src/pages/TeamDetailPage.tsx`(新)| `/teams/<name>` 详情页,3 面板 layout |
| `crates/ccteam-web/web/spa/src/panels/TeamTopology.tsx`(新)| D3 force-directed graph;节点 = teammate(从 config.json::members[]);边 = subscriptions[] |
| `crates/ccteam-web/web/spa/src/panels/TaskBoard.tsx`(新)| Kanban 3 列(pending/in_progress/completed);卡片含 task title / owner / 依赖 |
| `crates/ccteam-web/web/spa/src/panels/MailboxStream.tsx`(新)| 时间线 + 未读高亮(`read: false`)+ 按 teammate 对过滤 + 搜索 |
| `crates/ccteam-web/web/spa/src/AppRoutes.tsx` | header tabs 加 Teams 项;路由 `/teams` + `/teams/:name` |
| `crates/ccteam-web/tests/api_v1_teams_test.rs`(新)| 4 endpoint × 3-4 测试(空 / 1 team / 多 team / schema 兼容 fallback) |
| `crates/ccteam-web/web/spa/test/panels/*.test.tsx` | 每面板 component test 5+ 个;**fixtures 用 references/claude-code/teams-samples/** |

**估工**:1 个前端 + 1 个 API subagent 并行,3 天(schema 已 probe → 比原计划清晰)。

---

## 测试矩阵全景

| 层 | 数量 | 覆盖 |
|---|---|---|
| Rust unit | +30 | F92 6 / F93 8 / F94 5 / F95 6 / F96 4 / F97-99(延期)|
| Rust integration | +12 | spawn lead + 装 hook + 镜像 event 端到端;F92 host data fixture;mode 切换 |
| Web API | +8 | /api/v1/projects/<slug>/team 4 + 5 SSE event 透传 4 |
| Web SPA component | +15 | 3 面板 × 5 个 test |
| **总新增** | **+65** | baseline 750 → ~815(MVP) |

测试运行:`cargo test --workspace --locked` + `npm test --prefix crates/ccteam-web/web/spa`。

---

## 风险盘点

| 风险 | 缓解 |
|---|---|
| Anthropic Agent Teams 协议漂移(`config.json` / `tasks/` schema)| F95 解析失败 WARN 不 panic;镜像 degrade 到 mtime-only;version-gate 加在 `doctor --check-agent-teams` |
| `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 改名废弃 | 锁 Claude Code 版本下限到 V0.5.0 ship 时的当前 stable;F99 加 doctor 检测 |
| F92 pricing.json 跟 Anthropic 公开价漂移 | pricing.json 内嵌 schema_version + ship date;`doctor --check-pricing-version` 半年触发 WARN |
| Haiku 总结 cost 累积(F95 SendMessage 总结)| 缓存 24h(同 from/to/body hash);用户可 `agent_team.summarize_messages: false` 关闭(走原文,长消息截 200 char) |
| `__lead` role 用户改坏 | `ccteam doctor --validate-team` 加 `__lead.md` body hash check;改了 WARN,**不 fail** — 留 escape hatch |
| lead 退出但 teammate 还活(机器 sleep + respawn 场景)| F97 `--restart-team` 处理;MVP 简化为 force-kill 全 team |
| 3 个新 web 面板大幅扩 SPA 体积 | 用 React lazy + code splitting,team-mode 项目才加载 3 个 chunk |
| F93 `__lead.md` 默认 prompt 写得太死 | 设计 review 阶段征求用户反馈;default `__lead.md` 偏"中性"(只声明协调职责,不暗示具体团队规模或风格) |

---

## ship 节奏

| 阶段 | 内容 | 截止 |
|---|---|---|
| **doc review** | 本 PRD + dev-plan 用户 review,确认 schema / 红线 / 测试覆盖 | T+0(立项后) |
| **Wave 0** | F92 单 subagent 实施 + ship merge | T+2 天 |
| **Wave 1+2 并行** | F93 / F94 / F95 三个 subagent 派工(stub API agreement 先定) | T+5 天 |
| **Wave 3** | F96 web SPA 在 wave1+2 stub merge 后启动 | T+7 天 |
| **integration** | 全 wave merge + host E2E + bug fix loop | T+9 天 |
| **doc 同步** | `interfaces.md` / `tech-design.md` / `orchestration-patterns.md §五` 更新 + `user-manual.md` 写 | T+10 天 |
| **ship + tag** | `cargo workspace version bump 0.4.6 → 0.5.0`,commit `vX.Y.Z:` 前缀,push tag | T+11 天 |
| **CLAUDE.md baseline 回填** | `cargo test --workspace` 新数 + V0.5.0 进当前最新版字段 | ship 后 |

---

## 与 V0.4.6 + V0.5.x 关系

- **V0.4.6 全部保留** — `mode` 字段缺失 = `artifact-driven`,V0.4.6 跑得动 V0.5.0 binary 不需要改 workflow.yaml
- **V0.5.1+ 候选**(本版本 V0.5.x 延期 F97-99 落地后):
  - F97 lifecycle 完善(3 cleanup 策略 + restart-team)
  - F98 plan-approval ↔ outbox 联动
  - F99 Claude Code 版本 gating doctor 检测
  - 仍可考虑 `orchestration-patterns.md §五` 剩余缺口:动态 Routing sugar / Evaluator-Optimizer 显式 sugar / workflow.yaml `extends`

---

## 红线复述

参 `prd.md` "V0.5.0 整体红线" 第 6 条。dev-plan 强调实施层守住:

1. **不**在 Rust 进程内模拟 lead 行为(不直接写 `~/.claude/teams/`)
2. **不**给 lead session 注入 system prompt(`lead_seed` 是 user-turn message)
3. **不**破 V0.4.6 现有 7 event;新 5 event 严格 `team_*` 命名前缀
4. **不**让 F92 影响调用方签名;数据源切换内部完成
5. **不**让 web SPA agent-team 面板影响 artifact-driven workflow 渲染路径
