# oh-my-claudecode vs ccteam:编排架构对比

> **调研对象**:`/home/rob/workplace/agents/oh-my-claudecode/`(OMC,`oh-my-claude-sisyphus@4.13.7`)
> **对比对象**:本仓 `ccteam`(V0.4.0)
> **调研时间**:2026-05-15
> **结论一句话**:OMC 把"编排逻辑"放在 **prompt(SKILL.md)** 里、TS 代码只做基础设施;ccteam 把"编排逻辑"放在 **Rust binary** 里、Claude 只做 worker。两条路线各有取舍,**不是"谁更好"的问题,是"控制权放在哪"的问题**。

---

## 一、OMC 项目骨架(只看与编排相关的部分)

```
oh-my-claudecode/
├── skills/team/SKILL.md         # 1040 行 — 编排"剧本"(prompt-as-code)
├── agents/                      # 19 个 *.md,每个 ~80–280 行 — 单个 agent 的 persona prompt
│   ├── executor.md  planner.md  architect.md  critic.md  verifier.md ...
├── src/                         # ~142 kLOC TS(含测试)— 基础设施
│   ├── team/                    # ~20 kLOC,核心 runtime
│   │   ├── runtime-v2.ts        # 2230 行 — 事件驱动 runtime(默认)
│   │   ├── runtime.ts           # 1034 行 — legacy v1 runtime
│   │   ├── runtime-cli.ts       #  629 行 — detached Node 子进程入口
│   │   ├── tmux-session.ts      # 1086 行 — tmux pane 管控(CLI workers 用)
│   │   ├── monitor.ts           #  535 行 — 快照 / 心跳
│   │   ├── role-router.ts       #  264 行 — 任务 → 角色 路由
│   │   ├── stage-router.ts      #  227 行 — provider/model 解析快照
│   │   ├── sentinel-gate.ts     #  177 行 — premature-completion 阻断
│   │   ├── git-worktree.ts      # worker 隔离用 git worktree
│   │   ├── allocation-policy.ts # 任务分配
│   │   ├── inbox-outbox.ts / mcp-comm.ts / events.ts ...
│   ├── cli/team.ts              # 1415 行 — `omc team start|status|wait|cleanup|api` 命令
│   ├── mcp/team-server.ts       #  655 行 — MCP server,4 个工具,都是 CLI 命令的薄包装
│   └── hooks/  installer/  ...  # 安装、hook、HUD 等周边
└── bridge/                      # 打包出的 CJS 入口,npm install -g 用
```

**版本号有意思**:`v4.x` 已经是 native team mode(用 Claude Code 内置 `TeamCreate`/`Task(team_name,name)`/`SendMessage`/`TaskCreate` 工具),`v3` 之前是 SQLite swarm。SKILL.md §"Comparison: Team vs Legacy Swarm" 详细解释了切换原因 —— 不再需要 `better-sqlite3` 原生扩展,改用 Claude Code 内置原语 + `~/.claude/teams/<name>/config.json` + `~/.claude/tasks/<name>/<id>.json` 文件存储。

---

## 二、`skills/team/SKILL.md` 真的"编排"全部 19 个 agent 吗?

**简短回答:能,但不是直接控制流执行 —— 而是 LLM 按 SKILL.md 的指令在对话里调度。**

### 2.1 编排谁?编排了什么?

SKILL.md 是一份 **给 lead Claude 看的剧本**。当用户输入 `/oh-my-claudecode:team 3:executor "fix all TS errors"`,Claude Code 的 skill loader 把整个 SKILL.md 注入当前会话的 system context;**当前会话变成 team lead**,按 SKILL.md 写的 7 个阶段(Parse → Decompose → TeamCreate → TaskCreate → Spawn → Monitor → Completion)依次调用工具:

