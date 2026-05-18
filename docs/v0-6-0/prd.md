# V0.6.0 — PRD

6 个 finding。F106 是架构原则文档,定义本版基调;F107 重构准备(零行为变化);F108 + F109 模式 3 落地;F110 + F111 MCP 优化。

> **本 PR 范围**:只 land 本 PRD + `README.md` + `dev-plan.md`。代码改动**全部走后续 wave PR**,以便每 wave 单独 review + 跑 baseline。

---

## F106 — 三执行模式定型 + 红线按模式 scope 重写

### 痛点

CLAUDE.md 现有红线"每次 spawn = 全新 1M context"、"文件系统是控制平面"、"progress.jsonl 唯一 state SoT" 等,**全部默认了一种执行模型**(`claude --bg` 任务式 spawn,artifact-driven)。当 V0.5.0 落了 `ccteam-team` skill(in-process Task 模式)、V0.6.0 要落 chat/IM bot(tmux 长跑模式)时,这些红线**部分不适用 / 含义需要细分**。

具体冲突:
- in-proc team(模式 1)没有 file-system 控制平面,但红线写"是控制平面"
- chat bot(模式 3)就是要复用 context,但红线写"每次 spawn fresh"
- chat bot 输出来自 `~/.claude/projects/<…>/<session-id>.jsonl` 而非 progress.jsonl,但红线写"progress.jsonl 唯一 SoT"

不澄清的后果:后续 PR 作者要么误读红线限制实现、要么实现后跟现有红线表面冲突、要么把红线悄悄改弱。

### 需求

把三模式 + 红线适用矩阵显式写进 tier-1 文档(`tech-design.md` + `CLAUDE.md`),让任何 PR 作者一眼看到自己的改动落在哪个模式 + 守哪些红线。

#### 三模式定义表(权威版)

| 维度 | 模式 1: in-proc team | 模式 2: bg sessions | 模式 3: tmux sessions |
|---|---|---|---|
| **直观场景** | CC 插件 / skill 起临时 team | 长跑自动化 workflow | 随叫随到 chat bot / IM agent |
| **触发** | 用户在 Claude session 内 `/ccteam-team "..."` | `ccteam start <slug>` daemon 起 + ArtifactWatcher 触发 | IM webhook(TG / Slack)→ bridge → send-keys |
| **spawn 原语** | `Task(subagent_type=…)` | `claude --bg --agent <role>` | 长跑 `claude`(交互模式)+ `tmux send-keys` |
| **进程模型** | parent Claude session 内 N teammate(同进程)| 每 spawn 一个独立 Claude job 进程,run-to-completion | 每 bot 一个常驻 tmux pane,内含长跑 `claude` 进程 |
| **input 面** | tool args(`Task` 的 `prompt`)| `.ccteam/inbox/msg-<ts>-NNN.md` 文件 | `tmux send-keys -t <pane> "<text>" Enter`(含 `/new` `/compact` `/clear`)|
| **output 面** | tool result(返回值)| `progress.jsonl` 业务事件 + `state.json` cost | `~/.claude/projects/<encoded-path>/<session-id>.jsonl` tail(Anthropic 官方 session 格式)|
| **context 生命周期** | parent session window,死则同死 | **每次 spawn fresh 1M**,无 `--resume` | **长跑复用**;`/compact` 摘要 / `/new` 重置 / `/clear` 清,**在线管理** |
| **cost model** | shares parent context budget | 每 spawn 重 tokenize 全 prompt,无 cache 命中 | warm cache(turn 2 起 cache hit),`/compact` 重置 baseline |
| **可观测性** | parent Claude 自看 SendMessage | progress.jsonl + web UI | progress.jsonl(业务事件)+ session jsonl(对话原文)+ web UI 加 Chat View |
| **状态恢复** | 不适用(session 死即丢)| progress.jsonl + 文件 SoT 完整可重建 | session-id 持久化(`~/.ccteam/im/<bot>/session-id`),失效则重启 + 重放 last-N(F108 §恢复)|
| **跨设备** | ✗(锁在 parent session)| ✓(daemon 在哪跑,在哪看)| ✓(IM 是 location-agnostic)|

#### 红线 × 模式适用矩阵(改 `tech-design.md` §0 + `CLAUDE.md` §三)

