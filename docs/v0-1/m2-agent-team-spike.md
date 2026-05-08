# M2.2.0 Agent Team 兼容性 spike 报告

> 触发任务:`docs/v0-1/development-plan.md` §4 M2.2 子任务 M2.2.0(0.5 天硬上限)
>
> 目的:验证 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 在 Claude Code 当前
> 版本下是否仍触发 Agent Teams 实验功能(Lead + 多角色并行子 agent)。
> 结果决定 M2.2 是否能按 spec 继续(`parallelism: agent_team` +
> `agent_team: [{role: ...}]` YAML 直接使能)。

## 实测环境

- **CLI**:`claude --version` → `2.1.128 (Claude Code)`
- **Spike 时间**:2026-05-06
- **Env**:`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 已由 Claude Code 父进程注入
  (在本 spike 进程的 `env` 输出里能看到),说明这个 env var 在 2.1.128
  下**仍被某段代码引用**(否则不会被传递)
- **OS**:Linux 6.6.87.2-microsoft-standard-WSL2

## 调查方法

不实测真子进程(避免 spike 期间双 claude session 互相污染本对话上下文),
改走 desk research:

1. `claude --help` 完整 dump,搜索 `agent` / `team` / `experimental` 关键字
2. `claude agents --help` 与 `claude agents list` 实测
3. `claude --help` 列出的所有 agent 相关 flag(`--agent`、`--agents`、
   `agents` 子命令)
4. `claude-plugins-official` 缓存目录里搜 `EXPERIMENTAL_AGENT_TEAMS` /
   `agent_team` 引用
5. ccteam 自己 `docs/` 与 `references/` 是否有更早的实测笔记

## 结论(三项,每项都有证据)

### 1. `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` 不再是用户可见的 CLI feature

`claude --help` **没有任何**与 agent_team / experimental teams /
multi-role 相关的 flag。env var 仍由父进程注入(说明代码里某处还有引用),
但**不通过 CLI 暴露**给用户配置。

可见的 agent 接口换成了:

| 接口 | 用途 |
|---|---|
| `--agent <agent>` | 整个 session 用一个角色(覆盖 `agent` setting) |
| `--agents <json>` | 内联 JSON 注册自定义 agent(ad-hoc) |
| `claude agents` 子命令 | Manage background and configured agents |
| `Task(subagent_type=...)` 工具 | phase 内一次性派单(subagent 一次跑完返回) |
| `~/.claude/agents/<name>.md` | 把 plugin agent 注册成 Task-callable subagent |

这是 ccteam M0.5 已经在用的工具触发面 —— 单点 dispatch + 异步,**不是
Agent Teams 原本的"Lead 协调多角色并行"模型**。

### 2. 找不到 Lead-coordinated multi-role 的 first-class CLI 路径

`claude --help` 没有 `--agent-team` / `--lead` / `--roles` / `--team-config`
等任何 flag。`--agents <json>` 注册的是 ad-hoc subagent 的字典,**不构造
Lead 协调关系**。

`claude agents list` 输出 4 + 1 = 5 个内置 agent(Explore / general-purpose
/ Plan / statusline-setup / claude-code-guide)+ `~/.claude/agents/` 里
linked 的 plugin agents。**没有"team" / "lead" 这种实体的输出**。

`claude-plugins-official` 缓存目录里搜 `EXPERIMENTAL_AGENT_TEAMS` 与
`agent_team` 都**零结果**;只有一处宽泛措辞:
`code-modernization/commands/modernize-reimagine.md` 说"orchestrates a
multi-agent team with explicit human checkpoints"——指 plugin 的工作流编排,
**不是 Claude Code 的 Agent Teams 实验功能**。

### 3. ccteam 现有 `agent_team` 设计基于 2026 年初的 Anthropic 公开范例

回查 ccteam 仓库,只有 ccteam 自己的 docs(`user-guide.md` / `tech-design.md`
/ `interfaces.md`)在用 `agent_team` 名词;`references/` 里没有 spike 实测
笔记可对照。**没有证据证明 Anthropic 当前版本仍把这个 feature 当作
first-class CLI surface**。

## 风险评估

实施 M2.2(把 dev `implement` phase 改 `parallelism: agent_team` + 三角色
prompt 注入)有以下可能后果(按风险递增):

- (低)env var 仍生效但行为变化:role 命名约定改了,Lead 协调机制改了,
  prompt 注入仍能让 claude 按角色叙述,但**没有真并行子 agent**。和
  `parallelism: solo` 退化成同一种行为。
- (中)env var 已退化为 no-op:flag 仍被代码读取但分支被废弃。
  `agent_team` YAML 完全不生效,但 ccteam 看不到任何错误信号(因为
  env var 不报"已废弃")。
- (高)2.1.x 主干已经移除该路径:env var 命中残留代码但产生 panic /
  hang。orchestrator 会**直到 stall_warn_minutes 才发现**,代价是用户
  时间。

**关键观察**:本 spike 没有跑真子 claude 验证以上三档哪一个为真。三档的
实测单价都是 5–15 分钟 + 真 LLM 成本 + 跑出来的输出还要人为判断"这是真
multi-role 还是 single role 角色扮演"。**spike 0.5 天硬上限内无法完成
完整三档判定**。

## 建议(决策点 → escalate 给用户)

按 user prompt §7「M2.2.0 spike 失败 = 立即 escalate」的精神,本 spike
**判定为不可推进 M2.2 启用步骤**——但失败原因不是"env var 已确认坏",
是"我们没有任一确凿证据它仍按原 spec 工作,而 CLI 路径明显已退化"。

提交三种备选,**等用户拍板再继续**:

### 备选 A:**保留 schema,放弃启用**(推荐)

- M2.2 schema 部分(M2.3 已落):`parallelism: agent_team` 在
  `validate_m0` 通过(只要 `agent_team[]` 非空),YAML 字段保留。
- M2.2 启用部分:**砍掉**。dev `implement` phase 继续 `parallelism: solo`,
  3 角色 prompt 留给 phase markdown 自行用文字描述(claude 角色扮演,
  非真并行)。
- 后续 milestone 在更新 `claude --version` 后重新 spike;通过则只改
  YAML 一行。

### 备选 B:模拟 multi Task() 替代 agent_team

- orchestrator 把 phase YAML `agent_team[]` 翻译成多个 Task() 调用,
  顺序或并发 fan-out。
- **缺点**:违反 user prompt §2 的 "agent_team 失败立即 escalate,不要
  自己绕"红线;ccteam-core 多一处 team 名映射代码。
- **不推荐**,但如果用户愿意接受这种降级,M2.2 启用可在此基础上做。

### 备选 C:整体推迟到 M3+

- M2.2 整个任务推到 M3 团队抽象之后再处理 —— 那时 `team.yaml` 落地,
  agent_team 配置从 ccteam-core 迁出,实施压力小一些。
- **缺点**:M2 本来想兑现的 "速度并行(痛点 13)"在 dev pipeline 上零进展。
- 取舍较平衡,但需要 development-plan reorder。

## 仍需用户决定的具体问题

1. 是否同意本 spike 不实测真 claude 子进程(因为不可控副作用)?如果用户
   要求实测,**请提供一段非污染的环境**(独立 tmux session + 用户登录
   过的 claude),我可以走 hello-world 三角色路径。
2. **A / B / C 选哪个?** 目前 schema 已落地(M2.3 把 `parallelism:
   agent_team` 校验改为"只检查 roles 非空");选 A 影响 0,选 B 影响
   ccteam-core,选 C 影响 development-plan。
3. 如果选 A 但仍想保留 M2.2 启用路径的占位,是否同意把 implement.md
   的 `parallelism` 留 `solo`,但加一个 commented-out section 写
   "M2.2-pending: switch to agent_team after spike confirms"?

## 本 PR 实际产出

- **schema 部分**(M2.3 commit `7314787`):`parallelism: agent_team`
  通过 validation,只要 `agent_team[]` 非空 — 已 ship。
- **启用部分**:**未 ship**,等用户对上面问题 1–3 的答复。
- spike 报告(本文件):写出供未来回看。

— Claude Opus 4.7 (1M context),2026-05-06
