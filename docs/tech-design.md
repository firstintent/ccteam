# ccteam 技术实现方案

> 本文档基于 [requirements.md](./requirements.md)（已确认的用户痛点），以 Claude Code 为执行 agent，给出 ccteam 当前 (V0.6.0) 的技术架构、组件分解、数据协议、扩展点映射。
>
> **核心问题**：用户用一句自然语言提需求，系统自动产出可运行软件——且**不需要主对话窗口在线**、**多项目自动排队**、**测试不过不交付**、**经验跨项目沉淀**；V0.6.0 起扩展到 **24/7 长跑 IM bot 形态**(模式 3),`vendor: {claude, codex}` 双 LLM 一等公民。

---

## 0. 红线表(V0.6.0 F106 双轴 scope)

> 详 `docs/versions/v0-6-0/README.md §五`;CLAUDE.md §三 是简版镜像。任何 PR 违反红线 = block。

| 红线 | 模式 1 in-proc | 模式 2 bg(Claude / Codex)| 模式 3 chat(Claude / Codex)|
|---|---|---|---|
| R1 文件系统是控制平面 | — | 守(artifact 双 vendor) | 守 — Claude: tmux 长 session + transcript jsonl byte-offset 增量读;Codex: app-server UDS;两 vendor 共写 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl` |
| R2 `progress.jsonl` 唯一 state SoT | — | 守(双 vendor) | 业务事件 SoT 守(7 类 + `chat_session_reset` / `turn_done`);对话原文走 `turns.jsonl` |
| R3 No prompt injection | 守 | 守 | 守 — `/compact /new /clear` 完全透传 |
| R4 每次 spawn = fresh 1M context | — | Claude: `claude --bg`(无 `--resume`);Codex: trait 决定可复用,用户不见 yaml | 不适用 — chat 复用 context 是 feature |
| R5 永不主动 kill 长 session | 守 | per-vendor `budgets.{claude,codex}.max_cost_usd_per_24h` 触顶 → F84 auto-disable | tmux 长跑 24/7;`/compact /new` 是合法 turn |
| R6 不解析 tmux 终端输出 | — | 守 | 守 — 读 transcript jsonl + Claude Code 官方 hooks fast event 通道;`tmux capture-pane` 仅 dev-time 调试 + screenshot 只读 |
| R7 fix-loop 3 次必 escalate | 守 | 守(`fix_counts` map) | 守 + AgentPath depth limit(借 Codex `agent_max_depth`)替代平铺 fix_counts |
| R8 `ccteam-core` 零 team 名字面量 | 守 | 守 | 守 |
| R9 跨项目记忆走官方接口 | 守 | Claude: `~/.claude/{CLAUDE.md,rules}`;Codex: `~/.codex/AGENTS.md`(`ccteam init` 落 symlink)| 同 |
| R10 新建项目走 `<projects_root>/<team>-<slug>/` | — | 守 | per-bot tmux session = `<project>/<bot>`;IM bot 落 `.ccteam/chat/<bot>/` |

**vendor 列补充**(V0.6 F107 / F112):**不 vendor** Claude / Codex 二进制(`references/` git-ignore,实际 spawn 走 `$PATH` binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override);`vendor: AgentVendor::{Claude, Codex}` enum 是 trait 一等公民,无 default。

---

## 1. 设计原则

每条原则都直接挂钩 `requirements.md` 中的某条痛点。

| 原则 | 对应痛点 | 落地约束 |
|---|---|---|
| **守护进程化** | 痛点 9：AI 团队需要人来主持 | Orchestrator 独立于任何 Claude Code 主对话，systemd / cron 长跑；F86 graceful shutdown cancel token，SIGTERM / `ccteam stop` 写 `/tmp/ccteam-<user>.shutdown` → daemon 收到 trigger → event_loop 优雅退 + 写 `workflow_done reason="shutdown"` |
| **文件即状态机** | 痛点 7：进度永远不透明 | 一切状态可从文件系统恢复；进程重启不丢任务；**`progress.jsonl` 7 类业务事件（workflow_start / agent_spawn / agent_done / artifact_received / gate_triggered / budget_exceeded / workflow_done + escalation）是唯一 SoT**（§5） |
| **声明式拓扑** | 痛点 12 + 痛点 13 | `workflow.yaml::agents` 声明角色 + Trigger + parallelism + 可选 budget；orchestrator 用 ArtifactWatcher（inotify/fsevents）把文件系统事件 → agent spawn；agent 行为完全在 `.claude/agents/<role>.md` 内，与编排解耦 |
| **bg-job 形态** | 痛点 8/9 + Claude 后台模式 | Claude agent 走 `claude --bg --agent <role>` 写 `~/.claude/jobs/<job_id>/state.json`，orchestrator 读 state.json 拿 liveness + cost；Codex CLI adapter 仍走 tmux + statusline（独立 adapter，F62 推迟标准化）|
| **多 trigger / 受控并发** | 痛点 13 | `Trigger::Watch(path)` + 每 agent `parallelism: u32` 上限（只对 Watch 触发有意义，其他 trigger 强制 ≤ 1）；Manual / Schedule / Gate 三类 trigger 各自语义清晰（§3.3） |
| **3-Strike 自愈再升级** | 痛点 4：bug 修复无限循环 | thin orchestrator 维护 per-role `fix_counts`，撞 3 次顶 → `escalation` 事件 + meta-agent inbox notify；fix-loop 形态 "watch:fix-requests/ 上 fixer 写第 4 个 file → escalation"（§3.5） |
| **跨项目沉淀** | 痛点 10：每个新项目从零开始 | 复用官方 `~/.claude/CLAUDE.md` + `~/.claude/rules/ccteam-lessons-<team>.md` + per-repo auto-memory；每次 `claude --bg` 起新 job = 全新 1M context，加载机制自动注入；**ccteam-core 零 memory 检索代码**（§3.7） |
| **零交互沙盒** | 痛点 8：每一步都点允许 | 项目级 Docker / 容器隔离 + Claude bg-job 默认 `--dangerously-skip-permissions`；每个项目根 / `.ccteam/` 隔离 |
| **决策点 ≤ 3** | 痛点 2：AI 仍要求我当 PM | 只有不可逆决策（架构、scope 大改、API 形态）才走 escalation 事件 + meta-agent inbox |
| **预算硬上限**（F84） | 痛点 5 + 自激励 loop 防失控 | `workflow.yaml::budget.{max_cost_usd_per_24h, max_agent_spawns_per_hour}` 任一超限 → 写 `budget_exceeded` 事件 + 自动 `enabled: false` 优雅终止 event_loop；cost 数据源 F91 已收敛（详 §6.12） |
| **agent 内调 skill / subagent** | 痛点 12：工作流插件靠人手动调 | agent 在自己的 `.claude/agents/<role>.md` 里声明 `Task(subagent_type=...)` 与 skill 调用；orchestrator 不调度 sub-skill，agent 内部决定；复用 claude-plugins-official plugin，不重写 |
| **纵深防御替代人值守** | 痛点 11：关键节点不把控 | L1 架构约束（hooks + 危险命令拦截）+ L2 多 agent 互检（workflow 内 explorer / fixer / reviewer 多视角）+ L3 用户兜底（仅 escalation / budget_exceeded 弹）；详见 §3.6 |
| **smart layer 只 translate，不 decide** | watchdog / 后续 ux-helper 不能改 orchestrator 状态 | translation 层只读取既有遥测，产出 NL 通知；**绝不**调 orchestrator API、写 progress.jsonl、kill session；**所有状态变更只能由 orchestrator + hooks 走；** 详见 §3.9 |

---

## 2. 总体架构

### 2.1 三层架构（Channel / Interaction / Orchestration）

V0.6.0 起 **Channel Layer 实化为 `ccteam-imd` supervisor**(F116;V0.6.1 F130 折入 `ccteam start` 单进程,作为 tokio task 与 orchestrator + web 共享 shutdown channel,独立 binary 已删)+ 统一 `openhuman/channels` Rust crate 14+ IM 平台(F109);**HarnessAdapter trait 是 5-method thread/turn 接口对齐 Codex `ThreadManager::{submit, next_event}`**(F107);新增**模式 3 chat 形态**(F108):per-bot tmux 长 session + Claude TUI 长跑 + dual-track hooks+transcript 镜像 ccteam-owned `turns.jsonl`。V0.6.3 F143 起 Channel Layer 还包含 **webhook ingress**(`POST /webhook/:project/:token`,挂在 daemon 已有 axum web server,token-only constant-time 比对 + 256 KiB body cap),payload 落 `<project>/.ccteam/webhooks/<ts>-<rand>.json`,由 agent 现成的 `trigger: watch:.ccteam/webhooks/` 消费 —— **`Trigger` enum 零改动**;webhook 不内嵌 LLM、不进 spawn argv,与 inbox 同级别"dumb router 写文件"。

```
Channel Layer (V0.6.0 F109+F116 实化;V0.6.1 F130 single-process; V0.6.3 F143 加 webhook ingress)  →  inbox/outbox 文件协议 + IM event bus
   ccteam-imd supervisor task (in-process tokio task inside `ccteam start`)
     ├── openhuman/channels Rust crate (telegram/slack/discord/lark/dingtalk/qq/...)
     ├── Reply Listener (borrowed OMC reply-listener.ts) + bot-to-bot @ routing + hop_limit
     ├── HarnessAdapter trait 调用(把 inbound IM 消息翻译成 TurnInput)
     └── HTTP webhook route (V0.6.3 F143; axum POST /webhook/:project/:token → .ccteam/webhooks/)
        ↓
User Interaction Layer (M1, V0.6 F108 扩 chat)
   meta-agent session + project agent bg-jobs (~/.claude/jobs/<job_id>/)
   + V0.6 F108: per-bot tmux long-session (claude TUI 24/7)
   + V0.6 F112: Codex adapter(`codex exec` bg / `codex app-server` UDS chat)
   接入面契约：<project>/.ccteam/{workflow.yaml, <artifact_dir>/, inbox/, outbox/,
                                  chat/<bot>/turns.jsonl, handoffs/<workflow>/<stage>.md}
                + .claude/agents/<role>.md
        ↓ inotify ArtifactWatcher / inbox watcher / Claude Code hooks(模式 3 fast event)
Orchestration Layer (F66 thin orchestrator)
   Rust daemon + progress.jsonl 业务事件 SoT(7 类 + chat_session_reset + turn_done)
   + ArtifactWatcher (F64)
   每 workflow 一个 event_loop (JoinSet + F82 cancel token + F86 graceful shutdown)
   26 个 mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}* tools (V0.6 F111 24 + V0.6.1 F128 +2)
   + hooks (§6.4) + cost telemetry (F91 + V0.6 F112 双 pricing table, §6.12)
```

**HarnessAdapter trait**(V0.4.0 落地 `crates/ccteam-core/src/harness.rs:75`,V0.6 F107 重写对齐 Codex `ThreadManager`):

```rust
pub trait HarnessAdapter: Send + Sync {
    async fn start_thread(&self, spec: &AgentSpec, ctx: &SpawnCtx) -> Result<ThreadHandle>;
    async fn submit_turn(&self, h: &ThreadHandle, input: TurnInput) -> Result<TurnId>;
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle>;
    async fn close_thread(&self, h: &ThreadHandle) -> Result<()>;
}

pub enum TurnInput {
    UserText(String),
    Artifact(PathBuf),           // bg mode inbox
    SystemDirective(String),     // /compact /new /clear 退化为特殊 turn
    Image(PathBuf),              // rich media(F108)
}

