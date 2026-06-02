# v0.8.4 PRD —— IM 日常驱动「最后一公里」

> **一句话**:让「cd 进已有项目 → `ccteam init` → 在 Telegram 里日常开发」从「能跑通 smoke」逼近「能丝滑替代 TUI」,通过解决 3 个硬阻断:**进度可见 / 长消息分片 / 图文 I/O**。
>
> 来源:v0.8.3 gap 分析(spine 已 e2e-tested,但这 3 条每天都撞)。**基准线**:用户当下已经用官方 `plugin:telegram` 从 TG 驱动 Claude Code——那是 UX 下限,ccteam 网关在多项目/多 session 路由 + 双 vendor + 成本预算上更强,但目前在「长消息分片」「图文」两点**反而不如**它,本版补齐。

---

## 0. 状态与红线

| 项 | 值 |
|---|---|
| 基线起点 | `cargo test --workspace --exclude ccteam-web` = **1759/0**,clippy 0 warning(HEAD `a44402b`)|
| 版本 | `0.8.4`(**本 PRD 是 doc-first,代码未动;不在本阶段 bump 版本**)|
| 形态 | 4 个独立可发、独立 verify-gated 的 phase(P0/P1/P2a/P2b),各自一份 PR + baseline gate |

**必须守的红线(CLAUDE.md §三)**:
- **不解析终端输出**:进度只来自 `HarnessAdapter::events()` 的 `CanonicalEvent`,**不** scrape pane。
- **`progress.jsonl` 是 state SoT**:本版不改 progress 写 schema;IM 进度是「投递层 UX」,不是新 SoT。
- **gateway 保持 channel-neutral**:分片/编辑能力走 `Channel` trait 的 per-channel 接口,**禁止**把 `4096`/Telegram 假设硬编进 gateway 或 daemon(web `ChatConsole` 经 `WsChannel` 同走这条路,硬编会误伤)。
- **at-least-once + 顺序**:出站投递语义不退化;分片只是把 1 条变 N 条**有序**子消息。
- **不写迁移/兼容分支**(Pre-v1.0):不留 alias、不写 backwards-compat shim。

---

## 1. 关键架构事实(代码实测 @ `a44402b`,实现前必读)

> 这些是 PRD 设计赖以成立的拓扑,dev session 起手先复核没漂移。

1. **双出站路径互斥**。`Gateway::submit_to_current`(gateway.rs:803)提交 turn 后:
   - **`event_sink == Some`(生产 daemon)**:同步只返回 ack `"submitted s{n} turn {id}"`(:843),**真答案由异步 event pump 投递**。
   - **`event_sink == None`(无 sink 的纯进程测试)**:同步 drain `event_text`(:837)拿首条 AgentMessage 返回。
   - ⇒ **不会双投**。B1 进度只需挂**异步 pump**(`spawn_event_pump` gateway.rs:660);sync `event_text` 路径是 fallback,镜像即可。
2. **出站唯一 choke point** = `send_gateway_outbound`(daemon.rs:542)→ `finish_durable_outbound_send` → `channel.send()`(:570)。两个 caller(inbound-reply :465/:479、event pump :510)都汇于此。⇒ **B2 分片只改这一处**。
3. **进度被丢弃**:`async_event_text`(gateway.rs:1077)/`event_text`(:1197)都只放行 `AgentMessage` + error 文本,`ToolCall/CommandExecution/FileChange/WebSearch/Reasoning` 与所有 Turn/Item 生命周期事件 `=> None`。
4. **ack 是天然种子**:pump 模式下 sync 返回的 `"submitted s{n} turn {id}"` 会被当普通 reply 发回(daemon.rs:462-466)——它正好可改造成 B1 的初始「⏳ working…」status 消息,后续被编辑。
5. **传输层纯文本**:`ChannelMessage`/`SendMessage`(transport/mod.rs)均无附件字段;`Channel` trait 无 `edit`、无 `max_len`;`TgMessage`(telegram.rs:78)只反序列化 `text`;出站只有 `sendMessage`。
6. **MCP socket handler 只拿 `paths`**(main.rs:2154 `handle_mcp_socket_connection`),**没有** live `Gateway`/`ChannelMap` 句柄;`chat_send_input`(mcp_chat_tools.rs:452)是「写 `InboxEnvelope` 文件」的无状态 drop。bot 注册项带 `(im_platform, im_chat_id)`(inbound.rs:273、gateway.rs:333)。`screenshot` 工具(mcp_serve.rs:428)今天只**返回 PNG 路径**,不推送 chat。⇒ 这决定了 **B3b 出站文件的「agent 如何把文件寻址回自己 chat」是本版唯一真正的开放设计题**(见 P2b)。
7. **deterministic fake**:`CCTEAM_{CLAUDE,CODEX}_BIN` + `MockChannel`(in-proc 双向)+ `WsChannel`(over real socket)。pump 级测试用 fake adapter 发 `ThreadEvent`;e2e 用 `daemon_routes_*` 家族(inbound_wiring_test.rs)。