| SKILL.md 指令 | 实际工具调用 | 谁在执行决策 |
|---|---|---|
| Phase 2: Analyze & Decompose | `Task(subagent_type=explore/architect)` | **lead Claude 自己想**,explore/architect 只是子任务 |
| Phase 3: Create Team | `TeamCreate(team_name=...)` | Claude Code 内置工具 |
| Phase 4: Create Tasks + 依赖 | `TaskCreate` × N + `TaskUpdate(blockedBy)` | Claude Code 内置工具 |
| Phase 5: Spawn Teammates | `Task(subagent_type='oh-my-claudecode:executor', team_name, name)` × N | Claude Code 内置工具,**并行 spawn** |
| Phase 6: Monitor | `TaskList` 轮询 + `SendMessage` 入站 / 出站(自动投递) | lead Claude 在对话循环里 |
| Phase 6.5: Stage transition | `state_write(mode="team", current_phase=...)` | OMC MCP `state_*` 工具 |
| Phase 7: Completion | `shutdown_request` × N → 等 `shutdown_response` → `TeamDelete` | Claude Code 内置工具 |

### 2.2 agent 路由 —— stage-aware,不是 user-aware

SKILL.md §"Stage Agent Routing" 列了一张 5×N 矩阵:

| Stage | Required | Optional |
|---|---|---|
| team-plan | `explore`(haiku), `planner`(opus) | `analyst`, `architect` |
| team-prd | `analyst`(opus) | `critic` |
| team-exec | `executor`(sonnet) | `executor`(opus), `debugger`, `designer`, `writer`, `test-engineer` |
| team-verify | `verifier`(sonnet) | `test-engineer`, `security-reviewer`, `code-reviewer`(opus) |
| team-fix | `executor` | `debugger`, `executor`(opus) |

用户传 `N:agent-type` 只影响 **team-exec 阶段的 worker 类型**;其它 stage 的 agent 由 lead Claude 按"任务特征 + 风险等级 + 成本模式"动态挑选。"动态挑选"=lead 自己读 SKILL.md 的 selection criteria,自己判断。

### 2.3 19 个 agent 中实际被 SKILL.md routing 表点名的:13 个

被点到的(`team-plan/prd/exec/verify/fix` stage):`explore, planner, analyst, architect, critic, executor, debugger, designer, writer, test-engineer, verifier, security-reviewer, code-reviewer` —— 13 个。

未在 routing 表里出现的 6 个:`code-simplifier, document-specialist, git-master, qa-tester, scientist, tracer` —— 但 SKILL.md `agent-type` 参数验证逻辑接受"任何已知 OMC subagent"(`Phase 1: Parse Input`),用户可以显式 `/team 3:tracer "..."` 把这些 agent 接入 team-exec 阶段。所以 **19 个 agent 都可以被 SKILL.md 编排,但其中 6 个需要用户显式声明**,不会被自动 routing 选中。

### 2.4 编排的本质:LLM-driven control flow

关键区别 —— 阶段转换、错误处理、任务重分配、卡 worker 检测、shutdown 协议 …… 这些"控制平面决策"**全部由 lead Claude 在 conversation turn 里 LLM-推理完成**。SKILL.md §"Phase 6: Monitor / Task Watchdog Policy" 写的是:

> - Max in-progress age: If a task stays `in_progress` for more than 5 minutes without messages, send a status check
> - Suspected dead worker: No messages + stuck task for 10+ minutes → reassign task to another worker

这些规则**没有一个 TS 函数实现**(verify 过 `src/team/runtime.ts` / `runtime-v2.ts` —— 它们提供原语 like `monitorTeam(teamName, cwd)` 返回快照,但 reassign / status-check / 升级到 architect 的决策 **必须由 lead Claude 在 conversation 里读快照然后做判断**)。

---

## 三、`src/` 是核心编排吗?—— **不是,是基础设施**

**短答:src/ 142 kLOC 提供 substrate(状态文件、tmux pane、worktree、CLI worker 桥接、role routing 解析、merge orchestrator、sentinel gate);决策仍在 SKILL.md / lead Claude 手里。**

### 3.1 src/team/ 的实际职责

