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
| **跨项目沉淀** | 痛点 10：每个新项目从零开始 | 复用官方 `~/.claude/CLAUDE.md` + `~/.claude/rules/ccteam-lessons-<team>.md` + per-repo auto-memory；retro phase 让 Claude 写，Seed/verdict phase 启动时官方机制自动注入（详见 §3.7） |
| **零交互沙盒** | 痛点 8：每一步都点允许 | 项目级 Docker / 容器隔离 + 全放行 settings.json |
| **决策点 ≤ 3** | 痛点 2：AI 仍要求我当 PM | 只有不可逆决策（架构、scope 大改、API 形态）才走 escalation |
| **纵深防御替代人值守** | 痛点 11：关键节点不把控 | L1 架构约束（hooks + required_outputs）+ L2 多 agent 互检 + cross-cutting watcher（议事）+ L3 用户兜底（仅 deadlock 弹）；详见 §3.6 |
| **pipeline 编排 sub-skill** | 痛点 12：工作流插件靠人手动调 | 9 主干 phase + 每 phase front matter `sub_skills` 字段；orchestrator 自动 trigger，产物自动接力；复用 gstack / claude-plugins-official 的 plugin，不重写；详见 §6.10 |
| **并行规模自适应** | 痛点 13：大项目串行慢、并行规模选不对 | plan-eng 按 spec 复杂度选 `parallelism: solo / agent_team / multi_session`；subagent 任何粒度可叠加（ad-hoc，不在协议中声明）；三档叠加层级，不是互斥；详见 §3.3、§6.3、§6.11 |
| **smart layer 只 translate,不 decide**（V0.2 M0.21） | watchdog / 后续 ux-helper 不能改 orchestrator 状态 | translation 层(meta-agent watchdog 等)只读取既有遥测,产出 NL 通知;**绝不**调 orchestrator API、写 progress.jsonl、kill session、re-inject prompt;**所有状态变更只能由 orchestrator + hooks 走;** 详见 §3.X |

---

## 2. 总体架构

### 2.1 三层架构(Channel / Interaction / Orchestration)

> **架构沿革**:原"用户接入层"把 Telegram bot 与 CLI / 文件
> inbox 平铺。复盘 + Telegram-as-agent-IM 的成熟外部参考(Claude Code 官
> 方 TG / openclaw / hermes-agent)后,把架构改成三层:**channel 是上层
> 可插拔适配器,user interaction 在 ccteam-managed 长会话上发生,
> orchestration 是底层稳态**。User Interaction Layer 已 ship(M1);
> Channel Layer 是 M2+ 才上线的适配器,且很可能直接复用现有开源方案
> (Claude Code 官方 TG channel / 开源 bot 框架),不在 ccteam 主代码库。

```
┌──────────────────────────────────────────────────────────┐
│  Channel Layer  (M2+ stub,可插拔,不在 ccteam 主代码库)  │
│  ┌───────────┐  ┌────────┐  ┌─────────┐  ┌───────────┐  │
│  │ Telegram  │  │ Feishu │  │ Slack   │  │ ...       │  │
│  │ adapter   │  │ adapter│  │ adapter │  │ email/SMS │  │
│  └─────┬─────┘  └────┬───┘  └────┬────┘  └─────┬─────┘  │
│        │             │           │             │         │
│  每个 adapter 都是 dumb router:Channel ↔ inbox/outbox    │
│  无内嵌 LLM(Symphony 反模式禁止);用现成开源方案,不重写 │
└────────┼─────────────┼───────────┼─────────────┼────────┘
         │             │           │             │
         └─────────────┴─────┬─────┴─────────────┘
                             │ inbox/outbox 文件协议
                             ▼
┌──────────────────────────────────────────────────────────┐
│  User Interaction Layer  (已 ship,M1)                   │
│                                                          │
│  ┌──────────────────────┐  ┌─────────────────────────┐  │
│  │ meta-agent session   │  │ project sessions(N 条)│  │
│  │ ccteam-meta-<user>   │  │ ccteam-<team>-<slug>    │  │
│  │ - 常驻、永不 terminal│  │ - 每项目一条独立 tmux   │  │
│  │ - NL 派单 / 跨项目   │  │ - 独立 progress.jsonl   │  │
│  │   查询 / 监控         │  │ - 独立 phase DAG        │  │
│  │ - 跨项目 lessons     │  │ - 独立 context cache    │  │
│  │   via ~/.claude/rules │  │                         │  │
│  │   (M4 已 ship)        │  │                         │  │
│  │ - tmux attach 即对话 │  │ - tmux attach 即对话    │  │
│  └──────────────────────┘  └─────────────────────────┘  │
│                                                          │
│  接入面契约:                                             │
│  - ~/projects/<team>-<slug>/.ccteam/inbox/  &  outbox/   │
│  - ~/projects/<user>-meta/.ccteam/inbox/  &  outbox/     │
│  (NL markdown + JSON 元数据,channel 层翻译外部消息进入) │
└──────────────────────────┬───────────────────────────────┘
                           │ tmux send-keys / inbox watcher
                           ▼
┌──────────────────────────────────────────────────────────┐
│  Orchestration Layer  (已 ship,M0 + M0.5)               │
│  - Rust orchestrator daemon(~/.ccteam/ 状态平面)        │
│  - progress.jsonl 唯一状态事实来源(§5.5)                │
│  - hooks(§6.2)/ auto_loop(§3.5)/ context reset(§6.9)│
│  - cost / stall watchers(§6.8)                          │
│  - tmux session lifecycle(§6.1)                         │
│  - team abstraction(已 ship,M3,§3.3)                  │
└──────────────────────────────────────────────────────────┘
```

#### 2.1.1 三层各自的职责边界

| 层 | 谁负责 | 内嵌 LLM? | 何时落地 |
|---|---|---|---|
| Channel | 翻译外部消息系统 ↔ inbox/outbox 文件协议;无业务语义 | ❌(Symphony 反模式禁止) | M2+ stub,首选复用开源方案 |
| User Interaction | LLM 驱动的对话与决策(meta-agent + 项目 session);**所有 NL 理解、任务调度、记忆调用都发生在这一层** | ✅ 但**只通过 ccteam-managed claude session 落地**,不是适配器进程内的 LLM | 项目 session ✓(M0);meta-agent 与 inbox 协议 ✓(M1) |
| Orchestration | Rust 编排状态机 / 文件系统状态平面 / 进程生命周期 / hooks 反射 | ❌(永远是 Rust) | ✓(M0 + M0.5) |

#### 2.1.2 这个分层解决了什么

1. **避免 Symphony 反模式**:NL 处理只发生在 ccteam-managed claude session
   一处,channel 适配器是无脑路由(原架构反复踩这个洞)
2. **Channel 可插拔**:Telegram / Feishu / Slack 互不影响,新平台加一个
   adapter 即可;M2+ 选型时直接用现成开源 bot 框架,不在 ccteam 主代码
   库重写
3. **meta-agent 可以多 channel 接入**:同一 meta-agent session 同时被终
   端 `tmux attach` + Telegram 群组 + 未来 web 接入,**LLM 状态只有一份**
4. **M1 工作量收敛**(已 ship):M1 不落任何具体 channel,只把 meta-agent
   session 跑起来 + 把 inbox/outbox 协议钉死;Telegram bot 实现推到
   M2+

#### 2.1.3 进程视图(实施细节)

上面是逻辑分层。**进程视图**对应 §6.1:每条 tmux 长 session(meta-agent
session + 每个项目 session)是独立 OS 进程;Rust orchestrator 是另一个
独立进程;channel adapter 进入 M2+ 后又是若干独立进程。所有进程之间用
**文件系统协议**通信,**不用共享内存 / sockets / IPC**(§5 与 §3.1)。
进程崩溃只丢自己的进程内存,文件状态留给重启后恢复。

### 2.2 关键架构决策

**为什么 Orchestrator 在 Claude Code 之外（不是 Agent Teams 的 Lead）？**

- Agent Teams 的 Lead 必须保持主对话存活，违反"关掉电脑也要跑"（痛点 9）。
- Lead 上下文压缩后会"失忆"——即便走 `team-snapshot.md` 恢复，也需要人触发。
- 长跑守护进程（Rust / Python / Node 等）原生支持 systemd / 重启自恢复，符合 Symphony "tracker-driven recovery" 思路。

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

**实现选型**：Rust（tokio）+ 单一长跑进程。理由：
- 单 binary 分发,与 hooks 共享同一份 serde schema(progress.jsonl 事件 / state.json 字段)
- tokio 适合"轮询 + 子进程管理 + 多任务并发"
- 单进程拥有所有可变状态——抄 Symphony 的 "single GenServer" 思路，避免锁
- 零运行时依赖,与产出项目的容器化路径正交

**核心循环**（伪代码）：
```rust
async fn main_loop() {
    let mut state = State::load_from_fs();          // 启动时从文件恢复
    reattach_tmux_sessions(&mut state).await;       // 已存在的 session 直接续接
    loop {
        poll_inbox(&mut state).await;               // 新想法 → seeding 队列
        ensure_session_started(&mut state).await;   // 项目首启动 → 拉起 tmux + claude
        dispatch_next_phase(&mut state).await;      // idle-aware send-keys 注入（见 §6.9）
        consume_progress_jsonl(&mut state).await;   // inotify 读 hooks 写的事件流
        detect_stall_and_warn(&mut state).await;    // 5/15/30 min 三档软告警
        detect_user_attach(&mut state).await;       // 检测到人介入则暂停自动调度
        reset_if_context_high(&mut state).await;    // phase 边界 + ctx > 60% → reset session
        maybe_notify_user(&mut state).await;        // escalation / done
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
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

**单点 + claim 防重**：claim 粒度是**项目级**（每项目一个 tmux session 一个 claude 进程）。state.json 记录 `tmux_session: ccteam-<team>-<slug>`、`claude_pid`、`phase_state: in_flight | idle | fix_locked`。

- `in_flight` = 已 send-keys 注入 phase prompt，等待 progress.jsonl 中出现 `phase_done` 或 `escalate` 事件
- `idle` = 上一 phase 完成，session 还活着但没在跑——orchestrator 可以注入下一 phase
- `fix_locked` = 当前在 fix-cycle 中，Stop hook 按 §3.5 的 ralph-loop 范式接管自循环；orchestrator **不**注入新 prompt，直到 progress.jsonl 出现 `phase_done`（测试绿）或 `escalate`（撞 3 次顶）

orchestrator 重启时：`tmux has-session` + `kill -0 <claude_pid>` 双重校验。session 还在 → 续接；进程不在 → 走 §6.1 的"极端情况——session 必须重启"路径用 `--resume` 恢复对话历史。

#### 3.2.1 Evergreen 团队(V0.2 §6.4 candidate 5)

某些团队不走 phase DAG —— meta-agent 是事件循环 session,V0.3 watchdog /
reviewer agent 同样会是常驻角色。这类 team 的 `team.yaml` 设
`evergreen: true` + `cost_policy: {kind: none}` 后,orchestrator 会:

- `process_project` 早返,转到 `process_meta_project`(inbox drain +
  context reset,§6.9)
- `enforce_cost_thresholds` 跳过整个 cost 阶梯
- `warn_if_stalled` 跳过 stall 警告(idle 是常态)
- 不计入 `MAX_CONCURRENT_PROJECTS` 配额

**红线**:`Orchestrator` / `process_project` / `enforce_cost_thresholds` /
`warn_if_stalled` / `count_active_regular` 全部用 `is_evergreen(team)` 查
TeamSpec.evergreen,**不许**回到 `state.team == META_TEAM_NAME` 字面量
分叉(strategic doc §3 ccteam-core 红线)。`cost_policy` 两种 variant
(`None` / `KillAt(Option<f64>)`)的语义在
[interfaces.md §5.5](./interfaces.md#55-teamyaml-团队配置m31--m32--m33--v02-m016) 详述
(PRD §6.4 草稿曾列第三个 `Track` variant 给 V0.3 watchdog,review 时删 —
V0.3 真要 cost 追踪不杀时再定义具体行为)。

`teams/meta-agent.yaml` 是首个 evergreen 范例,V0.2 起作为 shipped seed
随 binary 发布;`Orchestrator::new` / `ccteam start` / `ccteam doctor
--reset-shipped-teams` 都会把它写到 `~/.ccteam/teams/meta-agent/team.yaml`。

#### 3.2.2 Team layout + TEAM_SOURCES(V0.2 §5.1 / §5.2)

V0.2 M0.17 把每个 team 的 yaml + phases 整目录化:

```
~/.ccteam/teams/<name>/
├── team.yaml          # 配置 schema 见 interfaces §5.5
└── phases/            # `team.yaml.phase_dir`,默认 `phases`
    └── *.md