---

## 2. 目标 / 非目标

**目标**(按发布顺序)
- **P0 / B2**:任何出站文本超 channel 上限时**有序分片**多发,绝不静默丢失;代码块跨片不破坏。
- **P1 / B1**:一次 turn 进行中,TG 里能看到**逐步骤进度**(跑了什么命令、改了什么文件、在想什么),不再一片静默;最终答案单独成消息(会 ping)。
- **P2a / B3 入站**:用户在 TG 发图片/文件 + caption → agent 能读到(贴报错截图可用)。
- **P2b / B3 出站**:agent 能把文件/截图发回 TG(`sendPhoto`/`sendDocument`)。

**非目标**(本版明确不做)
- **token 级流式**:transcript 只在「步骤完成」时落行,**不暴露** sub-second token tick;进度粒度 = per-completed-step(P1 据此设计,别去追 token 流)。
- **HITL 审批**:`--dangerously-skip-permissions` 维持现状(推后)。
- **Slack / Discord 附件**:trait 接口要中立留口,但 provider 实现只做 Telegram;Slack/Discord 留 `None`/default。
- **web `ChatConsole` 附件 UI**:本版只保证 gateway 中立、不回归;web 侧富交互推后。
- **错误回显全面整改**:P0 顺带把「分片/发送失败」从 silent 改成回 chat 一行,但 gap 分析里的「错误回显不一致」整体整改推后。

---

## 3. 分阶段设计

> 每个 phase:**决策 + 被否方案 + 触碰文件 + 验收(AC)+ 测试 + 红线复核**。各 phase 独立 PR、独立 baseline gate(≥1759/0 + clippy 0)。

### P0 —— 长消息分片(B2)〔先发:最便宜、立即止血〕

**决策**
- `Channel` trait 增 `fn max_message_len(&self) -> Option<usize> { None }`(default 无限)。`TelegramChannel` 返回**保守预算**(见下),Slack/Discord 暂 `None`。
- `sanitize` 增 `pub fn split_for_channel(text: &str, max_units: usize) -> Vec<String>`:按 **UTF-16 code unit** 预算切(Telegram 的 4096 是 UTF-16 单元,非 char;emoji/补充平面 1 char = 2 单元)。断点优先级:段落 `\n\n` > 行 `\n` > 空白 > 硬字符边界(永不切出无效 UTF-8)。**代码围栏(``` fence)跨片时,在每片重开/闭合 fence**(尽量保留 ```lang)。可选给多片加 `(i/N)` 头。
- `TelegramChannel::max_message_len` 取**保守值(建议 3800–4000)**留余量给分片头 + reply 元数据;实现注释写明「4096 是 UTF-16 单元上限,留头」。
- 接入点:`send_gateway_outbound`(daemon.rs:542)——发送前若 `channel.max_message_len()` 为 `Some(limit)` 且内容超限,则 `split_for_channel` 后**顺序逐片** `channel.send()`,每片一条 durable-outbound row(`{id}-{seq}-{part}`)。`None` 走原路。
- **失败可见**:某片 `send` 失败 → 回 chat 一行 `⚠️ 部分消息发送失败 (part k/N)`(替换今天的纯 warn-log silent)。