| 文件 | 干啥 | 是否"编排决策"? |
|---|---|---|
| `runtime-v2.ts` (2230 LOC) | exports `startTeamV2 / monitorTeamV2 / shutdownTeamV2 / resumeTeamV2` — 创建 tmux session、写 `config.json`、轮询 worker 状态返回 snapshot | ❌ 算子(operations),不是控制循环;由 `runtime-cli.ts` detached 子进程在后台跑 |
| `runtime-cli.ts` (629 LOC) | `omc team start` spawn 出去的 detached Node 进程入口,**只服务 CLI workers(codex/gemini tmux pane)** 的 lifecycle —— Claude teammates 走 Claude Code native tools 不经过这里 | 一半算编排:管 CLI worker 的死活、sentinel gate、auto-merge |
| `tmux-session.ts` (1086 LOC) | 包装 `tmux split-window / send-keys / kill-session`,只为 CLI worker | ❌ 纯基础设施 |
| `role-router.ts` (264) + `stage-router.ts` (227) | 解析 `.claude/omc.jsonc::team.roleRouting` → 决定每个 role 用什么 provider/model;**在 TeamCreate 时一次性 snapshot** 进 `TeamConfig.resolved_routing` | 一半算编排:配置驱动的路由表,但不做 runtime 决策 |
| `sentinel-gate.ts` (177) | 阻止"所有 task 还没 in_progress 就 mark complete" 这种 LLM 短路行为 | ✅ 算硬约束(determinism guard) |
| `git-worktree.ts` | 给每个 worker 起独立 worktree,branch = `omc-team/<team>/<worker>` | ❌ 基础设施 |
| `events.ts` | append-only 事件日志 `.omc/state/team/<team>/events.jsonl` | ❌ 观测,不参与决策 |
| `monitor.ts` (535) | 读 worker heartbeat + task files,返回 `TeamSnapshot` | ❌ 只读 |
| `allocation-policy.ts` | 给定 N 个 task 和 M 个 worker,计算如何分配 | ✅ 算法,被 lead 调用 |
| `cli-worker-contract.ts` / `mcp-comm.ts` / `inbox-outbox.ts` | CLI worker 输出契约、跨进程消息 | ❌ 协议 |

### 3.2 `src/mcp/team-server.ts`(655 LOC,**MCP 入口只暴露 4 个工具**)

```
omc_run_team_start    →  spawn detached runtime-cli, 返回 jobId
omc_run_team_status   →  读 jobs/<id>/state.json
omc_run_team_wait     →  阻塞直到 job 完成
omc_run_team_cleanup  →  kill + rm 状态
```

**这 4 个工具都是 CLI 命令(`omc team start/status/wait/cleanup`)的薄包装**。Claude teammates 用的 `TeamCreate / TaskCreate / SendMessage / Task(team_name, name)` 等"team 工具"**不是 OMC 实现的,是 Claude Code 内置 native tools**(`v4` 系列重写的核心动机)。

### 3.3 一张图说明三层关系

```
┌──────────────────────────────────────────────────────────────────────┐
│ LEAD CLAUDE SESSION (用户当前 chat)                                  │
│   ↓ 读 skills/team/SKILL.md 当 system prompt                         │
│   ↓ 按 SKILL.md 7 个 phase 调用工具                                  │
│   ↓ 编排决策:任务分解、agent 挑选、阶段转换、重分配、shutdown        │
└──────────────────────────────────────────────────────────────────────┘
              ↓ Tool calls                       ↓ Tool calls
┌────────────────────────────────┐   ┌─────────────────────────────────┐
│ Claude Code native team tools  │   │ OMC MCP / CLI / runtime-cli     │
│ TeamCreate / TaskCreate /      │   │ - omc team start (spawn 子进程) │
│ Task(team_name, name) /        │   │ - state_read/write              │
│ SendMessage / TaskList /       │   │ - CLI worker(tmux+codex)       │
│ TaskUpdate / TeamDelete        │   │ - sentinel_gate, watchdog       │
│                                │   │ - git worktree                  │
│  → ~/.claude/teams/<name>/     │   │  → .omc/state/team/<name>/      │
│  → ~/.claude/tasks/<name>/     │   │  → .omc/handoffs/<stage>.md     │
└────────────────────────────────┘   └─────────────────────────────────┘
              ↓                                   ↓
┌──────────────────────────────────────────────────────────────────────┐
│ WORKER LAYER                                                         │
│ - Claude teammates:Claude Code 子会话(每个 worker 一个会话)        │
│ - CLI workers:tmux pane 跑 `codex` / `gemini` CLI                   │
└──────────────────────────────────────────────────────────────────────┘
```

