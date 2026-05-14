# ccteam V0.4.0 用户手册

> 本手册面向**ccteam V0.4.0 终端用户**：日常用 `ccteam` 编排 Claude Code /
> Codex 多 agent workflow 的开发者。架构理论 / 设计决策见 [`prd.md`](prd.md)；
> 从 V0.3.x 升级见 [`migration-guide.md`](migration-guide.md)。

---

## 1. 概述

V0.4.0 是 ccteam 的**架构级重构**。核心变化：

- **Phase 模板系统全部删除**——ccteam 不再预定义 `plan-eng` / `implement` /
  `review` 这类固定 phase。每个项目的 workflow 由用户的 `workflow.yaml`
  完全定义。
- **Agent-as-Network 模型**——`workflow.yaml` 只描述 agent 之间的连线
  （触发条件 + artifact 目录）；**ccteam 不向 workflow.yaml 注入任何 prompt**。
  agent 行为完全由各自的 `.claude/agents/<role>.md` 定义（Claude Code 原生
  subagent 文件格式）。
- **Claude Code Agent View 原生集成**——`claude --bg --agent <role>` 派发
  后台 session，ccteam 不再用 tmux 当 Claude Code 的宿主，也不再解析 statusline
  JSON；session 状态从 `~/.claude/jobs/<id>/state.json` 读取。
- **Artifact-driven 触发**——agent 间唯一通信媒介是文件系统目录。一个
  agent 写文件到 output 目录 → inotify watcher 检测到 → 自动 spawn 下游 agent。

V0.4.0 的目标：**100 个 agent 可同时并行运行**；人只在目标输入和最终 review
出现，中间全程 autonomous。

---

## 2. 快速上手（5 分钟跑通 ui-quality-loop）

### 第 1 步：创建项目

```bash
ccteam new ui-quality --team dev
```

这会在 `~/projects/dev-ui-quality/` 创建一个新项目目录（slug 自动带 team
前缀）。`cd ~/projects/dev-ui-quality`。

### 第 2 步：拷贝 workflow + agent 模板

```bash
# workflow 拓扑
cp ~/workplace/agents/ccteam/examples/workflows/ui-quality-loop.yaml ./workflow.yaml

# agent 行为定义
mkdir -p .claude/agents
cp ~/workplace/agents/ccteam/examples/workflows/agents/{explorer,fixer,reviewer,shipper}.md .claude/agents/
```

随后 review 并按你的项目改写 `.claude/agents/*.md` 正文 prompt。模板是
通用骨架，真实 prompt 应描述你的代码栈、UI 规范、验收标准。

### 第 3 步：启动 orchestrator

```bash
ccteam run dev-ui-quality
```

orchestrator 启动时：

1. 加载 `workflow.yaml` 并验证（任何 `prompt:` 字段会被 reject）
2. 验证每个 agent role 都有对应 `.claude/agents/<role>.md`
3. 注册 inotify watcher 到所有 `watch:` trigger 的路径
4. 初始化 Gate 状态（默认 locked）
5. progress.jsonl 写入 `workflow_start` event

### 第 4 步：在 web UI 看 workflow 拓扑

```bash
ccteam web --bind 127.0.0.1:7331
# 浏览器打开 http://127.0.0.1:7331/app/projects/dev-ui-quality
```

可以看到：

- workflow 拓扑图（agent 节点 + trigger 边）
- artifact 目录浏览（`.ccteam/issues/` / `.ccteam/fixes/` / ...）
- 当前活跃 session 列表（live 监控走 `claude agents`，web UI 只展示摘要）
- Gate 状态 + 解锁按钮
- progress.jsonl 实时流（SSE）

### 第 5 步：触发首轮

通过 meta-agent（推荐）或 CLI：

```bash
# 通过 meta-agent session（自然语言）
# 跟 meta-agent 说："现在跑一轮 explorer，让它探索登录页的 UI 问题"

# 或直接 CLI
ccteam ctl spawn-agent --slug dev-ui-quality --role explorer
```

