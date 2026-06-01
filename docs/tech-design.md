# ccteam 技术实现方案

> 本文档基于 [requirements.md](./requirements.md)（已确认的用户痛点）给出 ccteam 当前的技术架构、组件分解、数据协议、扩展点映射。
>
> **产品定位**：「云 CC/Codex + IM」—— 把一个常驻 gateway daemon 架在你机器上的真实 Claude Code / Codex 之上,让你在 IM(Telegram 等)里像用一台终端一样,跨多个项目、多个 session 操作真实 agent。

---

## 0. 架构红线

源 `docs/tech-design.md` + CLAUDE.md §三。任何 PR 不得违反。红线按「模式 × vendor」双轴 scope 表述;**当前运行态只有模式 3(chat / gateway)落地**,模式 1(in-proc)/ 模式 2(bg)属于推后的 `ccteam-flow` 编排层(详 §7),红线仍按三模式列出以保证编排层落地时不退基线。

| 红线 | 模式 1 in-proc | 模式 2 bg(Claude / Codex)| 模式 3 chat(Claude / Codex)|
|---|---|---|---|
| R1 文件系统是控制平面 | — | 守(artifact 双 vendor) | 守 — Claude: tmux 长 session + transcript jsonl byte-offset 增量读;Codex: app-server UDS;两 vendor 共写 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl` |
| R2 `progress.jsonl` 唯一 state SoT | — | 守(双 vendor) | 业务事件 SoT 守(7 类 + `chat_session_reset` / `turn_done`);对话原文走 `turns.jsonl` |
| R3 No prompt injection | 守 | 守 | 守 — agent 行为住 `.claude/agents/<role>.md`,不向 tmux pane 注入 system prompt;`/compact /new /clear` 完全透传 |
| R4 每次 spawn = fresh 1M context | — | Claude: `claude --bg`(无 `--resume`);Codex: trait 决定可复用,用户不见 | 不适用 — chat 复用 context 是 feature |
| R5 永不主动 kill 长 session | 守 | per-vendor `budgets.{claude,codex}.max_cost_usd_per_24h` 触顶 → auto-disable workflow | 守 — `/compact /new` 是合法 turn,非 kill;tmux 长跑 24/7 |
| R6 不解析 tmux 终端输出 | — | 守 | 守 — 读 transcript jsonl + Claude Code 官方 hooks fast event 通道;不 scrape pane(`tmux capture-pane` 仅 dev-time 调试 + screenshot tool 只读) |
| R7 fix-loop 撞 3 次必 escalate | 守 | 守(`fix_counts` map) | 守 + AgentPath depth limit(借 Codex `agent_max_depth` 实现 hop_limit 替代平铺 fix_counts)|
| R8 `ccteam-core` 零 team 名字面量 | 守 | 守 | 守 |
| R9 跨项目记忆走官方接口 | 守 | Claude: `~/.claude/{CLAUDE.md,rules}`;Codex: `~/.codex/AGENTS.md`(`ccteam init` 落 symlink)| 同 |
| R10 新建项目走 `<projects_root>/<team>-<slug>/` | — | 守(`pick_unused_slug` 强制 team 前缀)| per-bot tmux session = `<project>/<bot>`;IM bot 落 `.ccteam/chat/<bot>/` |
| R11 HITL approval state SoT | — | progress.jsonl::plan_decision | 同 |

**vendor 红线**:ccteam **不 vendor** Claude / Codex 二进制(`references/{claude-code,codex/codex-rs}/` git-ignore 不入库,仅协议参考;实际 spawn 走 `$PATH` 内 `claude` / `codex` binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override);`vendor: AgentVendor::{Claude, Codex}` enum 是 trait 一等公民,无 default。

**HITL 红线**:当前 IM 路径里的 agent 走 `--dangerously-skip-permissions`,**无批准门**;`ApprovalIR` 是中立的类型占位,留给未来手机批准能力,当前不产生也不消费 approval。

---

## 1. 设计原则

每条原则都直接挂钩 `requirements.md` 中的某条痛点。

| 原则 | 对应痛点 | 落地约束 |
|---|---|---|
| **守护进程化** | 痛点 9:AI 团队需要人来主持 | gateway daemon(`ccteam start`)独立于任何 Claude Code 主对话,systemd / 后台长跑;SIGTERM / `ccteam stop` 写 `/tmp/ccteam-<user>.shutdown` trigger → daemon 收到后优雅 drain(web / IM gateway / MCP socket / hook sink,每任务 ≤ 5s) |
| **文件即状态机** | 痛点 7:进度永远不透明 | 一切状态可从文件系统恢复:`progress.jsonl` 业务事件 SoT + chat 原文 `turns.jsonl` + Claude session jsonl + Codex thread;daemon 重启按持久化 session id 续接,不丢上下文 |
| **一个 chat = 一台终端** | 痛点 9 + 痛点 13 | chat ⇄ project ⇄ session 三层模型(详 §2.1):一个 chat 跨多个 project,`/new` 起多个 session,`@handle` / `/use` / `/cd` 切换;不同 chat 状态隔离 |
| **声明式 agent 行为** | 痛点 12 + 痛点 13 | agent 系统提示 / 工具表面完全在 `.claude/agents/<role>.md`(Claude Code first-class spec),**不含 prompt 注入**;ccteam 只决定「消息路由到哪个 session」 |
| **resume-by-id 会话** | 痛点 8/9 | session 按需 spawn(首条消息触发)+ 按 session id resume + 空闲释放;不常驻吊着(state 落盘,重启后 reconnect),避免影子 SoT |
| **跨项目沉淀** | 痛点 10:每个新项目从零开始 | 复用官方 `~/.claude/CLAUDE.md` + `~/.claude/rules/ccteam-lessons-<team>.md` + per-repo auto-memory + Codex `~/.codex/AGENTS.md`;加载机制自动注入,**ccteam-core 零 memory 检索代码**(§3.5) |
| **零交互沙盒** | 痛点 8:每一步都点允许 | IM 路径的 Claude session 走 `--dangerously-skip-permissions`;只把 bot 暴露给可信 chat(`allowed_chat_ids` allowlist) |
| **失败必须 IM 可见** | 痛点 7 + 痛点 11 | gateway 的原则是失败不能静默挂住:submit/turn 超时、Claude tmux pane 死、Codex app-server 断,都翻成 `gateway error: ...` 回到 IM |
| **预算硬上限** | 痛点 5 + 自激励 loop 防失控 | 成本 per-vendor 记账(`ccteam-cost`);`budgets.{claude,codex}.max_cost_usd_per_24h` per-vendor cap;CLAUDE.md $200 全 ccteam 物理上限兜底 |
| **smart layer 只 translate,不 decide** | 通知层不能改状态 | 通知 / 翻译层只读既有遥测产出 NL;**绝不**写 progress.jsonl、kill session;所有状态变更只能由 gateway + adapter + hooks 走 |

---

## 2. 总体架构

### 2.1 核心模型:chat ⇄ project ⇄ session

ccteam gateway 把 IM 消息路由到真实 Claude/Codex session。核心对象只有三个:

| 对象 | 含义 |
|---|---|
| **chat** | 一个 IM 私聊或群聊。每个 chat 有自己的当前项目、当前 session 和 session 列表。 |
| **project** | 本地一个已 `ccteam init` 的项目目录,用 slug 标识。 |
| **session** | 一个可继续上下文的 agent 会话,属于某个 chat × 某个 project × 某个 vendor × 某个 role。它是一个常驻的 `ThreadHandle`,有自己独立的 context(`/compact` `/clear` 各自独立)。 |

一个 chat 可以同时活多个项目、多个 session;另一个 chat 状态独立,不会串。chat-local 路由命令:

| 命令 | 作用 |
|---|---|
| `/pair <code>` | 将当前 chat 建立为可用入口,并确保默认 session 存在 |
| `/cd <project>` | 当前 chat 切到项目(只切当前 chat,不影响其他 chat) |
| `/new claude <handle>` | 在当前项目创建 Claude tmux session |
| `/new codex <handle>` | 在当前项目创建 Codex app-server session |
| `/use <session-id>` | 当前 chat 切到已有 session |
| `/sessions` / `/projects` | 列当前 chat 的 session / daemon 已知项目 |
| `@handle <text>` | 把这一条路由到指定 session,并把它设为当前 session;不带 `@handle` 时消息发给当前 session |

这些 chat-local 命令由 gateway 路由表(`Gateway::is_gateway_command`)拦截处理;`/compact` / `/review` / `/clear` 这类**不是** gateway 命令,会作为一个普通 turn 透传给当前 session 的 adapter,由 adapter 翻译成 vendor-native 操作。

### 2.2 daemon = IM⇄session 路由网关

无 slug 的 `ccteam start` 是一个常驻 gateway daemon,**不是** tick loop / orchestrator 循环。它在同一个 tokio runtime 内、共享一条 shutdown 信号(Ctrl-C / SIGTERM / `ccteam stop` trigger 文件),启动以下任务:

| 组件 | 位置 / 说明 |
|---|---|
| IM gateway | `ccteam-im::run_daemon_with_shutdown`;Telegram(等)long-poll 入站 + 出站发送;chat⇄project⇄session 路由表 |
| MCP socket | `~/.ccteam/run/mcp.sock` —— daemon-local line-delimited JSON-RPC handler,供 Claude/Codex plugin 调 ccteam 工具 |
| Web server | axum + SSE,默认 `http://127.0.0.1:7331` |
| Hook sink(可选) | `CCTEAM_HOOK_VIA_DAEMON=1` 时 bind `~/.ccteam/run/hook.sock`,让 daemon 成为 `progress.jsonl` 单一 writer,关掉 hook 子进程直写的两-writer race |

