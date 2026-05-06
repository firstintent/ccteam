# ccteam 开发计划

> 本文从 [tech-design.md](./tech-design.md) §7 拆出并扩展。
>
> - `requirements.md` 回答 **为什么做**(10 条痛点)
> - `tech-design.md` 回答 **怎么做**(架构、协议、扩展点)
> - **本文回答 何时做什么、做到什么标准、谁阻塞谁**——是单一权威的进度文档,PR 必须能映射回本文某条任务。

---

## 0. 总体节奏

| 里程碑 | 状态 | 时长 | 主目标 | 关键解锁的痛点 |
|---|---|---|---|---|
| **M0** | ✅ shipped | 2–3 周 | 单项目 CLI MVP——一句话需求 → 自动跑出能用的代码 | 1, 2, 3, 4, 7, 8, 9 |
| **M0.5** | ✅ shipped | 1 周 | 工具触发面闭环——让 Claude Code 全套能力在自治模式下真正可调 | 11(地基)、12(地基) |
| **M1** | ✅ shipped | 2 周 | meta-agent + 多项目并发 | 5, 9(强化) |
| **M2** | ✅ shipped | 1.5 周 | dev pipeline 工程机制(sub-skill / phase YAML / ccteam-mcp / golden_rules schema) | 6(部分;Seed 提到 M3.4)、3(部分;golden_rules) |
| **M2.3-followup** | ✅ shipped | — | F22 slug team prefix(`~/projects/<team>-<slug>/`,let `~/.claude/rules/` paths frontmatter scope 命中) | — |
| **M3** | ✅ shipped | 3 周 | Team Abstraction + product-research team——`ccteam new --team=product-research` 跑通,dev 路径零回归 | 团队泛化地基、6(verdict / REJECT) |
| **M4.1–M4.4** | ✅ shipped | — | 跨项目记忆(走官方 auto-memory + `~/.claude/rules/` + 可选 claude-mem;ccteam-core 零检索代码) | 10 |
| **M4.5–M4.9** | ❌ planned | — | audit 矩阵 / 投票 / multi_session / 新插件挂载 / TUI | 11(深化)、12(深化)、13 |
| **M5** | ❌ planned | 3 周 | Critic Agent 闭环——超越"测试通过=完成";critic_dimensions 团队感知 | 3(深化) |
| **M6** | ❌ open | 3–6 月 | 大型软件长跑能力(对标 Symphony) | 长期 |

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
| 10. 每项目从零开始 | — | — | — | — | — | retro 写官方 auto-memory + `~/.claude/rules/ccteam-lessons-<team>.md` 跨项目共享(检索全在 Claude session 内,ccteam 零代码) | — |
| 11. 关键节点不把控 | L1 架构约束(hooks + required_outputs) | **L1 扩展:tools_required + 启动期可达性校验** | cross-cutting watcher 上线 | 单 critic + dev 隔离;L3 telegram fork 决策 | — | phase 内 audit 矩阵 + 投票共识 | anti-leniency + WEAK BLOCK |
| 12. 工作流编排 | phase 主干 + `sub_skills` 字段定义(空允许) | **plugin agent 注册 + skill 懒注入**(让 sub_skills 真有得调) | — | sub-skill 自动 trigger + 产物接力 | 团队抽象解锁多团队 sub_skills 共享 | 新插件按 `skill_intent.yaml` 自动挂载 | — |
| 13. 并行规模 | phase 模板 `parallelism: solo` 字段(只此一档) | — | — | `parallelism: agent_team` 启用 | — | `parallelism: multi_session` 启用 | 自动并行规模识别 |
| **新:团队泛化地基** | — | — | — | — | `ccteam new --team=research` 跑通,§B 审计 P0 全清 + team.yaml schema 落地 | retro_schema 团队感知(F20)、critic_dimensions 数据驱动(§A §2.3 invariant 1) | critic_dimensions per-dim anti_leniency_strictness(§A §2.3 invariant 2) |

**门槛规则**:某个里程碑若未真正解决其声明的痛点,**不许跳到下一个里程碑**——这是质量门,不是日历推进。

---

## 2. M0 — 单项目 CLI MVP(✅ shipped)

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
- 跨项目记忆(M4;走官方 auto-memory + `~/.claude/rules/`,不自建索引)
- Score / Critic / 6 维评分(M5)
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

## 2.5. M0.5 — 工具触发面闭环(✅ shipped)

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