| # | 红线 | 模式 1 | 模式 2 | 模式 3 | 细则 |
|---|---|:-:|:-:|:-:|---|
| R1 | 文件系统是控制平面 | — | ✓ | ⚠ | 模式 3 **输出**是文件(session jsonl);**输入**是 send-keys,**不是文件**。措辞改为"输出 SoT 是文件" |
| R2 | progress.jsonl 唯一 state SoT | — | ✓ | ⚠ | 模式 3:progress.jsonl 是**业务事件 SoT**(@-routed / compact / hop-escalate),对话原文在 session jsonl。两个 SoT **不重叠**,各管一半 |
| R3 | 每次 spawn = fresh 1M context | — | ✓ | ✗ | 模式 3 显式**不适用**。chat 场景 context 复用是 feature,通过 `/compact` `/new` 管理 |
| R4 | 不解析 tmux 输出 | — | ✓ | ✓ | 模式 3 **守**。读 session jsonl(Anthropic 官方格式)替代 tmux capture-pane |
| R5 | 永不主动 kill 长 session | ✓ | ✓(F84 budget cap 唯一例外)| ✓ | 模式 3:`/compact` `/new` 是**合法状态操作**,不算 kill |
| R6 | fix-loop 3 次必 escalate | ✓ | ✓ | ✓ + 新增 | 模式 3 加 bot-to-bot @ ping-pong **hop_limit**(默认 3,workflow.yaml 可配),撞 limit 同等 escalate |
| R7 | `ccteam-core` 零 team 名字面量 | ✓ | ✓ | ✓ | 全模式守 |
| R8 | 跨项目记忆走官方接口 | ✓ | ✓ | ✓ | 模式 3 的 IM 用户身份 ≠ ccteam memory,不污染 `~/.claude/CLAUDE.md` |
| R9 | 新建项目走 `<projects_root>/<team>-<slug>/` | — | ✓ | ✓ | 模式 3 `ccteam new --mode chat <slug>` 仍走此 layout;bot per project |

(— = 模式不涉及该红线场景)

### 验收

1. `docs/tech-design.md` §0(开篇红线表)按上表重写,每红线标 `[模式 1/2/3]` 适用
2. `CLAUDE.md` §三红线表加"模式"列(精简版,详 link 到 tech-design)
3. `docs/orchestration-patterns.md` §一加"模式 × 拓扑模式"对应表(哪个执行模式适合哪种 orchestration topology)
4. `docs/v0-6-0/README.md` 三模式表是唯一权威源,其他文档引用 link 回 README
5. PR 描述带 `grep -rn "每次 spawn.*fresh"` 全仓比对:所有命中处或加 [模式 2] 标记、或删

### 不在范围

- 不动现有 V0.4-V0.5 prd / dev-plan(EOL 版本归档不动,CLAUDE.md §五原则)
- 不删任何红线,只 scope 和细则化
- 不引入新红线(模式 3 hop_limit 是 R6 的扩展,不是新条)

---

## F107 — `ExecutionAdapter` trait 抽离 + `BgSpawner` 改造对接

### 痛点

现有 `crates/ccteam-core/src/orchestrator.rs` + `bg_spawner.rs`(实际文件名待验,可能在 `spawn.rs` / `claude_job.rs`)直接以 `claude --bg --agent` 为唯一 spawn 路径。模式 3 要塞 `claude`(交互模式)长跑 + send-keys input + session jsonl tail output,**没法在现有路径里加 if/else**,会污染 orchestrator 主循环。

需要在 ccteam-core 抽出一个 `ExecutionAdapter` trait,把"如何 spawn / 如何送 input / 如何观察 output / 如何 lifecycle 操作"抽象掉。`BgSpawner` 重命名为 `BgSpawnAdapter` 实现 trait,**行为零变化**。F108 再加 `TmuxInteractiveAdapter`,F109 的 im-bridge 通过 trait 用。

### 需求

#### trait 接口

```rust
// crates/ccteam-core/src/execution/mod.rs(新)
pub trait ExecutionAdapter: Send + Sync {
    /// spawn 一个新 agent session;返回 handle 用于后续 input/observe/lifecycle 操作
    fn spawn(&self, role: &str, ctx: SpawnContext) -> Result<SessionHandle>;

    /// 投递一条 input message 到 session
    /// 模式 2:写 .ccteam/inbox/msg-*.md
    /// 模式 3:tmux send-keys
    fn send_input(&self, handle: &SessionHandle, input: Input) -> Result<()>;

    /// 流式观察 session 输出(driver 侧 await)
    /// 模式 2:tail progress.jsonl 业务事件
    /// 模式 3:tail session jsonl + emit ChatTurn
    fn observe(&self, handle: &SessionHandle) -> BoxStream<'static, ObservedEvent>;

    /// session 生命周期操作(模式 3 主用)
    fn lifecycle(&self, handle: &SessionHandle, op: LifecycleOp) -> Result<()>;
}

pub struct SpawnContext {
    pub workflow_slug: String,
    pub agent_md_path: PathBuf,
    pub env: Vec<(String, String)>,
    pub mode_specific: ModeSpecificCtx,  // 区分 mode 2 / mode 3 必要字段
}

pub enum Input {
    User { content: String },          // chat 场景的 user 输入
    SlashCommand { name: String, args: Vec<String> },
    InboxMarkdown { body: String },    // 模式 2 现有 path
}

pub enum LifecycleOp {
    Compact,                            // 模式 3:/compact
    NewSession,                         // 模式 3:/new
    Clear,                              // 模式 3:/clear
    Close,                              // 全模式
}

pub enum ObservedEvent {
    BusinessEvent(ProgressEvent),       // mode 2 主路径
    ChatTurn { speaker: Speaker, content: String, ts: SystemTime },  // mode 3 主路径
    LifecycleAck { op: LifecycleOp, ok: bool },
    Closed { reason: String },
}

pub struct SessionHandle {
    pub mode: ExecutionMode,
    pub identity: String,               // mode 2: jobs/<id>;mode 3: tmux pane + session-id
    pub spawned_at: SystemTime,
}

pub enum ExecutionMode { InProc, Bg, TmuxInteractive }
```