**结论:src/ 提供算子和基础设施,SKILL.md 提供控制流。两者拼起来是完整 orchestrator;只看 src/ 看不到"编排"的完整故事。**

---

## 四、对比 ccteam 的 Rust 编排

### 4.1 ccteam V0.4.0 的编排模型

```
┌──────────────────────────────────────────────────────────────────────┐
│ ccteam Rust orchestrator(binary 名 `ccteam`)                       │
│   - workflow.yaml 读 agent 拓扑 + trigger + parallelism             │
│   - inotify ArtifactWatcher 监听 `<project>/artifacts/` 目录        │
│   - 根据 artifact event 决定 spawn 哪个 agent                       │
│   - 写 progress.jsonl(7 类业务 event)= 唯一状态 SoT                │
│   - 控制循环 100% 在 Rust,没有 LLM 在 control plane                 │
└──────────────────────────────────────────────────────────────────────┘
                        ↓ spawn / signal
┌──────────────────────────────────────────────────────────────────────┐
│ Workers                                                              │
│ - `claude --bg --agent <role>` 起 background Claude job              │
│ - `~/.claude/jobs/<job_id>/state.json` 是 worker 状态                │
│ - Codex via tmux+codex(F62 待标准化)                                │
└──────────────────────────────────────────────────────────────────────┘
                        ↑
┌──────────────────────────────────────────────────────────────────────┐
│ User 接口层(只有"对话操作",没有"决策权")                          │
│ - Meta-agent(常驻 Claude session)+ ccteam-control skill            │
│ - ccteam MCP server,17 个 `mcp__ccteam__*` 工具                     │
│ - Web UI(ccteam web)+ SSE live updates                             │
└──────────────────────────────────────────────────────────────────────┘
```

**架构红线**(`CLAUDE.md §三`):
- 文件系统(`progress.jsonl` + `artifacts/`)是控制平面唯一 SoT
- orchestrator 不解析 tmux 终端输出
- meta-agent 不是 orchestrator,是给用户用的"操盘手"
- fix-loop 撞 3 次顶必 escalate,**绝不静默重置**

### 4.2 OMC vs ccteam 维度对比

