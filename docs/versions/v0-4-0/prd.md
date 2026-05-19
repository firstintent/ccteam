# PRD V0.4.0 — Artifact-Driven Workflow 架构（Phase 机制全清、Claude --bg 原生集成）

> 范围：V0.4.0 是一次**架构级重构**，而非 patch round。
> 核心诉求来自用户在 V0.3.x 使用过程中反复碰到的痛点：
> phase 模板机制过于死板、与 Claude Code 内置能力竞争、
> 阻碍真实业务场景落地。同时 Claude Code 2.1.139+ 推出的
> Agent View（`claude --bg --agent`）提供了原生多后台 session
> 管理能力，ccteam 应当拥抱而不是重造。
>
> base = `origin/main` HEAD（V0.3.2 ship 终点）；
> workspace.version 起点 `0.3.2`，V0.4.0 ship 时 bump `0.4.0`；
> 测试 baseline `866/0`（`cargo test --workspace`，pre-existing 9
> clippy errors 不本轮处理）。
>
> 本轮 **F-finding 编号 F60–F69**（10 个 PR）。
> 工作量预计：Rust 核心大幅缩减（删 ~3500 LOC，新增 ~800 LOC），
> 前端适配（ccteam-web Agent View state.json + workflow 视图）。

---

## 0. session 起手 onboarding（30 秒）

```bash
git rev-parse origin/main
cargo test --workspace 2>&1 | grep -E "^test result" \
  | awk '{p+=$4;f+=$6}END{print "passed:",p,"failed:",f}'   # baseline 866/0
```

读完本 PRD §1–§4（背景 + 架构哲学 + 核心抽象 + 三层架构）→
看 §5 被删的内容 → 看 §6 新组件规格 → 看 §10 F-finding 表 →
用 `docs/versions/v0-4-0/dev-plan.md`（待写）对照实施。

**关键约束**：V0.4.0 是破坏性重构，`team.yaml::kind: workflow`
（phase 驱动）在本轮 **EOL**；迁移路径见 §9。

---

## 1. 背景 — 为什么 V0.4.0 是架构重构

### 1.1 用户反馈：phase 模板机制不好用

V0.3.x 的核心编排机制由 `phases.rs`（934 行）驱动：ccteam 定义了
一套预设 phase（`plan-eng`、`implement`、`code-review`……），
orchestrator 按 DAG 顺序注入 prompt、等待 `progress.jsonl` 信号、
切 session。

用户使用后的核心反馈：

> "每个领域的 workflow 完全不同。我的需求根本装不进
> 你预设的那几个 phase。这和 LangChain 的 prompt node 本质一样——
> 是在和 Claude Code 的任务规划能力竞争。"

本质问题：ccteam 在 phase 这一层承担了**本该由 Claude Code 自身
完成的**任务规划与执行协调职责，导致：

1. **prompt 注入层面的竞争**：ccteam 的 `inject_directives` /
   `golden_rules` / `decision_mode` 与 Claude Code 的原生 CLAUDE.md
   + subagent 机制打架，用户的实际 prompt 被稀释
2. **workflow 不可定制**：每增加一种业务 workflow，需要修改
   ccteam 核心代码（`phases.rs`），而不是写一个 YAML 文件
3. **可观测性倒置**：orchestrator 管 prompt，但 Claude Code 自己
   就能管；orchestrator 反而对 "哪个 agent 读了哪个文件" 一无所知

### 1.2 新机遇：Claude Code Agent View（`claude --bg`）

Claude Code 2.1.139+ 新增了 **Agent View**（`claude agents`）：

- `claude --bg --agent <name>` 在后台派发一个 Claude Code session
- session 状态存储于 `~/.claude/jobs/<id>/state.json`
- `claude agents` 提供原生多后台 session 列表 / 监控 UI

这意味着：
- ccteam **不再需要自己管 tmux 作为 Claude Code 的宿主**
  （tmux 仍用于 Codex，但不用于 CC session 的生命周期管理）
- ccteam **不再需要解析 statusline JSON**（状态从 `state.json` 读）
- Claude Code 原生有了多 agent 并发管理能力，ccteam 要做的是
  **在项目层面的 workflow 拓扑 + artifact 流转** 这一层编排，
  而非 session 内的 phase 管理

### 1.3 Token-Maxxing 目标

V0.4.0 的另一条动力是 **token-maxxing**：