pub struct ThreadHandle {
    pub vendor: AgentVendor,     // Claude | Codex
    pub mode: ExecutionMode,     // InProc | Bg | Chat
    pub identity: String,
}
```

`/compact /new /clear` 不再是独立 `LifecycleOp` enum,而是 `TurnInput::SystemDirective("compact")` 特殊 turn — adapter 内部翻译为 backend-specific 操作(Claude → `/compact` slash 透传;Codex → `compact_remote` API)。**统一模式 2 + 模式 3 = "all chat is a turn sequence"**。

**Vendor-seam forward-compat**(V0.6.3 F144):ccteam 读 sub-harness 吐出的 `state.json` / `codex exec --json` / `codex app-server` JSON-RPC 通知 —— 这些是 vendor 自有 schema,按自家节奏新增字段 / 新 enum 值,ccteam 清不掉也管不了(与「不做历史迁移」红线管 ccteam 自有 state 不冲突)。`ccteam-core::vendor_compat::warn_unknown_vendor_token(seam, token, detail)` 是 process-wide warn-once helper(`(seam, token)` 作 dedup key,不在每个 poll tick 刷屏)。三处接缝的降级策略:**未知 Claude job state** → 当非终态、orchestrator 继续 probe(宁可多 probe,不可误判 done 留 phantom job);**未知 Codex `--json` event** → skip + warn(不中断 event stream);**未知 Codex app-server notification** → 同样 skip + warn。回归测试喂合成 future-JSON 锁死「不 panic + 降级符合预期」语义。

#### 2.1.1 三层各自的职责边界

| 层 | 谁负责 | 内嵌 LLM? | 何时落地 |
|---|---|---|---|
| Channel | 翻译外部消息系统 ↔ inbox/outbox 文件协议；无业务语义 | ❌（Symphony 反模式禁止） | M2+ stub，首选复用开源方案 |
| User Interaction | LLM 驱动的对话与决策（meta-agent + 项目 agent bg-job + agent-team teammate）；**所有 NL 理解、任务调度、记忆调用都发生在这一层**。V0.5.0 F101: meta-agent 重新定位为**轻量 router + cross-project memory bridge + dashboard chat**(不再当 phase pipeline coordinator,也不当 agent team lead;创建项目走 `ccteam-creator` skill,起 agent team 让用户在自己 session 跑 `/ccteam-team`) | ✅ 但**只通过 ccteam-managed claude session 落地**，不是适配器进程内的 LLM | 项目 agent ✓（M0）；meta-agent ✓（M1） |
| Orchestration | Rust 编排状态机 / 文件系统状态平面 / 进程生命周期 / hooks 反射 | ❌（永远是 Rust） | ✓（M0 + M0.5） |

#### 2.1.2 进程视图

- **meta-agent session** 是独立 tmux + claude 长进程（meta-agent 走 dispatcher role，事件循环需要长跑 + attach）。
- **项目 agent job** 走 `claude --bg --agent <role>`，每个 spawn 是独立 bg-job 进程，生命周期 = 一次 trigger → 一次 agent_done 即终止；项目持久化在 `<project>/.ccteam/state.json` + `<project>/.ccteam/workflow.yaml`，agent 进程都是短命的。
- **Codex 例外**：Codex CLI adapter 走 tmux + statusline 长 session 路径（F62 推迟标准化）。
- **Rust orchestrator daemon** 独立进程（F66 thin 形态，只读 progress.jsonl + 文件系统 trigger）。
- **channel adapter**（M2+）又是若干独立进程。

所有进程之间用**文件系统协议**通信，**不用共享内存 / sockets / IPC**（§5 与 §3.1）。
进程崩溃只丢自己的进程内存，文件状态留给重启后恢复。F86 graceful shutdown：daemon 收到 SIGTERM / `/tmp/ccteam-<user>.shutdown` → 所有 event_loop 优雅退、`workflow_done reason="shutdown"` 写进 progress.jsonl、in-flight bg-job 留给下次启动 F80 phantom cleanup 补 synthetic agent_done（详 §6.11）。

### 2.2 关键架构决策

**为什么 Orchestrator 在 Claude Code 之外（不是 Agent Teams 的 Lead）？**

- Agent Teams 的 Lead 必须保持主对话存活，违反"关掉电脑也要跑"（痛点 9）。
- 长跑守护进程（Rust）原生支持 systemd / 重启自恢复，符合 Symphony "tracker-driven recovery" 思路。

**为什么用文件系统当控制平面，不是 Linear / GitHub Issues？**

- Symphony 选 Linear 因为其用户是企业团队，已有 issue tracker。
- ccteam 的用户是独立开发者，引入外部 tracker 增加摩擦。
- 文件协议零依赖、可审计、可备份。
- 真要外部 tracker，留作可选 adapter（M3+）。

**`claude --bg --agent` bg-job 形态（为什么不用 tmux 长 session）：**

- **后台模式成熟**：Claude Code 已成熟支持 `--bg --agent <role>` 写 `~/.claude/jobs/<job_id>/state.json`，可外部观测 liveness + 累计 cost，无需再靠 tmux UI / stream-json 解析。
- **每次 spawn = 全新 1M context**：bg-job 本身就短命（workflow agent 平均 30s-5min），context 不会撑爆，无需额外 reset 路径。
- **agent 行为 SoT 分离**：`workflow.yaml` 只声明 trigger + 连线 + 并发上限，**不含 prompt**；agent 系统提示 / 工具表面在 `.claude/agents/<role>.md`（Claude Code first-class spec），编排器与 prompt 完全解耦。
- **trade-off**：cache miss 频率比单长 session 高，但 spawn 也短；单次 spawn cost 易算清，F84 budget cap 直接套（workflow.yaml `max_cost_usd_per_24h`）。
- **Codex CLI 适配器例外**：Codex 走 tmux + statusline，作为独立 adapter 路径保留（详 §6.1）。

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
3. 进入项目目录，meta-agent 触发 workflow

### 3.2 Orchestrator 守护进程

**实现选型**：Rust（tokio）+ 单一长跑进程。理由：
- 单 binary 分发，与 hooks 共享同一份 serde schema（progress.jsonl 事件 / state.json 字段）
- tokio 适合"轮询 + 子进程管理 + 多任务并发"
- 单进程拥有所有可变状态——抄 Symphony 的 "single GenServer" 思路，避免锁
- 零运行时依赖，与产出项目的容器化路径正交

**核心循环**（F66 thin orchestrator）：daemon 主循环用 `tokio::select!` 同时等四件事 — (1) `shutdown_token.notified()` → F86 graceful shutdown，cancel 所有 event_loop + 30s timeout fallback `abort_all()`；(2) `JoinSet::join_next()` → event_loop 自己退（workflow_done reason=disabled/shutdown/budget_exceeded）或 panic；(3) `tokio::time::sleep(tick)` → poll global inbox + spawn 新 rostered 项目 + `enforce_budget` (F84) + `cleanup_stale_spawns` (F80)；(4) 每 workflow 一个独立 `run_project(slug, cancel)` event_loop，内部再用 `select!` 等 cancel token + watch_rx.recv()（artifact 来了就 `spawn_claude_bg` + append `agent_spawn` 事件，先 check `running_for_role >= parallelism` 准入）。

**状态模型**：

- **`progress.jsonl` 是 SoT**：7 类业务事件（`workflow_start` / `agent_spawn` / `agent_done` / `artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done` + `escalation`）实时累积；orchestrator 只读这一份做"哪些 agent 在跑、跑了多少、cost 多少"判定。
- **`state.json`** 持久化 ccteam 元数据（cwd / team / per-project counters）；spawn 信息走 progress.jsonl + `~/.claude/jobs/<job_id>/state.json` 双源（前者是事件流，后者是 Claude Code 自写的运行时 SoT，liveness 走 `claude_job::probe_job` 读）。

**单点 + claim 防重**：claim 粒度是 **agent role 级**（同 role 并发上限 = `AgentSpec::parallelism`，只对 `Trigger::Watch` 有意义，其他 trigger 强制单实例）。orchestrator `running_for_role()` 扫 progress.jsonl 末尾事件，有 `agent_spawn` 但还没匹配 `agent_done` 就算 running。

**orchestrator 重启时**：F80 phantom cleanup 扫每个项目 progress.jsonl，凡 `agent_spawn` 无匹配 `agent_done` 且对应 `~/.claude/jobs/<job_id>/state.json` 已不存在或处 terminal state → 补 synthetic `agent_done status="cleanup"` + cost 0；ArtifactWatcher 重新装，event_loop 重新跑，完全无状态恢复负担。

#### 3.2.1 meta-agent / 常驻 role

meta-agent 是一个 workflow.yaml 配 `trigger: manual` 的 dispatcher agent + `<project>/.ccteam/inbox/` 作 Watch trigger 的接力跑者（用户写 inbox → ArtifactWatcher 触发 dispatcher agent 一次）。watchdog / reviewer 同理 — 在自家 workflow.yaml 用 `trigger: manual` + meta-agent 在自己 outbox 写 NL trigger，或用 `trigger: schedule`（V0.6.3 F142 起接真 5 段 cron + skip-missed 语义，详 §3.3.2）。

`teams/meta-agent.yaml` 是首个常驻范例，作为 shipped seed 随 binary 发布；`Orchestrator::new` / `ccteam start` / `ccteam doctor --reset-shipped-teams` 都会把它写到 `~/.ccteam/teams/meta-agent/team.yaml`（只是种子，实际运行时编排器只看 workflow.yaml + state.json）。

**红线**：`ccteam-core` 不出现 team 名字面量（M0.16 基线持续维持）；团队特定行为靠用户写的 `workflow.yaml` + `.claude/agents/<role>.md`。

#### 3.2.2 Team layout + TEAM_SOURCES

每个 team 的 yaml + 模板整目录化：

```
~/.ccteam/teams/<name>/
├── team.yaml          # 配置 schema 见 interfaces §5.5
└── agents/            # 该 team 的 .claude/agents/<role>.md 模板
    └── *.md
```

仓内 ship 同布局（`teams/dev/team.yaml`），`include_str!` 1:1 对应 on-disk 路径。

**三层加载优先级**（`crates/ccteam-core/src/team_resolver.rs`，借鉴 Claude Code `SETTING_SOURCES` 模式）：

```rust
const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,  // <project_dir>/.ccteam/team/team.yaml
    TeamSource::User,     // ~/.config/ccteam/teams/<name>/team.yaml
    TeamSource::Repo,     // ~/.ccteam/teams/<name>/team.yaml
];
```

整团维度，first-source-wins（撞名 project 完全覆盖 user / repo，**不**字段级合并）。读容错（yaml 错 → warn + 下一层），写严格（`save_team` 拒绝覆盖不可解析的现有 yaml）。orchestrator 启动调 `discover_team_names(ctx)` 拿到所有 User+Repo 层的 team 名，逐个走 `resolve_team(name, ctx)` 应用 layered override 语义，组成 `TeamRuntime` 表。

**红线**：`load_team_runtimes` **不**手工扫 `~/.ccteam/teams/`，全部走 resolver — 后续加新 plugin layer 时，只在 `TeamSource::User` 实现里扩展 `path_for`，resolver 主流程零改动。

**Soft rename via aliases**：`team.yaml::aliases: Vec<String>` 让 shipped team 可以改 canonical 名，老项目 `state.json::team` 完全不动；`resolve_team` 先按目录名 try_load，未命中后扫每个 source 按 `spec.aliases` 匹配。

### 3.3 Workflow 拓扑

每个项目用 `<project>/.ccteam/workflow.yaml`（F83 起 canonical 位置，旧 `<project>/workflow.yaml` fallback）声明 agent 拓扑 + trigger + 并发上限，**不含任何 prompt**；每个 agent 的系统提示 + 工具表面在 `<project>/.claude/agents/<role>.md`（Claude Code first-class spec）。完整 schema → **[interfaces.md §17](./interfaces.md#17-workflowyaml-schema)**。

#### 3.3.1 `workflow.yaml` schema 速览

```yaml
name: dex-ui-autoloop                    # 必，workflow 标识
description: explorer/fixer/master 自激励循环  # 可选，UI 用
enabled: true                            # F82，default true，false 时 daemon 跳过 roster
mode: bg                                 # V0.6 F108：bg | chat；default bg
budget:                                  # F84 + V0.6 F112 双 vendor cap
  claude:                                #   per-vendor 子表（V0.6 F112）
    max_cost_usd_per_24h: 5.00
    max_agent_spawns_per_hour: 100
  codex:
    max_cost_usd_per_24h: 5.00
agents:                                  # role → AgentSpec，IndexMap 保留 YAML 顺序
  explorer:
    executor: claude                     # claude | codex（default claude）
    vendor: claude                       # V0.6 F107 trait 一等公民，无 default
    trigger: manual                      # 或 schedule / gate / watch:<path>
    parallelism: 1                       # 只对 watch trigger 有意义，其他强制 ≤1
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

**V0.6.1 F124 + F98 HITL 扩展**（`mode: human-approval` 或 per-agent `plan_approval:` block;两者独立可叠加）：

```yaml
mode: human-approval                     # V0.6.1 F124：第 4 mode,workflow-level gate
                                          # 每个 agent_done 后 orchestrator emit
                                          # plan_decision_required + park pending
                                          # 直到 plan_decision 落 progress.jsonl
agents:
  reviewer:
    trigger: watch:.ccteam/reviews/
    plan_approval:                       # V0.6.1 F98:per-agent gate(可独立于 mode)
      enabled: true                      # default true(写 block 即 opt-in)
      outbox: telegram                   # registered IM transport
      timeout_min: 60                    # 0 = 不超时
      on_timeout: escalate               # escalate | auto-approve | reject
    tools:                               # V0.6.1 F128:admin_add_tool 写入位置
      - ReadFile
      - WebFetch
```

`mode: human-approval` 用于"每 step 都要 approve"(workflow-level);`plan_approval:` 用于"这个 agent 写完 plan 才要 approve"(agent-level)。两者共享 `plan_pending` / `plan_decision` / `plan_timeout` 三 progress event,完整 IM round-trip flow 详 §6.7。

**V0.6 F108 + F114 chat-mode 扩展**(`mode: chat` 时启用顶层 `chat:` 段):

```yaml
mode: chat
vendor: claude                           # workflow 顶层 vendor 默认（agent 可单独覆盖）
chat:
  bot_name: pocket-bot                   # tmux session 名 + IM bot identity
  compact_every_turns: 50                # 自动 /compact 触发阈值（adapter 内部，用户不感知）
  hop_limit: 4                           # bot-to-bot @ routing 深度上限（借 Codex AgentPath）
  recover_last_n_turns: 10               # F118 chat session 失效后从 turns.jsonl 重建 N 条
  chat_acl:                              # 谁可以 DM / 群内 @ 这个 bot（F117 onboarding 落）
    dm_allowlist: [u123, u456]
    group_allowlist: [tg_chat_-100xxx]
```

