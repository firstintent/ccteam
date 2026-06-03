# v0.8.5 架构小范围重构 —— 三个核心概念 + 两处支撑修缮

> 配套 `prd.md`(实现规约)+ `dev-plan.md`(wave 编排)。本文回答:**D1–D6 / P1 / P3 / F10 这批需求落进现有代码,应该抽象出什么概念、改哪些接缝**。
>
> **概念纪律:全版收敛为 3 个核心概念** —— `Directive`(命令面)、`Interaction`(挂起交互)、`ThreadStatus`(会话状态)。三个概念吃掉 v0.8.5 全部功能性需求;概念之外只剩两处**不引入新概念**的支撑修缮(F10 的 transport/factory 生命周期)。任何实现物必须能挂到这 3+2 之下,挂不上的就是 scope creep。
>
> 范围纪律:只动 v0.8.5 触及的接缝。不动 crate 拓扑(`core→harness→cost`)、不动 progress.jsonl SoT、不动 ProcessBackend 轴、不动 bg 路径。代码引用基于 HEAD(`5f2eb4b`)。

---

## 0. 总览

| 核心概念 | 一句话 | 中立类型(住 `ccteam-harness/adapter.rs` 层) | 表面 | 吃掉的需求 |
|---|---|---|---|---|
| **① Directive — 命令面** | 一条指令的完整生命周期:slash 文本 → `Directive` → 五种 `DirectiveOutcome`;每个 vendor **显式**声明自己的命令面,gateway 纯路由 | `Directive` / `DirectiveOutcome` | trait 方法 `handle_directive`(无 default) | D1 / D2 / D5 / P1(菜单与 `/help` 是命令面的可发现性投影)|
| **② Interaction — 挂起交互** | 一次等待用户选择的事务:`ChoicePrompt` → 渲染 → 回填 → 归一 `ChoiceSelection` → 按 **origin** 分发;producer 无关、channel 无关 | `ChoicePrompt` / `ChoiceOption` / `ChoiceSelection` | gateway `PendingInteraction` 注册表(双 origin)| D3 / D4 / D6;ApprovalIR 交互前身 |
| **③ ThreadStatus — 会话状态** | session 是可询问的:model + context 用量;**pull** 统一两 vendor 相反的数据流向,adapter 出数、gateway 出样式 | `ThreadStatus` / `ContextUsage` | trait 方法 `thread_status`(无 default) | P3 / D2.4(tokenUsage 消费)/ D2.1 `/status` 同源 |
| 支撑修缮(无新概念) | F10 配套:Codex transport 单轴化 + adapter factory 单例化 | `CodexTransport`(adapter 私有)| — | F10 + 子进程泄漏缺口 |

三个概念在 trait 上的投影恰好对称:`HarnessAdapter` 本版**只新增两个方法**(`handle_directive` / `thread_status`),Interaction 不上 trait —— 它是 `NeedsChoice` outcome 在 gateway 侧的对偶物。

---

## 1. 概念① Directive —— 命令面

### 1.1 定义与不变量

「用户向一个 session 发出的指令」第一次成为一等概念:gateway 把 slash 文本解析成 `Directive{name, args, choice?}`,adapter 用 `DirectiveOutcome` 五值穷尽回答它的归宿(成 turn / 即时回执 / 要选择 / 显式拒绝 / 重定向)。**不变量**:

- **每个 vendor 显式声明命令面**:`handle_directive` 无 default impl(对齐「vendor enum 无 default」红线)——「静默降级为普通文本」这类 bug(今天 `turn_input_for_session` 的病,`gateway.rs:1422-1431`)在类型层杜绝。
- **gateway 对命令语义零知识**:只认自己的 `GATEWAY_COMMANDS` 表,其余一律转 `Directive` 给 adapter;新 vendor 只实现 `handle_directive`,不感知任何 channel。
- **开放集透传不变**:Claude prompt 型命令照旧零知识透传(No prompt injection 红线原样)。

PRD D1 的类型形状(`Directive` / `DirectiveOutcome{Turn,Done,NeedsChoice,Rejected,Redirect}`)**背书不改**。`handle_directive` 放 `HarnessAdapter` 本体而非独立 sub-trait —— 考虑过 `DirectiveSurface` 拆分,弃:`Arc<dyn HarnessAdapter>` 调 sub-trait 要 trait-object upcast 折腾,代价只是 bg 两个 adapter 各写一行全拒。

