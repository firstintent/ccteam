# v0.8.5 架构小范围重构 —— 补全两个核心概念:Harness 与 Session

> 配套 `prd.md`(实现规约)+ `dev-plan.md`(wave 编排)+ `arch-refactor-review.md`(本文的 5-lens 评审报告,verdict = APPROVE_WITH_FIXES,本版已按 must-fix 修订)。本文回答:**D1–D6 / P1 / P3 / F10 这批需求落进现有代码,应该围绕什么概念组织、改哪些接缝**。
>
> **概念纪律:不造新顶层概念**。v0.8.5 全部需求收敛为对既有**两个核心概念**的补全:
>
> - **Harness** —— vendor 执行引擎(Claude Code / Codex / 未来 Gemini CLI、Amp、OpenCode…),即执行层两轴中的 `HarnessAdapter` 轴;
> - **Session** —— harness 上的长会话,即 `chat ⇄ project ⇄ session` 三层模型里的 session、resume-by-id 红线保护的那个对象。
>
> 这两个概念**轴与对象都已存在**(harness 轴横跨 chat(本版)/ bg(已有)/ 推后的 `ccteam-flow` 编排层;session 是 tech-design §2.1 三层模型的第三层);v0.8.5 做的是把它们**抬升为完整概念、在 chat 路径上补完整** —— bg / flow 不动,日后 flow 落地时直接继承补全后的两概念,不再返工。
>
> `Directive` / `ChoicePrompt` / `ThreadStatus` 等新类型都是这两个概念的**刻面词汇**(vocabulary),不是新概念。判据:任何实现物必须回答「它让 harness 还是 session 变完整了?」——答不上来的就是 scope creep。
>
> **克制原则:重构不是目的**。每一项改动必须过准入门槛 —— 要么修**真实缺陷**,要么消除 v0.8.5 落地**必然产生的重复**;抽象不上的就保持原样(§0 末尾的准入表逐项给出理由;§7「明确不做」与改动项同等重要)。
>
> 范围纪律:只动 v0.8.5 触及的接缝。不动 crate 拓扑(`core→harness→cost`)、不动 progress.jsonl SoT、不动 ProcessBackend 轴、不动 bg 路径、**不改任何既有名字**(`HarnessAdapter` / `ThreadHandle` / `GatewaySession` 本来就承载这两个概念,补语义不换皮)。代码引用基于 HEAD(`cc65860`)。

---

## 0. 总览:两个概念今天各缺什么,补全成什么

| 概念 | 今天(代码现状) | v0.8.5 补全后 |
|---|---|---|
| **Harness** | 一个**可丢弃的 trait object**:factory 每次调用 new 一个(`daemon.rs:60-69`,gateway 三处按需调,`gateway.rs:680/:864/:283`);transport 选路是调用点内的 env 四态矩阵(`codex_app_server.rs:153-206`);vendor 命令知识缺位 —— gateway 替它做 vendor 分支(`gateway.rs:1422-1431` 的 `compact\|review` allowlist,Codex 其余 slash 静默降级) | **每 daemon × 每 vendor 恰好一个**的长生命周期引擎:拥有自己的 transport(构造期定)、vendor 级运行态(notification dispatcher / 缓存)、**命令面解释权**(`handle_directive`)、session 属性推导(`thread_status` 的实现方) |
| **Session** | 一个**能收发文本的句柄**(`ThreadHandle` + `GatewaySession`):有身份、能 resume、能 submit_turn,但**不可询问**(model/ctx 无处问)、**不可被指令**(slash 靠 `TurnInput::SystemDirective` 硬塞成特殊 turn)、**不能反问**(AskUserQuestion 弹 TUI picker 卡死 turn) | 「**有属性、可被指令、能反问**」的对象:属性 = `ThreadStatus`(model+ctx);指令 = `Directive → DirectiveOutcome` 五值穷尽;反问 = 挂起交互(`ChoicePrompt` + PendingInteraction,单飞 per session) |

需求映射:Harness 补全吃掉 **F10 + D2.2/D2.4 + D1 的 trait 面**;Session 三刻面吃掉 **D1–D6 + P1 + P3**。P4(skill packaging)与概念正交,纯打包,本文不涉。

**改动项准入表(克制原则的落实:每项要么修真实缺陷,要么消除必然重复;两者都不沾的不进本文)**:

| 改动 | 准入理由 |
|---|---|
| harness 单例(§1.1) | **真实缺陷**:stdio 化后 factory-per-call = 单 daemon 多 codex session 即多子进程 |
| transport 单轴(§1.2) | **真实缺陷**:四态矩阵两格死路;断言需拉进程才能测 |
| tracker(§1.3) | **真实缺陷**:流内副作用会被多订阅重算 / 无消费者丢数;同时消除 D2.2/D2.4 两套机制重复 |
| `handle_directive`(§1.4) | PRD D1 本体(背书 + 放置论证,非新增重构) |
| `GATEWAY_COMMANDS` + `register_commands`(§2.1) | **必然重复**:P1 落地否则四处手抄名单 + daemon 写 telegram 特例 |
| PendingInteraction 双 origin(§2.2) | **必然重复**:否则 W2 D6 重写一整套挂起/超时/回填归一 |
| `thread_status`(§2.3) | PRD P3 本体的表面收敛(否则 `/sessions` 与 codex `/status` 各拼各的) |