#### `BgSpawnAdapter` 改造点

| 现在 | 改后 |
|---|---|
| `orchestrator.rs` 直接 `Command::new("claude").arg("--bg")...` | 通过 trait `bg_adapter.spawn(...)` |
| ArtifactWatcher 发现 inbox 文件 → 现 `spawn_session(...)` | trait `bg_adapter.send_input(handle, Input::InboxMarkdown {..})` |
| progress.jsonl tail 现 ad-hoc | trait `bg_adapter.observe(handle)` 返流 |
| pause/resume/cancel 现 `cancel_token` 直接 | trait `bg_adapter.lifecycle(handle, LifecycleOp::Close)` |

**关键不变量**:重构后 `cargo test --workspace` 全部测试通过,数字跟 V0.5.1 baseline(942/1)持平。**这是 wave 1 唯一硬验收**。

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/mod.rs`(新)| trait 定义 + 共享类型(SpawnContext / Input / LifecycleOp / ObservedEvent / SessionHandle / ExecutionMode)|
| `crates/ccteam-core/src/execution/bg.rs`(新,迁移自现 spawn 路径)| `BgSpawnAdapter` impl ExecutionAdapter;包装现 spawn_session / state.json watch / progress.jsonl tail |
| `crates/ccteam-core/src/orchestrator.rs` | 主循环改 `Arc<dyn ExecutionAdapter>`;路径分支 `match mode { Bg => bg_adapter, TmuxInteractive => tmux_adapter, ... }` |
| `crates/ccteam-cli/src/commands.rs` | spawn / send / pause / resume 路径改走 trait |
| `crates/ccteam-core/tests/execution_trait_test.rs`(新)| 4 测试:trait 对象动态分发 / BgSpawnAdapter 路径全 happy path / lifecycle Close 等价旧 cancel / observe 流退出条件 |

### 验收

1. `cargo test --workspace --locked --no-fail-fast` 通过数 ≥942(允许 +新增 trait 测试,**禁止减少**);失败数仅 1(已有 ccteam-web flake,**新增不允许**)
2. clippy 0 errors + ≤18 warnings(允许减少,禁止新增)
3. `grep -rn "Command::new(\"claude\")" crates/ccteam-core` 命中**仅在** `execution/bg.rs`,主 orchestrator 零命中
4. `grep -rn "execution::ExecutionAdapter" crates/ ` ≥3 命中(core + cli + 测试)
5. host probe:V0.5.1 跑过的 dex-ui workflow.yaml 在 V0.6.0 wave-1 二进制下行为完全一致(progress.jsonl event 序列 diff = 0)

### 不在范围

- 不实现 `TmuxInteractiveAdapter`(F108)
- 不实现 `InProcTeamAdapter`(在 F107 trait 设计完后,V0.5.0 `ccteam-team` skill 是否能 trait 化是 V0.6.1 议题,V0.6.0 不强求 — skill 是 in-Claude-session 逻辑,本来就不在 ccteam-core 里跑)
- 不改 MCP / CLI 用户面接口(纯内部重构)

---

## F108 — `TmuxInteractiveAdapter`(模式 3 执行 runtime)

### 痛点

模式 3 chat bot 需要长跑 Claude session 复用 context(不复用 → 每轮重 tokenize 全历史,N 轮 5k 历史成本 O(N²),无 prompt cache 命中)。现有 `claude --bg` 是 run-to-completion,出口即死,不能多轮。

Claude Code 本身支持 `claude --resume <session-id> --input-format stream-json --output-format stream-json`,可以长跑 + stream I/O。但 ccteam 现有红线"不解析 tmux 输出"指向**不用 capture-pane**,所以要找到既能 stdin-style 控制又不 scrape pane 的路径。

**结论**(详 README §七 决策记录):
- **input** 走 `tmux send-keys`(tmux pane 已有,plumbing 现成)
- **output** 走 `~/.claude/projects/<encoded-path>/<session-id>.jsonl` tail(Anthropic 官方 session 格式,稳定可解析)
- pane 内跑 `claude --resume <session-id>` 长跑,session-id 由 adapter 持久化

### 需求

#### `TmuxInteractiveAdapter` 行为

```
spawn(role, ctx):
  1. 决定 bot identity:slug = ctx.workflow_slug + role + bot-name
  2. 检查 ~/.ccteam/im/<slug>/session-id:
     - 存在 → claude --resume <id> 在 tmux pane 启动
     - 不存在 → claude(新 session)在 tmux pane 启动,捕获新 session-id 写文件
  3. agent_md_path 通过 Claude Code subagent resolver(项目 .claude/agents/ → ~/.claude/agents/)
  4. 返回 SessionHandle{ mode: TmuxInteractive, identity: <pane-id>+<session-id> }