```

仓内 ship 同布局(`teams/dev/team.yaml` + `teams/dev/phases/`),
`include_str!` 1:1 对应 on-disk 路径。旧值(`phase_dir: phases-product-research`,
M3.x "相对 ~/.ccteam/" 语义)在 `TeamSpec::parse` 自动重写为 `phases`(legacy
compat,warn-only)。

**三层加载优先级**(`crates/ccteam-core/src/team_resolver.rs`,借鉴 Claude
Code `SETTING_SOURCES` 模式):

```rust
const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,  // <project_dir>/.ccteam/team/team.yaml
    TeamSource::User,     // ~/.config/ccteam/teams/<name>/team.yaml
    TeamSource::Repo,     // ~/.ccteam/teams/<name>/team.yaml
];
```

整团维度,first-source-wins(撞名 project 完全覆盖 user / repo,**不**字段级
合并)。读容错(yaml 错 → warn + 下一层),写严格(`save_team` 拒绝覆盖
不可解析的现有 yaml)。orchestrator 启动调 `discover_team_names(ctx)` 拿到
所有 User+Repo 层的 team 名,逐个走 `resolve_team(name, ctx)` 应用 layered
override 语义,组成 `TeamRuntime` 表。

**红线**:`load_team_runtimes` **不**手工扫 `~/.ccteam/teams/`,全部走
resolver — 后续要加 V0.3 plugin layer 时,只在 `TeamSource::User` 实现里
扩展 `path_for`,resolver 主流程零改动。

**Soft rename via aliases**(V0.2.2 F40):`team.yaml::aliases: Vec<String>`
让 shipped team 可以改 canonical 名,老项目 `state.json::team` /
`~/projects/<old>-*` 目录 / `~/.claude/rules/ccteam-lessons-<old>.md` 全
**不动**;`resolve_team` 第一遍按目录名 try_load 不命中后,第二遍扫每个
source 的 `teams/*/team.yaml` 按 `spec.aliases` 匹配;
`Orchestrator::team_runtime(team)` 同步走 `teams.get(team)` 兜底
`teams.values().find(|rt| rt.spec.aliases.contains(team))`。V0.2.2 首例:
`product-research` → `research`(详 `docs/v0-2-2/prd.md §9`)。`dev` 已经
短,未做。

### 3.3 Phase Pipeline（短期对标 gstack-auto）

每个 phase 是一个 markdown 文件 + YAML front matter（抄 Symphony 的 WORKFLOW.md 形态）。**完整字段定义、9 个 phase 列表、Seed verdict 输出格式 → [interfaces.md §5](./interfaces.md#5-phase-模板-schema)**。

phase 协议的核心架构选择（论证留本节,字段细节看 interfaces）:

- **YAML front matter 是 orchestrator 的唯一解析入口**——`required_inputs` / `required_outputs` 给 L1 架构约束验证；`parallelism` / `agent_team` / `sub_skills` 给痛点 11/12/13 的实现层；`hooks` 给 phase 级生命周期。不解析 prompt body,prompt body 完全留给 claude。
- **Seed 输出靠 YAML 决定走向（PASS/REJECT/CLARIFY），不依赖 LLM 自然语言判断**——orchestrator 只 parse front matter `verdict`，避免"AI 说话不算数"。
- **`parallelism` 字段决定主框架并行粒度**(详见 §6.11):solo(已 ship,默认) / agent_team(永久 deferred,见 §6.3 模式 A 与 docs/v0-1/m2-agent-team-spike.md) / multi_session(未 ship,M4.8)。subagent **不在此声明**——任何 agent 都可 ad-hoc 通过 Task 工具启动,叠加在主框架之上。

### 3.4 Workspace 隔离与并行

**每项目一个 git worktree**（在 `~/projects/<team>-<slug>/`），独立分支。team 前缀（F22 已 ship）让 `~/.claude/rules/ccteam-lessons-<team>.md` 的 `paths:` frontmatter 能正确 scope 到该项目。

**项目目录结构**：
```
~/projects/<team>-<slug>/
├── src/                          # 实际代码
├── tests/
├── package.json / pyproject.toml
├── CLAUDE.md                     # 项目级运营手册（自动生成）
├── .ccteam/                      # ccteam 元数据（git 跟踪）
│   ├── spec.md                   # 用户原始需求 + Seed 后澄清
│   ├── plan-eng.md               # plan-eng phase 产物
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

1. **进入 fix-cycle**：orchestrator 写状态文件 `~/projects/<team>-<slug>/.ccteam/fix-loop.state.md`，YAML front matter 含 `iteration: 1` / `max_iterations: 3` / `completion_signal: "TESTS_GREEN"`，正文是 fix prompt（**每轮开头先写 `.ccteam/fix-plan-<iteration>.md` 记录本轮诊断与修复方案** → 读 test-report → 改代码 → 重跑测试）。每轮独立的 fix-plan 文件让 escalation 收集（见下文）拿得到三次完整诊断。同时把 state.json 的 `phase_state` 切到 `fix_locked`（见 §3.2）。
2. **首次注入**：orchestrator send-keys 一次，触发 fix prompt 跑第一轮。然后 orchestrator 完全退出 fix-cycle 的控制路径。
3. **Stop hook 接管**：claude 想退出时 Stop hook 检查 `fix-loop.state.md`——若存在、未达 `max_iterations`、且最后一次 assistant 输出未含 `TESTS_GREEN`，**输出 `{"decision": "block", "reason": "<同一段 fix prompt>"}` 拦截退出并重喂**；同时 `iteration += 1`。这步直接复用 ralph-loop 的 hook 逻辑，cache 在同 session 内复用,fix 1 / 2 / 3 不会重读 plan 与代码上下文。
4. **释放控制**：测试通过（claude 输出 `TESTS_GREEN`）或撞 `max_iterations` → Stop hook 删除状态文件、放行退出 → orchestrator 通过 progress.jsonl 上的 `phase_done` / `escalate` 事件感知并接管。

**为什么混合**：phase 切换仍由 orchestrator 主控（因为 phase 间需要 reset context、跨项目调度、注入完全不同的下一段 prompt）；但单 phase 内的 fix-cycle 是"同一段 prompt 反复跑直到收敛"——这正是 ralph-loop 设计的形态。两者职责不冲突：orchestrator 管"phase 之间"，Stop hook 管"phase 内的自愈循环"。

**Stop hook 复用**：直接抄 `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/ralph-loop/hooks/stop-hook.sh`，改三点：
- 状态文件路径换成 ccteam 约定（`fix-loop.state.md`，避免与 ralph-loop 的 `.claude/ralph-loop.local.md` 冲突）
- 完成信号从 `<promise>...</promise>` 改成纯文本 `TESTS_GREEN`（约束简单、grep 即得）
- 计数到顶或主动放弃时把 `escalate` 事件 append 到 `~/.ccteam/progress/<slug>.jsonl`，让 orchestrator 知道

**fix-cycle 决策逻辑只能在一处**：Claude Code 允许一个 Stop hook entry 下挂多个 command（§6.2 settings.json 即如此——同时跑 `parse-phase-end.sh` 和异步的 `progress-append.sh Stop`），这没问题。但**fix-cycle 的"是否拦截退出 + 重喂"决策只能由 `parse-phase-end.sh` 单点输出**——同 entry 内多个 command 的执行顺序虽稳定，但只有第一个 stdout JSON 决策有效，其它 command 必须 `async: true` 仅做 append/log 类副作用。脚本内部判断"现在是不是 fix-cycle 模式"分支处理（fix-cycle → ralph 范式拦截重喂；非 fix-cycle → 解析 `PHASE_DONE` / `ESCALATE`）。

**V0.2.2 F35 — silence classifier 兜底层（detached from fix-loop）**：
ralph-loop / `auto_loop::decide` 只在 Stop hook 触发时跑（input = last
assistant text），有两个失效场景:(1) API tool-call hang — `PreToolUse`
后 `PostToolUse` / `Stop` 永不来,auto-loop 根本不触发;(2) send-keys
路由错误 — `phase_inject` 发了但 prompt 落到 sub-agent 上下文,主 agent
没 Stop。orchestrator daemon 主循环新增 `silence_classifier::classify`
(deterministic,无 LLM),按 `progress.jsonl` 末事件 + 静默时长分 7 类
(`Healthy` / `Terminal` / `SubagentBusy` / `SubagentRunaway` /
`MidToolHung(tool)` / `PostStopLimbo` / `InjectLimbo`):前 3 类 noop,
`SubagentRunaway` / `MidToolHung` 写 enriched
`needs_attention.outbox.json`(meta-agent NL 翻译 + propose-confirm 三选
一,**不**自动 act),两类 `*Limbo` 由 orchestrator 直接 deterministic
re-inject 1 次(`MAX_LIMBO_RETRY`,per-phase 计数器存
`<project>/.ccteam/limbo-retry-count.json`),超 cap 转 enriched escalate。
红线:`silence_classifier` 不发 Ctrl-C / 不 kill / 不 LLM;`pane_tail` 只
入 outbox payload 给人读,**不**进 orchestrator 状态机。详见
`crates/ccteam-core/src/silence_classifier.rs` 与 `interfaces.md` §6.2.1。

**escalation 触发时**，orchestrator 收集：
- 最后一次 test-report.md
- 三次 fix-plan.md 的诊断
- 最近 200 行 progress.jsonl（hooks 事件流）
- git diff 最近 3 commit
- `tmux capture-pane -p -t ccteam-<team>-<slug>` 当前可见输出（最后一屏，给人看的上下文）

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

#### V0.2 M0.19 — auto_loop default-on + Stop hook self-loop fallback

**`PhaseTemplate.auto_loop` 默认 `true`**：每个 phase 自循环,撞 `auto_loop_max_iterations`(默认 3)顶才 escalate。phase yaml 显式 `auto_loop: false` 才 opt-out(evergreen 桥接 / ad-hoc diagnostic phase 用)。`completion_signal` 留空时 `effective_completion_signal()` 回退到 `PHASE_DONE: <phase-name>`,因此 phase yaml 不需要重复声明协议字面量。

**Stop hook 三档兜底**(`crates/ccteam-hooks/src/parse_phase_end.rs`):

1. **Auto-loop reinject**:`<project>/.ccteam/auto-loop.state.md` 存在 → 按 ralph-loop 范式重喂 prompt,撞顶时 emit escalate 事件。
2. **PHASE_DONE / ESCALATE 解析**:assistant 末行匹配协议关键字 → 写 progress 事件,Continue。
3. **Self-loop fallback**(V0.2 新增):前两档都没命中,且 `<project>/.ccteam/outbox/` 没有本 phase 内的 `clarify-* / escalation-* / reply-*` 新文件 → 区分两种情形:
   - **第一次进入**(`stop_hook_active` 为 false / 缺):返回 exit 2 + stderr `phase 未正常收尾,请输出 PHASE_DONE / ESCALATE / 写 outbox 之一`。Claude Code 把 stderr 当 blockingError 注入下一轮(`hooks.ts:2784-2805`),assistant 被强制重选合法出口。
   - **第二次进入**(`stop_hook_active` 为 true,L3 fail-safe):写 `<project>/.ccteam/needs_attention.outbox.json`(含 `last_assistant_message` + `tmux capture-pane` 末 30 行),不再 block。watchdog(M0.21)读这个文件 surface 给用户。

**关键约束**:第三档**不**让 ccteam orchestrator 主动 send-keys 续 loop;Claude Code 自己接管循环。orchestrator 只在 progress.jsonl 出现 phase_done / escalate 事件时换 phase。`tmux capture-pane` 输出**只**写进 needs_attention 文件作为给用户看的 surface,不参与状态机决策(沿用"主路径不解析终端输出"红线)。

#### V0.2 M0.19.3 — PreToolUse 拦截 AskUserQuestion

`AskUserQuestion` 是 LLM 内部同步阻塞,Stop hook 不会触发。bootstrap 写 `<project>/.claude/settings.json` 时配 `PreToolUse` matcher `AskUserQuestion`,跑 `ccteam hook intercept-ask` 返回 `permissionDecision: deny`(reason 指引去 outbox)。LLM 收到 deny 立即改写 outbox。pair 的 prompt-layer 软约束在 team.yaml `golden_rules.protocol.forbid_ask_user_question` —— inject prompt 把 directive 文字写进协议红线段(progress.rs `build_phase_prompt_for_template_with_team`)。

### 3.6 三层防御协议（Defense in Depth）

替代旧方案中"人持续在场审查"的能力，用三层独立机制保证质量与方向不偏（呼应痛点 11）：

#### L1 架构约束（deterministic，写死的红线）

不与 agent 商量、不可绕过。具体形态：

- **phase 模板 `required_outputs`**——本 phase 必产出物，hook 在 Stop 前 verify；缺则不视为 phase_done
- **危险命令拦截**——`PostToolUse(Bash matcher)` 拦截 `git push.*` / `rm -rf /` / deploy 脚本（详见 §6.2）
- **scope budget**——超出 plan-eng.md 声明 scope 的实现尝试由 scope-watcher（L2）触发 BLOCK
- **不可改 invariant**——`.ccteam/` 之外的元数据不许 ccteam 自动改

**已 ship(M0)**：`required_outputs` 校验 + 危险命令拦截（hook 实现，详见 §6.2）。
**已 ship(M2.3)**：`golden_rules` executor（5 项基础检查 + 项目特定补充），phase `after` hook 调用。

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
- 有 CONCERN 但无 BLOCK → 单 critic 模式直接通过；M4.5+ 进入投票

**里程碑落地**：
- **已 ship(M0)**：仅靠 L1 + 测试通过，不启用 audit agent
- **已 ship(M1)**：cross-cutting watcher（cost-watcher / scope-watcher），Stop hook 触发
- **已 ship(M2.3)**：golden_rules executor + 单 critic agent 路径（借鉴 gstack-auto 6 维评分简化版：Functionality 0.30 / Quality 0.20 / Tests 0.15 / UX 0.10 / Speed 0.15 / Docs 0.10 + bug penalty）
- **未 ship(M4.5)**：phase 内 audit 矩阵 + 投票 + 共识机制
- **未 ship(M4.6)**：anti-leniency（每 audit 至少一项 CONCERN，禁止全维度高分）+ WEAK 维度强制 BLOCK

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

**里程碑落地**：M1 inbox/outbox protocol（已 ship）+ 简易 ABC 选项；信任档位 + tweak 句注入未 ship（M4.5+）。Telegram channel adapter 仍是 M2+ stub。

#### 顺序约束

L1 → L2 → L3，不并联。L2 启动前 L1 已通过；L3 启动前 L2 已议事完毕但拍不了板。

> **痛点 11 直接对应**：旧方案靠"人持续在场做品味与方向校准"；ccteam 把它分解到三层独立机制——L1 兜系统性偏差、L2 兜单 agent 偏差、L3 兜前两层都拍不了板的偏差。

### 3.7 Cross-project Memory（差异化护城河）

> **架构沿革**:放弃自建索引/向量库,主路径完全复用 Claude Code 官方
> 记忆机制(`~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` + per-repo auto-memory),
> **检索发生在 Claude session 内部,ccteam-core 零 memory 检索代码**。决策依据见
> `references/research/claude-code-memory-research.md` 末尾「M4 决策依据」节;
> 官方文档 https://code.claude.com/docs/en/memory。

**两条共享通道(官方 first-class 机制)**:

| 通道 | 路径 | 加载方式 | ccteam 用法 |
|---|---|---|---|
| 项目内累积 | `~/.claude/projects/<encoded>/memory/MEMORY.md` + topic 文件 | 每 session 启动加载前 200 行 / 25KB,topic 文件按需 | retro phase prompt 引导 Claude 用 `/memory` 自写 |
| 跨项目共享 | `~/.claude/rules/ccteam-lessons-<team>.md`(支持 `paths:` frontmatter scope) | 每 session 启动加载,匹配路径才生效 | retro phase prompt 引导 Claude 用 `Edit` 写入 marked section;Seed/verdict 自动注入 |

**写入时机**(全部经 Claude session 内官方接口,不走 ccteam 代码):
- 每个项目终态(shipped / rejected / escalated)触发 retro phase
- phase prompt 引导 Claude:
  - 项目特定 lessons → `/memory` 写本仓 auto-memory(Claude 自主决策何时写)
  - 跨项目 lessons / 反模式 → `Edit ~/.claude/rules/ccteam-lessons-<team>.md`(限 `<!-- ccteam-managed:lessons -->` marked section,不污染用户其他段)
- schema 字段从 `team.yaml.retro_schema[]` 读(已 ship,F20);dev 写 tech-stack/坑/成功设计/不要再做,
  research 写方法学/数据源/假设结果

**召回时机**(全部经 Claude session 内官方接口):
- Seed/verdict phase 启动时:rules 已通过加载机制自动注入(零 RPC),Claude 直接看到上下文
- 需深挖本项目历史 → Claude 用 `/memory` 浏览 + `Read` 读 topic 文件
- 命中相似失败项目 → verdict 倾向 REJECT/CLARIFY

**可选增强**(用户装了 [claude-mem](https://docs.claude-mem.ai/usage/search-tools)):
- claude-mem 自带 5 个 hook(SessionStart/UserPromptSubmit/PostToolUse/Stop/SessionEnd)自动捕获,
  ccteam 不调任何 hook;暴露 4 个 read-only MCP tool(`search` / `timeline` / `get_observations` / `__IMPORTANT`)支持跨项目 FTS5 检索 + type 过滤(bugfix/feature/decision/discovery/refactor/change)
- phase prompt 提示"如检测到 `mcp__*claude-mem*search` 工具,可用于跨项目深度检索",**LLM 自看 tool surface 决定调不调**;ccteam 不写检测代码,不写集成代码
- 用户没装则 100% 走默认路径,功能不受影响

**ccteam 实际改动量**(已 ship,M4.1–M4.4):
- M4.1 retro phase prompt(纯 markdown)
- M4.2 `ccteam doctor --install-memory-bridge`(创建 rules 占位文件 + marked section + path frontmatter,**唯一一段 ccteam 代码**)
- M4.3 Seed/verdict phase prompt(纯 markdown,含 conversation continuity——M4.6 已折叠进 M4.3)
- M4.4 容器 bind-mount `~/.claude/` spike(已验证 rules + claude-mem hook 在 `--dangerously-skip-permissions` 容器内可见)

### 3.8 用户接口层

> **架构沿革**:原架构把"用户接入"等同于"用户自己的 daily-driver
> claude 会话",这个假设包含了"用户必须坐在电脑前"。复盘 + 现代 agent
> 产品(openclaw / hermes-agent / Claude Code 官方 TG)实践后,改成
> **meta-agent session(ccteam 自己 manage 的常驻 claude 会话)+ Channel
> Layer(M2+ 接 Telegram / Feishu 等)**。详见 §2.1 三层架构。

#### claude session 架构层级(全系统视角)

ccteam 全系统 6 类 claude 会出现的位置:

| 层 | 是 claude 吗 | 常驻 / 短命 / 外包 | 何时出现 |
|---|---|---|---|
| **L0 Channel Layer** | **不是**(各 channel 适配器进程,无内嵌 LLM,Symphony 反模式禁止) | 适配器进程随用户配置启动 | M2+ stub,大概率复用开源方案 |
| **L0.5 meta-agent session** | **是**(ccteam-managed 常驻 tmux + claude) | 常驻、永不 terminal | 已 ship(M1) |
| **L1 编排层**(orchestrator daemon) | 不是(Rust) | 常驻 | ccteam start 后 |
| **L2 项目级 claude**(每项目一个 tmux session) | 是 | 常驻(长 session,直到 ship/abort) | ccteam new 后 |
| **L3 phase 内 agent team / subagent** | 是(Task 工具启动) | 短命(phase 内,跑完返回总结即销毁) | subagent 已 ship(M2);agent_team 永久 deferred(spike A,docs/v0-1/m2-agent-team-spike.md) |
| **L4 multi_session 子模块 claude** | 是(每子模块一个完整 session) | 常驻 | 未 ship(M4.8) |
| **L5 横切短命 claude**(cost-watcher / scope-watcher / drift-detector) | 是 | 短命(Stop hook 触发,跑完即退) | 已 ship(M1) |

**关键原则不变**:**ccteam 不在适配器进程内嵌 LLM**——所有 NL 处理都
落到 ccteam-managed 长会话(L0.5 / L2 / L4)上,channel 层(L0)是
dumb router。这条贯彻到底,避免 Symphony 多层 agent 反模式
(CLAUDE.md §六、tech-design §10)。

**meta-agent session 与项目 session 的差异**:

| 维度 | meta-agent session(L0.5) | 项目 session(L2) |
|---|---|---|
| 生命周期 | 永不 terminal,跟用户 ccteam 实例同寿 | ship / abort 即终态 |
| 行为模式 | 事件循环(等输入→处理→等输入)| phase DAG(plan-eng → ... → ship) |
| 主要工具 | `ccteam-control` skill(已 ship,M1.8)/ `ccteam-mcp`(已 ship,M2.8)/ 跨项目 lessons(已 ship,M4 走 `~/.claude/rules/` + auto-memory)| 项目级文件操作 / 内嵌 plugin agents(已 ship,V0.2 M0.20 改走 `enabledPlugins` 写到 `<project>/.claude/settings.json`,Claude Code in-memory plugin pipeline 自动 namespace `<plugin>:<name>`,不再 ln -sf 进 `~/.claude/agents/`) |
| context reset | 60% 阈值时桥接 CLAUDE.md(M0.10 已 ship);跨项目记忆通过 `~/.claude/rules/ccteam-lessons-<user>-meta.md` 滚动累积(M4 路径,无独立 conversation-log) | 60% 阈值时把当前 phase 进度写 CLAUDE.md(已 ship,M0.10) |
| 用户 attach | `tmux attach -t ccteam-meta-<user>`,直接 NL 对话 | `tmux attach -t ccteam-<team>-<slug>`,可介入项目执行 |

#### CLI(已 ship,M0)

```bash
ccteam new "做一个本地书签管理器"     # 写 inbox(无 LLM,纯薄壳)
ccteam ls                              # 查所有项目状态
ccteam show <slug>                     # 详情
ccteam progress <slug> --tail          # 实时 tail progress.jsonl
ccteam answer <slug> "用 PWA"          # 回应 clarify 问题
ccteam attach <slug> / peek <slug>     # 介入 / 瞄一眼
ccteam start / stop                    # orchestrator 生命周期
```

**关键约束**:CLI 必须输出 LLM 友好的结构化数据——所有查询命令支持 `--format json`(详见 [interfaces.md §10](./interfaces.md#10-cli-命令签名))。理由:让用户自带 claude 通过 Bash 工具调时不用解析表格。

#### meta-agent session + inbox/outbox 协议 + ccteam-control skill(已 ship,M1)

> **架构沿革**:原 M1 把"Telegram bot 实现"列为核心任务。现在
> Telegram bot **下沉到 Channel Layer(M2+ stub)**;M1 只交付能跑 NL 对话
> 的最小集合:meta-agent 长会话 + inbox/outbox 文件协议 + ccteam-control
> skill。

- **meta-agent session**(已 ship,M1.0):ccteam-managed 常驻 tmux session,
  跑 `claude --dangerously-skip-permissions`,装 `ccteam-control` skill。
  用户用 `tmux attach -t ccteam-meta-<user>` 在终端 NL 对话,meta-agent
  调 ccteam CLI 派单 / 查项目 / 跨项目召回(详见 development-plan §3 M1)
- **inbox/outbox 文件协议**(已 ship,M1.1):`<session>/.ccteam/inbox/msg-<n>.md`
  接收 NL 消息,`outbox/reply-<n>.md` 推回应。orchestrator inotify watch
  inbox,触发 send-keys 注入到对应 session;session 写 outbox,
  Channel Layer(M2+ stub)读 outbox 推到对应 channel。**M1 不实现具体
  channel,只把协议钉死**
- **ccteam-control skill**(已 ship,M1.8):描述 ccteam CLI 命令清单 +
  典型工作流。**首要 consumer 是 meta-agent session,次要 consumer
  是用户自己的 daily-driver claude**(详见 [interfaces.md §11](./interfaces.md#11-ccteam-control-skillm1))
- **CLARIFY 多轮**(推至 channel 层):channel 层落地后再设计;当前用
  "tmux attach 直接对话"覆盖

#### Channel adapters + ccteam-mcp MCP server

- **Channel adapter 实现**(M2+ stub,未 ship):Telegram bot / Feishu bot 等。
  **强烈倾向直接复用开源方案**(Claude Code 官方 TG channel /
  python-telegram-bot 等),做最薄的 adapter 层:订阅外部消息 → 写到对应
  session 的 inbox / 从 outbox 推到对应 channel。无内嵌 LLM
- **`ccteam-mcp` MCP server**(已 ship,M2):暴露 9 个 structured tool
  (`ls` / `show` / `new` / `peek` / `progress` / `pause` / `resume` /
  `inject_decision` / `send_to_session`,详见
  [interfaces.md §12](./interfaces.md#12-ccteam-mcp-mcp-server-m2));
  meta-agent 与用户 daily-driver claude 都受益(MCP 比 shell parse 更鲁棒)

#### 为什么"用户自带 daily-driver claude"不再是核心入口

架构沿革:M0 / M1 计划曾假设 meta 层外包给用户自己的 claude session。
这个假设的隐性前提是**用户必须在电脑前**。复盘后发现:

1. **手机 / 离场场景**用户也想 NL 调度——这要求 ccteam 自己 manage
   一条常驻 claude 会话,channel 层翻译外部消息进入
2. **多通道收敛**:用户在终端 attach + 在手机 Telegram + 在公司 Slack
   三处对话,**不能各起一个 LLM 上下文**——必须收敛到一份 meta-agent
   session
3. **不在适配器嵌 LLM** 的红线没动——meta-agent 仍然是**一份** claude
   session,channel 是 dumb router

**用户自带 daily-driver claude 仍然有用**:用户在自己电脑前已经开了一
个 claude 处理别的事,装 `ccteam-control` skill 后随时可调度 ccteam,
这条**作为辅助路径保留**,但不是 ccteam 的核心入口。核心入口是
meta-agent session + Channel Layer。

#### Web 仪表盘(V0.3 M5.1+ 已 ship,read-only;M5.2/M5.3 后续 ship live + write)

V0.2 / V0.2.2 之前曾把 web 仪表盘剥离主线(`ccteam ls --format json` + 用户自带 claude 已覆盖"用人话汇报")。V0.3 重新评估:多项目并发(M3+ team factory ship 后用户实际跑 ≥ 3 项目)与离场场景下,「一屏看全局」需要可视化聚合;`ccteam ls` 表格在 ≥ 5 项目时阅读成本陡增。**因此 V0.3 ship `cct web`,作为第四类用户接入面**,与 CLI / MCP / filesystem 共存,各自最强项不同(终端 = power user 全控;MCP = meta-agent 自动化;filesystem = hooks / 调试;web = 一屏总览 + 局域网 / 手机访问)。

实现栈:axum 0.8 + askama 0.12 + htmx 2.0.4(vendored,~50 KB),无 npm / Vite / build toolchain;`include_bytes!` 把 htmx + CSS 打进 binary,模仿 V0.2.2 F38 vendored TTF 模式。`ccteam-web` 是独立 workspace crate,**dep 只**到 `ccteam-core`,不依赖 `ccteam-cli`(binary-as-library 反模式;`tests/dep_graph_test.rs` 锁红线)。

V0.3 ship 状态:

| Milestone | 范围 | 已 ship |
|---|---|---|
| **M5.0** | crate scaffold + write helper promote 到 `ccteam-core::actions` + `GET /health` | ✅ |
| **M5.1** | read-only dashboard:`GET /` 项目列表 + `GET /project/<slug>` 详情 + `GET /assets/{file}` 静态资源 + 状态 badge(F35 silence_classifier 只读复用)+ outbox 渲染(`SessionMailbox`)| ✅ |
| **M5.2** | SSE 实时事件流(`/sse/all` + `/sse/project/<slug>` 单 notify watcher fan-out)+ 按需 PNG 截图(`/screenshot/<slug>.png` 复用 F38 `render_screenshot`)| 计划中 |
| **M5.3** | 写动作(`POST /api/<slug>/{btw,inject_decision,pause,resume}` 全走 `ccteam_core::actions::*` M5.0 promote)+ token 鉴权(loopback 免 token / 非 loopback 默认 token,`Authorization: Bearer ccteam:<token>`,`subtle::ConstantTimeEq` 比对,`~/.ccteam/web-token` mode 0600)| 计划中 |
| **M5.4** | E2E + retro + workspace.version `0.2.2` → `0.3.0` ship gate | 计划中 |

V0.3 主要红线(详 PRD §3 / `interfaces.md` §15 / CLAUDE.md §三):

- **`progress.jsonl` 是 SoT**:web 层走 `ccteam_core::collect_recent_events`,**不解析 tmux 终端输出**(M5.2 截图通过 `ccteam_core::render_screenshot` 内部 vt100 化)
- **永不主动 kill**:M5.1 read-only;M5.2/M5.3 加 SSE / 写动作时仍守红线 — 仅走 `actions::*`(inbox + state.json 控制平面),不发 SIGINT / `tmux kill-session`
- **status badge 是只读 label**:即使 `silence_classifier::classify` 返 `PostStopLimbo` / `SubagentRunaway`,web 层 **不**调 `LimboAction::from` / 重新注入 — orchestrator 持续走 F35 副作用路径
- **dep graph**:`cargo tree -p ccteam-web | grep ccteam-cli` 必须 0 命中

#### 前端层(可插拔)

ccteam 核心(orchestrator + tmux + hooks)是 **headless 状态引擎**——所有 UI 都是可插拔前端,共用 `ccteam-core` lib API。分层关系:

```
+----------------------------------------------------------+
|  前端层(可插拔,M0 起严格 lib/binary 分离)               |
|                                                          |
|  ccteam CLI       ccteam tui (M4.9)   ccteam serve (backlog) |
|  (已 ship)        ratatui 仪表盘      xterm.js + WS bridge|
|       \                |                    /            |
|        \               |                   /             |
|         v              v                  v              |
|  +----------------------------------------------------+  |
|  |          ccteam-core(Rust lib crate)              |  |
|  |  get_state / list_projects / submit_control /     |  |
|  |  tail_progress / attach_progress(stream)          |  |
|  +----------------------------------------------------+  |
|                       |                                  |
|                       v                                  |
|  +----------------------------------------------------+  |
|  |   orchestrator daemon(L1,常驻 Rust + tokio)       |  |
|  |   tmux + hooks + progress.jsonl + state.json      |  |
|  +----------------------------------------------------+  |
+----------------------------------------------------------+
```

**Warp / iTerm2 / Alacritty 等本地终端 = 用户终端选择,不是 ccteam 集成对象**。`ccteam attach <slug>` 在任何 tmux 兼容终端里行为完全一致——ccteam 透明兼容,无需特殊适配。用户偏好哪个终端就用哪个,与 ccteam 无关。

**前端档位**:

| 前端 | 里程碑 | 性质 | 实现栈 |
|---|---|---|---|
| `ccteam` CLI | 已 ship(M0) | 关键路径(默认入口) | clap derive + serde |
| `ccteam tui` | 未 ship(M4.9) | 机会主义,非关键路径 | ratatui + crossterm |
| `ccteam serve`(web dashboard) | backlog(§11) | 机会主义,非关键路径 | axum + WebSocket + xterm.js |

##### 前端层 invariant(红线)

任何前端(CLI / TUI / web dashboard)**不得**在 ccteam 内引入新 LLM 层。

- ✅ web dashboard 通过 xterm.js + WebSocket 桥**直通到 tmux 内的项目级 claude**——等价于"远程版 `ccteam attach`"。用户在浏览器键入 = 通过 send-keys 注入 tmux,不经任何 ccteam 中介 LLM
- ✅ web 介入触发 `PreToolUse` hook 检测 user_attach,自动暂停 phase(与本地 attach 语义一致)
- ❌ 不在 ccteam 层起 meta-claude / 自实现聊天 UI / 翻译用户 prompt(已被否决的 `ccteam chat` 路径复活)

LLM 推理只发生在两处:① **L2 项目级 claude**(tmux 内) ② **L0 用户自带 claude**(机器上的 `claude` 进程)。这条 invariant 与 §3.8 上方"ccteam 自始至终不自造 AI"原则一脉相承——前端层加再多花样,核心 headless 引擎都不让步。

##### 抄作业指针:`references/agent-of-empires/`

未来 ratatui TUI(M4.9)与 web dashboard 的前端栈实现**直接抄** `references/agent-of-empires/`(已 clone 到本仓库,`.gitignore` 屏蔽不入仓库):

- 栈与 ccteam 完全对齐:Rust + ratatui + crossterm + tokio + axum(ws)
- 抄的范围:`Cargo.toml` dep 组合 + ratatui 主循环范式 + WebSocket bridge 实现 + 它的 `docs/guides/web-dashboard.md`
- **不抄核心**:9-phase 编排、Seed Gate、跨项目 lessons(走官方 `~/.claude/rules/` + 可选 claude-mem)、Defense in Depth 是 ccteam 差异化护城河,AoE 没有

详见 development-plan M4.9 任务说明。

### 3.9 Watchdog(translation-only smart layer,V0.2 M0.21)

> **架构沿革**:meta-agent(§3.8)主路径是"用户主动问 → meta-agent 答"。
> 但 ccteam 的疼点之一是**没人值守时项目静默卡死**——L2 hooks 只能记录,
> 没法主动捅醒用户。V0.2 把"低层信号 → 用户能读懂的 NL"这一步独立出来叫
> **watchdog**:不是新组件 / 新进程,而是 meta-agent 的一个角色面 +
> 一组 ccteam Rust 函数。

**translation only 红线**(本文 §1 表格新增条):
- ❌ watchdog 不调 orchestrator API、不写 progress.jsonl、不 kill session、不 re-inject prompt
- ❌ watchdog 不替用户拍板("该不该 attach"、"该不该 kill"、"该不该改方案")
- ✅ watchdog 只读 4 个数据源,翻译成 NL,推到 meta-agent 自己的 outbox(§3.4.3)

**4 个数据源**(全是只读):

| 信号 | 路径 | 来源 milestone |
|---|---|---|
| `needs_attention` | `<project>/.ccteam/needs_attention.outbox.json` | M0.19 Stop hook L3 兜底 |
| `auto_loop_cycle` | `<project>/.ccteam/auto-loop.state.md::iteration` | M0.12 ralph-loop |
| `cost_overrun` / `phase_duration_overrun` | `<project>/.ccteam/state.json::cost_used_usd` / `last_progress_event_at` | 一直有 |
| `daemon_down` | `~/.ccteam/state/orchestrator.heartbeat` mtime | M0.23.1 |

**信号源选择**(详见 `docs/v0-2/alignment-review.md` §3.3):
**不用 SessionEnd**——其 `exit_reason` 6 个枚举全是用户主动事件,stall 不触发。
靠外部 timer + Stop hook L3 兜底就够了。

**用户配置**:`~/.ccteam/watchdog.yaml`(interfaces.md `watchdog.yaml schema`):

```yaml
notify_on_cycle_count: 2          # 默认 cap-1=2
notify_on_phase_cost_usd: 30.0    # USD,可选
notify_on_phase_duration_min: 60  # 分钟,可选
notify_mode: normal               # quiet / normal / verbose
```

`quiet` 模式只放行 `cost_overrun` + `daemon_down`(钱 / 守护死必报);
`verbose` 不去重,每次扫描都重发 `needs_attention`。

**触发**(M0.21):**手动**——meta-agent 自己跑 `ccteam watchdog scan` 这条命令。
M2+ channel layer 上线后会有 cron-style 自动触发(60s 默认推荐;
当前 milestone 不实现自动 timer)。

**实施要点**:
- 全部代码在 `crates/ccteam-core/src/watchdog.rs`(单文件,~600 行)
- `crates/ccteam-cli/src/main.rs::Command::Watchdog::Scan` 暴露 CLI:
  `ccteam watchdog scan [--push --user <handle>]`
- meta-agent role prompt(§3.8 引用)新增 §7 描述 watchdog 角色边界
- `crates/ccteam-core/src/orchestrator.rs` **零** watchdog 引用
  (grep `watchdog` 命中 0 次是核心红线)

---

## 4. 关键流程

### 4.1 端到端：从想法到交付（Happy Path）

```
T+0:00  用户在 Telegram 发："做个本地书签管理器，离线可用"
T+0:00  bot 写 ~/.ccteam/inbox/20260504-bm.md
T+0:30  orchestrator 轮询发现新文件 → triage → 分配 slug
T+0:35  启动 tmux session ccteam-dev-bookmark-mgr-a3f9 + send-keys 注入 Seed prompt
T+1:30  Seed 输出 verdict: PASS，建议技术栈：Vite + Dexie + PWA
T+1:35  Seed phase 启动；~/.claude/rules/ccteam-lessons-dev.md 自动注入（含 PWA 离线缓存 lessons）+ auto-memory 加载
T+1:35  写 spec.md 合并，进入 Plan phase
T+3:00  plan-eng 完成
T+3:00  Implement phase 启动（solo session；subagent ad-hoc 启动按需）
T+25:00 实现完成，写 implement-report.md
T+25:30 test-author phase 编测试
T+30:00 test-run phase 全绿 → review
T+33:00 review approved
T+33:30 golden_rules executor pass → ship phase
T+34:00 git tag v0.1.0；ship phase inline retro：Claude 调 /memory 写本仓 auto-memory + Edit ~/.claude/rules/ccteam-lessons-dev.md（marked section）
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
5. claude 产出文件落到 `~/projects/<team>-<slug>/.ccteam/<phase>-report.md`。
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

完整字段、JSON schema、文件命名规则、事件类型清单 → **[interfaces.md](./interfaces.md)**。本节只保留架构约束:

| 子节 | 架构约束 | interfaces.md 章节 |
|---|---|---|
| §5.1 全局目录布局 | `~/.ccteam/` 是单一根;不跨用户共享 | [§1.1](./interfaces.md#11-全局目录ccteam) |
| §5.2 项目级 state.json | 原子写(`.tmp` + rename);`phase_state` 三态(`in_flight` / `idle` / `fix_locked`);损坏走 backup | [§2](./interfaces.md#2-state-协议) |
| §5.3 Inbox 协议 | 文件名 `<ISO-timestamp>-<random>.md`,原子写 | [§3.1](./interfaces.md#31-inbox) |
| §5.4 控制协议 | orchestrator 30s 扫,处理后**删除文件**(幂等) | [§3.3](./interfaces.md#33-control用户--orchestrator) |
| §5.5 Progress.jsonl | **唯一状态事实来源**——orchestrator 只读这一个文件做状态转移;tmux 终端输出不参与状态判定 | [§4](./interfaces.md#4-progressjsonl-事件流) |

§5.5 关键论证(留本节,详见 interfaces §4):**"progress.jsonl 唯一事实来源"是架构红线**。曾经考虑过解析 tmux capture-pane 输出做状态判断——拒,因为终端文本格式不稳定、ANSI 转义难、对 prompt cache 表现敏感。所有状态转移走 hook 写出的 JSONL,deterministic 且可重放。

---

## 6. Claude Code 扩展点映射

### 6.1 Tmux 长 session 调用模板

**为什么不用 `claude -p` 子进程**：每 phase 起新进程意味着重读 CLAUDE.md / spec / 上游产物，反复触发冷启动；prompt cache 5 分钟 TTL 命中不到。长跑项目（数小时-数天）改用一个**项目级长 session**——同 session 跨 phase 共享缓存，且天然支持随时 attach 观察与介入。

#### 项目首次启动

```bash
TEAM="dev"
SLUG="bookmark-mgr-a3f9"
PROJECT_DIR="${HOME}/projects/${TEAM}-${SLUG}"

tmux new-session -d \
  -s "ccteam-${TEAM}-${SLUG}" \
  -c "${PROJECT_DIR}" \
  "claude --dangerously-skip-permissions"

# 等 SessionStart hook 写 ready 标记
while ! [ -f "${PROJECT_DIR}/.ccteam/ready" ]; do sleep 1; done
```

#### 注入 phase prompt

推荐用 `@文件引用` 而非 send-keys 大段文本，避免转义问题：

```bash
PHASE="03-implement"
tmux send-keys -t "ccteam-${TEAM}-${SLUG}" \
  "请按 @.ccteam/phases/${PHASE}.md 完成本阶段。完成后写 .ccteam/${PHASE}-report.md，并在最后单独输出一行：PHASE_DONE: ${PHASE} （或 ESCALATE: <一句话原因>）。" \
  Enter
```

`PHASE_DONE` / `ESCALATE` 这一行作为终态信号——Stop hook 检测到 → 写 progress.jsonl → orchestrator 读到 → 注入下一个 phase。

#### 多 pane 仪表盘布局（用户 attach 时一屏看全）

```bash
# 主 pane：claude 交互
# 右上 pane：progress.jsonl 实时滚动
tmux split-window -h -t "ccteam-${TEAM}-${SLUG}" -p 30 \
  "tail -f ~/.ccteam/progress/${TEAM}-${SLUG}.jsonl | jq -c '.ts + \" \" + .event + \" \" + (.tool // .note // \"\")'"

# 右下 pane：成本累计 / 当前 phase 计时
tmux split-window -v -t "ccteam-${TEAM}-${SLUG}":0.1 -p 50 \
  "watch -n 5 'jq -r \"[当前 phase: \" + .current_phase + \" | 累计: \\$ \" + (.cost_used_usd|tostring) + \"]\" ~/projects/${TEAM}-${SLUG}/.ccteam/state.json'"
```

#### 断开后重连

```bash
# orchestrator 重启时
if tmux has-session -t "ccteam-${TEAM}-${SLUG}" 2>/dev/null; then
  echo "tmux session 仍在，直接续接（无需操作）"
else
  # session 丢失，用 --resume 在新 tmux 起新 claude 进程恢复对话历史
  CLAUDE_SESSION=$(jq -r .claude_session_id "${PROJECT_DIR}/.ccteam/state.json")
  tmux new-session -d -s "ccteam-${TEAM}-${SLUG}" -c "${PROJECT_DIR}" \
    "claude --dangerously-skip-permissions --resume ${CLAUDE_SESSION}"
fi
```

`--resume` 让 Claude Code 重新加载完整对话历史——cache 仍要预热一次（cold start），但工作记忆不丢。

#### 用户介入

```bash
ccteam attach <slug>     # = tmux attach -t ccteam-<team>-<slug>
# 用户键入文本 → claude 当作 prompt 接收
# Ctrl+B D 离开（claude 继续跑）
```

orchestrator 通过 `PreToolUse` hook 检测最近一次输入源：若来自人（vs. 来自 send-keys 时盖的 marker），自动暂停 phase 推进，等 `ccteam resume <slug>` 或用户 detach 超过 N 分钟（视为放权）。

#### 关键约束

- ✅ 用 `--dangerously-skip-permissions`（消灭弹窗，痛点 8）
- ✅ **默认开 1M 上下文**：长跑必备，给 cache 足够空间；超过 60% 在 phase 边界 reset（详见 §6.9）
- ❌ **不**用 `claude -p`（失去 attach / 介入能力）
- ❌ **不**设 `--max-turns`（用户要求长跑，由 stall + 成本上限兜底）
- ❌ **不**设 `--max-budget-usd`（同上；改用 hooks 累计 + 软告警，见 §6.8）

**实现注**:orchestrator 的 Rust 实现用 `tokio::process::Command` 包装上述所有 tmux 命令(`new-session` / `send-keys` / `split-window` / `has-session` 等),异步 spawn + 收集 stdout/stderr,失败落 tracing 日志——单 binary 零额外运行时依赖。

### 6.2 Hooks 配置

完整 `settings.json` 模板、Hook 事件用途表、`cost-accumulate.sh` 工作原理 → **[interfaces.md §6](./interfaces.md#6-hooks-配置-schema)**。本节只保留架构论证:

**为什么 hooks 是 ccteam 可观测性命脉**:Claude Code hooks 是 deterministic 的(详见 claude-code-best-practices §4.5)——同一事件触发同一脚本,这是把"AI 的随机推理"转成"系统可处理的事件流"的桥。ccteam 把所有 phase 边界 / 工具调用 / 退出信号都通过 hooks 落到 progress.jsonl,orchestrator 据此做状态转移,完全不解析 tmux 终端文本。

**实现形态**:hook 实现是 `ccteam hook <name>` 子命令(如 `ccteam hook progress-append` / `ccteam hook parse-phase-end` / `ccteam hook cost-accumulate`)——单 binary 分发,与 orchestrator 共享同一份 serde schema(progress.jsonl 事件定义、state.json 字段),不再依赖独立 bash / python 脚本运行时。official plugin 自带的 hook(如 `security_reminder_hook.py`)通过 shell shim 包装挂上,不直接依赖。

**Hook 写作纪律**(实现 PR 必须遵守):
- append 类必须 `async: true`——别拖慢主流程
- 解析 `PHASE_DONE` / `ESCALATE` 的 hook 设 `timeout: 10`,失败要落日志
- hook 脚本放 `~/.ccteam/hooks/`,不放项目目录(避免被 claude 自己改)
- `Stop` 一个 entry 内可挂多 command,但**`decision: block` 决策只能由 `parse-phase-end.sh` 单点输出**(详见 §3.5);其它 command 必须 `async: true` 仅做 append/log

**cost 来源关键事实**(写代码前必须知道):Claude Code **不**在 hook 输入里给 `cost_usd`——必须从 `transcript_path` 读 JSONL 解析 `usage.*` 自算。完整流程见 [interfaces.md §6.3](./interfaces.md#63-cost-accumulatesh-工作原理)。

### 6.3 Multi-agent 编排（phase 内并行 + cross-cutting watcher）

ccteam 用 multi-agent 编排同时承担两个不同目标——**质量**（痛点 11 L2，多视角议事）与**速度**（痛点 13 L 加速，多角色并行）。两个目标用同一个 Agent Teams 机制实现，但 phase prompt 中表达不同：

| 目标 | 多 agent 干啥 | 典型 phase | 痛点 |
|---|---|---|---|
| **质量**（垂直） | 看同一份输入，各视角审 | review、plan-eng | 痛点 11 |
| **速度**（水平） | 各做不同事 | implement | 痛点 13 |

两个目标的 multi-agent **可同 phase 共存**——例如 implement phase 启 `backend-dev`/`frontend-dev`（速度）+ `reviewer` 旁路审产物（质量）。

下面三种模式并存：

#### 模式 A：Phase 内 agent team（永久 deferred,见 spike A）

> **现状**:`parallelism: agent_team` 启用路径**永久 deferred**——
> Claude Code 当前版本无 first-class CLI 表面让 phase prompt 内显式
> 调度多 sub-agent 协作(详见 docs/v0-1/m2-agent-team-spike.md 决策记录)。
> 概念槽位保留在 phase front matter 与 §6.11 三档叠加体系中,但 ccteam
> 不依赖此机制——phase 内"多角色议事"通过 ad-hoc subagent(Task 工具)
> 与 cross-cutting watcher(模式 B)实现。

历史设计(参考):在 `implement` / `review` 这种复杂 phase 里启用 Claude Code 的 Agent Teams 实验特性:

```bash
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude -p ...
```

phase prompt 显式调用:

> 你作为 implement phase 的 team-lead。
> 启动两个 sub-agent：
> - `backend-dev`：实现 API 层
> - `frontend-dev`：实现 UI 层
> 让他们并行工作。每 2 次工具调用后写 progress.md。

**当前替代**:在 tmux 长 session 内,主 agent 通过 `Task` 工具 ad-hoc 启 subagent(code-explorer / code-architect 等);并行多视角 audit 走 cross-cutting watcher(模式 B,Stop hook 触发,跨 phase)。

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

#### Subagent 与 Agent Teams 的叠加（概念上不互斥;agent_team 路径永久 deferred）

Agent Teams 是 phase 内的横向多角色编排（永久 deferred,见模式 A 注），subagent 是任何 agent 内的纵向 context 节流（已 ship,M2）。两者职责正交,概念上可叠加:
- 例（若 agent_team 未来重启）:implement phase 启 Agent Teams（`backend-dev` ∥ `frontend-dev` ∥ `reviewer`），**backend-dev 内部**同时用 `Task(subagent_type=code-explorer)` 启 subagent 研究"我们 codebase 怎么用 SQLAlchemy"——主线写代码，subagent 跑研究后返回结构化总结，不污染 backend-dev 自己的 context
- subagent **不在 phase 协议中声明**——任何 agent 在任何时刻都可 ad-hoc 启动；只受 `max_subagents_per_phase` 资源约束（详见 §6.11）

#### 与 Sub-skill 调度的边界

agent（本节）= 在 phase 内或后台**并行**跑的 multi-agent；sub-skill（§6.10）= phase 进入/完成时**串行**调用的工作流单元（如 code-reviewer 跑完输出文件给下个 phase）。两者协作但不重叠：
- 每个 phase 可同时启用 phase 内 agent team（并行 implement）+ sub-skills（串行 review/qa）+ cross-cutting watcher（后台监督）
- phase 协议 front matter 同时支持 `parallelism` / `agent_team` / `sub_skills` 三个字段

### 6.4 MCP servers

#### 消费的 MCP(ccteam 不写,只接)

| MCP | 用途 | 出处 |
|---|---|---|
| **Telegram bot** | 通知 + 接收用户消息 | Channel Layer M2+ stub |
| **claude-mem** | 跨项目记忆**可选增强**(read-only MCP search / timeline / get_observations + 自带 hook 自动捕获);ccteam 不写集成代码,LLM 自看 tool surface 决定用不用 | 已 ship 为可选项(M4)——默认路径走官方 `~/.claude/rules/` + auto-memory,装了 claude-mem 自动叠加 |
| **Playwright** | E2E 测试(前端项目) | 已有 |
| **GitHub** | PR 创建、issue 管理 | 可选(优先 `gh` CLI) |

#### 提供的 MCP:`ccteam-mcp`(已 ship,M2,ccteam 自己暴露)

暴露 9 个 structured tool(`ls` / `show` / `new` / `peek` / `progress` / `pause` / `resume` / `inject_decision` / `send_to_session`)。**两个消费者**:

1. **用户自带的 Claude Code session**(主消费者)——在任意目录开 `claude`,通过 MCP 调度 ccteam,完成"用对话方式管 ccteam"的体验。这是 §3.8"用户自带 claude 当入口"路径的实现层
2. **项目级 claude**(次要,phase 内查询)——能查"我在哪个项目里、累计 cost、当前 phase 状态",用于 phase prompt 内自检

完整 tool schema 与协议见 [interfaces.md §12](./interfaces.md#12-ccteam-mcp-mcp-server-m2)。**M0 / M1 走 CLI `--format json` 兜底路径**——用户的 claude 用 Bash 工具调即可;M2 后 MCP 路径作为首选,CLI 仍然保留作为脚本化入口。

**实现形态**:`ccteam-mcp` 与 `ccteam-core` 同 crate(workspace 内 lib + 多 binary),通过 `ccteam mcp-serve` 子命令暴露——读写同一份 state.json / progress.jsonl,**为将来 `ccteam tui`(未 ship,M4.9) / `ccteam serve` web 前端(backlog)预留同一状态读写 API**。三种前端共用 `ccteam-core` lib API(详见 §3.8 前端层小节),MCP 只是把这套 API 套上 MCP wire protocol 给外部 LLM 消费。

#### Plugin pipeline(V0.2 M0.20,候选 7)

**Spawned project session 启 plugin agent 走 `enabledPlugins` 路径,不再 ln -sf 进 `~/.claude/agents/`**。

`bootstrap_project` 写 `<project>/.claude/settings.json` 时,根据 team 的 phase YAML
`tools_required.subagents` + `sub_skills` 解析依赖的 Claude Code plugin
(eg `code-reviewer` → `pr-review-toolkit@claude-plugins-official`),写入
`enabledPlugins: {"<plugin>@<mkt>": true}`。Claude Code session 启动时
in-memory plugin pipeline 加载 enabled plugin,**自动加 `<plugin>:` namespace**
(eg `pr-review-toolkit:code-reviewer`);phase markdown 用裸名
`Task(subagent_type="code-reviewer")` 仍然可调,plugin pipeline 自匹配。

- 静态映射表:`crates/ccteam-core/src/plugin_resolution.rs`
  (`KNOWN_PLUGIN_AGENTS` const,8 个 `claude-plugins-official` agent;V0.3 改运行时发现)
- doctor `--tool-surface` 校验:`enabledPlugins` 引用的 plugin source 文件
  存在于 `~/.claude/plugins/marketplaces/<mkt>/plugins/<plugin>/agents/<name>.md`
- doctor `--migrate-recommended-agents`:一次性清理 V0.1 留下的
  `~/.claude/agents/` ln -sf(只删指向 marketplace 的 symlink,
  操作员手写文件保留)

**ccteam-core 不再写 `~/.claude/agents/`**——M4 红线"零检索 + 不写程序读 memory 文件"
扩展到 plugin pipeline:plugin 装载交还 Claude Code 官方 in-memory pipeline,
ccteam 只声明依赖。

### 6.5 项目级 CLAUDE.md（每项目自动生成）

orchestrator 在 plan phase 后写入：

```markdown
# CLAUDE.md (auto-generated by ccteam)

## 项目上下文
- slug: dev-bookmark-mgr-a3f9
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

## 跨项目经验（来自 ~/.claude/rules/ccteam-lessons-<team>.md 自动注入 + per-repo auto-memory）
{{ 召回的 top-3 patterns 摘要 }}
```

### 6.6 A2A bridge（可选，未 ship）

如果未来需要"两个 ccteam 实例对话"（例如本地 ccteam 和云端 ccteam 协作），用 A2A bridge 协议。当前不需要。

### 6.7 Skills 复用（gstack 模式）

ccteam 出两个 skill:

#### `ccteam-phases`(phase prompt 分发,非守护模式 fallback)

```
~/.claude/skills/ccteam-phases/
├── SKILL.md           # 元数据
└── phases/
    ├── 02-plan-eng.md
    ├── 03-implement.md
    ├── 04-test-author.md
    ├── 05-test-run.md
    ├── 06-fix.md
    └── 09-ship.md
```

用户在自己的 Claude Code 里手动喊 `/ccteam-implement` 等,跑单 phase——作为不起 daemon 的 fallback。product-research team 的 phase 序列(`01-kickoff` / `02-market-survey` / `03-differentiation-analysis` / `04-value-proposition` / `05-feasibility` / `06-verdict`)走 `phases-product-research/` 子目录。

#### `ccteam-control`(已 ship,M1+,用户自带 claude 调度 ccteam 的入口)

```
~/.claude/skills/ccteam-control/
└── SKILL.md           # CLI 命令清单 + 典型工作流 + 何时 attach vs peek
```

**用途**:用户在任意目录开 `claude` → skill 自动激活 → claude 知道:
- 怎么调 `ccteam ls --format json` 看跨项目状态
- 怎么调 `ccteam new "..."` 立项(并先多轮澄清)
- 卡住时综合 `ccteam peek <slug>` + `ccteam progress <slug> --tail` 给用户一句可贴的纠偏 prompt
- 何时该建议用户 `ccteam attach`(自己介入)vs `ccteam pause`(暂停后再决定)

这是 §3.8"用户自带 claude 当入口"路径的实现。M2 已上 ccteam-mcp MCP server(§6.4),skill 仍保留作为发现 / 引导层。

完整 SKILL.md 内容契约见 [interfaces.md §11](./interfaces.md#11-ccteam-control-skillm1)。

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

#### Daemon 健康监督（M0.23.1)

orchestrator 是**所有 phase 派发 + inbox 派送**的单点 — daemon 死了用户写的 inbox / 调的 MCP 命令会**沉默成功**(写到磁盘但永远不被消费)。M0.23.1 给一条 fail-loud 路径:

| 文件 | 谁写 | 谁读 | 语义 |
|---|---|---|---|
| `~/.ccteam/state/orchestrator.pid` | daemon 启动时写 | `ccteam stop` | PID(已有,M1.5) |
| `~/.ccteam/state/orchestrator.heartbeat` | daemon 每 30s 写 | MCP 入口 / meta-agent skill | mtime 是 liveness 唯一来源 |

**判定**:`now - mtime ≤ 60s` → healthy;否则 stale(grace 是 2× heartbeat 间隔,容忍单次 GC pause / 阻塞 IO)。文件不存在 → no_heartbeat(daemon 未启动)。

**消费规则**(action vs read-only 二分):

- **action 工具**(`pause`/`resume`/`send_to_session`/`inject_decision`)— daemon 不健康直接返回 error,**绝不**写出 inbox 文件就成功(否则用户以为消息派出去了实际烂在磁盘)。M0.23.3 也走这一条。
- **read-only 工具**(`ls`/`show`/`peek`/`progress`)— state.json 在磁盘,daemon 死也能查;`ls` 在响应里附 `orchestrator.daemon_health` 字段(`status`/`age_secs`/`message`),meta-agent 自看自决定要不要提示用户。

**红线**:health check **只 stat heartbeat 文件**,不做任何 RPC / kill -0 / tmux capture-pane。pure stat 才能放在每个 MCP 调用的 hot path。daemon 启动时立即 touch 一次心跳文件(不等 30s),所以"刚起来的 daemon" 也立刻可观察。

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
- **V0.2.2 F35 silence classifier 兜底**(详见 §3.5 末段):orchestrator
  daemon 主循环每个 tick 调 `silence_classifier::classify`,把"phase_inject 后无任何下游事件 ≥ warn 阈值" / "Stop 后 auto-loop 未推进 ≥ warn"两类
  limbo 自动 deterministic re-inject 1 次(`MAX_LIMBO_RETRY = 1`,per-phase
  计数);超 cap 写 enriched `needs_attention.outbox.json` 由 watchdog +
  meta-agent 翻译给用户。**不发 Ctrl-C / 不 kill / 不 LLM**。
- **V0.2.2 F36 send-keys subagent guard**:`dispatch_phase_with_state` 注入前
  调 `progress::subagent_active(events)`(扫末事件:`PreToolUse(tool=Task)` 减
  `SubagentStop` 配对 > 0 → true),发现子 agent 在飞时不发 send-keys、不写
  `phase_inject` 事件,改落 `<project>/.ccteam/pending-inject.json`(单文件,
  schema 详 interfaces.md §6.2.3)。daemon tick 后续在 SubagentStop 真到 + 不
  active 时真发并删本文件;`max_defer_minutes`(默认 10)兜底,超时改写 enriched
  outbox `ccteam_classification: "inject_defer_timeout"`。F35 `attempt_limbo_reinject`
  发现 pending-inject 在飞时跳过本次 retry,不烧 deterministic 预算;F36 race
  漏接(eg 子 agent 几秒后才 emit)兜底走 F35 `InjectLimbo`。

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

#### V0.2 M0.19 — auto_loop default-on 后的 phase 完结路径

`auto_loop` 默认 `true`(§3.5)后,phase 退出路径只有四种合法形态(orchestrator / Stop hook 都按这套识别):

| 出口 | 触发 | progress 事件 | 后续 |
|---|---|---|---|
| `PHASE_DONE: <phase>` | assistant 末行匹配 | `phase_done` | orchestrator 换下一 phase |
| `PHASE_DONE_PENDING — ...` | assistant 末行匹配 | `phase_done_pending` + `open_decisions` | orchestrator 看 outbox / 静等 |
| `ESCALATE: <prefix> <reason>` | assistant 末行匹配 | `escalate` | orchestrator 走 escalate 路由 |
| `<project>/.ccteam/outbox/clarify-*.md` | phase 在 phase_inject ts 之后写新文件 | (无) | orchestrator 决策队列接力 |

**没产出三种合法出口任一 → Stop hook fallback 接管**:第一次 Stop 返回 exit 2 + stderr 强制 LLM 续聊,第二次 (`stop_hook_active=true`) 写 `<project>/.ccteam/needs_attention.outbox.json` 让 watchdog(M0.21)接力 surface 给用户。**永远不出现"silent halt"**——即使 LLM 反复输出纯文本问句,撞 cycle cap 也会硬 escalate。

### 6.10 Sub-skill 自动调度（替人编排 plugin）

ccteam 不重写 gstack / claude-plugins-official 的 skill；ccteam 的差异化是**替人 orchestrate 它们的调用时机与产物接力**——直接对应痛点 12。

完整 schema(`sub_skills` 字段 / trigger 时机表 / `skill:` 路径前缀三档 / `skill_intent.yaml` 扩展协议) → **[interfaces.md §7](./interfaces.md#7-sub-skill-调度-schema)**。

本节保留架构论证:

**核心设计选择**:
- **trigger 只两档**(`phase_start` / `phase_done`)——`before_done` 需 Stop hook 拦截,等同 fix-cycle 复杂度,不开
- **产物自动接力**——orchestrator 把上一 phase 的 `output_to` 路径作为下一 phase prompt 的 `@文件引用` 自动追加,用户从头到尾不复制粘贴
- **三种复用粒度共存**——`@文件引用`(零安装) / 拷贝到项目(冻结版本) / 整 plugin 安装(后者未 ship,M4.7);`skill:` 字段路径前缀分发
- **新插件靠 `skill_intent.yaml` 自描述**——社区作者写自己的挂载推荐,ccteam 不改代码即可接入(未 ship,M4.7)

**与 §6.3 Multi-agent 编排的边界**:
- `agent_team`(§6.3) = phase 内**并行**跑的 audit/dev sub-agent
- `sub_skills`(本节) = phase 进入或完成时**串行**调用的工作流单元,产物落文件给下游用

两字段在 phase front matter 共存、互不冲突。

### 6.11 Multi-session per project（痛点 13 大项目加速；未 ship,M4.8）

适用：plan-eng 在分析 spec 时识别出"≥3 个独立子模块且接口稳定"——例如 SaaS 拆 backend-api / frontend-dashboard / mobile-app / docs。**未 ship(M4.8)**；当前默认 `parallelism: solo`(`agent_team` 槽位永久 deferred,见 §6.3 模式 A)。

#### 与 §6.3 Agent Teams 的关键区别

| 维度 | `parallelism: agent_team`（§6.3 模式 A） | `parallelism: multi_session`（本节） |
|---|---|---|
| 进程模型 | 1 session N agent | N session（独立 claude 进程） |
| Context | 共享 1M 主 session 上下文 | 每 session 独立 1M context |
| Cache | 高效复用主 session prompt cache | 各自独立，不共享 |
| 适用 | 中型项目，phase 内多角色 | 大型项目，子模块独立度高、接口稳定 |
| 开销 | 中（共享进程） | 大（N 进程 × 1M context） |
| 取舍 | 优化 token 成本 | 优化墙钟时间 |

工作区结构、tmux 命名、状态管理、资源约束 → **[interfaces.md §1.3](./interfaces.md#13-multi-session-项目子模块布局parallelism-multi_session)、[§2.2-2.3](./interfaces.md#22-master-statejsonparallelism-multi_session)、[§8](./interfaces.md#8-multi-session-per-project-协议m3)**。本节保留 fan-out / fan-in 论证:

#### Fan-out / Fan-in 协议(架构论证)

主流程是分形的——master 项目级 phase 流(`plan-eng` → `fan-out` → `implement-parallel` → `fan-in` → `review` → `ship`)在 master session 跑;每子模块在自己的 session 跑完整 9-phase 流,与单 session 项目协议**完全一致**。

关键论证:
1. **plan-eng 在 master 决定子模块切分**——子模块清单不是用户先验给的,是 plan-eng 输出的(`interface-contracts.md` + 模块清单)。master 不假设子模块独立度。
2. **Fan-out 一次性、Fan-in 阻塞**——master orchestrator 起 N 个 sub-session 后退到 idle,通过 inotify 监听所有 sub-module `progress.jsonl`;**所有** sub-module 都到 review phase 才触发 fan-in。任一子模块 escalate → master 暂停 fan-in。
3. **Review 在 master 跑,验证 contracts**——master 不是 N 个子模块的简单合并,而是有责任 audit 跨模块的接口契约(M4.8 靠 review phase 跑测试 + 人审 contracts.md;M5 才有形式化验证)。

#### 状态管理(关键纪律)

- **master `state.json`** 维护项目级 phase + 子模块状态摘要(详见 interfaces §2.2)
- **sub-module `state.json`** 维护子模块 phase 进度(与单 session 协议一致,详见 §2.3)
- 总 token 预算 = master + sum(sub-modules);硬上限触发 fan-in escalate

#### 三档叠加体现

multi_session 项目内每个 sub-session 仍可独立选 `parallelism: agent_team`（嵌套）或叠加 subagent。例如：
- master `plan-eng` 用 `agent_team` 启 architect / scope-watcher 议事
- backend-api session 的 `implement` phase 用 `agent_team` 启 api-impl / db-impl 并行
- 每个 agent 内仍可 ad-hoc 启 subagent 做局部研究

#### 边界（M4.8 不解决的）

- **自动子模块切分** = M5（本节假设 plan-eng 已能识别"有 N 个独立子模块"）
- **子模块接口契约的形式化验证** = M5（M4.8 仅靠 review phase 跑测试 + 人审 contracts.md 满足度）
- **跨子模块的 stop-the-world 重构** = M5（impl 中发现 contract 错时只能 escalate）

### 6.12 Team factory(V0.2 M0.22 — 用户自定义 team 落地为 plugin)

源自 PRD §4 + alignment-review §2。**复用 Claude Code plugin 格式,不发明 ccteam 私有打包**。

#### 三阶段流水线

```text
            interview                 init                  publish
meta-agent ───────────►  CLI/factory ───────►  staging ──────────────►  marketplace / GitHub
  (skill)                              ~/.config/ccteam/teams/<name>/
```

1. **Interview** — 元 agent 跑 `ccteam-team-author` skill,跟用户对话收集 metadata(name / description / author)+ phase 列表 + tools + golden_rules + retro_schema + verdict_schema。一次一题,默认值能用就用。
2. **Init** — `ccteam team init <name>` 落 staging 树到 `~/.config/ccteam/teams/<name>/`,内含:
   - `.claude-plugin/plugin.json`(Claude Code plugin manifest 严格 schema:`name` / `description` / `author`)
   - `team.yaml`(ccteam team 配置,作为 plugin 顶级 unknown 字段;zod 默认 strip,plugin pipeline 加载时忽略)
   - `phases/<NN>-<phase>.md`(frontmatter + 正文领域模板;**正文不写 `PHASE_DONE:` / `ESCALATE:`**——M0.18 D 方案,协议关键字仅由 orchestrator inject prompt 注入)
   - `README.md`
3. **Publish** — `ccteam team publish <name> --target {local|github}`:
   - `local`:软链 staging 到 `~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>/`,产出 directory-source 标识 `<name>@ccteam-local`(用户 `claude /plugin enable` 启用)
   - `github`:`gh repo create` + push,产出 GitHub URL(用户 `claude /plugin add <owner>/<repo>` 拉取)

#### 关键设计决策

- **不在 ccteam 自营 marketplace 注册中心**(alignment-review §2.3):用 Claude Code 已有的 `directory` source 作 `ccteam-local`,远程走 `gh repo` + `github` source。
- **`team.yaml` 不走 plugin `settings` 注入**(alignment-review §2.7):plugin loader 只 allowlist `agent` key,其他 strip。改作 plugin 根目录顶级 unknown 文件,ccteam 自己读(`team_resolver`)。
- **plugin manifest schema 借鉴 `claude-plugins-official`**:观察 `~/.claude/plugins/marketplaces/claude-plugins-official/plugins/*/.claude-plugin/plugin.json` 实例,所有都只用 `name` / `description` / `author { name, email? }` + 偶有 `version`。`PluginManifest` struct 严格 serialize 这四个字段,反序列化 lenient(unknown 字段忽略,符合 zod default strip)。
- **`enabledPlugins` 复用 M0.20 已 ship pipeline**:工厂产物的 `team.yaml` 声明依赖,`bootstrap_project` 写 `<project>/.claude/settings.json` 时由 `plugin_resolution` 推断。工厂自身不直接管理 plugin pipeline。
- **doctor `--validate-team` 二档验证**(M0.22.4):
  1. 已 ship 的 phase IO 契约 + frontmatter 校验(M0.18.5)
  2. 新增 plugin manifest schema + name 一致性校验(M0.22.4),仅当 staging 树存在时触发

#### 红线

- ccteam-core 不出现 team 名字面量(M0.16 基线)— 工厂代码也不许;团队特定行为靠用户写的 `team.yaml`。
- 工厂产物 phase markdown 正文不许写 `PHASE_DONE: <name>` / `ESCALATE: <prefix>` 关键字(M0.18 D 方案)。
- 不 vendor `claude-plugins-official` — 工厂模板可写 `tools_required.subagents: [code-reviewer]` 引用 plugin agent,**不**把 plugin source 拷到工厂产物。
- `--target github` 用户未 `gh auth login` → fail-loud,不试图绕过;不嵌入凭证。

#### 实现位置

- `crates/ccteam-core/src/team_factory.rs` — `init_team_staging` / `publish_team` / `validate_staged_team` 主路径;`PluginManifest` / `PhaseScaffold` / `PublishTarget` 数据类型。
- `crates/ccteam-core/src/templates/ccteam_team_author_skill.md` — meta-agent 用的 dialogue skill。
- `crates/ccteam-cli/src/team_factory_cli.rs` — `ccteam team init` / `ccteam team publish` CLI 包装。
- `crates/ccteam-cli/src/commands.rs` — `--validate-team` 加 plugin manifest 段。
- `docs/v0-2/team-factory-userguide.md` — 用户实操指引。

V0.3 候选(本里程碑不做):
- `userConfig` 工厂 emit(用户 enable plugin 时填表)
- `dependencies`(team-plugin 间依赖,eg 引用 `code-reviewer` plugin)
- 多 phase 一次性 init(当前 V0.2 单 phase 起步,多 phase 走 skill 多轮 init)

### 6.13 Web layer(V0.3 M5.0 起;占位,M5.4 补全)

V0.3 主线版本新加第四接入层(继 terminal / MCP / filesystem 之后),由 `crates/ccteam-web` crate 提供:

- **入口**:`ccteam web --bind <addr> [--no-auth] [--token-file <path>]`(`docs/interfaces.md` §10.6)。CLI subcommand 调 `ccteam_web::serve(ServeOpts)`,axum 0.8 server 绑端口、装路由、Ctrl-C / SIGTERM 优雅退出。
- **依赖图**:`ccteam-web` 只 depend on `ccteam-core`(F45 promote 后 4 个 write helper 落 `actions::*` 模块)。**严格不 dep `ccteam-cli`** — `crates/ccteam-web/tests/dep_graph_test.rs` 自检 `cargo tree -p ccteam-web` 不命中 `ccteam-cli`。
- **M5.0 范围**(本里程碑):scaffold + `GET /health` 200 JSON + `ServeOpts { bind, no_auth, token_file }` 类型形稳。
- **后续里程碑**:M5.1 read-only dashboard + 项目详情页(askama 模板 + htmx)/ M5.2 SSE + 按需 PNG 截图 / M5.3 写动作 endpoint + 默认 token 鉴权(loopback bypass)/ M5.4 e2e + ship gate。详 `docs/v0-3/prd.md` §3-§7。
- **架构红线**(V0.3 主线维持):progress.jsonl 仍是 SoT,web 不解析 tmux 终端;web 不 kill 长 session;web 不写跨项目记忆;`/btw` 走跟 telegram channel + MCP `send_to_session` 完全相同的 inbox + idle dispatch 路径,不开新通路。

---

## 7. 里程碑路线图

历史 milestone(V0.1 + V0.2)。每个版本的具体任务详情在该版本的 dev-plan
文档,本节仅一句话索引：

| 里程碑 | 主目标 | 状态 | 详情 |
|---|---|---|---|
| **M0** | 单项目 CLI MVP | 已 ship | [docs/v0-1/development-plan.md](./v0-1/development-plan.md) |
| **M0.5** | 工具表面 | 已 ship | 同上 |
| **M1** | meta-agent + decisions queue + inbox/outbox | 已 ship | 同上 |
| **M2** | sub-skill auto-trigger + ccteam-mcp 9 tools | 已 ship(M2.2 agent_team 永久 deferred,见 [m2-agent-team-spike](./v0-1/m2-agent-team-spike.md))| 同上 |
| **M2.3** | golden_rules executor(L1 强化) | 已 ship | 同上 |
| **M3** | team abstraction + product-research team | 已 ship | 同上 |
| **M4.1-M4.4** | 跨项目记忆(官方 rules + auto-memory + 可选 claude-mem) | 已 ship | 同上 |
| **M0.16-M0.23** | V0.2 全部 8 milestone | 已 ship | [docs/v0-2/dev-plan.md](./v0-2/dev-plan.md) |
| **M4.5-M4.6** | 多 audit 投票 + anti-leniency | 未 ship | (未规划到具体版本)|
| **M4.7-M4.9** | plugin auto-mount / multi_session / TUI | 未 ship | (未规划到具体版本)|
| **M5** | Critic Agent 深化 + 大型软件长跑(对标 Symphony) | 未 ship | V0.3+ 候选,见 [docs/v0-2/README.md V0.3 deferred](./v0-2/README.md) |

**版本化文档维护**:每发布一个版本,该版本所有规划文档(PRD / dev-plan /
design / retro / userguide)归档到 `docs/v<major>-<minor>/`,通过该目录的
README.md 索引;**根目录只保留跨版本 SoT**(本文件 / interfaces / requirements /
dev-coupling-audit / claude-code-* / 战略文档)。当前版本的 in-flight 任务
单列在该版本 dev-plan,不再维护"全局 development-plan"。

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
| **fix-loop 在边缘 case 错收敛** | 看似通过其实有 bug | M2.3 已 ship golden_rules（强制 L1 红线）；M4.5/M4.6 未 ship 引入投票 + anti-leniency |
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
| **Claude Code 内建 `/loop`** | ScheduleWakeup 动态模式（同会话）或 CronCreate 模式（Anthropic 云端调度远程 agent） | **不用**——动态模式依赖会话存活，违反痛点 9；CronCreate 模式虽能脱离会话但引入云端调度依赖，与 ccteam「本地优先 + `--dangerously-skip-permissions` 项目沙盒」模型不兼容（沙盒里跑的代码不该被云端 agent 远程注入）。ccteam 的循环驱动器永远是本地 Rust orchestrator |
| **Conductor / Worktrees IDE** | 多 session IDE | ccteam 用 git worktree 取代，无需 IDE |

---

## 10. 附录

### 10.1 命令签名 / 文件路径

完整 CLI 命令签名 → **[interfaces.md §10](./interfaces.md#10-cli-命令签名)**;关键文件路径速查 → **[interfaces.md §11](./interfaces.md#11-关键文件路径速查)**。本节不再重复维护。

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
| Critic / 评分 | 无 | 6 维评分 | PR review | **M2.3 已 ship golden_rules / M4.5+ Critic agent 投票（未 ship）** |
| 跨项目学习 | gbrain（可选） | 无 | 无 | **核心差异化（已 ship,M4：官方 rules + auto-memory）** |
| 执行 agent | Claude Code | Claude Code | Codex | **Claude Code** |
| 长跑能力 | 单 session 限制 | 单 sprint | 周级别 continuation | **M5 对标 Symphony（未 ship）** |
| 部署形态 | skill 安装 | Docker + Fly.io | Elixir 服务 | **本地守护进程（Rust）** |

---

## 11. 本文档的位置

- `requirements.md` 回答 **为什么做** 与 **谁会用**——已确认。
- 本文档 `tech-design.md` 回答 **怎么做**——架构论证、设计权衡、扩展点选择。
- [`docs/v0-1/docs/v0-1/development-plan.md`](./v0-1/development-plan.md) 回答 **何时做什么**——里程碑细化到任务级,含痛点反向映射、依赖图、验收门、风险登记。
- [`interfaces.md`](./interfaces.md) 回答 **接口确切长什么样**——YAML schema、JSON shape、文件路径、事件类型、命令签名。

所有实现 PR 必须能映射回:
1. `requirements.md` 的某条痛点
2. 本文档某个组件 / phase / 流程
3. `docs/v0-1/docs/v0-1/development-plan.md` 某条任务编号
4. (改协议时) `interfaces.md` 必须同步

无法映射的,先放进 backlog 而非合入主线。
