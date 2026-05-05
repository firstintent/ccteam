# ccteam 技术实现方案

> 本文档基于 [requirements.md](./requirements.md)（已确认的用户痛点），参考 [gstack-auto](https://github.com/loperanger7/gstack-auto)（短期对标）与 [OpenAI Symphony](https://github.com/openai/symphony)（长期对标），以 Claude Code 为执行 agent，给出 ccteam 的技术架构、组件分解、数据协议、扩展点映射与里程碑路线。
>
> **核心问题**：用户用一句自然语言提需求，系统自动产出可运行软件——且**不需要主对话窗口在线**、**多项目自动排队**、**测试不过不交付**、**经验跨项目沉淀**。

---

## 1. 设计原则

每条原则都直接挂钩 `requirements.md` 中的某条痛点。

| 原则 | 对应痛点 | 落地约束 |
|---|---|---|
| **守护进程化（daemonize）** | 痛点 9：AI 团队需要人来主持 | Orchestrator 独立于任何 Claude Code 主对话，作为 systemd / cron 长跑进程 |
| **文件即状态机** | 痛点 7：进度永远不透明 | 一切状态可从文件系统恢复；进程重启不丢任务 |
| **长 session 优先** | 痛点 8/9 + cache 复用降成本 | 每项目一个 tmux + `claude --dangerously-skip-permissions` 长 session；不用 `-p`，phase 切换靠 send-keys 注入；同 session 跨 phase 共享 prompt cache |
| **三态 Seed Gate** | 痛点 6：不是每个想法都值得做 | 每个项目先过 PASS / REJECT / CLARIFY；REJECT 不下场，节省精力 |
| **测试即验收** | 痛点 3：测试和质量是黑洞 | 自动化 fix loop 收敛后跑 test，全绿才标记 done；无主观 review |
| **3-Strike 自愈再升级** | 痛点 4：bug 修复无限循环 | 失败先自己修 N 轮，撞顶才升级用户，并附"试过什么 / 卡在哪" |
| **跨项目沉淀** | 痛点 10：每个新项目从零开始 | 全局 `~/.ccteam/memory/` + RAG 检索，新项目 Seed 阶段自动召回 |
| **零交互沙盒** | 痛点 8：每一步都点允许 | 项目级 Docker / 容器隔离 + 全放行 settings.json |
| **决策点 ≤ 3** | 痛点 2：AI 仍要求我当 PM | 只有不可逆决策（架构、scope 大改、API 形态）才走 escalation |
| **纵深防御替代人值守** | 痛点 11：关键节点不把控 | L1 架构约束（hooks + required_outputs）+ L2 多 agent 互检 + cross-cutting watcher（议事）+ L3 用户兜底（仅 deadlock 弹）；详见 §3.6 |
| **pipeline 编排 sub-skill** | 痛点 12：工作流插件靠人手动调 | 9 主干 phase + 每 phase front matter `sub_skills` 字段；orchestrator 自动 trigger，产物自动接力；复用 gstack / claude-plugins-official 的 plugin，不重写；详见 §6.10 |
| **并行规模自适应** | 痛点 13：大项目串行慢、并行规模选不对 | plan-eng 按 spec 复杂度选 `parallelism: solo / agent_team / multi_session`；subagent 任何粒度可叠加（ad-hoc，不在协议中声明）；三档叠加层级，不是互斥；详见 §3.3、§6.3、§6.11 |

---

## 2. 总体架构

### 2.1 进程拓扑

```
┌──────────────────────────────────────────────────────────────────┐
│  用户接入层（异步、非阻塞）                                       │
│  ┌──────────────┐  ┌────────────┐  ┌──────────────────────────┐  │
│  │ Telegram bot │  │ ccteam CLI │  │ 文件 inbox（手动 echo）  │  │
│  └──────┬───────┘  └─────┬──────┘  └──────────┬───────────────┘  │
└─────────┼─────────────────┼──────────────────┼──────────────────┘
          │                 │                  │
          │ 写入            │ 写入             │ 写入
          ▼                 ▼                  ▼
   ┌────────────────────────────────────────────────────┐
   │  ~/.ccteam/inbox/  （文件即消息队列）              │
   └─────────────────────┬──────────────────────────────┘
                         │ 轮询
                         ▼
   ┌────────────────────────────────────────────────────┐
   │  ccteam-orchestrator（守护进程，单点）              │
   │  - 30s 轮询 inbox                                  │
   │  - 单 GenServer 状态机：claim / dispatch / release │
   │  - 项目状态：seeded → planned → coding → ...        │
   │  - workspace 管理 + git worktree                   │
   │  - phase 调度 + fix loop 上限                      │
   └─────────────────────┬──────────────────────────────┘
                         │ tmux send-keys 注入 phase prompt
                         ▼
   ┌────────────────────────────────────────────────────┐
   │  Claude Code 执行实例（tmux 长 session，每项目一个）│
   │  tmux new-session -d -s ccteam-<slug>              │
   │      "claude --dangerously-skip-permissions"       │
   │  ─ orchestrator 用 send-keys 注入下一 phase prompt  │
   │  ─ 用户随时 `ccteam attach <slug>` 看 / 介入        │
   │  ─ 同 session 跨 phase 复用 prompt cache            │
   │  ─ 内部可启用 Agent Teams（dev / reviewer 等）      │
   │  ─ Hooks 把结构化进度写 progress.jsonl              │
   └─────────────────────┬──────────────────────────────┘
                         │ 文件 + git commit + progress.jsonl
                         ▼
   ┌────────────────────────────────────────────────────┐
   │  持久化层                                          │
   │  ~/projects/<slug>/    项目工作区（git）           │
   │  ~/projects/<slug>/.ccteam/  状态、artifacts        │
   │  ~/.ccteam/memory/     跨项目记忆（RAG 索引）       │
   │  ~/.ccteam/progress/   每项目结构化事件流（hooks）  │
   └─────────────────────┬──────────────────────────────┘
                         │ 完成 / 失败 / 需澄清
                         ▼
   ┌────────────────────────────────────────────────────┐
   │  通知层（Telegram MCP / 邮件 / push）               │
   │  仅在终态或 escalate 时触发                        │
   └────────────────────────────────────────────────────┘
```

### 2.2 关键架构决策

**为什么 Orchestrator 在 Claude Code 之外（不是 Agent Teams 的 Lead）？**

- Agent Teams 的 Lead 必须保持主对话存活，违反"关掉电脑也要跑"（痛点 9）。
- Lead 上下文压缩后会"失忆"——即便走 `team-snapshot.md` 恢复，也需要人触发。
- 长跑守护进程（Python / Node / Rust）原生支持 systemd / 重启自恢复，符合 Symphony "tracker-driven recovery" 思路。

**Agent Teams 仍然在每个 phase 内部使用**——例如"实现 + 评审"phase 启用 dev / reviewer 两个 sub-agent 并行。但 Lead 的角色是 phase 内的 team-lead，不是全局调度器。

**为什么用文件系统当控制平面，不是 Linear / GitHub Issues？**

- Symphony 选 Linear 因为其用户是企业团队，已有 issue tracker。
- ccteam 的用户是独立开发者，引入外部 tracker 增加摩擦。
- 文件协议零依赖、可审计、可备份。
- 真要外部 tracker，留作可选 adapter（M3+）。

**为什么用 tmux 长 session 而非每 phase 一个 `claude -p` 子进程？**

- **prompt cache 复用**：Anthropic prompt cache TTL 5 分钟，命中后费用降到约 10%。每 phase 起新子进程意味着重读 CLAUDE.md / spec / 上游产物，反复触发冷启动；同 session 跨 phase 共享缓存，长跑项目（数小时-数天）累计省下大量成本。
- **可观测性天然解决**：tmux 是终端 UI，用户 `ccteam attach <slug>` 立刻看到 Claude 在做什么；headless 子进程必须靠解析 stream-json 才能知情。
- **可中断 / 可注入**：用户 attach 后直接键入指令纠偏（"等等，先别用 SQLite"），Ctrl+C 中断推理；headless 模式没有这个能力。
- **detach 即守护**：tmux session 在用户断开后继续运行——这是 tmux 的本质优势，刚好契合"关掉电脑团队继续跑"。
- **不限制 max_turns / max_budget**：长 session 模式下不再硬封顶（理由见 §6.8）；只保留 stall 检测作为软告警。

**Trade-off**：长 session 上下文会膨胀，单 session 跑数日后可能逼近上限。应对见 §6.8 与 §8。

---

## 3. 核心组件

### 3.1 Inbox 与项目入队

**形态**：`~/.ccteam/inbox/<timestamp>-<random>.md`，每个文件就是一个想法。

**写入端**（多种异步入口）：
- **Telegram bot**：用户发消息 → bot 写文件
- **`ccteam new "做一个书签管理器"`**：CLI 直接写文件
- **手动 `echo` / 编辑器**：完全降级路径

**文件格式**：
```markdown
---
source: telegram        # 来源
user: rob
created_at: 2026-05-04T10:23:00Z
---

# 想法

做一个本地书签管理器，离线可用，按域名归类，支持搜索。
最好是 PWA，能装到手机。
```

**Triage**（orchestrator 第一步）：
1. 分配项目 slug（`bookmark-mgr-a3f9`）
2. 移到 `~/.ccteam/queue/seeding/<slug>.md`
3. 进入 Seed 阶段（见 §3.3）

### 3.2 Orchestrator 守护进程

**实现选型**：Python（asyncio）+ 单一长跑进程。理由：
- Python 生态成熟，易写文件协议处理
- asyncio 适合"轮询 + 子进程管理 + 多任务并发"
- 单进程拥有所有可变状态——抄 Symphony 的 "single GenServer" 思路，避免锁
- 启动成本低、单文件可读

**核心循环**（伪代码）：
```python
async def main_loop():
    state = State.load_from_fs()              # 启动时从文件恢复
    await reattach_tmux_sessions(state)        # 已存在的 session 直接续接
    while True:
        await poll_inbox(state)                # 新想法 → seeding 队列
        await ensure_session_started(state)    # 项目首启动 → 拉起 tmux + claude
        await dispatch_next_phase(state)       # idle-aware send-keys 注入（见 §6.9）
        await consume_progress_jsonl(state)    # inotify 读 hooks 写的事件流
        await detect_stall_and_warn(state)     # 5/15/30 min 三档软告警
        await detect_user_attach(state)        # 检测到人介入则暂停自动调度
        await reset_if_context_high(state)     # phase 边界 + ctx > 60% → reset session
        await maybe_notify_user(state)         # escalation / done
        await asyncio.sleep(30)
```

**状态机**（per project，参考 Symphony Run attempt）：

```
inbox
  └─ triage → seeding
       └─ Seed phase → 
            ├─ rejected   (终态，archive + 通知用户带理由)
            ├─ clarify    (问用户一个问题，等回答，重跑 Seed)
            └─ seeded → planning
                 └─ Plan phase → planned
                      └─ Dev phase → coding
                           ├─ tests-pass → reviewing
                           │    └─ Review phase →
                           │         ├─ approved → shipped (终态)
                           │         └─ blocked → coding (fix-loop, 上限 3)
                           └─ tests-fail → fixing
                                └─ Fix loop ≤ 3 轮 →
                                     ├─ tests-pass → reviewing
                                     └─ blocked → escalated (终态，通知用户)
```

**单点 + claim 防重**：claim 粒度是**项目级**（每项目一个 tmux session 一个 claude 进程）。state.json 记录 `tmux_session: ccteam-<slug>`、`claude_pid`、`phase_state: in_flight | idle | fix_locked`。

- `in_flight` = 已 send-keys 注入 phase prompt，等待 progress.jsonl 中出现 `phase_done` 或 `escalate` 事件
- `idle` = 上一 phase 完成，session 还活着但没在跑——orchestrator 可以注入下一 phase
- `fix_locked` = 当前在 fix-cycle 中，Stop hook 按 §3.5 的 ralph-loop 范式接管自循环；orchestrator **不**注入新 prompt，直到 progress.jsonl 出现 `phase_done`（测试绿）或 `escalate`（撞 3 次顶）

orchestrator 重启时：`tmux has-session` + `kill -0 <claude_pid>` 双重校验。session 还在 → 续接；进程不在 → 走 §6.1 的"极端情况——session 必须重启"路径用 `--resume` 恢复对话历史。

### 3.3 Phase Pipeline（短期对标 gstack-auto）

每个 phase 是一个 markdown 文件 + YAML front matter（抄 Symphony 的 WORKFLOW.md 形态）：

```
~/.ccteam/phases/
  00-seed.md             # 可行性评估（PASS/REJECT/CLARIFY）
  01-plan-ceo.md         # 产品规划
  02-plan-eng.md         # 技术规划
  03-implement.md        # 代码实现
  04-test-author.md      # 测试编写
  05-test-run.md          # 跑测试，输出报告
  06-fix.md              # 修 bug（在 fix loop 中循环）
  07-review.md           # 代码审查
  08-score.md            # 评分（6 维 + bug penalty）
  09-ship.md             # 提交、产文档、收尾
```

**phase 文件格式**：
```markdown
---
name: implement
required_inputs:
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
required_outputs:
  - src/**/*
  - .ccteam/implement-report.md
soft_cost_warn_usd: 5.0    # 仅告警，不打断
stall_warn_minutes: 5       # 5 分钟无 hook event 第一次软告警
parallelism: solo           # 主框架并行粒度（痛点 13；详见 §6.11）
                            #   solo（M0 默认）         | agent_team（M2）        | multi_session（M3）
                            # subagent 不在此声明——任何 agent 都可 ad-hoc 通过 Task 工具启动，叠加在主框架之上
agent_team:                 # 可选：本 phase 内启用 sub-agent（仅 parallelism: agent_team 时生效）
  - role: backend-dev
  - role: frontend-dev
  - role: reviewer
sub_skills:                 # 替人编排的 sub-skill（痛点 12；详见 §6.10）
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done       # phase_start | phase_done（M0/M2 仅这两档）
    output_to: .ccteam/code-review.md
hooks:
  before: scripts/snapshot-git.sh
  after: scripts/run-golden-rules.sh
---

# 任务

你正在为 {{project.slug}} 的 implement phase 工作。
读取 {{required_inputs}}，按 plan-eng.md 实现。
产物必须满足 {{required_outputs}}。

不要交互式询问任何问题——所有决策已经在 plan 阶段定好。
如果发现 plan 有问题，写到 .ccteam/escalation.md 后退出。

完成后写 .ccteam/implement-report.md 总结你做了什么。
```

**Phase 0：Seed 评估**（这是 ccteam 区别于 gstack-auto 的关键）：

输出固定格式：
```markdown
---
verdict: PASS | REJECT | CLARIFY
confidence: 0.0-1.0
---

## 市场分析
（已有竞品 / 用户量 / 替代方案）

## 技术可行性
（核心难点 / 依赖 / 估算工作量）

## 商业可行性（按需）
...

## 决策
- PASS 时：建议技术栈、项目骨架
- REJECT 时：列举具体理由（已有 X、成本不可持续、用户量级 < N）
- CLARIFY 时：提出唯一一个问题（不要列 5 个）
```

Seed 输出由 orchestrator 解析 YAML front matter 决定走向，不依赖 LLM 的自然语言判断。

### 3.4 Workspace 隔离与并行

**每项目一个 git worktree**（在 `~/projects/<slug>/`），独立分支。

**项目目录结构**：
```
~/projects/<slug>/
├── src/                          # 实际代码
├── tests/
├── package.json / pyproject.toml
├── CLAUDE.md                     # 项目级运营手册（自动生成）
├── .ccteam/                      # ccteam 元数据（git 跟踪）
│   ├── spec.md                   # 用户原始需求 + Seed 后澄清
│   ├── plan-ceo.md
│   ├── plan-eng.md
│   ├── architecture.md
│   ├── implement-report.md
│   ├── test-report.md
│   ├── review-report.md
│   ├── scorecard.md
│   ├── state.json                # 状态机当前态
│   └── escalation.md             # 触发用户介入时写这里
└── .gitignore
```

**并发模型**：

```yaml
# ~/.ccteam/config.yml
max_concurrent_projects: 3            # 同时活跃的 tmux session 数
max_total_claude_processes: 5          # 全局 claude 进程上限（每项目一个）
soft_cost_warn_per_project_usd: 20     # 软告警阈值（不打断）
hard_cost_kill_per_project_usd: 200    # 物理上限（防 bug 死循环才启用）
stall_warn_minutes: 5                  # 5 分钟无事件 → 第一次告警
stall_suspicious_minutes: 15           # 15 分钟无事件 → 标记可疑（仍不 kill）
stall_escalate_minutes: 30             # 30 分钟以上 → 升级为 escalation
```

orchestrator 每轮 dispatch 时按这几条做准入控制。注意：**没有 max_turns、没有 max_budget 硬封顶**——长跑由用户决定何时介入。

**为什么不用 Conductor**：Conductor 是 Anthropic 的多 session 工作区工具，但要求人在 IDE 里使用。ccteam 用 tmux + git worktree 取代 Conductor 的工作区隔离能力——tmux 自带后台运行 + 随时 attach 的能力，比 IDE 更适合无人值守。

### 3.5 Self-healing Fix Loop

**结构**（抄 gstack-auto 的 11a/11b/11c 但收紧）：

```
test-run phase
  ├─ 全绿 → 进入 review
  └─ 失败 → fix-cycle (n)
       ├─ fix-plan: 读 test-report，写 fix-plan.md
       ├─ implement-fix: 按 fix-plan 改代码
       ├─ test-run: 重跑
       │    ├─ 全绿 → review
       │    └─ 失败 →
       │         ├─ n < 3 → fix-cycle (n+1)
       │         └─ n = 3 → escalated
```

**执行机制（混合模式）**：fix-cycle 不是"orchestrator 反复 send-keys 注入新的 fix prompt"——那样每轮都是一段独立 user 消息进对话历史，污染上下文也丢掉 cache 的近因优势。改用 [`ralph-loop`](https://github.com/anthropics/claude-plugins-official/tree/main/plugins/ralph-loop) 的 Stop hook 拦截范式：

1. **进入 fix-cycle**：orchestrator 写状态文件 `~/projects/<slug>/.ccteam/fix-loop.state.md`，YAML front matter 含 `iteration: 1` / `max_iterations: 3` / `completion_signal: "TESTS_GREEN"`，正文是 fix prompt（**每轮开头先写 `.ccteam/fix-plan-<iteration>.md` 记录本轮诊断与修复方案** → 读 test-report → 改代码 → 重跑测试）。每轮独立的 fix-plan 文件让 escalation 收集（见下文）拿得到三次完整诊断。同时把 state.json 的 `phase_state` 切到 `fix_locked`（见 §3.2）。
2. **首次注入**：orchestrator send-keys 一次，触发 fix prompt 跑第一轮。然后 orchestrator 完全退出 fix-cycle 的控制路径。
3. **Stop hook 接管**：claude 想退出时 Stop hook 检查 `fix-loop.state.md`——若存在、未达 `max_iterations`、且最后一次 assistant 输出未含 `TESTS_GREEN`，**输出 `{"decision": "block", "reason": "<同一段 fix prompt>"}` 拦截退出并重喂**；同时 `iteration += 1`。这步直接复用 ralph-loop 的 hook 逻辑，cache 在同 session 内复用,fix 1 / 2 / 3 不会重读 plan 与代码上下文。
4. **释放控制**：测试通过（claude 输出 `TESTS_GREEN`）或撞 `max_iterations` → Stop hook 删除状态文件、放行退出 → orchestrator 通过 progress.jsonl 上的 `phase_done` / `escalate` 事件感知并接管。

**为什么混合**：phase 切换仍由 orchestrator 主控（因为 phase 间需要 reset context、跨项目调度、注入完全不同的下一段 prompt）；但单 phase 内的 fix-cycle 是"同一段 prompt 反复跑直到收敛"——这正是 ralph-loop 设计的形态。两者职责不冲突：orchestrator 管"phase 之间"，Stop hook 管"phase 内的自愈循环"。

**Stop hook 复用**：直接抄 `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/ralph-loop/hooks/stop-hook.sh`，改三点：
- 状态文件路径换成 ccteam 约定（`fix-loop.state.md`，避免与 ralph-loop 的 `.claude/ralph-loop.local.md` 冲突）
- 完成信号从 `<promise>...</promise>` 改成纯文本 `TESTS_GREEN`（约束简单、grep 即得）
- 计数到顶或主动放弃时把 `escalate` 事件 append 到 `~/.ccteam/progress/<slug>.jsonl`，让 orchestrator 知道

**fix-cycle 决策逻辑只能在一处**：Claude Code 允许一个 Stop hook entry 下挂多个 command（§6.2 settings.json 即如此——同时跑 `parse-phase-end.sh` 和异步的 `progress-append.sh Stop`），这没问题。但**fix-cycle 的"是否拦截退出 + 重喂"决策只能由 `parse-phase-end.sh` 单点输出**——同 entry 内多个 command 的执行顺序虽稳定，但只有第一个 stdout JSON 决策有效，其它 command 必须 `async: true` 仅做 append/log 类副作用。脚本内部判断"现在是不是 fix-cycle 模式"分支处理（fix-cycle → ralph 范式拦截重喂；非 fix-cycle → 解析 `PHASE_DONE` / `ESCALATE`）。

**escalation 触发时**，orchestrator 收集：
- 最后一次 test-report.md
- 三次 fix-plan.md 的诊断
- 最近 200 行 progress.jsonl（hooks 事件流）
- git diff 最近 3 commit
- `tmux capture-pane -p -t ccteam-<slug>` 当前可见输出（最后一屏，给人看的上下文）

打包成 telegram 消息：
```
🚨 项目 bookmark-mgr-a3f9 卡住了
位置: implement (test 失败 3 次后)
卡点: 数据库迁移在 sqlite 上跑不通，agent 三次方案都失败
试过：
  1. 改用 alembic 自动迁移 - 报 schema 冲突
  2. 手写 raw SQL - 报 FK 约束
  3. 切到内存 DB - 测试可过但 spec 要求持久化
建议你：[A] 接受内存 DB（spec 让步）  [B] 我换用 better-sqlite3  [C] 我跳过这个项目去做下一个
```

**禁止静默重试**：fix loop 撞 3 次顶绝不静默重置——这是 ccteam 区别于"AI 永远说没事"的承诺。

### 3.6 三层防御协议（Defense in Depth）

替代旧方案中"人持续在场审查"的能力，用三层独立机制保证质量与方向不偏（呼应痛点 11）：

#### L1 架构约束（deterministic，写死的红线）

不与 agent 商量、不可绕过。具体形态：

- **phase 模板 `required_outputs`**——本 phase 必产出物，hook 在 Stop 前 verify；缺则不视为 phase_done
- **危险命令拦截**——`PostToolUse(Bash matcher)` 拦截 `git push.*` / `rm -rf /` / deploy 脚本（详见 §6.2）
- **scope budget**——超出 plan-eng.md 声明 scope 的实现尝试由 scope-watcher（L2）触发 BLOCK
- **不可改 invariant**——`.ccteam/` 之外的元数据不许 ccteam 自动改

**M0 落地**：仅 `required_outputs` 校验 + 危险命令拦截（hook 实现，详见 §6.2）。
**M2 落地**：加 `golden-rules.py`（5 项基础检查 + 项目特定补充），phase `after` hook 调用。

#### L2 多 agent 互检（stochastic 但多视角）

每 phase 启用相关 audit agent，多视角议事——对应痛点 11 "为什么单 agent 抓不住、必须靠团队议事"。两类 agent 并存：

**Phase 内 audit agent**（短期，仅本 phase 活）：

| 角色 | 视角 | 何时启用 |
|---|---|---|
| `architect` | 技术方案合理性 | plan-eng / implement |
| `critic` | 代码品味、边界 case | review |
| `designer` | UX、交互（前端项目） | plan-eng / review |
| `security` | OWASP/STRIDE | review / pre-ship |
| `scope-watcher` | scope drift（每 phase 检查 spec.md 范围） | 每 phase |

实现复用 §6.3 与 CLAUDE.md §三.4：`claude-plugins-official` 的 `pr-review-toolkit/agents/*.md`、`feature-dev/agents/code-architect.md` 等直接 `@文件引用`，不重写。

**Cross-cutting watcher**（长期后台，跨 phase 跑）：
- `cost-watcher` — token / 预算累计
- `drift-detector` — 实现是否偏离 plan-eng

**触发频率纪律（关键）**：cross-cutting watcher **在 phase 边界（Stop hook）运行**，**不**在每个 `PostToolUse` 跑——否则一个 1 小时的 implement phase 会有 100+ 次工具调用，3 个 watcher 共 300+ 次启动，progress.jsonl 灌爆 + 成本难看。

**议事结果**：每个 audit agent 输出 `PASS / CONCERN / BLOCK` 三档：
- 全 PASS → 自动通过
- 任意 BLOCK → 进入 fix-cycle（§3.5）或转 L3（视严重度）
- 有 CONCERN 但无 BLOCK → M2 单 critic 模式直接通过；M3+ 进入投票

**里程碑落地**：
- **M0**：不启用 audit agent，仅靠 L1 + 测试通过
- **M1**：cross-cutting watcher 上线（cost-watcher / scope-watcher），Stop hook 触发
- **M2**：单 critic agent + dev 进程隔离（借鉴 gstack-auto 6 维评分简化版：Functionality 0.30 / Quality 0.20 / Tests 0.15 / UX 0.10 / Speed 0.15 / Docs 0.10 + bug penalty）
- **M3**：phase 内 audit 矩阵 + 投票 + 共识机制
- **M4**：anti-leniency（每 audit 至少一项 CONCERN，禁止全维度高分）+ WEAK 维度强制 BLOCK

#### L3 用户 fork 决策（last resort）

仅在 L1 PASS + L2 拍不了板时弹出。**不是 first checkpoint，是 last resort**——痛点 11 主路径是 L1+L2，**不是 L3**。

**触发条件**：
- L2 至少一个 audit BLOCK 且 fix-cycle 无法修复
- L2 投票分裂（多数 PASS 但有持续 CONCERN）
- 用户预设 careful 模式且本 phase 列为关键 fork

**形态**：telegram push（项目摘要 + 各 audit 立场 + 2-3 个推荐选项 + 一句话 tweak），24h 不响应自动通过——不阻塞长跑。

**信任档位**（用户 `~/.ccteam/config.yml` 设）：
- `yolo` — L3 永不弹（仅 L1 BLOCK 时 escalate）
- `balanced`（默认）— L3 仅在 L2 投票分裂时弹
- `careful` — 任何 CONCERN 都弹

**里程碑落地**：M1 telegram bot + 简易 ABC 选项；M3+ 信任档位 + tweak 句注入。

#### 顺序约束

L1 → L2 → L3，不并联。L2 启动前 L1 已通过；L3 启动前 L2 已议事完毕但拍不了板。

> **痛点 11 直接对应**：旧方案靠"人持续在场做品味与方向校准"；ccteam 把它分解到三层独立机制——L1 兜系统性偏差、L2 兜单 agent 偏差、L3 兜前两层都拍不了板的偏差。

### 3.7 Cross-project Memory（差异化护城河）

**目录布局**：
```
~/.ccteam/memory/
├── patterns/
│   ├── 2026-04-12-clipcache-fastapi-tdd.md
│   ├── 2026-04-15-bookmark-mgr-pwa-offline.md
│   └── ...
├── anti-patterns/
│   └── 2026-04-20-failed-ai-recipe-app.md  # REJECT 案例
└── index.json                                # RAG 索引元数据
```

**写入时机**：
- 每个项目终态（shipped / rejected / escalated）触发 retro phase
- retro phase 让 Claude Code 读项目全历史 + scorecard，产出一份 pattern.md
- 关键字段：tech stack、踩过的坑、成功的设计选择、不要再做的事

**召回时机**：
- Seed phase 启动时，先 RAG 检索 memory（按 spec.md 的 embedding）
- 召回的 top-k 作为 prompt 上下文注入
- 命中相似失败项目 → Seed 倾向于 REJECT 或 CLARIFY

**实现选型（短期）**：
- M0 用纯文件 + grep（无 RAG）
- M3 引入向量索引：用 [claude-mem MCP](https://code.claude.com/) 或 sqlite + sentence-transformers

### 3.8 用户接口层

**M0：CLI**
```bash
ccteam new "做一个本地书签管理器"     # 写 inbox
ccteam status                          # 查所有项目状态
ccteam status <slug>                   # 详情
ccteam logs <slug>                     # 实时 tail stream-json
ccteam answer <slug> "用 PWA"          # 回应 clarify 问题
ccteam reject <slug>                   # 手动否决
ccteam start                           # 启动 orchestrator
ccteam stop                            # 优雅停机
```

**M1：Telegram bot**
- 复用 ccteam-creator 提到的 telegram skill 或自己起一个 bot
- 收消息 → 写 inbox
- 接 notify → 推送 escalation / shipped

**M2：Web 仪表盘（可选）**
- 看项目网格、phase 进度、token 累计
- 抄 gstack-auto 的 Mission Control，但只读、不接管控

---

## 4. 关键流程

### 4.1 端到端：从想法到交付（Happy Path）

```
T+0:00  用户在 Telegram 发："做个本地书签管理器，离线可用"
T+0:00  bot 写 ~/.ccteam/inbox/20260504-bm.md
T+0:30  orchestrator 轮询发现新文件 → triage → 分配 slug
T+0:35  启动 tmux session ccteam-bookmark-mgr-a3f9 + send-keys 注入 Seed prompt
T+1:30  Seed 输出 verdict: PASS，建议技术栈：Vite + Dexie + PWA
T+1:35  RAG 检索 memory，召回 2 条相关项目（PWA 离线缓存模式）
T+1:35  写 spec.md 合并，进入 Plan phase
T+3:00  plan-ceo + plan-eng 完成
T+3:00  Implement phase 启动（agent team: backend-dev + frontend-dev）
T+25:00 实现完成，写 implement-report.md
T+25:30 test-author phase 编测试
T+30:00 test-run phase 全绿 → review
T+33:00 review approved
T+33:30 score phase（M2+）→ ship phase
T+34:00 git tag v0.1.0，写 retro.md，更新 memory
T+34:00 telegram 推送：✅ bookmark-mgr 已交付，36/36 测试通过
```

整个过程中**用户只看到 2 条消息**（提需求 + 收结果）。

### 4.2 Seed 阶段：否决 vs 澄清

```
Seed phase ─┐
            ├─ verdict: PASS    → 进 Plan
            ├─ verdict: REJECT  → 写 reason，告知用户："已否决，因为 X"
            └─ verdict: CLARIFY → 写 question，等用户回答
                                  ├─ 收到回答 → 合并到 spec，重跑 Seed
                                  └─ 24h 无回答 → 自动归档（避免堆积）
```

**关键**：CLARIFY 必须只问一个问题。Seed phase 的 prompt 显式约束。

### 4.3 多项目调度

orchestrator 每轮检查：

```python
running_count = state.count(status='coding')
if running_count < config.max_concurrent_projects:
    candidate = state.next_pending(priority_order=[
        'clarify-answered',  # 用户刚回答的优先
        'fixing',            # 已开工的优先
        'planned',           # 等待 implement 的
        'seeded',            # 等待 plan 的
    ])
    if candidate:
        spawn_phase(candidate)
```

用户在 telegram 发的新想法**自动入队**，不会打断在跑的项目。

### 4.4 Phase 间数据流

每个 phase 在**同一个 tmux session** 内进行（不起新进程）：

1. orchestrator 检查 progress.jsonl 末尾事件判断 claude 是否 idle：
   - `Stop` 或 `Notification:idle_prompt` → idle，直接 send-keys
   - 其他（最近一条是 `PreToolUse`/`PostToolUse`） → 忙，用 `/btw <prompt>` 排队（见 §6.9）
2. 注入的 prompt 形如：
   > 请按 `@.ccteam/phases/<phase>.md` 完成本阶段。完成后写 `.ccteam/<phase>-report.md`，并在最后单独输出一行：`PHASE_DONE: <phase>` 或 `ESCALATE: <一句话原因>`。
3. claude 在同 session 中执行——CLAUDE.md / 已读 spec / plan 等仍在 prompt cache 里，**无重读成本**。
4. claude 工具调用触发 hooks，每次 hook 把结构化事件 append 到 `~/.ccteam/progress/<slug>.jsonl`。
5. claude 产出文件落到 `~/projects/<slug>/.ccteam/<phase>-report.md`。
6. claude 输出最后一行 `PHASE_DONE` / `ESCALATE` → Stop hook 解析后写 `phase_done` / `escalate` 事件。
7. orchestrator inotify 监听末尾终态事件 → 更新 state.json → 注入下一个 phase（回到 1）。

整个过程**不重启 claude**，cache 保留，phase 边界对 claude 而言只是一段新 prompt。仅在 context 超 60% 时才在 phase 边界 reset（见 §6.9）。

### 4.5 失败与升级

| 失败类型 | 处理 |
|---|---|
| claude 进程 crash | 在同 tmux 内 `claude --resume <session_id>` 重启进程恢复历史；3 次仍失败 escalate |
| tmux session 整体丢失 | 起新 tmux + `--resume <session_id>` 全量恢复对话历史 |
| stall（5 分钟无 progress 事件） | **不 kill**，发 telegram 软告警："看起来卡了，要不要 attach 看看" |
| stall 持续 15–30 分钟 | 标记 `suspicious`，仍不 kill；告警升级 |
| stall 超 30 分钟 | 升级为 escalation，由用户决定 |
| 软成本阈值（项目累计 $20 / $50） | 单次告警，继续跑 |
| 硬成本上限（项目累计 $200） | kill claude + escalate（防 bug 死循环） |
| fix-cycle 撞 3 次顶 | escalate，附三次诊断 + capture-pane 快照 |
| Seed REJECT | 终态，归档 + 通知 |
| explicit `ESCALATE: ...` 输出 / escalation.md | 终态，转发给用户 |
| 用户 attach 中手动介入 | 自动暂停 phase 推进，等 detach 或 `ccteam resume <slug>` |

---

## 5. 数据与文件协议

### 5.1 全局目录布局

```
~/.ccteam/
├── config.yml             # 全局配置（并发上限、API key、bot token）
├── inbox/                  # 待 triage
├── queue/
│   ├── seeding/
│   ├── planning/
│   ├── coding/
│   ├── reviewing/
│   ├── done/
│   └── archive/
├── phases/                 # phase 模板（启动时复制到项目）
│   ├── 00-seed.md
│   ├── 01-plan-ceo.md
│   └── ...
├── memory/                 # 跨项目记忆
│   ├── patterns/
│   ├── anti-patterns/
│   └── index.json
├── progress/
│   └── <slug>.jsonl       # 每项目一个事件流（hooks 写入，inotify 监听）
├── log/
│   └── <slug>/             # stream-json 归档（可选，调试用）
├── tmux/
│   └── <slug>.layout       # 项目专属 tmux 多 pane 布局模板
└── state/
    └── orchestrator.json   # orchestrator 自身 in-memory 状态的快照
```

### 5.2 项目级 state.json

```json
{
  "slug": "bookmark-mgr-a3f9",
  "created_at": "2026-05-04T10:23:00Z",
  "tmux_session": "ccteam-bookmark-mgr-a3f9",
  "claude_session_id": "abc123-def-456",
  "claude_pid": 12345,
  "phase_state": "in_flight",
  "current_phase": "implement",
  "phase_history": [
    {"phase": "seed",     "status": "passed", "duration_s": 90, "cost_usd": 0.12},
    {"phase": "plan-ceo", "status": "passed", "duration_s": 45, "cost_usd": 0.08},
    {"phase": "plan-eng", "status": "passed", "duration_s": 60, "cost_usd": 0.15}
  ],
  "fix_cycle_count": 0,
  "cost_used_usd": 1.23,
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 142000,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": "2026-05-04T11:23:45Z",
  "last_event_type": "Stop",
  "last_user_interaction_at": "2026-05-04T10:23:00Z",
  "user_attached": false,
  "user_pause_pending": false
}
```

### 5.3 Inbox 协议

文件名：`<ISO-timestamp>-<random>.md`，原子写入（先写 `.tmp` 再 `mv`）。
内容：见 §3.1。

### 5.4 控制协议（用户 → orchestrator）

```
~/.ccteam/control/
├── reject-<slug>          # 创建文件 = 命令"否决项目 <slug>"
├── pause-all              # 创建文件 = 暂停所有调度
├── answer-<slug>.md        # 内容 = 用户对 clarify 问题的回答
└── boost-<slug>            # 提升优先级
```

orchestrator 每轮扫描 control/，处理后删除文件。

### 5.5 Progress.jsonl 格式（结构化事件流）

每个项目一个 `~/.ccteam/progress/<slug>.jsonl`，由 Claude Code 的 hooks 写入。**这是 orchestrator 唯一的状态事实来源**——tmux 终端输出只给人看，不解析。

**事件示例**：
```jsonl
{"ts":"2026-05-04T11:23:00Z","event":"session_start","tmux_session":"ccteam-bookmark-mgr-a3f9"}
{"ts":"...","event":"phase_inject","phase":"implement"}
{"ts":"...","event":"PreToolUse","tool":"Edit","path":"src/db.ts"}
{"ts":"...","event":"PostToolUse","tool":"Bash","cmd":"pnpm test","exit_code":0,"duration_ms":4521}
{"ts":"...","event":"phase_milestone","phase":"implement","note":"完成 schema + migration"}
{"ts":"...","event":"phase_done","phase":"implement","duration_s":4521,"cost_usd":2.13}
{"ts":"...","event":"escalate","reason":"db migration 不可调和","cycle":3}
{"ts":"...","event":"user_attach","detected_by":"PreToolUse-input-source"}
```

**写入机制**：
- `session_start` / `phase_inject`：orchestrator 直接 append（在 send-keys 前后）
- `PreToolUse` / `PostToolUse` / `phase_milestone`：Claude Code hooks 调用脚本 append
- `phase_done` / `escalate`：Stop hook 解析 claude 最后一行输出（`PHASE_DONE: ...` / `ESCALATE: ...`）写入

**消费方**：
- orchestrator：用 inotify 监听末尾，做状态转移与 stall 检测
- 用户 dashboard pane：`tail -f progress/<slug>.jsonl | jq -c '.event + ":" + (.tool // .note // "")'`
- retro phase：作为项目历史输入

**Stream-json 仍可保留**（可选）：用 hook 把 `--output-format stream-json` 的内容旁路归档到 `~/.ccteam/log/<slug>/`，仅供事后调试，不参与状态判定。

---

## 6. Claude Code 扩展点映射

### 6.1 Tmux 长 session 调用模板

**为什么不用 `claude -p` 子进程**：每 phase 起新进程意味着重读 CLAUDE.md / spec / 上游产物，反复触发冷启动；prompt cache 5 分钟 TTL 命中不到。长跑项目（数小时-数天）改用一个**项目级长 session**——同 session 跨 phase 共享缓存，且天然支持随时 attach 观察与介入。

#### 项目首次启动

```bash
SLUG="bookmark-mgr-a3f9"
PROJECT_DIR="${HOME}/projects/${SLUG}"

tmux new-session -d \
  -s "ccteam-${SLUG}" \
  -c "${PROJECT_DIR}" \
  "claude --dangerously-skip-permissions"

# 等 SessionStart hook 写 ready 标记
while ! [ -f "${PROJECT_DIR}/.ccteam/ready" ]; do sleep 1; done
```

#### 注入 phase prompt

推荐用 `@文件引用` 而非 send-keys 大段文本，避免转义问题：

```bash
PHASE="03-implement"
tmux send-keys -t "ccteam-${SLUG}" \
  "请按 @.ccteam/phases/${PHASE}.md 完成本阶段。完成后写 .ccteam/${PHASE}-report.md，并在最后单独输出一行：PHASE_DONE: ${PHASE} （或 ESCALATE: <一句话原因>）。" \
  Enter
```

`PHASE_DONE` / `ESCALATE` 这一行作为终态信号——Stop hook 检测到 → 写 progress.jsonl → orchestrator 读到 → 注入下一个 phase。

#### 多 pane 仪表盘布局（用户 attach 时一屏看全）

```bash
# 主 pane：claude 交互
# 右上 pane：progress.jsonl 实时滚动
tmux split-window -h -t "ccteam-${SLUG}" -p 30 \
  "tail -f ~/.ccteam/progress/${SLUG}.jsonl | jq -c '.ts + \" \" + .event + \" \" + (.tool // .note // \"\")'"

# 右下 pane：成本累计 / 当前 phase 计时
tmux split-window -v -t "ccteam-${SLUG}":0.1 -p 50 \
  "watch -n 5 'jq -r \"[当前 phase: \" + .current_phase + \" | 累计: \\$ \" + (.cost_used_usd|tostring) + \"]\" ~/projects/${SLUG}/.ccteam/state.json'"
```

#### 断开后重连

```bash
# orchestrator 重启时
if tmux has-session -t "ccteam-${SLUG}" 2>/dev/null; then
  echo "tmux session 仍在，直接续接（无需操作）"
else
  # session 丢失，用 --resume 在新 tmux 起新 claude 进程恢复对话历史
  CLAUDE_SESSION=$(jq -r .claude_session_id "${PROJECT_DIR}/.ccteam/state.json")
  tmux new-session -d -s "ccteam-${SLUG}" -c "${PROJECT_DIR}" \
    "claude --dangerously-skip-permissions --resume ${CLAUDE_SESSION}"
fi
```

`--resume` 让 Claude Code 重新加载完整对话历史——cache 仍要预热一次（cold start），但工作记忆不丢。

#### 用户介入

```bash
ccteam attach <slug>     # = tmux attach -t ccteam-<slug>
# 用户键入文本 → claude 当作 prompt 接收
# Ctrl+B D 离开（claude 继续跑）
```

orchestrator 通过 `PreToolUse` hook 检测最近一次输入源：若来自人（vs. 来自 send-keys 时盖的 marker），自动暂停 phase 推进，等 `ccteam resume <slug>` 或用户 detach 超过 N 分钟（视为放权）。

#### 关键约束

- ✅ 用 `--dangerously-skip-permissions`（消灭弹窗，痛点 8）
- ✅ 启用 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`（settings.json env 段）
- ✅ **默认开 1M 上下文**：长跑必备，给 cache 足够空间；超过 60% 在 phase 边界 reset（详见 §6.9）
- ❌ **不**用 `claude -p`（失去 attach / 介入能力）
- ❌ **不**设 `--max-turns`（用户要求长跑，由 stall + 成本上限兜底）
- ❌ **不**设 `--max-budget-usd`（同上；改用 hooks 累计 + 软告警，见 §6.8）

### 6.2 Hooks 配置

每个项目 `.claude/settings.json`（结构参考 ccgram / moshi-hooks 等线上项目）：

```json
{
  "permissions": {
    "allow": ["*"],
    "deny": ["WebFetch(url:https://*.bank.com/*)"]
  },
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  },
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/load-context.sh", "timeout": 5},
          {"type": "command", "command": "scripts/progress-append.sh session_start", "async": true}
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/parse-phase-end.sh", "timeout": 10},
          {"type": "command", "command": "scripts/progress-append.sh Stop", "async": true}
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "idle_prompt|permission_prompt",
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh notification", "async": true}
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh PreToolUse", "async": true}
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh PostToolUse", "async": true},
          {"type": "command", "command": "scripts/cost-accumulate.sh", "async": true}
        ]
      },
      {
        "matcher": "Bash:git push.*",
        "hooks": [
          {"type": "command", "command": "scripts/block-push.sh"}
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh SubagentStop", "async": true}
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh SessionEnd", "async": true}
        ]
      }
    ]
  }
}
```

**关键 hook 用途**：

| Hook | 作用 |
|---|---|
| `SessionStart` | 写 ready 标记；append `session_start` 事件 |
| `Stop` | claude 完成一轮 → 解析最后一行 `PHASE_DONE` / `ESCALATE`；append `Stop` 事件（**这是 idle 信号**，§6.9 注入协议依赖此事件） |
| `Notification:idle_prompt` | claude 显式等待用户输入 → 同样作为 idle 信号 |
| `Notification:permission_prompt` | 不应该出现（`--dangerously-skip-permissions` 兜底）；出现说明配置失效，记录排查 |
| `PreToolUse` | append 工具调用事件；活跃信号（用于 stall 检测的反向判断） |
| `PostToolUse`（通用） | append 事件；累加 token / cost 到 state.json（context_tokens_used 用于 §6.9 的 60% 阈值判断）|
| `PostToolUse`（Bash matcher） | 拦截危险命令（`git push`、`rm -rf /`、deploy 脚本等） |
| `SubagentStop` | 子 agent 退出（仅 Agent Teams phase 内相关）|
| `SessionEnd` | claude 进程退出 → orchestrator 知道要么 reset 完成，要么 crash |

**cost-accumulate.sh 工作原理（重要）**：Claude Code **不会**在 hook 输入里直接给 `cost_usd`——必须自己算。流程：

1. hook 通过 stdin 收到 JSON，内含 `transcript_path`（Claude Code 的 session JSONL 路径）。
2. 脚本 tail 该 JSONL 文件最后一条 `role:assistant` 记录，解析 `message.usage` 字段：`input_tokens` / `cache_read_input_tokens` / `cache_creation_input_tokens` / `output_tokens`（字段名参考 `claude-plugins-official/session-report/skills/session-report/analyze-sessions.mjs`）。
3. 按当前模型单价（在 `~/.ccteam/config.yml` 维护一张 `model_rates` 表）计算本轮成本增量。
4. 原子地（`.tmp` + rename）累加到 `state.json.cost_used_usd` 和 `state.json.context_tokens_used`。
5. 后者直接驱动 §6.9 的 60% 阈值判断。

`async: true` 不能省——同步阻塞会拖慢 PostToolUse 路径。

### 6.3 Multi-agent 编排（phase 内并行 + cross-cutting watcher）

ccteam 用 multi-agent 编排同时承担两个不同目标——**质量**（痛点 11 L2，多视角议事）与**速度**（痛点 13 L 加速，多角色并行）。两个目标用同一个 Agent Teams 机制实现，但 phase prompt 中表达不同：

| 目标 | 多 agent 干啥 | 典型 phase | 痛点 |
|---|---|---|---|
| **质量**（垂直） | 看同一份输入，各视角审 | review、plan-eng | 痛点 11 |
| **速度**（水平） | 各做不同事 | implement | 痛点 13 |

两个目标的 multi-agent **可同 phase 共存**——例如 implement phase 启 `backend-dev`/`frontend-dev`（速度）+ `reviewer` 旁路审产物（质量）。

下面三种模式并存：

#### 模式 A：Phase 内 agent team（短期，隔离 audit / 加速）

在 `implement` / `review` 这种复杂 phase 里启用 Claude Code 的 Agent Teams 实验特性：

```bash
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude -p ...
```

phase prompt 显式调用：

> 你作为 implement phase 的 team-lead。
> 启动两个 sub-agent：
> - `backend-dev`：实现 API 层
> - `frontend-dev`：实现 UI 层
> 让他们并行工作。每 2 次工具调用后写 progress.md。

**注意**：在 tmux 长 session 模式下，主 session 的上下文与 cache 跨 phase 持续存在。每个 phase 启动 sub-agent 仍是独立的——上一 phase 的 sub-agent 已退出，本 phase 重新派发。如果某些 sub-agent 应跨 phase 持续（少见），需在 phase prompt 中显式续命。

#### 模式 B：Cross-cutting watcher（长期后台，跨 phase 跑）

不属于任何单 phase，全程监督：

| Watcher | 关注 | 触发频率 |
|---|---|---|
| `cost-watcher` | token / 预算累计与阈值 | 每 phase 边界 |
| `scope-watcher` | 实现 / 计划是否漂出 spec.md | 每 phase 边界 |
| `drift-detector` | 实际进度与 plan-eng 偏离度 | 每 phase 边界 |

**实现机制（关键纪律）**：watcher **在 Stop hook 触发**，**不**在每个 `PostToolUse` 触发。理由：一个 1 小时 implement phase 有 100+ 次工具调用，3 个 watcher 共 300+ 次启动会灌爆 progress.jsonl + 成本难看。Stop hook 是天然的 phase 边界，每 phase 启动一次 watcher 已经足够。

**输出协议**：watcher 异步子进程跑完后，输出 `PASS / CONCERN / BLOCK`：
- PASS → 静默
- CONCERN → append `progress.jsonl` 一条 `watcher_concern` 事件
- BLOCK → append `watcher_block` 事件 + 写 `escalation.md`，orchestrator 据此决定是否进 L3

#### Subagent 与 Agent Teams 的叠加（不互斥）

Agent Teams 是 phase 内的横向多角色编排，subagent 是任何 agent 内的纵向 context 节流。两者职责正交、可叠加：
- 例：implement phase 启 Agent Teams（`backend-dev` ∥ `frontend-dev` ∥ `reviewer`），**backend-dev 内部**同时用 `Task(subagent_type=code-explorer)` 启 subagent 研究"我们 codebase 怎么用 SQLAlchemy"——主线写代码，subagent 跑研究后返回结构化总结，不污染 backend-dev 自己的 context
- subagent **不在 phase 协议中声明**——任何 agent 在任何时刻都可 ad-hoc 启动；只受 `max_subagents_per_phase` 资源约束（详见 §6.11）

#### 与 Sub-skill 调度的边界

agent（本节）= 在 phase 内或后台**并行**跑的 multi-agent；sub-skill（§6.10）= phase 进入/完成时**串行**调用的工作流单元（如 code-reviewer 跑完输出文件给下个 phase）。两者协作但不重叠：
- 每个 phase 可同时启用 phase 内 agent team（并行 implement）+ sub-skills（串行 review/qa）+ cross-cutting watcher（后台监督）
- phase 协议 front matter 同时支持 `parallelism` / `agent_team` / `sub_skills` 三个字段

### 6.4 MCP servers

ccteam 需要的 MCP：

| MCP | 用途 | 出处 |
|---|---|---|
| **Telegram bot** | 通知 + 接收用户消息 | 自己写或用现成 |
| **claude-mem** | 跨项目向量记忆 | M3 引入 |
| **Playwright** | E2E 测试（前端项目） | 已有 |
| **GitHub** | PR 创建、issue 管理 | 可选 M4 |

ccteam 自身不需要写 MCP，但会暴露 ccteam 状态查询作为 MCP（让 Claude Code 在 phase 中能查"我在哪个项目里、当前 phase 是什么"）。

### 6.5 项目级 CLAUDE.md（每项目自动生成）

orchestrator 在 plan phase 后写入：

```markdown
# CLAUDE.md (auto-generated by ccteam)

