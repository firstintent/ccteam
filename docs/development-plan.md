# ccteam 开发计划

> 本文从 [tech-design.md](./tech-design.md) §7 拆出并扩展。
>
> - `requirements.md` 回答 **为什么做**(10 条痛点)
> - `tech-design.md` 回答 **怎么做**(架构、协议、扩展点)
> - **本文回答 何时做什么、做到什么标准、谁阻塞谁**——是单一权威的进度文档,PR 必须能映射回本文某条任务。

---

## 0. 总体节奏

| 里程碑 | 时长 | 主目标 | 关键解锁的痛点 |
|---|---|---|---|
| **M0** | 2–3 周 | 单项目 CLI MVP——一句话需求 → 自动跑出能用的代码 | 1, 2, 3, 4, 7, 8, 9 |
| **M0.5** | 1 周 | 工具触发面闭环——让 Claude Code 全套能力在自治模式下真正可调 | 11(地基)、12(地基) |
| **M1** | 2 周 | 多项目并发 + Telegram 入口 | 5, 9(强化) |
| **M2** | 2 周 | Seed Gate 否决无效想法 + Score 客观质量门 | 6, 3(强化) |
| **M3** | 2 周 | Team Abstraction——`ccteam new --team=research` 跑通,dev 路径零回归 | 团队泛化地基 |
| **M4** | 3 周 | 跨项目记忆(差异化护城河;retro_schema 团队感知) | 10 |
| **M5** | 3 周 | Critic Agent 闭环——超越"测试通过=完成";critic_dimensions 团队感知 | 3(深化) |
| **M6** | 3–6 月 | 大型软件长跑能力(对标 Symphony) | 长期 |

**累计**:M0–M5 大约 15–16 周,基本覆盖 requirements.md 所有痛点。M6 进入开放探索期。

**2026-05-05 reorder**:原 plan M3=记忆 / M4=Critic / M5=Symphony。现 M3=Team Abstraction
插入到记忆 / Critic 之前——因为 retro_schema(M4)和 critic_dimensions(M5)都需要团
队抽象作为前提,否则会写死 dev 字段后再被迫推倒重来(详见 `docs/dev-coupling-audit.md`
F20 / `docs/ccteam-as-domain-agnostic-orchestrator.md` §2.3 / §6)。

---

## 1. 痛点 → 里程碑反向映射

每条痛点应能指出"在哪个里程碑被解决到什么程度":

| 痛点 | M0 | M0.5 | M1 | M2 | M3(team) | M4(memory) | M5(critic) |
|---|---|---|---|---|---|---|---|
| 1. 想法死于"开始" | 跑通端到端 | — | — | — | — | — | — |
| 2. AI 仍要求当 PM | 跑通自治流水线 | — | — | Seed 不再问"做不做" | — | — | Critic 不再问"够不够好" |
| 3. 测试是黑洞 | tests-pass 即终态 | — | — | golden-rules + 6 维评分 | — | — | anti-leniency + WEAK BLOCK(team-aware critic_dimensions) |
| 4. bug 修复无限循环 | fix-loop 上限 3 + escalate | — | — | — | — | — | — |
| 5. 想法多全部烂尾 | — | — | 多项目并发 + 排队 | — | — | — | — |
| 6. 不是每个想法都值得做 | — | — | — | Seed REJECT/CLARIFY | — | — | — |
| 7. 进度不透明 | tmux + progress.jsonl | — | — | — | — | — | — |
| 8. 每步都点允许 | `--dangerously-skip-permissions` | — | — | — | — | — | — |
| 9. AI 团队需要主持 | 守护进程 + tmux long session + CLI `--format json` | — | Telegram 入口替代 CLI + `ccteam-control` skill | `ccteam-mcp` MCP server | — | meta-agent 跨项目记忆落地 | — |
| 10. 每项目从零开始 | — | — | — | — | — | RAG 召回 + 反模式(retro_schema 团队感知) | — |
| 11. 关键节点不把控 | L1 架构约束(hooks + required_outputs) | **L1 扩展:tools_required + 启动期可达性校验** | cross-cutting watcher 上线 | 单 critic + dev 隔离;L3 telegram fork 决策 | — | phase 内 audit 矩阵 + 投票共识 | anti-leniency + WEAK BLOCK |
| 12. 工作流编排 | phase 主干 + `sub_skills` 字段定义(空允许) | **plugin agent 注册 + skill 懒注入**(让 sub_skills 真有得调) | — | sub-skill 自动 trigger + 产物接力 | 团队抽象解锁多团队 sub_skills 共享 | 新插件按 `skill_intent.yaml` 自动挂载 | — |
| 13. 并行规模 | phase 模板 `parallelism: solo` 字段(只此一档) | — | — | `parallelism: agent_team` 启用 | — | `parallelism: multi_session` 启用 | 自动并行规模识别 |
| **新:团队泛化地基** | — | — | — | — | `ccteam new --team=research` 跑通,§B 审计 P0 全清 + team.yaml schema 落地 | retro_schema 团队感知(F20)、critic_dimensions 数据驱动(§A §2.3 invariant 1) | critic_dimensions per-dim anti_leniency_strictness(§A §2.3 invariant 2) |

**门槛规则**:某个里程碑若未真正解决其声明的痛点,**不许跳到下一个里程碑**——这是质量门,不是日历推进。

---

## 2. M0 — 单项目 CLI MVP(active)

**唯一验收**:用 CLI 提一个需求 → 关掉所有终端 → 半小时后回来 → 看到一个能跑的项目 + 测试报告。

### 2.1 任务清单

