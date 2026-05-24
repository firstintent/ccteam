# Multi-LLM with Codex — Advanced Guide

> **Audience**:已经会用 ccteam,想理解 Codex 集成怎么工作 + 怎么定制 / 双 vendor workflow 调优 / cross-vendor 限制的 power user。新手不必读 — Codex 在默认场景下对你**完全透明**。

---

## 1. Codex 的定位:advanced 隐藏开关

ccteam 的"低门槛 / Claude-Code-native UX"原则要求**用户从不直接选 vendor**。Codex 不是 first-class user-facing executor,而是**在 4 个明显占优场景静默接入**;其他场景默认全 Claude。

#### 4 个用户可见场景

| 场景 | 入口(用户面)| Codex 角色 | degrade |
|---|---|---|---|
| **A. Parallel voting** | `/ccteam-advise vote\|parallel "<question>"` skill(`mcp__ccteam__advise_vote` / `advise_parallel`)| Claude + Codex 同 prompt 并发跑 advisor,合成 verdict(vote)或 raw dump(parallel)| codex 缺 / quota → 静默 Claude-only,verdict prose 说 "Codex unavailable: <reason>",**不报错** |
| **B. Auto-critic** | `ccteam-creator` skill phase 内 | 当 role ∈ {critic, reviewer, architect, code-reviewer} 且 `ccteam doctor --check-codex-auto-critic` 返回 exit 0 → 自动赋 `vendor: codex`(yaml 内部) | codex 不可用 → 全 Claude,yaml 不写 vendor 字段 |
| **C. Quota fallback** | `~/.ccteam/preferences.toml` `fallback.on_claude_quota = "codex"` | Claude 报 quota / 5xx → 同 turn 重试 Codex 一次 | 默认 off;meta-agent 在用户抱怨 quota 时主动提议 opt-in |
| **D. `/ccteam-team` second-opinion** | `/ccteam-team N "task ..."` skill | in-proc team 自动加 1 个 Codex critic teammate(if codex 可用) | codex 不可用 → 全 Claude,team size 不变 |

**所有其他场景**(qa-loop / autonomous-orchestrator / chat-bot / 通用 executor / planner / fixer)默认 Claude。Codex 不出场。

**实现路径**:advise 场景由 `crates/ccteam-core/src/advise.rs` 实现 `advise_vote` / `advise_parallel` 两个公开 fn,各自并行 spawn 一个 advisor per vendor(Claude 走 claude-tui adapter / Codex 走 `CodexExecAdapter`),合成器读 `<ccteam_root>/cost-budget.json` 做 per-vendor cap enforcement。auto-critic 场景由 `ccteam-creator` Phase 3.5 调 `ccteam doctor --check-codex-auto-critic` 子进程(exit 0/2/3 三态)决定是否写入 vendor 字段。**Critic 路径(场景 B/D)** 统一走 daemon-routed `CodexExecAdapter`,`submit_turn` post-turn 自动 append ledger row,与 advise 同账 ── 跨 vendor 调用都在同一账上,旧的 bash 直跑 `codex exec` 路径 deprecated;`ccteam doctor --check-cost-orphan` 守这条 invariant。

---

## 2. 什么时候用 Codex / Claude / 混用

ccteam-creator skill 内部按下表自动决策。手编 `workflow.yaml` 的高级用户可参考:

| 场景 | 推荐 vendor | 理由 |
|---|---|---|
| 长链 reasoning(architect / planner / hard debug)| **Codex** o-series / gpt-5.2-codex | `reasoning_output_tokens` 单独计费,长 CoT 强项;sandbox 默认严格 |
| 编辑大量代码(executor / fixer / multi-file edit)| **Claude** Opus 4.x / Sonnet | tool use 稳定性 + ephemeral 1h cache 显著省 token |
| 长跑 chat bot(mode 3)| **Claude**(目前) | `claude -p --resume` stream-json + cache hit turn 2 起;Codex chat 在 backlog |
| Multi-advisor 投票(同 prompt 多视角)| **混合** | Claude + Codex 各跑一次,合成"双方同意 / 冲突 + 选谁"— 见场景 A |
| 严格 sandbox + approval workflow | **Codex** | sandbox + approval_policy 是 first-class config;Claude 只能靠 hook 模拟 |
| Critic / code-reviewer / architect role | **Codex**(自动)| 长链 reasoning 强,二阶意见更有价值 |
| Anthropic-only 合规客户 | **Claude only** | doctor 不 nag 不装 codex |
| OpenAI Enterprise 合规客户 | **Codex first** | 反向同上;workflow.yaml 模板可 default vendor=codex |

---

## 3. Prerequisites

仅当用户开启场景 A/B/C/D 任一时必需:

```bash
codex --version    # ≥ 0.45(`--experimental-json` API 稳定 baseline)
codex login        # ChatGPT plan / API key / device code 任选(Codex 4 路 auth)
codex account      # 探活,确认 auth ok
```

`~/.codex/config.toml` 推荐 preset(`ccteam doctor --install-codex-profile` 自动落):

```toml
[profiles.ccteam-default]
sandbox = "workspace-write"      # 写权限限 repo 内
approval_policy = "on-failure"   # 默认信任,失败再问
model = "gpt-5.2-codex"
model_reasoning_effort = "medium"
```

ccteam 内部跑 codex 用 `--profile ccteam-default` 引这套;**用户不必手编 sandbox/approval 字段**。

---

## 4. `ccteam doctor` 检 Codex

默认 doctor 不强求 codex(全 Claude workflow 不 nag)。显式查:

```bash
ccteam doctor --check-codex                  # binary 存在 + auth ok
ccteam doctor --check-codex-auto-critic      # 跑一次 codex exec --json canary,验 auto-critic 路径可用
ccteam doctor --install-codex-profile        # 落 ~/.codex/config.toml [profiles.ccteam-default]
ccteam doctor --check-cost-orphan            # 对账 24h ledger 与 progress.jsonl(catch 漏 ledger 的 spawn 路径)
ccteam doctor --verify-mcp                   # 自检 MCP 工具表面 0 STUB(CI gate)
```

`--check-codex-auto-critic` 退出码:
- **0** = codex 可用 + canary 输出格式良好(`turn.completed` JSONL 解析通过)→ `ccteam-creator` 给 critic role 自动注入 `vendor: codex`
- **2** = binary 缺 / 版本探测失败 → 不注入,纯 Claude
- **3** = output 格式异常(codex 升级后协议漂移)→ silent fallback,不注入

doctor 输出语义:

```
[ok]   claude --version: 2.0.x
[ok]   codex --version: 0.45.x
[ok]   codex auth: user@example.com
[ok]   ~/.codex/config.toml profile ccteam-default present
[info] workflow.yaml: 2/5 agent vendor=codex (auto-assigned by ccteam-creator)
```

workflow 含 `vendor: codex` 但 codex auth 缺 → **启动前**报错,不等到 spawn 才挂。

---

## 5. 手动启用 Codex preferences

`~/.ccteam/preferences.toml` 是 advanced 偏好文件,**不让用户直接 vim**;走 meta-agent 或 skill 命令:

```
# 在 Claude session 里
/ccteam prefs fallback.on_claude_quota codex
/ccteam prefs default.critic_vendor codex
/ccteam prefs codex.profile ccteam-default
```

写入后:

```toml
# ~/.ccteam/preferences.toml(skill 生成,不要手编)
[fallback]
on_claude_quota = "codex"        # 场景 C 启用

[default]
critic_vendor = "codex"          # 场景 B 强制(默认 = auto-detect)

[codex]
profile = "ccteam-default"       # codex --profile flag 用此名
```

**preferences 是 user-global**(`~/.ccteam/`),不进 git,不污染项目。

---

## 6. `workflow.yaml` `vendor:` 字段(advanced)

`workflow.yaml` 由 `ccteam-creator` skill 自动生成,vendor 字段系统写。**advanced 用户**可手编(承认你愿意维护 yaml):

```yaml
version: 0.6
mode: bg
agents:
  - role: planner
    vendor: claude                   # claude | codex | auto
    model: opus-4-7

  - role: critic
    vendor: codex                    # 用 Codex o-series 长链 reasoning
    model: gpt-5.2-codex
    codex_sandbox: read-only         # 只 vendor=codex 适用
    codex_approval: never            # critic 只读,无 approval
    codex_reasoning_effort: high

  - role: executor
    vendor: claude
    codex_sandbox:                   # 写了 schema validate 报错(vendor 不匹配)
```

#### 字段说明

| 字段 | vendor | 默认 | 取值 |
|---|---|---|---|
| `vendor` | both | `claude` | `claude | codex | auto`(`auto` = ccteam-creator 决策) |
| `model` | both | vendor 默认 | vendor-specific 名 |
| `codex_sandbox` | codex only | `workspace-write` | `read-only | workspace-write | danger-full-access` |
| `codex_approval` | codex only | `on-failure` | `untrusted | on-failure | on-request | never` |
| `codex_reasoning_effort` | codex only | `medium` | `low | medium | high` |

`codex_*` 字段在 `vendor: claude` 下出现 → schema validate 报错(明确错误,不静默忽略)。

---

## 7. Cost 透明 — 双 pricing table

#### 内部架构