**被否**
- *在 `TelegramChannel::send` 内部分片*:把多消息语义藏进 provider,durable-outbound ledger 只记到 1 条,丢失「N 片各自投递状态」的可观测性 + 破坏 at-least-once 记账。✗
- *截断到 4096 + 省略号*:仍丢数据(一个 diff 被砍)。✗ 分片才是正解。

**触碰文件**:`transport/mod.rs`(trait + 可能给 `SendMessage` 不动)、`transport/providers/telegram.rs`(`max_message_len`)、`sanitize.rs`(`split_for_channel`,复用现有 `truncate_to_max` 的 char-boundary 经验)、`daemon.rs`(`send_gateway_outbound` 分片循环 + 失败回显)。

**AC**
- 5000 字符的英文答案 → TG 收到 ≥2 条有序消息,拼接 == 原文(去掉可选片头)。
- 含 ```rust … ``` 且整体超限 → 每片都是合法 fence(打开必闭合)。
- 含大量 emoji、总 char < 4096 但 UTF-16 单元 > 4096 → 仍正确分片,无 Telegram 400。
- `None`(MockChannel/WsChannel 未声明 limit)→ 行为与今天一致,单条发出。

**测试**(deterministic)
- `sanitize::split_for_channel` 单测:UTF-16 边界、fence 连续性、断点优先级、`max_units` 等于/略小于内容。
- daemon 出站:fake/MockChannel 声明小 `max_message_len`(如 20)→ 断言 ledger 记 N 条 `Sent`,顺序正确。
- **ledger 约束(硬)**见 §4.2。

---

### P1 —— 进度可见(B1)

**决策**
- `Channel` trait 增 `async fn edit_message(&self, recipient: &str, message_id: &str, content: &str) -> anyhow::Result<Option<String>>`,**default impl = `self.send(new message)`**(不支持编辑的 channel 优雅降级为追加)。`TelegramChannel` 实现 `editMessageText`。
- **live-edited status 消息**(对齐官方插件 UX):
  - `spawn_event_pump`(gateway.rs:660)改造:**区分两类事件**——
    - **答案类**(`ItemCompleted{AgentMessage}` + error)→ 走出站 = **新消息**(经 P0 分片;会 ping)。
    - **进度类**(`ToolCall/CommandExecution/FileChange/WebSearch/Reasoning`,`ItemStarted/Updated`,`TurnStarted/Completed`)→ 维护**每 turn 一条 status 消息**:首个进度事件时发「⏳ working…」(复用 §1.4 的 ack 作为种子),之后把**滚动进度行**(cap 最近 N 行,建议 6–8;每行一条 compact 摘要,如 `🔧 Bash: cargo test`、`✏️ edit src/foo.rs`、`🔎 web: …`、`🧠 thinking…`)**编辑**进 status 消息。
  - **节流**:相邻 `edit_message` 间隔 ≥ 阈值(建议 1.5s;TG 编辑软限 ~1/s),期间合并缓冲;turn 结束把 status 收尾成 `✅ done · n tools · m files`(或失败态)。
  - **Codex delta 顺带修好**:`ItemUpdated{AgentMessage(delta)}`(Codex app-server 的流式增量)今天会被当独立答案发 → 刷屏。新规则:**只有 `ItemCompleted{AgentMessage}` 算「投递的答案」**;`ItemUpdated{AgentMessage}` delta 喂给 status 的「✍️ drafting…」,不单独发。
- **粒度声明**:进度 = **每步骤完成**(transcript item 完成时落行);一条 2 分钟的 `cargo test` 是在它**返回后**显示「ran cargo test」,**不是**运行中逐 token。设计与测试都按此,别追流式。
- **开关**:env `CCTEAM_IM_PROGRESS=off` 全局关(回退到只发答案);默认 on。
- **事件 → 进度行**:新增纯函数 `progress_event_line(evt) -> Option<String>`(与 `async_event_text` 解耦);pump 路由二选一。

**被否**
- *每个进度事件发一条独立 TG 消息*:15 个工具 = 15 条 ping,刷屏,反丝滑。✗
- *纯定时合并、不编辑*:仍是多条新消息或延迟感强;编辑 + 节流才是官方插件那种「一条活的 status」体验。✗
- *把进度写进 progress.jsonl 让 web 读*:那是 SoT 层,不是 IM 投递;IM 进度是投递层 UX,直接走 Channel。✗