## 3. M1 — meta-agent + 多项目并发(✅ shipped)

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
| M1.0 ✅ | **meta-agent session 骨架(新)** | `ccteam doctor --install-meta-agent <user-handle>` 命令落地;创建 `~/projects/<user>-meta/` 目录,写 meta-agent role prompt 到 `.ccteam/CLAUDE.md`(含 dispatch 决策树 + dispatcher-not-worker 行为约束,见 strategic doc §7.2.2 / §7.2.3);ln -sf `ccteam-control` skill;orchestrator 把 meta session 当一种特殊"team type"管(常驻、永不 terminal、事件循环);`ccteam start` 自启 meta session;`tmux attach -t ccteam-meta-<user>` 即 NL 对话入口 | M0.5 | tech-design §2.1 / §3.8 / strategic doc §7 |
| M1.1 ✅ | **inbox/outbox 文件协议加固(改)** | `<session>/.ccteam/inbox/msg-<n>.md`(NL markdown + 顶部 YAML 元数据:source channel / timestamp / user)`<session>/.ccteam/outbox/reply-<n>.md`(同 schema);orchestrator inotify watch inbox,触发 send-keys 注入对应 session(idle 直送 / 忙加 `/btw`);interfaces.md 加协议章节。**M1 不实现具体 channel**,只钉协议。**M1 落地为 30s 轮询,inotify 升级留 P2**(WSL2 兼容 + 简洁) | M1.0 | tech-design §2.1.2 / interfaces.md |
| M1.2 ✅ | 多项目并发调度 | `max_concurrent_projects=3` 准入;超出排队;每项目独立 tmux session;meta session 不计入并发上限(它常驻) | M1.0 | tech-design §6.1 |
| M1.3 ✅ | meta-agent dispatch 端到端 | meta session NL 指令 `"做一个 todo cli"` → meta 用 `ccteam-control` 调 `ccteam new` → 项目 session 启动 → meta 在 tmux pane 看到"项目已派,跟踪中"反馈;5 个连续派单测试串行准入 | M1.0、M1.2、M1.8 | strategic §7.2.2 |
| M1.4 ✅ | 项目级 CLAUDE.md 自动生成 | plan phase 后写;reset 时追加"当前进度"节(已在 M0.10 设计,M1 兑现到 meta session 也享受) | M0.10 | tech-design §6.5 / §6.9 |
| M1.5 ✅ | 优雅停机 + 重启自恢复 | `ccteam stop` 不杀 session(包含 meta session);`ccteam start` 自动 reattach 所有活跃 session(meta + 项目) | M1.2 | tech-design §6.1 |
| M1.6 | cross-cutting watcher agents(L2 起步) | `cost-watcher` + `scope-watcher` 实现;**Stop hook 触发**(每 phase 边界跑一次,不在 PostToolUse 跑——避免 300+ 次/phase 灌爆);输出 PASS/CONCERN/BLOCK,append `progress.jsonl`;BLOCK 写 `escalation.md` | M1.5 | tech-design §3.6 L2、§6.3 模式 B |
| M1.7 | **L3 fork 决策走 NL 通道(改)** | watcher BLOCK / fix-loop escalate 时,orchestrator 写 `escalation.md`;meta-agent watcher 检测到 → 在 meta session 用 NL 描述项目卡点 + 备选方案;用户 NL 回复(在 meta session pane 或将来 channel),meta 解析后用 `ccteam-control` 把决策注入对应项目 session 的 inbox。**砍掉旧的 ABC structured push** | M1.6、M1.0 | tech-design §3.6 L3 |
| M1.8 ✅ | `ccteam-control` skill 发行 | binary 内嵌 SKILL.md;`ccteam doctor --install-skill` 写到 `~/.claude/skills/ccteam-control/`;字段约定 + body 必含章节按 interfaces §11 落地;**首要 consumer 是 meta-agent session**(M1.0 自动装);辅助 consumer 是用户自己的 daily-driver claude(用户手动装) | M0.11 | tech-design §3.8、§6.7、interfaces §11 |

> **2026-05-06 M1 Phase 1 完成**(commit `m1/meta-agent-dispatch`):
> M1.0/1.1/1.2/1.3/1.4/1.5/1.8 一并 ship。M1.6 / M1.7 留作后续 PR——
> Phase 1 已让 meta-agent NL 派单端到端能跑(终端 attach 即对话),
> watcher 与 L3 NL 通道是单独的功能集,与 dispatch 主链解耦。

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
| meta session context 涨爆 | 用户跟 meta 聊几周,1M 上限内 reset 60% 阈值仍要触发 | 沿用项目 session 的 context reset 机制;M4 conversation continuity 落 `~/.claude/rules/ccteam-lessons-<user>-meta.md` 滚动累积 + auto-memory(已 ship);M1 简易版用 `~/projects/<user>-meta/.ccteam/CLAUDE.md` 滚动追加 |
| meta session 错把项目级请求当问答处理 | NL 解析有概率性 | meta-agent role prompt 显式写"任何项目级动作派单前 ESCALATE 一次确认";风险换可控 |
| M1.0 写 role prompt 时把行为约束漏一条 | strategic §7.2.3 列了三条,实施时漏一条 | M1.0 验收清单显式对照 strategic §7.2.2/§7.2.3 |

---

## 4. M2 — dev pipeline 工程机制(✅ shipped;M2.2 enablement permanently deferred — spike A)