| 维度 | OMC(SKILL.md + src/team) | ccteam(Rust binary) |
|---|---|---|
| 编排决策位置 | Lead Claude 的 LLM 推理(读 SKILL.md) | Rust 代码(`workflow.yaml` 驱动) |
| 控制循环 token 成本 | 高:每次 phase 转换 / monitor 都消耗 lead session token | 零:Rust loop 不调 LLM |
| 状态 SoT | `~/.claude/tasks/<name>/<id>.json`(per-task)+ `.omc/state/team/<name>/events.jsonl` + `.omc/handoffs/<stage>.md` + lead conversation context | `~/.ccteam/progress/<slug>.jsonl`(append-only,7 类业务 event) |
| 状态可观测性 | 多文件分散;lead context 是隐性状态(compact 后丢) | 单文件 append-only;orchestrator 无内存状态 |
| 阶段定义 | SKILL.md 写死 5 stage(plan/prd/exec/verify/fix)| `workflow.yaml` 用户声明,无 phase 概念,只有 agent 节点 + trigger |
| Agent 行为定义 | `agents/<role>.md` markdown persona prompt | `.claude/agents/<role>.md`(Claude Code 标准格式) |
| 实施成本 | ~3.5 kLOC SKILL.md + agents + ~20 kLOC TS substrate(测试另算);新加 agent = 写一个 md | ~15 kLOC Rust + ~12 kLOC TS web;新加节点 = 改 yaml + 写 agent md |
| 灵活度 | 高:lead 可即时改 plan、跳 stage、reassign;edge case 容忍度高 | 低:workflow.yaml schema 严格,边界外行为需要改 Rust |
| 决定论 | 弱:依赖 LLM 读懂 SKILL.md;同一输入两次跑可能走不同 stage 顺序 | 强:同一 workflow.yaml + 同一 artifact 序列 = 同一 spawn 序列 |
| 测试 | 难:测 SKILL.md 等于测 LLM 行为,只能跑 E2E + behavior snapshots(`src/team/__tests__/` 有 ~120 个测试文件) | 易:Rust unit + integration test,~770 tests cargo --workspace |
| Fix-loop 边界 | SKILL.md 写 `max_fix_loops: 3` 默认,**lead 自觉守护**;突破靠 prompt 自律 | Rust 代码 `budget.fix_loop_attempts` 强约束,撞顶必 escalate event |
| Premature-completion 防护 | `sentinel-gate.ts`(177 LOC)在 TS 侧守 + SKILL.md "Stop Conditions"提醒 lead | progress.jsonl + workflow.yaml 完成条件由 orchestrator 判,LLM 不直接控 |
| 用户交互延迟 | 低:lead 自己想自己做 | 中:meta-agent ↔ MCP ↔ Rust orchestrator,多一跳 |
| 跨项目支持 | 同 lead 同时只能跑 1 个 team(skill 注入 system) | 多项目并行,每个项目独立 progress.jsonl,meta-agent 切换上下文 |
| 长时间运行 | 受 lead session context 长度限制(SKILL.md §"Stage Handoff Convention" 要写 `.omc/handoffs/<stage>.md` 缓解 compact 风险) | 无限:Rust orchestrator 不存内存状态,随时重启从 progress.jsonl 恢复 |
| Hybrid worker(codex/gemini) | ✅ 一等公民,SKILL.md §"CLI Workers" + `cli-worker-contract.ts` | 🚧 V0.4.1 候选,目前主推 Claude;codex 通过 tmux 接 |

### 4.3 哪个更好?—— 看你优化什么

**OMC 模式胜出的场景**:
- 任务边界模糊、需要 lead 灵活判断的"探索型工作"(refactor / design / multi-stage 分析)
- 多 provider 混搭(Claude + Codex + Gemini hybrid)是核心需求 —— OMC 在这块已经把 SKILL.md / role-router / model-contract / 三方 CLI worker 串通
- 团队不想维护 Rust,只想写 prompt + TypeScript
- 用户**不需要**长时间运行(单次 team-plan→exec→verify 跑完即止)
- 想要在用户对话里直接看到 lead 的"思考过程"(可读、可干预)

**ccteam 模式胜出的场景**:
- 需要 **长时间、持续、跨多个 Claude session 重启都不丢状态** 的 orchestrator(progress.jsonl append-only,Rust 无状态服务重启就恢复)
- 关心 **token 成本** —— ccteam 控制循环 0 LLM 调用,OMC 每次 monitor tick 都烧 lead 的 token
- 关心 **可测试性 / 决定论 / 形式化 stop 条件**(撞顶必 escalate、budget cap 物理执行)
- 多项目并行,meta-agent 切换上下文
- 文件系统作控制平面、artifact-driven workflow(workflow.yaml 是"事件驱动",OMC SKILL.md 是"阶段驱动")

**不是非此即彼**。OMC 的 `src/team/sentinel-gate.ts` + `events.ts` + `runtime-cli.ts` 其实就是在往 ccteam 的方向慢慢迁:把"硬约束"逐步从 SKILL.md 抽到 TS。如果继续抽,最后会变成"thin SKILL.md + 厚 TS runtime",**那就是用 TS 实现 ccteam 模型**。