> 每个项目能同时跑多个 claude --bg agent，并行处理不同的 issue /
> fix / review；parallelism 参数可动态调节；人只在首尾出现——
> 目标输入 + 最终 review，中间全程 autonomous。

phase 机制是顺序的（DAG 无并发），天然不支持 10 个 fixer 并行跑。
artifact-trigger 模型天然并发：一个 issue 文件 → 一个 fixer
session，parallelism 上限控制总数。

---

## 2. 架构哲学（三原则）

### 原则一：永远不和 Claude Code 内置功能竞争

Claude Code 能做的，ccteam 不重做：

| Claude Code 原生能力 | ccteam 的正确姿势 |
|---|---|
| 任务规划（`plan-eng`、`implement`、`review`） | **不注入** phase prompt；让 agent 自己规划 |
| 多后台 session（Agent View `claude --bg`） | 调用 `claude --bg --agent <role>`；不自建 session manager |
| Agent 角色定义（`.claude/agents/<role>.md`） | **零注入**；角色文件完全由用户 / meta-agent 写 |
| Context 管理（1M window、`/exit` reset） | 保留；orchestrator 不替代 Claude Code context 决策 |
| 工具权限（`.claude/settings.json`） | 保留；orchestrator 不越权 |

### 原则二：文件系统是控制平面

Agent 之间**唯一合法的通信媒介**是文件系统路径：

- 每个 agent 把 output 写到指定目录（`.ccteam/issues/`、
  `.ccteam/fixes/`、`.ccteam/verdicts/`……）
- inotify watcher 检测到新文件 → 触发下游 agent session
- `progress.jsonl` 仍是 **business state 唯一 SoT**
  （task 完成度、cost、escalation 记录、gate 状态）

没有 RPC、没有 message queue、没有 agent 间直接调用。
文件系统 = 异步消息总线，可 replay、可审计、可调试。

### 原则三：人只在首尾出现

```
人 → 目标（自然语言 or workflow.yaml）
         ↓
     全程 autonomous
         ↓
人 ← 结果（artifact 目录 + gate 解锁请求）
```

中间所有的 "要不要派更多 fixer"、"这个 fix 要不要 escalate"、
"review 通过了没" 都由 **meta-agent** 通过 ccteam MCP 工具决策。
人唯一需要出现的地方是 **Gate**（显式检查点），且 Gate 可以
配置为 meta-agent 自动解锁（low-risk 场景）。

---

## 3. 核心抽象（五个概念）

### 3.1 概念一览

| 概念 | 定义 | 对应 V0.3.x |
|---|---|---|
| **Workflow** | agent 拓扑 YAML，只有连线，**无 prompt** | `team.yaml::phases`（含 prompt） |
| **Agent** | `.claude/agents/<role>.md` + executor + trigger | phase template（ccteam-managed） |
| **Artifact** | 文件系统路径（目录/文件），agent 间唯一通信媒介 | 无直接对应（inbox/outbox 是局部实现） |
| **Meta-agent** | workflow 里的常驻协调者，通过 MCP 动态决策 | meta-agent（角色不变，工具扩充） |
| **Gate** | 显式人工或自动检查点，解锁后下游才能启动 | `user_pause_pending` 文件（局部实现） |

### 3.2 关键区分：workflow.yaml 里没有一行 prompt

V0.3.x `team.yaml` 片段（**有 prompt，本轮删除**）：

```yaml
# 旧 team.yaml（V0.3.x，本轮 EOL）
phases:
  - name: plan-eng
    prompt: |
      你是一个软件工程师。你需要先分析需求……
      请输出 plan.md……
```

V0.4.0 `workflow.yaml` 片段（**无 prompt**）：

```yaml
# 新 workflow.yaml（V0.4.0）
name: ui-quality-loop
agents:
  explorer:
    executor: claude
    trigger: schedule
    output: .ccteam/issues/
  fixer:
    executor: claude
    trigger: watch:.ccteam/issues/
    parallelism: 10
    input: .ccteam/issues/
    output: .ccteam/fixes/
```

`explorer` 的行为完全由 `.claude/agents/explorer.md` 定义。
ccteam 只负责：在 trigger 满足时调 `claude --bg --agent explorer`，
把 input/output 目录路径作为环境变量注入。

### 3.3 Artifact 流转示意图