**关键约束**:此路径**不构造** `ccteam-flow::Orchestrator`,**不跑** supervisor tick。daemon 退出时**不 kill** tmux session(R5):下次 `ccteam start` 按持久化 session id 重新接管(Claude 重接 tmux TUI;Codex 走 app-server resume);未发送 / 失败的 IM 出站回复保存在 `~/.ccteam/imd/outbound.jsonl`,启动后重放。

### 2.3 执行层两轴:HarnessAdapter × ProcessBackend

执行层正交两轴,组合是 N+M 不是 N×M:

- **`HarnessAdapter`(vendor 怎么驱动)** —— Claude = tmux TUI + `tmux send-keys` + transcript-tail + 官方 PreToolUse / Stop 等 hook;Codex = `codex app-server` JSON-RPC。
- **`ProcessBackend`(进程跑哪)** —— tmux / inproc / remote 等承载位置。tmux pane 操作(capture / resize / pane_pid)**只**住在 `PaneBackend` 子 trait,不在 base trait。

一个 session = `HarnessAdapter(vendor)` × `ProcessBackend(host)`。两个 vendor 都归一成中立的 `CanonicalEvent` + `ApprovalIR`(**不**抄 codex-emulation —— 每个 vendor 用自家原生通道:Claude 走 hook + transcript,Codex 走 JSON-RPC)。

**`HarnessAdapter` trait**(`crates/ccteam-harness/src/adapter.rs`)—— 5 个 async 生命周期方法 + 2 个同步标识方法:

```rust
#[async_trait::async_trait]
pub trait HarnessAdapter: Send + Sync {
    fn name(&self) -> &'static str;                 // "claude-tui" / "codex-exec" / ...
    fn vendor(&self) -> AgentVendor;                // Claude | Codex
    async fn start_thread(&self, spec: &AgentSpecBrief, ctx: &SpawnCtx) -> Result<ThreadHandle, HarnessError>;
    async fn submit_turn(&self, h: &ThreadHandle, input: TurnInput) -> Result<TurnId, HarnessError>;
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError>;
    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError>;
}

pub enum TurnInput {
    UserText(String),            // chat DM / 群消息 / kick prompt
    Artifact(PathBuf),           // 文件系统 artifact / 附件
    SystemDirective(String),     // /compact /new /clear → adapter 翻成 vendor-native op
    Image(PathBuf),              // rich media
    ToolResult { call_id: String, content: serde_json::Value },
}

pub struct ThreadHandle {
    pub vendor: AgentVendor,     // Claude | Codex
    pub mode: ExecutionMode,     // InProc | Bg | Chat
    pub identity: String,        // tmux session 名 / Claude job id / Codex thread id
    pub started_at: DateTime<Utc>,
    pub raw_extras: serde_json::Value,   // vendor-specific bag(tmux_session / pid / ...)
}
```