| # | 任务 | 验收(可执行) | 依赖 | 对应 tech-design 章节 |
|---|---|---|---|---|
| **W1 — Foundation** |
| M0.1 | 仓库骨架(Cargo workspace) | 顶层 `Cargo.toml` workspace + 三 crate:`crates/ccteam-core`(lib,状态/协议/tmux 包装/hook 共享 schema)+ `crates/ccteam-cli`(bin,主入口 `ccteam`,含 `hook` 子命令组)+ `crates/ccteam-hooks`(可独立 lib,也可合并到 cli);`phases/` + `tmux/` 保留独立目录(模板 + tmux layout 不进 Rust crate);`cargo build --release` 出单 binary `ccteam`,`ccteam --version` 能跑 | — | §4 目录现状、§6.1 |
| M0.2 | 5 个最小 phase 模板 | `phases/{02-plan-eng,03-implement,04-test-author,05-test-run,06-fix,09-ship}.md` 都带 YAML front matter(必含 `name` / `required_inputs` / `required_outputs` / `parallelism` / `agent_team` / `sub_skills` 字段——M0 `parallelism: solo` 写死,`sub_skills` 列表可空),可被 orchestrator 解析 | — | §3.3、§6.10、§6.11 |
| M0.3 | `ccteam hook` 子命令组(3 个 hook 子命令) | 在 `crates/ccteam-cli` 加 `hook` subcommand group,每个 hook 是一个 fn:`ccteam hook progress-append` / `ccteam hook parse-phase-end` / `ccteam hook cost-accumulate`;手动喂 stdin JSON 验证 stdout 输出符合 interfaces §6 schema | — | §6.2 |
| M0.4 | 产出项目 settings.json 模板 | 模板渲染后挂到示例项目,`claude` 启动后 SessionStart hook 写出 ready 标记 | M0.3 | §6.2 |
| **W2 — Orchestrator** |
| M0.5 | state.json schema + 原子读写 | `.tmp` + rename;启动校验 schema;损坏走 backup | M0.1 | §5.2 |
| M0.6 | orchestrator 主循环 | `asyncio` 30s 轮询 + inotify 监听 progress.jsonl;`ccteam start --foreground` 跑得起;**解析 phase 模板的 `sub_skills` 字段时空列表 no-op 不报错**(M2 才真正调度);**`parallelism` 字段读到非 `solo` 报错并 fail-fast**(避免静默走错路径) | M0.5 | §3.2、§6.10、§6.11 |
| M0.7 | tmux session 启动 + 重连 | 首启用 `tmux new-session -d ... claude`;重启走 `tmux has-session` + `kill -0 pid` 双重校验 | M0.6 | §6.1 |
| M0.8 | idle-aware 注入 | tail progress.jsonl 末尾事件;`Stop`/`idle_prompt` → `send-keys`;否则 `/btw` | M0.7 | §6.9 |
| M0.9 | PHASE_DONE/ESCALATE 解析 + 状态机转移 | Stop hook 解析 claude 最后一行;orchestrator 收到事件后按 §3.2 状态机切换 | M0.8 | §3.2、§4.4 |
| M0.10 | context 60% 边界 reset | PostToolUse hook 累加 token;在下一个 phase 边界 `/exit` + 新 session + `.ccteam/CLAUDE.md` 桥接 | M0.9 | §6.9 |
| **W3 — CLI + Fix-loop + 兜底** |
| M0.11 | 核心 CLI 命令 | `ccteam new` / `ls` / `show` / `start` / `attach <slug>` / `resume <slug>` / `peek` / `progress` 全部可用;**所有查询命令支持 `--format json`**(`ls --format json` / `show --format json` 必出,schema 见 interfaces §10.3)——为"用户自带 claude 当入口"路径打底 | M0.6 | §3.8、§10.1、§10.3 |
| M0.12 | fix-loop 集成 ralph-loop Stop hook 模式 | 进入 fix phase 时 orchestrator 写 fix-loop 状态文件;Stop hook 检测到则按 ralph-loop 范式拦截退出 + 重喂同一段 fix prompt;3 次未过则清状态文件,orchestrator 接管 escalate | M0.9 | §3.5(改写) |
| M0.13 | stall 检测三档软告警 | 5/15/30 min 阈值触发不同等级日志(M0 暂用 stderr,M1 接 telegram) | M0.6 | §6.8 |
| M0.14 | 成本累计 + 软告警 + 物理上限 | hook 读 transcript_path,parse `usage.*` 累加 → state.json;阈值 $20/$50/$200 三档,$200 真 kill | M0.6 | §6.8 |
| M0.15 | 端到端打通 | 一次完整 happy path:CLI 提需求 → tmux session 自启 → 跑 plan-eng → implement → test → fix(若有)→ ship,全程不需要主对话 | M0.1–M0.14 | §4.1 |

### 2.2 M0 不做的事(scope guard)

明确排除,出现在 PR 描述里就要拒:

- 多项目并发(M1)
- Telegram bot(M1)
- Seed phase——M0 默认所有需求 PASS 直接进 plan(M2)
- 跨项目记忆 / RAG(M3)
- Score / Critic / 6 维评分(M2/M4)
- `--resume` 兜底崩溃恢复(M0 用 `/exit` + 新 session + CLAUDE.md 桥接;`--resume` 留作 M1+ 增强)
- web dashboard、score UI 之类的可视化(永远不在 ccteam 主线)

### 2.3 M0 主要风险

| 风险 | 触发场景 | 兜底 |
|---|---|---|
| Stop hook 未触发就卡死 | claude 进程 hung,既不 idle 也不工具调用 | M0.13 stall 检测兜底——30 min 升级 escalation |
| context reset 时 CLAUDE.md 桥接信息不全 | 写入"当前进度"节遗漏关键决策 | M0.10 在写桥接前要求 claude 自己输出"当前进度"摘要,orchestrator 拼接而非凭空写 |
| ralph-loop Stop hook 与 ccteam 自有 Stop hook 冲突 | 同时存在两个 Stop hook,顺序不确定 | M0.12 把 ralph-loop 风格逻辑合并到 ccteam 自己的 parse-phase-end.sh,**不**装两个 Stop hook |
| send-keys 注入与用户 attach 同时输入造成 race | 用户 ccteam attach 中键入,正好 orchestrator 也在 send-keys | tech-design §6.1:PreToolUse hook 检测输入源,orchestrator 检测 user_attach 立即暂停自动注入 |