### 1.2 gateway 自有命令是同一概念的 gateway 层(P1 落点)

gateway 自有命令(`/pair /new /use /cd /sessions /projects /newproject /help`)是命令面概念在 gateway 层的那一截,今天却散在三处手抄名单:`is_gateway_command`(`gateway.rs:380-385`)、`handle_command` match(`:461-551`)、(即将)P1 的菜单注册与 `/help` 文案。**收敛为单表,四个消费点全部派生**:

```rust
// gateway.rs —— 唯一名单
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

guard、dispatch、`/help` 渲染、`register_commands` 入参(`in_menu` 过滤,带参命令不进菜单)同源。`setMyCommands` 只活在 Telegram override 里;daemon 启动一行 `for ch in channels { ch.register_commands(...) }`,不写 telegram 特例(PRD P1「照 api_url POST 模式 startup 调一次」按此修正为走 trait)。新 channel(飞书/钉钉)各自决定菜单形态,gateway 零改动。

### 1.3 概念的减法面:`TurnInput` 提纯

Directive 成为概念后,`TurnInput::SystemDirective` 删除(PRD 已定),TurnInput 退回**纯内容**(UserText / Artifact / Image / ToolResult)。补三个实现规约:

1. **判定上移 gateway 路由层**(替代 `turn_input_for_session` 的 vendor 分支):mention 解析后、提交前 —— `trim` 后以 `/` 开头**且单行** ⇒ 走 `handle_directive`;**多行消息一律按普通文本**(防粘贴 `/home/...` 路径、代码块被误判;今天 Codex 静默吃掉,概念落地后误判会变成 Rejected 打扰用户,此规则必须写死并测试)。
2. **D5 通道 1/2 在 Claude adapter 内部 = `self.submit_turn(h, TurnInput::UserText("/{name} {args}"))`**:与旧 `SystemDirective` 路径 wire 等价(`claude_tui.rs:524-529` 本就是 `format!("/{d}")`),不开第二条发送路径。
3. **caller 全量名单**(grep 已核):`claude_tui.rs` / `codex_app_server.rs`(submit_turn + `turn_input_to_items`)/ `codex_exec.rs` / `session_recovery.rs` / `turns_mirror.rs` / `gateway.rs` / **`ccteam-cli/src/web_chat_bridge.rs:261`**(易漏的 cli 层)/ `ccteam-flow/src/workflow.rs` / harness 三个 tests。

---

## 2. 概念② Interaction —— 挂起交互

### 2.1 定义与不变量

「ccteam 正在等这个用户做一个选择」成为一等概念:一次挂起交互 = `prompt + origin + TTL`,与**谁发起**(adapter 弹窗 / agent 提问 / 未来批准)和**在哪渲染**(TG 按钮 / WS chips / 纯文本编号)双向解耦。**不变量**:

- **producer 无关**:任何来源的提问走同一注册表、同一渲染、同一回填归一。
- **channel 零语义**:channel 只搬运不透明的 `data` 与 `label`,真实选项 id 不出 gateway。
- **单飞 per (chat, session)**;**任何终结都显式**(超时、被抢占、stale token 全部有去向,绝不静默吞)。

### 2.2 PendingInteraction:双 origin,一套机制(D6 的关键)

PRD D3 说两类 producer「同一路径」,但两者**应答去向**根本不同:adapter `NeedsChoice` 的答案要**重入** `handle_directive(原 Directive + choice)`;D6 hook 的答案要送回**正阻塞在 mcp.sock 上等回包的 hook 进程**。若 Wave 1 把 pending-picker 写成「只存 Directive 的 map」,Wave 2 的 D6 就得另起一套挂起/超时/归一 —— 全部重复。统一为:

```rust
// ccteam-im/src/gateway.rs
enum InteractionOrigin {
    /// adapter NeedsChoice:重入 = handle_directive(原 directive + choice)
    Directive { session_id: String, directive: Directive },
    /// hook / 未来 approval:应答经 oneshot 送回等待方(mcp.sock handler 持 Receiver)
    External { reply: tokio::sync::oneshot::Sender<ChoiceSelection> },
}
struct PendingInteraction { prompt: ChoicePrompt, origin: InteractionOrigin, expires_at: Instant }
// key = (ChatKey, session_id),单飞;TTL 与 D6 hook 超时同一常量
```

- **D6 链路变成纯组装**:mcp.sock handler 收 hook 请求 → 反查 chat(`GatewaySession.project/role` 已有,补 `find_chat_for_bot(slug, role)` 读 `reply_to`)→ 注册 `External{oneshot}` → 出站渲染 → `await` oneshot(带超时)→ 回写 sock → hook stdout `allow + updatedInput`。不开新 socket、不 file-watch(对齐「live agent→daemon 走 mcp.sock + 内存 sink」既有决策)。
- **三形态归一只写一次**:按钮回调 / 纯数字短回复 / 完整 arg-form → `ChoiceSelection` → 按 origin 分发。
- **抢占策略(PRD 未写,定死)**:同 key 再来一个 ⇒ 驱逐旧的 + 出「已取消上一个选择」;被驱逐的 `External` 立即按超时语义 resolve(hook 走 deny-with-reason)。数字短回复只命中当前 pending。
- **token 纪律**:`ChoicePrompt.token` 由 producer mint,**≤16 字节 ASCII**;注册表校验,stale 回「该选择已过期」。

**与 ApprovalIR 的分层(防概念打架)**:`ApprovalIR`(`adapter.rs:241-279`,已有 `ApprovalKind::Question`)是**语义/风险层**;`ChoicePrompt` 是**交互层**。**不合并**:日后 HITL = ApprovalIR 翻译出 ChoicePrompt、走同一注册表(origin=External)。「交互前身」在代码上 = 复用注册表与回填,不是同一个类型。

### 2.3 传输贯通:opaque data 把「channel 零语义」落成类型事实

PRD 只说 `SendMessage.options`,有三个洞:① 出站选项字段长什么样;② 入站 callback 的中立表示(`ChannelMessage` 今天只有 text+attachments,`transport/mod.rs:62-82`);③ 同步回复路径载体(`handle_message` 返回 `Vec<String>`,daemon 在 `daemon.rs:487-490` 包成 SendMessage —— 纯文本管道塞不下选项)。且 Telegram `callback_data` 有 **64 字节**硬限,model id 直接当 data 会超。补齐(全部 `#[serde(default)]`,grep 全 impl:tg/slack/discord/mock/ws):