之后整个 workflow 自动运转：explorer → 写 issues → watcher 触发 fixer ×
N（并行）→ 写 fixes → 触发 reviewer → 写 verdicts。

### 第 6 步：解锁 Gate 发布

reviewer 攒够 pass 的 verdict 后，在 web UI 点击 shipper 的 unlock 按钮，
或 CLI：

```bash
ccteam ctl trigger-gate --slug dev-ui-quality --gate shipper
```

shipper 启动 → 整合 fix → 创建 PR → 写 ship-log → 完成。

---

## 3. `workflow.yaml` 完整 schema

```yaml
name: <string>                # 必填，workflow 名（唯一标识，progress.jsonl 关联用）
description: <string>         # 可选，给 web UI 展示

agents:
  <role>:                     # role 名必须有对应 .claude/agents/<role>.md
    executor: claude | codex  # 必填
    trigger:                  # 必填，4 选 1
      manual                  # meta-agent / CLI 显式触发
      schedule                # 定时（搭配 interval 字段）
      watch:<path>            # 监听 artifact 目录，新文件即触发
      gate                    # 等 Gate 解锁（trigger_gate MCP 工具）
    interval: <duration>      # 仅 trigger: schedule 有效（如 "5m"、"1h"、"30s"）
    input: <path>             # 可选，通过 CCTEAM_INPUT env 注入；相对项目根
    output: <path>            # 可选，通过 CCTEAM_OUTPUT env 注入；相对项目根
    parallelism: <int>        # 可选，默认 1；watch trigger 下最大并发 session 数
    timeout: <duration>       # 可选，默认无；超时后 meta-agent 收 signal
    on_timeout: escalate | retry | skip  # 可选，默认 escalate
```

### 字段说明

| 字段 | 含义 | 必填 | 默认 |
|---|---|---|---|
| `name` | workflow 唯一标识 | 是 | - |
| `description` | web UI 展示用 | 否 | - |
| `agents.<role>.executor` | `claude` 或 `codex` | 是 | - |
| `agents.<role>.trigger` | `manual`/`schedule`/`watch:<path>`/`gate` | 是 | - |
| `agents.<role>.interval` | schedule trigger 的间隔 | 仅 schedule | - |
| `agents.<role>.input` | 输入目录路径 | 否 | - |
| `agents.<role>.output` | 输出目录路径 | 否 | - |
| `agents.<role>.parallelism` | 最大并发 session | 否 | `1` |
| `agents.<role>.timeout` | 单 session 超时 | 否 | 无 |
| `agents.<role>.on_timeout` | 超时处理策略 | 否 | `escalate` |

### Schema 红线

| 禁止字段 | 原因 |
|---|---|
| `prompt:` | agent 行为由 `.claude/agents/*.md` 定义，不能在 workflow.yaml 里注入 |
| `system_prompt:` | 同上 |
| `messages:` | 同上 |
| `model:` / `temperature:` 等 LLM 参数 | 写到 `.claude/agents/<role>.md` 的 frontmatter `model:` 字段 |

orchestrator 启动时遇到任一禁止字段 = hard error，进程退出非零退出码。

---

## 4. Agent role 文件约定

每个 workflow.yaml 中的 `<role>` 都需要对应一个 `.claude/agents/<role>.md`：

```markdown
---
name: <role>                  # 必填，必须和 workflow.yaml 中 role 名一致
description: <string>         # 必填，一句话说明（Claude Code 选择 agent 时用）
tools: Read, Write, Bash, ... # 可选，列出该 agent 允许的工具
model: opus | sonnet | haiku  # 可选，指定 Claude 模型
color: red | green | blue ... # 可选，web UI 显示色
---

# 正文 prompt

描述 agent 的任务、输入输出约定、验收标准。可以用 `$CCTEAM_INPUT` /
`$CCTEAM_OUTPUT` 占位引用 ccteam 注入的环境变量。
```