**已知 vendor 知识残留(评审 lens 实证,本版如实记录、按克制原则处置)**:补完 `handle_directive` 后,gateway 仍有两处 vendor 知识不在本版清除:

1. **resume 恢复策略的 vendor match**(`gateway.rs:284-319`):Claude arm 带 resume-fail→start_thread reattach 兜底,Codex arm 没有 —— 恢复**策略**住在 gateway 而非 adapter。新 vendor 加 `AgentVendor` variant 时编译器会强制补 arm(有编译信号,非静默),但概念归属错位。处置:**推后**(§7-7,升级路径 = 策略下沉为 adapter 的 resume 语义)。
2. **事件去重的 final-only 隐式假设**(`async_event_text` `gateway.rs:1341-1362` / `event_text` `:1468-1485`):「drop ItemUpdated delta、只投 ItemCompleted」是跨 vendor 的隐式 cadence 假设 —— 把最终文本只放 ItemUpdated 的未来 vendor 会被**静默丢消息**,且无编译信号,恰是本文要杜绝的静默降级病。处置:**本版钉契约**(§1.5,零代码:把 final-only 写成 `events()` 的文档化契约,gateway 两个 helper 成为契约的执行者而非隐式假设)。

---

## 1. Harness:从 trait object 到长生命周期引擎

### 1.1 实例化纪律:每 daemon × vendor 恰好一个(F10 的隐藏硬配套)

「harness」要成为真概念,先得**存在**——今天它没有稳定的实例:`default_adapter_factory` 每次调用 new 一个 adapter,gateway 在 `start_session` / `load_state` / `resume_restored_sessions` 三处按需调 factory,得到的是一批互不相干的 trait object。UDS 时代这无伤大雅(N 个实例 = N 条到同一外部 daemon 的连接);**F10 改默认 stdio 后语义突变**:每个 `CodexAppServerAdapter` 实例的 `client()` 各自 spawn 一个 `codex app-server` 子进程(`codex_jsonrpc.rs:108-127`)——**单 daemon 内两个 codex session 就是两个子进程**,与「每 bot 恰好一个常驻子进程」的 PRD 预期直接冲突(跨 daemon 重启不泄漏:旧子进程 `kill_on_drop` 随父进程回收;问题在单 daemon 存续期内的实例×子进程膨胀)。修缮(5 行级,语义关键):

```rust
pub fn default_adapter_factory() -> AdapterFactory {
    let claude = Arc::new(ClaudeTuiAdapter::new());
    let codex  = Arc::new(CodexAppServerAdapter::new());   // harness 实例 = 进程级单例
    Arc::new(move |vendor| match vendor {
        AgentVendor::Claude => claude.clone(),
        AgentVendor::Codex  => codex.clone(),               // Clone 共享 inner(已是 Arc 字段)
    })
}
```

- 结果:**每 daemon 恰好一个 codex app-server 子进程**,所有 codex session 走 `threadId` 多路复用(app-server 本就是多 thread 服务,`thread/start` 带各自 cwd,跨项目成立);§1.3 的 tracker / override map 天然全局一份。
- **红线核对**:codex 子进程是 harness 的 **transport 进程,不是会话** —— thread 状态由 Codex 落盘 `CODEX_HOME`,session resume-by-id 重连即回;daemon 退出子进程随 `kill_on_drop` 死,不违反「永不主动 kill 长 session」(红线保护的是 session,不是 harness 的管道)。精度限定:resume 恢复的是已落盘历史;子进程死亡瞬间**进行中的 turn 会被中断且不自动续跑**(需用户重发)——这是崩溃恢复语义,非红线所指的主动 kill。
- 故障半径:子进程崩 ⇒ 全部 codex session 同时 submit error(IM 可见)⇒ 经 `forget_client()` 重拨自愈。注意:`forget_client`(`codex_app_server.rs:265-267`)**当前全仓零 caller**,是本版需要补接的自愈钩子(submit 错误路径调用),不是既有兜底。可接受;要隔离再升 per-bot map,本版不做。Claude 侧零影响(unit struct)。

### 1.2 transport 是 harness 的构造期属性(对 PRD §3-F10「最小改 client()」的修订)

今天 `client()` 在调用点内嗅**两个** env(`SOCKET` × `TRANSPORT`)走四态矩阵;fix A 落地后两格是死的。收敛为:

```rust
pub enum CodexTransport { Stdio { program: String }, Socket { path: PathBuf } }
/// 纯函数,单轴:CCTEAM_CODEX_APP_SERVER_SOCKET 设了 ⇒ Socket(自带 daemon 的 power user);
/// 没设 ⇒ Stdio(CCTEAM_CODEX_BIN | "codex")。
pub fn resolve_codex_transport() -> CodexTransport
```