- **`CanonicalEvent`** 是 `ThreadEvent` 的中立别名(`ThreadStarted` / `TurnStarted` / `TurnCompleted{usage}` / `TurnFailed` / `Item{Started,Updated,Completed}` / `Error`);schema 对齐 Codex `ThreadEvent`,两 vendor 的 emitter 1:1 映射进 gateway。
- **`SystemDirective`** 把 `/compact` `/new` `/clear` 退化为特殊 turn:Claude → slash 透传到 TUI;Codex → JSON-RPC(`/compact`=`thread/compact/start`、`/review`=`review/start`)。统一「all chat is a turn sequence」。
- **resume 语义**:bg 形态每次 spawn 是 fresh 1M context,`resume_thread` 返回 `NotImplemented`(R4);chat 形态 resume-by-id —— Claude 用 deterministic session 名 lossless 续接,Codex 用 thread id app-server resume。
- **`close_thread`** 是唯一杀长 session 的路径,只能由用户主动 `ccteam session rm` 触发,绝不静默。

**Vendor-seam forward-compat**:ccteam 读 sub-harness 吐出的 `state.json` / `codex app-server` JSON-RPC 通知 —— 这些是 vendor 自有 schema,按自家节奏新增字段 / 新 enum 值,ccteam 清不掉也管不了(与「不做历史迁移」红线只管 ccteam 自有 state 不冲突)。`ccteam-harness::warn_unknown_vendor_token(seam, token, detail)` 是 process-wide warn-once helper(`(seam, token)` 作 dedup key,不每个 poll tick 刷屏)。降级策略:未知 Claude job state → 当非终态继续 probe;未知 Codex event / notification → skip + warn(不中断 event stream)。

### 2.4 双 harness 的 vendor 选型

per-adapter best-fit,不强行统一:

| Vendor | Harness | 为什么 | slash 行为 |
|---|---|---|---|
| Claude | tmux TUI session | 全 TUI + 耐久 + send-keys/transcript/hooks 已成熟;`-p --resume` 每 turn 冷启 cache 失效 + slash 不透传是 UX 退化 | `/compact` `/clear` 等按字面 `send-keys -l` 发给 Claude TUI |
| Codex | app-server JSON-RPC | 原生、文档化的控制平面;`/compact` `/review` 映射到 Codex-native RPC | `/compact`→`thread/compact/start`;`/review`→`review/start` |

两种 harness 可以在同一个 chat 并发存在。输出统一成 IM 回复:gateway 先回 `submitted <session> turn <id>`,随后把 assistant / error 事件通过同一条 outbound ledger 发回 IM。

---

## 3. 核心组件

### 3.1 Crate 拓扑

```
ccteam-cli (bin)
  ├── ccteam-im        (IM gateway + 路由 + 出站 ledger)
  ├── ccteam-flow      [推后的编排层 —— 不接进运行中的 gateway daemon,详 §7]
  ├── ccteam-web       (可选 SPA dashboard,axum + SSE)
  ├── ccteam-hooks     (hook dispatch → progress.jsonl)
  ├── ccteam-harness   (执行层:HarnessAdapter × ProcessBackend × PaneBackend)
  ├── ccteam-core      (primitives leaf:paths / state / progress re-export / vendor / ...)
  └── ccteam-cost      (pricing / budget / token usage —— leaf,无 ccteam 依赖)
```

**依赖方向**(权威,以各 crate `Cargo.toml` 为准):

- `ccteam-cost` 是叶子,不依赖任何 ccteam crate。
- `ccteam-harness` 只依赖 `ccteam-cost`。
- `ccteam-core` 依赖 `ccteam-harness` + `ccteam-cost` —— 即 **`core → harness → cost`**(core 在上,cost 在底)。
- `ccteam-im` / `ccteam-flow` / `ccteam-web` / `ccteam-hooks` 依赖 `ccteam-core` + `ccteam-harness` + `ccteam-cost`。
- `ccteam-cli` 是 bin,依赖以上全部。

> 拓扑只能是 `core -> harness -> cost`,**不要**翻成 `harness -> core`。`ccteam-flow` 是推后的编排层,当前未接进运行中的 gateway daemon。

**progress 写入权威**:`ccteam-harness::progress_bridge` 是 `progress.jsonl` 业务事件 schema 的单一权威,`ccteam-core` 只 re-export。mode-3 chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`,**不**依赖 Anthropic 内部 `~/.claude/projects/`。

### 3.2 项目布局与初始化

`ccteam init` 把任意 cwd 变成 ccteam 项目,写入:

```
<project>/
├── src/ tests/ ...                   # 业务代码,永远不动
├── CLAUDE.md                         # 项目级运营手册(可选,§6.2)
├── .claude/
│   └── agents/<role>.md              # agent 行为 SoT(Claude Code 当前读取路径)
└── .ccteam/                          # ccteam 项目状态(gitignored)
    ├── agents/<role>.md              # 中立 ccteam 副本(IM/session onboarding + 未来非-Claude adapter)
    ├── skills/.gitkeep               # 预留项目自有 skill 扩展
    ├── state.json                    # per-project ccteam 元数据
    └── chat/<bot>/turns.jsonl        # chat 原文(R2 mode-3 SoT)