### ccteam 注入的环境变量

每次 spawn agent session 时，ccteam 设置以下环境变量：

| Env var | 含义 | 何时有 |
|---|---|---|
| `CCTEAM_PROJECT_SLUG` | 当前项目 slug | 总是 |
| `CCTEAM_ROLE` | 当前 agent role 名 | 总是 |
| `CCTEAM_SESSION_ID` | 本次 spawn 的 session id | 总是 |
| `CCTEAM_INPUT` | workflow.yaml `input:` 字段的绝对路径 | 配置了 `input:` 时 |
| `CCTEAM_OUTPUT` | workflow.yaml `output:` 字段的绝对路径 | 配置了 `output:` 时 |
| `CCTEAM_WORKFLOW_NAME` | 当前 workflow 名 | 总是 |
| `CCTEAM_PROJECT_DIR` | 项目根目录绝对路径 | 总是 |

agent prompt 直接 `$CCTEAM_INPUT` 引用即可（shell 风格展开）。

---

## 5. 四种 trigger 详解

### 5.1 `manual`

需要 meta-agent 或人显式调 `ccteam__spawn_agent(role, slug)` 才启动。
适合：作为流程起点的 agent（如 `explorer`、`searcher`），或者作为应急
backup（meta-agent 看到流程卡了，手动塞一个 worker）。

```yaml
explorer:
  executor: claude
  trigger: manual
  output: .ccteam/issues/
```

### 5.2 `schedule`

按 `interval` 间隔自动 spawn。适合：定时巡检、定时数据抓取。

```yaml
explorer:
  executor: claude
  trigger: schedule
  interval: 10m          # 也支持 "1h" / "30s" 等
  output: .ccteam/issues/
```

**注意**：interval 是**距离上一次 spawn 完成**的时间，不是 wall clock；
也就是说如果一次 explorer 跑了 5 分钟，下一次会在 15 分钟后 spawn，
而非 10 分钟。这是为了避免叠加。

### 5.3 `watch:<path>`

inotify 监听某个目录，新文件出现就 spawn 一个下游 agent。
**这是 V0.4.0 最核心的 trigger 类型**。

```yaml
fixer:
  executor: claude
  trigger: watch:.ccteam/issues/
  parallelism: 10
  input: .ccteam/issues/
  output: .ccteam/fixes/
```

行为：

1. orchestrator 启动时 mkdir -p `.ccteam/issues/`（lazy 创建）
2. 注册 inotify `IN_CLOSE_WRITE` 监听
3. 新文件写完 → debounce 200ms（避免 partial write）→ 触发 spawn
4. 检查当前 `fixer` session 数 < `parallelism` → spawn；否则 queue

ccteam 不保证 fixer 处理"哪个" issue——fixer 自己 grab 一个未处理的
issue（用 lock 文件约定），其他 fixer 跳过。详见
`examples/workflows/agents/fixer.md` 的 "lock 文件协议"。

### 5.4 `gate`

等 Gate 解锁后才能启动。Gate 默认 locked，需要：

- meta-agent 调 `ccteam__trigger_gate("<role>", slug)`
- 或人在 web UI / CLI 点 unlock

```yaml
shipper:
  executor: claude
  trigger: gate
  input: .ccteam/verdicts/
```

Gate 解锁后，shipper spawn 一次（不像 watch 会反复触发）。如果需要
再跑一次，重新解锁。

---

## 6. Meta-agent MCP 工具（17 个）

V0.4.0 在原 10 个工具基础上新增 7 个。完整列表：

### 原有 10 个（V0.3.x 继承，部分实现更新）

| 工具 | 用途 |
|---|---|
| `ccteam__new` | 创建新项目 |
| `ccteam__ls` | 列出所有项目 |
| `ccteam__show` | 查看项目详情 |
| `ccteam__progress` | 读 progress.jsonl 状态 |
| `ccteam__pause` | 暂停 workflow |
| `ccteam__resume` | 恢复 workflow |
| `ccteam__peek` | 查看 session 内容（read-only） |
| `ccteam__send_to_session` | 向 session 发消息（旧 idle-aware 注入） |
| `ccteam__inject_decision` | 注入决策点 |
| （未列；详见 `interfaces.md`） | |