send_input(handle, Input::User { content }):
  tmux send-keys -t <pane> -- "<escape(content)>" Enter
  (escape:换行用 send-keys Enter 分段;backtick / $ 用 -l 字面量模式)

send_input(handle, Input::SlashCommand { name, args }):
  tmux send-keys -t <pane> -- "/<name> <args>" Enter

observe(handle) -> Stream<ObservedEvent>:
  inotify watch ~/.claude/projects/<…>/<session-id>.jsonl
  parse 增量 line(每 line 一条 Anthropic SDK event)
  → ChatTurn { speaker: Assistant, content: ... }(filter user/system)
  → 检测 lifecycle ack(/compact 完毕 / /new 后新 session-id 出现)
  → 文件被 Anthropic 重命名(/new 触发)→ 重新 watch 新 session-id

lifecycle(handle, op):
  Compact -> send-keys "/compact" Enter
  NewSession -> send-keys "/new" Enter;监 ~/.claude/projects/<…>/ 新 jsonl 出现 → 更新 session-id 持久化
  Clear -> send-keys "/clear" Enter
  Close -> send-keys C-c C-c + tmux kill-pane;session-id 文件保留(下次 spawn 可 resume)
```

#### workflow.yaml `mode: chat` schema(新)

```yaml
version: 0.6
mode: chat                              # 新枚举值,触发 TmuxInteractiveAdapter
agents:
  - role: helpful-bot
    bot_name: "@helpful_bot"            # IM 暴露的 handle(F109 用)
    compact_every_turns: 50             # 累计 turn 数达到则自动 /compact
    new_on_topic_shift: false           # 暂留 false(主题检测下版)
    hop_limit: 3                        # bot-to-bot @ 链路上限(R6 红线)
    model: opus-4-7                     # 同现有
  - role: critic-bot
    bot_name: "@critic_bot"
    compact_every_turns: 100
    hop_limit: 3
im_channels:                            # F109 用
  - kind: telegram
    chat_id_env: TG_CHAT_ID
    bot_token_env: TG_BOT_TOKEN
```

#### session-id 失效恢复

session-id 失效场景:Claude Code 升级后 session 文件格式 incompat、用户 `rm -rf ~/.claude/projects/`、磁盘 fsck 丢文件。

V0.6.0 策略:**fail-open + 业务事件可见**
- 检测 `claude --resume <id>` 退出码非 0 → adapter 启用 fresh `claude`,新 session-id 持久化
- emit progress 事件 `chat_session_reset { bot, old_session_id, reason }`(让用户 web UI / TG 知道 bot "失忆"了)
- **不**自动重放历史(那是 V0.6.1 议题)

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/tmux_interactive.rs`(新)| `TmuxInteractiveAdapter` impl,~600 行 |
| `crates/ccteam-core/src/execution/session_jsonl_tail.rs`(新)| inotify watch + 增量 jsonl parse;复用 V0.5.0 F92 transcript_scanner 的 mtime+length memoize 思路 |
| `crates/ccteam-core/src/execution/send_keys.rs`(新)| tmux send-keys 安全 escape;**带 fuzzing test**(任意 unicode + ANSI 控制字符 输入不 corrupt pane)|
| `crates/ccteam-core/src/workflow_schema.rs`(改)| 加 `mode: chat` 枚举值 + `bot_name` / `compact_every_turns` / `new_on_topic_shift` / `hop_limit` 字段;serde tagged enum 区分 artifact-driven vs agent-team vs chat |
| `crates/ccteam-core/src/progress_event.rs`(改)| 加 4 新 event 类型:`chat_turn` / `chat_session_reset` / `chat_compact_done` / `chat_hop_escalate` |
| `crates/ccteam-core/tests/tmux_interactive_test.rs`(新)| 8 测试:spawn 新 session / spawn resume 已存 session / send_input escape edge case / observe 流增量 / lifecycle compact / lifecycle new(session-id 更新)/ session-id 失效 fallback / fuzzing send-keys |