---

## 2.5. M0.5 — 工具触发面闭环(1 周)

> 本里程碑由 [docs/claude-code-tool-surface.md](./claude-code-tool-surface.md)
> 的实测结论催生。M0 happy path 跑通后,我们发现"phase markdown 能编排
> 什么 / orchestrator 能触发什么"的边界一直靠假设没有契约——把契约钉死
> 是 M1+ 所有"高级编排"功能的地基。

### 2.5.1 本阶段痛点(M0 跑通后才暴露的盲区)

ccteam 定位是"在 Claude Code 之上、不阉割其能力、自动调用其全套工具",
但 M0 之后实测发现以下**不让 ccteam 真正落地这条定位的硬障碍**:

1. **plugin agent 装了 plugin 仍然 Task 调不到** —— `pr-review-toolkit`
   等 plugin 里的 `agents/<name>.md` 不进 Task 全局注册表;`Task(subagent_type="code-reviewer")`
   报 "Agent type not found",Available 列表只有 5 个内置 subagent。M1.6 cross-cutting
   watcher、M2.6 sub-skill 自动调度全建立在 plugin agent 真的能调上,这条不
   解决,后续都是空中楼阁。**(实测确认 2026-05-05,详见 tool-surface §1.2.5)**
2. **agent 文件不热加载** —— `~/.claude/agents/<name>.md` 在会话启动时一次
   性扫描,中途 `ln -sf` 无效。**bootstrap_project 必须在 `tmux new-session`
   之前完成 ln -sf**,这条是硬约束。
3. **skill 实时监听被白白浪费** —— Claude Code 对 SKILL.md 实时监听,中途
   写文件立即可调,但前提是顶层 skills 目录会话启动时已存在。M0 没占位,
   未来 director-claude 想"按 phase 懒注入 skill"会撞上这个边角。
4. **phase 模板没有工具依赖声明** —— phase markdown 里 @ 引用 plugin agent
   或 Task subagent,运行时才发现工具不存在,silent fail。orchestrator 应该
   **启动期就校验**,缺工具直接 fail-fast 告诉用户。
5. **ESCALATE 自由文本被误用** —— phase 写 "ESCALATE: 请 reset",但
   orchestrator 是 Rust 程序,看不懂自然语言;却又没有结构化指令通道,
   导致"phase 想跨层指挥 orchestrator"无路可走。
6. **没有"工具体检"** —— 用户重装机器、ccteam 升级、plugin 改名后,phase
   模板里某个 subagent_type 还在不在?M0 没有任何体检命令,问题只在运行
   时第一次撞上才知道。

### 2.5.2 唯一验收

新建一个项目 →`ccteam new "<brief>"` 自动把所需 plugin agent 链好、skills
目录占位好、`ccteam doctor --tool-surface` 报 `所有 phase 的 tools_required
全部可达` →`ccteam start` 跑端到端 happy path,过程中 phase markdown 能成
功 `Task(subagent_type="code-reviewer")` 自调插件级 review,**全过程没有
人工 ln -sf 也没有 `/reload-plugins`**。

### 2.5.3 任务清单

| # | 任务 | 验收(可执行) | 依赖 | 对应文档 |
|---|---|---|---|---|
| M0.5.1 | `bootstrap_project` ln -sf 推荐 plugin agents | `ccteam new` 在写 settings.json 之后、orchestrator `ensure_session` 之前,把 `tool-surface.md §6.2` 列出的 8 个推荐 agent(`code-reviewer` / `code-architect` / `code-explorer` / `code-simplifier` / `silent-failure-hunter` / `pr-test-analyzer` / `type-design-analyzer` / `comment-analyzer`)`ln -sf` 到 `~/.claude/agents/`;首次跑产生新文件、再次跑幂等;ln -sf 之后启动的 claude session 能成功 `Task(subagent_type="code-reviewer")` | M0.15 | tool-surface §1.2.6 |
| M0.5.2 | `bootstrap_project` 占位 skills 目录 | `mkdir -p ~/.claude/skills/ <project>/.claude/skills/`(即使空也建);verify session 启动后 SKILL.md 写到这两个目录都能立即被 Skill 工具识别 | — | tool-surface §1.2.4 |
| M0.5.3 | phase YAML 增 `tools_required` 字段 | front matter 加 `tools_required: { subagents: [...], skills: [...], mcp: [...] }`;`ccteam-core` 解析;**启动期校验**——orchestrator init 时枚举 `~/.claude/agents/`、`~/.claude/skills/`、当前 MCP server,与所有 phase 模板 `tools_required` 交叉比对,缺谁报缺谁 + 给出修复命令(如 `ccteam doctor --install-recommended-agents`) | M0.5.1、M0.5.2 | tool-surface §1.1.3、§6 |
| M0.5.4 | 结构化 ESCALATE 语法 | `parse-phase-end` 识别三档前缀:`ESCALATE: REVERT_TO_PHASE <name> — <reason>` / `NEED_USER_INPUT — <questions>` / `ABORT — <reason>`;无前缀降级为通用 escalation(等价 NEED_USER_INPUT);三档分别走不同 orchestrator 路由(回退 phase / 进 inbox 等用户 / 永久标 failed);interfaces.md 同步增 ESCALATE grammar 章节 | M0.9 | tool-surface §2.2.3 |
| M0.5.5 | `ccteam doctor --install-recommended-agents` | 命令对当前已有项目补做 M0.5.1 的 ln -sf;支持 `--dry-run`;不破坏用户手工放在 `~/.claude/agents/` 的自定义 agent | M0.5.1 | — |
| M0.5.6 | `ccteam doctor --tool-surface` 体检 | 跑出报告:每个 phase 模板 `tools_required` 与当前可达性的交叉表;subagent 列表用启动一个 `claude --dangerously-skip-permissions` headless 子进程跑 `Task(subagent_type="probe-XXX")` 拿 Available 列表反推(或读 `~/.claude/agents/` 直接枚举);MCP / skill 同理;输出 markdown 表格,缺项标红 + 给出修复命令 | M0.5.5 | tool-surface §6.6 |
| M0.5.7 | 实测回归:plugin agent 端到端 | 在 review phase 模板里加一段 "请 `Task(subagent_type="code-reviewer")` 自检",`ccteam new` → 跑通 review → progress.jsonl 出现 `event: "subagent_done", subagent_type: "code-reviewer"` 事件 | M0.5.1–M0.5.6 | tool-surface §1.1.4 |