### 新增 7 个（V0.4.0 F65 引入）

| 工具 | 参数 | 用途 |
|---|---|---|
| `ccteam__spawn_agent` | `role, project_slug, input_path?` | 立即派一个 agent session（绕过 trigger）|
| `ccteam__stop_agent` | `session_id` | 软停一个 agent session（写 stop signal 文件，不 kill）|
| `ccteam__observe_agents` | `project_slug` | 列出当前所有 session 及其状态（读 state.json） |
| `ccteam__signal` | `session_id, message` | 向运行中 session 发 /btw 风格消息（inbox 文件） |
| `ccteam__set_parallelism` | `role, n` | 动态调整 role 的 parallelism 上限 |
| `ccteam__trigger_gate` | `gate_name, project_slug` | 解锁 Gate，使 gate-trigger agent 可启动 |
| `ccteam__get_artifact_summary` | `project_slug, path` | 读 artifact 目录摘要（文件数、最新 N 条、总大小） |

meta-agent prompt 文档（`~/.claude/CLAUDE.md` 或 meta session 启动注入）
应列出全部 17 个工具，让 meta-agent 知道可用能力。

---

## 7. Gate 解锁方式

Gate 是 V0.4.0 的显式人机交互检查点。三种解锁方式：

### 7.1 Web UI 点击

```
http://localhost:7331/app/projects/<slug>
→ workflow 拓扑图
→ 点 gate-trigger agent 的 [Unlock] 按钮
→ 弹出确认（含 artifact 摘要 + 待发布条目数）
→ 确认 → Gate 解锁
```

### 7.2 CLI

```bash
ccteam ctl trigger-gate --slug <slug> --gate <role>
```

### 7.3 Meta-agent 自动

meta-agent 通过 `ccteam__trigger_gate` 自主决策解锁。**适合 low-risk
场景**（如：reviewer 100% verdicts pass + cost < $5 → 自动解锁 shipper）。
meta-agent prompt 应明确约束哪些情况下可自动解锁、哪些必须 surface 给人。

---

## 8. Parallelism 动态调节

watch trigger 下的 `parallelism: N` 字段是**初始默认值**。运行时可以动态改：

```bash
# Meta-agent 调
mcp__ccteam__ccteam__set_parallelism(role="fixer", n=5)

# CLI 等价
ccteam ctl set-parallelism --slug <slug> --role fixer --n 5
```

降速场景：

- cost 烧太快 → 临时降到 2-3 试水
- 某 agent 反复 escalate → 降到 1 让 meta-agent 介入排查
- 上游 explorer 速度过慢，多并发 fixer 也没活干 → 降回默认

升速场景：

- 早期 smoke 后确认逻辑稳定 → 把 fixer parallelism 从 3 提到 10
- cost budget 充足 + 任务积压 → 提到 20

**注意**：实际并发数也受系统 cost budget guard 影响——见 §9。

---

## 9. Budget 检查（$200 上限）

ccteam 跟踪每个项目累计 cost（读取 `state.json` 中的 `cost_usd` +
聚合所有 session）。默认上限 **$200 / project**，超出后：

1. progress.jsonl 写入 `budget_exceeded` event
2. orchestrator 停止 spawn 新 session（**不主动 kill 在跑 session**——红线）
3. web UI / meta-agent / CLI 都收到 alert
4. 用户需要：
   - 调 budget：`ccteam ctl set-budget --slug <slug> --limit 300`
   - 或降 parallelism + 等已跑 session 完成
   - 或暂停 workflow：`ccteam ctl pause --slug <slug>`

软告警阈值（不自动 kill）：