**触碰文件**:`transport/mod.rs`(`edit_message`)、`transport/providers/telegram.rs`(`editMessageText`)、`gateway.rs`(pump 改造 + `progress_event_line` + status 状态机 + env toggle)、`daemon.rs`(event consumer 支持「编辑既有消息 vs 发新消息」——需要把「这是进度编辑还是答案新消息」沿 `GatewayEvent` 传递,见下)、可能 `GatewayEvent` 加 `kind: Progress|Answer` + `edit_target: Option<String>` 字段。

**AC**
- fake adapter 发序列 `[TurnStarted, ToolCall(Bash), CommandExecution, FileChange, AgentMessage("done"), TurnCompleted]`:
  - status 消息出现并被**编辑** ≥1 次,含工具/文件摘要;
  - `"done"` 作为**独立新消息**投递;
  - 总「新消息」数 = 1(答案)+ 1(status 种子),其余是 edit,**不刷屏**。
- `CCTEAM_IM_PROGRESS=off` → 只收到答案新消息,无 status。
- Codex 风格 `ItemUpdated{AgentMessage(delta)}×k` → **0** 条独立 delta 消息;仅 status drafting + 末尾 `ItemCompleted` 一条答案。

**测试**(deterministic)
- **显式 gate(advisor 点名)**:先写一个 fake-transcript 测试断言 `ItemCompleted{ToolCall/CommandExecution/FileChange}` **确实从 adapter `events()` 流出**(别假设;earlier 探索说会流出,这里钉死)。
- pump 单测:用 MockChannel(记录 send vs edit 调用序列)断言上面的 AC。
- 节流:注入假时钟/可配阈值(env `CCTEAM_IM_PROGRESS_THROTTLE_MS`)确保测试不靠真 sleep。

---

### P2a —— 入站图文(B3 入站)〔高价值:贴报错截图〕

**决策**
- `TgMessage`(telegram.rs:78)扩展反序列化:`photo: Vec<TgPhotoSize>`(取 `file_id` 最大尺寸)、`document: Option<TgDocument>`、`caption: Option<String>`。
- `ChannelMessage`(transport/mod.rs)增 `attachments: Vec<ChannelAttachment>`(`{ kind: image|file, file_name, local_path, mime, size }`,`#[serde(default)]`)。
- `TelegramChannel::listen`:遇 photo/document → `getFile` 拿 `file_path` → 从 `https://api.telegram.org/file/bot<token>/<file_path>` 下载 → 落 daemon staging 目录 `~/.ccteam/imd/attachments/inbound/<cid>-<sanitized_name>`(此刻还不知道路由到哪个 project/role,先 channel-scoped staging;`local_path` 放进 `ChannelAttachment`)。caption 进 `content`。
- 注入 turn:`handle_text` 升级为携带 attachments(新 `handle_message(channel, chat_id, sender, text, attachments)`,旧 `handle_text` 保留为 `attachments=[]` 的薄封装——**注意**:这不是 backwards-compat shim,是同一函数的参数化;不留废弃别名)。turn 文本 = caption + 追加 `\n\n[附件:图片 /abs/path.png]` / `[附件:文件 /abs/path.pdf]`,Claude session 用 Read 工具读路径;send-keys 路径上路径即字面文本。
- **安全**:文件名 sanitize(去路径分隔/控制符)、大小上限(Telegram bot 下载上限 20MB,超限回 chat 一行拒收)、仅 `chat_allowed` 的 chat。

**被否**
- *把图片内容塞进 turn 文本(base64/OCR)*:污染上下文、丢真实文件语义。✗ 落盘给路径,让 agent 用 Read。
- *直接写进 project chat 目录*:listen 阶段还没路由,不知 project/role;staging + 路由后引用更干净。✗(若 dev 发现路由信息在 listen 可得,可优化,但默认 staging。)