```rust
// transport/mod.rs —— channel 只见「展示 + 不透明回执」
pub struct MessageOption { pub data: String, pub label: String }   // data 恒为 "{token}:{idx}"
pub struct SendMessage   { /* 现有 */, #[serde(default)] pub options: Vec<MessageOption> }
pub struct ChoiceReply   { pub data: String }
pub struct ChannelMessage{ /* 现有 */, #[serde(default)] pub selection: Option<ChoiceReply> }
```

```rust
// gateway.rs —— 同步回复不再是裸 String;异步出站同构
pub struct GatewayReply { pub text: String, pub options: Vec<MessageOption> }   // From<String> 平滑迁移
// GatewayEvent 加 options: Vec<MessageOption>(External producer 走异步出站也能带按钮)
```

- **`data = "{token}:{idx}"`**(idx=选项序号):恒短(≤20B),永不爆 64B;真实 option id 由注册表按 idx 反查。
- **harness 的 `ChoiceOption` 不穿透 transport**:gateway 做 `ChoicePrompt → Vec<MessageOption>` 映射 —— channel 轴在**编译期**就 import 不到 vendor 轴类型。
- 入站:TG `listen`(`telegram.rs:446`)新增消费 `callback_query` → 填 `selection`(+`answerCallbackQuery` 消 loading,channel 内部);WS chips / mock 同构;数字回复不动 transport。

---

## 3. 概念③ ThreadStatus —— 会话状态

### 3.1 定义与不变量

「session 是可询问的对象」:model + context 用量(绝对值+窗口)是 session 的属性,不是某条日志的副产品。**不变量**:

- **pull 模型**:两 vendor 数据流向相反 —— Claude 按需倒读 transcript(pull),Codex 收 tokenUsage 通知(push)——唯一能统一的是 pull 方法,push 侧自己先缓存(§3.3)。
- **adapter 出数,gateway 出样式**:`[1m]`→1M、200k 基线是 Claude vendor 知识,只住 claude_tui;Codex 窗口从 `modelContextWindow` 线上来(`codex_app_server.rs:1166-1169` 已解析),零常量;`188k / 1M (19%)` 渲染是 gateway 单点(`format_tokens` helper)。
- **同源消费**:gateway `/sessions`(P3)与 Codex `/status` 查询合成(D2.1)读同一个 `ThreadStatus`,不各拼各的。