### 2.5.4 M0.5 不做的事(scope guard)

- **director-claude watcher**(M1+)—— 本阶段只解决 "工具能调" 的地基,
  "什么时候该调哪个" 是上层决策,留给 M1
- **phase 内 sub_skills 自动 trigger**(M2.6)—— 本阶段只补 `tools_required`
  声明 + 启动期校验,运行期自动调度还是 M2 的事
- **mid-session 装新 plugin** —— 这条 Claude Code 不让做(实测 §1.2.4),
  ccteam 不挑战这条边界
- **重写 `build_phase_prompt` 为 LLM 动态生成** —— 这是把 cache 命中位置
  搬到没缓存位置,违反 tech-design §6.1。本阶段维持死壳子,内容靠 phase
  markdown 表达

### 2.5.5 M0.5 风险

| 风险 | 触发场景 | 兜底 |
|---|---|---|
| `ln -sf` 与用户手工放的同名 agent 冲突 | 用户已在 `~/.claude/agents/code-reviewer.md` 放了自定义版本 | M0.5.1 检测目标已存在且非软链时跳过并告警;`--force` 才覆盖 |
| `tools_required` 校验过严 fail-fast 阻塞老项目 | 已经在 M0 跑了一半的项目升级 ccteam 后启动报缺工具 | M0.5.3 校验失败给出 `ccteam doctor --install-recommended-agents` 一键修复;`ccteam start --skip-tool-check` 临时跳过 |
| 推荐 agent 清单跟 plugin 升级失同步 | plugin 改名 / 删除 agent 文件 | M0.5.6 体检跑出来红;CI 加一条 nightly 跑 `--tool-surface` 提前发现 |

---

## 3. M1 — meta-agent + 多项目并发(2 周)

> **2026-05-06 reframe**:原 M1 主目标是"Telegram bot + 多项目 + Telegram fork
> 决策"。复盘 + tech-design §2.1 的三层架构定下后,**Telegram bot 实现下沉
> 到 Channel Layer(M2+),且优先复用开源方案;M1 的核心交付改为
> meta-agent session + inbox/outbox 协议 + 多项目调度**。这样 M1 完工 =
> User Interaction Layer 全员到位,channel 层在不在线都能跑(终端 attach
> 即对话)。

**唯一验收**:在终端跑 `ccteam start`,然后 `tmux attach -t ccteam-meta-<user>`,
**用 NL 在 meta-agent claude TUI 里说**"做一个 todo cli";meta-agent 用
ccteam-control 调 `ccteam new`,起项目 session;再说 5 个不同想法,看到 3 个并
发跑、2 个排队;关掉所有终端,半小时后回来,3 个跑完,在 meta-agent 里 NL 问
"你们做完了吗",得到正确摘要回答。**全程不需要 Telegram 或任何外部 channel**。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M1.0 | **meta-agent session 骨架(新)** | `ccteam doctor --install-meta-agent <user-handle>` 命令落地;创建 `~/projects/<user>-meta/` 目录,写 meta-agent role prompt 到 `.ccteam/CLAUDE.md`(含 dispatch 决策树 + dispatcher-not-worker 行为约束,见 strategic doc §7.2.2 / §7.2.3);ln -sf `ccteam-control` skill;orchestrator 把 meta session 当一种特殊"team type"管(常驻、永不 terminal、事件循环);`ccteam start` 自启 meta session;`tmux attach -t ccteam-meta-<user>` 即 NL 对话入口 | M0.5 | tech-design §2.1 / §3.8 / strategic doc §7 |
| M1.1 | **inbox/outbox 文件协议加固(改)** | `<session>/.ccteam/inbox/msg-<n>.md`(NL markdown + 顶部 YAML 元数据:source channel / timestamp / user)`<session>/.ccteam/outbox/reply-<n>.md`(同 schema);orchestrator inotify watch inbox,触发 send-keys 注入对应 session(idle 直送 / 忙加 `/btw`);interfaces.md 加协议章节。**M1 不实现具体 channel**,只钉协议 | M1.0 | tech-design §2.1.2 / interfaces.md |
| M1.2 | 多项目并发调度 | `max_concurrent_projects=3` 准入;超出排队;每项目独立 tmux session;meta session 不计入并发上限(它常驻) | M1.0 | tech-design §6.1 |
| M1.3 | meta-agent dispatch 端到端 | meta session NL 指令 `"做一个 todo cli"` → meta 用 `ccteam-control` 调 `ccteam new` → 项目 session 启动 → meta 在 tmux pane 看到"项目已派,跟踪中"反馈;5 个连续派单测试串行准入 | M1.0、M1.2、M1.8 | strategic §7.2.2 |
| M1.4 | 项目级 CLAUDE.md 自动生成 | plan phase 后写;reset 时追加"当前进度"节(已在 M0.10 设计,M1 兑现到 meta session 也享受) | M0.10 | tech-design §6.5 / §6.9 |
| M1.5 | 优雅停机 + 重启自恢复 | `ccteam stop` 不杀 session(包含 meta session);`ccteam start` 自动 reattach 所有活跃 session(meta + 项目) | M1.2 | tech-design §6.1 |
| M1.6 | cross-cutting watcher agents(L2 起步) | `cost-watcher` + `scope-watcher` 实现;**Stop hook 触发**(每 phase 边界跑一次,不在 PostToolUse 跑——避免 300+ 次/phase 灌爆);输出 PASS/CONCERN/BLOCK,append `progress.jsonl`;BLOCK 写 `escalation.md` | M1.5 | tech-design §3.6 L2、§6.3 模式 B |
| M1.7 | **L3 fork 决策走 NL 通道(改)** | watcher BLOCK / fix-loop escalate 时,orchestrator 写 `escalation.md`;meta-agent watcher 检测到 → 在 meta session 用 NL 描述项目卡点 + 备选方案;用户 NL 回复(在 meta session pane 或将来 channel),meta 解析后用 `ccteam-control` 把决策注入对应项目 session 的 inbox。**砍掉旧的 ABC structured push** | M1.6、M1.0 | tech-design §3.6 L3 |
| M1.8 | `ccteam-control` skill 发行 | binary 内嵌 SKILL.md;`ccteam doctor --install-skill` 写到 `~/.claude/skills/ccteam-control/`;字段约定 + body 必含章节按 interfaces §11 落地;**首要 consumer 是 meta-agent session**(M1.0 自动装);辅助 consumer 是用户自己的 daily-driver claude(用户手动装) | M0.11 | tech-design §3.8、§6.7、interfaces §11 |