```
.ccteam/issues/ ←── explorer(schedule)
       │
       │ inotify: 新文件
       ▼
    fixer × N ──────────────→ .ccteam/fixes/
    (parallelism: 10)               │
                                    │ inotify: 新文件
                                    ▼
                              reviewer(codex) ──→ .ccteam/verdicts/
                                                        │
                                                    [Gate]
                                                        │ meta-agent 或人解锁
                                                        ▼
                                                  shipper(claude)
```

---

## 4. 三层架构

```
┌─────────────────────────────────────────────────────────────┐
│  Human（只在目标输入 + Gate 解锁 + 最终 review 出现）        │
└──────────────────────────┬──────────────────────────────────┘
                           │ 自然语言 or workflow.yaml
                           ▼
┌─────────────────────────────────────────────────────────────┐
│  Meta-agent（常驻 Claude Code session）                      │
│  · 解析用户目标 → 选择/生成 workflow.yaml                   │
│  · 通过 ccteam MCP 工具动态协调                             │
│    spawn_agent / observe_agents / signal                     │
│    set_parallelism / trigger_gate / get_artifact_summary     │
│  · escalate 决策 / fix-loop 上限 3 次顶                     │
└──────────────────────────┬──────────────────────────────────┘
                           │ MCP 调用
                           ▼
┌─────────────────────────────────────────────────────────────┐
│  ccteam orchestrator（Rust，极薄，目标 ~400 LOC）            │
│  · 解析 workflow.yaml → 构建 artifact-trigger graph         │
│  · inotify watcher → trigger → spawn agent session          │
│  · progress.jsonl append（唯一 business state SoT）         │
│  · Gate 状态管理                                            │
│  · ClaudeCodeAdapter: claude --bg --agent <role>            │
│  · CodexAdapter: tmux new-window + codex（真实实现）        │
└───────────────┬─────────────────────────────────────────────┘
                │
    ┌───────────┴────────────┐
    ▼                        ▼
┌────────────┐         ┌────────────┐
│ CC Sessions│         │Codex Sess  │
│ claude --bg│         │tmux + codex│
│ --agent    │         │            │
│ state.json │         │state.json  │
└────────────┘         └────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────┐
│  Artifact 目录（文件系统控制平面）                           │
│  .ccteam/issues/ .ccteam/fixes/ .ccteam/verdicts/ …        │
│  inotify watcher → signal → trigger 下游 agent             │
└─────────────────────────────────────────────────────────────┘
```

### 每层职责

| 层 | 职责 | 不做 |
|---|---|---|
| **Human** | 输入目标；Gate 解锁（可选）；最终验收 | 中间调度、session 监控 |
| **Meta-agent** | workflow 选择/生成；动态调度决策；escalation | phase prompt 注入；session 生命周期管理 |
| **ccteam orchestrator** | workflow.yaml 解析；trigger 监听；spawn/observe；progress 记录 | prompt 注入；agent 内部规划；Claude Code 功能复现 |
| **Agent sessions** | 业务执行（探索、修复、审查、发布） | 互相直接通信（只通过 artifact 目录） |
| **Artifact 目录** | agent 间异步消息总线；可 replay；可审计 | 不是 orchestrator 状态（那是 progress.jsonl） |

---

## 5. 什么被删除

### 5.1 删除清单

| 模块/文件 | 代码量（估） | 原功能 | 删除原因 |
|---|---|---|---|
| `crates/ccteam-core/src/phases.rs` | ~934 LOC | phase 模板系统（plan-eng / implement / review…） | 和 Claude Code 原生能力竞争；用户无法定制 |
| `orchestrator.rs` phase 状态机 | ~2713 LOC（目标保留 ~400） | phase DAG 执行、prompt 注入、session 切换 | 被 artifact-trigger graph 替换 |
| `dag.rs` phase DAG | ~300 LOC | phase 依赖图 | 被 `watcher.rs` artifact-trigger 替换 |
| `inject_directives` / `golden_rules` | ~200 LOC | 向 Claude Code session 注入 prompt 规则 | 原则一：不竞争 CC 功能 |
| `decision_mode` / `escalate_grammar` | ~150 LOC | 决策模式 / escalation 语法注入 | 同上 |
| `team.yaml::kind: workflow`（phase 驱动） | schema 层 | phase 驱动模式的 team 配置 | EOL；`workflow.yaml` 替代 |
| statusline JSON 解析（CC session 侧） | ~100 LOC | 解析 CC session statusline → HarnessSnapshot | `claude --bg` state.json 替代 |
| tmux 作为 CC session 宿主逻辑 | ~150 LOC | 用 tmux 管理 CC session 生命周期 | `claude --bg` 原生管理 |