## 项目上下文
- slug: bookmark-mgr-a3f9
- 用户原始需求: 见 .ccteam/spec.md
- 当前 phase: implement
- 技术栈: Vite + Dexie + PWA（来自 plan-eng.md）

## 工作约定
- 不要交互式询问。所有决策已在 .ccteam/plan-eng.md 中。
- 测试不过不算完成。
- 修改 API 必须同步 .ccteam/api-contracts.md。

## 不做的事
- 不要 git push（被 hook 拦截）
- 不要修改 .ccteam/ 之外的元数据
- 不要碰其他项目目录

## 跨项目经验（来自 ~/.ccteam/memory/）
{{ 召回的 top-3 patterns 摘要 }}
```

### 6.6 A2A bridge（可选，M3+）

如果未来需要"两个 ccteam 实例对话"（例如本地 ccteam 和云端 ccteam 协作），用 A2A bridge 协议。M0-M2 不需要。

### 6.7 Skills 复用（gstack 模式）

ccteam 的 phase prompt 可以以 Claude Code skill 形式分发：

```
~/.claude/skills/ccteam-phases/
├── SKILL.md           # 元数据
└── phases/
    ├── 00-seed.md
    └── ...
```

这样未来用户也能在自己的 Claude Code 里手动喊 `/ccteam-seed`，作为非守护进程模式的 fallback。

### 6.8 透明度与可观测性

ccteam 长跑场景下"看不到 AI 在干什么"是首要担忧。三层透明度：

#### 第一层：tmux session（给人看，零延迟）

`ccteam attach <slug>` 立即看到完整 claude 交互界面：
- 当前 thinking
- 上一次 tool call 的输入与结果
- 文件 diff
- status line（自定义脚本：slug / phase / 累计 cost）

attach 中**直接键入**注入新指令、Ctrl+C 中断、Ctrl+B D 离开后台继续跑。

#### 第二层：progress.jsonl（给程序看，结构化）

hooks 把每次工具调用、phase 转移、关键事件写到 `~/.ccteam/progress/<slug>.jsonl`（格式见 §5.5）。orchestrator 用 inotify 监听末尾——**这是唯一的状态事实来源**，避免解析 tmux 终端输出的脆弱性。

#### 第三层：仪表盘 pane（一屏看全）

tmux session 预设多 pane 布局（启动模板见 §6.1）：
- 主 pane：claude 交互
- 右上 pane：`tail -f progress.jsonl | jq` 滚动事件流
- 右下 pane：实时成本累计 + 当前 phase 计时

attach 一次即可同时看到"AI 在做什么 / 已经做过什么 / 花了多少钱"。

#### Stall 检测（软告警，不强制 kill）

orchestrator 监听 progress.jsonl 最新事件时间戳：

| 静默时长 | 动作 |
|---|---|
| < 5 min | 正常（推理 / 长 Bash / 网络等待） |
| 5–15 min | 第一次软告警："项目 X 看起来卡了，要不要 `ccteam attach`？" |
| 15–30 min | 标记 `suspicious`，但**仍不 kill**；告警升级 |
| > 30 min | 升级为 escalation，由用户决定 |

**永远不主动 kill**——除非命中物理上限（项目累计 cost > $200，防 bug 死循环）。这是与 headless 模式最大的区别：**相信长跑、相信 cache、相信用户能 attach 看**。

#### 成本观测（软告警，不强制截断）

`PostToolUse` hook 累加 `cost_used_usd` 写到 state.json：

| 阈值 | 动作 |
|---|---|
| 项目累计 $5 | 静默记录 |
| 项目累计 $20 | 一次软告警（继续跑） |
| 项目累计 $50 | 再次软告警 + 触发 retro 评估 |
| 项目累计 $200 | 物理上限，kill + escalate |

阈值都在 `~/.ccteam/config.yml` 里可调；CLI `ccteam show <slug>` 可实时查询。

### 6.9 长跑鲁棒性（单一策略）

针对长跑场景的两个典型问题，各采用一条最直接的路径——**不做多层兜底**。

#### 长 session 上下文膨胀 → 60% 阈值 + phase 边界 reset

- **默认开 1M 上下文**：避免短期内触顶。
- **PostToolUse hook 持续累加**：每次 turn 的 `usage.input_tokens + cache_read_input_tokens` 写入 `state.json.context_tokens_used`。
- **超过 60% 触发 reset**（即 `context_tokens_used > 600_000`）：

  1. **不**立即打断当前推理。
  2. 等当前 phase 的 `phase_done` 事件出现。
  3. orchestrator 把项目当前进度追加到 `.ccteam/CLAUDE.md` 的"当前进度"节（已完成 phase / 待办 / 关键决策）。
  4. tmux send-keys 注入 `/exit` 终止 claude 进程。
  5. 同 tmux session 内启动新 claude（**不用 `--resume`**——目的就是清空上下文）。
  6. 新 session 自动加载 CLAUDE.md，从"当前进度"节继续。
  7. 重置 `state.json.context_tokens_used = 0`。

这条路径的代价：cache 失效一次（一次冷启动）。但在长跑累计成本里可忽略，避免冲到性能崩溃区。

**为什么 reset 不用 `--resume`，而 §6.1 的崩溃恢复用 `--resume`**：两者目标相反——
- **§6.1 崩溃恢复（被动）**：claude 进程意外死亡，**目的是把对话历史救回来**才能续上未完工作；`--resume` 加载完整历史（cache 仍要冷启动一次，但工作记忆不丢）。
- **§6.9 主动 reset**：恰恰因为对话历史撑爆 context，**目的是丢弃历史**；`/exit` + 全新 session 是手段本身，CLAUDE.md 桥接代替历史承载关键信息。

不要混用：在该 reset 时用 `--resume` 等于没 reset；在该恢复时用全新 session 等于丢了所有进度。

#### Phase 注入：idle-aware

- **判断 idle**：从 `~/.ccteam/progress/<slug>.jsonl` 末尾读最新事件——`Stop` 或 `Notification:idle_prompt` 表示 claude 当前 idle；其他事件（最近一条是 `PreToolUse` / `PostToolUse`）表示 claude 正在干活。
- **idle 时直接 send-keys**：注入 phase prompt + Enter。
- **忙时用 `/btw`**：claude 不会被打断，会把消息排队到当前任务完成后处理。

```bash
# 注入前判断
LAST_EVENT=$(tail -1 ~/.ccteam/progress/${SLUG}.jsonl | jq -r .event)
if [[ "$LAST_EVENT" == "Stop" || "$LAST_EVENT" == "notification" ]]; then
  # idle，直接注入
  tmux send-keys -t "ccteam-${SLUG}" \
    "请按 @.ccteam/phases/${PHASE}.md 完成本阶段，最后输出 PHASE_DONE: ${PHASE}" Enter
