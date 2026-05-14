# ccteam V0.4.0 示例 workflow

> 本目录提供 V0.4.0 artifact-driven workflow 架构下的可拷贝参考示例。
> 每个示例 = 一份 `workflow.yaml`（agent 拓扑）+ 一组 `.claude/agents/<role>.md`
> （agent 行为定义）。**ccteam 不向 workflow.yaml 注入任何 prompt**——agent
> 的行为完全由对应的 `.claude/agents/<role>.md` 文件决定。

## 示例清单

| 文件 | 场景 | 涉及 agent | 触发模式 |
|---|---|---|---|
| [`ui-quality-loop.yaml`](ui-quality-loop.yaml) | UI 质量持续巡检：探索 UI 问题 → 并行修复 → codex review → 人工 Gate → 发布 | explorer / fixer / reviewer / shipper | manual + watch + gate |
| [`research-loop.yaml`](research-loop.yaml) | 研究信息收集：持续抓取 raw data → 并行评估提炼 insights → codex 审查 → 报告 | searcher / synthesizer / reviewer / reporter | manual + watch + gate |

两个示例都用 4 个 agent 的"扇出 → 扇入 → Gate → 收尾"骨架，
区别在 input/output 目录约定 + 业务语义。把任何一个当成新 workflow 的起点都行。

## 怎么用（5 分钟跑通 ui-quality-loop）

### 第 1 步：创建项目

```bash
ccteam new ui-quality --team dev
# 项目落到 ~/projects/dev-ui-quality/（slug 自动带 team 前缀）
cd ~/projects/dev-ui-quality
```

### 第 2 步：拷贝 workflow 模板

```bash
# 项目根 workflow.yaml（orchestrator 默认查找路径）
cp ~/workplace/agents/ccteam/examples/workflows/ui-quality-loop.yaml ./workflow.yaml
# 或放到 .ccteam/workflow.yaml（次选路径，与 .ccteam/ 状态目录同处）
```

### 第 3 步：拷贝 agent role 模板

```bash
mkdir -p .claude/agents
cp ~/workplace/agents/ccteam/examples/workflows/agents/{explorer,fixer,reviewer,shipper}.md .claude/agents/
```

随后 **逐个 review 并按你的项目改写** `.claude/agents/*.md` 的正文 prompt。模板只是
起点，真实 prompt 应描述你的具体 UI / 代码栈 / 验收标准。

### 第 4 步:启动 orchestrator daemon

```bash
ccteam start --foreground
# orchestrator 扫所有 ~/.ccteam/projects/ 下含 workflow.yaml 的项目,
# 各自 watch trigger 已注册,gate 处于 locked 状态。
# 此命令是全局 daemon — 不是 per-slug,要看具体项目用:
#   ccteam show <slug>
#   ccteam progress <slug> --tail
```

### 第 5 步:触发首轮探索

V0.4.0 没有 `ccteam ctl` CLI。两条触发路径:

```bash
# A. 通过 meta-agent(推荐):在 meta-agent claude session 里说自然语言
#    "现在跑一轮 explorer 探索登录页 UI 问题"
#    meta-agent 调 mcp__ccteam__ccteam__spawn_agent(slug="dev-ui-quality", role="explorer")

# B. 直接写 marker 文件让 orchestrator 下一 tick(5s)接走:
mkdir -p ~/projects/dev-ui-quality/.ccteam/spawn_requests
echo '{}' > ~/projects/dev-ui-quality/.ccteam/spawn_requests/explorer-$(date +%s).json
```

explorer 把 UI 问题写到 `.ccteam/issues/`,inotify 自动触发 fixer 并发(最多
10 个)→ fixer 写 `.ccteam/fixes/` → 触发 reviewer → 写 `.ccteam/verdicts/`。

### 第 6 步:解锁 Gate 发布

reviewer 攒够 verdict 后,在 ccteam-web UI 点击 "shipper" gate 的解锁按钮,
或通过 meta-agent MCP 工具,或直接写 marker 文件:

```bash
# A. meta-agent NL:"我看 verdicts 都过了,解锁 shipper 出货"
#    调用 mcp__ccteam__ccteam__trigger_gate

# B. 直接写 marker:
mkdir -p ~/projects/dev-ui-quality/.ccteam/gate_override
echo '{}' > ~/projects/dev-ui-quality/.ccteam/gate_override/shipper
```