| crate / file | 用途 |
|---|---|
| `crates/ccteam-cost/pricing/anthropic.toml` | Claude 各 tier 定价(`opus-4-7` / `sonnet-4-5` / `haiku-4-5`),含 `cache_creation_input_tokens` / `cache_read_input_tokens` 写入价 + 命中价 |
| `crates/ccteam-cost/pricing/openai.toml` | Codex 各 tier 定价(`gpt-5.2-codex` / `o3` / `o3-mini`),含 `reasoning_output_tokens` 单独计费 |
| `crates/ccteam-cost/src/budget.rs` | `UnifiedTokenUsage` enum + per-vendor budget cap |
| `<ccteam_root>/cost-budget.json` | per-vendor ledger(48h 自动 GC,atomic-rename 写入)── advise / Codex critic / `CodexExecAdapter::submit_turn` **统一**写入,跨 vendor 同账 |

`UnifiedTokenUsage` 是 superset,容两边字段;`progress.jsonl::agent_done.usage` 序列化此结构。

**Cost-orphan invariant**:`ccteam doctor --check-cost-orphan` 在终端跑(或 CI nightly job),对账 24h 内 `agent_done` event 数与 ledger row 数 ── 不对账即某 spawn 路径绕过 ledger(自加 custom adapter 漏接 hook / 外挂 bash 直跑 codex)。健康输出 `[ok] claude: N agent_done vs N ledger rows` per vendor,WARN 列差额。Codex critic 调用走 daemon-routed `CodexExecAdapter`,与 main spawn 同账,**不再** silent 漏算。

#### Per-vendor budget cap

```yaml
# workflow.yaml(系统生成,user 不见)
budgets:
  claude:
    max_cost_usd_per_24h: 5.0
  codex:
    max_cost_usd_per_24h: 2.0
```

**触底行为**:任一 vendor 触底 → workflow 整体 `enabled: false`(红线),emit `vendor_budget_exhausted { vendor }` event;**不**自动切另一 vendor(避免行为漂移)。advise 场景例外:某 vendor 撞 cap → 静默跳过该 vendor 跑其余,verdict 标注 `budget_exhausted`。

#### 用户面展示(MCP `workflow_show_cost`)

```
$ /mcp ccteam workflow show-cost
Total: $1.50 / day
  claude: $1.00 (Opus 4.7 main x 3 hour, Sonnet critic x 30min)
  codex:  $0.50 (o3-mini critic x 1 hour, reasoning 30% of cost)
```

聚合数 + 分 vendor 详情,user 看 1 行;web UI 同模式渲染。

---

## 8. AGENTS.md vs CLAUDE.md

| 文件 | Vendor | 内容 |
|---|---|---|
| `<repo>/CLAUDE.md` | Claude | Claude Code 项目级指令(已有) |
| `<repo>/AGENTS.md` | Codex | Codex 项目级指令(本身格式与 CLAUDE.md 同) |

**ccteam init** 落 `AGENTS.md` 作 POSIX symlink → `CLAUDE.md`:

```bash
ln -s CLAUDE.md AGENTS.md         # ccteam init 默认行为(Linux / macOS)
```

Windows 无 POSIX symlink → 副本 + git pre-commit hook 守同步(`scripts/sync-agents-md.sh`)。doctor 检测 drift,落 finding。

**用户改 CLAUDE.md 自动反映到 AGENTS.md**(symlink),Codex bot 看到同一份项目规则。"跨项目记忆走官方接口"红线扩到"两边官方路径,ccteam 不写第三份"。

---

## 9. Cross-vendor 限制(物理事实)

| 跨 vendor 场景 | 可行性 | 原因 |
|---|---|---|
| **模式 1 in-proc team(同 parent session)** | ✗ **永远不可能** | Anthropic 官方 `TeamCreate` 文档明示 "workers are Claude sessions";Codex 同理只能 spawn Codex 子 thread。**没有任何技术路径**让一个 Claude session 把 Codex 作 in-proc teammate |
| **模式 2 bg sessions 跨 vendor** | ✓ | ccteam orchestrator 是外置中转;`.ccteam/inbox/*.md` 触发 fresh `claude --bg` / `codex exec`;`progress.jsonl::agent_done.vendor` 字段区分 |
| **模式 3 chat bots 跨 vendor**(同 TG 群)| ✓ | `ClaudeStreamJsonAdapter` + `CodexAppServerAdapter` 各管各 bot;Codex chat bot 已可启用 |
| **Auto-fallback Claude↔Codex**(同任务自动切)| ⚠ opt-in only | 场景 C,默认 off;两边 system prompt 不同 + sandbox 不同 + cost model 不同,**自动切换会让 debug 极难找根因** — 用户显式开 |

#### Cross-vendor 桥协议(mode 2 跨 vendor 时)