**总删除量约：~3500 LOC**（orchestrator.rs 主要部分 + phases.rs + 周边支撑代码）

### 5.2 保留的部分

以下内容**明确保留**，是 ccteam 核心价值所在：

- `progress.jsonl` 机制（business state SoT）
- `watcher.rs`（inotify → artifact change signal，重写为 artifact-trigger）
- `ccteam-web`（web UI，适配新 workflow 视图）
- `ccteam-mcp`（MCP server，工具集扩充）
- tmux 作为 **Codex session 宿主**（不变）
- meta-agent 常驻 session 模型（不变）
- 文件系统控制平面约定（不变）
- Gate 机制（重新实现，更 explicit）

---

## 6. 新组件规格

### 6.1 workflow.yaml schema

```yaml
# workflow.yaml — 完整字段定义
name: <string>                     # workflow 名，唯一标识
description: <string>              # 可选描述

agents:
  <role>:                          # role 名必须对应 .claude/agents/<role>.md
    executor: claude | codex       # executor 类型
    trigger:
      # 以下三种 trigger 选一：
      schedule                     # 定时（结合 interval 字段）或 meta-agent 手动触发
      watch:<path>                 # 监听 artifact 目录/文件，有新内容即触发
      gate                         # 等待 Gate 解锁（meta-agent 或人调用 trigger_gate）
    interval: <duration>           # 仅 trigger: schedule 时有效（如 "5m", "1h"）
    input: <path>                  # 可选：artifact 输入目录，通过 CCTEAM_INPUT env 注入
    output: <path>                 # 可选：artifact 输出目录，通过 CCTEAM_OUTPUT env 注入
    parallelism: <int>             # 可选，默认 1；watch trigger 下的最大并发 session 数
    timeout: <duration>            # 可选，默认无；超时后 meta-agent 收到 signal
    on_timeout: escalate | retry | skip  # 可选，默认 escalate
```

**核心约束**：

- `agents.<role>` 的 role 名必须有对应的 `.claude/agents/<role>.md`
  文件存在（orchestrator 启动时验证）；
- `workflow.yaml` 里**不允许** `prompt:` 字段——这是 schema 级
  hard error；
- `input`/`output` 路径是相对于项目根的相对路径；
- `parallelism > 1` 仅对 `trigger: watch:*` 有意义；
  `schedule`/`gate` trigger 的 agent 同时只有一个实例。

### 6.2 watcher.rs（artifact-trigger inotify watcher）

**新 watcher.rs**（~150 LOC）替换旧 `dag.rs` phase DAG 逻辑：

```
功能：
  - 对 workflow.yaml 中所有 trigger: watch:<path> 的路径注册 inotify IN_CLOSE_WRITE
  - 新文件写入完成 → 发送 ArtifactEvent { role, path, trigger_path }
  - signal bus 广播到 orchestrator + web SSE

实现约束：
  - 使用 inotify-rs crate（已在 workspace）
  - debounce 200ms（同一路径连续写入只触发一次）
  - 路径不存在时自动 mkdir -p（lazy 创建 artifact 目录）
  - watch 在 orchestrator 启动时注册，workflow reload 时重新注册
```

### 6.3 新 MCP 工具（meta-agent 用）

在现有 `ccteam-mcp`（9 tools）基础上新增 7 个工具：

| 工具名 | 参数 | 用途 |
|---|---|---|
| `spawn_agent` | `role, project_slug, input_path?` | 立即派发一个 agent session（不等 trigger）|
| `stop_agent` | `session_id` | 停止指定 agent session（软停：写 stop signal 文件）|
| `observe_agents` | `project_slug` | 列出当前所有 agent session 及其状态（读 state.json）|
| `signal` | `session_id, message` | 向指定 agent 发送 /btw 风格消息（通过 inbox 文件系统通道）|
| `set_parallelism` | `role, n` | 动态调整指定 role 的 parallelism 上限 |
| `trigger_gate` | `gate_name, project_slug` | 解锁指定 Gate，使 gate-trigger agent 可以启动 |
| `get_artifact_summary` | `project_slug, path` | 读取 artifact 目录摘要（文件数、最新几条、大小）|