构造期解析一次存 harness 实例;`client()` 只剩 `match`;UDS 连接代码保留(PRD don't-touch 守住)。**删 `APP_SERVER_TRANSPORT_ENV`**(pre-v1 不留 alias,grep 迁移;纯 stdio 测试删 env 即得默认行为)。两处连带:

- `ThreadHandle.raw_extras.transport`(`:364`)改写 resolved 真值 —— 今天它无脑回显 env 或 `"uds"`,F10 后会说谎。
- **real-binary smoke 测试**(`tests/codex_app_server_test.rs:48-56`)现用 `APP_SERVER_TRANSPORT_ENV` 做 stdio/UDS 分支驱动 —— 删 env 后需改写为按 `CCTEAM_CODEX_APP_SERVER_SOCKET` presence 分支(对齐单轴),否则 UDS 臂无法再被驱动。
- dev-plan「默认无 env 选 stdio」断言直接打在纯函数上,不拉进程。

### 1.3 harness 拥有 vendor 级运行态:`CodexThreadTracker`(对 PRD §3-D2.4 的修订)

PRD 写「在 `events()` 流里消费 tokenUsage → 维护缓存」。先校正一个 PRD 前提:PRD §3-D2.4/§5.2 说「ccteam 现在不消费该通知」**不准** —— `events()` 已经消费它:经 `build_codex_notification_progress_line`(`codex_app_server.rs:502→:1152-1175`)镜像进 progress.jsonl。**真实 gap 是「无可同步查询的内存缓存」**,不是「未订阅」。W3 第一步不要白做「确认未消费」,更不要误删既有镜像逻辑。

而把内存缓存的副作用挂在 `events()` 流内,有三个实底问题:

1. **多订阅重算**:`events(h)` 每次调用都对 broadcast 新开一个订阅(`codex_app_server.rs:408-541`),副作用随订阅数翻倍 —— 生产期 event pump 一条流(`gateway.rs:733`),`submit_to_current` 的无-sink **测试**路径每 turn 再开一条(`gateway.rs:973`;两者按 `event_sink` 二值互斥,生产不并发,但测试路径单流内即按 turn 重复消费);active-turn map 一旦也挂上去,任何多订阅场景直接竞态。
2. **没人拉就没数据**:缓存更新依赖某消费者正在 poll;daemon 重启到 pump 重挂之间通知丢失。
3. **职责错位**:`events()` 是面向 gateway 的展示翻译层,塞业务状态副作用,和「progress 写散在各处」是同一类病(当年靠 progress_bridge 单一权威治住)。

且 D2.2(active-turn map:`turn/steer` 选路 + `/interrupt` 的 expectedTurnId)需要**完全相同的机制**。harness 既然是长生命周期引擎(§1.1),状态就该它自己养 —— 一个常驻 dispatcher,生命周期绑 client:

```rust
// codex_app_server.rs
#[derive(Default)]
struct CodexThreadTracker { threads: HashMap<String, ThreadLive> }   // Arc<Mutex<…>>,per-session 条目
struct ThreadLive {
    usage: Option<ContextUsage>,    // ← thread/tokenUsage/updated(total + modelContextWindow)
    active_turn: Option<String>,    // ← turn/started 置;turn/completed + error(terminal) 清
    model: Option<String>,          // ← thread/start 响应 / spawn ctx
}
// client() 握手成功后 spawn 一次:client.subscribe() → 循环更新 tracker。
// broadcast Closed ⇒ task 自然退出;forget_client 重拨 ⇒ 新 dispatcher。
```

`handle_directive`(`/status`、steer-vs-start 选路、`/interrupt`)与 `thread_status()`(P3-Codex)都**读 tracker**;`events()` 退回纯翻译(progress_bridge 镜像照旧)。骨架先例都现成:`bridges` map(`:116`)证明 per-thread 的 harness 状态成立,`maybe_start_codex_typed_event_tap`(`:352`)证明独立订阅 task 成立。D2.1 的 per-session override map、D2 的 skills/list 缓存同属此层,同 keyed-by-thread_id 模式。

> **skills 缓存的前置依赖(评审实证)**:PRD 的「`skills/changed` 通知失效缓存」机制当前**断链** —— `skills/changed` 被列入 initialize 握手的 `optOutNotificationMethods`(`codex_app_server.rs:240`),服务端会抑制该通知,失效永不触发 ⇒ 缓存只增不失效、会话中途 skill 增删改 stale-forever。W3 实现 skills 缓存前必须:把 `skills/changed` 移出 opt-out 列表(`:228-244`)+ dispatcher 消费之(否则它会落进 `translate_notification` 的 `other =>` warn 分支刷日志);若改走 TTL/每次重取方案,则需同步修订 PRD/dev-plan 所述失效机制。
>
> D2.4「净新增工作」定性不变 —— 消费点从流内挪到 dispatcher,换来一次写、处处读、无重算。

### 1.4 命令面解释权:`handle_directive`(D1 形状背书)

harness 对「自己 vendor 的命令长什么样」有唯一解释权 —— PRD D1 的类型形状(`Directive` / `DirectiveOutcome{Turn,Done,NeedsChoice,Rejected,Redirect}`)**背书不改**,两条强调:

- **无 default impl**(对齐「vendor enum 无 default」红线):「静默降级为普通文本」这类 bug 在类型层杜绝。
- 放 `HarnessAdapter` 本体而非独立 sub-trait —— 考虑过 `DirectiveSurface` 拆分,弃:`Arc<dyn HarnessAdapter>` 调 sub-trait 要 trait-object upcast 折腾,代价只是非 chat adapter 各写一行 stub。

本版 trait 全部增量 = **两个方法**:`handle_directive`(本节)+ `thread_status`(§2.3)。**trait churn 一次性发生在 W1**(全 workspace 的 impl —— Fake/claude_tui/codex_app_server/codex_exec/claude_bg 及各测试 fake —— 一次补齐 stub,W2–W4 只换实现体)。**bg/非 chat adapter 的拒绝姿势钉死**(现状先例 `codex_exec.rs:567-575` 对 SystemDirective 返 `Err(SubmitFailed)`,易误导):

- `handle_directive` bg → `Ok(DirectiveOutcome::Rejected{reason})`(**合法 outcome,非 Err**——「这个 harness 不支持交互命令面」是回答,不是失败);
- `thread_status` bg → `Ok(ThreadStatus::default())`(显式空,意图可 grep;default impl 才是被禁的静默)。

### 1.5 既有方法的契约钉死(零代码,纯文档化)

把 §0「已知残留」第 2 条就地治本:在 `HarnessAdapter::events()` 的 rustdoc 写明 **final-only 契约** —— adapter 必须把最终 agent 文本恰好一次地以 `ItemCompleted{AgentMessage}` 发出;`ItemUpdated` 只承载增量/presentation,消费者有权丢弃。gateway 的 `async_event_text`/`event_text` 从「隐式跨 vendor 假设」变成「契约的执行者」;未来 vendor 照契约实现 `events()` 即不会被静默丢消息。零代码改动,W1 顺手落。

---

## 2. Session:三个新刻面

session(`ThreadHandle` + `GatewaySession`,resume-by-id)从「能收发文本的句柄」补全为「有属性、可被指令、能反问」的对象。三刻面的中立类型全部住 `ccteam-harness/adapter.rs` 层(与 `ThreadEvent`/`ApprovalIR` 同层)。

### 2.1 刻面一:可被指令(D1/D5/P1)

**命令有两类,分界即概念边界**:

- **管 session 的命令**(创建/切换/列表:`/new /use /cd /sessions /projects /newproject /pair /help`)→ 属 **chat/gateway 概念**(在路由层拦截,不经 `GatewaySession`/`ThreadHandle`),**单表收敛**。今天名单散在三处手抄(`is_gateway_command` `gateway.rs:380-385`、`handle_command` match `:461-551`、即将到来的菜单/`/help`),必漂移:

```rust
// gateway.rs —— 唯一名单,四个消费点(guard / dispatch / `/help` / 菜单注册)全部派生
pub struct GatewayCommandSpec { pub name: &'static str, pub arg_hint: Option<&'static str>,
                                pub help: &'static str, pub in_menu: bool }
pub const GATEWAY_COMMANDS: &[GatewayCommandSpec] = &[ /* …8 条… */ ];
```

```rust
// transport/mod.rs —— 菜单注册是 channel 能力,默认 no-op(同 max_message_len 先例,:187)
pub struct CommandSpec { pub name: String, pub description: String }
#[async_trait] pub trait Channel {
    /* 现有方法 */
    async fn register_commands(&self, _cmds: &[CommandSpec]) -> anyhow::Result<()> { Ok(()) }
}
```

  `setMyCommands` 只活在 Telegram override;daemon 启动一行 `for ch in channels { ch.register_commands(...) }`(PRD P1「照 api_url POST 模式 startup 调一次」按此修正为走 trait,不写 telegram 特例)。带参命令 `in_menu=false`。`/help` = 这条分界的可发现性投影:列 gateway 命令 + 一句「其余 `/` 命令交给当前 session 的 agent」。

- **给 session 的命令**(`/compact /model /review /status …`)→ 解析成 `Directive` 交 harness 解释(§1.4),gateway 对语义零知识。**判定规则上移路由层**(替代 `turn_input_for_session` vendor 分支):mention 解析后、提交前 —— `trim` 后以 `/` 开头**且单行** ⇒ 走 `handle_directive`;**多行消息一律按普通文本**(防粘贴 `/home/...` 路径、代码块被误判;今天 Codex 静默吃掉,补全后误判会变成 Rejected 打扰用户,此规则必须写死并测试)。

**输入面随之提纯**:`TurnInput::SystemDirective` 删除(PRD 已定),TurnInput 退回纯内容(UserText/Artifact/Image/ToolResult)。D5 通道 1/2(prompt 透传 + BRIDGE_SAFE)在 Claude adapter 内部 = `self.submit_turn(h, TurnInput::UserText("/{name} {args}"))` —— 与旧路径 wire 等价(`claude_tui.rs:524-529` 本就是 `format!("/{d}")`),不开第二条发送路径,零知识透传红线原样。**caller 名单分层**(grep 已核,防误判工作量):真实构造/匹配点 = `codex_app_server.rs`(submit_turn + `turn_input_to_items`)/ `codex_exec.rs:567` / `claude_tui.rs:524` / `gateway.rs:1427`;test-only = `web_chat_bridge.rs:261`(测试模块 FakeAdapter stub)+ harness 三个 tests;doc-comment 提及(改注释即可)= `session_recovery.rs` / `turns_mirror.rs` / `ccteam-flow/workflow.rs` / `adapter.rs` 本体。

### 2.2 刻面二:能反问(D3/D4/D6,ApprovalIR 交互前身)

「session 正在等用户做一个选择」成为 session 的一等状态。**单飞 per (chat, session)**;与谁发起(harness 弹窗 / agent 提问 / 未来批准)、在哪渲染(TG 按钮 / WS chips / 纯文本编号)双向解耦。

**注册表(D6 的关键)**。PRD D3 说两类 producer「同一路径」,但两者**应答去向**根本不同:harness `NeedsChoice` 的答案要**重入** `handle_directive(原 Directive + choice)`;D6 hook 的答案要送回**正阻塞在 mcp.sock 上等回包的 hook 进程**。若 Wave 1 把 pending-picker 写成「只存 Directive 的 map」,Wave 2 的 D6 就得另起一套挂起/超时/归一。统一为:

```rust
// ccteam-im —— session 的挂起交互态(独立模块,独立锁;见下「归属与 ingress」)
enum InteractionOrigin {
    /// harness NeedsChoice:重入 = handle_directive(原 directive + choice)
    Directive { session_id: String, directive: Directive },
    /// hook / 未来 approval:应答经 oneshot 送回等待方(mcp.sock handler 持 Receiver)
    External { reply: tokio::sync::oneshot::Sender<ChoiceSelection> },
}
struct PendingInteraction { prompt: ChoicePrompt, origin: InteractionOrigin, expires_at: Instant }
// key = (ChatKey, session_id),单飞;TTL 与 D6 hook 超时同一常量
```

**归属与 ingress(评审 must-fix:这不是「纯组装」,是 W2 的真实新增 plumbing)**。现状缺口:mcp.sock handler 只持一个**单向** sink(`handle_mcp_socket_connection` 仅拿 `Option<GatewayEventSink>`,`ccteam-cli/main.rs:2180-2184`),`GatewayEvent`(`gateway.rs:118-135`)只有 Answer/Progress 两种 kind —— **够不着**住在 `Arc<Mutex<Gateway>>` 里的任何状态。oneshot 的 Receiver 端(hook handler)与 Sender 端(应答方)之间缺一条进入路径。**定死方案:注册表是独立的 `Arc<Mutex<PendingInteractions>>`(自带锁,不复用 `Mutex<Gateway>`)**,daemon 装配时同时交给 ① gateway(NeedsChoice 时注册 Directive-origin、入站应答时 resolve)与 ② mcp.sock handler(D6 时注册 External-origin 后**锁外** `await` oneshot)。选独立锁而非 `GatewayEvent` 新增 RegisterInteraction variant,理由:`GatewayEvent` 是出站投递管道(消费者是 channel 投递 loop),把注册请求塞进出站管道方向就错了;独立锁还顺带满足锁纪律(§7-1):600s 级的 External await 永远不持任何 gateway 锁。

- **D6 链路(W2 工作项,非组装)**:hook 经 mcp.sock 发请求 → handler 反查 chat(`GatewaySession.project/role` 已有,补 `find_chat_for_bot(slug, role)` 读 `reply_to`)→ 注册 `External{oneshot}` → 经既有 GatewayEvent sink 出站渲染(带 options)→ 锁外 `await` oneshot(带超时)→ 回写 sock → hook stdout `allow + updatedInput`。不开新 socket、不 file-watch(对齐「live agent→daemon 走 mcp.sock + 内存 sink」既有决策)。配独立端到端验收:注册 → 入站 resolve → 回写 sock(§8-8)。
- **三形态归一只写一次**:按钮回调 / 纯数字短回复 / 完整 arg-form → `ChoiceSelection` → 按 origin 分发。
- **抢占策略(PRD 未写,定死)**:同 key 再来一个 ⇒ 驱逐旧的 + 出「已取消上一个选择」;被驱逐的 `External` 立即按超时语义 resolve(hook 走 deny-with-reason),**绝不静默吞**。数字短回复只命中当前 pending。
- **token/data 纪律**:`ChoicePrompt.token` 由 producer mint,**≤16 字节 ASCII 且不含 `:`**(注册表按第一个 `:` split);`idx` 为十进制选项序号,反查前校验上界。注册表校验 token,stale 回「该选择已过期」。

**传输贯通(channel 轴只加搬运字段,不加概念;此处对 PRD 构成第 4 处修订,见 §4)**。PRD/dev-plan 明写 `SendMessage.options: Vec<ChoiceOption>`(harness 中立类型直入 transport,`prd.md:145` / `dev-plan.md` W1 落点);本文改为 **transport 本地 `MessageOption`**,理由:channel 轴不引 harness 轴类型(呼应 PRD §4 双轴解耦自己的话)。注意这是**分层纪律而非编译屏障** —— `ccteam-im` 本就依赖 `ccteam-harness`(gateway.rs:17 已 import),transport 不 import 靠 review 纪律守(可后续加 dep-lint)。三个洞补齐(全部 `#[serde(default)]`,grep 全 impl:tg/slack/discord/mock):

```rust
// transport/mod.rs —— channel 只见「展示 + 不透明回执」
pub struct MessageOption { pub data: String, pub label: String }   // data 恒为 "{token}:{idx}"
pub struct SendMessage   { /* 现有 */, #[serde(default)] pub options: Vec<MessageOption> }
pub struct ChoiceReply   { pub data: String }
pub struct ChannelMessage{ /* 现有 */, #[serde(default)] pub selection: Option<ChoiceReply> }
// gateway.rs —— 同步回复不再是裸 String;异步出站同构
pub struct GatewayReply { pub text: String, pub options: Vec<MessageOption> }   // From<String> 平滑迁移
// GatewayEvent 加 options: Vec<MessageOption>(External producer 走异步出站也能带按钮)
```

`data = "{token}:{idx}"` 恒短(≤20B)永不爆 Telegram callback_data 的 64 字节硬限;真实 option id 由注册表按 idx 反查,语义不出 gateway。入站:TG `listen`(`telegram.rs:446`)新增消费 `callback_query` → 填 `selection`(+`answerCallbackQuery` 消 loading);mock 同构;数字回复不动 transport。

**web 是平行 plumbing,不是「同构白嫖」(评审 must-fix)**。web ⊥ im 红线下,web 走自己的 wire 类型:`WebSendMessage`(`chat_protocol.rs:69-74`,无 options)/ `WebChannelMessage`(`:57-65`,无 selection、**也无 attachments**),`web_chat_bridge.rs` 手工逐字段拷贝 —— `to_im_message`(`:110-120`)硬写 `attachments: Vec::new()`,**v0.8.4 的出站文件在 web 路径今天就被静默丢**(既有 bug,W1 顺手修)。因此 options/selection 不会被 web 自动继承,W1 必须显式做四件:扩 `WebSendMessage`(+options)、扩 `WebChannelMessage`(+selection)、`web_chat_bridge` send/to_im_message 双向字段映射补全(含 attachments)、`chat_protocol` 的 Server/Client 帧加 chips 渲染 + chip-click variant。验收 §8-4 的往返用例必须覆盖 TG 与 web 两路。

**与 ApprovalIR 的分层(防概念打架)**:`ApprovalIR`(`adapter.rs:241-279`,已有 `ApprovalKind::Question`)是**语义/风险层**;`ChoicePrompt` 是**交互层**。**不合并**:日后 per-tool-call HITL = ApprovalIR 翻译出 ChoicePrompt、走同一注册表(origin=External,与 D6 hook 同构的阻塞-oneshot 形态)。「交互前身」在代码上 = 复用注册表与回填,不是同一个类型。(flow 的 plan-level durable 审批走 progress.jsonl + decision 文件,是另一条 substrate,与本注册表无关、互不替代。)

### 2.3 刻面三:有属性(P3)

「session 是可询问的对象」:model + context 用量是 session 的属性,不是某条日志的副产品。

```rust
// adapter.rs(与 ThreadEvent 同层)
pub struct ContextUsage { pub used_tokens: u64, pub window_tokens: u64 }   // pct() 派生
pub struct ThreadStatus { pub model: Option<String>, pub context: Option<ContextUsage> }

#[async_trait] pub trait HarnessAdapter {
    /* 现有 7 方法 + handle_directive */
    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError>;  // 无 default
}
```

- **pull 模型**:两 vendor 数据流向相反 —— Claude 按需倒读 transcript(pull),Codex 收 tokenUsage 通知(push)—— 唯一能统一的是 pull 方法,push 侧由 harness 的 tracker 先缓存(§1.3)。
- **无 default impl**,与 `handle_directive` 同理:default 返回空 = `/sessions` 对新 vendor 静默缺列,正是「静默降级」病灶;bg adapter 显式 `Ok(ThreadStatus::default())`(§1.4 姿势)。
- **harness 出数,gateway 出样式**:`[1m]`→1M、200k 基线是 Claude vendor 知识,只住 claude_tui(PRD 已定:不写死 per-model 清单);Codex 窗口从 `modelContextWindow` 线上来(`codex_app_server.rs:1166-1169` 已解析),零常量;`188k / 1M (19%)` 渲染是 gateway 单点(`format_tokens` helper)。
- **同源消费**:gateway `/sessions`(P3)与 Codex `/status` 查询合成(D2.1)读同一个 `ThreadStatus`,不各拼各的。
- Claude 实现注记:context 占用 = **最后一条**带 `message.usage` 的 transcript 行的 `input+cache_creation+cache_read` 三字段和(对照 ref `tokens.ts`),**倒读 tail window**,不全量 parse(transcript 可达数十 MB)。
- Codex 实现注记(评审提醒):`turn/completed` 通知在真实 wire 上**没有 usage 字段**(`translate_notification` 注释已说明,`codex_app_server.rs:722-732`)—— tracker 的 usage **只能**来自 `thread/tokenUsage/updated` 独立通知,不要从 `TurnCompleted` 事件取。
- 既有 `HarnessSnapshot`(`adapter.rs:504-514`,bg state.json 的 model/ctx% 形状)**不动不并**:bg/web SSE 的 presentation 遗产,与 chat 路径各管各。

---

## 3. 概念 ↔ 代码映射(速查)

| 概念 / 刻面 | 中立类型 | 承载代码 | 需求 |
|---|---|---|---|
| Harness(实例纪律) | — | `default_adapter_factory` per-vendor 单例 | F10 配套 |
| Harness(transport) | `CodexTransport` | `resolve_codex_transport()` + `client()` | F10 |
| Harness(运行态) | `ThreadLive` | `CodexThreadTracker` + dispatcher task | D2.2/D2.4 |
| Harness(命令解释权) | `Directive` / `DirectiveOutcome` | trait `handle_directive`(无 default) | D1/D2/D5 |
| Harness(事件契约) | — | `events()` rustdoc final-only 契约(§1.5) | 残留治理 |
| **chat/gateway**(命令可发现性) | `GatewayCommandSpec` / `CommandSpec` | `GATEWAY_COMMANDS` + `Channel::register_commands`(此行属 chat 概念;Session 侧只有 `handle_directive` 一条入口) | P1 |
| Session(可被指令) | `Directive`(同上) | gateway 路由层单行判定 → `handle_directive` | D1 |
| Session(能反问) | `ChoicePrompt`/`ChoiceOption`/`ChoiceSelection` | 独立 `PendingInteractions`(双 origin、自带锁)+ transport `MessageOption`/`selection` + web 平行字段 | D3/D4/D6 |
| Session(有属性) | `ThreadStatus`/`ContextUsage` | trait `thread_status`(无 default)+ gateway 渲染 | P3 |

## 4. 对 PRD/dev-plan 的四处修订点(动工前需确认)

| # | PRD/dev-plan 原文 | 修订为 | 出处 |
|---|---|---|---|
| 1 | §3-D2.4「在 `events()` 流里消费 tokenUsage」+「ccteam 现在不消费该通知」 | harness 级独立 dispatcher(tracker),`events()` 保持纯翻译;并校正前提:`events()` 已镜像该通知进 progress.jsonl,真实 gap 是无内存缓存 | §1.3 |
| 2 | §3-F10「最小改 `client()`」+ 保留 transport env | 构造期 `resolve_codex_transport()` 单轴,**删** `APP_SERVER_TRANSPORT_ENV`(连带改写 real-binary smoke 的分支驱动) | §1.2 |
| 3 | §3-F10「每 codex bot 恰好一个常驻子进程」 | **每 daemon** 恰好一个(harness 单例 + threadId 多路复用) | §1.1 |
| 4 | §3-D3 / dev-plan W1「`SendMessage.options: Vec<ChoiceOption>`」 | `Vec<MessageOption>`(transport 本地类型,`data="{token}:{idx}"` opaque);`ChoiceOption` 留在 harness 层,gateway 做映射 —— channel 轴不引 vendor 轴类型 | §2.2 |

其余均为 PRD 留白处的形状收敛,不改对外行为与验收条目。

## 5. 红线一致性(对 CLAUDE.md §三)

- **No prompt injection**:§2.1 透传复用 submit_turn 零改写;反问/指令面全是 ccteam 自有交互,不进 pane/app-server prompt。
- **不解析终端输出**:全文无 capture;tracker 消费 app-server 原生通知。
- **progress.jsonl SoT**:tracker 是 harness 内存运行态(answering 展示查询),不写 progress、不替代 progress_bridge 既有镜像行。
- **永不主动 kill 长 session**:§1.1 子进程 = harness transport,非 session;session 落盘 resume-by-id 不受影响(精度限定见 §1.1:in-flight turn 中断属崩溃恢复语义,非主动 kill)。
- **文件系统是状态面**:PendingInteraction 是秒级交互运行态,daemon 重启过期重来,不入 SoT —— 与 outbound ledger(`outbound.jsonl` / `DurableOutboundRow`,`daemon.rs:842`;投递可靠性才入盘)分界一致。既有同类先例 = `nl_admin.rs:196` 的 per-target pending-confirm 内存 map。
- **vendor enum 无 default**:两个新 trait 方法双双无 default impl(bg 显式 stub 是「显式回答」,非变相 default)。
- **channel-neutral**:opaque data + `register_commands`,gateway/daemon 全程零新增 `"telegram"` 字面量。

## 6. wave 映射 delta(相对 dev-plan)

| Wave | 原计划 | 增量 |
|---|---|---|
| **W1** | D1+D3 基建 | trait 同 PR 加 `thread_status` 签名(全 workspace impl 一次补齐 stub,bg 姿势照 §1.4);`PendingInteractions` 独立模块(双 origin 类型,External 仅类型)+ daemon 装配双交;`GatewayReply` / `GatewayEvent.options` / `MessageOption` / `ChannelMessage.selection`;**web 平行 plumbing 四件**(WebSendMessage+options / WebChannelMessage+selection / bridge 双向映射含 attachments 既有 bug 修复 / chat_protocol chips 帧);`GATEWAY_COMMANDS` 表 + `Channel::register_commands` 默认实现;slash 单行判定;`events()` final-only rustdoc 契约(§1.5) |
| **W2** | D5+D6 | D6 = 真实新增 plumbing(非组装):mcp.sock handler 接 `PendingInteractions` + `find_chat_for_bot` + 锁外 oneshot await + 回写 sock;端到端验收 §8-8 |
| **W3** | F10 + D2/D4 | 第 0 步 = §1.1 + §1.2 **同一 PR**(harness 单例 + transport 单轴 + `resolve_codex_transport` 默认断言 + smoke 分支改 socket-presence + `forget_client` 补接);D2 先立 tracker 再写映射表;skills 缓存前置 = `skills/changed` 移出 opt-out(§1.3) |
| **W4** | P1/P3/P4 | P3 只剩两端 `thread_status` 实现体 + gateway 渲染;P1 只剩 TG override + startup 一行 |

## 7. 明确不做(防 scope creep)

1. **不动 gateway 锁模型**,但钉两条最小护栏:(a) **任何长阻塞交互(尤其 D6 的 600s 级 oneshot await)必须在所有 gateway 锁之外**——`handle_message` 全程持 `Mutex<Gateway>`(`daemon.rs:473-484`),await 落锁内会卡死整个 daemon 入站;`PendingInteractions` 独立锁(§2.2)即为此设。(b) 同步 vendor RPC(`handle_directive` 内)在锁内跨 chat 串行化,记为**已知退化**,沿用 submit 同款超时包裹(`CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS`)兜底;锁粒度重构是另一版本的事。
2. **不持久化 per-session override map**(PRD 已接受重启丢失;日后挂 `SavedGatewaySession` 扩展字段,一跳可达)。
3. **不动 `events()` 签名 / `ThreadEvent` schema**:tokenUsage 不进 CanonicalEvent(走 pull),避免 event schema 为展示需求漂移(§1.5 只加 rustdoc 契约,不动签名)。
4. **不合并 `HarnessSnapshot` 与 `ThreadStatus`**(§2.3)。
5. **不做 per-bot codex 子进程隔离**(§1.1,故障半径可接受,留升级路径)。
6. **不做任何改名**:`HarnessAdapter`/`ThreadHandle`/`GatewaySession` 名字已与概念对齐,补语义不换皮。
7. **不下沉 resume 恢复策略**(§0 残留 1):gateway 的 vendor-match 恢复策略本版保留(加 vendor 时有编译信号,非静默);升级路径 = 把「resume 失败→reattach/recreate」策略收进 adapter 的 resume 语义,留给 flow/下一 minor。

## 8. 新增验收(概念锁定测试,叠加 PRD §5.2)

1. `resolve_codex_transport`:无 env ⇒ `Stdio`;socket env ⇒ `Socket`(取代 dev-plan「daemon.rs:1137 补 transport 断言」的进程级写法)。
2. **harness 单例**:factory 两次取 Codex arm ⇒ `Arc::ptr_eq` 同一实例(§1.1 防回归)。
3. `ThreadHandle.raw_extras.transport` == resolved 真值(无 socket env ⇒ `"stdio"`)——补回 PRD §3-F10 要的钉死。
4. `data="{token}:{idx}"` 往返 ×2 路:TG callback 与 web chip-click 各一条 → 归一出的 `ChoiceSelection.ids` = 真实 option id(idx 反查正确;含 idx 越界拒绝)。
5. PendingInteraction:Directive origin 重入带 choice;External origin oneshot 收到 selection;TTL 过期 / 被抢占 ⇒ External 收终止信号(W2 映射 deny-with-reason)。
6. tracker:scripted peer 喂 `turn/started`→`tokenUsage`→`turn/completed` ⇒ `thread_status` 反映 usage(且 usage 来源断言为 tokenUsage 通知,非 TurnCompleted)、active-turn 置/清正确;**同时开两条 `events()` 流,tracker 不重算**(钉死 §1.3 问题 1)。
7. `thread_status` Claude:fake transcript 带/不带 `[1m]` ⇒ 1M / 200k 分母。
8. **D6 端到端**:模拟 hook 经 mcp.sock 注册 External → IM 入站应答 → oneshot resolve → sock 收到回写(W2)。
9. 多行 `/` 开头消息 ⇒ 按 UserText 提交,不产生 Rejected(§2.1)。
10. web bridge 双向映射:含 attachments(回归修复)与 options/selection 的字段保真往返。