```

`ccteam init --slug <name>` 重跑安全;`--force` 才覆盖 ccteam 生成物。业务代码 / `.git/` / `.env` 永远保留。`progress.jsonl` 业务事件 SoT **不落项目内**,而在全局 `~/.ccteam/progress/<slug>.jsonl`(按 slug 分文件;全局根布局见 §3.1 与 interfaces §1.1)。

### 3.3 Workspace 隔离与并发

- **每项目一个目录**(任意 cwd 经 `ccteam init`;新建走 `~/projects/<team>-<slug>/`,team 前缀让 `~/.claude/rules/ccteam-lessons-<team>.md` 的 `paths:` frontmatter 正确 scope 到该项目,R10)。
- **per-bot tmux session** 名 = `ccteam-chat-<slug>-<role>` deterministic 命名;daemon 重启后用同名 lossless reattach。
- **成本上限**:per-vendor `budgets.{claude,codex}.max_cost_usd_per_24h`(软上限,触顶 auto-disable);CLAUDE.md $200 全 ccteam 物理上限(粗粒度 alert)。
- **为什么用 git worktree 而非 Conductor**:Conductor 要求人在 IDE 里使用;ccteam 用 git worktree + 文件系统隔离取代,更适合无人值守。

### 3.4 三层防御协议(Defense in Depth)

替代「人持续在场审查」,用三层独立机制保证质量与方向不偏(呼应痛点 11):

- **L1 架构约束(deterministic,写死的红线)**:危险命令拦截(`PostToolUse(Bash)` 拦 `git push.*` / `rm -rf /` / deploy)、agent output artifact 校验、`.ccteam/` 之外元数据不许 ccteam 自动改。`golden_rules` executor 基础检查 + 项目特定补充。
- **L2 多 agent 互检(stochastic 但多视角)**:每个 audit agent 输出 `PASS / CONCERN / BLOCK`;全 PASS 自动通过、任意 BLOCK 进 fix-cycle 或转 L3。复用 `claude-plugins-official` 的 `pr-review-toolkit` 等 agent,不重写。
- **L3 用户 fork 决策(last resort)**:仅在 L1 PASS + L2 拍不了板时弹出(**不是** first checkpoint,痛点 11 主路径是 L1+L2)。信任档位 `~/.ccteam/config.yml`:`yolo` / `balanced`(默认)/ `careful`。

顺序约束 L1 → L2 → L3,不并联。L1 兜系统性偏差、L2 兜单 agent 偏差、L3 兜前两层都拍不了板的偏差。

> L2 / L3 的自动议事与投票编排,以及 fix-cycle 的自激励 loop,属于推后的 `ccteam-flow` 编排层(§7);当前运行态由用户在 IM 里手动驱动多个 audit session。

### 3.5 Cross-project Memory(差异化护城河)

主路径完全复用 Claude Code / Codex 官方记忆机制,**检索发生在 agent session 内部,ccteam-core 零 memory 检索代码**:

| 通道 | 路径 | 加载方式 | ccteam 用法 |
|---|---|---|---|
| 项目内累积 | per-repo auto-memory(`/memory`)+ `<project>/CLAUDE.md` | 每 session 启动加载 | reviewer agent prompt 引导 agent 自写 |
| 跨项目共享(Claude) | `~/.claude/rules/ccteam-lessons-<team>.md`(支持 `paths:` frontmatter scope) | 每 session 启动加载,匹配路径才生效 | agent prompt 引导写入 `<!-- ccteam-managed:lessons -->` marked section |
| 跨项目共享(Codex) | `~/.codex/AGENTS.md`(`ccteam init` 落 `AGENTS.md → CLAUDE.md` POSIX symlink) | Codex 加载机制注入 | 同 |

唯一一段 ccteam 代码 = `ccteam doctor --install-memory-bridge`(创建 rules 占位文件 + marked section + `paths:` frontmatter);其余都是 agent prompt 模板里的指引。

**可选增强**:用户装了 [claude-mem](https://docs.claude-mem.ai/usage/search-tools) 则它自带 hook 自动捕获 + 暴露 read-only MCP search 工具;ccteam 不写检测 / 集成代码,**LLM 自看 tool surface 决定调不调**;没装则 100% 走默认路径,功能不受影响。

---

## 4. 关键流程

### 4.1 一条 IM 消息的端到端路径

```
用户在 IM 发 "@reviewer 看一下这个项目的 README,给我三条风险"
   │
   ▼  IM gateway 收入站(经 allowed_chat_ids allowlist 校验)
   │
   ▼  Gateway::handle_text:解析 @reviewer mention → 定位/创建 chat 当前 project 的 reviewer session
   │     (首条消息触发 HarnessAdapter::start_thread;已存在则复用 ThreadHandle)
   │
   ▼  HarnessAdapter::submit_turn(handle, TurnInput::UserText("看一下..."))
   │     Claude: tmux send-keys -l "<text>" Enter
   │
   ▼  gateway 立刻回 IM:"submitted <session> turn <id>"
   │
   ▼  HarnessAdapter::events 流(Claude: hooks fast event + transcript tail;Codex: JSON-RPC)
   │     → CanonicalEvent::ItemCompleted{AgentMessage} / TurnCompleted{usage}
   │     → 镜像写 turns.jsonl(R2)+ progress.jsonl 业务事件 + cost ledger(per-vendor)
   │
   ▼  outbound ledger 把 assistant 文本发回 IM(at-least-once;失败重放)
```

### 4.2 失败与恢复

gateway 的原则:失败必须在 IM 里可见,不能静默挂住。

| 故障 | 用户看到 | 恢复方式 |
|---|---|---|
| agent 启动失败 | `gateway error: ...` | 看 daemon log,确认 `claude` / `codex` 在 `PATH` 且已登录 |
| Claude tmux pane 死 | `gateway error: ...` | `ccteam stop && ccteam start`,再发同一 `@handle` 消息(按 deterministic session 名 reattach;dead pane 则 recreate + `--resume` lossless 续接,失败回退 fresh spawn + emit `chat_session_reset{reason}`) |
| Codex app-server socket 断 | `gateway error: ...` | 确认 `codex app-server` 可用;必要时重启 daemon(app-server resume 接回 thread) |
| turn 超时 | `gateway error: turn timed out ...` | 稍后重试;反复出现则 compact 或新建 session |
| daemon 被 kill | 短暂离线 | 重新 `ccteam start`;session 和 outbound ledger 会恢复 |
| 一个 session context 不可用 | —— | 直接 `/new` 建新 session;旧 session 不影响同 chat 其他 session |

---

## 5. 数据与文件协议

完整字段、JSON schema、文件命名规则、事件类型清单**以代码为准**(见 §12「协议 → 代码位置」指针表)。本节只保留架构约束:

| 子节 | 架构约束 | 代码位置(SoT) |
|---|---|---|
| 全局目录布局 | `~/.ccteam/` 是单一根;`run/mcp.sock`、`imd/outbound.jsonl`、`im/credentials.json`、`web-token`、`ccteam.pid` 都在其下 | `ccteam-core::paths`(`CcteamPaths`) |
| 项目级 state.json | 原子写(`.tmp` + rename);per-project 元数据;损坏走 backup | `ccteam-core`(`ProjectState`) |
| progress.jsonl | **唯一业务状态事实来源** —— 写 schema 单一权威 = `ccteam-harness::progress_bridge`,core re-export;tmux 终端输出不参与状态判定 | `ccteam-harness::progress_bridge` / `enriched_event::EventKind` |
| turns.jsonl | chat 对话原文 SoT,ccteam-owned `<project>/.ccteam/chat/<bot>/`;**不**依赖 Anthropic 内部目录长期可读 | `ccteam-im`(turns mirror) |
| outbound ledger | `~/.ccteam/imd/outbound.jsonl`;at-least-once IM 投递,daemon 启动后重放未发送 / 失败行 | — |

**关键论证**:「progress.jsonl 唯一事实来源」是架构红线。曾考虑过解析 tmux `capture-pane` 输出做状态判断 —— 拒,因为终端文本格式不稳定、ANSI 转义难、对 prompt cache 表现敏感。所有状态转移走 hook + transcript jsonl + app-server event,deterministic 且可重放。

---

## 6. Claude Code / Codex 扩展点映射

### 6.1 Tmux 长 session(Claude mode-3 bot)

mode-3 = per-bot 一个 tmux session + `claude` TUI 24/7 长跑;dual-track 观测,**不 scrape pane**(R6)。

```bash
SESSION="ccteam-chat-<slug>-<role>"      # tmux session deterministic 命名
# fresh spawn:claude --name 让 Anthropic 把 session jsonl 落在 deterministic
# 名下,使日后 recreate 能 --resume(argv 顺序以 claude_tui.rs 为准)
tmux new-session -d -s "${SESSION}" -c "${PROJECT_DIR}" \
  "claude --dangerously-skip-permissions --name <session-id-name>"