保留现有 9 个工具（`run_new`、`list_projects`、`get_progress` 等），
根据新架构更新其实现。

### 6.4 ClaudeCodeAdapter（重构，~50 LOC）

```rust
// 新 ClaudeCodeAdapter（极薄）
pub struct ClaudeCodeAdapter;

impl HarnessAdapter for ClaudeCodeAdapter {
    /// 派发后台 agent session
    fn spawn(&self, role: &str, project_dir: &Path, env: &HashMap<String, String>)
        -> Result<SessionId>;
    // 实现：claude --bg --agent {role}
    //       工作目录 = project_dir
    //       额外 env = CCTEAM_INPUT, CCTEAM_OUTPUT, CCTEAM_PROJECT_SLUG

    /// 读取 session 状态
    fn observe(&self, id: &SessionId) -> Result<HarnessSnapshot>;
    // 实现：读 ~/.claude/jobs/{id}/state.json → 解析为 HarnessSnapshot
}
```

**删除**：
- statusline JSON 解析（`statusline_adapter.rs` 相关逻辑）
- tmux session 创建 / 管理（CC session 侧；Codex 侧保留）
- `pipe-pane` / FIFO 机制（CC session 侧；WS PTY 仍走 tmux attach）

### 6.5 CodexAdapter（真实实现，从 V0.3.3 吸收）

V0.3.1 仅有 trait stub，所有路径返回 `Err(NotImplemented)`。
V0.4.0 真实实现：

```rust
pub struct CodexAdapter {
    tmux_session_prefix: String,  // 默认 "ccteam-codex"
}

impl HarnessAdapter for CodexAdapter {
    fn spawn(&self, role: &str, project_dir: &Path, env: &HashMap<String, String>)
        -> Result<SessionId>;
    // 实现：
    //   tmux new-window -t {session_prefix} -n {role}-{id}
    //   codex --model o3 --dangerously-skip-permissions
    //         --input {CCTEAM_INPUT} --output {CCTEAM_OUTPUT}
    //   state 写 ~/.ccteam/codex/{id}/state.json

    fn observe(&self, id: &SessionId) -> Result<HarnessSnapshot>;
    // 实现：读 ~/.ccteam/codex/{id}/state.json
}
```

### 6.6 薄 orchestrator（目标 ~400 LOC）

旧 `orchestrator.rs` ~2713 LOC → 重写后目标 ~400 LOC：

```
职责（保留）：
  - 读 workflow.yaml → 构建 trigger graph
  - 启动 watcher，监听 artifact 目录
  - ArtifactEvent → 检查 parallelism → spawn_agent
  - Gate 状态管理（gate_states: HashMap<GateName, GateState>）
  - progress.jsonl append（task 开始 / 结束 / escalation / gate event）
  - fix-loop 计数器（3 次顶 escalate，不静默重置）
  - MCP server loop（接受 meta-agent 工具调用）

职责（删除）：
  - phase 状态机（plan-eng → implement → review → …）
  - prompt 注入（inject_directives / golden_rules）
  - decision_mode / escalate_grammar
  - phase 边界 context reset 逻辑（由 agent 自己管，或 meta-agent 通过 /exit 决策）
```

---

## 7. 示例 Workflow

### 7.1 ui-quality-loop（UI 质量循环）

**场景**：对一个 web 项目持续探索 UI 问题 → 并行修复 → codex review → 人工放行 → 发布。

```yaml
# .ccteam/workflows/ui-quality-loop.yaml
name: ui-quality-loop
description: 探索 UI 问题 → 并行修复 → codex review → 人工 Gate → 发布

agents:
  explorer:
    executor: claude
    trigger: schedule
    interval: 10m
    output: .ccteam/issues/

  fixer:
    executor: claude
    trigger: watch:.ccteam/issues/
    parallelism: 10
    input: .ccteam/issues/
    output: .ccteam/fixes/

  reviewer:
    executor: codex
    trigger: watch:.ccteam/fixes/
    input: .ccteam/fixes/
    output: .ccteam/verdicts/

  shipper:
    executor: claude
    trigger: gate
    input: .ccteam/verdicts/
```

**执行流程**：