**M1 砍掉 / 推后的任务**:
- ~~M1.1 Telegram bot 入口~~ → **下沉 M2** Channel Layer(优先复用 Claude Code
  官方 TG channel / 开源 bot 框架,不在 ccteam 主代码库重写)
- ~~M1.9 多轮 CLARIFY 协议~~ → **推 M2**;M1 用 "tmux attach 直接 NL 对话"覆盖
  CLARIFY 场景

**M1 不做**:Seed phase 完整(M2)、score、跨项目记忆、agent 投票/共识(M4)、
ccteam-mcp MCP server(M2)、具体 channel adapter 实现(M2+ 复用开源)。

**M1 风险**:

| 风险 | 触发 | 应对 |
|---|---|---|
| meta session 是新概念,M3 团队抽象未上线时怎么落地 | M1 早于 M3,无 `team.yaml` 体系 | M1.0 把 meta-agent 当作 hardcoded special team(orchestrator 内一个 enum 分支),M3 时再泛化进 `team.yaml` |
| meta session context 涨爆 | 用户跟 meta 聊几周,1M 上限内 reset 60% 阈值仍要触发 | 沿用项目 session 的 context reset 机制;M4.6 落 conversation continuity 之前,M1 简易版用 `~/projects/<user>-meta/.ccteam/CLAUDE.md` 滚动追加 |
| meta session 错把项目级请求当问答处理 | NL 解析有概率性 | meta-agent role prompt 显式写"任何项目级动作派单前 ESCALATE 一次确认";风险换可控 |
| M1.0 写 role prompt 时把行为约束漏一条 | strategic §7.2.3 列了三条,实施时漏一条 | M1.0 验收清单显式对照 strategic §7.2.2/§7.2.3 |

---

## 4. M2 — Seed Gate + Score(2 周)