### 验收

1. host probe:跑 chat workflow,bot 多轮 @ 后 `claude --resume` 实测 prompt cache hit ≥turn 2(从 transcript usage 字段读 cache_read_input_tokens > 0)
2. `/compact` 触发后,后续 turn 的 input_tokens 显著下降(<前一个 turn 的 1/3,具体阈值 manual probe)
3. session-id 失效 mock test:删 session jsonl → spawn 退化为 fresh,emit `chat_session_reset`
4. send-keys fuzzing 200 sample 输入,无 pane corruption(pane 内 `claude` 进程仍 alive)
5. `cargo test --workspace --locked --no-fail-fast` baseline 数字增长(预计 +10 测试)

### 不在范围

- **跨 session memory 重建** — session-id 丢了不重放,V0.6.1 议题
- **主题漂移检测自动 `/new`**(`new_on_topic_shift: true` 暂留 false 不实现) — V0.6.2 议题
- **stdin pipe 替代 send-keys** — 决策选 send-keys,V0.6.x 不切

---

## F109 — `ccteam-im-bridge` crate(模式 3 IM 触发源,Telegram 起步)

### 痛点

模式 3 的触发源不是 ArtifactWatcher(文件触发),而是 IM webhook(`@bot_name "..."` 来自 TG / Slack)。这是一个独立的 trigger 通道,职责清晰:**把 IM 消息翻译成 `ExecutionAdapter::send_input` 调用,并把 `observe()` 出来的 ChatTurn 翻译回 IM post**。

放新 crate 而不是塞 ccteam-core 的理由:
- IM SDK(`teloxide` / TG bot crate)是大依赖,不该污染 core
- 多 IM 平台扩展点天然走插件式 crate(`ccteam-im-bridge-telegram` / `ccteam-im-bridge-slack` / ...)
- 独立 crate 边界 + `cargo tree` 测试守住:core 永不依赖 IM SDK

### 需求

#### Crate 结构

```
crates/ccteam-im-bridge/
├── Cargo.toml                       # 依赖:ccteam-core(读 workflow.yaml),teloxide(TG)
├── src/
│   ├── lib.rs                       # 总入口 trait ImBridge
│   ├── telegram.rs                  # TgBridge impl
│   ├── router.rs                    # bot-to-bot @ 路由(本 crate 内,不入 core)
│   ├── hop_tracker.rs               # R6 hop_limit 计数 + escalate
│   └── session_link.rs              # IM user ↔ bot session-id 映射
└── tests/...
```

#### trait `ImBridge`

```rust
pub trait ImBridge: Send + Sync {
    /// 启动 webhook / long-poll;阻塞循环
    async fn run(&self, ctx: BridgeContext) -> Result<()>;
}

pub struct BridgeContext {
    pub adapter: Arc<dyn ExecutionAdapter>,            // F107 trait
    pub workflow_slug: String,
    pub bot_roster: Vec<BotConfig>,                    // workflow.yaml `agents:` 解析
    pub progress_writer: ProgressWriter,               // emit chat_* event
}
```

#### TG 实现关键路径

```
TG @helpful_bot "explain X":
  1. teloxide 收 message,parse @mention
  2. session_link.lookup(tg_chat_id, "@helpful_bot") -> bot SessionHandle
     - 不存在 → adapter.spawn(role="helpful-bot", ctx) → 存映射
  3. adapter.send_input(handle, Input::User { content: "explain X" })
  4. await adapter.observe(handle).take_while(|e| !is_turn_end(e))
  5. assistant content → teloxide.send_message(tg_chat_id, content)
  6. progress_writer.emit(ChatTurn { bot, ts, ... })

assistant content 含 "@critic_bot 看下":
  1. router.parse_mentions(content) -> ["@critic_bot"]
  2. hop_tracker.check(tg_chat_id, current_chain) -> ok | escalate
     - escalate → progress_writer.emit(ChatHopEscalate { chain, limit })
       + teloxide 回 "@user 该话题 bot 间已链 3 轮,人工介入"
  3. ok → 递归:adapter.send_input(critic_handle, Input::User {..})
  4. critic 输出 → 同样 teloxide post 到 TG(以 critic_bot 身份)
```