反过来,ccteam V0.4.0 也在借鉴 OMC 的优势 —— `.claude/agents/<role>.md` per-role prompt + 17 个 MCP 工具让 meta-agent 用自然语言操盘,**ccteam 的 meta-agent 层很像 OMC 的 lead Claude**(都是 LLM 驱动用户对话),只是 ccteam 把"决策权"留在 Rust、meta-agent 只做翻译;OMC 把"决策权"和"翻译"都给了 lead Claude。

---

## 五、`src/` 核心是不是编排?—— 给个明确判断

**不是,但也不是无关。**

精确表述:
1. **src/team/runtime-v2.ts + runtime-cli.ts + runtime.ts(~4 kLOC)是"半个编排"** —— 它们管 CLI worker 的 lifecycle(spawn tmux pane / 死活检测 / sentinel gate / 自动 merge),但只对 codex/gemini workers 生效;Claude workers 走 Claude Code 内置 team tools 不经过这层。
2. **src/team/role-router.ts + stage-router.ts + allocation-policy.ts(~700 LOC)是"决策辅助"** —— 提供路由 / 分配算子被 lead 调用,本身不主动决策。
3. **src/team/tmux-session.ts + git-worktree.ts + monitor.ts + events.ts + state-paths.ts(~3 kLOC)是"基础设施"** —— 完全不参与决策。
4. **src/cli/team.ts + src/mcp/team-server.ts(~2 kLOC)是"API 入口"** —— 把上面的算子暴露成命令 / MCP 工具,**MCP 暴露的只有 4 个工具,都是 CLI 薄包装**。
5. **src/agents/(~5 kLOC TS)是"agent 元信息"** —— 不是编排,是声明每个 agent 的 model / level / prompt template 怎么加载。
6. **src/hooks/(~3.1 MB,最大子目录)是"事件钩子"** —— `keyword-detector`、`auto-slash-command`、`agent-usage-reminder`、`code-simplifier` 等,负责"在对话流里注入提示"驱动 lead 调用 skill,**核心仍是引导 lead Claude,不是替代 lead**。

**真正的 orchestrator 核心是 skills/team/SKILL.md 的 1040 行 prompt**。`src/` 是这份 prompt 能跑起来所需的所有 substrate —— 没有 src/,SKILL.md 调不动 CLI worker、没有事件日志、没有跨进程 IPC;但**没有 SKILL.md,src/ 提供的算子只是一堆孤立的函数,谁来 monitorTeam、什么时候 shutdown、要不要重分配,无人决策**。

---

## 六、对 ccteam 的启发(可选 takeaway)

(只列调研中浮现的、跟 V0.4.1 候选有关的点,不要求采纳)

1. **OMC 的 `Stage Handoff Convention`(`.omc/handoffs/<stage>.md`)很务实** —— 解决"lead context compact 后阶段决策上下文丢失"的问题。ccteam 的 meta-agent 也有同类痛点(切项目时上下文断),可以借鉴一份轻量 handoff 文档机制(类似 ccteam 的 `~/.claude/rules/ccteam-lessons-<team>.md`,但 per-workflow-run 而非 per-team)。

2. **OMC `cli-worker-contract.ts` 的"verdict 解析"模式** —— CLI worker 输出 contract 化 JSON,lead 解析回 task status。ccteam V0.4.1 codex executor 标准化(F62)时可以参考:不要让 orchestrator 解析 codex 终端输出,而是约定 codex 写一个固定路径的 verdict 文件,orchestrator 只读文件。

3. **OMC `sentinel-gate.ts` 的"premature completion 阻断"是好硬约束** —— ccteam 现在的 fix-loop 撞 3 次升级是同类约束;可以系统化梳理一下还有哪些 LLM 短路行为需要硬阻断(例如 agent 自报 done 但 artifact 未生成、agent_spawn 后 N 分钟无 progress event 等)。