**唯一验收**:提"AI 菜谱生成器" → Seed 直接 REJECT,附"已有 N 个免费同类工具";一个测试全绿但实现糙的项目得分低于阈值,自动进 fix-cycle。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M2.1 | Seed phase 模板与 verdict 解析 | YAML front matter 输出 `verdict: PASS/REJECT/CLARIFY`;orchestrator 据此分流 | M1.5 |
| M2.2 | CLARIFY 单问单答 | 强制 prompt 约束"只问一个问题";走 control/answer 回路 | M2.1、M1.3 |
| M2.3 | Score phase + 6 维加权 | 输出 `scorecard.md` 含 Functionality/Quality/Tests/UX/Speed/Docs + bug penalty | M1.5 |
| M2.4 | golden-rules.py 集成 | 抄 ccteam-creator 5 项 + 项目特定补充;phase `after` hook 调用,失败阻断 ship | M2.3 |
| M2.5 | Critic 与 dev 进程隔离(M2 简化版) | Score 阶段单独起子进程读 implement 产物,**禁止** dev 自评 | M2.3 |
| M2.6 | sub-skill 自动调度 | phase front matter `sub_skills` 被 orchestrator 自动 trigger;两档 trigger(`phase_start` / `phase_done`);产物按 `output_to` 落文件,自动作为下 phase prompt 的 `@文件引用`;复用 `claude-plugins-official:pr-review-toolkit/agents/code-reviewer` 验证一次端到端 | M2.5 | §6.10、§3.3 |
| M2.7 | `parallelism: agent_team` 启用 | implement phase 模板设 `parallelism: agent_team`;orchestrator 启用 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`;phase prompt 注入 backend-dev / frontend-dev / reviewer 三角色;Lead 协调下产物落 `.ccteam/<role>-output/` | M2.5 | §3.3、§6.3、§6.11 |
| M2.8 | `ccteam-mcp` MCP server | binary 出 `ccteam mcp-serve` 子命令(stdio 协议);暴露 `ccteam__ls` / `__show` / `__new` / `__peek` / `__progress` / `__pause` / `__resume` 七个 tool;`ccteam doctor --install-mcp` 写 `~/.claude.json`;装后用户自带 claude 通过 MCP 调度 ccteam 比 Bash 工具更鲁棒;ccteam-control skill body 改为推荐"优先 MCP,fallback Bash" | M1.8 | §3.8、§6.4、interfaces §12 |

**M2 不做**:RAG 召回(M3)、anti-leniency 严格规则(M4)、phase 内 audit 矩阵 / 投票(M3)、`parallelism: multi_session`(M3)、ccteam-mcp 写权限扩展(暂只暴露上述七个,attach/start/stop/memory rebuild 不暴露,见 interfaces §12.3)。

---

## 5. M3 — Team Abstraction(2 周)

> 见 [docs/ccteam-as-domain-agnostic-orchestrator.md](./ccteam-as-domain-agnostic-orchestrator.md)
> §6 落点论证。本里程碑把 ccteam 从"开发团队的编排层"泛化为"任意 AI 团队的编排层",
> 是 M4 跨项目记忆 / M5 Critic 不写死 dev 假设的前提。

**唯一验收**:`ccteam new --team=research "<topic>"` 能跑通 happy path,产出最终研究
报告;dev 团队的现有项目零迁移成本(`ccteam new "<brief>"` 默认 `--team=dev` 仍然
工作)。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M3.1 | §B 审计的 P0 项目全部修复 | `cargo test` 全绿;dev pipeline happy path 不变;`pub use ... M0_PHASE_DAG, FIRST_PHASE` lib API breaking change 完成(`docs/dev-coupling-audit.md` F1–F4 + F12 + F13 + **F20 升级 P0**) | M0.5 |
| M3.2 | `team.yaml` schema + 解析 | `ccteam doctor --team dev` 列出 dev 团队当前的 7 件契约都从配置读;phase YAML `tools_required` / `auto_loop` / `completion_signal` / `stall_warn_minutes`(F21)从 phase 模板读,不再依赖常量 | M3.1 |
| M3.3 | `ccteam new --team <name>` / `start --team <name>` | `--team` 缺省值 `dev`(向后兼容);非 dev team 启动需 team.yaml 存在;`state.json.team` 字段持久化 | M3.2 |
| M3.4 | `phases-research/` 起草纳入仓库 | 9 个 phase 模板按 `phases-research/` 目录结构入仓(已由 strategic doc §C 起草);research 团队 phase DAG 通过 `validate_team` 校验;ESCALATE 至少含 1 个团队特有前缀(如 `HYPOTHESIS_REJECTED`) | M3.2 |
| M3.5 | research 团队端到端 happy path | 起一个真实 research 项目跑到 ship,产出 `.ccteam/report.md`;progress.jsonl 完整事件序列含 phase 切换 + auto_loop 触发 + 至少一条 ESCALATE-resume 回路 | M3.1–M3.4 |
| M3.6 | meta-agent skill 集占位(M2.8 之后顺手) | `ccteam-control` skill body 里加一节"team selection",说明 dispatch 时 `--team` 怎么选;`ccteam-dispatch` skill 起草作为 backlog 不落地 | M3.3、M2.8 | strategic §7 |

**M3 不做**:
- `phases-marketing/` / `phases-ops/` 起草(M3.5 验证抽象后,M5 之后任意时机做,不阻塞 critical path)
- 跨项目记忆 namespace 化(留给 M4,因为 M3 还没记忆)
- meta-agent conversation continuity(留给 M4 RAG 落地一并设计)
- Critic 维度泛化(留给 M5,但 M3 起草 team.yaml schema 时必须为 critic_dimensions 留好数据形式,不允许 enum 写死)

**M3 风险**:

| 风险 | 触发 | 应对 |
|---|---|---|
| §B 审计 P0 项过多,M3.1 单条堵住整个里程碑 | 审计发现深层耦合(例:fix-loop 状态机假设 dev 流程) | M3.1 拆为多个子 PR,每条 P0 独立 PR;按 §B 优先级排序逐个清 |
| dev 团队的 `team-dev.yaml` 反推时和现状不一致 | 写 team-dev.yaml 时发现某些行为靠"巧合"工作,没显式契约 | 反推时逐条对照 strategic doc §1 责任分界表;"没契约的现状"必须先写到 §1 再纳入配置 |
| research 团队跑通靠的是借用 dev plugin 的能力,而不是真验证了契约 | research phase 模板偷懒,ESCALATE 不用自定义前缀,critic 不用自定义维度 | M3.4 验收时强制要求 research 至少有 1 个自定义 ESCALATE 前缀 + 至少 1 个 dev 没有的 critic 维度 |
| 显式拒绝清单(strategic doc §3)被 PR 软性绕过 | "为了通用"在 ccteam-core 加 `if team == "research"` | code-review 加规则:`ccteam-core/` 内出现 team 名字符串字面量 = 自动拒收 |

---

## 6. M4 — 跨项目记忆(3 周)

**唯一验收**:第二次提相似项目 → Seed 阶段 prompt 里出现"上次做过 X,建议复用 Y";
research 项目召回不污染 dev 项目的 RAG 索引(team namespace 已落地)。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M4.1 | retro phase 自动产出 pattern.md(team-aware schema) | 项目终态(shipped/rejected/escalated)触发;字段从 `team.yaml.retro_schema[]` 读(F20 解决);dev 输出 tech stack/坑/成功设计/不要再做,research 输出方法学/数据源/假设结果 | M3.2 |
| M4.2 | 向量索引(sqlite + sentence-transformers) | `~/.ccteam/memory/index.json` + 向量 db;**team namespace 隔离**(`team=dev` 召回不混入 `team=research` 模式);`ccteam memory rebuild` 全量重建 | M4.1 |
| M4.3 | Seed phase 接 RAG 召回 | top-3 patterns 注入 Seed prompt;命中相似失败项目时 verdict 倾向 REJECT/CLARIFY;**只召回同 team 的 patterns**,anti-patterns 跨 team 共享 | M4.2、M2.1 |
| M4.4 | anti-patterns 库 | REJECT 案例独立 namespace;召回时显式标注"不建议";跨 team 共享 | M4.1 |
| M4.5 | claude-mem MCP 接入(可选) | 若稳定则替换自建索引;不稳就跳过 | M4.2 |
| M4.6 | meta-agent conversation continuity 落地 | `claude-mem` MCP 加 user namespace,或 `~/.ccteam/meta/conversation-log.md` 滚动追加;`ccteam-control` skill 启动时主动读取(strategic doc §7.2.1) | M4.2 | strategic §7.2.1 |
| M4.7 | phase 内 audit 矩阵(L2 升级) | `architect` / `critic` / `designer` / `security` / `scope-watcher` 按 phase 启用清单跑;复用 `claude-plugins-official` 现成 agent | M2.5 | tech-design §3.6 L2 |
| M4.8 | agent 投票与共识机制 | M4.7 audit 输出 PASS/CONCERN/BLOCK;按 `yolo`/`balanced`/`careful` 信任档位决定是否上推 L3;分裂时弹用户 | M4.7、M1.7 | tech-design §3.6 L2/L3 |
| M4.9 | 新插件自动挂载(扩展性) | 扫 `~/.claude/plugins/.../skill_intent.yaml`;按推荐 phase 自动加进 phase 模板 `sub_skills` | M2.6 | tech-design §6.10 |
| M4.10 | `parallelism: multi_session`(大项目加速) | 实现 fan-out / fan-in 协议:plan-eng 输出子模块清单 + interface-contracts.md;orchestrator 起 N 个 sub-session;子模块独立跑 phase;review/ship 在 master fan-in;`max_sessions_per_project=4` 兜底;一次端到端验证(SaaS demo:backend ∥ frontend ∥ docs) | M2.7、M1.5 | tech-design §6.11 |
| M4.11 | ratatui TUI 前端(机会主义,**非关键路径**) | `ccteam tui` 可跑;数据源走 `ccteam-core` lib API(M2.8 `mcp-serve` 同源 schema),不另起进程;不引入新 LLM 层(前端层 invariant) | M2.8 | tech-design §3.8 前端层 |

**M4 不做**:跨项目接口契约管理(M6)、自动子模块切分(M6)、跨子模块 stop-the-world 重构(M6)。

**M4 风险**:11 条任务对 3 周窗口偏紧(尤其 M4.7/4.8/4.10 都是新机制)。落地时按以下
顺序削:M4.5(claude-mem MCP,可选)→ M4.4(anti-patterns,可推 M5)→ M4.9(新插件
自动挂载,可推 M5)。M4.10 不可削——它是痛点 13 的最终落地。

---

## 7. M5 — Critic Agent 闭环(3 周)

**唯一验收**:测试全绿但 critic 发现"接口不优雅" → 自动进 fix-cycle 而非直接 ship。
research 项目的 critic 用 `strict` 严格度成功拦下 LLM 主观打高分的 false-pass。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M5.1 | Critic agent 独立子进程 | 复用 `pr-review-toolkit/agents/code-reviewer.md`;不与 implement 共享 session;`team.yaml.critic_dimensions[]` 数据驱动加载(strategic §A §2.3 invariant 1) | M3.2 |
| M5.2 | Anti-leniency 规则(per-dim strictness) | `lenient` / `normal` / `strict` 三档,从 `team.yaml.critic_dimensions[].anti_leniency_strictness` 读;research 核心维度用 `strict`(strategic §A §2.3 invariant 2) | M5.1 |
| M5.3 | WEAK 维度强制 BLOCK | 任一维度 ≤ `weak_threshold`(从 config 读,不是常量;invariant 3) 立即进 fix-cycle,绕过"勉强通过" | M5.2 |
| M5.4 | 评审维度 per-project 自适应 | 前端项目偏重 UX、CLI 偏重 Docs;由 plan-eng 阶段提示;调权重不是改维度集 | M5.1 |
| M5.5 | Critic 与 dev 切换的 cache 优化 | dev session 跨 phase cache 复用,Critic 单独热;算总成本回报 | M5.1 |
| M5.6 | web dashboard(机会主义,**非关键路径**) | `ccteam serve --port` 可跑,浏览器开 xterm.js 终端可远程介入项目级 claude(发送 keystroke 直通 tmux,触发 user_attach 自动暂停 phase——前端层 invariant:不在 ccteam 层起新 LLM,用户键入 = `send-keys` 注入 tmux);直接抄 `references/agent-of-empires/` 的 axum ws + xterm.js bridge 范式 | M2.8 | tech-design §3.8 前端层 invariant |

**M5 不做**:多个 Critic 并行 / 投票(M6)。

---

## 8. M6 — 长期对标 Symphony(3–6 月,开放探索)

**目标**:能搭大型软件——多模块、多服务、长跑数日。

不展开为短任务,仅列待解问题:

- 任务自动分解:把"做一个交易系统"拆成 N 个独立子项目,自动建 DAG
- DAG 调度(blocks / produces / consumes,而非平铺队列)
- Milestone 概念(周级别检查点),与子项目级 phase 区分开
- 跨子项目接口契约管理(谁定 schema、谁验证)
- 长 token 预算精细化(按子项目独立预算,跨子项目转账)
- A2A bridge——本地 ccteam 与云端 ccteam 协作
- 外部 tracker adapter(Linear / GitHub Projects),作为可选
- 多用户共享单 ccteam 实例(权限 / 隔离 / 公平调度)

**M6 进入条件**:M0–M5 全部经过至少 5 个真实小项目验证,确认现有协议(progress.jsonl / state.json / phase 模板 / team.yaml)能扛住非平凡场景。

---

## 9. 跨里程碑依赖(关键路径)

```
M0.6 (orchestrator 主循环)
  ├─→ M0.7 (tmux 管理) ─→ M0.8 (idle-aware) ─→ M0.9 (状态机) ─→ M0.10 (reset) ─→ M0.15
  │                                                         └─→ M0.12 (fix-loop)
  └─→ M0.13 / M0.14 (stall + cost)