```
1. orchestrator 启动 ui-quality-loop
2. 每 10 分钟：spawn explorer(claude --bg --agent explorer)
   explorer 把发现的 UI 问题写到 .ccteam/issues/<timestamp>-<id>.md
3. inotify 检测到 .ccteam/issues/ 有新文件
   → 检查 fixer parallelism（当前 < 10）→ spawn fixer
   → fixer 读取 issue，修复，把 diff/fix 写到 .ccteam/fixes/
4. inotify 检测到 .ccteam/fixes/ 有新文件
   → spawn reviewer(codex)
   → reviewer 验证 fix 质量，写 verdict 到 .ccteam/verdicts/
5. meta-agent 通过 get_artifact_summary 观察 verdicts 积累
   → 达到 threshold 后调用 trigger_gate("ship", slug)
   （或人直接在 web UI / CLI 解锁 Gate）
6. shipper 启动，读取 verdicts 列表，执行发布流程
```

**关键点**：
- explorer、fixer、reviewer 的**行为**完全由各自的
  `.claude/agents/<role>.md` 定义，ccteam 不注入任何 prompt；
- parallelism: 10 意味着最多同时跑 10 个 fixer session；
- meta-agent 可以随时调 `set_parallelism("fixer", 5)` 降速，
  或 `spawn_agent("explorer", slug)` 立即触发一轮探索。

### 7.2 research-loop（研究信息收集循环）

**场景**：持续抓取原始数据 → 评估 / 提炼洞察。

```yaml
# .ccteam/workflows/research-loop.yaml
name: research-loop
description: 持续抓取 raw data → 评估提炼 insights

agents:
  claw:
    executor: claude
    trigger: schedule
    interval: 30m
    output: .ccteam/raw-data/

  evaluator:
    executor: claude
    trigger: watch:.ccteam/raw-data/
    parallelism: 5
    input: .ccteam/raw-data/
    output: .ccteam/insights/
```

**执行流程**：

```
1. 每 30 分钟：spawn claw → 抓取原始数据写到 .ccteam/raw-data/
2. inotify 检测到 .ccteam/raw-data/ 有新文件
   → spawn evaluator（最多 5 并发）
   → evaluator 读原始数据，提炼 insight，写到 .ccteam/insights/
3. meta-agent 可订阅 .ccteam/insights/ 摘要（get_artifact_summary），
   定期 surface 给用户
```

---

## 8. Agent View 集成定位

### 8.1 ccteam **不**重建的功能

Claude Code Agent View（`claude agents`）是原生的多后台 session 管理 UI：

- session 列表、状态（running/idle/error）
- 单 session 进入 / 监控
- session 间切换

这些功能 **ccteam 不重建**。用户需要 live 监控单个 CC agent session 时，
直接用 `claude agents`。

### 8.2 ccteam-web 负责的层

ccteam-web 负责的是 **项目维度** 的上下文，Claude Code Agent View 无法提供的：

| 维度 | ccteam-web | Claude Code Agent View |
|---|---|---|
| **项目/workflow 视图** | workflow.yaml 拓扑可视化；artifact 目录状态；gate 状态 | 不支持（session 粒度） |
| **business state** | progress.jsonl 实时展示；cost 累计；task 完成度 | 不支持 |
| **artifact 目录浏览** | .ccteam/issues/、fixes/、verdicts/ 文件列表；最新内容预览 | 不支持 |
| **Gate 操作** | 在 web UI 点击解锁 Gate | 不支持 |
| **PTY 终端** | WS PTY relay（V0.3.2 F56/F57 已 ship）；tmux attach（Codex 用） | CC sessions 有自己的 TUI |
| **写操作** | BTW 注入、Gate 解锁、parallelism 调节 | 不支持 |

### 8.3 state.json 读取路径

ClaudeCodeAdapter.observe() 读取：

```
~/.claude/jobs/{id}/state.json
```

字段映射到现有 `HarnessSnapshot`：

```json
{
  "status": "running | idle | error | completed",
  "model": "claude-opus-4-5",
  "context_pct": 0.45,
  "cost_usd": 1.23,
  "last_activity": "2026-05-14T10:00:00Z"
}
```

ccteam-web 在 harness panel 展示此数据（现有 SSE `/sse/harness/` 通道），
不需要额外 API 端点。

---

## 9. 从 v0.3.x 迁移

### 9.1 team.yaml::kind: workflow（phase 驱动模式）EOL

V0.4.0 `team.yaml::kind: workflow` **不再被 orchestrator 支持**。
现有 phase 驱动项目需要迁移。

**迁移路径**：