# 输入面:tmux send-keys -l 直送 user content + Enter(literal 模式,0 escape 雷区);
#         /compact /new /clear 透明透传(ccteam 不主动调也不过滤)
# 输出面 Track A:Claude Code 官方 hooks(UserPromptSubmit / Stop / SubagentStop /
#                 SessionStart / PostToolUse)作 fast event 通道(低延迟 turn boundary)
# 输出面 Track B:byte-offset 增量读 transcript jsonl → 抽 full message content →
#                 镜像写 ccteam-owned turns.jsonl(R2 SoT)
```

**关键约束**:
- ✅ 用 `--dangerously-skip-permissions`(消灭弹窗,痛点 8)
- ❌ **不**用 `claude -p`(失去 attach / 介入能力)
- ❌ **不**设 `--max-turns`(用户要求长跑,由超时 + 成本上限兜底)

**daemon-restart 上下文恢复**:dead-pane recreate 路径改用 `claude --dangerously-skip-permissions --resume <session-id-name>` lossless lookup —— Anthropic 自己 reload session jsonl,模型 cache + 推理链续接;`--resume` 失败才 fallback fresh spawn + emit `chat_session_reset{bot, reason}`(user-visible degraded,不冒充 resume)。借而不替:`progress.jsonl` SoT 零扩(只增 reason 字段),不向 pane 注入 system prompt,不 `capture-pane`。

tmux 命令包在 `tokio::process::Command` 异步 spawn —— 单 binary 零额外运行时依赖。

### 6.2 项目级 CLAUDE.md

`ccteam init` 可在项目根生成 `<project>/CLAUDE.md`(项目上下文 + 工作约定 + 「不做的事」)。跨项目经验段由 Claude Code 加载机制自动注入 `~/.claude/rules/ccteam-lessons-<team>.md`(匹配 `paths:`)+ per-repo auto-memory,**无需 ccteam 检索**。agent prompt 住 `.claude/agents/<role>.md`,Claude Code 起 session 时按 role 读对应文件:

```markdown
---
name: reviewer
description: Review the project and surface risks
tools: [Read, Grep, Bash]
model: claude-opus-4-5
---

