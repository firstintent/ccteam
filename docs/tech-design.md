# ccteam 技术实现方案

> 本文档基于 [requirements.md](./requirements.md)（已确认的用户痛点），参考 [gstack-auto](https://github.com/loperanger7/gstack-auto)（短期对标）与 [OpenAI Symphony](https://github.com/openai/symphony)（长期对标），以 Claude Code 为执行 agent，给出 ccteam 的技术架构、组件分解、数据协议、扩展点映射与里程碑路线。
>
> **核心问题**：用户用一句自然语言提需求，系统自动产出可运行软件——且**不需要主对话窗口在线**、**多项目自动排队**、**测试不过不交付**、**经验跨项目沉淀**。

---

## 1. 设计原则

每条原则都直接挂钩 `requirements.md` 中的某条痛点。

> **V0.4.0 起架构换血**:**phase DAG / `parallelism: solo/agent_team/multi_session` / 9 阶段流水线全 EOL**。新模型 = `workflow.yaml`(agent 拓扑 + `Trigger::{Manual,Schedule,Gate,Watch(PathBuf)}` + 每 agent `parallelism: u32`)+ `.claude/agents/<role>.md`(agent 行为 SoT)+ orchestrator 用 inotify ArtifactWatcher 把文件系统事件 fan-out 到 agent spawn。下面原则按 V0.4.6 现状重写;phase 时代等价机制保留在 §3.3 EOL 节作为历史。

| 原则 | 对应痛点 | 落地约束（V0.4.6） |
|---|---|---|
| **守护进程化（daemonize）** | 痛点 9：AI 团队需要人来主持 | Orchestrator 独立于任何 Claude Code 主对话，作为 systemd / cron 长跑进程；V0.4.6 F86 加 graceful shutdown cancel token，SIGTERM / `ccteam stop` 写 `/tmp/ccteam-<user>.shutdown` → daemon 收到 trigger → 所有 event_loop 优雅退 + 写 `workflow_done reason="shutdown"` |
| **文件即状态机** | 痛点 7：进度永远不透明 | 一切状态可从文件系统恢复；进程重启不丢任务；**`progress.jsonl` 7 类业务事件（workflow_start / agent_spawn / agent_done / artifact_received / gate_triggered / budget_exceeded / workflow_done + escalation）是唯一 SoT**（§5.5） |
| **声明式拓扑而非命令式 phase**（V0.4.0+） | 痛点 12 + 痛点 13 | `workflow.yaml::agents` 声明角色 + Trigger + parallelism + 可选 budget；orchestrator 用 ArtifactWatcher（inotify/fsevents）把文件系统事件 → agent spawn，不再用编排器维护 phase 状态机；agent 行为完全在 `.claude/agents/<role>.md` 内，与编排解耦 |
| **bg-job 替代长 tmux session**（V0.4.0+） | 痛点 8/9 + Claude 后台模式成熟 | Claude agent 走 `claude --bg --agent <role>` 写 `~/.claude/jobs/<job_id>/state.json`（后台模式），orchestrator 读 state.json 拿 liveness + cost；**只有 Codex CLI adapter** 仍走 tmux + statusline 路径（独立 adapter，F62 推迟标准化）；旧版 tmux + statusline + `--dangerously-skip-permissions` 长 session 路径 EOL，但 §6.1 保留作历史 |
| **多 trigger / 受控并发** | 痛点 13 | `Trigger::Watch(path)` + 每 agent `parallelism: u32` 上限（只对 Watch 触发有意义，其他 trigger 强制 ≤ 1）；Manual / Schedule / Gate 三类 trigger 各自语义清晰（§3.3） |
| **3-Strike 自愈再升级** | 痛点 4：bug 修复无限循环 | F66 thin orchestrator 维护 per-role `fix_counts`，撞 3 次顶 → `escalation` 事件 + meta-agent inbox notify；**phase 概念已废**，fix-loop 改 "watch:fix-requests/ 上 fixer 写第 4 个 file → escalation" 形态（§3.5） |
| **跨项目沉淀** | 痛点 10：每个新项目从零开始 | 复用官方 `~/.claude/CLAUDE.md` + `~/.claude/rules/ccteam-lessons-<team>.md` + per-repo auto-memory；每次 `claude --bg` 起新 job = 全新 1M context，加载机制自动注入；**ccteam-core 零 memory 检索代码**（§3.7） |
| **零交互沙盒** | 痛点 8：每一步都点允许 | 项目级 Docker / 容器隔离 + Claude bg-job 默认 `--dangerously-skip-permissions`；每个项目根 / `.ccteam/`（V0.4.6 F83）隔离 |
| **决策点 ≤ 3** | 痛点 2：AI 仍要求我当 PM | 只有不可逆决策（架构、scope 大改、API 形态）才走 escalation 事件 + meta-agent inbox |
| **预算硬上限**（V0.4.6 F84） | 痛点 5 + 自激励 loop 防失控 | `workflow.yaml::budget.{max_cost_usd_per_24h, max_agent_spawns_per_hour}` 任一超限 → 写 `budget_exceeded` 事件 + 自动 `enabled: false` 优雅终止 event_loop；cost 数据源 V0.4.6 F91 已收敛（详 §6.X cost telemetry） |
| **pipeline 编排 sub-skill** | 痛点 12：工作流插件靠人手动调 | agent 在自己的 `.claude/agents/<role>.md` 里声明 `Task(subagent_type=...)` 与 skill 调用；orchestrator 不再 phase-level 调度 sub-skill，而是 agent 内部决定何时调；复用 claude-plugins-official 的 plugin，不重写；详见 §6.10 |
| **纵深防御替代人值守** | 痛点 11：关键节点不把控 | L1 架构约束（hooks + 危险命令拦截）+ L2 多 agent 互检（workflow 内 explorer / fixer / reviewer 多视角）+ L3 用户兜底（仅 escalation / budget_exceeded 弹）；详见 §3.6 |
| **smart layer 只 translate，不 decide** | watchdog / 后续 ux-helper 不能改 orchestrator 状态 | translation 层只读取既有遥测，产出 NL 通知；**绝不**调 orchestrator API、写 progress.jsonl、kill session；**所有状态变更只能由 orchestrator + hooks 走；** 详见 §3.9 |

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
│  User Interaction Layer  (已 ship,M1 + V0.4.0 重塑)     │
│                                                          │
│  ┌──────────────────────┐  ┌─────────────────────────┐  │
│  │ meta-agent session   │  │ project agent jobs(N 个)│  │
│  │ ccteam-meta-<user>   │  │ ~/.claude/jobs/<job_id>/│  │
│  │ - 常驻、永不 terminal│  │ - claude --bg --agent X │  │
│  │ - NL 派单 / 跨项目   │  │ - state.json::cost_usd_ │  │
│  │   查询 / 监控         │  │   total / state ∈ {     │  │
│  │ - 跨项目 lessons     │  │   working/completed/    │  │
│  │   via ~/.claude/rules │  │   errored/...}          │  │
│  │   (M4 已 ship)        │  │ - workflow.yaml 描述拓扑│  │
│  │ - tmux attach 即对话 │  │ - artifact 触发 spawn   │  │
│  │   (meta 仍 tmux)     │  │ - codex adapter 仍 tmux │  │
│  └──────────────────────┘  └─────────────────────────┘  │
│                                                          │
│  接入面契约:                                             │
│  - <project>/.ccteam/<artifact_dir>/  (Watch trigger SoT)│
│  - <project>/.ccteam/workflow.yaml  (拓扑 SoT,V0.4.6 F83)│
│  - <project>/.claude/agents/<role>.md  (agent 行为 SoT) │
│  - ~/projects/<user>-meta/.ccteam/inbox/  &  outbox/     │
│  (artifact 是文件,inbox/outbox 是 NL markdown)         │
└──────────────────────────┬───────────────────────────────┘
                           │ inotify ArtifactWatcher / inbox watcher
                           ▼
┌──────────────────────────────────────────────────────────┐
│  Orchestration Layer  (V0.4.0 F66 thin orchestrator)    │
│  - Rust orchestrator daemon(~/.ccteam/ 状态平面)        │
│  - progress.jsonl 7 类业务事件唯一 SoT(§5.5)            │
│    workflow_start / agent_spawn / agent_done /          │
│    artifact_received / gate_triggered / budget_exceeded │
│    / workflow_done + escalation                         │
│  - ArtifactWatcher(notify crate inotify/fsevents,F64)│
│  - 每 workflow 一个 event_loop(JoinSet,F82 cancel       │
│    token,F86 graceful shutdown)                         │
│  - 17 个 mcp__ccteam__* tools(F65)                      │
│  - hooks(§6.2)/ cost telemetry(F91,§6.X)            │
│  - team abstraction(M3 已 ship,§3.2.2)                │
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

上面是逻辑分层。**进程视图**(V0.4.0 后):

- **meta-agent session** 仍然是独立 tmux + claude 长进程(meta-agent 走 dispatcher role,事件循环不需要 bg-job 形态);
- **项目 agent job** 走 `claude --bg --agent <role>`,每个 spawn 是一个独立 bg-job 进程,生命周期 = 一次 trigger → 一次 agent_done 即终止;**没有"项目长 session"概念了** — 项目持久化在 `<project>/.ccteam/state.json` + `<project>/.ccteam/workflow.yaml`,agent 进程都是短命的;
- **Codex 例外**:Codex CLI adapter 仍走 tmux + statusline 长 session 路径(V0.4.0 trait stub,F62 推迟标准化),保留 §6.1 的 tmux 操作模板作历史;
- **Rust orchestrator daemon** 是另一个独立进程(F66 thin 形态,只读 progress.jsonl + 文件系统 trigger,不维护 phase 状态机);
- **channel adapter** 进入 M2+ 后又是若干独立进程。

所有进程之间用**文件系统协议**通信,**不用共享内存 / sockets / IPC**(§5 与 §3.1)。
进程崩溃只丢自己的进程内存,文件状态留给重启后恢复。
V0.4.6 F86 加 graceful shutdown 后,daemon 收到 SIGTERM / `/tmp/ccteam-<user>.shutdown` →
所有 event_loop 优雅退、`workflow_done reason="shutdown"` 写进 progress.jsonl、in-flight
bg-job 留给下次启动 F80 phantom cleanup 补 synthetic agent_done(详 §6.X graceful shutdown)。

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

**V0.4.0 起改走 `claude --bg --agent` bg-job 形态(为什么不再用 tmux 长 session)：**

- **后台模式成熟**:V0.4.0 时点 Claude Code 已成熟支持 `--bg --agent <role>` 写 `~/.claude/jobs/<job_id>/state.json`,可外部观测 liveness + 累计 cost,无需再靠 tmux UI / stream-json 解析。
- **prompt cache 仍复用**:同一 claude binary process 内 cache 仍按 1M context 5min TTL 累加;`workflow.yaml` agent 触发短跑(每 spawn 通常 < 5min),命中冷启动概率低。
- **agent 行为 SoT 分离**:`workflow.yaml` 只声明 trigger + 连线 + 并发上限,**不含 prompt**;agent 系统提示 / 工具表面在 `.claude/agents/<role>.md`(Claude Code first-class spec),编排器与 prompt 完全解耦。
- **每次 spawn = 全新 1M context**:不需要 §6.9 旧版的"60% 阈值 + phase 边界 reset"复杂路径——bg-job 本身就是短命,context 不会撑爆。
- **trade-off**:cache miss 频率比 V0.3 单长 session 高,但 spawn 也更短(workflow agent 平均 30s-5min);单次 spawn cost 易算清,F84 budget cap 直接套(workflow.yaml `max_cost_usd_per_24h`)。
- **Codex CLI 适配器例外**:Codex 仍走 tmux + statusline(stable 后台模式 V0.4.6 时还没就绪),作为独立 adapter 路径保留,详 §6.1 历史小节。

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

**核心循环**(V0.4.0 F66 thin orchestrator 形态,伪代码):
```rust
async fn run() -> Result<()> {
    let mut state = State::load_from_fs();          // 启动时从文件恢复
    let mut tasks: JoinSet<()> = JoinSet::new();    // 每 workflow 一个 event_loop
    let shutdown_token = Arc::new(Notify::new());   // V0.4.6 F86 graceful shutdown
    let cancel_map = Arc::new(DashMap::new());      // F82 per-workflow cancel
    install_signal_handlers(shutdown_token.clone());
    spawn_artifact_watchers(&state).await;          // F64 inotify per Watch trigger
    spawn_workflow_yaml_watchers(&state).await;     // F82 hot reload on workflow.yaml mtime
    spawn_new_rostered_projects(&state, &mut tasks, cancel_map.clone()).await;
    loop {
        select! {
            _ = shutdown_token.notified() => {
                // F86: graceful shutdown — cancel all event_loop,等 JoinSet 自然退,
                // 30s timeout 后才走 abort_all() fallback
                graceful_shutdown(&mut tasks, cancel_map, Duration::from_secs(30)).await;
                return Ok(());
            }
            Some(result) = tasks.join_next() => {
                // event_loop 自己退(workflow_done reason="disabled"/"shutdown"/"budget_exceeded")
                // 或 panic → 记录,等 F82 watcher 重新 roster
            }
            _ = tokio::time::sleep(tick) => {
                poll_global_inbox(&mut state).await;
                spawn_new_rostered_projects(&state, &mut tasks, cancel_map.clone()).await;
                enforce_budget(&state).await;          // F84 24h cost / 1h spawn rate
                cleanup_stale_spawns(&state).await;    // F80 phantom agent_spawn 清理
            }
        }
    }
}

// per-workflow event_loop(run_project 实质)
async fn run_project(slug: &str, cancel: CancelToken) {
    let workflow = WorkflowSpec::load(&project_dir)?;
    if !workflow.enabled { return; }                 // F82
    append_event(slug, "workflow_start", json!({...}));
    let watch_rx = build_artifact_watchers(&workflow).await;
    loop {
        select! {
            _ = cancel.cancelled() => {
                append_event(slug, "workflow_done", json!({"reason": "shutdown"}));
                return;
            }
            Some(artifact) = watch_rx.recv() => {
                if running_for_role(slug, &artifact.role) >= parallelism(&artifact.role) { continue; }
                let job_id = spawn_claude_bg(&workflow, &artifact).await?;
                append_event(slug, "agent_spawn", json!({"role": ..., "job_id": ..., ...}));
            }
            // gate / manual / schedule triggers 走 MCP tool 或定时 tick
        }
    }
}
```

**状态模型**(V0.4.0 起):

- **`progress.jsonl` 是 SoT**:7 类业务事件(`workflow_start` / `agent_spawn` / `agent_done` / `artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done` + `escalation`)实时累积;orchestrator 只读这一份做"哪些 agent 在跑、跑了多少、cost 多少"判定。
- **`state.json` 退化为 serde-compat shell**:`current_phase` / `phase_history` / `decision_candidates` 等 V0.3 字段 `#[serde(skip_serializing_if = "Option::is_none")]` 保留**只读**,新写不带,旧 state.json 仍可读;F66 thin orchestrator 完全不消费它们。
- **`session_handle` / `tmux_session` / `claude_pid` 路径退役**:F66 后 spawn 拿到的是 `~/.claude/jobs/<job_id>/state.json` 路径而非 PID;每个 `agent_spawn` 事件 payload 直接带 `job_id`,liveness 走 `claude_job::probe_job` 读 state.json(F80)。

**单点 + claim 防重**(V0.4.6):claim 粒度是 **agent role 级**(同 role 并发上限 = `AgentSpec::parallelism`,只对 `Trigger::Watch` 有意义,其他 trigger 强制单实例)。orchestrator `running_for_role()` 扫 progress.jsonl 末尾事件,有 `agent_spawn` 但还没匹配 `agent_done` 就算 running。

**orchestrator 重启时**:F80 phantom cleanup 扫每个项目 progress.jsonl,凡 `agent_spawn` 无匹配 `agent_done` 且对应 `~/.claude/jobs/<job_id>/state.json` 已不存在或处 terminal state → 补 synthetic `agent_done status="cleanup"` + cost 0;ArtifactWatcher 重新装,event_loop 重新跑,完全无状态恢复负担。

#### 3.2.1 Evergreen 团队（meta-agent / 常驻 role）

> **V0.4.0 起重塑**:phase DAG 全废后,evergreen 概念退化为"普通项目 + workflow.yaml 里用 `trigger: manual` 或 `trigger: watch` 跑事件循环"。`team.yaml::evergreen: true` 标志位仍 serde-compat 保留,但 orchestrator 不再据此分叉 — `process_meta_project` / `enforce_cost_thresholds` 等老路径在 F66 thin orchestrator 后不复存在。

V0.4.6 实际形态:meta-agent 是一个 workflow.yaml 配 `trigger: manual` 的 dispatcher agent + `<project>/.ccteam/inbox/` 作 Watch trigger 的接力跑者(用户写 inbox → ArtifactWatcher 触发 dispatcher agent 一次)。watchdog / reviewer 同理 — 在自家 workflow.yaml 用 `trigger: manual` + meta-agent 在自己 outbox 写 NL trigger,或用 `trigger: schedule`(V0.4.6 仍是 stub,V0.4.7+ 才有真 cron)。

`teams/meta-agent.yaml` 是首个 evergreen 范例,V0.2 起作为 shipped seed 随 binary 发布;`Orchestrator::new` / `ccteam start` / `ccteam doctor --reset-shipped-teams` 都会把它写到 `~/.ccteam/teams/meta-agent/team.yaml`(只是种子,实际运行时编排器只看 workflow.yaml + state.json)。

**红线**:`ccteam-core` 不出现 team 名字面量(M0.16 基线 V0.4.6 继续维持);团队特定行为靠用户写的 `workflow.yaml` + `.claude/agents/<role>.md`。

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

### 3.3 Workflow 拓扑（V0.4.0 起）

> **V0.4.0 起架构换血**:**Phase Pipeline EOL**。9 阶段(`01-seed` / `02-plan-eng` / `03-implement` / ... / `09-ship`)、phase YAML front matter(`required_inputs` / `required_outputs` / `parallelism: solo|agent_team|multi_session` / `verdict` / `auto_loop`)、`team.yaml::kind: workflow|multi_workflow|flex` 全废。新模型见下,旧 phase 字段仅作 §3.3 EOL 历史小节保留。

V0.4.0 起每个项目用 `<project>/.ccteam/workflow.yaml`(V0.4.6 F83 起 canonical 位置,旧 `<project>/workflow.yaml` fallback)声明 agent 拓扑 + trigger + 并发上限,**不含任何 prompt**;每个 agent 的系统提示 + 工具表面在 `<project>/.claude/agents/<role>.md`(Claude Code first-class spec)。完整 schema → **[interfaces.md §17](./interfaces.md#17-workflowyaml-schema)**。

#### 3.3.1 `workflow.yaml` schema 速览

```yaml
name: dex-ui-autoloop                    # 必,workflow 标识
description: explorer/fixer/master 自激励循环  # 可选,UI 用
enabled: true                            # V0.4.6 F82,default true,false 时 daemon 跳过 roster
budget:                                  # V0.4.6 F84,可选 budget cap
  max_cost_usd_per_24h: 5.00
  max_agent_spawns_per_hour: 100
agents:                                  # role → AgentSpec,IndexMap 保留 YAML 顺序
  explorer:
    executor: claude                     # claude | codex(default claude)
    trigger: manual                      # 或 schedule / gate / watch:<path>
    parallelism: 1                       # 只对 watch trigger 有意义,其他强制 ≤1
    output: .ccteam/fix-requests/
    timeout: 30m
    on_timeout: escalate                 # escalate | retry | skip
  fixer:
    trigger: watch:.ccteam/fix-requests/
    parallelism: 3                       # 并发 3 个 fixer
    input: .ccteam/fix-requests/
    output: .ccteam/done/
  master:
    trigger: gate                        # 等 trigger_gate MCP 工具释放
    input: .ccteam/done/
```

#### 3.3.2 `Trigger` 四类语义

| Trigger | 语义 | 触发源 | 并发约束 |
|---|---|---|---|
| `manual` | 用户 / meta-agent 显式 `ccteam internal spawn <slug> <role>` 或 `mcp__ccteam__spawn_agent` | CLI / MCP / 用户 inbox 消息 | parallelism 强制 1 |
| `schedule` | 定时 trigger;V0.4.0 - V0.4.6 仍 stub(meta-agent 手动触发),V0.4.7+ 接真 cron 解析 `AgentSpec::interval` | (未 ship) | parallelism 强制 1 |
| `gate` | 等 `mcp__ccteam__trigger_gate` MCP 工具调用释放;释放后消费 input 目录所有 artifact 后再回 gated | MCP 工具 | parallelism 强制 1 |
| `watch:<path>` | inotify(Linux)/ fsevents(macOS)监听项目相对路径,新文件 → spawn 一个 session | ArtifactWatcher(F64,V0.4.5 F78 修复项目相对路径) | `parallelism: u32` 上限内并发 |

#### 3.3.3 与 `.claude/agents/<role>.md` 的解耦

**红线**(F63 PRD):`workflow.yaml` 不许出现 `prompt:` / `system_prompt:` / `messages:` 字段 — 任何 PR 加这些 = schema violation。

agent 行为完全靠 `.claude/agents/<role>.md`(Claude Code 官方 agent 文件格式):前置 YAML 声明 `name` / `description` / `tools` / `model`,正文是 system prompt。orchestrator 用 `claude --bg --agent <role> --workdir <project>` spawn 后,Claude Code 读这个文件加载 agent — ccteam 完全不解析 prompt 内容。

这种解耦让用户改 prompt 不需要重启 orchestrator(V0.4.6 F82 workflow.yaml 热加载,prompt 改动 claude 下次 spawn 自动加载),改拓扑不需要改 prompt(workflow.yaml 调 trigger 路径,agent prompt 不变)。

#### 3.3.4 V0.3.x phase pipeline(已 EOL)

V0.3.2 及更早走 phase DAG + `team.yaml::kind: workflow`(phase 顺序) / `multi_workflow`(多 phase 序列) / `flex`(无 phase DAG)。9 个 phase(`01-seed` → `09-ship`)、`PHASE_DONE: <name>` / `ESCALATE: <prefix>` 协议关键字、auto_loop ralph-loop self-loop、golden_rules executor、Seed verdict YAML 解析等都依附 phase 边界。**V0.4.0 起全部废除**,旧 team.yaml 用 `ccteam doctor --migrate-phase-to-workflow` 一次性迁出生成 workflow.yaml 骨架 + `.claude/agents/<role>.md` 模板(prompt 语义需手动调,phase 顺序 → artifact-trigger 事件驱动)。

### 3.4 Workspace 隔离与并行

**每项目一个目录**（V0.4.2 F72/F75 起,任意 cwd 经 `ccteam init` 即可成 ccteam 项目;新建走 `ccteam new <slug>` thin wrapper 写到 `~/projects/<team>-<slug>/`)。team 前缀（F22 已 ship）让 `~/.claude/rules/ccteam-lessons-<team>.md` 的 `paths:` frontmatter 能正确 scope 到该项目。

**项目目录结构(V0.4.6)**:
```
<project>/                            # 任意路径(V0.4.4 F77 walk-up 支持)
├── src/                              # 实际代码(business)
├── tests/
├── package.json / pyproject.toml
├── CLAUDE.md                         # 项目级运营手册（V0.4.6 不带 phase 字段,见 §6.5）
├── .claude/                          # Claude Code 原生约定
│   ├── agents/<role>.md              # V0.4.0 agent 行为 SoT
│   └── settings.json                 # hook + enabledPlugins
├── .ccteam/                          # ccteam orchestration state(gitignored)
│   ├── workflow.yaml                 # V0.4.6 F83 canonical 位置
│   ├── state.json                    # serde-compat shell(V0.4.0 后大部分字段 deprecated)
│   ├── spawn_requests/<role>-<ts>.json  # `ccteam internal spawn` 触发的 marker
│   ├── fix-requests/  done/  ...     # workflow.yaml `Trigger::Watch` 监听的 artifact dirs(用户定义)
│   ├── outbox/                       # agent 写 NL 通知(meta-agent 翻译)
│   └── inbox/                        # 用户写 / agent 间消息
└── .gitignore                        # 包含 .ccteam/ 整段
```

**并发模型(V0.4.6)**:
- **`AgentSpec::parallelism: u32`** 决定每 role 并发上限(只对 `Trigger::Watch` 有意义,其他强制 ≤1)
- **`MAX_CONCURRENT_PROJECTS`**(~/.ccteam/config.yaml,default 3)决定 roster 上限
- **`workflow.yaml::budget`**(V0.4.6 F84)是 per-project 软上限:`max_cost_usd_per_24h` + `max_agent_spawns_per_hour`,任一超限 → `budget_exceeded` 事件 + 自动 `enabled: false`
- **CLAUDE.md §三红线 $200 物理上限**仍守 — 全 ccteam 进程合计累计 cost 超 → daemon 整体 alert(粒度比 budget cap 粗)

orchestrator 每轮 tick 后按这几条做准入控制。**V0.4.0 起没有 max_turns 概念**(bg-job 自然短命,无需);budget 改 per-project YAML 声明而非全局 yaml。

**为什么不用 Conductor**:Conductor 是 Anthropic 的多 session 工作区工具,但要求人在 IDE 里使用。ccteam 用 git worktree + 文件系统 trigger 取代 Conductor 的工作区隔离能力 — 比 IDE 更适合无人值守。

### 3.5 Self-healing Fix Loop（V0.4.0 后形态）

> **V0.4.0 起重写**:phase + ralph-loop self-loop EOL。Fix 是 workflow 拓扑里的一个 agent role(典型:`fixer` watch `fix-requests/`),撞 N 次顶由 thin orchestrator 计数后 escalate。

**结构**(典型 workflow.yaml 形态):

```yaml
agents:
  explorer:
    trigger: manual                  # 用户/meta 触发分析
    output: .ccteam/fix-requests/    # 写 fix request artifact
  fixer:
    trigger: watch:.ccteam/fix-requests/
    parallelism: 3                   # 最多 3 个 fixer 并行
    input: .ccteam/fix-requests/
    output: .ccteam/done/
  master:                            # 可选 reviewer
    trigger: gate
    input: .ccteam/done/
```

**事件流**:
1. **explorer** 写 `.ccteam/fix-requests/req-001.md`(诊断 + plan)→ ArtifactWatcher inotify → orchestrator spawn `fixer` claude bg-job → progress.jsonl 加 `agent_spawn` 事件。
2. **fixer** 读 input artifact → 改代码 → 跑测试 → 测试绿:写 `.ccteam/done/req-001.md`,bg-job 结束(`state.json::state="completed"`)→ Stop hook 写 `agent_done` 事件 + cost。
3. **失败时**:fixer 自己决定写新 `fix-request` 重试(自激励 loop 必须配 F84 budget cap,否则 4h 烧 \$1.10 自激励 — 2026-05-16 dex-ui 实证)。
4. **3-strike 计数**:F66 thin orchestrator 维护 per-role `fix_counts`(从 progress.jsonl `agent_done.status="errored"` 累加),撞 3 次顶 → 写 `escalation` 事件 + 推 meta-agent inbox notify;**不**自动停 workflow(F84 budget 才会优雅停)。

**为什么这套替代 ralph-loop**:V0.4.0 后没有"长 session 内 Stop hook 拦截退出"概念了 — `claude --bg --agent` 每次 spawn 都是新进程,生命周期 = 一次 trigger → 一次 agent_done 即终止。"同一段 prompt 反复跑直到收敛"由 workflow 拓扑(fixer 反复触发自己,直到 explorer 不再写 fix-request)与 F84 budget cap 联合实现,**无需 ralph-loop hook**。

#### 3.5.1 escalation 事件

V0.4.0 起 `escalation` 是 progress.jsonl 7 类业务事件之一,payload:

```json
{
  "ts": "2026-05-16T12:34:56Z",
  "event": "escalation",
  "kind": "fix_count_exceeded" | "manual" | "budget_exceeded_fallback",
  "role": "fixer",
  "count": 3,
  "last_errors": ["test_foo failed", "test_bar failed", "test_baz failed"],
  "recommendation": "..."  // 可选,explorer/master 写
}
```

orchestrator 看到 → 推 meta-agent inbox 一条 enriched markdown(含最近 200 行 progress events + git diff 最近 3 commits + 最后 3 个 fix-request artifacts);meta-agent 看 NL 后回用户。

**禁止静默重试**:撞 3 次顶绝不静默 — ccteam 区别于 "AI 永远说没事" 的承诺,V0.4.0 后仍守。

#### 3.5.2 V0.3 phase + ralph-loop self-loop(EOL)

V0.3.2 及更早:phase pipeline + `auto_loop` default-on + Stop hook 三档兜底(Auto-loop reinject / PHASE_DONE-ESCALATE 解析 / self-loop fallback)+ silence classifier 7 类 + PreToolUse 拦截 AskUserQuestion 等机制全废,代码留 serde-compat shell 不调用。新建项目改写 workflow.yaml + `.claude/agents/<role>.md` 直接走 V0.4.0 模型。详 `docs/v0-4-0/migration-guide.md`。

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
- 每个 agent 终态(`agent_done`)由 agent 自己决定是否写记忆 — 典型在 reviewer / master role 的 `.claude/agents/<role>.md` prompt 中引导 Claude:
  - 项目特定 lessons → `/memory` 写本仓 auto-memory(Claude 自主决策何时写)
  - 跨项目 lessons / 反模式 → `Edit ~/.claude/rules/ccteam-lessons-<team>.md`(限 `<!-- ccteam-managed:lessons -->` marked section,不污染用户其他段)
- V0.3.x 版的 `team.yaml.retro_schema[]` 字段保留 serde-compat;V0.4.0 后 retro 由 agent prompt 自行约束(workflow.yaml 不再强制 retro phase)

**召回时机**(全部经 Claude session 内官方接口):
- **每次 `claude --bg` 起新 job = 全新 1M context**:无需 V0.3 时代"60% 阈值 reset" 复杂逻辑 — bg-job 本身就短命(workflow agent 平均 30s-5min 即 agent_done),context 不会撑爆;新 job 启动时 Claude Code 加载机制自动注入 `~/.claude/rules/ccteam-lessons-<team>.md`(匹配 `paths:` frontmatter 的项目)+ per-repo `CLAUDE.md`,零 RPC。
- 需深挖本项目历史 → Claude 用 `/memory` 浏览 + `Read` 读 topic 文件
- 跨项目相似失败 → reviewer agent 看 lessons 后建议 escalation

**V0.3 → V0.4.0 reset 触发点变更**:V0.3 时代 "phase 边界 + context > 60% → /exit + 新 session + CLAUDE.md 桥接" 路径 EOL,因为 V0.4.0 起没有"长 session" 了 — 每次 `agent_spawn` 都是新 bg-job 进程,context 自然清零。CLAUDE.md 桥接退化为 agent prompt 模板里的 "如有未完成任务读 .ccteam/state.json 续上" 指引。

**可选增强**(用户装了 [claude-mem](https://docs.claude-mem.ai/usage/search-tools)):
- claude-mem 自带 5 个 hook(SessionStart/UserPromptSubmit/PostToolUse/Stop/SessionEnd)自动捕获,
  ccteam 不调任何 hook;暴露 4 个 read-only MCP tool(`search` / `timeline` / `get_observations` / `__IMPORTANT`)支持跨项目 FTS5 检索 + type 过滤(bugfix/feature/decision/discovery/refactor/change)
- agent prompt 提示"如检测到 `mcp__*claude-mem*search` 工具,可用于跨项目深度检索",**LLM 自看 tool surface 决定调不调**;ccteam 不写检测代码,不写集成代码
- 用户没装则 100% 走默认路径,功能不受影响

**ccteam 实际改动量**(已 ship,M4.1–M4.4 + V0.4.0 沿用):
- M4.1 retro guidance(V0.4.0 后并入 `.claude/agents/<role>.md` agent prompt 模板)
- M4.2 `ccteam doctor --install-memory-bridge`(创建 rules 占位文件 + marked section + path frontmatter,**唯一一段 ccteam 代码**)
- M4.3 conversation continuity guidance(V0.4.0 后并入 agent prompt 模板)
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

| 维度 | meta-agent session(L0.5) | project agent bg-job(L2,V0.4.0+) |
|---|---|---|
| 生命周期 | 永不 terminal,跟用户 ccteam 实例同寿 | 一次 trigger → 一次 agent_done 即终止 |
| 行为模式 | 事件循环(等 inbox → 处理 → 等 inbox) | workflow.yaml trigger fires → spawn → 跑完即退 |
| 进程形态 | tmux long session + `claude --dangerously-skip-permissions` | `claude --bg --agent <role>` 写 `~/.claude/jobs/<job_id>/state.json` |
| 主要工具 | `ccteam-control` skill(M1.8 ✅)/ `ccteam-mcp`(M2 ✅,V0.4.0 起 **17** tools)/ `ccteam-creator` skill(V0.4.4 ✅)/ 跨项目 lessons(M4 ✅) | `.claude/agents/<role>.md` 声明工具 / 内嵌 plugin agents(V0.2 M0.20 走 `enabledPlugins`) |
| context | 60% 阈值时桥接 `~/.claude/CLAUDE.md`(meta-agent 仍长跑,沿用 M0.10 路径) | 每次 spawn = 全新 1M context(无需 reset) |
| 用户 attach | `tmux attach -t ccteam-meta-<user>`,直接 NL 对话 | bg-job 无 attach 概念,看 `ccteam show <slug>` / web SPA WorkflowView 实时观测 |

#### CLI(V0.4.6 F89 后)

V0.4.6 F89 把 CLI 切成 **9 user-facing** + `internal` 折叠组(meta-agent / MCP / hook installer 内部用)。`ccteam --help` 看到:

**用户日常(9 个,V0.4.6)**:
```bash
ccteam init                  # 一次性 setup ~/.ccteam/ + 当前目录变 ccteam 项目(V0.4.2 F72 三合一)
ccteam start [--no-web]      # 起 orchestrator daemon + 嵌入 web UI(V0.4.1 合并)
ccteam stop                  # F86 graceful shutdown:写 /tmp/ccteam-<user>.shutdown trigger
ccteam new <slug>            # init thin wrapper:在 ~/projects/<team>-<slug>/ 起新项目(V0.4.2 F75)
ccteam ls                    # 列所有 rostered 项目 + daemon health
ccteam show <slug>           # 项目详情:cost / running agents / recent events / budget util
ccteam remove <slug>         # V0.4.6 F81 un-roster:守 §三红线(活 session refuse)+ 可选 --purge
ccteam doctor [--gc-claude-jobs|--install-mcp|...]  # 健康检查 + 维护工具
ccteam web                   # 单独跑 web SPA(start 已含,这里给 headless server 用)
```

**Internal(`ccteam internal <subcmd>`,V0.4.6 F89 折叠)**:
```bash
ccteam internal hook <progress-append|parse-phase-end|load-context|intercept-ask>
ccteam internal mcp-serve                       # MCP stdio server,~/.claude.json wire
ccteam internal spawn <slug> <role> [prompt]    # 手动 spawn,写 .ccteam/spawn_requests/<role>-<ts>.json
ccteam internal send <slug> <body>              # 写项目 inbox,allow_hyphen_values(F87)
ccteam internal attach <slug>                   # tmux attach(meta + codex 还用)
ccteam internal peek <slug>                     # capture-pane 不 attach
ccteam internal progress <slug> [--tail]
ccteam internal resume <slug>
```

V0.4.5 的 `phase` / `decisions` / `watchdog scan` 三个 V0.3 legacy 子命令 F89 已删除;`ccteam hook progress-append` 等老顶层路径 V0.4.6 一版兼容(发 deprecation WARN),V0.5 删。

**关键约束**:所有查询命令支持 `--format json`(详见 [interfaces.md §10](./interfaces.md#10-cli-命令签名)),让用户自带 claude 通过 Bash 工具调时不用解析表格。

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
- **`ccteam-mcp` MCP server**(M2 ship,**V0.4.0 起 17 个 tool**):暴露
  - **V0.3 时代 10 个 `mcp__ccteam__*`**:`ls` / `show` / `new` / `peek` / `progress` / `pause` / `resume` / `inject_decision` / `send_to_session` / `screenshot`(详见 [interfaces.md §12](./interfaces.md#12-ccteam-mcp-mcp-server-m2))
  - **V0.4.0 F65 新加 7 个 workflow tool**:`spawn_agent` / `stop_agent` / `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` / `get_artifact_summary`(在 `crates/ccteam-cli/src/mcp_workflow_tools.rs`)
  - meta-agent 与用户 daily-driver claude 都受益(MCP 比 shell parse 更鲁棒)

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

#### Web 仪表盘(V0.3 起 ship,V0.4.0 重塑 + V0.4.6 F90 增强)

**V0.3 vintage 是 askama SSR + htmx + vanilla CSS,V0.4.0 F69 已彻底删除** — 现行 `crates/ccteam-web/` 是 Vite + React TypeScript SPA,`build.rs` 在 `cargo build` 时自动跑 `npm run build`(本机 dev 可 `CCTEAM_SKIP_WEB_BUILD=1` 或 `--no-default-features` 跳过)。Backend 仍 axum + SSE,但只服务 SPA bundle(`/app/*` + `/assets/spa/*`)+ JSON API + SSE。`templates/{dashboard,project,session}.html` 已删,`templates/base.html` 留作 askama SSR fallback 给 `/health`、`/health-old`、deep-link 301 用。

**Authentication 路径**(从 V0.3 沿用):loopback 免 token、非 loopback 自动生成 `~/.ccteam/web-token` mode 0600 + 5s LAN-RCE 倒计时;URL shim `?token=ccteam:<hex>` → HttpOnly cookie + 303 → 干净 URL。V0.4.6 F88 起 `ccteam start` 输出 token 时同时 probe `xclip` / `wl-copy` / `pbcopy` / `clip.exe` 把 token 拷到剪贴板(`--no-clipboard` 关)。

**SPA 路由 4 页**:
- `/app/` — projects list(WorkflowView 入口)
- `/app/project/<slug>` — workflow view(agent cards + 4 panels,V0.4.6 F90)
- `/app/project/<slug>/session/<sid>` — Codex tmux session 详情(legacy,保留)
- `/app/settings` — token 管理

**V0.4.0 F69 + V0.4.6 F90 WorkflowView 4 个新面板**(代码 `crates/ccteam-web/web/src/components/`):
- **ArtifactQueuePanel** — 每个 `Trigger::Watch(path)` agent 显示待处理 artifact 数 + 最旧文件 age + 最新文件名(后端 `GET /api/v1/projects/<slug>/artifact_queue` 实时 `fs::read_dir`)
- **EventsTimelinePanel** — progress.jsonl 最近 100 行 + 颜色编码(绿色 `agent_done`、橙色 `gate_triggered`/`budget_exceeded`、红色 `escalation`)+ SSE 实时插
- **FailureInspector** — errored agent card 点击 → `GET /api/v1/projects/<slug>/jobs/<job_id>/log?tail=200` 渲染 `~/.claude/jobs/<job_id>/output.log` 尾部(read-only)
- **CostSparkline** — 24h + 7d SVG sparkline,数据源 F91 收敛后的 `workflow_summary.cost_24h_usd` + 历史 `progress.jsonl::agent_done.cost_usd` aggregated by hour(`GET /api/v1/projects/<slug>/cost_history?window=24h|7d`)

**V0.4.5 F80 加 pulsing-dot 活动指示**(每个 agent card running session 有 active dot,SSE 推送)。

**Codex tmux SessionDetail / Terminal / Btw / Keyboard 组件保留** — Codex CLI adapter(F62 推迟)仍走 tmux 模式,这些组件复用,不算 dead code。

**架构红线**(V0.4 维持):progress.jsonl 仍是 SoT;web 不解析 tmux 终端(SSE watcher 仅读 progress.jsonl);web 不 kill 长 session;web 不写跨项目记忆;`/api/v1/projects/<slug>/btw` 走跟 telegram channel + MCP `send_to_session` 完全相同的 inbox + watcher dispatch 路径;`cargo tree -p ccteam-web | grep ccteam-cli` 必须 0 命中(独立 dep graph 红线由 `tests/dep_graph_test.rs` 锁)。

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
- V0.4.6 F89 起 `ccteam watchdog scan` 顶层 CLI **已删除**(V0.3 legacy);
  meta-agent 直接读 4 个数据源 NL 翻译,不再走专用子命令
- `crates/ccteam-core/src/orchestrator.rs` **零** watchdog 引用
  (grep `watchdog` 命中 0 次是核心红线)

### 3.10 项目生命周期(V0.4.6 F81-F83)

V0.4.5 之前没有"删项目"命令,workflow.yaml 改一字段需要 daemon stop/start,workflow.yaml 位置又在项目根上和业务代码混淆。V0.4.6 三个 finding 一次解决:

#### 3.10.1 `ccteam remove <slug>`(F81)

```bash
ccteam remove <slug> [--purge] [--dry-run] [--force]
```

- **always**:从 `~/.ccteam/config.yaml::projects[]` 删该 slug;
  通过 F82 wiring 告知 daemon 热剔除(JoinSet abort + cancel token 优雅退);
  删 `~/.ccteam/progress/<slug>.jsonl`、`~/.ccteam/inbox/<slug>/`、
  `~/.ccteam/control/<slug>/`(如有)
- **`--purge`**:同时 `rm -rf <project>/.ccteam/` + `<project>/.claude/agents/` + `<project>/workflow.yaml`(以及 `.ccteam/workflow.yaml`,F83 后 canonical 位置)。**业务代码 / .git/ / .env 永远保留**
- **守 §三红线 refusal**:有活 tmux session / 活 claude bg job / 未匹配 `agent_spawn` 时 refuse,`--force` 才绕过

衍生子命令 `ccteam abandon <slug>` PRD 中讨论后并入 `ccteam remove`(不加 `--purge` 等价 abandon — config 删但项目目录不动)。

#### 3.10.2 `workflow.yaml` 热加载 + `enabled` 开关(F82)

- **`WorkflowSpec::enabled: bool`**(default `true`,opt-out 形式 `enabled: false`)— V0.4.6 加在 schema 顶层,daemon 跳过 `enabled: false` 的 workflow,
  仍在 roster 内但不跑(`workflow_done reason="disabled"` 写进 progress.jsonl)
- **daemon 监听 workflow.yaml mtime + 内容 hash**:每个 rostered 项目装一个 inotify watch on `<project>/.ccteam/workflow.yaml`(F83 canonical 位置,旧位置 fallback);
  改动 → 解析新 spec → diff 老 spec:
  - `enabled: false` → cancel token trigger 优雅终止老 event_loop,写 `workflow_done reason="disabled"`
  - `enabled: true` 且老 loop 在 → 替换 spec,trigger 变了重装 ArtifactWatcher
  - `agents` 拓扑变 → 终止老 loop + 重启新 loop(干净)
- **cancellation token 而非 `JoinSet::abort_handle()`** — abort_all 会硬中断 in-flight session,改用 `tokio::sync::Notify` cancel token,event_loop 在 `select!` 中等 token → 收到后写 `workflow_done` 事件 + clean exit

#### 3.10.3 workflow.yaml 位置迁移到 `.ccteam/`(F83)

- **新建项目**:`ccteam init` / `ccteam new` 写到 `<project>/.ccteam/workflow.yaml`(不再 root)
- **`.gitignore` 整段已经包含 `.ccteam/`** — workflow.yaml 自然 gitignored(orchestration state 不入业务库,正合 CLAUDE.md §三红线)
- **read 优先级**:`<project>/.ccteam/workflow.yaml` > `<project>/workflow.yaml`(旧位置 fallback,V0.5 删)
- **migration**:`ccteam doctor --migrate-workflow-to-ccteam-dir` 把根上 workflow.yaml 移到 `.ccteam/`

**红线**:`.ccteam/workflow.yaml` 是项目级 orchestration SoT,业务代码 / `.git/` / `.env` 永远不动。

---

## 4. 关键流程（V0.3 phase pipeline 历史）

> **V0.4.0 起 EOL**:9-phase happy path / Seed-Plan-Implement-Test 串行流 / "phase 间数据流" / "stall + cost 阈值告警" 全部 EOL — phase 概念已废。V0.4.6 实际形态:用户写 workflow.yaml + agent prompt → orchestrator 跑 ArtifactWatcher 触发 bg-job → progress.jsonl 7 类事件累积 → meta-agent 看事件 + 收 escalation/budget_exceeded notify。完整 V0.4.0 happy path 示例见 `docs/v0-4-0/prd.md` §3。下面 V0.3 流程作历史保留。

### 4.1 端到端：从想法到交付（Happy Path，V0.3 历史）

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

### 6.1 Tmux 长 session 调用模板（V0.3.x 历史 / Codex adapter 仍用）

> **V0.4.0 起标 historical**:Claude bg-job(`claude --bg --agent`)取代了项目级 tmux 长 session(详 §2.2 关键架构决策)。**meta-agent session 仍走 tmux**(事件循环需要长跑 + attach);**Codex CLI adapter 仍走 tmux**(独立 adapter,F62 推迟 bg-job 化)。下面模板保留作:(1) meta-agent / Codex adapter 实现参考;(2) 历史架构演进记录。常规项目 agent 现走 §3.3 workflow.yaml 触发 bg-job 路径,**不再 send-keys 长 session**。

**为什么不用 `claude -p` 子进程**(V0.3 当年理由,V0.4.0 后 bg-job 已替代):每 phase 起新进程意味着重读 CLAUDE.md / spec / 上游产物,反复触发冷启动;prompt cache 5 分钟 TTL 命中不到。长跑项目(数小时-数天)改用一个**项目级长 session**——同 session 跨 phase 共享缓存,且天然支持随时 attach 观察与介入。

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

### 6.3 Multi-agent 编排（V0.4.0 起重塑;phase 内并行 + cross-cutting watcher 历史模型）

> **V0.4.0 起重塑**:phase 概念已废,所以"phase 内并行" / "cross-cutting watcher" 都不再以这种形态存在。V0.4.0 后的多 agent 编排走 §3.3 workflow.yaml + `Trigger::Watch(path)` + `AgentSpec::parallelism: u32` — 一条 workflow.yaml 内多个 role(典型 explorer / fixer / reviewer / master)各自 trigger,fixer 并发 3 等并行靠 parallelism cap;cross-cutting watch 直接由 ArtifactWatcher 监听 artifact 目录实现(每写一个新 artifact spawn 一个对应 role 的 bg-job)。
>
> 下面三种模式作历史架构演进记录保留(V0.3.x 落地形态);新建项目用 workflow.yaml 拓扑替代。

ccteam 用 multi-agent 编排同时承担两个不同目标——**质量**（痛点 11 L2，多视角议事）与**速度**（痛点 13 L 加速，多角色并行）。两个目标用同一个 Agent Teams 机制实现，但 phase prompt 中表达不同：

V0.3.1 另加一条**进程级**并行路径: `kind: flex` 项目通过
`ccteam session add/ls/attach/rm` 手动管理多个 harness session。它不是
phase 内 Agent Team,也不参与 fan-out/fan-in;每个 session 都有独立 cwd /
progress stream / harness snapshot,用于用户原生 Claude Code 工作流与
cross-review。

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

`ccteam init` / `ccteam new` 在项目根生成(`<project>/CLAUDE.md`),内容不再带 phase 字段(V0.4.0 phase 概念已废):

```markdown
# CLAUDE.md (auto-generated by ccteam)

## 项目上下文
- slug: dev-bookmark-mgr-a3f9
- 用户原始需求: 见 .ccteam/spec.md(可选)
- workflow 拓扑: 见 .ccteam/workflow.yaml(V0.4.6 F83 canonical 位置)
- agent 行为: 见 .claude/agents/<role>.md(每 role 一份)

## 工作约定
- 不要交互式询问(`AskUserQuestion` 由 hook 拦截 → 写 .ccteam/outbox/ 反馈用户)。
- 测试不过不算完成。
- 修改 API 必须同步 .ccteam/api-contracts.md(如有)。

## 不做的事
- 不要 git push（被 hook 拦截）
- 不要修改 .ccteam/ 之外的元数据
- 不要碰其他项目目录

## 跨项目经验（来自 ~/.claude/rules/ccteam-lessons-<team>.md 自动注入 + per-repo auto-memory）
{{ Claude Code 加载机制自动注入,无需 ccteam 检索 }}
```

agent prompt 在 `.claude/agents/<role>.md` 里:Claude Code 起 bg-job 时按 `--agent <role>` 参数读对应文件。例:

```markdown
---
name: fixer
description: Apply fix-request artifact and run tests
tools: [Read, Write, Edit, Bash]
model: claude-opus-4-5
---

You are a fixer agent. Read input artifact at `$CCTEAM_INPUT/<file>`,
apply the suggested fix, run `npm test`, and write result to `$CCTEAM_OUTPUT/`.
...
```

### 6.6 A2A bridge（可选，未 ship）

如果未来需要"两个 ccteam 实例对话"（例如本地 ccteam 和云端 ccteam 协作），用 A2A bridge 协议。当前不需要。

### 6.7 Skills 复用（gstack 模式）

ccteam 出三个 skill(V0.4.4 起):

#### `ccteam-phases`(**V0.4.0 起 EOL**,phase 概念已废)

V0.3.x 时代用户在自己 Claude Code 里 `/ccteam-implement` 跑单 phase 作 daemon fallback;V0.4.0 phase 全删后此 skill 失去意义。skill 目录留 marker(空 SKILL.md 标 EOL)以防工具 surface 校验告警,实际工作流已迁:
- workflow 自动化 → `ccteam start` daemon + workflow.yaml + ArtifactWatcher
- 手动单跑 → `ccteam internal spawn <slug> <role>` 直接触发一次 agent

#### `ccteam-creator`(V0.4.4 ✅,**workflow.yaml + agent + skill 创建 dialogue 指引**)

```
~/.claude/skills/ccteam-creator/
└── SKILL.md           # 引用官方 workflow.yaml + agent spec,不复制
```

meta-agent / 用户日常 claude 装这个 skill 后可对话生成新项目的 workflow.yaml + `.claude/agents/<role>.md` 骨架。skill 内部引用 Claude Code 官方 agent file spec(不复制),ccteam 只补"agent 之间用 input/output artifact 接力"的连线指引。

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

### 6.9 长跑鲁棒性（V0.3 单长 session 策略,V0.4.0 起部分 EOL）

> **V0.4.0 起部分 EOL**:本节"长 session 上下文膨胀 → 60% 阈值 + phase 边界 reset" 路径只适用 meta-agent + Codex tmux session。Claude bg-job 形态下每次 spawn = 全新 1M context,根本不撑爆,无需 reset。"Phase 注入:idle-aware" / "auto_loop 完结路径" / silence_classifier / pending-inject 等 V0.2.2 F35/F36 机制全部 EOL(phase + send-keys 长 session 消费者已废)。保留作历史架构记录 + Codex adapter 参考。

针对长跑场景的两个典型问题，各采用一条最直接的路径——**不做多层兜底**。

#### 长 session 上下文膨胀 → 60% 阈值 + phase 边界 reset（V0.3 历史）

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

### 6.10 Sub-skill 自动调度（V0.4.0 起 EOL）

> **V0.4.0 起 EOL**:phase + `sub_skills` 字段都废后,sub-skill 调度不再由 orchestrator 在 phase 边界 trigger。新模型 = agent 在自己 `.claude/agents/<role>.md` 内决定何时 `Task(subagent_type=...)` 调 plugin agent / skill;orchestrator 完全不感知。下面 V0.3 设计保留作历史。

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

### 6.11 Multi-session per project（V0.3.1 flex / V0.4.0 起重塑）

> **V0.4.0 起重塑**:`team.yaml::kind: flex` + `ccteam session add` 的 adhoc multi-session 路径在 V0.4.0 phase + team.yaml::kind 全废后语义模糊。V0.4.6 时点 `Command::Session` CLI 仍在(给 Codex tmux adapter 用),但常规 Claude 项目改走 workflow.yaml `parallelism: u32` per role + bg-job 形态实现并发。痛点 13 fan-out / fan-in 概念 V0.4.6 仍未 ship,workflow.yaml 条件分支(V0.5 候选)落地后才能完整对标。下面保留作历史。

V0.3.1 已 ship **adhoc flex multi-session**:只对 `kind: flex` 项目生效,
用户显式 `ccteam session add <slug> --harness=claude` 创建
`<project>/.ccteam/sessions/<sid>/`,master `state.json::sessions` 注册,
tmux 名 `ccteam-<slug>-<sid>`,progress 走
`~/.ccteam/progress/<slug>/<sid>.jsonl`。`session rm` 是唯一用户显式授权
关闭 session 的路径。Codex harness 在 V0.3.1 是 trait stub,V0.3.2 实现。

下面的 **phase fan-out/fan-in multi_session** 仍未 ship(M4.8):

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

### 6.13 Web layer(V0.3 askama+htmx → V0.4.0 Vite+React SPA + V0.4.6 F90 panels)

> **V0.4.0 起 SPA 重塑** + **V0.4.6 F90 加 4 个新面板**:askama+htmx vintage 已删,详 §3.8 "Web 仪表盘" 节当前形态;此节保留作 V0.3 vintage 历史(M5.0-M5.4 ship 时序 + 渲染栈演进)。

V0.3 主线版本新加第四接入层(继 terminal / MCP / filesystem 之后),由 `crates/ccteam-web` crate 提供:

- **入口**:`ccteam web --bind <addr> [--no-auth] [--token-file <path>]`(`docs/interfaces.md` §10.6)。CLI subcommand 调 `ccteam_web::serve(ServeOpts)`,axum 0.8 server 绑端口、装路由、Ctrl-C / SIGTERM 优雅退出。
- **依赖图**:`ccteam-web` 只 depend on `ccteam-core`(F45 promote 后 4 个 write helper 落 `actions::*` 模块)。**严格不 dep `ccteam-cli`** — `crates/ccteam-web/tests/dep_graph_test.rs` 自检 `cargo tree -p ccteam-web` 不命中 `ccteam-cli`。
- **M5.0 范围**:scaffold + `GET /health` 200 JSON + `ServeOpts { bind, no_auth, token_file }` 类型形稳。
- **M5.1 范围**:read-only dashboard(`/`)+ 项目详情(`/project/<slug>`)+ vendored htmx / CSS(`/assets/*`)+ outbox / events 渲染 + status badge(F35 silence_classifier 只读复用)。
- **M5.2 范围**(本里程碑):**SSE 实时事件流 + 按需 PNG 截图**。
  - **SSE topology**:单个 `notify::RecommendedWatcher` recursive 监 `~/.ccteam/progress/`,守一条专用 OS 线程跑 + per-file 字节 watermark + 单一 `tokio::sync::broadcast::Sender<ProgressUpdate>`(capacity = `1024` 字面);两个 SSE handler(`/sse/all` + `/sse/project/<slug>`)各自 `subscribe()`,后者 server-side filter 按 slug 字段。详 `docs/interfaces.md` §15.6 wire format。
  - **截图**:`/screenshot/<slug>.png` 同步调 `ccteam_core::render_screenshot`(F38)wrap 在 `tokio::task::spawn_blocking`(F38 内部 shell out tmux + imageproc 渲染,会 ~200-500 ms 阻塞);F38 graceful degrade(tmux 无 session)→ **504 + plain-text reason**,不 polling(`Cache-Control: no-cache, must-revalidate`)。
- **M5.3 范围**:写动作 endpoint(`POST /api/<slug>/{btw,inject_decision,pause,resume}`)+ 默认 token 鉴权(loopback bypass + 非 loopback 自动生成 `~/.ccteam/web-token` mode 0600 + URL shim cookie + CSRF 防御)。
- **M5.4 范围**:`crates/ccteam-web/tests/e2e_test.rs` 端到端 canary(GET / → /project/<slug> → SSE → POST /btw 跨层 sequenced)+ `docs/v0-3/e2e-retro.md` ship 报告 + workspace.version `0.2.2 → 0.3.0` + CLAUDE.md baseline 回填。详 `docs/v0-3/prd.md` §3-§7。
- **V0.3.1 范围**:flex 项目 web 适配(`/session/<slug>/<sid>`、sid-scoped
  SSE / harness / screenshot / `/btw`),`crates/ccteam-web/tests/flex_e2e_test.rs`
  作为 F51 ship gate canary。
- **架构红线**(V0.3 主线维持):progress.jsonl 仍是 SoT,web 不解析 tmux 终端(M5.2 SSE watcher 仅读 progress.jsonl,F38 截图内部 vt100 化非文本解析);web 不 kill 长 session;web 不写跨项目记忆;`/btw` 走跟 telegram channel + MCP `send_to_session` 完全相同的 inbox + idle dispatch 路径,不开新通路。

### 6.14 Graceful shutdown(V0.4.6 F86)

V0.4.5 daemon 收 SIGTERM 时直接 `JoinSet::abort_all()`,所有 event_loop 硬中断 → in-flight session 不写 `workflow_done`,下次启动靠 F80 phantom cleanup 补 synthetic agent_done。F80 是症状缓解,F86 才根治。

**机制**:

- **`Orchestrator::shutdown_token: Arc<Notify>`**(或 `watch::Sender<bool>`)— daemon 主循环 `select!` 中 arm 一条 `shutdown_token.notified()`
- **`ccteam stop`** 不再 `kill PID` + poll pidfile,改:
  - 写 `/tmp/ccteam-<user>.shutdown` trigger 文件 → daemon 文件 watcher 收到 → trigger Notify
  - daemon 主循环 arm 触发 → cancel 所有 event_loop(用 F82 cancellation token,workflow_done reason="shutdown") → JoinSet `join_all()` 等所有 task 真正退出
  - **timeout 30s 后才走 abort_all() fallback**(防卡死 event_loop 永远不返回)
- **SIGTERM/SIGINT 兼容**:linux signal handler 也 trigger shutdown_token(双触发兼容 systemd / docker stop)

**红线**:cancel token + Notify 用 `tokio::sync` 原语,不引入新依赖;`abort_all()` 仍作 30s timeout fallback 保留,不删 — 防 event_loop 自身卡死 await。

**与 F80 phantom cleanup 关系**:F86 让 graceful 路径下 `workflow_done` 完整写入,F80 cleanup 在异常路径(panic / OOM kill / `-9`)仍作兜底。两层互不冲突。

### 6.15 Cost telemetry(V0.4.6 F91 + V0.4.7 F92 known gap)

V0.4.5 cost 三个数据源:
1. `state.cost_used_usd`(per project,在 `~/.ccteam/projects/<slug>/state.json` 里)— 由 `Hook::CostAccumulate` 接收 stdin parse claude 输出累加
2. `progress.jsonl::agent_done.cost_usd`(per session,F66 hook 写)
3. `claude_job::probe_job` 读 `~/.claude/jobs/<id>/state.json::cost_usd_total`(F80 加)

任一 hook miss → ccteam 端 cost 漂。**真实来源就是 Claude 自己写的 state.json**,ccteam 不该自己再算一份。

**V0.4.6 F91 收敛**:

- **删 cost 累加路径**:`Hook::CostAccumulate` enum branch + `ccteam_hooks::cost_accumulate` 函数 + `ccteam doctor --install-hooks` 模板里的 `cost-accumulate` hook 全删;`doctor --update-hooks` 同步清现有项目 settings.json
- **`state.cost_used_usd` 字段保留 serde compat**:`#[serde(default)]` 接受老文件,写入路径不再 mutate(标 `#[deprecated]`,V0.5 删),读取路径(`workflow_summary` / `ccteam show`)改用:
  ```rust
  pub struct CostSummary {
      pub cost_24h_usd: f64,      // sum progress.jsonl::agent_done.cost_usd within 24h
      pub cost_active_usd: f64,    // sum live ~/.claude/jobs/<active>/state.json::cost_usd_total
      pub cost_total_usd: f64,
  }
  ```
- **F84 budget cap** 用 `cost_24h_usd` 判定;**F90 Cost sparkline** 用同源数据

**已知 gap(V0.4.7 F92 候选)**:V0.4.6 仍有 `agent_done.cost_usd` 字段需要 hook 写 — 真实数据其实在 `~/.claude/jobs/<id>/linkScanPath` 下的 jsonl 里。F92 候选打算直接从那读,完全摆脱 hook 依赖。当前 V0.4.6 在 hook miss 场景下 `cost_24h_usd` 仍可能漂(已知 limitation,记录于 `docs/v0-4-6/prd.md F91 验收 #3`)。

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