> **2026-05-06 重构**:原 M2 把"Seed Gate(idea 是否值得做)"与"Score(构建质量)"
> 都塞进 dev team,违反 strategic doc §3.1「不替领域定 done criteria」+ §3.4「不
> 预设质量评分维度」。讨论后确认:
>
> - **Seed/Reject 流程提取为 product-research team**(M3.4 落地;dev team 不再
>   自带否定流程)
> - **Score 整体删除**:fix-loop(M0)+ phase YAML `golden_rules`(本里程碑)
>   覆盖硬质量;软质量交给 M5 Critic 独立 team
> - **CLARIFY 协议复用 M1.1 inbox/outbox**(无独立任务)
> - **加 phase YAML 三字段**(`decision_mode` / `max_clarify_rounds` / `golden_rules`)+
>   phase template `@文件引用` 机制(为 product-research team 与 review-with-user 模式服务)
>
> 净效果:8 → 5 任务,2 周 → 1.5 周,M2 聚焦 dev 工程机制(sub-skill 调度 /
> agent_team / MCP server / phase 协议扩展)。

**唯一验收**:dev 团队跑 `ccteam new "<完整 brief>"` 直通 ship —— sub-skill
auto-trigger 跑通 ≥1 个 plugin agent(如 `code-reviewer`);agent_team mode
在 implement phase 跑通 backend-dev / frontend-dev / reviewer 三角色;
meta-agent 通过 ccteam-mcp 全部 9 个 tool 调度;phase template `@` 引用基础模板
生效。**全程不出现 Seed REJECT 案例**(那个挪到 M3.5 product-research E2E)。

| # | 任务 | 验收 | 依赖 | 来源 |
|---|---|---|---|---|
| M2.1 ✅ | sub-skill 自动调度 + link agents 扩展 | phase YAML `sub_skills` 被 orchestrator 自动 trigger(两档:`phase_start` / `phase_done`);产物按 `output_to` 落文件,自动作为下 phase prompt 的 `@文件引用`;`link_recommended_agents` 扩展接 phase YAML driven list,session 启动前扫 `sub_skills` + `tools_required` 全 ln -sf;复用 plugin 的 `code-reviewer` 端到端验证 | M1.5 | tech-design §6.10、§3.3 |
| M2.2 ⏸️ | agent_team 兼容性 spike + 启用 | **M2.2.0 spike(0.5 天)**:hello-world 多 agent 协作验证 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 在当前 Claude Code 版本仍有效,失败立即 escalate;通过后 implement phase 模板设 `parallelism: agent_team`,prompt 注入 backend-dev / frontend-dev / reviewer 三角色;Lead 协调下产物落 `.ccteam/<role>-output/`;**role 集合 dev 写死,M3 时改 team.yaml 配置**(任务描述显式标注)。**M2.2.0 spike 已 ship**(`docs/m2-agent-team-spike.md`):claude 2.1.128 下 env var 仍由父进程注入但 `claude --help` 不再暴露 first-class CLI 路径,**spike 判 inconclusive but suspicious,推荐备选 A「保留 schema 放弃启用」**;schema 部分(`validate_m0` 放宽 + `agent_team[]` 非空校验)在 M2.3 已落,启用步骤推迟 | M2.1 | tech-design §3.3、§6.3、§6.11 |
| M2.3 ✅ | phase YAML 字段批扩展(三字段)| interfaces.md §5.1 schema 加:① `decision_mode: sync\|async\|hybrid`(默认 hybrid;sync 用 AskUserQuestion 阻塞,async 写 outbox 不阻塞,hybrid 先试 AskUserQuestion 1-2 分钟超时降级 outbox);② `max_clarify_rounds: 3`(超限 phase 强制 best-effort 产出 + ESCALATE `INSUFFICIENT_CLARIFICATION`);③ `golden_rules: [{rule_id, pattern\|cmd}]`(orchestrator 不内置规则,只跑 enforcement —— **plugin 化,不再 ccteam-creator 5 项写死**)。**注**:M2.3 落 schema + `validate_m0` 强制 cmd\|pattern 二选一;`golden_rules` 执行端(后处理 spawn)未 ship,等用户 ack §4.3 路径决策(c)后单独 PR | M1.5 | strategic §3.1 / §3.4 / §3.5 |
| M2.4 ✅ | phase template `@文件引用` 机制 + `~/.ccteam/templates/` | 落两个基础模板:`review-with-user-loop.md`(读上游 → 多轮 challenge → 落 review.md)+ `kickoff-reverse-interview.md`(spec 太薄时反向面试用户综合 brief);phase markdown 用 `@~/.ccteam/templates/<name>.md` 拼装(Claude Code 原生 `@` 机制,零新 schema);`team.yaml` 可覆盖默认路径(M3 衔接);**dev plan-eng phase 至少一处 `@review-with-user-loop` 验证生效** | M1.5 | best practices §3 / §5.2、tech-design §3.1 |
| M2.5 ✅ | `ccteam-mcp` MCP server(9 tool) | binary 出 `ccteam mcp-serve` 子命令(stdio 协议);暴露 9 tool:`ls / show / new / peek / progress / pause / resume / send_to_session / inject_decision`;`ccteam doctor --install-mcp` 写 `~/.claude.json`;**首要 consumer = meta-agent session,辅助 = daily-driver claude**;`ccteam-control` skill body 改为推荐"优先 MCP,fallback Bash";interfaces.md §12 同步 9 tool schema(`send_to_session` 是 inbox 写入,`inject_decision` 是 ESCALATE 结构化注入) | M1.8 | tech-design §3.8、§6.4、interfaces §12 |