else
  # 忙，排队
  tmux send-keys -t "ccteam-${SLUG}" \
    "/btw 请按 @.ccteam/phases/${PHASE}.md 完成本阶段，最后输出 PHASE_DONE: ${PHASE}" Enter
fi
```

`/btw`（by the way）是 Claude Code 内建命令——把消息塞到"待办"，不打断当前推理，claude 完成手头的事后会处理。这一条命令就是注入失败的全部解法，**不**做超时重试 / capture-pane 解析 / kill-restart 多层兜底。

### 6.10 Sub-skill 自动调度（替人编排 plugin）

ccteam 不重写 gstack / claude-plugins-official 的 skill；ccteam 的差异化是**替人 orchestrate 它们的调用时机与产物接力**——直接对应痛点 12。

#### Phase front matter 的 `sub_skills` 字段

呼应 §3.3。每个 phase 在 YAML front matter 列本阶段应自动 trigger 的 sub-skills：

```yaml
sub_skills:
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done
    output_to: .ccteam/code-review.md
  - skill: "claude-plugins-official:security-guidance/hooks/security_reminder_hook.py"
    trigger: phase_start
    output_to: .ccteam/security-precheck.md
```

#### Trigger 时机（M0/M2 仅两档，足够）

| Trigger | 何时跑 | 实现 |
|---|---|---|
| `phase_start` | phase prompt 注入前 | orchestrator 把 skill 内容前置到 prompt（或异步先跑产出文件供 phase 引用） |
| `phase_done` | claude 输出 `PHASE_DONE` 后 | orchestrator 在状态机转移前调用 skill，产物落到 `output_to` |

**M0/M2 不引入 `before_done` 之类需 Stop hook 拦截的 trigger**——那等同于再开一条 fix-cycle 复杂度路径。如未来真有用例（例：claude 写完代码主动跑 lint 后再退出），M3+ 再加。

#### 产物接力（自动化的核心价值）

每个 sub-skill 产物按 `output_to` 落到项目 `.ccteam/` 下。orchestrator 在调度下一 phase 时：
1. 扫上一 phase 的 `sub_skills` 全部 `output_to` 路径
2. 把这些路径作为下一 phase prompt 的 `@文件引用` 自动追加
3. 下一 phase claude 自动读到上游 audit / review 产物

**用户视角**：从头到尾不需要手动复制粘贴任何 skill 产物——这就是痛点 12 的落地。

#### 复用粒度（呼应 CLAUDE.md §三.7）

按需选三种粒度之一：
- **直接 `@文件引用`**（零安装）——phase 模板里 inline 引用 plugin 文件
- **拷贝到项目**（冻结版本）——`cp` 到 `~/projects/<slug>/.claude/agents/` 后改
- **整 plugin 安装**（M2/M3 才考虑）——`/plugin install <name>@claude-plugins-official`

`sub_skills` 字段支持三种粒度——`skill:` 路径既可指向官方仓库（自动按粒度 1）、也可指向项目级（粒度 2）、也可是已安装 plugin 的命令（粒度 3）。orchestrator 解析时按路径前缀分发。

#### 新插件如何接入（Phase 协议的可扩展性）

社区出新插件时，作者无须改 ccteam 代码——只需提供：
1. 一份 `skill_intent.yaml` 描述本 skill 适合的 trigger（`phase_start` / `phase_done`）和典型 output 形态
2. 推荐的挂载 phase（plan-eng / implement / review / ship 等）

ccteam 在 Seed phase 后扫一次 `~/.claude/plugins/.../skill_intent.yaml`，按推荐挂载点把 skill 加到对应 phase 模板的 `sub_skills` 列表（M3+ 自动化；M0-M2 手动维护 phase 模板）。

#### 与 §6.3 Multi-agent 编排的边界

- **`agent_team`（§6.3）** = phase 内**并行**跑的 audit/dev sub-agent
- **`sub_skills`（本节）** = phase 进入或完成时**串行**调用的工作流单元，产物落文件给下游用

两个字段在 phase front matter 共存、互不冲突。

### 6.11 Multi-session per project（痛点 13 大项目加速；M3）

适用：plan-eng 在分析 spec 时识别出"≥3 个独立子模块且接口稳定"——例如 SaaS 拆 backend-api / frontend-dashboard / mobile-app / docs。**M3 才上线**；M0/M1/M2 默认 `parallelism: solo` 或 `agent_team`。

#### 与 §6.3 Agent Teams 的关键区别

| 维度 | `parallelism: agent_team`（§6.3 模式 A） | `parallelism: multi_session`（本节） |
|---|---|---|
| 进程模型 | 1 session N agent | N session（独立 claude 进程） |
| Context | 共享 1M 主 session 上下文 | 每 session 独立 1M context |
| Cache | 高效复用主 session prompt cache | 各自独立，不共享 |
| 适用 | 中型项目，phase 内多角色 | 大型项目，子模块独立度高、接口稳定 |
| 开销 | 中（共享进程） | 大（N 进程 × 1M context） |
| 取舍 | 优化 token 成本 | 优化墙钟时间 |

#### 工作区结构

```
~/projects/<slug>/
├── .ccteam/
│   ├── state.json                   # master state（项目级）
│   ├── parallelism: multi_session
│   ├── sub-modules/
│   │   ├── backend-api/
│   │   │   ├── state.json           # 子模块 state（独立 phase 进度）
│   │   │   └── progress.jsonl
│   │   ├── frontend-dashboard/
│   │   ├── mobile-app/
│   │   └── docs/
│   └── interface-contracts.md       # 子模块间接口契约（fan-out 时定下，fan-in 时验证）
├── backend-api/                     # 实际代码（独立目录）
├── frontend-dashboard/
├── mobile-app/
└── docs/
```

#### Tmux session 命名

```
ccteam-<slug>                        # master session（编排）
ccteam-<slug>--backend-api           # 子模块 session
ccteam-<slug>--frontend-dashboard
ccteam-<slug>--mobile-app
ccteam-<slug>--docs
```

#### Fan-out / Fan-in 协议

主流程：
1. `plan-eng` phase（master session 跑）→ 输出 `interface-contracts.md` + 子模块清单
2. **Fan-out**：master orchestrator 起 N 个 tmux 子 session，每 session 喂自己的子模块 spec + 共享 contracts
3. 各子模块独立跑 implement / test / fix → phase 边界写各自 `progress.jsonl`
4. **Fan-in**：所有子模块都到达 review phase 时，master session 起来跑 review（读所有子模块产物 + 验证 contracts 满足）
5. `ship` phase 在 master session 跑（统一打包/发布）

#### 状态管理

- **master `state.json`**：项目级 phase（plan / fan-out / fan-in / review / ship）+ 子模块状态摘要
- **sub-module `state.json`**：本子模块的 phase 进度（与单 session 项目协议一致）
- master orchestrator 通过 inotify 监听**所有** sub-module `progress.jsonl`，决定何时 fan-in

#### 资源约束

- `~/.ccteam/config.yml` 加 `max_sessions_per_project: 4`（默认；可项目级覆盖）
- 总 token 预算按 master + sum(sub-modules) 累加；硬上限触发 fan-in escalate
- context reset：每个 sub-session 独立按 60% 阈值 reset，不互相影响（§6.9）

#### 三档叠加体现

multi_session 项目内每个 sub-session 仍可独立选 `parallelism: agent_team`（嵌套）或叠加 subagent。例如：
- master `plan-eng` 用 `agent_team` 启 architect / scope-watcher 议事
- backend-api session 的 `implement` phase 用 `agent_team` 启 api-impl / db-impl 并行
- 每个 agent 内仍可 ad-hoc 启 subagent 做局部研究

#### 边界（M3 不解决的）

- **自动子模块切分** = M5（本节假设 plan-eng 已能识别"有 N 个独立子模块"）
- **子模块接口契约的形式化验证** = M5（M3 仅靠 review phase 跑测试 + 人审 contracts.md 满足度）
- **跨子模块的 stop-the-world 重构** = M5（impl 中发现 contract 错时只能 escalate）

---

## 7. 里程碑路线图

详细任务、依赖、验收门、痛点反向映射、风险登记 → [development-plan.md](./development-plan.md)。本节仅给一句话索引：

| 里程碑 | 主目标 | 时长 | 解锁的关键痛点 |
|---|---|---|---|
| **M0** | 单项目 CLI MVP——一句话需求自动产出能跑的代码 | 2-3 周 | 1, 2, 3, 4, 7, 8, 9 |
| **M1** | 多项目并发 + Telegram 入口 | 2 周 | 5, 9（强化） |
| **M2** | Seed Gate（否决无效想法）+ Score（客观质量门） | 2 周 | 6, 3（强化） |
| **M3** | 跨项目记忆——项目 N+1 比项目 N 快 | 3 周 | 10 |
| **M4** | Critic Agent 闭环——超越"测试通过=完成" | 3 周 | 3（深化） |
| **M5** | 大型软件长跑（对标 Symphony） | 3-6 月 | 长期 |

**任何里程碑修改优先改 development-plan.md**——本节只作目录索引，不维护具体任务。里程碑推进准则在 development-plan.md §1 与 §10 维护。

---

## 8. 关键风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| **claude 卡死（长 session）** | 项目永远停在某 phase | 三档软告警（5/15/30 min），不主动 kill；用户 attach 决定 |
| **成本失控** | 一夜烧光 | 软告警阈值（$20/$50）+ 物理上限（$200）兜底；不限 max_turns |
| **长 session 上下文膨胀** | 性能下降 / 成本增加 | 默认 1M 上下文；`context_tokens_used > 60%` 时在下一 phase 边界 reset session（CLAUDE.md 作桥）。详见 §6.9 |
| **send-keys 在 claude 忙时打断推理** | prompt 注入到当前轮造成污染 | idle 时（`Stop` / `idle_prompt` 事件后）才直接 send-keys；忙时改用 `/btw <prompt>` 排队。详见 §6.9 |
| **用户 attach 时与 orchestrator 竞态** | 双方都在 send-keys，prompt 错乱 | PreToolUse hook 检测输入源（人 vs 自动）；orchestrator 检测到 user_attach 立即暂停自动注入 |
| **tmux server 死掉** | 所有项目 session 全断 | systemd 守护 tmux server；orchestrator 启动检查 has-session；丢失走 `--resume` 全量恢复 |
| **`--dangerously-skip-permissions` 被滥用** | rm -rf 用户文件 | 每项目独立 docker container 或 unshare 命名空间；hook 拦危险 Bash |
| **state.json 损坏** | orchestrator 启动崩溃 | 写入用 `.tmp` + rename 原子操作；启动校验 schema，损坏走 backup |
| **fix-loop 在边缘 case 错收敛** | 看似通过其实有 bug | M4 引入 critic + anti-leniency；M2 强制 golden-rules |
| **跨项目记忆污染** | 老项目错误经验影响新项目 | retro 阶段强制标注成功 / 失败；召回时按时间衰减 |
| **Telegram bot 单点** | 通知不到用户 | 双通道（telegram + 邮件 + 文件 fallback） |
| **Claude Code 协议变更** | hook 字段或 CLI flag 失效 | 用 `claude --version` 校验；锁定测试过的版本 |
| **用户提的需求过大** | 一个项目跑数天烧大量 token | Seed phase 检测"超过 N 个子模块" → REJECT 并建议拆分 |

---

## 9. 与已有方案的边界

| 方案 | 形态 | 与 ccteam 的关系 |
|---|---|---|
| **gstack** | Claude Code skill 包，需主对话 | ccteam 借鉴其 phase 划分，但**不**依赖主对话 |
| **gstack-auto** | Web UI + Conductor 编排 | ccteam 短期对标，**砍掉** Web 和 Conductor，换成守护进程 + git worktree |
| **OpenAI Symphony** | Linear + Codex orchestrator | ccteam 长期对标，**保留** orchestrator 模式，**替换** 执行层为 Claude Code，**新增** 任务分解 + 跨项目记忆 + critic |
| **ccteam-creator** | Claude Code 内的多 agent 编排 skill | 完全不同方向：creator = 人在场协作；ccteam = 人不在场交付 |
| **ralph-loop plugin** | 同 session、Stop hook 拦截退出 + 同 prompt 重喂直到 `<promise>` 命中 | **fix-cycle 直接抄**（见 §3.5）——单 phase 内自循环正合此范式；但**不**用于 phase 流水线（phase 间需 orchestrator 主控 reset / 调度 / 注入不同 prompt） |
| **Claude Code 内建 `/loop`** | ScheduleWakeup 动态模式（同会话）或 CronCreate 模式（Anthropic 云端调度远程 agent） | **不用**——动态模式依赖会话存活，违反痛点 9；CronCreate 模式虽能脱离会话但引入云端调度依赖，与 ccteam「本地优先 + `--dangerously-skip-permissions` 项目沙盒」模型不兼容（沙盒里跑的代码不该被云端 agent 远程注入）。ccteam 的循环驱动器永远是本地 Python orchestrator |
| **Conductor / Worktrees IDE** | 多 session IDE | ccteam 用 git worktree 取代，无需 IDE |

---

## 10. 附录

### 10.1 关键命令清单

```bash
# 启动 / 停止
ccteam start                           # 启动 orchestrator（前台）
ccteam start --daemon                  # 启动并 daemonize
ccteam stop                            # 优雅停机（保留 tmux session）
ccteam stop --kill-sessions            # 同时关闭所有 tmux session（慎用）