```
旧 team.yaml（V0.3.x）             新结构（V0.4.0）
─────────────────────────────────────────────────────────
kind: workflow                     → 删除 kind 字段（或 kind: flex）
phases:                            → 删除 phases 字段
  - name: plan-eng                 → 创建 .ccteam/workflows/<name>.yaml
    prompt: |                      → 创建 .claude/agents/<role>.md（迁移 prompt）
      你是软件工程师……
```

**ccteam doctor 自动化**：

`ccteam doctor --migrate-phase-to-workflow` 命令：
1. 检测 `team.yaml::kind: workflow` 的项目
2. 读取 `phases` 列表
3. 生成 `workflow.yaml` 骨架（trigger 顺序连线）
4. 生成各 phase 对应的 `.claude/agents/<role>.md`（prompt 内容迁移）
5. 提示用户 review 后删除 `phases` 字段

**注意**：自动迁移只生成结构，prompt 内容保留原样。
用户应 review 生成的 agent 文件，确保行为符合预期。
phase 驱动的顺序语义 vs artifact-trigger 的事件驱动语义
可能需要手动调整 workflow 拓扑。

### 9.2 向前兼容的部分

以下 V0.3.x 内容在 V0.4.0 **不需要改动**：

| 内容 | V0.4.0 状态 |
|---|---|
| `team.yaml` 基础字段（`slug`、`team`、`description`、`worktree_dir`） | 保留 |
| `team.yaml::kind: flex`（flex 多 session 模式） | 保留（重新实现为 workflow.yaml 的等价形态） |
| `progress.jsonl` 格式 | 保留（新增 gate event / artifact event 类型）|
| `ccteam-mcp` 现有 9 个工具 | 保留（更新实现）|
| web UI auth（cookie / Bearer token）| 保留 |
| WS PTY relay（V0.3.2 F56/F57）| 保留 |
| `.claude/agents/<role>.md` 文件格式 | 保留（Claude Code 原生）|

### 9.3 用户需要做什么

1. **有 phase 驱动项目**：运行 `ccteam doctor --migrate-phase-to-workflow`，
   review 生成文件，删旧 `phases` 配置
2. **新建项目**：直接写 `workflow.yaml` + `.claude/agents/<role>.md`，
   不再写 `team.yaml::phases`
3. **meta-agent prompt**：现有 meta-agent 的 CLAUDE.md 需更新 MCP 工具列表
   （新增 7 个工具）；`ccteam doctor --update-meta-agent` 自动更新

---

## 10. F-finding 概览（F60–F69）

| Finding | 范围 | 代码量估算 | 依赖 |
|---|---|---|---|
| **F60** | Phase machinery removal（删 `phases.rs`、`inject_directives`、`golden_rules`、`decision_mode`、`escalate_grammar`、phase DAG） | -~1500 LOC | 无（先删再建）|
| **F61** | ClaudeCodeAdapter thin refactor（`claude --bg --agent` spawn + `state.json` observe；删 statusline 解析、tmux 作 CC 宿主） | -~250 LOC net | F60 |
| **F62** | Real CodexAdapter（tmux + codex spawn；state.json observe；从 V0.3.3 deferred 吸收） | +~200 LOC | F61 |
| **F63** | workflow.yaml schema + parser（serde 解析；schema 验证：no prompt field；trigger graph 构建；`ccteam doctor --migrate-phase-to-workflow`） | +~200 LOC | F60 |
| **F64** | Artifact watcher（inotify-based trigger；debounce；lazy mkdir；ArtifactEvent → signal bus） | +~150 LOC | F63 |
| **F65** | Meta-agent MCP tools（新增 `spawn_agent`、`stop_agent`、`observe_agents`、`signal`、`set_parallelism`、`trigger_gate`、`get_artifact_summary` 7 个工具） | +~300 LOC | F61 F62 |
| **F66** | Thin orchestrator（2713 LOC → ~400 LOC；phase 状态机替换为 artifact-trigger loop；Gate 状态管理；fix-loop 3 次顶 escalate 保留） | -~2300 LOC net | F63 F64 F65 |
| **F67** | Progress tracking refactor（progress.jsonl 新增 gate event / artifact event / agent session 生命周期 event；business state SoT 保持） | +~150 LOC | F66 |
| **F68** | ccteam-web v0.4.0 adaptation（workflow 拓扑视图；artifact 目录浏览；Gate 解锁 UI；harness panel 改读 state.json；Agent View 说明文档链接） | +~500 LOC TS | F65 F67 |
| **F69** | Example workflows + e2e + ship gate（ui-quality-loop + research-loop YAML + agent md 示例；e2e 测试；`cargo test --workspace` ≥ baseline；bump version 0.4.0） | +~200 LOC | F60–F68 |