#### CLI 集成

```bash
ccteam start <chat-slug>              # 现有 daemon 起;chat mode 检测后额外起 im-bridge
                                       # bridge 进程是 daemon 的 child,daemon stop 时 graceful 关
ccteam internal im-bridge <slug>      # 单独起 bridge(调试用,daemon 不启)
```

实施:`ccteam-cli::start` 检测 workflow.yaml `mode: chat` → 注册 `ccteam-core` daemon hook:启动后 spawn `ccteam-im-bridge` 进程 + tokio task。bridge 通过 `ExecutionAdapter` trait 操作 adapter,**不直接 import `TmuxInteractiveAdapter`**(松耦合)。

#### env / secrets

TG bot token / chat ID 不入 `workflow.yaml`(R8 红线 + 安全),只声明 env 变量名(`bot_token_env: TG_BOT_TOKEN`),token 在 `ccteam start` 启动环境拿。

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-im-bridge/Cargo.toml`(新)| 依赖 ccteam-core(workflow.yaml 解析)+ teloxide 0.13(只在 telegram feature 下)+ tokio + anyhow |
| `crates/ccteam-im-bridge/src/lib.rs` 等(新)| 上述结构 |
| `crates/ccteam-cli/src/commands.rs::start` | 检测 `mode: chat` → spawn bridge |
| `crates/ccteam-cli/src/main.rs::internal` | 加 `im-bridge <slug>` 子命令 |
| `Cargo.toml`(workspace 根)| 加 member `crates/ccteam-im-bridge` |
| `crates/ccteam-im-bridge/tests/router_test.rs`(新)| mention parse / hop_limit / escalate 触发 |
| `crates/ccteam-im-bridge/tests/dep_graph_test.rs`(新)| `cargo tree -p ccteam-core` 不能包含 teloxide(松耦合红线)|

### 验收

1. host probe(需 TG 账号):TG 群里 `@helpful_bot 你好` → bot 回应 → emit `chat_turn` event;`@helpful_bot @critic_bot "review my plan"` → 双 bot 链式回应,emit 2 个 `chat_turn`
2. hop_limit 测试:mock bot-to-bot 链 4 轮 → 第 4 轮 escalate,TG 收人工介入消息
3. session_link 持久化:bridge 重启后,同一 TG user 继续 @ 同一 bot,bot 仍是同 session-id(prompt cache 仍命中)
4. dep_graph_test 通过:`ccteam-core` 不引 teloxide
5. `ccteam start <chat-slug>` 不阻塞 mode 2 现有 workflow(混合 mode 项目 — 一个 daemon 同时跑 artifact + chat;V0.6.0 不要求,但不能 break)

### 不在范围

- Slack / Discord / IRC bridge — V0.7.x;V0.6.0 只 trait + TG 一个实现,trait 必须足够泛化
- 富媒体(图片 / 文件 / voice)— TG 消息只 text + reply markup,V0.6.x 不扩
- DM(私聊)scope — V0.6.0 只支持 group chat;DM 是 V0.6.1
- IM webhook reverse proxy / public URL 自动注册 — 用户自行 ngrok / cloudflared

---

## F110 — MCP namespace `ccteam` → `ct` + 子前缀工具命名

### 痛点

现有 17 MCP 工具命名 `mcp__ccteam__<verb>`,prefix `ccteam` 占 6 字符 / `mcp__ccteam__` 占 11 字符。每次 Claude session 启动 / tool list 拉取 / Claude 调用 / 错误返回都吃这个 prefix。考虑 tool list 在 session 启动 prompt 里出现一次(~17 行 × 字符数)+ 每次 tool call assistant message 出现一次(每次 ~30 token),长期摊销有意义。

OMC 用单字母 `t` 作 namespace 已验证可行。ccteam 选 2 字母 `ct`(safer than `t`, 避撞)。

同时模式 3 加 6+ 新工具(send_chat_input / chat_lifecycle / observe_session / bot_roster 等),全部继续平铺 + 单一前缀会让 17 → 23 工具列表更难定位。**单 server + 子前缀**(`workflow_ls` / `chat_send_input`)更清晰。

### 需求

#### 新命名(全表)

| 现 | 新 |
|---|---|
| `mcp__ccteam__ls` | `mcp__ct__workflow_ls` |
| `mcp__ccteam__show` | `mcp__ct__workflow_show` |
| `mcp__ccteam__peek` | `mcp__ct__workflow_peek` |
| `mcp__ccteam__progress` | `mcp__ct__workflow_progress` |
| `mcp__ccteam__new` | `mcp__ct__workflow_new` |
| `mcp__ccteam__pause` | `mcp__ct__workflow_pause` |
| `mcp__ccteam__resume` | `mcp__ct__workflow_resume` |
| `mcp__ccteam__send_to_session` | `mcp__ct__workflow_send_to_session` |
| `mcp__ccteam__inject_decision` | `mcp__ct__workflow_inject_decision` |
| `mcp__ccteam__screenshot` | `mcp__ct__workflow_screenshot` |
| `mcp__ccteam__spawn_agent` | `mcp__ct__workflow_spawn_agent` |
| `mcp__ccteam__stop_agent` | `mcp__ct__workflow_stop_agent` |
| `mcp__ccteam__observe_agents` | `mcp__ct__workflow_observe_agents` |
| `mcp__ccteam__signal` | `mcp__ct__workflow_signal` |
| `mcp__ccteam__set_parallelism` | `mcp__ct__workflow_set_parallelism` |
| `mcp__ccteam__trigger_gate` | `mcp__ct__workflow_trigger_gate` |
| `mcp__ccteam__get_artifact_summary` | `mcp__ct__workflow_get_artifact_summary` |
| **(新)** | `mcp__ct__chat_send_input` |
| **(新)** | `mcp__ct__chat_lifecycle`(compact / new / clear / close)|
| **(新)** | `mcp__ct__chat_observe_session`(返当前 jsonl tail snapshot)|
| **(新)** | `mcp__ct__chat_bot_roster`(列当前 chat workflow 所有 bot + 状态)|
| **(新)** | `mcp__ct__chat_session_reset_force`(管理员强制 `/new`,emit reset 事件)|

#### Breaking 策略

CLAUDE.md §五:no backwards-compat shim。**直接 rename**,不留 `mcp__ccteam__*` alias 一版。同步改:
1. `ccteam-control` skill `SKILL.md` capability index(全 17 行替换 + 加 5 新行)
2. meta-agent prompt 全文 replace
3. `crates/ccteam-cli/src/mcp_serve.rs` + `mcp_workflow_tools.rs` 注册名字
4. test fixture / snapshot 同步

升级体验:用户跑 `ccteam doctor --install-mcp` 重新写 `~/.claude.json` + `/reload-mcp`。旧 session 的 tool call 用错名字 → MCP server 返 method-not-found,Claude 自动重试新名(LLM 看到错误能改)。损失低。

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/mcp_serve.rs` | server name `"ccteam"` → `"ct"`;tool registration 加 `workflow_` 前缀 |
| `crates/ccteam-cli/src/mcp_workflow_tools.rs` | 同上,7 工具加前缀 |
| `crates/ccteam-cli/src/mcp_chat_tools.rs`(新)| 5 chat 工具注册(F108 + F109 业务调用)|
| `skills/ccteam-control/SKILL.md` | capability index 表整体重写 |
| `crates/ccteam-cli/src/commands.rs::install_meta_agent` | meta-agent prompt template 全文 replace |
| `crates/ccteam-cli/tests/mcp_protocol_test.rs`(改名 / 新)| tool name 全更新 |