You are a reviewer agent. ...
```

### 6.3 Plugin pipeline

项目 session 启 plugin agent 走 `enabledPlugins` 路径,**不** ln -sf 进 `~/.claude/agents/`。`ccteam init` 写 `<project>/.claude/settings.json` 的 `enabledPlugins: {"<plugin>@<mkt>": true}`;Claude Code session 启动时 in-memory plugin pipeline 加载 enabled plugin,自动加 `<plugin>:` namespace,agent markdown 用裸名 `Task(subagent_type="code-reviewer")` 仍可调。静态映射表 `crates/ccteam-core/src/plugin_resolution.rs`(`KNOWN_PLUGIN_AGENTS`)。

### 6.4 Hooks 配置

完整 `settings.json` 模板、Hook 事件用途**以代码为准**(`ccteam init` 落 settings;hook impl = `ccteam internal hook` 子命令,见 §12 指针表)。本节只保留架构论证:

**为什么 hooks 是可观测性命脉**:Claude Code hooks 是 deterministic 的 —— 同一事件触发同一脚本,这是把「AI 的随机推理」转成「系统可处理的事件流」的桥。ccteam 把工具调用 / turn 边界事件通过 hooks 落到 progress.jsonl + turns.jsonl,**完全不解析 tmux 终端文本**。

**实现形态**:hook 实现是 `ccteam internal hook <name>` 子命令 —— 单 binary 分发,与 daemon 共享同一份 serde schema(progress 事件定义、state.json 字段),不依赖独立 bash / python 运行时。可选地,`CCTEAM_HOOK_VIA_DAEMON=1` 把 hook 事件经 `~/.ccteam/run/hook.sock` 转给 daemon,使 daemon 成为 progress.jsonl 单一 writer,消除两-writer race。

**Hook 写作纪律**:append 类 `async: true` 别拖慢主流程;解析 terminal-state 输出的 hook 设 `timeout`;hook 脚本放 `~/.ccteam/hooks/` 不放项目目录;`Stop` 一个 entry 可挂多 command,但 `decision: block` 只能由单点输出。

**cost 来源**:Claude bg 形态 cost 由 Claude Code 自己写 `~/.claude/jobs/<job_id>/state.json::cost_usd_total`;chat / Codex 形态由 adapter 从 `TurnCompleted{usage}` event 取 `UnifiedTokenUsage`(`ccteam-cost`)按 per-model pricing 估算,写进 per-vendor ledger `<ccteam_root>/cost-budget.json`。`ccteam doctor --check-cost-orphan` 扫近 24h 的 vendor `agent_done` events 对账 ledger,缺失即 WARN。

### 6.5 MCP servers

#### 消费的 MCP(ccteam 不写,只接)

| MCP | 用途 |
|---|---|
| Telegram | 统一走 `ccteam-im` gateway + `openhuman/channels` Rust crate;`claude-plugins-official/telegram` 作 backup transport |
| claude-mem | 跨项目记忆**可选增强**(read-only search / timeline + 自带 hook);ccteam 不写集成代码,LLM 自看 tool surface 决定用不用 |
| Playwright / GitHub | E2E 测试 / PR 管理(优先 `gh` CLI) |

#### 提供的 MCP:`ccteam`(27 工具,0 STUB,5 group 子前缀)

所有工具加 group 子前缀,**server name 不变**(`ccteam`):

| Group(子前缀) | 工具数 | 用途 |
|---|---|---|
| `workflow_` | 15 | 面向项目自动化的底层控制(show / peek / progress / new / pause / resume / spawn_agent / trigger_gate / ...) |
| `chat_` | 6 | register_bot / unregister_bot / list_bots / send_input / history / reset |
| `advise_` | 2 | vote(Claude + Codex 并行 advisor + 第三次 Claude verdict synthesis)/ parallel(N-of-N 原文返回);budget gate `<ccteam_root>/cost-budget.json` |
| `admin_` | 3 | ls / change_persona / add_tool |
| `screenshot` | 1 | 只读终端截图 |

`CCTEAM_DISABLE_TOOLS` 用 group enum(非 glob,防 typo):`CCTEAM_DISABLE_TOOLS=advise,chat` 关掉两组。`STUB_TOOLS: &[&str] = &[]` static const(`crates/ccteam-cli/src/mcp_tool_groups.rs`)是 invariant 守门员;`ccteam doctor --verify-mcp` 自检 stub-counter parity,stub_count > 0 → exit code 1。完整 tool schema 见代码 `mcp_tool_groups.rs` + `ccteam doctor --verify-mcp`(§12 指针表)。

**Wire 协议纪律**:`ccteam internal mcp-serve` stdout 是 line-delimited JSON-RPC frame channel,**所有 tracing / 日志走 stderr**,否则污染 frame parse → MCP client 解析挂。两条 transport(stdio + daemon 的 `~/.ccteam/run/mcp.sock`)共用同一 handler,读写同一份 state.json / progress.jsonl。

#### admin actions:change-persona + add-tool

- daemon-side **只做文件 mutation**:`change_persona` 读 `.claude/agents/<bot>.md` 替换 body(保留 frontmatter)写回 + emit `persona_changed`;`add_tool` 读 `workflow.yaml` parse `agents[bot].tools:` 去重 append + emit `tool_added`。**不调 LLM**。
- skill-side(`ccteam-control` SKILL.md)做 NL → markdown 合成(用户 client-side Claude 解读 NL 后传 MCP)。这种分工避免 daemon 进程内的 LLM 调用(R3 + R4)。
- 生效路径:bot 下次 turn 起 spawn 时读新 `.claude/agents/<bot>.md`。

### 6.6 chat-mode design

mode-3 = tmux 长 session + claude TUI 长跑(per bot)+ dual-track 观测 + ccteam-owned `turns.jsonl` + IM gateway 路由;**bot-to-bot 100% 走 IM group**(no in-process IPC —— IM history = 完整对话链,hop_limit 在 group msg 链上数)。

**输入面**(`HarnessAdapter::submit_turn`):
- `TurnInput::UserText(s)` → `tmux send-keys -l "$s" Enter`(literal 模式)
- `TurnInput::SystemDirective("compact"|"new"|"clear")` → `send-keys -l "/compact" Enter` 透传(通过 SessionStart hook 观察副作用 emit `chat_session_reset`)
- `TurnInput::Image(path)` / `Artifact(path)` → 写 `<bot>/attachments/<ts>` + send-keys `Read $path`

**输出面**(dual-track,`HarnessAdapter::events` 合流):Track A hooks fast event(低延迟 turn boundary)+ Track B byte-offset 增量读 transcript jsonl 镜像写 turns.jsonl(R2 SoT)。

**lifecycle**:compact 阈值由 adapter 内部计数(用户不感知);session reset 重建借 `claude --resume <name>` lossless,失败回退 fresh spawn + 从 turns.jsonl tail 重建 context + emit `chat_session_reset`。

**bot-to-bot @ routing**:IM group 内 `@<bot_name> <msg>` → gateway 解析 → 查 chat ACL allowlist → submit_turn 到对应 bot session;`hop_limit` 借 Codex `AgentPath` 层次树(同条 IM msg chain 上数,**不**在 in-process 计)。

**红线对齐**:R1(send-keys + turns.jsonl)/ R2(progress.jsonl 业务事件 + turns.jsonl 原文)/ R3(`/compact` 等透传,不注入 system prompt)/ R5(`/compact /new` 是合法 turn)/ R6(读 transcript + hooks,不 scrape pane)/ R7(AgentPath depth limit)全守。

### 6.7 安全

- **skip permissions**:IM 路径的 Claude session 用 `--dangerously-skip-permissions`,没有手机批准门;agent 按本机 Claude Code 权限直接执行允许范围内操作。这是 YOLO 模式,只把 bot 暴露给可信 chat。
- **Telegram allowlist**:`~/.ccteam/im/credentials.json` 的 `allowed_chat_ids` 是第一层边界,生产不留空;bot token 不进 git;daemon 只跑在你控制的机器上;Web UI 只绑 `127.0.0.1` 除非明确配反代 + 鉴权。

### 6.8 Web 仪表盘

`crates/ccteam-web/` 是 Vite + React SPA(`build.rs` 在 `cargo build` 时跑 `npm run build`,`CCTEAM_SKIP_WEB_BUILD=1` 跳过);backend axum + SSE,服务 SPA bundle + JSON API + SSE。

**Authentication**:loopback 免 token;非 loopback 自动生成 `~/.ccteam/web-token`(mode 0600)+ LAN-RCE 倒计时;URL shim `?token=ccteam:<hex>` → HttpOnly cookie + 303 干净 URL。

**架构红线**:progress.jsonl 仍是 SoT;web 不解析 tmux 终端(SSE watcher 仅读 progress.jsonl);web 不 kill 长 session;web 不写跨项目记忆;web 写控制走跟 IM channel 完全相同的 gateway dispatch 路径;`cargo tree -p ccteam-web | grep ccteam-cli` 必须 0 命中(独立 dep graph 红线由 `tests/dep_graph_test.rs` 锁)。

#### 前端层 invariant(红线)

ccteam 核心是 **headless 状态引擎** —— 所有 UI 都是可插拔前端,共用 lib API。任何前端**不得**在 ccteam 内引入新 LLM 层:web 通过 SSE 观测 + 写控制等价于「远程版 NL 派单」,用户键入 = 写控制文件,不经任何 ccteam 中介 LLM。LLM 推理只发生在 agent session(Claude/Codex)内部。

### 6.9 透明度与可观测性

| 你要看 | 命令 / 文件 |
|---|---|
| daemon 是否活着 | `ccteam status` |
| 安装和依赖 | `ccteam doctor` / `ccteam doctor --verify-mcp` |
| 最近 daemon 日志 | `tail -120 /tmp/ccteam.log` |
| outbound ledger | `~/.ccteam/imd/outbound.jsonl` |
| gateway session state | `~/.ccteam/im/gateway-state.json` |
| 项目状态 / 业务进度 | `<project>/.ccteam/state.json` / `<project>/.ccteam/progress.jsonl` |
| 一屏看全 | web SPA(SSE 实时) |

**Stall 检测(软告警,不强制 kill)**:`agent_spawn` 后无对应完成事件的时间差超阈值 → 软告警。**永远不主动 kill** —— 除非命中物理上限(per-vendor cap 或全项目 $200)。相信长跑、相信用户能介入或看 web。

---

## 7. 推后:ccteam-flow 编排层(非当前运行态)

> 以下是**推后**的自动编排能力,住在独立 crate `ccteam-flow`,**当前未接进运行中的 gateway daemon**。这里记录其设计与红线,供编排层落地时不退基线;**不要**把它当成当前 daemon 的运行方式。当前运行态由用户在 IM 里手动驱动多个 session(§2)。

### 7.1 模型:文件系统驱动的 thin orchestrator

编排层是一个文件系统驱动的状态机(`ccteam-flow::Orchestrator` + `ArtifactWatcher` + `WorkflowSpec`):
- **声明式拓扑**:每项目 `<project>/.ccteam/workflow.yaml` 声明 agent 角色 + trigger + 并发上限,**不含任何 prompt**(R3);agent 行为住 `.claude/agents/<role>.md`。
- **trigger 四类**:`manual`(显式 spawn)/ `schedule`(`croner` 5 段 cron + skip-missed,`last_fire` 持久化)/ `gate`(等 MCP 工具释放)/ `watch:<path>`(inotify/fsevents,新文件 → spawn,`parallelism: u32` 上限内并发)。
- **bg-job 形态**:Claude agent 走 `claude --bg --agent <role>`(每次 fresh 1M context,R4);Codex 走 `codex exec --json` / `codex app-server`。完成走 hook 写 `agent_done` + cost。
- **5 类编排模式**(推后的 `ccteam-flow` 编排层):见 `docs/research/orchestration-patterns.md`。
- **重启恢复**:phantom cleanup 扫每个项目 progress.jsonl,`agent_spawn` 无匹配 `agent_done` 且对应 job state.json 已不在 → 补 synthetic `agent_done status="cleanup"`。

### 7.2 `workflow.yaml` schema 速览

```yaml
name: dex-ui-autoloop
enabled: true                            # false 时跳过 roster
mode: artifact-driven                    # artifact-driven(默认)| chat | human-approval | agent-team
budgets:                                 # per-vendor cap(具体 key 见 workflow.yaml 解析代码)
  claude: { max_cost_usd_per_24h: 5.00, max_agent_spawns_per_hour: 100 }
  codex:  { max_cost_usd_per_24h: 5.00 }