# 提交需求
ccteam new "需求文本"
ccteam new -f spec.md                  # 从文件提

# 查询状态
ccteam ls                              # 所有项目状态
ccteam show <slug>                     # 单项目详情（含 session 状态、cost、最近 progress）
ccteam progress <slug> --tail          # 实时 tail progress.jsonl
ccteam progress <slug> --phase implement  # 看特定 phase 事件

# 进入项目（核心透明度入口）
ccteam attach <slug>                   # tmux attach 到项目 session（可介入）
ccteam peek <slug>                     # tmux capture-pane 一次性看当前屏，不 attach

# 控制
ccteam reject <slug>                   # 否决
ccteam pause <slug>                    # 暂停（不杀 session）
ccteam resume <slug>                   # 恢复自动调度（接管 attach 后的暂停）
ccteam answer <slug> "回答内容"          # 响应 clarify
ccteam kick <slug>                     # 软重启项目 session（claude --resume）

# 维护
ccteam memory ls                       # 看跨项目记忆
ccteam memory rebuild                  # 重建索引
ccteam config edit                     # 改全局配置
ccteam doctor                          # 体检：tmux server / claude 可用性 / 死 session 检测
```

### 10.2 关键文件路径速查

| 路径 | 用途 |
|---|---|
| `~/.ccteam/config.yml` | 全局配置 |
| `~/.ccteam/inbox/` | 用户提需求 |
| `~/.ccteam/queue/<state>/` | 项目状态分桶 |
| `~/.ccteam/phases/` | phase 模板 |
| `~/.ccteam/memory/` | 跨项目记忆 |
| `~/.ccteam/progress/<slug>.jsonl` | 结构化事件流（hooks 写，inotify 监听） |
| `~/.ccteam/log/<slug>/` | stream-json 归档（可选，调试用） |
| `~/.ccteam/tmux/<slug>.layout` | 项目 tmux pane 布局模板 |
| `~/projects/<slug>/.ccteam/` | 项目元数据 |
| `~/projects/<slug>/CLAUDE.md` | 自动生成的项目运营手册 |

### 10.3 参考项目

- [garrytan/gstack](https://github.com/garrytan/gstack)——23-skill 工程团队 skill pack
- [loperanger7/gstack-auto](https://github.com/loperanger7/gstack-auto)——phase 流水线 + 评分循环
- [openai/symphony](https://github.com/openai/symphony)——单 orchestrator + tracker-driven 长跑模式
- [jessepwj/CCteam-creator](https://github.com/jessepwj/CCteam-creator)——人在场的 multi-agent 编排（与 ccteam 互补）

### 10.4 关键设计差异速查（vs 三个参考项目）

| 能力 | gstack | gstack-auto | Symphony | ccteam |
|---|---|---|---|---|
| 用户主对话保持开启 | 必须 | 必须（部分时段） | 不需要 | **不需要** |
| 控制平面 | skill 文件 | Web UI + Conductor | Linear | **本地文件系统** |
| 多项目 | Conductor 多 session | Conductor + UI | Linear issues 并行 | **inbox 队列 + git worktree** |
| 任务分解 | 人 | 人 | 人（Linear 已分好） | **M5 自动**（短期不做） |
| 可行性评估 | 无 | 无 | 无 | **Seed phase（PASS/REJECT/CLARIFY）** |
| Critic / 评分 | 无 | 6 维评分 | PR review | **M2 评分 / M4 Critic agent** |
| 跨项目学习 | gbrain（可选） | 无 | 无 | **核心差异化（M3）** |
| 执行 agent | Claude Code | Claude Code | Codex | **Claude Code** |
| 长跑能力 | 单 session 限制 | 单 sprint | 周级别 continuation | **M5 对标 Symphony** |
| 部署形态 | skill 安装 | Docker + Fly.io | Elixir 服务 | **本地守护进程（Python）** |

---

## 11. 本文档的位置

- `requirements.md` 回答 **为什么做** 与 **谁会用**——已确认。
- 本文档 `tech-design.md` 回答 **怎么做**——架构、协议、扩展点。
- [`development-plan.md`](./development-plan.md) 回答 **何时做什么**——把 §7 里程碑细化到任务级，含痛点反向映射、依赖图、验收门、风险登记。
- 后续 `interfaces.md`（待写）回答 **每个组件的精确接口**——orchestrator API、phase prompt schema、状态机事件。

所有实现 PR 必须能映射回：
1. `requirements.md` 的某条痛点
2. 本文档某个组件 / phase / 流程

无法映射的，先放进 backlog 而非合入主线。