### 3.2 trait 表面

```rust
// adapter.rs(与 ThreadEvent 同层)
pub struct ContextUsage { pub used_tokens: u64, pub window_tokens: u64 }   // pct() 派生
pub struct ThreadStatus { pub model: Option<String>, pub context: Option<ContextUsage> }

#[async_trait] pub trait HarnessAdapter {
    /* 现有 7 方法 + handle_directive */
    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError>;  // 无 default
}
```

- **无 default impl**,与 `handle_directive` 同理:default 返回空 = `/sessions` 对新 vendor 静默缺列,正是「静默降级」病灶。bg adapter 显式 `Ok(ThreadStatus::default())` 一行,意图可 grep。
- **trait churn 一次性发生在 W1**:两个新方法同 PR 进 trait,Fake/claude_tui/codex_app_server/codex_exec/claude_bg 一次补齐 stub;W3/W4 只换实现体(对 dev-plan 的微调,原计划 P3 全压 W4)。
- Claude 实现注记:context 占用 = **最后一条**带 `message.usage` 的 transcript 行的 `input+cache_creation+cache_read` 三字段和(对照 ref `tokens.ts`),**倒读 tail window**,不全量 parse(transcript 可达数十 MB)。
- 既有 `HarnessSnapshot`(`adapter.rs:504-514`,bg state.json 的 model/ctx% 形状)**不动不并**:bg/web SSE 的 presentation 遗产,与 chat 路径各管各。

### 3.3 Codex 供给侧:CodexThreadTracker(对 PRD §3-D2.4 的修订)

PRD 写「在 `events()` 流里消费 tokenUsage → 维护缓存」。但 `events(h)` 是**按 handle 创建的翻译流**(`codex_app_server.rs:408-541`),把缓存副作用挂上去有三个实底问题:

1. **双算**:`submit_to_current` 的无-sink 测试路径每 turn **再开一条** `events()` 流(`gateway.rs:973`),event pump 另有一条 —— 两条流都带副作用,active-turn map 一旦也挂上去直接竞态。
2. **没人拉就没数据**:缓存更新依赖某消费者正在 poll;daemon 重启到 pump 重挂之间通知丢失。
3. **职责错位**:`events()` 是面向 gateway 的展示翻译层,塞业务状态副作用,和「progress 写散在各处」是同一类病(当年靠 progress_bridge 单一权威治住)。

且 D2.2(active-turn map:`turn/steer` 选路 + `/interrupt` 的 expectedTurnId)需要**完全相同的机制**。两个需求一个答案 —— adapter 内一个常驻 dispatcher,生命周期绑 client:

```rust
// codex_app_server.rs
#[derive(Default)]
struct CodexThreadTracker { threads: HashMap<String, ThreadLive> }   // Arc<Mutex<…>>
struct ThreadLive {
    usage: Option<ContextUsage>,    // ← thread/tokenUsage/updated(total + modelContextWindow)
    active_turn: Option<String>,    // ← turn/started 置;turn/completed + error(terminal) 清
    model: Option<String>,          // ← thread/start 响应 / spawn ctx
}
// client() 握手成功后 spawn 一次:client.subscribe() → 循环更新 tracker。
// broadcast Closed ⇒ task 自然退出;forget_client 重拨 ⇒ 新 dispatcher。
```

`handle_directive`(`/status`、steer-vs-start 选路、`/interrupt`)与 `thread_status()`(P3-Codex)都**读 tracker**;`events()` 退回纯翻译(progress_bridge 镜像照旧,`build_codex_token_usage_event` 不动)。骨架先例都现成:`bridges` map(`:116`)证明 per-thread adapter 状态成立,`maybe_start_codex_typed_event_tap`(`:352`)证明独立订阅 task 成立。

> D2.4「净新增工作」定性不变 —— 只是消费点从流内挪到 dispatcher,换来一次写、处处读、无双算。

---

## 4. 概念外的支撑修缮(F10 配套,不引入新概念)

