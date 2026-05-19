# V0.5.0 — dev-plan

**Wave 重排** — primary path skill 先做(用户立刻拿到价值),advanced CLI factory + skill/meta-agent 重构后做。

| Wave | 内容 | 占比 | 并行度 |
|---|---|---|---|
| Wave 0 | F92 真 cost 数据源 — 共享 prerequisite | 8% | 单 subagent,1-2 天 |
| Wave 1 | F95 全局 watcher + F93a skill(`/ccteam-team`)+ F96 web 3 面板 — **primary path 闭环** | 45% | 3 subagent 并行,3-4 天 |
| Wave 2 | F93b workflow.yaml `mode: agent-team` + `__lead.md` bg spawn + `ccteam start/attach` + F94 hook 注入 — **advanced path** | 18% | 2 subagent 并行,2-3 天 |
| Wave 2.5 | **F97 advanced path lifecycle** — `cleanup_on_stop` 3 策略 + `--restart-team` + hot-reload 约束 | 7% | 单 subagent,4-5 小时(mid-cycle 入版本)|
| **Wave 3**(新)| **F100 Skill surface refactor + F101 Meta-agent 角色重塑** — 清 V0.2/V0.3 phase 残留,简化 surface | 15% | 2-3 subagent 并行,1-2 天 |
| Wave 4 | integration + host E2E + 文档同步 + ship | 7% | 主 session,1-2 天 |

总:T+11.5 天,baseline 750 → ~931(F92/F93/F94/F95/F96/F97 新测试;F100 删 team_factory_xdg_test.rs 减部分;F101 不增减测试)。

---

## Wave 0(prerequisite,必须先 ship)

### F92 — 真 cost 数据源

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/cost_summary.rs` | 重写主入口:从 state.json 切到 transcript jsonl;保留 state.json fallback |
| `crates/ccteam-core/src/pricing.rs`(新)| 内嵌 Anthropic pricing table(`include_str!("../pricing.json")`)+ token → dollar 计算 |
| `crates/ccteam-core/src/pricing.json`(新)| 当前 model 列表:sonnet-4 / sonnet-4-6 / opus-4 / opus-4-6 / opus-4-7 / haiku-4-5;字段:input / output / cache_creation / cache_read 每 1M token 美元 |
| `crates/ccteam-core/src/transcript_scanner.rs`(新)| 读 transcript jsonl,提取 `message.usage`;按 mtime + length memoize 避免重扫 |
| `crates/ccteam-core/tests/cost_summary_test.rs` | 新增 6 测试:linkScanPath 正常 / linkScanPath 缺失 fallback / pricing 4 model 覆盖 / memoize 命中 / WARN log 路径 / F84 budget cap 触发 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | `--check-pricing-version` 子选项 |

测试矩阵:
- 真 host data:dex-ui 4h $1.10 session transcript jsonl 拷成 fixture → assert cost ≈ $1.10
- 跨 model:fixture sonnet + opus 混跑
- linkScanPath 缺失:fallback + WARN
- price drift:本地 stale 触发 doctor WARN

**估工**:1 个 subagent,1-2 天。

---

## Wave 1 — Primary path 闭环(F93a + F95 + F96)

3 subagent 并行,stub agreement 先定:
- F95 emit 的 5 类 event payload schema
- F96 web API `GET /api/v1/teams` 返回 shape
- F93a skill 不依赖前两个(纯 SKILL.md 内容)

### F93a — `/ccteam-team` skill(Primary path)

| 文件 | 改动 |
|---|---|
| `skills/ccteam-team/SKILL.md`(新)| 200-300 行 skill body:parse args / 可选 Explore / Plan-first protocol / native TeamCreate + Task spawn / Worker Preamble 30 行(中文化 OMC pattern)/ definition vs ad-hoc 分支;参考 `references/omc/skills/team/SKILL.md` 但**不抄 5-stage pipeline** |
| `skills/ccteam-team/README.md`(新)| skill 入口语法 + 触发示例,给 ccteam-creator dialog 引用 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` `--install-skill` | 现 V0.4.6 装 `ccteam-control`;扩成 `--install-skill all` 装全部 ccteam skill(含新 `ccteam-team`);保持 single-skill flag 兼容 |
| `crates/ccteam-core/src/templates/skills/`(可能新增目录)| skill ln -sf 源路径常量;沿用 F89 explorer.md `include_str!` 之外的目录方式(skill 是多文件) |
| `crates/ccteam-cli/tests/install_skill_test.rs` | 6 测试:install 后 `~/.claude/agents/<scope>/<skill>.md` 存在 / 默认 scope / multi-skill / 卸载 / 升级 (re-ln) / dry-run |