agents:
  explorer:
    vendor: claude                       # claude | codex,trait 一等公民,无 default
    trigger: manual
    output: .ccteam/fix-requests/
  fixer:
    trigger: watch:.ccteam/fix-requests/
    parallelism: 3
    input: .ccteam/fix-requests/
    output: .ccteam/done/
  master:
    trigger: gate
    input: .ccteam/done/
```

**红线**:`workflow.yaml` 不许出现 `prompt:` / `system_prompt:` / `messages:` 字段。完整 schema 见 workflow.yaml 解析代码(`ccteam-flow`,推后的编排层)。

### 7.3 Self-healing Fix Loop + escalation

fix 是 workflow 拓扑里的一个 agent role(典型 `fixer` watch `fix-requests/`)。thin orchestrator 维护 per-role `fix_counts`(从 `agent_done.status="errored"` 累加),撞 3 次顶 → 写 `escalation` 事件 + 推用户 inbox 一条 enriched markdown(R7);**不**自动停 workflow(budget cap 才优雅停)。**禁止静默重试** —— 撞 3 次顶绝不静默。

### 7.4 squad 跨 session 路由

workflow.yaml 顶层可选 `squad: { leader, members, hop_limit }` 补「跨 spawn / 跨 session 运行时路由」窄缝:leader 往 `.ccteam/squad/` 写 `<member>--h<N>--<rest>.md`,ArtifactWatcher **解析文件名前缀**(不读正文 → 不开 prompt-injection 面)spawn member;`<N>` 达 `hop_limit` → emit `escalation{kind:"squad_hop_limit"}`(R7)。

### 7.5 Plan-approval / HumanApproval(HITL)

`mode: human-approval`(workflow-level step gate)与 per-agent `plan_approval:` block(agent-level plan gate)独立可叠加,共享 `plan_pending` / `plan_decision` / `plan_timeout` 三 progress 事件。engine(`crates/ccteam-core/src/plan_approval.rs`)是 pure state machine(无 IO、无 LLM);decision 走**文件** `.ccteam/plan-decisions/<plan_id>.md`,agent 按标准 inbox-style read 取 —— **不**注入 prompt(R3)。IM round-trip:agent 写 plan → emit `plan_pending` + park → IM 发审批消息 → 用户回 `APPROVE` / `REJECT [reason]` / `EDIT <comment>` → emit `plan_decision` + 写 decision file + resume。Timeout 策略 `escalate`(默认)/ `auto-approve` / `reject`。

> 注:当前 IM 路径 agent 走 `--dangerously-skip-permissions`,**无批准门**;以上 HITL 编排随 `ccteam-flow` 落地启用,`ApprovalIR` 是当前的类型占位。

---

## 8. 关键风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| **agent session 卡死 / turn 超时** | 用户等不到回复 | submit/turn 超时阈值(`CCTEAM_IM_GATEWAY_{SUBMIT,TURN}_TIMEOUT_MS`)→ `gateway error` 回 IM;stall 软告警;per-vendor cap 兜底 |
| **daemon 死了消息沉默** | inbox / MCP 命令写到磁盘但永不消费 | action 工具在 daemon 不健康时直接返回 error,绝不假装成功;`ccteam status` 看 liveness |
| **成本失控** | 一夜烧光 | per-vendor `max_cost_usd_per_24h` + 全 ccteam $200 物理上限兜底;不限 max_turns;`ccteam doctor --check-cost-orphan` 对账 |
| **`--dangerously-skip-permissions` 被滥用** | rm -rf 用户文件 | `allowed_chat_ids` allowlist + hook 拦危险 Bash + 项目隔离;只暴露给可信 chat |
| **Claude tmux pane 死 / daemon restart 丢 context** | bot 失能 / 上下文断 | deterministic session 名 reattach;dead pane recreate 走 `--resume` lossless,失败 fresh spawn + `chat_session_reset{reason}`(user-visible) |
| **state.json 损坏** | 启动崩溃 | `.tmp` + rename 原子写;启动校验 schema,损坏走 backup |
| **vendor 协议变更** | hook 字段 / CLI flag / RPC 失效 | vendor-seam forward-compat warn-once 降级(skip + warn,不 panic);`claude --version` / `codex --version` 校验 |
| **跨项目记忆污染** | 老项目错误经验影响新项目 | reviewer agent 强制标注成功 / 失败;召回按官方加载机制时间衰减 |
| **Channel 单点(IM bot 死)** | 通知不到用户 | outbound ledger at-least-once + 重放;daemon 重启续接 |

---

## 9. 与已有方案的边界

| 方案 | 形态 | 与 ccteam 的关系 |
|---|---|---|
| **gstack** | Claude Code skill 包,需主对话 | ccteam 借鉴其工作流划分,但**不**依赖主对话 |
| **gstack-auto** | Web UI + Conductor 编排 | ccteam 短期对标,**砍掉** Web 和 Conductor,换成守护进程 + git worktree |
| **OpenAI Symphony** | Linear + Codex orchestrator | ccteam 长期对标(编排层),**替换**执行层为 Claude Code / Codex,**新增** chat⇄project⇄session IM 路由 + 跨项目记忆 + critic |
| **ccteam-creator(上游同名项目)** | Claude Code 内的多 agent 编排 skill | 完全不同方向:creator = 人在场协作;ccteam = 人不在场 / IM 远程驱动 |
| **Claude Code 内建 `/loop`** | 同会话动态模式 / 云端 cron 调度 | **不用** —— 动态模式依赖会话存活(违反痛点 9);云端 cron 引入云端调度依赖,与 ccteam「本地优先 + `--dangerously-skip-permissions` 项目沙盒」模型不兼容 |
| **Conductor / Worktrees IDE** | 多 session IDE | ccteam 用 git worktree 取代,无需 IDE |

---

## 10. 附录

### 10.1 命令签名 / 文件路径

完整 CLI 命令签名 = `ccteam-cli` clap 定义(`ccteam --help`);关键文件路径 = `ccteam-core::paths`。见 §12 指针表。

用户日常 CLI:

```bash
ccteam init [--slug NAME] [--in PATH] [--force]   # 初始化 / 刷新项目
ccteam start [--no-web] [--no-imd] [--no-clipboard]  # 启动 gateway daemon
ccteam stop                                       # 优雅 drain,保留 tmux session
ccteam status                                     # daemon + 项目摘要
ccteam doctor [--verify-mcp|--check-cost-orphan|...]  # 体检 + 维护
ccteam web [--bind 127.0.0.1:7331]                # 单独启动 Web UI
```

### 10.2 参考项目

- [garrytan/gstack](https://github.com/garrytan/gstack) —— 工程团队 skill pack
- [loperanger7/gstack-auto](https://github.com/loperanger7/gstack-auto) —— phase 流水线 + 评分循环
- [openai/symphony](https://github.com/openai/symphony) —— 单 orchestrator + tracker-driven 长跑模式
- [jessepwj/CCteam-creator](https://github.com/jessepwj/CCteam-creator) —— 人在场的 multi-agent 编排(与 ccteam 互补)

### 10.3 关键设计差异速查(vs 三个参考项目)

| 能力 | gstack | gstack-auto | Symphony | ccteam |
|---|---|---|---|---|
| 用户主对话保持开启 | 必须 | 必须(部分时段) | 不需要 | **不需要(IM 远程驱动)** |
| 控制平面 | skill 文件 | Web UI + Conductor | Linear | **本地文件系统 + IM gateway** |
| 多项目 | Conductor 多 session | Conductor + UI | Linear issues 并行 | **一个 chat 跨多项目 + git worktree** |
| 执行 agent | Claude Code | Claude Code | Codex | **Claude Code(tmux TUI)+ Codex(app-server)** |
| 跨项目学习 | gbrain(可选) | 无 | 无 | **核心差异化(官方 rules + auto-memory + AGENTS.md)** |
| 部署形态 | skill 安装 | Docker + Fly.io | Elixir 服务 | **本地 gateway 守护进程(Rust)** |

---

## 11. 本文档的位置

- `requirements.md` 回答 **为什么做** 与 **谁会用**(核心痛点)。
- 本文档 `tech-design.md` 回答 **怎么做** —— 架构论证、设计权衡、扩展点选择,描述当前架构;**协议确切长什么样以代码为准**,见下面 §12。
- `usage.md` 回答 **怎么用**(用户命令手册,纯命令)。

所有实现 PR 必须能映射回:① `requirements.md` 的某条痛点 ② 本文档某个组件 / 流程 ③ 改协议同步代码 + §12 指针表。无法映射的,先放进 backlog 而非合入主线。

---

## 12. 协议 → 代码位置(代码是唯一 SoT)

旧 `interfaces.md` 已退役。协议细节(CLI / JSON / event / 路由 / schema)全部以代码为准 —— 文档不再维护第二份会漂移的副本。下表是「想看 X → 去代码哪」的指针;有自检的优先跑自检。

| 协议 | 代码位置(SoT) | 自检 / 速查 |
|---|---|---|
| 文件系统布局 / 路径 | `crates/ccteam-core/src/paths.rs`(`CcteamPaths`) | — |
| 项目 / session state.json | `crates/ccteam-core/src/`(`ProjectState` / `SessionRecord` serde) | — |
| progress.jsonl 事件 schema | `crates/ccteam-harness/src/execution/progress_bridge.rs` + `enriched_event.rs`(`EventKind`) | schema 单一权威 |
| chat turns.jsonl | `crates/ccteam-im`(turns mirror)+ `ccteam-core` chat 路径 | — |
| CLI 命令 / flag | `crates/ccteam-cli/src/main.rs` + `commands.rs`(clap derive) | `ccteam --help` |
| MCP 工具清单 / schema | `crates/ccteam-cli/src/mcp_tool_groups.rs`(`STUB_TOOLS`)+ mcp serve | `ccteam doctor --verify-mcp`(drift → exit 1) |
| Hooks / settings.json | `crates/ccteam-hooks` + `ccteam internal hook` 子命令;`ccteam init` 落 settings | — |
| Web 路由 / SSE / WS | `crates/ccteam-web/src/routes/*`(axum `.route()`) | — |
| JSON API v1 | `crates/ccteam-web/src/routes/api_v1.rs` | — |
| IM transport / 凭证 | `crates/ccteam-im/src/transport/`(`Channel` trait + providers)+ `im/credentials.json` 解析 | — |
| workflow.yaml schema | `ccteam-flow` / `ccteam-core` 解析代码(推后的编排层) | — |
| HarnessAdapter / ProcessBackend | `crates/ccteam-harness/src/adapter.rs` + `lib.rs` | — |

改协议 = 改代码 +(若新增一类协议)补本表一行。**不**再维护独立的 interfaces.md。