4. **OMC 的 `roleRouting + resolved_routing snapshot` 模式** —— 团队创建时一次性解析 provider/model 路由,之后所有 worker / scale-up / restart 都读同一份 snapshot,保证 stickiness。ccteam V0.4.1 多 provider 混搭(Gemini/GPT-4o)如果做,这个 snapshot 模式可直接抄,**比每次 spawn 重读 yaml 安全**。

5. **OMC 选择 "skill orchestrator + TS substrate" 是为了 hybrid AI 模型** —— 它的核心差异化是 `/team N:codex` / `/team N:gemini` / 混合 team,跨厂商 CLI 通过 tmux pane 桥接。ccteam 如果要走"同样支持多 AI 厂商"的路,**不必抄 OMC 的 prompt-orchestrator 路线**,可以保持 Rust orchestrator 但加 `ExecutorAdapter` trait 抽象 + 每个 provider 一个 adapter 实现(ccteam V0.3.1 harness adapter 已经有方向,见 `docs/research/v0-3-1-harness-adapter-plan.md`)。

---

## 七、要点速回

> **Q1**:`oh-my-claudecode/skills/team/SKILL.md` 能编排 `oh-my-claudecode/agents/` 下全部 agent 吗?
> **A**:能 —— 13 个被 stage routing 自动选用(team-plan/prd/exec/verify/fix),其余 6 个(code-simplifier / document-specialist / git-master / qa-tester / scientist / tracer)需要用户显式 `/team N:<type> "..."` 注入到 team-exec 阶段。

> **Q2**:skill 编排 vs ccteam Rust 编排哪个好?
> **A**:不是"谁好",是"控制权放哪"。SKILL.md = LLM 在 conversation 里推理控制流,灵活、低实施成本、token 贵、不决定论;ccteam Rust = 代码守控制流,确定、可测、低 token、灵活度低。选哪个看你要长时间运行 + 形式化约束(ccteam),还是要快速迭代 + 多 provider hybrid(OMC)。

> **Q3**:OMC 中 `src/` 下的代码核心是编排吗?
> **A**:不是 —— `src/` 是支撑 SKILL.md 跑起来的基础设施(state、tmux、worktree、CLI worker 桥接、role routing 算子、监控、事件日志)。真正的 orchestrator 是 1040 行 SKILL.md;`src/` 没有 SKILL.md 是一盘散沙,SKILL.md 没有 `src/` 跑不通 CLI workers。两者构成"prompt-as-control-plane + TS-as-substrate" 的混合架构。

---

## 八、引用与原始证据(便于复核)

| 论点 | 证据位置 |
|---|---|
| SKILL.md 是 lead 注入 prompt | `oh-my-claudecode/skills/team/SKILL.md:1-7`(frontmatter)+ `level: 4` |
| 编排走 Claude Code native team tools | `SKILL.md:80-91`(`~/.claude/teams/<team>/config.json`)+ `SKILL.md:53-77` |
| 19 个 agent persona prompt | `oh-my-claudecode/agents/*.md` |
| Stage routing 自动选 13 个,user 注入 6 个 | `SKILL.md:101-117`(routing 表)+ `SKILL.md:196-200`(Parse Input) |
| `omc team start` spawn detached node | `oh-my-claudecode/src/cli/team.ts:378-425` |
| MCP server 只 4 个 CLI 包装工具 | `oh-my-claudecode/src/mcp/team-server.ts:1-50`(grep `name:`) |
| `runtime-v2.ts` 是算子库不是 daemon | `oh-my-claudecode/src/team/runtime-v2.ts:886/1695/1921/2186`(exports) |
| sentinel-gate 是硬约束 | `oh-my-claudecode/src/team/sentinel-gate.ts`(177 LOC)|
| Stage handoff `.omc/handoffs/<stage>.md` | `SKILL.md:151-185` |
| ccteam Rust orchestrator 模型 | `CLAUDE.md §一/§三` + `docs/versions/v0-4-0/README.md` |
| ccteam 文件系统是控制平面 | `CLAUDE.md §三` 红线 |
| ccteam 17 MCP 工具 | `CLAUDE.md §一` workspace 描述 |