### 4.1 `CodexTransport` 单轴(对 PRD §3-F10「最小改 client()」的修订)

今天 `client()`(`codex_app_server.rs:153-206`)调用点内嗅**两个** env(`SOCKET` × `TRANSPORT`)走四态矩阵;fix A 落地后两格是死的。收敛:

```rust
pub enum CodexTransport { Stdio { program: String }, Socket { path: PathBuf } }
/// 纯函数,单轴:CCTEAM_CODEX_APP_SERVER_SOCKET 设了 ⇒ Socket(自带 daemon 的 power user);
/// 没设 ⇒ Stdio(CCTEAM_CODEX_BIN | "codex")。
pub fn resolve_codex_transport() -> CodexTransport
```

构造期解析一次存 adapter;`client()` 只剩 `match`;UDS 连接代码保留(PRD don't-touch 守住)。**删 `APP_SERVER_TRANSPORT_ENV`**(pre-v1 不留 alias,grep 迁移;现有 stdio 测试删 env 即得默认行为)。`ThreadHandle.raw_extras.transport`(`:364`)改写 resolved 真值 —— 今天它无脑回显 env 或 `"uds"`,F10 后会说谎。dev-plan「默认无 env 选 stdio」断言直接打在纯函数上,不拉进程。

### 4.2 AdapterFactory 单例化(PRD 缺口:不做会泄漏子进程)

`default_adapter_factory`(`daemon.rs:60-69`)**每次调用 new 一个 adapter**;gateway 三处按需调它:`start_session`(`gateway.rs:680`)、`load_state`(`:864`)、`resume_restored_sessions`(`:283`)。UDS 时代无伤(N 实例 = N 条到同一 daemon 的连接);**stdio 化后语义突变**:每个 `CodexAppServerAdapter` 实例的 `client()` 各自 spawn 一个 `codex app-server` 子进程(`codex_jsonrpc.rs:108-127`)—— 两个 codex session 两个子进程;每次 daemon 重启 resume 再造实例 ⇒ 新子进程,旧的等旧 Arc 全 drop 才被 `kill_on_drop` 收走。PRD「每 codex bot 恰好一个常驻子进程」在现 factory 形状下不成立。修缮(5 行级,语义关键):

```rust
pub fn default_adapter_factory() -> AdapterFactory {
    let claude = Arc::new(ClaudeTuiAdapter::new());
    let codex  = Arc::new(CodexAppServerAdapter::new());   // 进程级单例
    Arc::new(move |vendor| match vendor {
        AgentVendor::Claude => claude.clone(),
        AgentVendor::Codex  => codex.clone(),               // Clone 共享 inner(已是 Arc 字段)
    })
}
```

- 结果:**每 daemon 恰好一个 codex app-server 子进程**,所有 codex session 走 `threadId` 多路复用(app-server 本就是多 thread 服务,`thread/start` 带各自 cwd,跨项目成立);tracker(§3.3)/override map 天然全局一份。
- **红线核对**:codex 子进程是 **RPC transport 进程,不是会话** —— thread 状态由 Codex 落盘 `CODEX_HOME`,resume-by-id 重连即回;daemon 退出子进程随 `kill_on_drop` 死,不违反「永不主动 kill 长 session」(R5 保护会话上下文,不是管道)。
- 故障半径:子进程崩 ⇒ 全部 codex session 同时 submit error(IM 可见)⇒ `forget_client()` 重拨自愈。可接受;要隔离再升 per-bot map,本版不做。Claude 侧零影响(unit struct)。

---

## 5. 对 PRD 的三处修订点(动工前需确认)

| # | PRD 原文 | 修订为 | 出处 |
|---|---|---|---|
| 1 | §3-D2.4「在 `events()` 流里消费 tokenUsage」 | adapter 级独立 dispatcher(tracker),`events()` 保持纯翻译 | §3.3 |
| 2 | §3-F10「最小改 `client()`」+ 保留 transport env | 构造期 `resolve_codex_transport()` 单轴,**删** `APP_SERVER_TRANSPORT_ENV` | §4.1 |
| 3 | §3-F10「每 codex bot 恰好一个常驻子进程」 | **每 daemon** 恰好一个(factory 单例 + threadId 多路复用) | §4.2 |

其余均为 PRD 留白处的形状收敛,不改对外行为与验收条目。

## 6. 红线一致性(对 CLAUDE.md §三)

- **No prompt injection**:§1.3 透传复用 submit_turn 零改写;①② 全是 ccteam 自有交互面,不进 pane/app-server prompt。
- **不解析终端输出**:全文无 capture;tracker 消费 app-server 原生通知。
- **progress.jsonl SoT**:tracker 是 adapter 内存运行态(answering 展示查询),不写 progress、不替代 progress_bridge 既有镜像行。
- **永不主动 kill 长 session**:§4.2 子进程=transport 非会话;thread 落盘 resume-by-id 不受影响。
- **文件系统是状态面**:PendingInteraction 是秒级交互运行态(同既有 pending-picker 定位),daemon 重启过期重来,不入 SoT —— 与 outbound ledger(投递可靠性才入盘)分界一致。
- **vendor enum 无 default**:两个新 trait 方法双双无 default impl。
- **channel-neutral**:opaque data + `register_commands`,gateway/daemon 全程零新增 `"telegram"` 字面量。

## 7. wave 映射 delta(相对 dev-plan)

| Wave | 原计划 | 增量 |
|---|---|---|
| **W1** | D1+D3 基建 | trait 同 PR 加 `thread_status` 签名(各 adapter stub 一次补齐);`PendingInteraction` 双 origin 类型(External 仅类型);`GatewayReply` / `GatewayEvent.options` / `MessageOption` / `ChannelMessage.selection`;`GATEWAY_COMMANDS` 表 + `Channel::register_commands` 默认实现;slash 单行判定规则 |
| **W2** | D5+D6 | D6 改走 External origin + mcp.sock 阻塞 handler(oneshot),不自建挂起逻辑 |
| **W3** | F10 + D2/D4 | 第 0 步 = §4.1 + §4.2 **同一 PR**(单轴 + factory 单例 + `resolve_codex_transport` 默认断言);D2 先立 tracker 再写映射表 |
| **W4** | P1/P3/P4 | P3 只剩两端 `thread_status` 实现体 + gateway 渲染;P1 只剩 TG override + startup 一行 |

## 8. 明确不做(防 scope creep)

1. **不动 gateway 锁模型**:`handle_message` 全程持 `Mutex<Gateway>`(`daemon.rs:473-484`),`handle_directive` 的 RPC await 同样在锁内 —— 沿用 submit 同款超时包裹(`CCTEAM_IM_GATEWAY_SUBMIT_TIMEOUT_MS`)兜底;锁粒度重构是另一版本的事。
2. **不持久化 per-session override map**(PRD 已接受重启丢失;日后挂 `SavedGatewaySession` 扩展字段,一跳可达)。
3. **不动 `events()` 签名 / `ThreadEvent` schema**:tokenUsage 不进 CanonicalEvent(走 pull),避免 event schema 为展示需求漂移。
4. **不合并 `HarnessSnapshot` 与 `ThreadStatus`**(§3.2)。
5. **不做 per-bot codex 子进程隔离**(§4.2,故障半径可接受,留升级路径)。

## 9. 新增验收(抽象锁定测试,叠加 PRD §5.2)

1. `resolve_codex_transport`:无 env ⇒ `Stdio`;socket env ⇒ `Socket`(取代 dev-plan「daemon.rs:1137 补 transport 断言」的进程级写法)。
2. factory 两次取 Codex arm ⇒ `Arc::ptr_eq` 同一实例(§4.2 防回归)。
3. PendingInteraction:Directive origin 重入带 choice;External origin oneshot 收到 selection;TTL 过期 / 被抢占 ⇒ External 收终止信号(W2 映射 deny-with-reason)。
4. `data="{token}:{idx}"` 往返:出站渲染 → 模拟 callback → 归一出的 `ChoiceSelection.ids` = 真实 option id(idx 反查正确)。
5. tracker:scripted peer 喂 `turn/started`→`tokenUsage`→`turn/completed` ⇒ `thread_status` 反映 usage、active-turn 置/清正确;**同时开两条 `events()` 流,tracker 不双算**(钉死 §3.3 问题 1)。
6. `thread_status` Claude:fake transcript 带/不带 `[1m]` ⇒ 1M / 200k 分母。
7. 多行 `/` 开头消息 ⇒ 按 UserText 提交,不产生 Rejected(§1.3-1)。
