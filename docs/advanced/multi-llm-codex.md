# Multi-LLM with Codex — Advanced Guide

> **Audience**:已经会用 ccteam,想理解 Codex 在 gateway 里怎么跑、双 vendor 怎么调优、cross-vendor 有哪些物理限制的 power user。新手不必读 —— 日常只要 `/new claude` / `/new codex` 就够。

---

## 1. Codex 的定位:一等公民 vendor

ccteam gateway 同时驱动真实的 Claude Code 和真实的 Codex。Codex **不是隐藏开关、也不是静默兜底**,而是你在建 session 时直接选的执行后端:

```text
/cd demo-app
/new claude reviewer        # Claude tmux TUI session
/new codex api              # Codex app-server session
@reviewer 总结风险
@api /review
```

两个 vendor 用各自最合适的 harness,可在同一个 chat 里并发存在:

| Vendor | Harness | 说明 |
|---|---|---|
| Claude | tmux TUI session | `/compact`、`/clear` 按字面发给 TUI;`--dangerously-skip-permissions`,无批准门。 |
| Codex | app-server JSON-RPC | resident session 走 `thread/start` + `turn/start`;`/compact` 走 `thread/compact/start`;`/review` 走 `review/start`。 |

此外还有一条 **one-shot Codex 路径**:`codex exec --json`,用于 advise / critic 这类「问一次、拿一个答案」的场景(见 §3、§4)。它不开 resident session,跑完即退。

> daemon 是 IM⇄session 路由网关,不跑自治编排循环。下文凡涉及「自动判定 vendor 后自治执行」的能力,都依赖尚未启用的 flow 编排,本文明确标 **(deferred-flow)**。

---

## 2. 用户可见的 Codex 场景

| 场景 | 入口 | Codex 角色 | 状态 |
|---|---|---|---|
| **A. Parallel voting** | `/ccteam-advise vote\|parallel "<question>"`(MCP `advise_vote` / `advise_parallel`)| Claude + Codex 同 prompt 并发,合成 verdict(vote)或 N 条原始答案(parallel)| **当前可用** |
| **B. Auto-critic 选型** | `ccteam-creator` skill 生成 workflow.yaml 时 | 当 role ∈ {critic / reviewer / architect / code-reviewer} 且 `ccteam doctor --check-codex-auto-critic` 返回 exit 0 → 自动写 `executor: codex` | 选型**当前可用**;自治执行该 critic 属 **(deferred-flow)** |
| **C. Quota fallback** | `~/.ccteam/preferences.toml` `fallback.on_claude_quota = "codex"` | Claude 报 quota → 同 spawn 改用 Codex 一次 | 偏好可写,执行逻辑住 flow 编排 → **(deferred-flow)** |
| **D. `/ccteam-team` second-opinion** | `/ccteam-team N "task ..."` skill | 当 `N ≥ 3` 且 codex 可用,自动加 1 个 Codex critic teammate | **当前可用**(skill-local,见下) |

**degrade 原则**:任何场景里 codex 不可用(binary 缺 / 未登录 / quota)→ 静默退回 Claude-only,verdict / 输出里注明 `Codex unavailable: <reason>`,**不报错、不挂住**。

#### A / D 的真实执行路径

- **A(advise)** 由 daemon MCP 工具 `advise_vote` / `advise_parallel` 实现:Claude advisor 跑 `claude -p <prompt> --output-format text --dangerously-skip-permissions`,Codex advisor 跑 `codex exec --json -`(prompt 走 stdin),合成器再跑一次 Claude 出 verdict。`vote` 出一条合并 verdict + agreement 分类(agree / partial / disagree / unknown);`parallel` 出 N 条原始答案不合成。
- **D(`/ccteam-team`)** 是 **Claude session-local skill**,用 Anthropic 原生 `TeamCreate` + `Task`,不经 ccteam gateway / 编排。Codex critic teammate **不**走 `TeamCreate`:优先经 daemon-routed `advise_vote` / `advise_parallel`(daemon 在跑时);无 daemon 时 fallback 到 skill body 直接 `codex exec --json` bash spawn。`N < 3` 不自动加 critic。

---

## 3. 什么时候用 Codex / Claude / 混用

`ccteam-creator` 自动按下表决策(写进 `workflow.yaml::agents.<role>.executor`)。手编 yaml 的高级用户可参考:

| 场景 | 推荐 vendor | 理由 |
|---|---|---|
| 长链 reasoning(architect / planner / hard debug)| **Codex** | reasoning token 单独计费,长 CoT 强项 |
| 编辑大量代码(executor / fixer / multi-file edit)| **Claude** | tool use 稳定 + cache 显著省 token |
| 长跑 chat bot | **任一** | Claude 走 tmux TUI;Codex 走 app-server resume |
| Multi-advisor 投票(同 prompt 多视角)| **混合** | 见场景 A |
| Critic / code-reviewer / architect role | **Codex**(creator 自动)| 长链 reasoning 出二阶意见更有价值 |
| Anthropic-only 合规 | **Claude only** | doctor 不强求 codex |
| OpenAI-first 合规 | **Codex first** | role 直接写 `executor: codex` |