**估工**:1 个 subagent,2 天(skill body 写 + 测试)。

### F95 — 全局 watcher(读 `~/.claude/teams/`,实测 schema 后简化)

实测发现 mailbox 是 `inboxes/<teammate>.json` **单文件** per 收件人,非目录;message 已带 `from/text/timestamp/color/read`。**不需要扫 transcript / 不需要 Haiku summarize**,直接读 + 截前 200 char。

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/artifact_watcher.rs` | 加 `add_agent_teams_discovery()` — daemon 启动 + 60s 周期扫 `~/.claude/teams/*/config.json`;新 team 出现 / 老 team 消失 自动 add/remove watch。**全局**,不绑任何 ccteam workflow |
| `crates/ccteam-core/src/teams_config_parser.rs`(新)| 解析 `~/.claude/teams/<>/config.json`,提取 `members[]` + 每 member 全字段;**计算 `definition_backed: bool`** = `agentType` ∉ `{"general-purpose", "team-lead"}`;diff snapshot → emit `team_member_joined` / `team_member_left` |
| `crates/ccteam-core/src/teams_inbox_parser.rs`(新)| 解析 `~/.claude/teams/<>/inboxes/<teammate>.json` 数组;diff snapshot by timestamp → 新 message → emit `team_message_sent`(text 截 200 char);**识别 `idle_notification` 类系统消息 → 分流到 F94 event(不进 mailbox)** |
| `crates/ccteam-core/src/teams_task_parser.rs`(新)| 解析 `~/.claude/tasks/<>/<id>.json`;监 dir 新文件 + modify;diff `status` → emit `team_task_created` / `team_task_completed` |
| `crates/ccteam-core/tests/agent_teams_watcher_test.rs`(新)| 用 host fixture(`references/claude-code/teams-samples/`)— config diff / inbox diff / task status 流转 / schema 失败 graceful |

**估工**:1 个 subagent,2 天。

### F96 — Web SPA Teams tab + 3 面板

| 文件 | 改动 |
|---|---|
| `crates/ccteam-web/src/api_v1.rs` | 加 5 endpoint:`GET /api/v1/teams`(列表)/ `/teams/<name>`(单 team)/ `/teams/<name>/tasks` / `/teams/<name>/inbox?teammate=&since=` / `/teams/<name>/member/<n>/definition`(definition-backed 返回 .md 解析,ad-hoc 404)|
| `crates/ccteam-web/src/subagent_resolver.rs`(新)| 按 Claude Code subagent scope 顺序(project `.claude/agents/` → user `~/.claude/agents/` → plugin → managed)解析 `<agentType>.md` 路径 + frontmatter + 标注 `skills` / `mcpServers` "not applied as teammate" |
| `crates/ccteam-web/src/sse.rs` | 加 `/api/v1/teams/<name>/events` SSE channel,推 6 类 team event(F95 5 + F94 1) |
| `crates/ccteam-web/web/spa/src/pages/TeamsListPage.tsx`(新)| `/teams` 顶级 tab,host 所有 team 卡片 |
| `crates/ccteam-web/web/spa/src/pages/TeamDetailPage.tsx`(新)| `/teams/<name>` 详情页,3 面板 layout |
| `crates/ccteam-web/web/spa/src/panels/TeamTopology.tsx`(新)| D3 force-directed;**ad-hoc/definition 徽章区分** |
| `crates/ccteam-web/web/spa/src/panels/TaskBoard.tsx`(新)| Kanban 3 列 |
| `crates/ccteam-web/web/spa/src/panels/MailboxStream.tsx`(新)| 时间线 + **`read: false` 未读高亮** + 过滤 |
| `crates/ccteam-web/web/spa/src/AppRoutes.tsx` | header tabs 加 Teams + 路由 |
| `crates/ccteam-web/tests/api_v1_teams_test.rs`(新)| 5 endpoint × 3-4 测试 |
| `crates/ccteam-web/web/spa/test/panels/*.test.tsx` | 每面板 5+ 个 test |

**估工**:1 个前端 + 1 个 API subagent,3 天。

---

## Wave 2 — Advanced path(F93b + F94)

Wave 1 ship + 用户用上 primary path 后,接续做 advanced path 给 automation use case。

### F93b — workflow.yaml `mode: agent-team` + `__lead.md` bg spawn + CLI

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs::WorkflowSpec` | 加 `pub mode: WorkflowMode`(enum `ArtifactDriven` / `AgentTeam`)+ `pub agent_team: Option<AgentTeamSpec>` |
| `crates/ccteam-core/src/workflow.rs::AgentTeamSpec`(新)| `team_name` / `lead_seed` / `teammate_mode` / `cleanup_on_stop` / `snapshot_path` / `suggested_teammates: Vec<SuggestedTeammate>` / `auto_spawn_teammates: bool` |
| `crates/ccteam-core/src/workflow.rs::SuggestedTeammate`(新)| `{ role, kind: enum {Definition, AdHoc}, spawn_brief, adhoc_model, adhoc_color, adhoc_tools }` |
| `crates/ccteam-core/src/templates/workflow.agent-team.yaml`(新)| 默认模板,`ccteam init --mode agent-team` 用;含 definition + ad-hoc 各一个示例 |
| `agents/__lead.md`(新)| Anthropic agent spec,F89 explorer.md pattern;include_str! 嵌入 binary;body 含两类 teammate spawn pattern + Worker Preamble + Plan-first Protocol;**跟 F93a skill body 复用同一套 boilerplate**(单一来源,skill body 用 macro / dev-plan 决定怎么 dedupe) |
| `crates/ccteam-cli/src/commands.rs::run_init` | 加 `--mode <artifact-driven\|agent-team>` flag |
| `crates/ccteam-cli/src/commands.rs::run_start` | agent-team mode 分支:打印 spawn preview → `[Y/n/attach]` confirm prompt(`--no-confirm`/`-y`/`--attach`/`--dry-run` 跳过)→ spawn lead → `attach` 选项 exec `claude attach <id>` |
| `crates/ccteam-cli/src/commands.rs::run_attach`(新)| 用户面 `ccteam attach <slug>`:读 team snapshot → exec `claude attach <lead-session-id>`;artifact-driven mode 返 friendly error |
| `crates/ccteam-cli/src/main.rs` clap 子命令 | 加 `Attach { slug }`(F89 用户面命令族) |
| `crates/ccteam-cli/src/commands.rs::DEFAULT_AGENT_SCAFFOLDS` | 加 `("__lead.md", include_str!("../../../agents/__lead.md"))` |
| `crates/ccteam-core/src/orchestrator.rs` | 加 `spawn_agent_team_lead(slug, spec)`;agent-team mode 项目跳过 ArtifactWatcher 装,只装 lead;`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` env 注入 |
| `crates/ccteam-core/tests/agent_team_spawn_test.rs`(新)| 12 测试覆盖 PRD F93 验收 1-12 |

### F94 — Agent Teams 3 hook 镜像(仅 advanced path 装)

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/templates/settings.agent-team.json`(新)| advanced path 独立 settings 模板,含 `TeammateIdle` / `TaskCreated` / `TaskCompleted` 三 hook;F93b init 时按 mode 选 |
| `crates/ccteam-cli/src/main.rs::HookCommand` | `ProgressAppend` 接受 5 新 event_type 字符串(`team_task_created` 等),复用现有 dispatch |
| `crates/ccteam-hooks/src/progress.rs` | 已支持任意 event_type 透传;agent-team 5 个 event 验证 payload schema |
| `crates/ccteam-core/src/orchestrator.rs::Event` enum | 加 6 变体(`#[serde(rename = "team_*")]`)|
| `crates/ccteam-core/tests/event_team_test.rs`(新)| 6 event 序列化 / 反序列化 / payload 校验 |
| `docs/interfaces.md §6.4` | 7 类 event 表扩到 13 类(7 + 6 team_*) |

**估工**:F93b + F94 2-3 个 subagent,2-3 天。

---

## Wave 2.5 — F97 Advanced path lifecycle 完善(mid-cycle 入版本)

Wave 2 advanced path 起来后,补 3 个生命周期 gap:graceful stop / sleep-resume / hot-reload 约束。单 subagent,~4-5 小时。

### F97 — `cleanup_on_stop` 3 策略 + `--restart-team` + hot-reload diff

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs` | `CleanupOnStop` 枚举(`force-kill` / `ask-lead` / `leave-running`,`#[serde(rename_all = "kebab-case")]`);`AgentTeamSpec::cleanup_on_stop` 从 `Option<String>` 改 `CleanupOnStop`(`#[serde(default)]` → `ForceKill`);`AgentTeamSpec::classify_reload(other)` 方法 |
| `crates/ccteam-core/src/state.rs` | `ProjectState::detached: bool`(`#[serde(default, skip_serializing_if = is_false)]`)|
| `crates/ccteam-core/src/harness.rs` | 暴露 `pub fn sigkill_pid` + `pub fn parse_pid_from_state`(F97 force-kill / pid probe 复用)|
| `crates/ccteam-core/src/orchestrator.rs` | hot-reload 分支:`handle_agent_team_reload` 方法读 workflow.yaml + snapshot → 调 `classify_reload` → `AgentTeamReloadOutcome::{HotApplied, ColdRequired, NotApplicable}`;`snapshot_to_team_spec` 反序列化 helper |
| `crates/ccteam-cli/src/main.rs::Stop` | `<slug>` arg + `--stop-timeout <secs>`(默认 60);`Start` 加 `--restart-team` flag |
| `crates/ccteam-cli/src/commands.rs` | `run_stop_slug` + `StopSlugOptions`;3 个 cleanup helper(`force_kill_lead` / `ask_lead_cleanup` / `leave_running`);`run_restart_team` + `RestartTeamOutcome::{ResumedAlive, FellThroughToSpawn}`;`run_start_agent_team` 加 `restart_team` 分支 + detached refusal |
| `crates/ccteam-core/src/templates/workflow.agent-team.yaml` | cleanup_on_stop 3 策略注释 + hot-reload HOT/COLD 字段表 |
| `crates/ccteam-core/tests/agent_team_lifecycle_test.rs`(新)| 16 测试:cleanup_on_stop 3 解析 + 默认 + unknown error;classify_reload HOT lead_seed / teammate_mode / cleanup_on_stop / adhoc_color;COLD team_name / role / kind / spawn_brief / count;空 diff |
| `crates/ccteam-cli/src/commands.rs::tests::*` | 8 测试:force-kill 路径 SIGKILL + 清 snapshot;ask-lead 写 inbox + timeout 退化 force-kill;ask-lead workflow_done 命中跳过 force-kill;leave-running 留 snapshot + 设 detached;restart-team alive lead 不 spawn + 清 detached;restart-team terminal lead WARN + 回落;restart-team 无 snapshot friendly error;plain start 在 detached 上 refuse |

### 验收
1-10:见 PRD F97 §验收(`cleanup_on_stop: ask-lead` 不 SIGKILL / `leave-running` lead 存活 / `--restart-team` 复活 / cold-reload event / detached refusal 等)

### 风险
- **`ask-lead` 依赖 lead 自觉读 inbox** — 当前 `__lead.md` 模板里没显式说 "ccteam writes stop requests to .ccteam/inbox/<ts>-stop-request.md, you must pick those up"。Wave 4 host E2E 时验证 + 必要时 patch `__lead.md`
- **`probe_job` Terminal 误判** — Anthropic state.json schema 漂移可能让 `firstTerminalAt` 解析错。F80 测试覆盖了 stable shape,host E2E 再验

**估工**:单 subagent,4-5 小时(本文件作者执行)。

---

## Wave 3 — Skill + Meta-agent refactor(F100 + F101)

2-3 subagent 并行(F100 删除任务可分两路:skill/teams 一路,team_factory_cli/rs 一路;F101 单独一路 rewrite meta_agent_role.md)。

### F100 — Skill surface refactor

#### 删除任务

| 路径 | 工具 | 备注 |
|---|---|---|
| `skills/ccteam-team-author/` | `git rm -rf` | 整目录 |
| `skills/ccteam-project-creator/` | `git rm -rf` | 整目录,内容先吸进 ccteam-creator |
| `crates/ccteam-cli/src/team_factory_cli.rs` | `git rm` | + 同步 `crates/ccteam-cli/src/main.rs` 删 `Team*` clap 子命令 + `mod team_factory_cli` |
| `crates/ccteam-core/src/team_factory.rs` | `git rm` | + 同步 `crates/ccteam-core/src/lib.rs` 删 `pub mod team_factory` |
| `crates/ccteam-core/tests/team_factory_xdg_test.rs` | `git rm` | 测试 |
| `teams/dev/` | `git rm -rf` | V0.2 phase team |
| `teams/research/` | `git rm -rf` | V0.2 phase team |
| `teams/research-academic/` | `git rm -rf` | V0.2 phase team(上次 phase hook cleanup 已提及)|

#### 重写任务

| 文件 | 来源 | 改动 |
|---|---|---|
| `skills/ccteam-control/SKILL.md` | 147 → ~120 行 | 删 5 处 phase 提及;V0.5.0 skill family 速查表(链 `ccteam-creator` 创项目 / `ccteam-team` 起 team);17 MCP 工具列表 review |
| `skills/ccteam-creator/SKILL.md` | 284 + 162 → ~300 行(合并)| 吸 `ccteam-project-creator` 的 step 1/2/3/4 new-project dialogue(原"4-phase"改名 step,避免跟 V0.4.0 已删 ccteam phase 混淆);保留自身的 workflow.yaml + agent.md + skill scaffold dialogue;删 11 处 phase 提及 + 删对 `ccteam-team-author` 引用 |

#### docs 同步

| 文件 | 改动 |
|---|---|
| `CLAUDE.md §四 Skills` | 4 skill → 3 skill |
| `docs/claude-code-best-practices.md:171` | 同上 |
| `docs/tech-design.md` | 删 team factory 段落(line ~807, 833) |
| `docs/versions/v0-4-6/user-manual.md:173` | `ccteam team init/publish/show` 加 "V0.5.0 removed" 注释,引导用 `ccteam-team` skill |
| `docs/requirements.md:345` | `ccteam team init` → `ccteam-creator` skill 引用 |
| `docs/interfaces.md:651` | 去 `team` subcommand |
| `docs/research/*.md` | **不更新**(research 文档按规则不进 SoT)|

**估工**:F100 1-2 subagent 并行(删除 + 重写两路),1-1.5 天。

### F101 — Meta-agent 角色重塑

#### 重写

| 文件 | 行数 | 改动 |
|---|---|---|
| `crates/ccteam-core/src/templates/meta_agent_role.md` | **303 → ~150** | 删 26 处 phase 提及;删 V0.2 "Seed Gate" / kickoff 段;改 V0.5.0 routing 决策树(详 PRD F101)|
| `crates/ccteam-core/src/templates/kickoff_reverse_interview.md` | rm | V0.2 反向访谈,V0.5.0 `ccteam-creator` skill step 1-4 替代 |
| `crates/ccteam-core/src/templates/review_with_user_loop.md` | rm | V0.2 review loop,V0.4.0 phase 删后失效 |
| `crates/ccteam-core/src/templates/memory_bridge_dev.md` | 保留 | cross-project memory 仍有效 |
| `crates/ccteam-core/src/templates/memory_bridge_research.md` | 保留 | 同上 |
| `crates/ccteam-core/src/templates/settings.json` | 审查 | 确认无 phase hook 引用(上次 phase hook cleanup 应已清)|
| `teams/meta-agent/team.yaml` | 简化 | 删 phase 字段,保留 name/description/cwd 等身份元数据 |
| `crates/ccteam-core/src/meta_agent.rs` | 审查 + 改 | grep `phase` / `kickoff` / `review_with_user_loop` 引用,逐处删 |

#### docs 同步

| 文件 | 改动 |
|---|---|
| `CLAUDE.md §一` 表 + 描述段 | meta-agent 从"全权调度"改"轻量 router + memory bridge + dashboard" |
| `docs/tech-design.md §2.1` 3-layer 描述 | L1 meta-agent 角色更新 |

**估工**:F101 单 subagent,1 天(重写 meta_agent_role.md 是核心,3 文件删除 + 1 文件简化 + 1 文件审查是辅助)。

---

## Wave 4 — Integration + ship

- 全 wave merge + host E2E(用 host roblog team 实际验证 F95/F96)
- bug fix loop
- 文档同步:`interfaces.md` / `tech-design.md`(L1 加 skill / L2 daemon 加全局 team watcher)
- `user-manual.md` 写(primary path 入门 + advanced 决策树)
- `cargo workspace version` bump 0.4.6 → 0.5.0
- commit `v0.5.0: ...` + push tag
- CLAUDE.md baseline 回填

**估工**:主 session,1-2 天。

---

## 测试矩阵全景

| 层 | 数量 | 覆盖 |
|---|---|---|
| Rust unit | +30 | F92 6 / F93b 12 / F94 6 / F95 6 / F96 — / 共享 helper |
| Rust integration | +12 | spawn lead + 装 hook + 镜像 event;F92 host fixture;mode 切换;skill install |
| Rust F97 lifecycle | +24 | 16 lifecycle (cleanup_on_stop 解析 + classify_reload hot/cold) + 8 CLI (run_stop_slug + restart-team + detached refusal) |
| Web API | +10 | 5 endpoints × 2 cases |
| Web SPA component | +15 | 3 面板 × 5 |
| **总新增** | **+91** | baseline 750 → ~931 |

测试运行:`cargo test --workspace --locked` + `npm test --prefix crates/ccteam-web/web/spa`。

---

## 风险盘点

| 风险 | 缓解 |
|---|---|
| Anthropic Agent Teams 协议漂移(`config.json` / `tasks/` / `inboxes/`)| F95 解析失败 WARN 不 panic;镜像 degrade 到 mtime-only;version-gate `doctor --check-agent-teams` |
| `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 改名废弃 | 锁 Claude Code 版本下限到 V0.5.0 ship 时 stable;F99 加 doctor 检测 |
| F92 pricing.json 跟 Anthropic 公开价漂移 | pricing.json 内嵌 schema_version + ship date;`doctor --check-pricing-version` 半年触发 WARN |
| **F93a skill body vs F93b `__lead.md` body 双源漂移** | dev-plan 强制:相同 boilerplate(Worker Preamble / Plan-first Protocol)单一源,两端 `include_str!` 同一文件 |
| Skill `/ccteam-team` 跟用户已有 `/team` skill 冲突 | namespace 用 `ccteam:team`;`ccteam doctor --install-skill` 装时检查 collision 并提示 |
| `__lead.md` 用户改坏(advanced path)| `ccteam doctor --validate-team` 加 `__lead.md` body hash check;改了 WARN,不 fail |
| 3 个新 web 面板大幅扩 SPA 体积 | React lazy + code splitting,`/teams` 路由才加载 chunk |
| Wave 1 / Wave 2 双 path 共享 `agents/__lead.md` 但路径不同(skill 加载 vs system prompt 装载)| 设计阶段定:`agents/__lead.md` 作为 system prompt 给 advanced path 用;skill body 直接 include 同一文件作为 supplemental — 同一份 boilerplate 双用 |

---

## ship 节奏

| 阶段 | 内容 | 截止 |
|---|---|---|
| **doc review** | 本 PRD + dev-plan 用户 review,确认 双 path 边界 / red lines | T+0 |
| **Wave 0** | F92 单 subagent + ship merge | T+2 天 |
| **Wave 1 并行 3 subagent** | F93a skill + F95 watcher + F96 web | T+5 天 |
| **Wave 1 ship intermediate** | host E2E:roblog team 在 web 出现 + `/ccteam-team` 在新 repo 起 team | T+5 天(中检验) |
| **Wave 2 并行 2 subagent** | F93b CLI factory + F94 hook 注入 | T+7 天 |
| **Wave 3 并行 2-3 subagent** | F100 skill refactor(删 team-author + 合并 project-creator + 清 phase)+ F101 meta_agent_role 重写 | T+9 天 |
| **Wave 4** | integration + 文档 + ship + tag | T+11 天 |
| **CLAUDE.md baseline 回填** | 新测试数(预计 ~810,-5 from team_factory_xdg_test rm)+ V0.5.0 进当前最新版字段 | ship 后 |

---

## 与 V0.4.6 + V0.5.x 关系

- **V0.4.6 全部保留** — `mode` 字段缺失 = `artifact-driven`,V0.4.6 跑得动 V0.5.0 binary 不需要改 workflow.yaml
- **V0.5.1+ 候选**:
  - F98 plan-approval ↔ outbox 联动
  - F99 Claude Code 版本 gating doctor 检测
  - `orchestration-patterns.md §五` 剩余:动态 Routing sugar / Evaluator-Optimizer 显式 sugar / workflow.yaml `extends`

---

## 红线复述

参 `prd.md` 各 finding "红线" section + V0.5.0 整体红线。dev-plan 实施层守住:

1. **不**在 Rust 进程内模拟 lead 行为(不直接写 `~/.claude/teams/`)
2. **不**给 lead session 注入 system prompt(`lead_seed` 是 user-turn message)
3. **不**破 V0.4.6 现有 7 event;新 6 event 严格 `team_*` 命名前缀
4. **不**让 F92 影响调用方签名;数据源切换内部完成
5. **不**让 web SPA agent-team 面板影响 artifact-driven workflow 渲染路径
6. **Primary path 零 ccteam workflow 依赖** — F93a skill 在任何 git repo / 任何路径下都跑得起来
7. **`/ccteam-team` 不修改用户 settings.json** — hook 注入是 F93b advanced 专属
8. **Wave 1 ship 后用户应立刻可用**(`/ccteam-team` 跑通)— 不等 Wave 2 advanced path