M0.15 ─→ M0.5.1 (ln -sf agents) ─→ M0.5.3 (tools_required 校验) ─→ M0.5.7 (端到端回归)
        ↓                          ↓
        M0.5.2 (skills 占位)      M0.5.5 / M0.5.6 (doctor)
        ↓
M0.5.7 ─→ M1.1 (telegram) ─→ M1.2 (多项目) ─→ M1.5 (重启恢复) ─┬─→ M1.6 (cross-cutting watcher) ─→ M1.7 (L3 fork 决策)
                                                                └─→ M2.1 (Seed) ─→ M2.5 (Critic 隔离) ─┬─→ M2.6 (sub-skill 调度)
                                                                                                       └─→ M2.7 (agent_team 启用)
                                                                                                         ↓
                                                                                          M3.1 (P0 audit 修复 — 含 F20/F21)
                                                                                                         ↓
                                                                                          M3.2 (team.yaml schema) ─→ M3.3 (--team CLI)
                                                                                                         ↓
                                                                                          M3.4 (phases-research) ─→ M3.5 (research E2E)
                                                                                                         ↓
                                                                  ┌──────────────────────┴──────────────────────┐
                                                                  ↓                                              ↓
                                                         M4.1 (team-aware retro)                       M5.1 (Critic data-driven)
                                                         M4.2 (RAG, namespace)                         M5.2 (anti-leniency strictness)
                                                         M4.3 (Seed RAG)                               M5.3 (WEAK BLOCK config)
                                                         M4.6 (meta-agent continuity)                  M5.4 (per-project 自适应)
                                                         M4.7/4.8 (audit 矩阵 + 投票)                  M5.5 (cache opt)
                                                         M4.10 (multi_session)