---

## 4. 接入 Codex

仅当你要用任一 Codex 场景时必需:

```bash
codex --version    # binary 在 PATH(`CCTEAM_CODEX_BIN` 可覆盖路径)
codex login        # ChatGPT plan / API key / device code 任选
```

Codex 配置在 `~/.codex`(`CODEX_HOME` 可覆盖)。resident session 走 app-server:gateway 默认连
`$CODEX_HOME/app-server-control/app-server-control.sock`;也可用 stdio transport:

| Env | 用途 |
|---|---|
| `CCTEAM_CODEX_BIN` | 覆盖 Codex CLI 路径(测试 / 多版本)。 |
| `CCTEAM_CODEX_APP_SERVER_SOCKET` | 指向 Codex app-server UDS。 |
| `CCTEAM_CODEX_APP_SERVER_TRANSPORT=stdio` | 改用 `codex app-server` stdio transport 而非 UDS。 |

gateway **不**自己拉起 app-server daemon;启动 Codex session 前确认 `codex app-server` 可用(`ccteam doctor --check-codex-auth` 会先验,失败在启动前报,不等 spawn 才挂)。

---

## 5. `ccteam doctor` 检 Codex

默认 `ccteam doctor` 不强求 codex(全 Claude workflow 不 nag)。显式查:

```bash
ccteam doctor --check-codex-version       # binary 存在 + 版本可探
ccteam doctor --check-codex-auth          # codex 登录态 ok
ccteam doctor --check-codex-auto-critic   # 跑一次 codex exec --json canary,验 auto-critic 选型可用
ccteam doctor --check-cost-orphan         # 对账 progress.jsonl 与成本 ledger
ccteam doctor --verify-mcp                # MCP 工具表面 0 STUB(CI gate)
```

`--check-codex-auto-critic` 退出码(供 `ccteam-creator` 决策):

- **0** = codex 可用 + canary 输出格式良好 → creator 给 critic role 自动写 `executor: codex`
- **2** = binary 缺 / 版本探测失败 → 不注入,纯 Claude
- **3** = 输出格式异常(codex 协议漂移)→ silent fallback,不注入

`--check-codex-auth` / `--check-codex-version` 缺失或失败时,doctor 报告里以 WARN 行呈现;若 workflow.yaml 已有 role 写了 `executor: codex` 但 codex 未登录,session 启动前就 abort,不静默。

---

## 6. `workflow.yaml` `executor:` 字段(advanced)

`workflow.yaml` 由 `ccteam-creator` 自动生成,vendor 由系统写入。advanced 用户可手编:

```yaml
agents:                           # 按 role 名为 key 的 map(非 list)
  planner:
    executor: claude              # 省略即默认 claude
    model: claude-opus-4-7
    trigger: watch                # trigger 必填

  critic:
    executor: codex               # 用 Codex 长链 reasoning 出二阶意见
    model: o3
    trigger: watch
```

#### 字段说明

| 字段 | 默认 | 取值 |
|---|---|---|
| `executor` | `claude` | `claude` \| `codex`(省略 = claude;Codex 显式 opt-in)|
| `model` | vendor 默认 | vendor-specific 模型名(见 §7 pricing 表里收录的名字)|

> sandbox / approval / reasoning-effort 不是 workflow.yaml 字段。Codex 的 sandbox 与 approval 由 `~/.codex` 配置和 IM 路径的 skip-permissions 语义决定,不在 ccteam 的 per-role schema 里。

---

## 7. Cost 透明 — 双 pricing table

| 文件 | 用途 |
|---|---|
| `crates/ccteam-cost/pricing/anthropic.toml` | Claude 各 tier(`claude-opus-4-7` / `claude-sonnet-4-6` / `claude-haiku-4-5` 等),含 cache 写入价 + 命中价 |
| `crates/ccteam-cost/pricing/openai.toml` | Codex 各 tier(`o3` / `o3-mini` / `gpt-4o` / `gpt-4o-mini`),reasoning token 按 output 价单独计费 |

`UnifiedTokenUsage` 是 superset,容两边字段;`progress.jsonl::agent_done.usage` 序列化此结构,`vendor` 字段区分 claude / codex。Codex app-server resident session 的 turn 边界(`turn/completed` / 失败)由 adapter 自动镜像成 `agent_done { vendor: "codex" }` 行写进 `progress.jsonl`,成本汇总按 vendor 滚动,无需另起 poller。

`ccteam doctor --check-cost-orphan` 对账 24h 内 `agent_done` event 数与成本 ledger 行数;不对账即某 spawn 路径绕过记账。