- **SendMessage**:走 orchestrator(写 `.ccteam/inbox/<recipient>/msg-*.md`);**不**直接传 Claude `SendMessage` tool call 给 Codex
- **cost 聚合**:`UnifiedTokenUsage` + 双 pricing,progress event `cost_update` 带 `vendor`
- **observability**:`progress.jsonl::*.vendor` 必填,web UI 按 vendor 着色 + 分组
- **memory 隔离**:`~/.claude/CLAUDE.md` 给 Claude bot,`~/.codex/AGENTS.md` 给 Codex bot,**ccteam 不混写**(同 §8 symlink 策略)

---

## 10. 5 编排 × 3 执行 × 2 vendor 矩阵(Codex 列摘要)

详细矩阵在 `docs/orchestration-patterns.md` §五。Codex 列(对照 Claude 列)摘要:

| Cell | Codex 列 | 注 |
|---|---|---|
| Chaining × mode 1 × Codex | ⚠ | Codex 原生 `spawn_agent` 但 ccteam-team skill 是 Claude session-local — 跨 vendor in-proc 不可行 |
| Chaining × mode 2 × Codex | ✓ | `codex exec` artifact-driven,行为对齐 Claude |
| Chaining × mode 3 × Codex | ✓ | `CodexAppServerAdapter` UDS bridge |
| Routing × all × Codex | ⚠ | Codex routing config-driven(role.toml 选模型),跟 Claude SKILL.md prompt-driven routing **不等价**;用户面统一走 `ccteam-creator` 自动选,内部 routing 各管各 |
| Parallel-vote × mode 1 × Codex | ✓ | 场景 A `/ccteam-advise` 头条用例 |
| Parallel-segment × mode 2 × mixed | ✓ | git worktree per agent,vendor 标在 progress event |
| Orchestrator-Worker × mode 3 × Codex | ✓ | 同 chaining mode 3 |
| Evaluator-Optimizer × mode 2 × Codex | ✓ | critic 角色自动接 Codex(场景 B) |
| Evaluator-Optimizer × mode 1 × mixed | ✗ | in-proc 跨 vendor 物理不可能 |
| **In-proc 跨 vendor 任意 cell** | ✗ **永远** | 物理不可能 |

---

## 11. 常见陷阱

| 陷阱 | 症状 | 解 |
|---|---|---|
| **Codex sandbox 默认 read-only** | bot 想写文件失败 "permission denied",静默 | workflow.yaml 显式 `codex_sandbox: workspace-write`;Claude 的 `--dangerously-skip-permissions` **无 Codex 等价** — 必须配 sandbox + approval=never 才同效果 |
| **`reasoning_output_tokens` 单独计费** | `gpt-5.2-codex` o-series 跑 fix-loop 3 次 bill 看起来比 Claude 贵 3x | `medium` reasoning effort 已是默认;`high` 只在 architect role 用,不在 executor;监控 `progress.jsonl::*.usage.reasoning_output_tokens` 比 input/output 还大就是 effort 配错 |
| **AGENTS.md 不读 → bot 行为漂移** | Codex bot 不知道项目规则,Claude bot 知道 | doctor 检测 `<repo>/AGENTS.md` 存在且 symlink 到 CLAUDE.md;CI 加 trufflehog 扫 AGENTS.md / CLAUDE.md drift |
| **Codex CLI 形态漂移激进** | `codex exec` 升级后 flag 改、`--experimental-json` API 变 | `CCTEAM_CODEX_BIN` env override(同 `CCTEAM_CLAUDE_BIN`);pin `codex >= 0.45`,任何 codex breaking 升 ccteam patch 版兜底 |
| **OpenAI auth 比 Anthropic 复杂** | `codex login` 4 路(ChatGPT plan / API key / device code / access token)首次失败率高 | doctor 强制 `codex account` 探活;workflow.yaml validate `vendor: codex` 但 auth 缺 → 启动前 abort |
| **session-id 命名空间冲突** | Claude bot 和 Codex bot 同 workflow,session-id 文件互覆盖 | 内部路径 `~/.ccteam/im/<workflow>/<bot>/<vendor>/session-id` 强制 vendor 段 |
| **cross-vendor in-proc 期望** | 用户问"为啥 `/ccteam-team` 不能起 Claude + Codex 混合 in-proc 队伍" | 物理限制:跨进程通过 mode 2 `.ccteam/inbox/` 桥可实现近似效果,但 in-proc(同 session)永不可能 |

---

## See also

- `docs/orchestration-patterns.md` §五 — 5 拓扑 × 3 执行模式适用矩阵全表
- `docs/tech-design.md` §2.1 / §3.3 — HarnessAdapter trait + vendor enum
- `docs/interfaces.md` — workflow.yaml schema + MCP tool surface + CLI lifecycle
- `CLAUDE.md` §三 红线表 — 模式 × vendor 下的措辞