```

**关键变化(2026-05-05 reorder)**:
- M3 = Team Abstraction(原 M4.5 提案)插入到记忆 / Critic 之前 —— retro_schema 与
  critic_dimensions 从 day 1 就团队感知,避免 M4 / M5 完工后再被迫推倒重来
- 旧 M3.1–M3.10 → 新 M4.1–M4.11(顺移,且 M4.1 retro 改为 team-aware schema)
- 旧 M4.1–M4.6 → 新 M5.1–M5.6(顺移,且 M5.1/5.2/5.3 改为 config-driven)
- 旧 M5(Symphony)→ 新 M6

不在关键路径上的(可与主线并行):M1.4(项目 CLAUDE.md)、M1.8(ccteam-control skill)、
M1.9(多轮 CLARIFY)、M2.4(golden-rules)、M2.8(ccteam-mcp MCP server)、M3.6(meta-agent
skill 集占位)、M4.4(anti-patterns)、M4.5(claude-mem MCP)、M4.9(新插件挂载)、
M4.11(ratatui TUI,机会主义)、M5.4(评审自适应)、M5.6(web dashboard,机会主义)。

**关键路径终点**(因 M4.10):M2.7 → M4.10 成为新关键路径终点之一(痛点 13 最终落地);
M2.7 卡住整条 multi-agent 速度并行链。

---

## 10. 进度风险登记

| 风险 | 概率 | 影响 | 应对 |
|---|---|---|---|
| Claude Code 协议变更(hook 字段、CLI flag) | 中 | M0 整体阻塞 | 锁定测试过的 `claude --version`;CI 新版本 smoke test |
| ralph-loop Stop hook 范式与 ccteam 自有 Stop hook 互相冲突 | 中 | M0.12 卡住 | 不挂两个 hook,把 ralph 逻辑合到 parse-phase-end.sh(见 §2.3) |
| context reset 后行为不一致(新 session 把 CLAUDE.md 桥接信息当独立任务做) | 中 | M0.10 不可用 | M0.10 必须包含至少 3 个真实项目的 reset 验证用例 |
| Telegram bot 平台变更或封号 | 低 | M1 入口断 | 双通道 fallback:邮件 + 文件 inbox(已在 §3.8) |
| 向量索引方案选错(自建 vs claude-mem) | 中 | M3 阻塞 | M3.2 与 M3.5 并行 spike 1 周,择优 |
| 估算偏差累积 | 高 | 整体延期 | 每个里程碑结束做 retro,把实际工时回填本文 |

---

## 11. 计划维护纪律

1. **每个 PR 描述必须含**:
   - 对应任务编号(`Closes M0.X`)
   - 痛点编号(`requirements.md 痛点 N`)
   - tech-design 章节(`tech-design §X.Y`)
2. **本文档优先于 tech-design.md §7**——§7 已退化为指针,任何里程碑变化只改本文。
3. **任务粒度**:M0 详到子任务级(因为活跃);M1–M5 详到任务级;M6 仅高层方向。M0 完成后,把 M1 推进到子任务级。
4. **里程碑推进准则**:声明该里程碑解决的痛点必须能在 §1 反向映射表里被一个真实场景验证,**不**靠 checkbox 凑数。
5. **新工作流入**:不能映射到现有任务的需求 → backlog,不进主线(对应 tech-design §11)。
6. **里程碑 reorder**:本文档 reorder 必须同步 update `docs/ccteam-as-domain-agnostic-orchestrator.md` §6 里程碑 label / `docs/dev-coupling-audit.md` 里出现的里程碑引用 / `docs/interfaces.md` 里出现的里程碑引用,否则跨文档不一致。2026-05-05 M3 ↔ Team Abstraction 互换是首次执行此规则。