**触碰文件**:`transport/providers/telegram.rs`(解析 + getFile + 下载)、`transport/mod.rs`(`ChannelMessage.attachments` + `ChannelAttachment`)、`gateway.rs`(`handle_message` + turn 文本拼接)、`daemon.rs`(inbound consumer 透传 attachments)。

**AC**
- TG 发一张图 + caption「这是报错」→ agent turn 收到 caption + 一个可 Read 的本地 png 路径;Claude 能读到图。
- 发 >20MB 文件 → chat 收到拒收提示,不崩。
- 纯文本消息 → 行为与今天一致(attachments 空)。

**测试**:MockChannel 注入带 attachments 的 `ChannelMessage` → 断言 `handle_message` 拼接的 turn 文本含路径;Telegram 解析单测用 fixture JSON(photo/document)断言 `ChannelAttachment` 提取正确(下载走 mock http 或抽出纯解析函数测)。

---

### P2b —— 出站文件(B3 出站)〔最重 + 唯一真开放题〕

**决策(部分)+ 开放设计题**
- `SendMessage`(transport/mod.rs)增 `attachments: Vec<OutboundFile>`(`{ path, caption: Option<String>, kind: photo|document }`,`#[serde(default)]`)。
- `TelegramChannel::send`:若 `attachments` 非空 → 对每个走 `sendPhoto`/`sendDocument`(multipart 上传),caption 放首个;否则维持 `sendMessage`。
- 新 MCP 工具 `ccteam__chat_send_file`(归 `chat_` group;参数 `workflow_slug, role, path, caption?, kind?`)。

> **⚠️ 开放设计题(dev session 实现前必须先确认可达性,再动手)**:
> agent 的 MCP 调用如何把文件**寻址回自己所在的那个 TG chat**?难点(§1.6):**daemon 的 MCP socket handler 只拿 `paths`,没有 live `Gateway`/`ChannelMap` 句柄**;`chat_send_input` 是无状态文件 drop。
>
> **候选 A(推荐,最少新耦合,贴合「文件系统是 artifact 控制面」)**:`chat_send_file` 由 registry 解析目标 `(im_platform, im_chat_id)`(注册项已有),写一条「出站文件请求」artifact(如 `~/.ccteam/imd/outbound-files/<ts>-<rand>.json`,含 channel/chat_id/path/caption/kind);daemon 起一个轻量 **outbound-file watcher**,消费后构造带 `attachments` 的 `SendMessage` → 经 `send_gateway_outbound` 投递(自动享受 ledger + 失败回显)。
>
> **候选 B**:给 MCP socket handler 注入一个「出站 enqueue 句柄」(`Sender<GatewayEvent>` 的扩展,`GatewayEvent` 带 attachments),直达 event consumer。更干净但需把 gateway sink 传进 socket handler,耦合更大。
>
> dev 先 grep 确认 socket handler 能否拿到 sink/channels;**拿不到就走候选 A**;别在没确认可达前把机制写死。`screenshot` 工具(返回 path)可在 `chat_send_file` 之上叠加(渲染 → send_file)。

**被否**
- *用 `FileChange` 事件路径自动发文件*:每次编辑都发,噪声爆炸;且语义错(编辑≠想发给用户)。✗ 必须 agent 显式 `chat_send_file`。
- *sentinel 字符串(`[[ccteam:send-file …]]`)塞进答案文本被 outbound 解析*:脆弱、易误触、要解析输出(蹭红线边)。✗ MCP 工具显式、类型化。

**触碰文件**:`transport/mod.rs`(`SendMessage.attachments` + `OutboundFile`)、`transport/providers/telegram.rs`(`sendPhoto/sendDocument`)、`mcp_chat_tools.rs`(`chat_send_file` dispatcher + 加进工具清单)、`mcp_serve.rs`/`mcp_tool_groups.rs`(注册 + group)、`daemon.rs`(候选 A 的 watcher 或候选 B 的 sink 注入)、`ccteam doctor --verify-mcp` 自检 + `STUB_TOOLS` drift。

**AC**
- agent 在 TG-routed session 里调 `chat_send_file(path=某png)` → 用户 TG 收到该图片(`sendPhoto`)。
- 文件不存在/超限 → 工具返回结构化 error,chat 收到一行提示,不崩。
- MCP 工具数 + `--verify-mcp` 自检通过(drift exit 1 不触发)。