shipper 启动,读取 verdict 列表执行发布动作。

## workflow.yaml 字段速查（详见 `docs/v0-4-0/user-manual.md`）

```yaml
name: <string>                     # workflow 名，唯一标识
description: <string>              # 可选

agents:
  <role>:                          # 必须有对应 .claude/agents/<role>.md
    executor: claude | codex       # 执行环境
    trigger:
      manual                       # meta-agent 或 CLI 显式触发
      schedule                     # 定时（搭配 interval: "5m")
      watch:<path>                 # 监听 artifact 目录，新文件触发
      gate                         # 等 Gate 解锁
    interval: <duration>           # schedule trigger 专用
    input: <path>                  # CCTEAM_INPUT env 注入
    output: <path>                 # CCTEAM_OUTPUT env 注入
    parallelism: <int>             # watch trigger 下的最大并发，默认 1
    timeout: <duration>            # 可选
    on_timeout: escalate|retry|skip
```

**禁止字段**：`prompt:` / `system_prompt:` / `messages:`——schema 级 hard error。
agent 行为只能写在 `.claude/agents/<role>.md`。

## Agent role 文件（`.claude/agents/<role>.md`）约定

参考 `agents/` 子目录的 4 个模板（`explorer.md` / `fixer.md` /
`reviewer.md` / `shipper.md`）。所有 agent 文件都遵循 Claude Code 原生格式：

```markdown
---
name: <role>
description: <一句话说明，用于 Claude Code 选择 agent>
tools: Read, Write, Edit, Bash, ...
model: opus | sonnet  # 可选
---

正文 prompt：描述 agent 的任务 / 输入输出约定 / 验收标准。
```

ccteam 在 spawn agent 时通过环境变量传递上下文：

| Env var | 含义 |
|---|---|
| `CCTEAM_PROJECT_SLUG` | 当前项目 slug（如 `dev-ui-quality`）|
| `CCTEAM_INPUT` | workflow.yaml 中 `input:` 字段的绝对路径 |
| `CCTEAM_OUTPUT` | workflow.yaml 中 `output:` 字段的绝对路径 |
| `CCTEAM_ROLE` | 当前 agent role 名 |
| `CCTEAM_SESSION_ID` | 本次 spawn 的 session id |

agent 在 prompt 里直接引用 `$CCTEAM_INPUT` / `$CCTEAM_OUTPUT` 即可。

## 不直接拷贝模板的话

如果你自己写新 workflow,只要遵守两条规则:

1. `workflow.yaml` 里 **不写一行 prompt**——所有行为 → `.claude/agents/*.md`
2. agent 间 **只通过 artifact 文件系统目录通信**——不依赖任何 RPC / 共享内存

剩下的 ccteam 帮你管:trigger / parallelism / Gate / progress.jsonl / cost
统计 / Web UI 拓扑可视化。

## 进阶

- **多 workflow**:一个项目里可以放多份 workflow.yaml(放到
  `.ccteam/workflows/<name>.yaml`),meta-agent 通过 `select_workflow` 切换
- **dynamic parallelism**:meta-agent 调用 `ccteam__set_parallelism("fixer", 5)`
  动态降速,无需重启 orchestrator
- **escalation**:fix-loop 撞 3 次顶,orchestrator 自动发 `escalate` signal
  给 meta-agent;meta-agent 决策是改 prompt / 派更强的 model / 还是 surface 给人
- **跨执行环境**:同一个 workflow 可以混用 `executor: claude` 和
  `executor: codex`,artifact 目录是唯一通信媒介

## 进一步阅读

- [`../../docs/v0-4-0/user-manual.md`](../../docs/v0-4-0/user-manual.md) —
  V0.4.0 完整用户手册
- [`../../docs/v0-4-0/prd.md`](../../docs/v0-4-0/prd.md) —
  架构哲学 + 核心抽象 + 三层架构
- [`../../docs/v0-4-0/migration-guide.md`](../../docs/v0-4-0/migration-guide.md) —
  V0.3.x phase 驱动项目迁移指南