> **2026-05-06 M2 完工**(PR #4 @ `f96f985`):M2.1 / M2.3 / M2.4 / M2.5 全
> ship。**M2.2 启用步骤推迟**(M2.2.0 spike escalate),等用户对 spike 报告
> §「仍需用户决定」三问拍板后单独 PR。M2 唯一验收里 sub-skill / template `@`
> / MCP 三件套已跑通(单元 + 集成测试 289 全绿);agent_team 启用与「dev
> 团队 happy path 真 LLM 端到端」未跑,等用户在另一 ccteam start session 实
> 测确认。

**M2 不做**:
- Seed phase / verdict 解析 → M3.4 product-research team
- Score(任何形式 / 任何维度) → 删
- CLARIFY 协议本身 → M1.1 inbox/outbox 已覆盖,M2 只用不写
- ccteam-mcp 写权限扩展超出 9 tool(attach/start/stop/memory rebuild 不暴露,见 interfaces §12.3)
- `PHASE_DONE_PENDING` 协议扩展 + phase defer 真不阻塞 → M3
- `ccteam decisions` 全局队列 CLI → 已挪入 M1 收尾增量(配合 M1.0 meta-agent;M1 主 PR 完工后单独小增量 PR)
- agent voting / phase 内 audit 矩阵 / `parallelism: multi_session` → M4 / M3 后

---

## 5. M3 — Team Abstraction + product-research team(✅ shipped)

> 见 [docs/ccteam-as-domain-agnostic-orchestrator.md](./ccteam-as-domain-agnostic-orchestrator.md)
> §6 落点论证。本里程碑把 ccteam 从"开发团队的编排层"泛化为"任意 AI 团队的编排层",
> 是 M4 跨项目记忆 / M5 Critic 不写死 dev 假设的前提。
>
> **2026-05-06 reframe**:首个非 dev team 由 academic research(学术研究)
> 改为 **product-research(产品调研:idea 是否值得做)**。理由:
> ① product-research 把 M2 砍掉的 Seed Gate / REJECT 流程接住,落地后用户路径
> 完整(uncertain idea → product-research 验证 → 决定派 dev 还是放弃);
> ② 价值差异更显著(verdict 输出 vs code 输出),更能验证 team 抽象红线;
> ③ 加 `PHASE_DONE_PENDING` 协议扩展(M2 砍掉的 phase defer 真不阻塞落到这里)。
> academic research team 的 9 phase 草稿(strategic §C)推到 M5 之后 backlog,
> 不阻塞 critical path。

**唯一验收**:`ccteam new --team=product-research "AI 菜谱生成器"` 跑通 happy path,
verdict=REJECT,产出 `verdict.md` + `rationale.md`(列已有 N 个免费同类工具)+
`next-steps.md`;dev 团队 `ccteam new "<brief>"` 默认 `--team=dev` 零迁移成本仍然工作。

> **现实对照**(2026-05-06 M2 完工后,起 M3 前):main HEAD `5458de9`,
> `cargo test --workspace` 289 个全绿。下表标 ✅ 是已 ship,🔧 是部分 ship,
> 空白是未起步。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M3.1 ✅ | §B 审计的 P0 项目全部修复 | F1/F2/F3/F4/F12/F13/F20,PR #1 @ `4766800` | M0.5 |
| M3.2 ✅ | `team.yaml` schema 全字段化 | `TeamSpec` 加 5 字段:`critic_dimensions[]`(M5 用,数据形式)/ `escalate_grammar_extensions[]`(团队特有 ESCALATE 前缀)/ `golden_rules[]`(team-wide 默认,phase YAML 优先)/ `phase_dir`(默认 `phases`)/ `verdict_schema`(verdict-emitting phase list)。所有字段 `#[serde(default)]`,legacy team.yaml 零迁移。`validate()` 检查 phase_dir 形态 + retro_schema 唯一性 + escalate_grammar_extensions 唯一性 / target_phase 必填 + golden_rules cmd\|pattern xor + critic_dimensions 唯一 | M3.1 |
| M3.3 ✅ | `ccteam new --team <name>` / orchestrator team-aware 加载 | `run_new` 对非 dev / 非 meta-agent 团队 fail-fast(检查 `~/.ccteam/teams/<team>/team.yaml` 或 embedded `TEAM_BUNDLES`);`bootstrap_project` 按 `team` 选 embedded phase set;`Orchestrator::new` 扫 `~/.ccteam/teams/*/team.yaml` 建 `HashMap<team, TeamRuntime>`,legacy `~/.ccteam/phases/` 注册为 implicit dev;`process_project` 按 state.team 选 DAG;新 ESCALATE 前缀走 `escalate_grammar_extensions` 数据驱动 | M3.2 |
| M3.4 ✅ | **product-research team 模板入仓** | `phases-product-research/` 6 phase(kickoff / market-survey / differentiation-analysis / value-proposition / feasibility / verdict)入仓 + `teams/product-research.yaml` 入仓,`include_str!` 嵌入 binary。所有 phase `parallelism: solo`;3 个 ESCALATE 前缀(`MARKET_DUPLICATE` / `INSUFFICIENT_VALIDATION` / `LOW_DIFFERENTIATION`)注册;`feasibility` 与 `verdict` 用 `decision_mode: async`;`kickoff` 引用 `@~/.ccteam/templates/kickoff-reverse-interview.md`;verdict phase 用 interfaces §5.3 通用 schema;`write_all_global_team_templates` 在 `ccteam init` 写齐 phase + team.yaml | M3.2、M2.4 |
| M3.5 ✅ | product-research E2E happy path(mock) | 8 个 E2E 集成测试(`crates/ccteam-cli/tests/m3_product_research_e2e_test.rs`):bootstrap 写 state.json team=product-research;orchestrator 加载 3 ESCALATE 前缀;6 phase walked via `decide_tick`;`feasibility` PHASE_DONE_PENDING + clarify outbox 不 block verdict;verdict ABORT → terminal;MARKET_DUPLICATE escalate-resume cycle 归档 marker;progress.jsonl 含全部 marker;`ccteam decisions` JSON 列 clarify outbox 文件名 + slug + team。真实 LLM 运行延后到用户实测确认 | M3.1–M3.4 |
| M3.6 ✅ | `PHASE_DONE_PENDING` 协议扩展 | (a) `state.rs` 加 `PhaseState::DonePending { open_decisions }`(drop Copy,struct variant);(b) Stop hook (`parse_phase_end.rs`) 识别 `PHASE_DONE_PENDING` 前缀,从 reason 文本扫 `reply-*.md` / `clarify-*.md` / `escalation-*.md` 文件名,写 `event: phase_done_pending` 含 `phase` / `open_decisions[]` / `reason` 三字段;(c) `decide_tick_from_events` 返回新 `TickAction::AdvancePhasePending { from, to, open_decisions }`;(d) `process_project` 用 `intersect_open_decisions_with_required_inputs(...)` 静态比对 — 无重叠 → 正常 advance,有重叠 → 切 `DonePending` + 写 escalation.md + 等 `ccteam resume`;(e) DonePending 状态下 decide_tick 返回 NoOp(等用户) | M3.5 | tech-design §3.5、interfaces §4.1.1 |
| M3.7 ✅ | meta-agent skill + dispatch 树扩展 | `meta_agent_role.md` §2 决策树第 2 步「团队选择」从单 dev 团队改为 dev / product-research 两选项 + NL 启发表(用户语气 → 团队);§4 派单工具加 product-research 命令样板;§3 边界节区分 dev plan-eng vs product-research verdict 的 clarify 分工。`ccteam_control_skill.md` 加「Team selection (M3+)」工作流 + 派单命令双路径 | M3.3、M2.5 | strategic §7.2.2 |

**M3 不做**:
- academic research team(`phases-research/` 学术调研)→ backlog,M5 后任意时机
- `phases-marketing/` / `phases-ops/`(critical path 之外,M5 后任意时机)
- 跨项目记忆 namespace 化(M4;按 team 分文件 `~/.claude/rules/ccteam-lessons-<team>.md`)
- meta-agent conversation continuity(M4;吃官方 auto-memory + 全局 rules 注入,无独立任务)
- Critic 维度泛化(M5,但 M3.2 必须为 critic_dimensions 留数据形式)
- M2.2 agent_team 启用复活(spike 推荐 A 仍生效;Claude Code 释出 first-class
  Agent Teams CLI 后再重 spike,与 M3 解耦)

**M3 外部依赖**(不算 M3 任务本身,但起 M3 前最好就位):
- **golden_rules executor**(M2.3 follow-up 独立小 PR):schema 已 ship,执行端
  落地后 phase 可在 PHASE_DONE 前跑 enforcement。M3.4 product-research 的
  `verdict` phase 可能用它 enforce 报告必填字段,**不是硬依赖**(phase
  prompt 也能自查);M3.5 happy-path 验证如果想跑 enforcement,需要先 ship

**M3 风险**:

| 风险 | 触发 | 应对 |
|---|---|---|
| product-research phase 模板偷懒 | ESCALATE 不用自定义前缀,phase 用 dev 那套 | M3.4 验收强制要求 3 个自定义 ESCALATE 前缀 + 至少 2 个 phase 用 `decision_mode: async` |
| `PHASE_DONE_PENDING` 协议扩展过深 | 跨 phase 依赖追踪复杂度爆炸 | M3.6 范围严限"phase 内能完成的部分先完成,decision-dependent 部分 defer";不做"跨 phase 子任务依赖图"(那是 M6 Symphony) |
| 显式拒绝清单(strategic doc §3)被 PR 软性绕过 | "为了通用"在 ccteam-core 加 `if team == "product-research"` | code-review 规则:`ccteam-core/` 内出现 team 名字符串字面量 = 自动拒收 |
| dev 团队的 `team-dev.yaml` 反推时和现状不一致 | 写 team-dev.yaml 时发现某些行为靠"巧合"工作,没显式契约 | 反推时逐条对照 strategic doc §1 责任分界表 |

---

## 6. M4 — 跨项目记忆 + 多 agent 升级(2 周;M4.1–M4.4 已 ship,M4.5+ 待启动)

**唯一验收**:第二次提相似项目 → Seed 阶段 prompt 里出现"上次做过 X,建议复用 Y";
research 项目的 lessons 不污染 dev 项目;**ccteam-core 内零 memory 检索代码**(全部
经 Claude session 内置机制完成)。

**2026-05-06 重塑**(决策依据见 `references/research/claude-code-memory-research.md`
末尾「M4 决策依据」节):放弃自建索引/向量库。主路径完全复用 Claude Code 官方
机制(`~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` + per-repo auto-memory),
检索发生在 Claude session 内部(不写程序读文件)。装了 [claude-mem](https://docs.claude-mem.ai/usage/search-tools) 即作可选增强,phase prompt 让 LLM 自看 tool surface 决定调不调,
ccteam 不写集成代码。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M4.1 ✅ | retro phase prompt 改造(team-aware) | **物理实现路径**:改造 `phases/09-ship.md` L20-24 的 inline retro 段(dev)+ `phases-product-research/06-verdict.md` REJECT 分支的 inline retro 段(product-research);**不**新增独立 retro phase 文件。phase prompt 引导 Claude 用 `/memory` 写本项目 auto-memory(`~/.claude/projects/<encoded>/memory/`)+ 用 `Edit` 写跨项目 lessons 到 `~/.claude/rules/ccteam-lessons-<team>.md`(限 marked section);schema 字段从 `team.yaml.retro_schema[]` 读。**前置**:`teams/product-research.yaml.retro_schema` 当前空(注释"M4.1 may revise"),M4.1 PR 必须填值(候选字段:market_signals / differentiation_findings / feasibility_assessment / verdict_rationale);`teams/dev.yaml.retro_schema` 已有 4 字段。**ccteam-core 零代码改动** | M3.2 |
| M4.2 ✅ | `ccteam doctor --install-memory-bridge` | 创建 `~/.claude/rules/ccteam-lessons-{dev,product-research}.md` 占位文件,带 `<!-- ccteam-managed:lessons begin/end -->` 标记 + `paths:` frontmatter scope 到 ccteam 项目目录前缀;幂等(重跑 no-op 或重写 marked section,不重复 append) | M0.5(doctor 框架) |
| M4.3 ✅ | Seed/verdict phase prompt 改造 | 提示分三层:① rules 已经自动注入(零成本)② 需深挖本项目历史 → `/memory` 浏览 + `Read` 读 topic 文件 ③ 如检测到 `mcp__*claude-mem*search` 工具 → 跨项目 search(LLM 自判,ccteam 不预先决定);命中相似失败项目时 verdict 倾向 REJECT/CLARIFY | M4.1、M4.2、M2.1 |
| M4.4 ✅ | spike + 容器 bind-mount 验证(0.5 天) | 实测:① `~/.claude/rules/*.md` 在 ccteam-managed `--dangerously-skip-permissions` + 容器项目里仍自动加载;② `paths:` frontmatter 用 `~/projects/<team>-*` 通配 scope 生效;③ retro 写 marked section 幂等;若容器屏蔽 `~/.claude/`,M4.2 doctor 加 bind-mount 文档/脚本。spike 报告见 `docs/m4-spike-2026-05-06.md`;F22 follow-up PR(`~/projects/<team>-<slug>/` 前缀)已 ship 让 paths frontmatter 在 phase claude session 启动时正确匹配 | M4.2 |
| M4.5 | phase 内 audit 矩阵(L2 升级) | `architect` / `critic` / `designer` / `security` / `scope-watcher` 按 phase 启用清单跑;复用 `claude-plugins-official` 现成 agent | M2.5 | tech-design §3.6 L2 |
| M4.6 | agent 投票与共识机制 | M4.5 audit 输出 PASS/CONCERN/BLOCK;按 `yolo`/`balanced`/`careful` 信任档位决定是否上推 L3;分裂时弹用户 | M4.5、M1.7 | tech-design §3.6 L2/L3 |
| M4.7 | 新插件自动挂载(扩展性) | 扫 `~/.claude/plugins/.../skill_intent.yaml`;按推荐 phase 自动加进 phase 模板 `sub_skills` | M2.6 | tech-design §6.10 |
| M4.8 | `parallelism: multi_session`(大项目加速) | 实现 fan-out / fan-in 协议:plan-eng 输出子模块清单 + interface-contracts.md;orchestrator 起 N 个 sub-session;子模块独立跑 phase;review/ship 在 master fan-in;`max_sessions_per_project=4` 兜底;一次端到端验证(SaaS demo:backend ∥ frontend ∥ docs) | M2.7、M1.5 | tech-design §6.11 |
| M4.9 | ratatui TUI 前端(机会主义,**非关键路径**) | `ccteam tui` 可跑;数据源走 `ccteam-core` lib API(M2.8 `mcp-serve` 同源 schema),不另起进程;不引入新 LLM 层(前端层 invariant) | M2.8 | tech-design §3.8 前端层 |

**M4 不做**:
- 自建向量索引 / sqlite + sentence-transformers / `~/.ccteam/memory/index.json`(被官方机制取代)
- ccteam-core 内 memory 检索代码(零代码红线,见 CLAUDE.md §六)
- claude-mem 集成代码(read-only API + 自带 hook 自动捕获,LLM 自调即可)
- 跨项目接口契约管理(M6)、自动子模块切分(M6)、跨子模块 stop-the-world 重构(M6)

**M4 风险**:9 条任务对 2 周窗口仍紧(M4.5/4.6/4.8 是新机制)。落地时按以下顺序削:
M4.7(新插件自动挂载,可推 M5)→ M4.9(TUI,机会主义)。M4.8 不可削——痛点 13 最终落地。
**记忆部分(M4.1–M4.4)体量约 3 天 + 0.5 天 spike**,主要是 phase prompt + 一个 install 函数。

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
M0.5.7 ─→ M1.0 (meta-agent 骨架) ─┬─→ M1.1 (inbox/outbox 协议加固)
                                  │
                                  ├─→ M1.2 (多项目并发) ─→ M1.5 (重启恢复) ─┬─→ M1.6 (watcher) ─→ M1.7 (L3 NL 通道)
                                  │                                          │
                                  │                                          └─→ M2.1 (sub-skill) ─→ M2.2 (agent_team) ─┐
                                  │                                              M2.3 (phase YAML 三字段)              │
                                  │                                              M2.4 (template @ 引用机制)            │
                                  └─→ M1.4 (项目 CLAUDE.md 兑现到 meta)                                                  │
                                                                                                                          ↓
M0.11 ─→ M1.8 (ccteam-control skill) ─→ M2.5 (ccteam-mcp 9 tool)                                              M3.4 (product-research team)
                                                                                                                          ↓
              M1.3 (meta dispatch E2E) ← 汇合 {M1.0, M1.1, M1.8}                                              M3.5 (product-research E2E)
              (M1 acceptance gate,不阻塞 M2)                                                                            ↓
                                                                                                                          ├──────────────────────┐
                                                                                                                          ↓                      ↓
                                                                                                                M4.1 (team-aware retro)   M5.1 (Critic data-driven)
                                                                                                                M4.2 (memory-bridge)      M5.2 (anti-leniency)
                                                                                                                M4.3 (verdict prompt)     M5.3 (WEAK BLOCK config)
                                                                                                                M4.4 (container spike)    M5.4 (per-project 自适应)
                                                                                                                M4.5/4.6 (audit + 投票)   M5.5 (cache opt)
                                                                                                                M4.8 (multi_session)
```

**关键变化(按时间倒序)**:

- **2026-05-06 post-M4 ship state**(main `ae094bf`):M0–M4.4 全 ship + F22 fix(slug team prefix
  `~/projects/<team>-<slug>/`,PR #12)+ E2E P0 fixes(F1+F2/F6/F8,PR #8)。M4.5–M4.9 / M5 / M6
  仍 planned;M2.2 agent_team enablement 永久 deferred(spike A:Claude Code 不再暴露 first-class
  Agent Teams CLI,schema 保留,启用步骤待官方释出后再 spike)。当前 critical path 终点:
  M4.5(audit 矩阵)/ M4.8(multi_session)。

- **2026-05-06 M2 简化**(架构红线 strategic doc §3.1 / §3.4 落实):
  - **Seed Gate(REJECT/CLARIFY)从 dev team 提取为 product-research team**(M3.4
    落地);dev team 不再自带"否定 idea"流程 —— 价值判断本属产品/市场 domain,
    不是 dev 工程职责
  - **Score 整体删除**:fix-loop(M0)+ phase YAML `golden_rules`(M2.3)覆盖
    硬质量;软质量交给 M5 Critic 独立 team
  - **CLARIFY 协议复用 M1.1 inbox/outbox**(无独立任务)
  - **三个 phase YAML 字段**(`decision_mode` / `max_clarify_rounds` /
    `golden_rules`)+ phase template `@文件引用` 机制 加进 M2,为 product-research
    team 与 review-with-user 模式服务
  - **academic research team 退到 backlog**;product-research 替代为 M3 首个
    非 dev team(M3.4 / M3.5)
  - 净效果:M2 任务 8 → 5,2 周 → 1.5 周

- **2026-05-06 M1 reframe**(三层架构落定后):
  - 旧 M1.1(Telegram bot 入口)下沉到 M2+ Channel Layer,优先复用开源方案,**不在 ccteam 主代码库**
  - 新 M1.0(meta-agent session 骨架)成为 M1 的根节点,所有 M1 任务都从此分叉
  - 新 M1.1(inbox/outbox 协议加固)是 channel layer 接入面契约,与 M1.2/M1.3 并联
  - M1.3(meta dispatch E2E)汇合 M1.0 + M1.1 + M1.8,作为 **M1 acceptance gate**,但**不阻塞 M2 启动**
  - M1.7 改"L3 fork ABC structured push" → "L3 fork 走 NL 通道"(经 meta-agent NL 解析,见 M1 §3.7)
  - 旧 M1.9(多轮 CLARIFY)→ 推 M2,M1 用 "tmux attach 直接 NL 对话"覆盖

- **2026-05-06 M4 简化**(决策依据 `references/research/claude-code-memory-research.md` + 官方
  https://code.claude.com/docs/en/memory):放弃自建向量索引/sqlite,主路径完全复用
  Claude Code 官方记忆机制(`~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` + per-repo
  auto-memory),检索发生在 Claude session 内部,**ccteam-core 零 memory 检索代码**。
  claude-mem 降级为可选增强(read-only API + 自带 hook 自动捕获,LLM 自看 tool surface
  决定调不调,ccteam 不写集成代码)。任务从 11 砍到 9(记忆 6 → 4,审查/扩展性 5 不动);
  3 周 → 2 周;记忆部分约 3 天 + 0.5 天 spike。旧 M4.2/4.3/4.4/4.5/4.6 → 新 M4.2/4.3
  + M4.4 spike;旧 M4.7–4.11 → 新 M4.5–4.9。

- **2026-05-05 milestone reorder**:
  - M3 = Team Abstraction(原 M4.5 提案)插入到记忆 / Critic 之前 —— retro_schema 与
    critic_dimensions 从 day 1 就团队感知,避免 M4 / M5 完工后再被迫推倒重来
  - 旧 M3.1–M3.10 → 新 M4.1–M4.11(顺移,且 M4.1 retro 改为 team-aware schema)
  - 旧 M4.1–M4.6 → 新 M5.1–M5.6(顺移,且 M5.1/5.2/5.3 改为 config-driven)
  - 旧 M5(Symphony)→ 新 M6

**不在关键路径上的(可与主线并行)**:M1.3(M1 acceptance gate,不阻塞 M2)、
M1.4(项目 CLAUDE.md)、M1.8(ccteam-control skill,M1.3 依赖但与 M1.0/1.1/1.2 主链并行)、
M2.3(phase YAML 三字段,与 sub-skill / agent_team 主链解耦)、
M2.4(template @ 引用机制,与 sub-skill / agent_team 主链解耦)、
M2.5(ccteam-mcp,依赖 M1.8 但与 M2.1/M2.2 解耦)、
M3.7(meta-agent skill + dispatch 树扩展,可与 M3.4/3.5/3.6 并行)、
M4.7(新插件挂载)、M4.9(ratatui TUI,机会主义)、
M5.4(评审自适应)、M5.6(web dashboard,机会主义)。

**关键路径终点**(因 M4.8):M2.2(原 M2.7)→ M4.8 成为新关键路径终点之一
(痛点 13 最终落地);M2.2 卡住整条 multi-agent 速度并行链。

**M1 内部依赖说明**(图中无法完整呈现的边):

- M1.3 = `merge(M1.0, M1.1, M1.8)` —— meta-agent 骨架 + 协议 + 控制 skill 三齐才能跑端到端 NL 派单
- M1.5 仅依赖 M1.2(重启恢复是多项目调度的扩展能力)
- M1.6 仅依赖 M1.5(watcher 跑在已经能 reattach 的 session 上)
- M1.7 依赖 M1.6 + M1.0(L3 NL 通道既要 watcher 报警,也要 meta-agent 解析 NL 回复)
- **关键路径(M1 → M2)**:M1.0 → M1.2 → M1.5 → M2.1。M1.1 / M1.3 / M1.6 / M1.7 / M1.8 都是 M1 内部完成度要求,不卡 M2 启动

---

## 10. 进度风险登记

| 风险 | 状态 | 概率 | 影响 | 应对 |
|---|---|---|---|---|
| Claude Code 协议变更(hook 字段、CLI flag) | open | 中 | M0 整体阻塞 | 锁定测试过的 `claude --version`;CI 新版本 smoke test |
| ralph-loop Stop hook 范式与 ccteam 自有 Stop hook 互相冲突 | ✅ closed(M0.12) | 中 | M0.12 卡住 | 不挂两个 hook,把 ralph 逻辑合到 parse-phase-end.sh(见 §2.3) |
| context reset 后行为不一致(新 session 把 CLAUDE.md 桥接信息当独立任务做) | ✅ closed(M0.10) | 中 | M0.10 不可用 | M0.10 必须包含至少 3 个真实项目的 reset 验证用例 |
| Channel Layer 适配器(M2+)外部依赖变更或封号 | open | 低 | 远程入口断,但终端 attach + 文件 inbox 不受影响 | 三层架构已隔离:M1 不依赖任何 channel,channel adapter 是 M2+ 可插拔件,挂一个补一个 |
| 容器化项目屏蔽 `~/.claude/rules/` 加载 | ✅ closed(M4.4 spike + F22) | 中 | M4.1–M4.3 跨项目 lessons 不可见 | M4.4 spike 已跑;F22 让 `~/projects/<team>-<slug>/` 前缀与 paths frontmatter 通配匹配生效 |
| `~/.claude/rules/` 被 LLM 写坏 | open | 低 | 用户级文件污染,影响其他项目 | retro phase prompt 严格限制只能写 marked section;一次性 setup 后 doctor `--verify-memory-bridge` 校验完整性 |
| 估算偏差累积 | open | 高 | 整体延期 | 每个里程碑结束做 retro,把实际工时回填本文 |

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