### 验收

1. `ccteam doctor --install-mcp` 写入的 `~/.claude.json` 含 `"mcpServers": { "ct": {...} }`(无 `"ccteam"` 键)
2. `mcp__ct__workflow_ls` 等所有工具在 `/mcp` 列表里出现,名字正确
3. `grep -rn "mcp__ccteam__" crates/ skills/ docs/v0-5-0/ docs/v0-5-1/` 命中**仅**老版本归档 dir(EOL 不动);`crates/` 和 `skills/` 命中 0
4. host probe:V0.5.1 跑过的 meta-agent 工作流(read project,inject_decision)在 V0.6.0 全部用新名字跑通

### 不在范围

- 不留 `ccteam` server 别名(breaking is OK)
- 不动 OMC / claude-mem 等外部 MCP 命名

---

## F111 — MCP 工具粒度配置(`CCTEAM_DISABLE_TOOLS` + 项目级 `.mcp.json`)

### 痛点

V0.6.0 后 MCP 工具总数 22(17 现 + 5 chat),全部发布到每个 Claude session 的 tool list:
- 每 session 启动 prompt 含 22 工具 schema(每个 ~50 token),~1100 token 固定开销
- 用户只用模式 2 → chat_* 5 工具白占空间;反之亦然
- 一些工具(`workflow_screenshot` 渲染图片成本高,`workflow_set_parallelism` 危险操作)用户想禁

OMC `OMC_DISABLE_TOOLS` env-driven 提供了精确控制范本。