详后面 "chat-mode design" 章节。完整 schema → **[interfaces.md §17](./interfaces.md#17-workflowyaml-schema)**。

#### 3.3.2 `Trigger` 四类语义

| Trigger | 语义 | 触发源 | 并发约束 |
|---|---|---|---|
| `manual` | 用户 / meta-agent 显式 `ccteam internal spawn <slug> <role>` 或 `mcp__ccteam__workflow_spawn_agent` | CLI / MCP / 用户 inbox 消息 | parallelism 强制 1 |
| `schedule` | 定时 trigger；V0.6.3 F142 接真 cron(`croner` crate,标准 5 段 cron),`AgentSpec::schedule: "<expr>"` 必填(workflow load 时 eager parse,语法错 → 拒载);per-`(project, role)` `last_fire` 持久化在 `<project>/.ccteam/state.json::schedule_last_fire`;**skip-missed 语义** — daemon 停机期间错过的 slot 不补跑,只判「下一个 due 时刻 ≤ now」 | daemon 主循环 tick 评估 | parallelism 强制 1 |
| `gate` | 等 `mcp__ccteam__workflow_trigger_gate` MCP 工具调用释放；释放后消费 input 目录所有 artifact 后再回 gated | MCP 工具 | parallelism 强制 1 |
| `watch:<path>` | inotify（Linux）/ fsevents（macOS）监听项目相对路径，新文件 → spawn 一个 session | ArtifactWatcher（F64，F78 修复项目相对路径） | `parallelism: u32` 上限内并发 |

#### 3.3.3 与 `.claude/agents/<role>.md` 的解耦

**红线**：`workflow.yaml` 不许出现 `prompt:` / `system_prompt:` / `messages:` 字段 — 任何 PR 加这些 = schema violation。

agent 行为完全靠 `.claude/agents/<role>.md`（Claude Code 官方 agent 文件格式）：前置 YAML 声明 `name` / `description` / `tools` / `model`，正文是 system prompt。orchestrator 用 `claude --bg --agent <role> --workdir <project>` spawn 后，Claude Code 读这个文件加载 agent — ccteam 完全不解析 prompt 内容。

这种解耦让用户改 prompt 不需要重启 orchestrator（F82 workflow.yaml 热加载，prompt 改动 claude 下次 spawn 自动加载），改拓扑不需要改 prompt（workflow.yaml 调 trigger 路径，agent prompt 不变）。

#### 3.3.4 跨 session 运行时路由 — `squad:` 块(V0.6.3 F145)

workflow.yaml 顶层新增可选 `squad: { leader, members, hop_limit }` 块,补「跨 spawn / 跨 session 运行时路由」这条窄缝(单 session 内的委派已被 Claude Code `Task` subagent 覆盖,ccteam 不重做)。模型:

- **成员关系静态声明**(声明式拓扑红线守):`leader` 与每个 `members[]` 都必须在 `agents:` map 里有对应 AgentSpec;`members:` 决定了"可被路由到"的 role 集合,改拓扑必改 workflow.yaml,`ls workflow.yaml` 即可审计。
- **运行时分发**:leader agent 往 `<project>/.ccteam/squad/` 写名为 `<member>--<rest>.md` 的 artifact;ArtifactWatcher 监听该 dir,**解析文件名前缀**(不读文件正文 → 不开 prompt-injection 面),spawn 对应 member role。membership 是 declaration,member 不另写 `trigger: watch:` —— 列在 `members:` 即等价 declaration。
- **hop 深度上限**(R7 fix-loop / depth escalate 红线):每次 re-route 文件名编码 `<member>--h<N>--<rest>.md`,`<N>` 达 `hop_limit`(默认 3)→ 不 spawn,改 emit `escalation` 事件(`kind: "squad_hop_limit"`);未声明的 member 前缀 → `kind: "squad_unknown_target"`。
- 路由 dir 的 `ArtifactWatcher` event 用 sentinel role `__squad_route__` 标记(`SQUAD_ROUTE_SENTINEL` 常量),与用户 roster 永不冲突。

完整 schema → **[interfaces.md §17](./interfaces.md#17-workflowyaml-schema)**。

### 3.4 Workspace 隔离与并行

**每项目一个目录**（V0.4.2 F72/F75 起，任意 cwd 经 `ccteam init` 即可成 ccteam 项目；新建走 `ccteam new <slug>` thin wrapper 写到 `~/projects/<team>-<slug>/`）。team 前缀（F22）让 `~/.claude/rules/ccteam-lessons-<team>.md` 的 `paths:` frontmatter 能正确 scope 到该项目。

**项目目录结构**：
```
<project>/                            # 任意路径（F77 walk-up 支持）
├── src/                              # 实际代码（business）
├── tests/
├── package.json / pyproject.toml
├── CLAUDE.md                         # 项目级运营手册（§6.5）
├── .claude/                          # Claude Code 原生约定
│   ├── agents/<role>.md              # agent 行为 SoT
│   └── settings.json                 # hook + enabledPlugins
├── .ccteam/                          # ccteam orchestration state (gitignored)
│   ├── workflow.yaml                 # F83 canonical 位置
│   ├── state.json                    # per-project ccteam 元数据
│   ├── spawn_requests/<role>-<ts>.json  # `ccteam internal spawn` 触发的 marker
│   ├── fix-requests/  done/  ...     # workflow.yaml `Trigger::Watch` 监听的 artifact dirs（用户定义）
│   ├── outbox/                       # agent 写 NL 通知（meta-agent 翻译）
│   └── inbox/                        # 用户写 / agent 间消息
└── .gitignore                        # 包含 .ccteam/ 整段
```

**并发模型**：
- **`AgentSpec::parallelism: u32`** 决定每 role 并发上限（只对 `Trigger::Watch` 有意义，其他强制 ≤1）
- **`MAX_CONCURRENT_PROJECTS`**（~/.ccteam/config.yaml，default 3）决定 roster 上限
- **`workflow.yaml::budget`**（F84）是 per-project 软上限：`max_cost_usd_per_24h` + `max_agent_spawns_per_hour`，任一超限 → `budget_exceeded` 事件 + 自动 `enabled: false`
- **CLAUDE.md §三红线 $200 物理上限**仍守 — 全 ccteam 进程合计累计 cost 超 → daemon 整体 alert（粒度比 budget cap 粗）

orchestrator 每轮 tick 后按这几条做准入控制。

**为什么不用 Conductor**：Conductor 是 Anthropic 的多 session 工作区工具，但要求人在 IDE 里使用。ccteam 用 git worktree + 文件系统 trigger 取代 Conductor 的工作区隔离能力 — 比 IDE 更适合无人值守。

### 3.5 Self-healing Fix Loop

Fix 是 workflow 拓扑里的一个 agent role（典型：`fixer` watch `fix-requests/`），撞 N 次顶由 thin orchestrator 计数后 escalate。

**结构**（典型 workflow.yaml 形态）：

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

**事件流**：
1. **explorer** 写 `.ccteam/fix-requests/req-001.md`（诊断 + plan）→ ArtifactWatcher inotify → orchestrator spawn `fixer` claude bg-job → progress.jsonl 加 `agent_spawn` 事件。
2. **fixer** 读 input artifact → 改代码 → 跑测试 → 测试绿：写 `.ccteam/done/req-001.md`，bg-job 结束（`state.json::state="completed"`）→ Stop hook 写 `agent_done` 事件 + cost。
3. **失败时**：fixer 自己决定写新 `fix-request` 重试（自激励 loop 必须配 F84 budget cap，否则 4h 烧 $1.10 自激励 — 2026-05-16 dex-ui 实证）。
4. **3-strike 计数**：thin orchestrator 维护 per-role `fix_counts`（从 progress.jsonl `agent_done.status="errored"` 累加），撞 3 次顶 → 写 `escalation` 事件 + 推 meta-agent inbox notify；**不**自动停 workflow（F84 budget 才会优雅停）。

#### 3.5.1 escalation 事件

`escalation` 是 progress.jsonl 7 类业务事件之一，payload：

```json
{
  "ts": "2026-05-16T12:34:56Z",
  "event": "escalation",
  "kind": "fix_count_exceeded" | "manual" | "budget_exceeded_fallback",
  "role": "fixer",
  "count": 3,
  "last_errors": ["test_foo failed", "test_bar failed", "test_baz failed"],
  "recommendation": "..."  // 可选，explorer/master 写
}
```

orchestrator 看到 → 推 meta-agent inbox 一条 enriched markdown（含最近 200 行 progress events + git diff 最近 3 commits + 最后 3 个 fix-request artifacts）；meta-agent 看 NL 后回用户。

**禁止静默重试**：撞 3 次顶绝不静默 — ccteam 区别于 "AI 永远说没事" 的承诺。

### 3.6 三层防御协议（Defense in Depth）

替代旧方案中"人持续在场审查"的能力，用三层独立机制保证质量与方向不偏（呼应痛点 11）：

#### L1 架构约束（deterministic，写死的红线）

不与 agent 商量、不可绕过。具体形态：

- **agent output artifact 校验**——agent 完成时 hook 校验 `output:` 目录有新文件（缺则视为 errored）
- **危险命令拦截**——`PostToolUse(Bash matcher)` 拦截 `git push.*` / `rm -rf /` / deploy 脚本（详见 §6.4）
- **scope budget**——超出原始 spec 声明 scope 的实现尝试由 scope-watcher（L2）触发 BLOCK
- **不可改 invariant**——`.ccteam/` 之外的元数据不许 ccteam 自动改

**已 ship**：output 校验 + 危险命令拦截（hook 实现，详见 §6.4）；`golden_rules` executor 5 项基础检查 + 项目特定补充（agent prompt 引导调用）。

#### L2 多 agent 互检（stochastic 但多视角）

每个 workflow 内启用相关 audit agent，多视角议事。典型角色：`architect`（gate 触发）/ `critic`/`reviewer`（watch done/）/ `designer`（gate）/ `security`（watch done/）/ `scope-watcher`+`cost-watcher`+`drift-detector`（schedule）。实现复用 `claude-plugins-official` 的 `pr-review-toolkit/agents/*.md`、`feature-dev/agents/code-architect.md` 等直接 `@文件引用`，不重写。

**议事结果**：每个 audit agent 输出 `PASS / CONCERN / BLOCK` —— 全 PASS 自动通过；任意 BLOCK 进入 fix-cycle（§3.5）或转 L3；有 CONCERN 但无 BLOCK 单 critic 模式直接通过（M4.5+ 进入投票）。已 ship：cross-cutting watcher（schedule trigger）+ 单 critic 路径（借鉴 gstack-auto 6 维评分简化版）。未 ship：多 audit 投票 + anti-leniency。

#### L3 用户 fork 决策（last resort）

仅在 L1 PASS + L2 拍不了板时弹出（**不是 first checkpoint**，痛点 11 主路径是 L1+L2）。触发条件：L2 至少一 audit BLOCK 且 fix-cycle 无法修复 / L2 投票分裂 / 用户 careful 模式。形态：写 meta-agent inbox（项目摘要 + 各 audit 立场 + 2-3 个推荐选项 + 一句话 tweak），24h 不响应自动通过。**信任档位**（`~/.ccteam/config.yml`）：`yolo`（永不弹）/ `balanced`（默认，L2 投票分裂时弹）/ `careful`（任何 CONCERN 弹）。

#### 顺序约束

L1 → L2 → L3，不并联。L1 兜系统性偏差、L2 兜单 agent 偏差、L3 兜前两层都拍不了板的偏差。

### 3.7 Cross-project Memory（差异化护城河）

主路径完全复用 Claude Code 官方记忆机制（`~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` + per-repo auto-memory），**检索发生在 Claude session 内部，ccteam-core 零 memory 检索代码**。决策依据见 `references/research/claude-code-memory-research.md`。

**两条共享通道（官方 first-class 机制）**：

| 通道 | 路径 | 加载方式 | ccteam 用法 |
|---|---|---|---|
| 项目内累积 | `~/.claude/projects/<encoded>/memory/MEMORY.md` + topic 文件 | 每 session 启动加载前 200 行 / 25KB，topic 文件按需 | reviewer/master agent prompt 引导 Claude 用 `/memory` 自写 |
| 跨项目共享 | `~/.claude/rules/ccteam-lessons-<team>.md`（支持 `paths:` frontmatter scope） | 每 session 启动加载，匹配路径才生效 | agent prompt 引导 Claude 用 `Edit` 写入 marked section |

**写入时机**（全部经 Claude session 内官方接口，不走 ccteam 代码）：
- 每个 agent 终态（`agent_done`）由 agent 自己决定是否写记忆 — 典型在 reviewer / master role 的 `.claude/agents/<role>.md` prompt 中引导 Claude：
  - 项目特定 lessons → `/memory` 写本仓 auto-memory（Claude 自主决策何时写）
  - 跨项目 lessons / 反模式 → `Edit ~/.claude/rules/ccteam-lessons-<team>.md`（限 `<!-- ccteam-managed:lessons -->` marked section，不污染用户其他段）

**召回时机**（全部经 Claude session 内官方接口）：
- **每次 `claude --bg` 起新 job = 全新 1M context**：bg-job 本身短命（workflow agent 平均 30s-5min 即 agent_done），context 不会撑爆；新 job 启动时 Claude Code 加载机制自动注入 `~/.claude/rules/ccteam-lessons-<team>.md`（匹配 `paths:` frontmatter 的项目）+ per-repo `CLAUDE.md`，零 RPC。
- 需深挖本项目历史 → Claude 用 `/memory` 浏览 + `Read` 读 topic 文件
- 跨项目相似失败 → reviewer agent 看 lessons 后建议 escalation

**可选增强**（用户装了 [claude-mem](https://docs.claude-mem.ai/usage/search-tools)）：
- claude-mem 自带 5 个 hook（SessionStart/UserPromptSubmit/PostToolUse/Stop/SessionEnd）自动捕获，
  ccteam 不调任何 hook；暴露 4 个 read-only MCP tool（`search` / `timeline` / `get_observations` / `__IMPORTANT`）支持跨项目 FTS5 检索 + type 过滤（bugfix/feature/decision/discovery/refactor/change）
- agent prompt 提示"如检测到 `mcp__*claude-mem*search` 工具，可用于跨项目深度检索"，**LLM 自看 tool surface 决定调不调**；ccteam 不写检测代码，不写集成代码
- 用户没装则 100% 走默认路径，功能不受影响

**ccteam 实际改动量**（已 ship，M4.1–M4.4）：唯一一段 ccteam 代码 = `ccteam doctor --install-memory-bridge`（创建 rules 占位文件 + marked section + `paths:` frontmatter）；其余 retro guidance / conversation continuity / 容器 bind-mount 等都是 agent prompt 模板内的指引。

### 3.8 用户接口层

#### claude session 架构层级（全系统视角）

| 层 | 是 claude 吗 | 常驻 / 短命 | 何时出现 |
|---|---|---|---|
| **L0 Channel Layer** | **不是**（各 channel 适配器进程，无内嵌 LLM） | 适配器进程随用户配置启动 | M2+ stub |
| **L0.5 meta-agent session** | **是**（ccteam-managed 常驻 tmux + claude） | 常驻 | 已 ship（M1） |
| **L1 编排层**（orchestrator daemon） | 不是（Rust） | 常驻 | ccteam start 后 |
| **L2 项目 agent bg-job** | 是（`claude --bg --agent <role>`） | 短命（一次 trigger → agent_done） | workflow.yaml trigger 触发 |
| **L3 agent 内 subagent** | 是（Task 工具启动） | 短命（agent 内，跑完返回总结即销毁） | agent prompt 内 ad-hoc 启动 |
| **L4 用户自带 daily-driver claude** | 是 | 由用户控制 | 辅助路径（装 `ccteam-control` skill） |

**关键原则**：**ccteam 不在适配器进程内嵌 LLM**——所有 NL 处理都落到 ccteam-managed 长会话（L0.5 / L2 / L3）上，channel 层（L0）是 dumb router。

**meta-agent（L0.5）vs 项目 agent bg-job（L2）**：meta 是长 tmux + 事件循环 + 用户 attach；项目 agent 是 `claude --bg` 每次新 1M context + state.json liveness + bg-job 无 attach（看 `ccteam show` / web SPA）。两者 prompt 来源不同（meta-agent 装 `ccteam-control`/`ccteam-creator` skill + 跨项目 lessons vs `.claude/agents/<role>.md`）。

#### CLI（F89 后）

CLI 切成 **9 user-facing** + `internal` 折叠组（meta-agent / MCP / hook installer 内部用）。

**用户日常（9 个）**：
```bash
ccteam init                  # 一次性 setup ~/.ccteam/ + 当前目录变 ccteam 项目（F72 三合一）
ccteam start [--no-web]      # 起 orchestrator daemon + 嵌入 web UI
ccteam stop                  # F86 graceful shutdown：写 /tmp/ccteam-<user>.shutdown trigger
ccteam new <slug>            # init thin wrapper：在 ~/projects/<team>-<slug>/ 起新项目（F75）
ccteam ls                    # 列所有 rostered 项目 + daemon health
ccteam show <slug>           # 项目详情：cost / running agents / recent events / budget util
ccteam remove <slug>         # F81 un-roster：守 §三红线（活 session refuse）+ 可选 --purge
ccteam doctor [--gc-claude-jobs|--install-mcp|...]  # 健康检查 + 维护工具
ccteam web                   # 单独跑 web SPA（start 已含，这里给 headless server 用）
```

**Internal**（`ccteam internal <subcmd>`，F89 折叠）：
```bash
ccteam internal hook <progress-append|load-context|intercept-ask>
ccteam internal mcp-serve                       # MCP stdio server，~/.claude.json wire
ccteam internal spawn <slug> <role> [prompt]    # 手动 spawn，写 .ccteam/spawn_requests/<role>-<ts>.json
ccteam internal send <slug> <body>              # 写项目 inbox，allow_hyphen_values（F87）
ccteam internal attach <slug>                   # tmux attach（meta + codex 还用）
ccteam internal peek <slug>                     # capture-pane 不 attach
ccteam internal progress <slug> [--tail]
ccteam internal resume <slug>
```

**关键约束**：所有查询命令支持 `--format json`（详见 [interfaces.md §10](./interfaces.md#10-cli-命令签名)），让用户自带 claude 通过 Bash 工具调时不用解析表格。

#### meta-agent session + inbox/outbox 协议 + ccteam-control skill（已 ship，M1）

- **meta-agent session**（M1.0）：ccteam-managed 常驻 tmux session，
  跑 `claude --dangerously-skip-permissions`，装 `ccteam-control` skill。
  用户用 `tmux attach -t ccteam-meta-<user>` 在终端 NL 对话，meta-agent
  调 ccteam CLI 派单 / 查项目 / 跨项目召回
- **inbox/outbox 文件协议**（M1.1）：`<session>/.ccteam/inbox/msg-<n>.md`
  接收 NL 消息，`outbox/reply-<n>.md` 推回应。orchestrator inotify watch
  inbox，触发 send-keys 注入到对应 session；session 写 outbox，
  Channel Layer（M2+ stub）读 outbox 推到对应 channel
- **ccteam-control skill**（M1.8）：描述 ccteam CLI 命令清单 +
  典型工作流。首要 consumer 是 meta-agent session，次要 consumer
  是用户自己的 daily-driver claude（详见 [interfaces.md §11](./interfaces.md#11-ccteam-control-skillm1)）

#### Channel adapters + ccteam-mcp MCP server

- **Channel adapter 实现**（M2+ stub，未 ship）：Telegram bot / Feishu bot 等。
  **强烈倾向直接复用开源方案**（Claude Code 官方 TG channel /
  python-telegram-bot 等），做最薄的 adapter 层：订阅外部消息 → 写到对应
  session 的 inbox / 从 outbox 推到对应 channel。无内嵌 LLM
- **`ccteam-mcp` MCP server**（M2 ship，**17 个 tool**）：
  - 10 个状态/控制 tool：`ls` / `show` / `new` / `peek` / `progress` / `pause` / `resume` / `inject_decision` / `send_to_session` / `screenshot`
  - 7 个 workflow tool（F65）：`spawn_agent` / `stop_agent` / `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` / `get_artifact_summary`（在 `crates/ccteam-cli/src/mcp_workflow_tools.rs`）
  - meta-agent 与用户 daily-driver claude 都受益（MCP 比 shell parse 更鲁棒）

完整 tool schema 与协议见 [interfaces.md §12](./interfaces.md#12-ccteam-mcp-mcp-server-m2)。

#### Web 仪表盘

`crates/ccteam-web/` 是 Vite + React TypeScript SPA，`build.rs` 在 `cargo build` 时自动跑 `npm run build`（本机 dev 可 `CCTEAM_SKIP_WEB_BUILD=1` 或 `--no-default-features` 跳过）。Backend 仍 axum + SSE，服务 SPA bundle（`/app/*` + `/assets/spa/*`）+ JSON API + SSE。

**Authentication 路径**：loopback 免 token、非 loopback 自动生成 `~/.ccteam/web-token` mode 0600 + 5s LAN-RCE 倒计时；URL shim `?token=ccteam:<hex>` → HttpOnly cookie + 303 → 干净 URL。F88 起 `ccteam start` 输出 token 时同时 probe `xclip` / `wl-copy` / `pbcopy` / `clip.exe` 把 token 拷到剪贴板（`--no-clipboard` 关）。

**SPA 路由 4 页**：
- `/app/` — projects list（WorkflowView 入口）
- `/app/project/<slug>` — workflow view（agent cards + 4 panels）
- `/app/project/<slug>/session/<sid>` — Codex tmux session 详情（codex adapter 用）
- `/app/settings` — token 管理

**WorkflowView 4 个面板**（F90，代码 `crates/ccteam-web/web/src/components/`）：
- **ArtifactQueuePanel** — 每个 `Trigger::Watch(path)` agent 显示待处理 artifact 数 + 最旧文件 age + 最新文件名（后端 `GET /api/v1/projects/<slug>/artifact_queue` 实时 `fs::read_dir`）
- **EventsTimelinePanel** — progress.jsonl 最近 100 行 + 颜色编码（绿色 `agent_done`、橙色 `gate_triggered`/`budget_exceeded`、红色 `escalation`）+ SSE 实时插
- **FailureInspector** — errored agent card 点击 → `GET /api/v1/projects/<slug>/jobs/<job_id>/log?tail=200` 渲染 `~/.claude/jobs/<job_id>/output.log` 尾部（read-only）
- **CostSparkline** — 24h + 7d SVG sparkline，数据源 F91 收敛后的 `workflow_summary.cost_24h_usd` + 历史 `progress.jsonl::agent_done.cost_usd` aggregated by hour（`GET /api/v1/projects/<slug>/cost_history?window=24h|7d`）

F80 加 pulsing-dot 活动指示（每个 agent card running session 有 active dot，SSE 推送）。

**架构红线**：progress.jsonl 仍是 SoT；web 不解析 tmux 终端（SSE watcher 仅读 progress.jsonl）；web 不 kill 长 session；web 不写跨项目记忆；`/api/v1/projects/<slug>/btw` 走跟 telegram channel + MCP `send_to_session` 完全相同的 inbox + watcher dispatch 路径；`cargo tree -p ccteam-web | grep ccteam-cli` 必须 0 命中（独立 dep graph 红线由 `tests/dep_graph_test.rs` 锁）。

#### 前端层（可插拔）

ccteam 核心（orchestrator + hooks + ArtifactWatcher）是 **headless 状态引擎**——所有 UI 都是可插拔前端，共用 `ccteam-core` lib API。

**前端档位**：

| 前端 | 状态 | 性质 | 实现栈 |
|---|---|---|---|
| `ccteam` CLI | 已 ship（M0） | 关键路径（默认入口） | clap derive + serde |
| `ccteam web`（SPA dashboard） | 已 ship | 关键路径 | Vite + React + axum + SSE |
| `ccteam tui` | 未 ship | 机会主义，非关键路径 | ratatui + crossterm |

##### 前端层 invariant（红线）

任何前端**不得**在 ccteam 内引入新 LLM 层。

- ✅ web 通过 SSE 实时观测 + 写 inbox 控制 — 等价于"远程版 NL 派单"。用户在浏览器键入 = 写 inbox 文件，不经任何 ccteam 中介 LLM
- ❌ 不在 ccteam 层起 meta-claude / 自实现聊天 UI / 翻译用户 prompt

LLM 推理只发生在两处：① **L2 项目 agent bg-job**（claude --bg）② **L0.5 meta-agent**（tmux long session）/ **L4 用户自带 claude**。

### 3.9 Watchdog（translation-only smart layer）

> ccteam 的疼点之一是**没人值守时项目静默卡死**——L2 hooks 只能记录，
> 没法主动捅醒用户。**watchdog** 把"低层信号 → 用户能读懂的 NL"独立出来：
> 不是新组件 / 新进程，而是 meta-agent 的一个角色面 + 一组 ccteam Rust 函数。

**translation only 红线**：
- ❌ watchdog 不调 orchestrator API、不写 progress.jsonl、不 kill session、不 re-inject prompt
- ❌ watchdog 不替用户拍板
- ✅ watchdog 只读数据源，翻译成 NL，推到 meta-agent 自己的 outbox

**数据源**（全是只读）：

| 信号 | 路径 |
|---|---|
| `escalation` 事件 | `~/.ccteam/progress/<slug>.jsonl`（meta-agent inbox 也会收 enriched 副本） |
| `budget_exceeded` 事件 | 同上 |
| `cost_overrun` / `agent_stall` | `~/.ccteam/progress/<slug>.jsonl::agent_spawn` 长期无 `agent_done` 匹配 |
| `daemon_down` | `~/.ccteam/state/orchestrator.heartbeat` mtime |

**用户配置**：`~/.ccteam/watchdog.yaml`（interfaces.md `watchdog.yaml schema`）：

```yaml
notify_on_cost_usd: 30.0       # USD，可选
notify_on_agent_stall_min: 60  # 分钟，agent_spawn 后无 agent_done
notify_mode: normal            # quiet / normal / verbose
```

`quiet` 模式只放行 `cost_overrun` + `daemon_down`（钱 / 守护死必报）。

**实施要点**：meta-agent 直接读数据源 NL 翻译，不再走专用子命令（F89 起 `ccteam watchdog scan` 顶层 CLI 已删除）。`crates/ccteam-core/src/orchestrator.rs` **零** watchdog 引用（grep `watchdog` 命中 0 次是核心红线）。

### 3.10 项目生命周期（F81-F83）

V0.4.5 之前没有"删项目"命令，workflow.yaml 改一字段需要 daemon stop/start，workflow.yaml 位置又在项目根上和业务代码混淆。F81-F83 一次解决：

#### 3.10.1 `ccteam remove <slug>`（F81）

```bash
ccteam remove <slug> [--purge] [--dry-run] [--force]
```

- **always**：从 `~/.ccteam/config.yaml::projects[]` 删该 slug；
  通过 F82 wiring 告知 daemon 热剔除（JoinSet abort + cancel token 优雅退）；
  删 `~/.ccteam/progress/<slug>.jsonl`、`~/.ccteam/inbox/<slug>/`、
  `~/.ccteam/control/<slug>/`（如有）
- **`--purge`**：同时 `rm -rf <project>/.ccteam/` + `<project>/.claude/agents/`。**业务代码 / .git/ / .env 永远保留**
- **守 §三红线 refusal**：有活 tmux session / 活 claude bg job / 未匹配 `agent_spawn` 时 refuse，`--force` 才绕过

衍生子命令 `ccteam abandon <slug>` PRD 中讨论后并入 `ccteam remove`（不加 `--purge` 等价 abandon — config 删但项目目录不动）。

#### 3.10.2 `workflow.yaml` 热加载 + `enabled` 开关（F82）

- **`WorkflowSpec::enabled: bool`**（default `true`，opt-out 形式 `enabled: false`）— daemon 跳过 `enabled: false` 的 workflow，仍在 roster 内但不跑（`workflow_done reason="disabled"` 写进 progress.jsonl）
- **daemon 监听 workflow.yaml mtime + 内容 hash**：每个 rostered 项目装一个 inotify watch on `<project>/.ccteam/workflow.yaml`（F83 canonical 位置，旧位置 fallback）；改动 → 解析新 spec → diff 老 spec：
  - `enabled: false` → cancel token trigger 优雅终止老 event_loop，写 `workflow_done reason="disabled"`
  - `enabled: true` 且老 loop 在 → 替换 spec，trigger 变了重装 ArtifactWatcher
  - `agents` 拓扑变 → 终止老 loop + 重启新 loop（干净）
- **cancellation token 而非 `JoinSet::abort_handle()`** — abort_all 会硬中断 in-flight session，改用 `tokio::sync::Notify` cancel token，event_loop 在 `select!` 中等 token → 收到后写 `workflow_done` 事件 + clean exit

#### 3.10.3 workflow.yaml 位置 `.ccteam/`（F83）

- **新建项目**：`ccteam init` / `ccteam new` 写到 `<project>/.ccteam/workflow.yaml`（不再 root）
- **`.gitignore` 整段已经包含 `.ccteam/`** — workflow.yaml 自然 gitignored（orchestration state 不入业务库，正合 CLAUDE.md §三红线）
- **read 优先级**：`<project>/.ccteam/workflow.yaml` > `<project>/workflow.yaml`（旧位置 fallback）

**红线**：`.ccteam/workflow.yaml` 是项目级 orchestration SoT，业务代码 / `.git/` / `.env` 永远不动。

---

## 4. 关键流程

### 4.1 端到端：从想法到交付（Happy Path）

```
T+0:00  用户在 meta-agent inbox（或 channel adapter）说："做个本地书签管理器，离线可用"
T+0:01  meta-agent dispatcher 看 inbox，问 1-2 clarify（如有），调 ccteam new <slug>
T+0:02  ccteam new 写 ~/projects/dev-bookmark-mgr-a3f9/，生成 workflow.yaml + agent 模板
T+0:03  ccteam doctor --install-memory-bridge（首次）；orchestrator 把项目 roster 入队
T+0:05  ArtifactWatcher 装好；meta-agent 调 mcp__ccteam__workflow_spawn_agent role=planner 启 plan agent
T+0:10  planner 写 .ccteam/specs/spec.md + .ccteam/specs/plan.md
T+0:11  ArtifactWatcher inotify trigger explorer（watch:.ccteam/specs/）→ spawn explorer
T+0:30  explorer 写 .ccteam/fix-requests/F1.md（首批待办）
T+0:31  ArtifactWatcher trigger fixer（watch:.ccteam/fix-requests/，parallelism=3）→ spawn 3 fixer 并发
T+1:30  fixer 们陆续写 .ccteam/done/F1.md ...；agent_done 事件累积
T+1:31  ArtifactWatcher trigger reviewer（watch:.ccteam/done/）→ spawn reviewer
T+1:50  reviewer PASS → 通过 mcp__ccteam__workflow_trigger_gate 释放 master agent
T+2:00  master 跑 ship workflow：git tag v0.1.0，写 .claude/rules/ccteam-lessons-dev.md（marked section）
T+2:05  workflow_done 事件；meta-agent 翻译给用户：✅ bookmark-mgr 已交付
```

整个过程中**用户只看到 2 条消息**（提需求 + 收结果）。**完整 happy path 示例** 见 `docs/versions/v0-4-0/prd.md` §3。

### 4.2 失败与升级

| 失败类型 | 处理 |
|---|---|
| claude bg-job 进程 crash | `~/.claude/jobs/<job_id>/state.json::state="errored"`；F80 phantom cleanup 写 synthetic agent_done；3 次撞顶 escalate |
| Codex tmux session 整体丢失 | 起新 tmux + `--resume <session_id>` 全量恢复对话历史（codex adapter 独有路径） |
| agent_stall（agent_spawn 后长期无 agent_done） | **不 kill**，watchdog 推 meta-agent inbox 一条 NL "看起来卡了，要不要 attach 看看" |
| 软成本阈值（项目累计 $20 / $50） | 单次软告警，继续跑 |
| 硬成本上限（F84 `max_cost_usd_per_24h` / 项目累计 $200） | 优雅终止 event_loop + escalate |
| fix-cycle 撞 3 次顶 | escalate，附三次诊断 + last_errors |
| meta-agent dispatcher reject（"做不了"） | 终态，meta-agent 通知用户 |
| explicit `escalation` 事件 | meta-agent 收 enriched payload，翻译给用户 |
| 用户 attach 项目目录手动改代码 | 不阻塞 — agent 下次 spawn 自然带上新改动；改 workflow.yaml 触发 F82 热加载 |

---

## 5. 数据与文件协议

完整字段、JSON schema、文件命名规则、事件类型清单 → **[interfaces.md](./interfaces.md)**。本节只保留架构约束：

| 子节 | 架构约束 | interfaces.md 章节 |
|---|---|---|
| §5.1 全局目录布局 | `~/.ccteam/` 是单一根；不跨用户共享 | [§1.1](./interfaces.md#11-全局目录ccteam) |
| §5.2 项目级 state.json | 原子写（`.tmp` + rename）；per-project 元数据；损坏走 backup | [§2](./interfaces.md#2-state-协议) |
| §5.3 Inbox 协议 | 文件名 `<ISO-timestamp>-<random>.md`，原子写 | [§3.1](./interfaces.md#31-inbox) |
| §5.4 控制协议 | orchestrator 30s 扫，处理后**删除文件**（幂等） | [§3.3](./interfaces.md#33-control用户--orchestrator) |
| §5.5 Progress.jsonl | **唯一状态事实来源**——orchestrator 只读这一个文件做状态转移；tmux 终端输出不参与状态判定 | [§4](./interfaces.md#4-progressjsonl-事件流) |

§5.5 关键论证：**"progress.jsonl 唯一事实来源"是架构红线**。曾经考虑过解析 tmux capture-pane 输出做状态判断——拒，因为终端文本格式不稳定、ANSI 转义难、对 prompt cache 表现敏感。所有状态转移走 hook + bg-job state.json 写出的 JSONL，deterministic 且可重放。

---

## 6. Claude Code 扩展点映射

### 6.1 Tmux 长 session（meta-agent / Codex adapter / V0.6 mode 3 Claude bot）

> 常规 Claude 项目走 §3.3 workflow.yaml + bg-job 路径。tmux 长 session
> **三个 consumer 在用**：meta-agent 需要事件循环 + 用户 attach；Codex CLI adapter
> 模式 2/3（F62 推迟标准化的 bg → V0.6 F112 落地 Option B）；**V0.6 F108 起 mode 3
> Claude bot 也用 tmux 长 session**（per-bot 一个 tmux session + `claude` TUI 24/7 长跑，
> dual-track:Claude Code 官方 hooks 快 event 通道 + transcript jsonl byte-offset 增量读
> → 镜像 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`)。下面模板写给所有 consumer。

**meta-agent session 启动**：
```bash
tmux new-session -d \
  -s "ccteam-meta-${USER}" \
  -c "${HOME}/projects/${USER}-meta" \
  "claude --dangerously-skip-permissions"
# 用户 ccteam start 时自动起；ccteam stop 时 SIGTERM
```

**Codex tmux adapter**(V0.6 F112 Option B mode 3 走 app-server,留 tmux 作 mode 2 退路 + dev attach):
```bash
SESSION="ccteam-codex-<slug>"
tmux new-session -d -s "${SESSION}" -c "${PROJECT_DIR}" "codex --bypass"
# V0.6 mode 3 优先走 codex app-server UDS JSON-RPC v2,tmux 仅 dev attach
# 状态写 .ccteam/sessions/<sid>/state.json(codex adapter trait 实现)
```

**V0.6 mode 3 Claude bot tmux**(F108 决策 — tmux 长跑 + send-keys -l):
```bash
SESSION="<project>/<bot_name>"          # 例:pocket-bot
tmux new-session -d -s "${SESSION}" -c "${PROJECT_DIR}" \
  "claude --dangerously-skip-permissions"
# 输入面:tmux send-keys -l 直送 user content + Enter(text);
#         /compact /new /clear 透明透传;附件走 attachments dir
# 输出面:Claude Code 官方 hooks(UserPromptSubmit / Stop / SubagentStop /
#         SessionStart / PostToolUse)作 fast event 通道 + byte-offset 增量读
#         transcript jsonl → 镜像 ccteam-owned turns.jsonl(R2 SoT)
# F118 chat 失效:从 turns.jsonl tail `recover_last_n_turns` 行重建 context
```

理由(综合 ccgram + OMC production 验证):`-p --resume` 每 turn 冷启 prompt cache 失效 + slash 命令不透传(用户面 UX 退化)+ mailbox-trigger 让短文本走 Read tool 增加 turn cost;tmux 长跑 + send-keys -l 直送是双方共识 + Claude Code hooks 官方文档化 fast event 通道。详 `docs/versions/v0-6-0/README.md §九 决策记录修订`。

**关键约束**(三个 consumer 都遵守):
- ✅ 用 `--dangerously-skip-permissions`（消灭弹窗，痛点 8）
- ❌ **不**用 `claude -p`（失去 attach / 介入能力）
- ❌ **不**设 `--max-turns`（用户要求长跑，由 stall + 成本上限兜底）

**实现注**：tmux 命令包在 `tokio::process::Command`，异步 spawn + 收集 stdout/stderr，失败落 tracing 日志——单 binary 零额外运行时依赖。

### 6.2 项目级 CLAUDE.md（每项目自动生成）

`ccteam init` / `ccteam new` 在项目根生成 `<project>/CLAUDE.md`：

```markdown
# CLAUDE.md (auto-generated by ccteam)

## 项目上下文
- slug: dev-bookmark-mgr-a3f9
- 用户原始需求：见 .ccteam/spec.md（可选）
- workflow 拓扑：见 .ccteam/workflow.yaml
- agent 行为：见 .claude/agents/<role>.md（每 role 一份）

## 工作约定
- 不要交互式询问（`AskUserQuestion` 由 hook 拦截 → 写 .ccteam/outbox/ 反馈用户）。
- 测试不过不算完成。
- 修改 API 必须同步 .ccteam/api-contracts.md（如有）。

## 不做的事
- 不要 git push（被 hook 拦截）
- 不要修改 .ccteam/ 之外的元数据
- 不要碰其他项目目录

## 跨项目经验（来自 ~/.claude/rules/ccteam-lessons-<team>.md 自动注入 + per-repo auto-memory）
{{ Claude Code 加载机制自动注入，无需 ccteam 检索 }}
```

agent prompt 在 `.claude/agents/<role>.md` 里：Claude Code 起 bg-job 时按 `--agent <role>` 参数读对应文件。例：

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

### 6.3 Plugin pipeline

**项目 bg-job 启 plugin agent 走 `enabledPlugins` 路径，不再 ln -sf 进 `~/.claude/agents/`**。

`bootstrap_project` 写 `<project>/.claude/settings.json` 时，按 team 推荐的 plugin 列表（eg `code-reviewer` → `pr-review-toolkit@claude-plugins-official`），写入 `enabledPlugins: {"<plugin>@<mkt>": true}`。Claude Code session 启动时 in-memory plugin pipeline 加载 enabled plugin，**自动加 `<plugin>:` namespace**（eg `pr-review-toolkit:code-reviewer`）；agent markdown 用裸名 `Task(subagent_type="code-reviewer")` 仍然可调，plugin pipeline 自匹配。

- 静态映射表：`crates/ccteam-core/src/plugin_resolution.rs`（`KNOWN_PLUGIN_AGENTS` const，8 个 `claude-plugins-official` agent）
- doctor `--tool-surface` 校验：`enabledPlugins` 引用的 plugin source 文件存在于 `~/.claude/plugins/marketplaces/<mkt>/plugins/<plugin>/agents/<name>.md`

**ccteam-core 不再写 `~/.claude/agents/`**——M4 红线"零检索 + 不写程序读 memory 文件" 扩展到 plugin pipeline：plugin 装载交还 Claude Code 官方 in-memory pipeline，ccteam 只声明依赖。

### 6.4 Hooks 配置

完整 `settings.json` 模板、Hook 事件用途表 → **[interfaces.md §6](./interfaces.md#6-hooks-配置-schema)**。本节只保留架构论证：

**为什么 hooks 是 ccteam 可观测性命脉**：Claude Code hooks 是 deterministic 的（详见 claude-code-best-practices §4.5）——同一事件触发同一脚本，这是把"AI 的随机推理"转成"系统可处理的事件流"的桥。ccteam 把所有工具调用 / agent 边界事件都通过 hooks 落到 progress.jsonl，orchestrator 据此做状态转移，完全不解析 tmux 终端文本。

**实现形态**：hook 实现是 `ccteam internal hook <name>` 子命令（如 `ccteam internal hook progress-append`）——单 binary 分发，与 orchestrator 共享同一份 serde schema（progress.jsonl 事件定义、state.json 字段），不再依赖独立 bash / python 脚本运行时。official plugin 自带的 hook（如 `security_reminder_hook.py`）通过 shell shim 包装挂上，不直接依赖。

**Hook 写作纪律**：
- append 类必须 `async: true`——别拖慢主流程
- 解析 terminal-state 输出的 hook 设 `timeout: 10`，失败要落日志
- hook 脚本放 `~/.ccteam/hooks/`，不放项目目录（避免被 claude 自己改）
- `Stop` 一个 entry 内可挂多 command，但**`decision: block` 决策只能由单点输出**；其它 command 必须 `async: true` 仅做 append/log

**cost 来源关键事实**：bg-job 形态下 cost 由 Claude Code 自己写到 `~/.claude/jobs/<job_id>/state.json::cost_usd_total`；ccteam 读这个值（详 §6.12）。tmux adapter（Codex / meta）由 hook 解析 `transcript_path` 自算。

### 6.5 MCP servers

#### 消费的 MCP（ccteam 不写，只接）

| MCP | 用途 | 出处 |
|---|---|---|
| **Telegram bot** | V0.6 起统一走 `ccteam-imd` daemon + `openhuman/channels` Rust crate(F109/F116);`claude-plugins-official/telegram` 作 backup transport | 用户偏好 backup 时 `/ccteam-im-setup --transport official-telegram` 切换 |
| **claude-mem** | 跨项目记忆**可选增强**（read-only MCP search / timeline / get_observations + 自带 hook 自动捕获）；ccteam 不写集成代码，LLM 自看 tool surface 决定用不用 | 已 ship 为可选项（M4）——默认路径走官方 `~/.claude/rules/` + auto-memory，装了 claude-mem 自动叠加 |
| **Playwright** | E2E 测试（前端项目） | 已有 |
| **GitHub** | PR 创建、issue 管理 | 可选（优先 `gh` CLI） |

#### 提供的 MCP:`ccteam-mcp`(V0.6.5 收官 = 27 工具,**0 STUB**,5 group 子前缀分组)

详见 §3.8 "Channel adapters + ccteam-mcp MCP server" 与 [interfaces.md §12](./interfaces.md#12-ccteam-mcp-mcp-server-m2)。

V0.6 F111 起所有工具加 group 子前缀,**server name 不变**(`ccteam`),用户 `~/.claude.json` 配置零 break:

| Group(子前缀) | 工具数 | 例 |
|---|---|---|
| `workflow_` | 15 | `mcp__ccteam__workflow_{show,peek,progress,new,pause,resume,send_to_session,inject_decision,spawn_agent,stop_agent,observe_agents,signal,set_parallelism,trigger_gate,get_artifact_summary}` |
| `chat_` | 6 | `mcp__ccteam__chat_{register_bot,unregister_bot,list_bots,send_input,history,reset}`(V0.6.5 F146/F147:`chat_lifecycle` STUB 拆为原子 register/unregister + `list_bots` 升真,`send_input` 升真,`session_reset`→`reset` / `show_turn_log`→`history` rename 无 alias)|
| `advise_` | 2 | `mcp__ccteam__advise_{vote,parallel}`(V0.6.5 F152/F153 升真:`vote` = Claude+Codex 并行 advisor + 第三次 Claude verdict synthesis;`parallel` = N-of-N 原文返回;budget gate `<ccteam_root>/cost-budget.json`)|
| `admin_` | 3 | `mcp__ccteam__admin_{ls,change_persona,add_tool}` |
| `screenshot`(单成员独立 group)| 1 | `mcp__ccteam__screenshot` |

**总计 V0.6.5 收官 27 工具,0 STUB,0 deprecated alias**(V0.6 F111 24 + V0.6.1 F128 +2 admin + V0.6.5 F146 chat +1 [register/unregister 拆 1 → 2,net +1] + F152/F153 advise 升真 — 详 `interfaces.md §12`)。`CCTEAM_DISABLE_TOOLS` env 用 group enum(非 glob,防 typo):`CCTEAM_DISABLE_TOOLS=advise,chat` 关掉两组。F110 上版的 `ccteam` → `ct` namespace rename **取消**(V0.5 用户肌肉记忆 override 4 字符节省)。

**Wire 协议纪律(V0.6.5 F165)**:`ccteam mcp-serve` stdout 是 line-delimited JSON-RPC frame channel,**所有 tracing / 日志走 stderr**(`init_tracing_stderr()`),否则 first `tools/list` 那次 `register_bot` 之类的 `info!` 会污染 frame parse → MCP client 解析挂。其他子命令(`ccteam start` / `ccteam web`)stdout 继续是 human readable。`RUST_LOG=error` 不再是 MCP test 的必经环境(F165 前 F147 等用过这个 workaround)。

**实现形态**：`ccteam-mcp` 与 `ccteam-core` 同 workspace（lib + 多 binary），通过 `ccteam internal mcp-serve` 子命令暴露——读写同一份 state.json / progress.jsonl，为 `ccteam tui`（未 ship） / `ccteam web` 三种前端共用 `ccteam-core` lib API（详见 §3.8 前端层小节），MCP 只是把这套 API 套上 MCP wire protocol 给外部 LLM 消费。

---

### 6.5a chat-mode design(V0.6 F108 + F109 + F112 + F116 + F118)

模式 3 = **tmux 长 session + claude TUI 长跑(per bot) + dual-track 观测 + ccteam-owned `turns.jsonl` + ccteam-imd Reply Listener**;**bot-to-bot 100% 走 IM group**(no in-process IPC,no cross-tmux SendMessage — IM history = 完整对话链,hop_limit 在 group msg 链上数)。

**输入面**(`HarnessAdapter::submit_turn`):
- `TurnInput::UserText(s)` → `tmux send-keys -l "$s" Enter`(literal 模式,0 escape 雷区,ccgram + OMC production 验证)
- `TurnInput::SystemDirective("compact"|"new"|"clear")` → `tmux send-keys -l "/compact" Enter` 透明透传(ccteam 不主动调也不过滤;通过 SessionStart hook 观察副作用 emit `chat_session_reset` event)
- `TurnInput::Image(path)` / `TurnInput::Artifact(path)` → 写 `<bot>/attachments/<ts>` + `tmux send-keys -l "Read $path" Enter`

**输出面**(dual-track 观测 — `HarnessAdapter::events` 合流):
- Track A:Claude Code 官方 hooks(`UserPromptSubmit` / `Stop` / `SubagentStop` / `SessionStart` / `PostToolUse`)作 fast event 通道,**低延迟 turn boundary 信号**;每 hook 触发 `ccteam internal hook chat-event-append` 写一行入 turns.jsonl(只 metadata,无 content)
- Track B:byte-offset 增量读 transcript jsonl(`~/.claude/projects/<encoded>/<sid>.jsonl`)→ 抽出 full message content → **镜像写入** ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`(R2 SoT,**不依赖** Anthropic 内部目录长期可读)

**lifecycle 操作**:
- compact: `compact_every_turns` 阈值由 adapter 内部计数(用户不感知);触发时 `submit_turn(SystemDirective("compact"))` 走 send-keys 透传
- session reset 重建(F118): `recover_last_n_turns` 配置;chat session 失效(TUI 崩溃 / OOM / SIGKILL / manual `/clear`)后,新 session 起,从 turns.jsonl tail 读 N 行 → `submit_turn(UserText("[Recovery] previous N turns: ...\n继续对话"))` 重建 context;`progress.jsonl` 写 `chat_session_reset { bot, recovered_turns: N }` event

**bot-to-bot @ routing**(F109 + ccteam-imd 一处实现):
- IM group 内 `@<bot_name> <msg>` → ccteam-imd 解析 → 查 chat_acl group_allowlist → submit_turn 到对应 bot tmux session
- `hop_limit` 借 Codex `AgentPath` 层次树实现(同条 IM msg chain 上数,**不**在 in-process fix_counts 计)
- `@ccteam <NL admin>` 走 meta-agent NL routing(eg `@ccteam pause helpful-bot` / `@ccteam list bots`)

**handoff 机制**(F115):多 bot 协作时,**stage 切换** 写 `<project>/.ccteam/handoffs/<workflow>/<stage>.md` 决策摘要(researcher / writer / reviewer 链);下一 stage bot `submit_turn(UserText("@读 handoffs/.../planner.md"))`,**不**再让用户在 IM 里粘贴上下文。

**进程拓扑**:
```
ccteam-imd daemon (F116)              <- supervisor binary;openhuman/channels event bus
  ├── reply_listener task             <- borrowed OMC reply-listener.ts 模式
  ├── per-channel adapter task        <- telegram / slack / discord / lark / ...
  └── HarnessAdapter call → ccteam-core orchestrator
            ↓ tmux send-keys
ccteam-core orchestrator              <- 项目 daemon(每 workflow 一个 event_loop)
  └── per-bot tmux long-session       <- 名 = "<project>/<bot_name>"
        └── claude TUI 长跑           <- 24/7;Claude Code hooks 同时写 progress.jsonl
              ↑ transcript jsonl polling(byte-offset 增量)→ turns.jsonl
```

**红线对齐**(详 §0):R1 文件系统控制平面 ✓(send-keys + turns.jsonl);R2 progress.jsonl 唯一 SoT ✓(业务事件) + turns.jsonl(对话原文);R3 no prompt injection ✓(`/compact` 等透传);R5 永不主动 kill ✓(`/compact /new` 是合法 turn);R6 不解析 tmux 终端输出 ✓(读 transcript jsonl + hooks,**不** scrape pane);R7 fix-loop 3 次 escalate ✓(AgentPath depth limit 替代平铺 fix_counts)。

### 6.6 A2A bridge（可选，未 ship）

如果未来需要"两个 ccteam 实例对话"（例如本地 ccteam 和云端 ccteam 协作），用 A2A bridge 协议。当前不需要。

### 6.7 Skills 复用（gstack 模式）

ccteam 出两个 skill：

#### `ccteam-creator`（V0.4.4 ✅，**workflow.yaml + agent + skill 创建 dialogue 指引**）

```
~/.claude/skills/ccteam-creator/
└── SKILL.md           # 引用官方 workflow.yaml + agent spec，不复制
```

meta-agent / 用户日常 claude 装这个 skill 后可对话生成新项目的 workflow.yaml + `.claude/agents/<role>.md` 骨架。skill 内部引用 Claude Code 官方 agent file spec（不复制），ccteam 只补"agent 之间用 input/output artifact 接力"的连线指引。

#### `ccteam-control`（已 ship，M1+，用户自带 claude 调度 ccteam 的入口）

```
~/.claude/skills/ccteam-control/
└── SKILL.md           # CLI 命令清单 + 典型工作流 + 何时 attach vs peek
```

**用途**：用户在任意目录开 `claude` → skill 自动激活 → claude 知道：
- 怎么调 `ccteam ls --format json` 看跨项目状态
- 怎么调 `ccteam new "..."` 立项（并先多轮澄清）
- 卡住时综合 `ccteam show <slug>` + `ccteam internal progress <slug> --tail` 给用户一句可贴的纠偏 prompt
- 何时该建议用户 `ccteam internal attach`（meta/codex）vs 看 web SPA

这是 §3.8"用户自带 claude 当辅助路径"的实现。M2 已上 ccteam-mcp MCP server（§6.5），skill 仍保留作为发现 / 引导层。

完整 SKILL.md 内容契约见 [interfaces.md §11](./interfaces.md#11-ccteam-control-skillm1)。

### 6.8 透明度与可观测性

ccteam 长跑场景下"看不到 AI 在干什么"是首要担忧。三层透明度：

- **给人看**：`ccteam show <slug>` 实时聚合 `~/.claude/jobs/<job_id>/state.json::current_step` + cost；web SPA 同源。meta / Codex tmux 用 `ccteam internal attach <slug>` 直接看完整 claude 交互界面。
- **给程序看**：`~/.ccteam/progress/<slug>.jsonl` 7 类业务事件（格式见 §5）—— orchestrator 用 inotify 监听末尾，**这是唯一的状态事实来源**。
- **一屏看全**：web SPA WorkflowView（详见 §3.8）—— agent cards + 4 panels + SSE 实时更新。

#### Stall 检测（软告警，不强制 kill）

orchestrator 监听 `agent_spawn` 后无 `agent_done` 的时间戳差：< 5 min 视为正常（推理 / 长 Bash / 网络）；5–15 min 软告警；15–30 min 标记 `suspicious`；> 30 min escalation。**永远不主动 kill**——除非命中物理上限（F84 budget cap 或全项目 cost > $200）。**相信长跑、相信用户能 attach（meta/codex）或看 web**。

#### 成本观测

详见 §6.12 cost telemetry + §3.4 并发模型。F84 `max_cost_usd_per_24h` 是 per-workflow 硬上限；CLAUDE.md §三 $200 全 ccteam 物理上限。

#### Daemon 健康监督（M0.23.1)

orchestrator 是**所有 agent spawn + inbox 派送**的单点 — daemon 死了用户写的 inbox / 调的 MCP 命令会**沉默成功**（写到磁盘但永远不被消费）。M0.23.1 给一条 fail-loud 路径：

| 文件 | 谁写 | 谁读 | 语义 |
|---|---|---|---|
| `~/.ccteam/state/orchestrator.pid` | daemon 启动时写 | `ccteam stop` | PID |
| `~/.ccteam/state/orchestrator.heartbeat` | daemon 每 30s 写 | MCP 入口 / meta-agent skill | mtime 是 liveness 唯一来源 |

**判定**：`now - mtime ≤ 60s` → healthy；否则 stale（grace 是 2× heartbeat 间隔，容忍单次 GC pause / 阻塞 IO）。文件不存在 → no_heartbeat（daemon 未启动）。

**消费规则**（action vs read-only 二分）：

- **action 工具**（`pause`/`resume`/`send_to_session`/`inject_decision`/`spawn_agent`）— daemon 不健康直接返回 error，**绝不**写出 inbox 文件就成功（否则用户以为消息派出去了实际烂在磁盘）。
- **read-only 工具**（`ls`/`show`/`peek`/`progress`/`observe_agents`）— state.json 在磁盘，daemon 死也能查；`ls` 在响应里附 `orchestrator.daemon_health` 字段（`status`/`age_secs`/`message`），meta-agent 自看自决定要不要提示用户。

**红线**：health check **只 stat heartbeat 文件**，不做任何 RPC / kill -0 / tmux capture-pane。pure stat 才能放在每个 MCP 调用的 hot path。daemon 启动时立即 touch 一次心跳文件（不等 30s），所以"刚起来的 daemon"也立刻可观察。

### 6.9 ~~Team factory~~（V0.5.0 F100 移除）

V0.2 M0.22 引入的 `ccteam team init/publish` 工厂 + `ccteam-team-author` skill +
`teams/dev|research|research-academic/` 模板,在 V0.4.0 删 phase 模型后已基本失效。
V0.5.0 F100 删除整套:`crates/ccteam-core/src/team_factory.rs` /
`crates/ccteam-cli/src/team_factory_cli.rs` / `skills/ccteam-team-author/` /
`skills/ccteam-project-creator/`(合并进 `ccteam-creator`)/ `teams/dev|research|research-academic/`。

替代:
- **创建新项目** → `skills/ccteam-creator/` step 1/2/3/4 对话(吸收原 `ccteam-project-creator`)
- **在当前 session 起 agent team** → `/ccteam-team` skill(V0.5.0 F93a)
- **自定义 workflow / agent / project-local skill** → `skills/ccteam-creator/` 同一向导

实施位置见 `crates/ccteam-core/src/skill.rs` + `crates/ccteam-cli/src/commands.rs` 的
`render_install_skill_report`。

### 6.10 Multi-session per project（Codex adapter / 未来 fan-out）

V0.4.6 时点 Codex CLI adapter 走 multi-session 路径：用户显式 `ccteam internal session add <slug> --harness=codex` 创建 `<project>/.ccteam/sessions/<sid>/`，master `state.json::sessions` 注册，tmux 名 `ccteam-<slug>-<sid>`，progress 走 `~/.ccteam/progress/<slug>/<sid>.jsonl`。

**未来 fan-out**（未 ship）：plan agent 输出"≥3 个独立子模块且接口稳定"的子模块清单 → master 起 N 个 sub-workflow（每个跑自家 workflow.yaml）→ 全部 review 通过后由 master review phase 验证 cross-module contracts。当前依赖 workflow.yaml 条件分支（V0.5 候选）。

### 6.11 Graceful shutdown（F86）

V0.4.5 之前 daemon 收 SIGTERM 时直接 `JoinSet::abort_all()`，所有 event_loop 硬中断 → in-flight session 不写 `workflow_done`，下次启动靠 F80 phantom cleanup 补 synthetic agent_done。F80 是症状缓解，F86 才根治。

**机制**：

- **`Orchestrator::shutdown_token: Arc<Notify>`** — daemon 主循环 `select!` 中 arm 一条 `shutdown_token.notified()`
- **`ccteam stop`** 不再 `kill PID` + poll pidfile，改：
  - 写 `/tmp/ccteam-<user>.shutdown` trigger 文件 → daemon 文件 watcher 收到 → trigger Notify
  - daemon 主循环 arm 触发 → cancel 所有 event_loop（用 F82 cancellation token，workflow_done reason="shutdown"） → JoinSet `join_all()` 等所有 task 真正退出
  - **timeout 30s 后才走 abort_all() fallback**（防卡死 event_loop 永远不返回）
- **SIGTERM/SIGINT 兼容**：linux signal handler 也 trigger shutdown_token（双触发兼容 systemd / docker stop）

**红线**：cancel token + Notify 用 `tokio::sync` 原语，不引入新依赖；`abort_all()` 仍作 30s timeout fallback 保留，不删 — 防 event_loop 自身卡死 await。

**与 F80 phantom cleanup 关系**：F86 让 graceful 路径下 `workflow_done` 完整写入，F80 cleanup 在异常路径（panic / OOM kill / `-9`）仍作兜底。两层互不冲突。

### 6.12 Cost telemetry（F91 + F92 known gap）

V0.4.5 cost 三个数据源：
1. `state.cost_used_usd`（per project，在 `~/.ccteam/projects/<slug>/state.json` 里）— 由 `Hook::CostAccumulate` 接收 stdin parse claude 输出累加
2. `progress.jsonl::agent_done.cost_usd`（per session，F66 hook 写）
3. `claude_job::probe_job` 读 `~/.claude/jobs/<id>/state.json::cost_usd_total`（F80 加）

任一 hook miss → ccteam 端 cost 漂。**真实来源就是 Claude 自己写的 state.json**，ccteam 不该自己再算一份。

**F91 收敛**：

- **删 cost 累加路径**：`Hook::CostAccumulate` enum branch + `ccteam_hooks::cost_accumulate` 函数 + `ccteam doctor --install-hooks` 模板里的 `cost-accumulate` hook 全删；`doctor --update-hooks` 同步清现有项目 settings.json
- **`state.cost_used_usd` 字段保留 serde compat**：`#[serde(default)]` 接受老文件，写入路径不再 mutate，读取路径（`workflow_summary` / `ccteam show`）改用：
  ```rust
  pub struct CostSummary {
      pub cost_24h_usd: f64,      // sum progress.jsonl::agent_done.cost_usd within 24h
      pub cost_active_usd: f64,    // sum live ~/.claude/jobs/<active>/state.json::cost_usd_total
      pub cost_total_usd: f64,
  }
  ```
- **F84 budget cap** 用 `cost_24h_usd` 判定；**F90 Cost sparkline** 用同源数据

**已知 gap（F92 候选）**：V0.4.6 仍有 `agent_done.cost_usd` 字段需要 hook 写 — 真实数据其实在 `~/.claude/jobs/<id>/linkScanPath` 下的 jsonl 里。F92 候选打算直接从那读，完全摆脱 hook 依赖。当前 V0.4.6 在 hook miss 场景下 `cost_24h_usd` 仍可能漂（已知 limitation，记录于 `docs/versions/v0-4-6/prd.md F91 验收 #3`）。

### 6.13 Plan-approval ↔ IM outbox round-trip(V0.6.1 F98 + F124)

V0.5 长跑 workflow 写 plan 时 user 无 IM 通道审批,夜里改方向无落点;V0.6.1 F98 + F124 闭环。两 finding 协作:**F124** 拥有 `WorkflowMode::HumanApproval` 第 4 mode + orchestrator dispatch arm,**F98** 拥有 per-agent `plan_approval:` block + state machine + IM round-trip。两者独立可叠加(mode 4 用于 workflow-level step gate,plan_approval 用于 agent-level plan gate;两者共享 `plan_pending` / `plan_decision` / `plan_timeout` 三 progress event)。

**Engine 解耦**:`crates/ccteam-core/src/plan_approval.rs`(710 行)是 pure state machine,跟 orchestrator 完全解耦;无 IO,无 LLM 调用。orchestrator 在主 loop tick 调 `engine.step(now, events)` → 返回需要写出的 actions(emit event / 写 decision file)。

**完整 flow**:

```
agent                       orchestrator                 ccteam-imd                 user IM
  │                              │                          │                          │
  │ write plan.md                │                          │                          │
  ├─────────────────────────────►│                          │                          │
  │                              │  artifact watcher fires  │                          │
  │                              │  emit plan_pending event │                          │
  │                              ├─────────────────────────►│                          │
  │                              │  park pending-spawn (mode=human-approval)            │
  │                              │  agent enters paused                                  │
  │                              │                          │  resolve outbox channel  │
  │                              │                          │  send IM message:        │
  │                              │                          │  "[<wf>] <agent> wrote   │
  │                              │                          │   plan: ...\n\nReply     │
  │                              │                          │   APPROVE / REJECT       │
  │                              │                          │   [<reason>] / EDIT      │
  │                              │                          │   <comment> in 60min"    │
  │                              │                          ├─────────────────────────►│
  │                              │                          │                          │
  │                              │                          │◄─── "APPROVE" reply ─────┤
  │                              │                          │  inbound parse           │
  │                              │  emit plan_decision      │                          │
  │                              │◄─────────────────────────┤                          │
  │                              │  write decision file:    │                          │
  │                              │  .ccteam/plan-decisions/ │                          │
  │                              │    <plan_id>.md          │                          │
  │ inbox-style read decision    │                          │                          │
  │◄─────────────────────────────┤  resume agent            │                          │
```

**Decision grammar**(IM-side parser,case-insensitive,trim 后正则):
- `APPROVE` → `{decision: approve}`
- `REJECT` / `REJECT <reason>` → `{decision: reject, comment?: <reason>}`
- `EDIT <comment>` → `{decision: edit, comment: <comment>}`

**Timeout 策略**(`PlanApprovalSpec::on_timeout`):
- `escalate`(默认)— emit `plan_timeout` + 推 meta-agent inbox + agent 持续 paused
- `auto-approve` — 自动 inject APPROVE decision + resume
- `reject` — 自动 inject REJECT decision(reason: timeout)+ resume

**红线**:
- `progress.jsonl` 是 SoT(`plan_pending` / `plan_decision` / `plan_timeout`);不在 state.json / TUI pane / SSE 加 hidden state
- decision 走 **文件**(`.ccteam/plan-decisions/<plan_id>.md`),agent 按标准 inbox-style read 取 — **不**向 stdin / tmux pane 注入 prompt(R3 no prompt injection)
- engine 是 pure state machine — 不在内部 spawn IM call;orchestrator 持有 ccteam-imd outbox handle,看 engine output actions 翻 IM payload
- `mode: human-approval` 与 `plan_approval:` 可独立使用(mode 4 是 workflow-level step gate;`plan_approval:` 是 agent-level plan gate);两者叠加 = workflow 每 step 都要 approve + agent 写 plan 时再 approve(critical migration scenario)

**Tests**:`crates/ccteam-core/tests/plan_approval_test.rs`(434 行,9 tests)— schema round-trip / parser / APPROVE happy path with progress.jsonl ordering / REJECT-with-reason / 3 timeout modes / unknown-plan no-op / idempotent re-decide。

### 6.14 Admin actions:change-persona + add-tool(V0.6.1 F128)

user-manual.md §2.4 长写 `/ccteam-control change-persona <bot> "..."` + `add-tool <bot> "..."` 但 V0.6.0 ship 时无实现。F128 闭这个 drift。

**架构选择**:
- daemon-side **只做文件 mutation**(`<project>/.claude/agents/<bot>.md` rewrite / `workflow.yaml::agents[bot].tools` append)— 不调 LLM
- skill-side(`ccteam-control` SKILL.md)做 **NL → markdown** merge prompt(用户的 client-side Claude LLM 解读 NL,合成新 persona markdown 后传 MCP)
- 这种分工避免 daemon 进程内的 LLM 调用(R3 + R4:fresh context per spawn + no prompt injection)

**MCP 工具**(`crates/ccteam-cli/src/mcp_admin_tools.rs`):
- `mcp__ccteam__admin_change_persona { slug, bot, new_persona_md }` — 读 `.claude/agents/<bot>.md`,替换 body(保留 frontmatter `name` / `tools` / `model`),写回,emit `persona_changed`
- `mcp__ccteam__admin_add_tool { slug, bot, tool_description }` — 读 `workflow.yaml`,parse `agents[bot].tools:` list,去重 append,写回,emit `tool_added`

**生效路径**:bot 下次 turn 起 spawn 时 Claude Code 读新 `.claude/agents/<bot>.md`;workflow.yaml 走 F82 热加载(daemon inotify watch workflow.yaml → diff → 无 cold-reload-required 字段变化 = hot reload)。

**测试**:`crates/ccteam-core/tests/admin_change_persona_test.rs` + `admin_add_tool_test.rs`(共 295 行)— mock skill output,verify file diff + event shape。

### 6.15 IM NL admin via meta-agent(V0.6.1 F129)

user-manual.md §3.2 写 IM 群内 `@ccteam pause <bot>` / `cost today` / `list bots` / `stop everything` 等 NL admin,但 V0.6 ccteam-imd inbound router 只识别 `@<bot>` route to bot,不识别 `@ccteam` mention。F129 加 `@ccteam` mention 路径。

**位置**:`crates/ccteam-imd/src/{inbound, nl_admin}.rs`。inbound 检测 `@ccteam <NL>` mention pattern,在 `@<bot>` route **之前**(避免 `@ccteam` 误 fallthrough 到 bot)。

**5 keyword admin action**(simple keyword match;复杂的留 `Task(subagent_type=ccteam-control)` 路径):

| NL pattern | MCP tool | 危险动作 |
|---|---|---|
| `pause <slug>` | `mcp__ccteam__workflow_pause` | no |
| `resume <slug>` | `mcp__ccteam__workflow_resume` | no |
| `list bots` / `ls` | `mcp__ccteam__workflow_show` + bot 状态 aggregate | no |
| `cost today` / `cost <slug>` | cost summary aggregate | no |
| `stop everything` / `kill all` | admin_stop_all | **yes — 2 步 confirm flow**(回 "Are you sure? Reply CONFIRM";only 二次 CONFIRM 才执行)|

**hop_limit 不消耗**:meta-agent admin path 不走 AgentPath 层次树(不算 bot-to-bot hop)。

**Tests**:`crates/ccteam-imd/tests/im_nl_admin_test.rs`(367 行)— mock TG inbound w/ 5 NL admin path + 危险 confirm flow。

### 6.16 运维健壮性(V0.6.5 F163 + F164)

V0.6.4 nas-box005 真生产部署 surfaces 两个长跑 daemon 用 blocker — daemon 不响应任何 graceful shutdown 信号(只能 SIGKILL,丢 in-memory state、留孤儿 pidfile);`claude-tui::start_thread` 看到已存在的 ccteam tmux session 直接报错而非 reattach(daemon 重启周期里 bot 永久失能,必须人工 `tmux kill-session`)。F163 + F164 在 V0.6.5 把这两个洞合上。

**F163 — `ccteam start` graceful drain**:

实际 PRD 写"加 SIGINT/SIGTERM 处理",写代码时发现 `wait_for_shutdown_signal()` 已经存在 —— 真 blocker 是 daemon 主循环退出后 `web_handle.await` / `imd_handle.await` 无界等(axum / IMD long-poll 不主动 wake 当 shutdown channel 触发)→ 进程挂死。
- 修法:`TASK_DRAIN_TIMEOUT = Duration::from_secs(5)` 套在两个 await 点;timeout branch 走 WARN log + 继续 pidfile cleanup + port 释放
- **不 kill tmux**:`tracing::info!("tmux sessions left running intentionally")` 加进 shutdown 路径(CLAUDE.md §三 "永不主动 kill 长 session" 守);tmux session 是 user-owned 资源,daemon stop ≠ bot session stop;F164 reattach 下次 daemon 启动自动接管
- 自动化 test:`crates/ccteam-cli/tests/graceful_shutdown_test.rs` 4 cases(SIGTERM / SIGINT / `/tmp/ccteam-<user>.shutdown` 触发文件 / tmux 存活验证)
- 行为契约:`docs/interfaces.md §CLI lifecycle` 新增 `stop` 行(5s drain timeout / pidfile unlink / port 立即释放 / tmux 不 kill)

**F164 — `claude_tui::start_thread` reattach 已存在 tmux session**:

3-path 决策:
- (a) session 已存在 + pane comm 含 `claude` → reattach(不开新进程,更新 hooks + 返 existing handle)
- (b) session 已存在 + pane 已死 → `tmux kill-session` then new session
- (c) session 不存在 → new session

健康检查走 `is_pane_running_claude(session) -> bool` helper,用 `ps -o comm=` 拿 pane pid 的 process name —— **不读 pane 内容**(CLAUDE.md §三 "不解析 tmux 终端输出" 守)。Hijack 风险:user 手工 `tmux new-session -s ccteam-chat-foo-bar` 然后跑 claude 会被 ccteam 当作已注册 bot adopt;接受这个 trade-off(documented in `claude_tui.rs` comment)—— ccteam-managed session 名按 `ccteam-chat-<slug>-<role>` 约定,hijack 需用户主动作意。

不动 `resume_thread`(那条路径正常 stop+start 周期已经良性)。`TmuxSession::list_pane_pids()` 加 helper via `tmux list-panes -F "#{pane_pid}"`。Tests:`crates/ccteam-core/tests/claude_tui_reattach_test.rs` 6 cases(alive / dead / fresh / hijack-doc 等)。

---

## 7. 里程碑路线图

历史 milestone（V0.1 + V0.2）。每个版本的具体任务详情在该版本的 dev-plan
文档，本节仅一句话索引：

| 里程碑 | 主目标 | 状态 | 详情 |
|---|---|---|---|
| **M0** | 单项目 CLI MVP | 已 ship | [docs/versions/v0-1/development-plan.md](./v0-1/development-plan.md) |
| **M0.5** | 工具表面 | 已 ship | 同上 |
| **M1** | meta-agent + decisions queue + inbox/outbox | 已 ship | 同上 |
| **M2** | sub-skill auto-trigger + ccteam-mcp 9 tools | 已 ship | 同上 |
| **M2.3** | golden_rules executor（L1 强化） | 已 ship | 同上 |
| **M3** | team abstraction + product-research team | 已 ship | 同上 |
| **M4.1-M4.4** | 跨项目记忆（官方 rules + auto-memory + 可选 claude-mem） | 已 ship | 同上 |
| **V0.2** | 8 milestone（plugin pipeline / team factory / watchdog 等） | 已 ship | [docs/versions/v0-2/dev-plan.md](./v0-2/dev-plan.md) |
| **V0.3 / V0.3.1 / V0.3.2** | web SPA + flex multi-session + htmx 退役 | 已 ship | `docs/versions/v0-3*/` |
| **V0.4.0** | workflow.yaml + ArtifactWatcher + 17 MCP + thin orchestrator + SPA WorkflowView | 已 ship | [docs/versions/v0-4-0/prd.md](./v0-4-0/prd.md) |
| **V0.4.1-V0.4.5** | UX 简化 + unified install + slug grammar + walk-up + F80 phantom cleanup | 已 ship | 各版本 `docs/versions/v0-4-x/` |
| **V0.4.6** | F81 remove / F82 enabled+hot-reload / F83 .ccteam/ / F84 budget / F86 graceful / F89 CLI / F90 panels / F91 cost | 已 ship | [docs/versions/v0-4-6/](./v0-4-6/) |
| **M4.5-M4.6** | 多 audit 投票 + anti-leniency | 未 ship | (未规划到具体版本) |
| **V0.4.7+** | F92 cost from jobs/linkScanPath / workflow.yaml 条件分支 / fan-out multi-session | 未 ship | V0.5 候选 |
| **V0.6.3** | F140 真 cron(`Trigger::Schedule` 接 5 段 cron + skip-missed) / F141 webhook ingress / F142 vendor-seam forward-compat / F143 squad routing | 已 ship | [docs/versions/v0-6-3/](./versions/v0-6-3/) |

**版本化文档维护**：每发布一个版本，该版本所有规划文档（PRD / dev-plan /
design / retro / userguide）归档到 `docs/v<major>-<minor>-<patch>/`，通过该目录的
README.md 索引；**根目录只保留跨版本 SoT**（本文件 / interfaces / requirements /
dev-coupling-audit / claude-code-* / 战略文档）。当前版本的 in-flight 任务
单列在该版本 dev-plan。

---

## 8. 关键风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| **bg-job 卡死** | agent 永远不写 agent_done | F80 phantom cleanup 启动时补 synthetic agent_done；运行时 stall watchdog 推 meta-agent inbox 软告警；F84 budget cap 兜底 |
| **成本失控** | 一夜烧光 | F84 `max_cost_usd_per_24h` per-workflow + 全 ccteam $200 物理上限兜底；不限 max_turns |
| **用户改 workflow.yaml 时 daemon 卡死老 spec** | 拓扑漂移 | F82 inotify watch + `enabled` 开关 + cancel token 优雅终止老 event_loop |
| **`--dangerously-skip-permissions` 被滥用** | rm -rf 用户文件 | 每项目独立 docker container 或 unshare 命名空间；hook 拦危险 Bash |
| **state.json 损坏** | orchestrator 启动崩溃 | 写入用 `.tmp` + rename 原子操作；启动校验 schema，损坏走 backup |
| **fix-loop 在边缘 case 错收敛** | 看似通过其实有 bug | golden_rules（强制 L1 红线）+ F84 budget cap 防自激励 loop 烧钱；M4.5/M4.6 未 ship 引入投票 + anti-leniency |
| **跨项目记忆污染** | 老项目错误经验影响新项目 | reviewer agent 强制标注成功 / 失败；召回时按时间衰减（Claude Code 加载机制自带） |
| **Channel 单点（Telegram bot 死）** | 通知不到用户 | 双通道（telegram + 邮件 + 文件 fallback） |
| **Claude Code 协议变更** | hook 字段或 CLI flag 失效 | 用 `claude --version` 校验；锁定测试过的版本 |
| **用户提的需求过大** | 一个项目跑数天烧大量 token | meta-agent dispatcher 检测"超过 N 个子模块" → 建议拆分 |

---

## 9. 与已有方案的边界

| 方案 | 形态 | 与 ccteam 的关系 |
|---|---|---|
| **gstack** | Claude Code skill 包，需主对话 | ccteam 借鉴其工作流划分，但**不**依赖主对话 |
| **gstack-auto** | Web UI + Conductor 编排 | ccteam 短期对标，**砍掉** Web 和 Conductor，换成守护进程 + git worktree |
| **OpenAI Symphony** | Linear + Codex orchestrator | ccteam 长期对标，**保留** orchestrator 模式，**替换** 执行层为 Claude Code，**新增** workflow.yaml + 跨项目记忆 + critic |
| **ccteam-creator (上游同名项目)** | Claude Code 内的多 agent 编排 skill | 完全不同方向：creator = 人在场协作；ccteam = 人不在场交付 |
| **Claude Code 内建 `/loop`** | ScheduleWakeup 动态模式（同会话）或 CronCreate 模式（Anthropic 云端调度远程 agent） | **不用**——动态模式依赖会话存活，违反痛点 9；CronCreate 模式虽能脱离会话但引入云端调度依赖，与 ccteam「本地优先 + `--dangerously-skip-permissions` 项目沙盒」模型不兼容。ccteam 的循环驱动器永远是本地 Rust orchestrator |
| **Conductor / Worktrees IDE** | 多 session IDE | ccteam 用 git worktree 取代，无需 IDE |

---

## 10. 附录

### 10.1 命令签名 / 文件路径

完整 CLI 命令签名 → **[interfaces.md §10](./interfaces.md#10-cli-命令签名)**；关键文件路径速查 → **[interfaces.md §11](./interfaces.md#11-关键文件路径速查)**。本节不再重复维护。

### 10.2 参考项目

- [garrytan/gstack](https://github.com/garrytan/gstack)——23-skill 工程团队 skill pack
- [loperanger7/gstack-auto](https://github.com/loperanger7/gstack-auto)——phase 流水线 + 评分循环
- [openai/symphony](https://github.com/openai/symphony)——单 orchestrator + tracker-driven 长跑模式
- [jessepwj/CCteam-creator](https://github.com/jessepwj/CCteam-creator)——人在场的 multi-agent 编排（与 ccteam 互补）

### 10.3 关键设计差异速查（vs 三个参考项目）

| 能力 | gstack | gstack-auto | Symphony | ccteam |
|---|---|---|---|---|
| 用户主对话保持开启 | 必须 | 必须（部分时段） | 不需要 | **不需要** |
| 控制平面 | skill 文件 | Web UI + Conductor | Linear | **本地文件系统** |
| 多项目 | Conductor 多 session | Conductor + UI | Linear issues 并行 | **inbox 队列 + git worktree** |
| 任务分解 | 人 | 人 | 人（Linear 已分好） | **meta-agent dispatcher + workflow.yaml** |
| 可行性评估 | 无 | 无 | 无 | **meta-agent clarify + planner agent** |
| Critic / 评分 | 无 | 6 维评分 | PR review | **golden_rules / reviewer agent / M4.5+ 投票（未 ship）** |
| 跨项目学习 | gbrain（可选） | 无 | 无 | **核心差异化（已 ship，M4：官方 rules + auto-memory）** |
| 执行 agent | Claude Code | Claude Code | Codex | **Claude Code（bg-job）+ Codex（tmux adapter）** |
| 长跑能力 | 单 session 限制 | 单 sprint | 周级别 continuation | **bg-job 每次新 1M context + budget cap** |
| 部署形态 | skill 安装 | Docker + Fly.io | Elixir 服务 | **本地守护进程（Rust）** |

---

## 11. 本文档的位置

- `requirements.md` 回答 **为什么做** 与 **谁会用**——已确认。
- 本文档 `tech-design.md` 回答 **怎么做**——架构论证、设计权衡、扩展点选择。当前架构（V0.4.6）。
- [`interfaces.md`](./interfaces.md) 回答 **接口确切长什么样**——YAML schema、JSON shape、文件路径、事件类型、命令签名。
- 各 `docs/v<major>-<minor>-<patch>/` 目录回答 **何时做什么** —— 每个版本的 PRD / dev-plan / retro。

所有实现 PR 必须能映射回:
1. `requirements.md` 的某条痛点
2. 本文档某个组件 / 流程
3. 当前版本 dev-plan 某条任务编号 / F-finding
4. (改协议时) `interfaces.md` 必须同步

无法映射的，先放进 backlog 而非合入主线。