- $50 → progress.jsonl `cost_warning_50`
- $100 → progress.jsonl `cost_warning_100`
- $200 → `budget_exceeded` + 停止 spawn

---

## 10. 故障排查 FAQ

### Q1: workflow 不启动？

```bash
# 检查 workflow.yaml 路径
ls workflow.yaml  # 或 .ccteam/workflow.yaml
# 检查 schema 是否合法
ccteam ctl validate-workflow --slug <slug>
# 看 orchestrator 日志
tail ~/.ccteam/logs/<slug>.log
```

常见原因：

- `workflow.yaml` 路径不在搜索列表（搜：项目根 / `.ccteam/workflow.yaml` /
  `.ccteam/workflows/*.yaml`）
- `prompt:` 字段被 reject
- `.claude/agents/<role>.md` 缺失（schema 校验失败）

### Q2: agent 不响应 / 看不到 session 出现？

```bash
# 查 Claude Code 原生 session 列表
claude agents

# 查 ccteam 这边的视角
ccteam ctl observe --slug <slug>

# 直接看 state.json
ls ~/.claude/jobs/
cat ~/.claude/jobs/<id>/state.json
```

常见原因：

- `claude --bg --agent <role>` 实际没派起来（CC 版本 < 2.1.139？）
- agent role 名不匹配 `.claude/agents/<role>.md`
- parallelism 已满（看 `ccteam__observe_agents`）

### Q3: artifact 触发失败？写了文件，下游没启动？

```bash
# 看 inotify watcher 日志
grep "watcher" ~/.ccteam/logs/<slug>.log

# 看 progress.jsonl 是否有 artifact_event
ccteam ctl progress --slug <slug> | grep artifact
```

常见原因：

- 文件写入用了 truncate-then-write（没有 `IN_CLOSE_WRITE`）→
  建议改用原子写：写到 `.tmp` 再 `mv`
- 文件路径不在 watch 列表（workflow.yaml `watch:` 字段拼错）
- debounce 期内有其他文件覆盖（200ms 内）

### Q4: cost 超支？

```bash
# 查累计 cost
ccteam ctl show --slug <slug> | grep cost
# 查每个 agent cost 占比
ccteam ctl observe --slug <slug> --include-cost

# 立即降速
ccteam ctl set-parallelism --slug <slug> --role fixer --n 1

# 看 budget_exceeded event
ccteam ctl progress --slug <slug> | grep budget
```

常见原因：

- parallelism 设太高 + agent context 没及时 reset
- 某个 agent 进入 infinite loop（fix-loop escalation 应该 3 次顶住，
  但偶发 race；查 progress.jsonl `escalation` event）
- model 选择不当（用了 opus 跑简单 fix）

---

## 11. 从 V0.3.x 迁移

如果你有 V0.3.x phase 驱动项目（`team.yaml::kind: workflow` + `phases:`
列表）：

```bash
ccteam doctor --migrate-phase-to-workflow
```

这个命令会：

1. 检测 `team.yaml::kind: workflow` 的项目
2. 读取 `phases:` 列表
3. 生成 `workflow.yaml` 骨架（按 phase 顺序连线）
4. 生成 `.claude/agents/<phase-name>.md`（迁移 prompt 内容）
5. 提示你 review 后删除旧 `phases` 字段

完整迁移指南：[`migration-guide.md`](migration-guide.md)。

---

## 12. 进一步阅读

- [`prd.md`](prd.md) — V0.4.0 PRD（架构哲学 + 设计决策）
- [`migration-guide.md`](migration-guide.md) — V0.3.x phase → V0.4.0 workflow 迁移
- [`e2e-retro.md`](e2e-retro.md) — F69 ship gate 验收记录
- [`../tech-design.md`](../tech-design.md) — ccteam 系统设计 SoT
- [`../interfaces.md`](../interfaces.md) — 协议参考（workflow.yaml schema /
  MCP 工具签名 / progress.jsonl event 类型）
- [`../../examples/workflows/`](../../examples/workflows/) — 可拷贝示例