> **记账面有两个,尚未完全合一**:advise 路径(场景 A)把近似成本记进 `<ccteam_root>/cost-budget.json::advise_today_usd`;resident session 记进 `progress.jsonl::agent_done`。两者各自正确,但**不是同一个文件**。**UNSURE**:advise 的 `cost-budget.json` 与 resident 的 `progress.jsonl` 滚动汇总是否在某一层合并出单一「跨 vendor 同账」数字,代码里没看到收敛点,这里不下断言。

#### Per-vendor budget cap

workflow.yaml 支持 per-vendor 24h 成本上限(系统生成,默认 claude $5 / codex $2):

```yaml
budgets_v060:                     # per-vendor 拆分(优先于扁平 budget: 字段)
  claude:
    max_cost_usd_per_24h: 5.0
  codex:
    max_cost_usd_per_24h: 2.0
```

触底行为:任一 vendor 触顶 → workflow 整体停用(红线),**不**自动切另一 vendor(避免行为漂移)。advise 场景例外:某 vendor 撞 cap → 静默跳过该 vendor,跑其余,verdict 标注。

---

## 8. Cross-vendor 限制(物理事实)

| 跨 vendor 场景 | 可行性 | 原因 |
|---|---|---|
| **同一 Claude session 把 Codex 作 in-proc teammate** | ✗ **永远不可能** | Anthropic `TeamCreate` 的 worker 只能是 Claude session;Codex 同理只能 spawn Codex 子 thread。没有任何技术路径让一个 Claude session 把 Codex 当 in-proc teammate。 |
| **同一 chat 并发 Claude + Codex session** | ✓ | gateway 用各自 adapter 管各自 session;`/new claude` / `/new codex` 并存,`@handle` 路由,输出统一回 IM。 |
| **Parallel advisor 跨 vendor** | ✓ | 场景 A,各 vendor 跑一次 one-shot,合成器汇总。 |
| **Auto-fallback Claude↔Codex(同任务自动切)** | ⚠ (deferred-flow) | 偏好可写;两边 system prompt / cost model 不同,自动切会让 debug 找根因变难 —— 执行逻辑住尚未启用的 flow 编排。 |

#### `/ccteam-team` 的近似「混队」

`/ccteam-team`(场景 D)在一个 Claude session 内当 lead,用 `TeamCreate` + `Task` 起 Claude teammates;Codex critic **不**进 `TeamCreate`,而是旁挂一条 `advise_vote` / `codex exec` 通道做 second-opinion。所以你拿到的是「Claude in-proc team + 一个旁路 Codex 评审」,**不是**真正的 in-proc 混合队伍 —— 后者物理不可能。

---

## 9. 跨项目记忆与 vendor 隔离

红线「跨项目记忆走官方接口」对两个 vendor 各走各的官方路径,ccteam 不写第三份:

| 文件 | Vendor | 内容 |
|---|---|---|
| `~/.claude/CLAUDE.md` + `<repo>/CLAUDE.md` | Claude | Claude Code 用户级 / 项目级指令 |
| `~/.codex/AGENTS.md` | Codex | Codex 用户级指令 |

> **UNSURE / 待核实**:`ccteam init` 当前布局写 `.ccteam/{agents,skills,state.json}` + `.claude/agents`,**没看到**它自动创建 `<repo>/AGENTS.md`(无论 symlink 还是副本)。若要让 Codex bot 读到项目规则,目前需要你自己在仓库放 `AGENTS.md`。原文里 `ccteam init` 落 `AGENTS.md → CLAUDE.md` symlink + `scripts/sync-agents-md.sh` 同步的描述,在当前代码里未实现,已移除。

---

## 10. 常见陷阱

| 陷阱 | 症状 | 解 |
|---|---|---|
| **Codex 未登录** | `/new codex` 后 `gateway error: ...` | `codex login`;`ccteam doctor --check-codex-auth` 先验 |
| **app-server socket 断** | Codex session `gateway error` | 确认 `codex app-server` 可用;必要时重启 daemon |
| **reasoning token 单独计费** | reasoning 模型跑多轮账面比 Claude 贵 | 监控 `progress.jsonl::*.usage` 里 reasoning 占比;critic 用低 effort 模型 |
| **Codex CLI 形态漂移** | codex 升级后 flag / 协议变 | `CCTEAM_CODEX_BIN` 钉版本;adapter 对未知 notification 前向兼容(跳过 + warn 一次)|
| **期望 in-proc 混队** | 「为啥 `/ccteam-team` 不能起 Claude + Codex 混合 in-proc 队伍」 | 物理限制:同 chat 多 session(`/new claude` + `/new codex`)或 advise 旁路可近似;同 session in-proc 永不可能 |

---

## See also

- `docs/user-manual.md` —— chat / project / session 模型 + 双 harness 日常用法
- `docs/orchestration-patterns.md` —— 拓扑 × 执行模式适用矩阵
- `docs/tech-design.md` —— HarnessAdapter trait + vendor enum
- `docs/interfaces.md` —— workflow.yaml schema + MCP tool surface + CLI
- `CLAUDE.md` 红线表 —— vendor 下的措辞