**测试**:`chat_send_file` dispatcher 单测(registry 解析 + artifact 写出 / enqueue);Telegram `send` 带 attachments 走 multipart 的请求 shape 单测(mock http);e2e(若候选 A)：写 artifact → daemon watcher → MockChannel 收到带 attachment 的 SendMessage。

---

## 4. 跨阶段硬约束

### 4.1 channel-neutral
gateway / daemon **不得**出现 `4096` 或 `"telegram"` 分支来决定分片/编辑;一律走 `Channel::max_message_len()` / `edit_message()` 的 trait 多态。Telegram 的常量只活在 `telegram.rs`。

### 4.2 ledger 多分片顺序(避免 v8.2 flake 复发)
P0 把 1 条变 N 条有序 send。`DurableOutboundRow`(daemon.rs:516)+ 其测试**历史上有 over-strict positional 顺序断言会 race**(v8.2 flake:inbound_wiring_test.rs 的 queued/sent 位置断言)。**硬约束**:
- 多分片记账语义:每片独立 row,id = `{inbound_id}-{seq}-{part}`,state 流转 `Queued→Sent/Failed`。
- 凡涉及 ledger 顺序的断言**一律 multiset + pairing**(每个 `Queued` 有配对 `Sent/Failed`;同一逻辑消息的片集合正确),**禁止 positional/index 断言**;并发 send 用「同一逻辑消息内串行,跨消息可并发」。否则在更高分片倍数下必复发 flake。

### 4.3 baseline gate
每 phase PR 必须:`cargo test --workspace --exclude ccteam-web` ≥ **1759/0**(只增不减)+ `cargo clippy --workspace --all-targets -- -D warnings` = 0 + `cargo fmt --all -- --check` 通过。退步 = 不发 PR。

### 4.4 不破 SoT / 不解析 pane / 不写迁移
进度来自 `events()`;不动 progress.jsonl schema;不 scrape pane;不留兼容分支/别名。

---

## 5. 风险与缓解

| 风险 | 缓解 |
|---|---|
| TG `editMessageText` 速率软限(~1/s)、`sendMessage` flood 限 | P1 节流 ≥1.5s + 合并缓冲 + status 行数 cap;P0 分片顺序发(不并发轰同一 chat)|
| 4096 是 UTF-16 单元非 char,emoji 易超 | `split_for_channel` 按 UTF-16 单元预算 + 保守 limit(3800–4000)|
| 代码 fence 跨片破坏渲染 | 每片重开/闭合 fence,带单测 |
| 附件下载占盘 / 超大文件 | 20MB 上限 + staging 目录定期清理(daemon 启动时清旧;本版至少不无限增长)|
| B3b MCP→daemn 可达性未知 | 列为显式开放题;候选 A(artifact + watcher)兜底,实现前先确认 |
| Telegram 真·活体仍只 host-probe | 各 phase 用 deterministic fake + MockChannel/WsChannel 测;**最终另起一次真 bot-token 活体 round-trip smoke**(超出自动化范围,人工 gate)|

---

## 6. 发布顺序与 ship-gate

发布顺序:**P0 → P1 → P2a → P2b**(价值/成本递增,各自独立可发)。

全部落地后(最后一个 PR)按 CLAUDE.md §五.7 做 ship-gate 同步:
- workspace `Cargo.toml` bump `0.8.3 → 0.8.4`;
- CLAUDE.md §一 baseline 回填(新 test 数);
- `docs/tech-design.md` 协议→代码指针表补 `Channel::{max_message_len,edit_message}` / 附件字段 / `chat_send_file`;
- root `README.md`(英文,当前能力描述,不写「v0.8.4 新增」)+ `docs/usage.md`(命令手册补 `chat_send_file`、进度/分片行为、发图用法);
- `docs/versions/v0-8-4/` 落各 phase handoff(Decided/Rejected/Risks/Files/Remaining 五段)。

> 详细执行编排见同目录 `dev-plan.md`;派工提示词见 `dev-prompt.md`。