**总体净减少**：Rust 侧约 -3000 LOC（删多建少）；TS 侧净增 +~500 LOC。

---

## 11. 不做（V0.4.0 scope limit）

以下内容**明确不在 V0.4.0 范围内**：

- **Workflow 可视化编辑器**（拖拽式 DAG 编辑）— V0.5+
- **Agent 市场 / workflow 模板库**（对外发布 workflow）— V0.5+
- **跨项目 artifact 共享**（一个项目的 output 触发另一个项目）— V0.5+
- **Codex 以外的第三方 executor**（Gemini CLI、GPT-4o CLI 等）— V0.5+
- **workflow.yaml 里的条件分支**（`if: artifact_count > N`）— V0.5+
- **Agent session 自动 context reset**（orchestrator 层面）— 保持由
  agent 自己管或 meta-agent 决策；V0.4.0 不引入 orchestrator 层 reset
- **Web Push 真实接入**（manifest + sw 骨架 V0.3.2 已 ship，协议端 V0.4 后续）
- **vitest 前端单测**（本轮 playwright e2e 为主）
- **`ccteam-web` 多租户 / 多用户**
- **`progress.jsonl` 历史 archive / 压缩**

---

## 12. 不可破的红线

直接 inherit 自 `CLAUDE.md §三` + `docs/tech-design.md`，
V0.4.0 有几条特别需要确认的项：

| 红线 | V0.4.0 状态 | 说明 |
|---|---|---|
| **progress.jsonl 是 business state 唯一 SoT** | **不破** | artifact 目录变化也记录到 progress.jsonl（新增 artifact event 类型）；artifact 目录本身不作为 orchestrator 状态来源 |
| **文件系统是控制平面** | **不破** | artifact 目录 = 唯一 inter-agent 通信；新增 gate state 也写文件系统 |
| **不解析 tmux 终端输出** | **不破** | CC session 改走 `state.json`；Codex 状态写 `~/.ccteam/codex/{id}/state.json` |
| **永不主动 kill 长 session** | **不破** | `stop_agent` MCP 工具写 stop signal 文件（soft stop）；不调 `tmux kill-session` 或 `pkill claude` |
| **fix-loop 撞 3 次顶必 escalate** | **不破** | 保留 orchestrator 的 fix-loop 计数；3 次后触发 `escalate` → meta-agent 收 signal |
| **ccteam-core 零 team 名字面量** | **不破** | workflow.yaml 的 agent role 名是用户定义数据；orchestrator 不 hardcode 任何 role 名 |
| **Agent View 不重建** | **新红线** | ccteam 不实现自己的多后台 CC session 列表 UI；`claude agents` 是原生监控层 |
| **workflow.yaml 禁止 prompt 字段** | **新红线** | schema hard error；任何在 workflow.yaml 里注入 prompt 的 PR 一律拒绝 |
| **不用 `--resume` flag** | **不破** | CC session 的 context 管理仍由 agent 自己处理；orchestrator 不干预 |
| **claude-mem 严格可选** | **不破** | meta-agent MCP 工具调用沿用 conditional "如有 `mcp__*claude-mem*search` 工具则可调" 约定 |

---

## 附 A — 参考资料

- `docs/tech-design.md` §2.1 三层架构 + §3.8 Web 仪表盘（V0.4.0 后更新）
- `docs/dev-coupling-audit.md`（F60-F69 entry 待填入）
- `docs/versions/v0-3-2/README.md`（V0.3.2 ship 状态，V0.4.0 base）
- `docs/versions/v0-3-1/prd.md` §10.3（CodexAdapter deferred，V0.4.0 F62 吸收）
- `crates/ccteam-core/src/phases.rs`（删除目标，934 LOC）
- `crates/ccteam-core/src/orchestrator.rs`（重构目标，2713 LOC → ~400 LOC）
- Claude Code 官方文档：`claude --bg --agent` + Agent View + `~/.claude/jobs/`
- `references/codex/codex-rs/`（CodexAdapter 接口参考，F62）