同时:现 MCP 注册路径只 `~/.claude.json`(user-global,所有 Claude session 都看到 ccteam MCP)。项目级 `.mcp.json` 更可移植:`git clone` 后任何 Claude session 在该项目根自动拿到 ccteam MCP,无需 doctor 安装。

### 需求

#### `CCTEAM_DISABLE_TOOLS` env

```bash
# 一律 glob 匹配 tool short name(不含 mcp__ct__ 前缀)
export CCTEAM_DISABLE_TOOLS="chat_*"                     # 用户只跑模式 2
export CCTEAM_DISABLE_TOOLS="workflow_screenshot,workflow_set_parallelism"
export CCTEAM_DISABLE_TOOLS="chat_*,workflow_screenshot"  # 组合
```

`ccteam mcp-serve` 启动时读环境变量,过滤 `tools/list` 返回。被禁工具被 call 时返 method-not-found。

#### 项目级 `.mcp.json` 自动生成

`ccteam init` 落项目 `.mcp.json`:

```json
{
  "mcpServers": {
    "ct": {
      "command": "ccteam",
      "args": ["mcp-serve"],
      "env": {
        "CCTEAM_PROJECT_ROOT": "${workspaceFolder}",
        "CCTEAM_DISABLE_TOOLS": ""
      }
    }
  }
}
```

Claude Code 在项目根启动时自动 merge `.mcp.json` + `~/.claude.json`(项目级覆盖 user-global)。

`ccteam doctor --install-mcp` 仍写 `~/.claude.json`(保 user-global fallback,任意 cwd 都能用);但**优先**推荐项目级。doctor 提示语调整:"推荐 `ccteam init` 落项目 `.mcp.json`,本命令是 user-global 后备。"

#### `.mcp.json` 已存在的合并

`ccteam init` 检测 `.mcp.json` 已存在:
- 含 `ct` 键 → 跳过,告知用户已存在
- 不含 `ct` 键 → merge `ct` 键进去,保留其他 MCP server 不动
- JSON 损坏 → 报错 abort(不覆盖用户文件)

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/mcp_serve.rs` | 启动读 `CCTEAM_DISABLE_TOOLS`,glob 过滤 `tools/list` + `tools/call` |
| `crates/ccteam-cli/src/commands.rs::init` | 调 `write_project_mcp_json(<project_root>)`,merge 逻辑 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | `--install-mcp` 输出加项目级推荐 |
| `crates/ccteam-cli/tests/mcp_disable_test.rs`(新)| 5 测试:single tool disable / glob disable / disable 全部 chat_* / 被禁工具 call 返错 / 无 env 行为不变 |
| `crates/ccteam-cli/tests/project_mcp_json_test.rs`(新)| 4 测试:全新 .mcp.json / 合并已有 / ct 键已存在跳过 / 损坏 JSON abort |

### 验收

1. `CCTEAM_DISABLE_TOOLS=chat_* ccteam mcp-serve` 启动后,`tools/list` 不含 chat_* 5 工具
2. `ccteam init` 在空目录落 `.mcp.json`,内容含 `ct` server 注册
3. `ccteam init` 在已有 `.mcp.json`(含其他 server)的目录运行,merge 后其他 server 保留
4. host probe:VS Code Claude Code extension 在 `ccteam init` 后的项目里启动,`/mcp` 显示 `ct` 已连接(无需 `--install-mcp`)
5. 同一 user 同时用 `~/.claude.json` 全局 ct + 项目 `.mcp.json` ct,无冲突(Claude Code 项目级覆盖)

### 不在范围

- 不实现工具类别分组(只 glob)
- 不做 `.mcp.json` GUI 编辑器
- 不替代 doctor --install-mcp(保留作 user-global 入口)

---

## 文档影响清单

| 文档 | 改动 |
|---|---|
| `CLAUDE.md` §一(当前状态表)| Workspace version → 0.6.0;baseline 数字 wave 3 ship 后回填 |
| `CLAUDE.md` §三红线 | 加"模式"列(精简);link 到 tech-design 详 |
| `docs/tech-design.md` §0(若存在)/ §2.1 | 三模式定义表 + 红线 × 模式矩阵;`ExecutionAdapter` trait 接口 |
| `docs/orchestration-patterns.md` §一 | 5 拓扑模式 × 3 执行模式适用矩阵 |
| `docs/interfaces.md` | MCP 工具名全表更新;workflow.yaml `mode: chat` schema 加入 |
| `docs/dev-coupling-audit.md` | F106-F111 各一条 finding 简述 |
| `docs/claude-code-best-practices.md` | 加一节"用 send-keys + session jsonl 实现长跑 agent"(模式 3 落地经验) |
| `docs/v0-6-0/user-manual.md`(新)| chat mode 用户使用指南(TG bot 起步教程)|
