# v0.8.5 Wave 1 handoff —— 中立基建(D1 + D3 + 骨架前置)

> 范围:arch-refactor §6-W1。落「命令面 + 反问 + 状态」三刻面的中立骨架,gateway 退纯路由,删 `TurnInput::SystemDirective`。W2/W3/W4 只换实现体,不再动 trait 面 / 公共结构。
> 分支:`v0.8.5-w1`(off `origin/dev`)。落地方式:**直接 commit dev + push**(用户决策,不走 PR;推前自跑对抗 review)。

## Decided(本 wave 拍板)

1. **trait 一次性加两个无-default 方法**(`adapter.rs`):`handle_directive` + `thread_status`。全 workspace ~24 个 impl 一次补齐:4 生产(claude_tui 透传 / codex_app_server 迁 compact+review、clear/new/resume→Redirect、其余→Rejected / 两个 bg 显式 `Rejected`+`ThreadStatus::default()`)+ ~20 测试 double stub。编译期锁扩到 4 生产 adapter(`harness_trait_test.rs`,补 CodexAppServer)。
2. **中立词汇住 `adapter.rs`**(与 `ThreadEvent`/`ApprovalIR` 同层):`Directive` / `DirectiveOutcome`(5 值)/ `ChoicePrompt` / `ChoiceOption`(`{id,label}`)/ `ChoiceSelection` / `ContextUsage`(`pct()` 派生)/ `ThreadStatus`。`events()` rustdoc 钉 final-only 契约(§1.5,零代码)。
3. **gateway 退纯路由**:slash 单行判定(`trim` 后 `/` 开头**且无 `\n`**)→ `Directive` → `handle_directive` → 渲染 5 outcome;删 `turn_input_for_session` 的 vendor 分支 + `compact|review` allowlist;`TurnInput::SystemDirective` 全删。
4. **`PendingInteractions` 独立模块独立锁**(`pending.rs`):双 origin(`Directive` 重入 live / `External` oneshot 仅类型,W2 接)、单飞 per (chat,session)、TTL、token 守卫、`drain_expired`。
5. **transport D3 形态**:`MessageOption{data:"{token}:{idx}", label}` + `SendMessage.options` + `ChannelMessage.selection: Option<ChoiceReply>` + `ChoiceReply` + `CommandSpec` + `Channel::register_commands`(默认 no-op);全 `#[serde(default)]`。TG `send()` 渲 inline keyboard + `listen()` 收 `callback_query` → selection + `answerCallbackQuery`;slack/discord/lark/mock 走 content 内嵌的编号文本兜底(无需改)。
6. **web 平行 plumbing**:`WebMessageOption` + `WebSendMessage.options` + `WebChannelMessage.selection` + `ServerChatFrame::Choice`(chips)+ `ClientChatFrame::Choice`(chip-click);`web_chat_bridge` send/to_im_message 双向映射;chat_ws `frame_to_messages` 加 Choice 臂 + `send_message_to_frames` 出 chips。
7. **GATEWAY_COMMANDS 单表 + `/help`**:`is_gateway_command` 由表派生;新增 `/help` 由表渲染。菜单注册的 daemon startup 调用 + TG `setMyCommands` override 留 W4-P1。

## Rejected / 偏离 doc(明示,待你裁可)

1. **不引入 `GatewayReply`,保留 `handle_message`/`handle_text` 的 `Vec<String>` 返回**。arch §2.2 代码草图给了 `GatewayReply{text,options}` 替裸 String。改为:`NeedsChoice` 的 options 走 **`GatewayEvent.options`(异步出站,生产路径)** + **content 内嵌编号文本(同步/无-sink 兜底)**。理由:① 同样满足「options 上 IM、channel-neutral」功能要求且 `GatewayEvent.options` 本就是 doc 要求的另一半;② 零 churn 到 73 处 `handle_text` 断言(改返回类型会波及全部);③ W1 无任何生产 adapter 产 `NeedsChoice`(claude 透传 Turn / codex Turn|Redirect|Rejected / bg Rejected),sync 路径带 options 仅测试需要,已由 `GatewayEvent.options` + 编号兜底覆盖。**若你坚持 doc 的 `GatewayReply` 形状,一句话我换过来**(机械活)。
2. **web 出站文件 attachments 不在本 wave 修全**。must_fix #3 提「web_chat_bridge 丢 v0.8.4 attachments」。实测:web 入站附件是 `/attach` **文本编码**(`chat_ws.rs:266`),`WebChannelMessage` 无附件源 ⇒ `to_im_message` 的 `attachments: Vec::new()` 入站方向**不丢任何东西**(已加注释说明);出站 `SendMessage.attachments`→web 需浏览器文件渲染面(v0.8.4 P2b 是 IM 专属,web 无渲染通道),属 v0.8.4 补完、与 v0.8.5 D3 正交 ⇒ **本 wave 只补 options/selection,attachments 出站标记为 follow-up**(见 Remaining)。
3. **`PendingInteractions` 本 wave 由 gateway 自建**(`Gateway::new*` 默认 new 一个),未走 daemon 共享 Arc 双交。`set_pending` 已就位。理由:W1 只用 Directive origin(全在 gateway 内);External origin 的 mcp.sock 双交是 D6(W2)唯一消费者,W1 装一个未用的 sock handler 参数只会触 clippy unused。**daemon 双交 + mcp handler 半边随 W2-D6 落**(届时 `daemon` 建共享 Arc → `set_pending` + 传 handler)。
4. codex `handle_directive` 把 `client()` 收进 compact/review 两臂(redirect/reject 不连接)——比草图更省一次握手,且让 `/clear→Redirect` 可无 peer 单测。

## Risks

- **真机未验**:TG inline keyboard + `callback_query` 入站、web chips 帧均为新路径,仅 deterministic 测试覆盖(FakeAdapter / 单测)。真机 smoke 建议在 W2/W4 顺带跑。
- **多订阅/竞态**:gateway 持 `Mutex<Gateway>` 处理入站,`pending` 独立锁;W1 无长 await 落锁内(D6 的 600s oneshot 是 W2 才接,届时务必锁外——arch §7-1)。
- **token 纪律**:`MessageOption.data="{token}:{idx}"`,producer mint token 需 ≤16B ASCII 且不含 `:`(注册表按首个 `:` split);W1 的 FakeAdapter/测试 token 合规,真实 producer(W2 D5 / W3 D4)落地时复核。

## Files(本 wave 改动)

- `ccteam-harness/src/adapter.rs` —— 中立类型 + trait 双方法 + 删 SystemDirective + events() final-only rustdoc。
- `ccteam-harness/src/lib.rs` —— re-export 新类型。
- `ccteam-harness/src/execution/{claude_tui,codex_app_server,codex_exec,claude_bg}.rs` —— 4 生产 adapter 补两方法 + 删 SystemDirective 旧路径。
- `ccteam-im/src/pending.rs` —— **新**:`PendingInteractions` + 单测。
- `ccteam-im/src/lib.rs` —— `pub mod pending`。
- `ccteam-im/src/gateway.rs` —— `GATEWAY_COMMANDS`+`/help`、路由重写(directive/selection/numeric)、`PendingInteractions` 接入、`GatewayEvent.options`、FakeAdapter 改 + dual-vendor 测试改 + 新 §8 测试。
- `ccteam-im/src/transport/mod.rs` —— `MessageOption`/`ChoiceReply`/`CommandSpec`/`SendMessage.options`/`ChannelMessage.selection`/`register_commands`。
- `ccteam-im/src/transport/providers/{telegram,slack,discord,lark,ws}.rs` —— `ChannelMessage.selection` 补 + TG inline keyboard/callback_query。
- `ccteam-im/src/daemon.rs` —— `with_options` 出站 + 传 `selection` 入 `handle_message`。
- `ccteam-cli/src/{main.rs,web_chat_bridge.rs}` —— mcp GatewayEvent.options + web 桥双向映射(含 attachments 注释)。
- `ccteam-web/src/chat_protocol.rs` + `routes/chat_ws.rs` —— web 平行 plumbing 四件。
- `ccteam-core/tests/harness_trait_test.rs` —— 编译锁补 CodexAppServer。
- 各测试 double 补 stub;harness 集成测试迁 `submit_turn(SystemDirective)`→`handle_directive`。

## Remaining(交下游 wave)

- **W2(D5/D6)**:claude `handle_directive` 四通道 gate(替 W1 透传);D6 = mcp.sock 接 `PendingInteractions`(External origin)+ `find_chat_for_bot` + 锁外 600s oneshot;daemon 建共享 pending Arc + `set_pending` + handler 双交。
- **W3(F10/D2/D4)**:codex 单例 factory + `resolve_codex_transport` + tracker;`handle_directive` 全六类映射 + D4 两段式 `NeedsChoice`;`/clear→Redirect` 等真值的 codex 适配器单测(scripted peer)。
- **W4(P3/P1)**:`thread_status` 两端真实现(claude 倒读 transcript `[1m]`→1M;codex 读 tracker);gateway 渲染 `188k / 1M (19%)`;daemon startup `register_commands` + TG `setMyCommands`。
- **§8 follow-up(本 wave 未尽)**:#4 的 **web chip-click 往返**单测 + #10 的 **web 桥双向(options/selection)往返**单测(待 web_chat_bridge 改定后补);web 出站文件 attachments 渲染(v0.8.4 补完,正交)。
- 若裁定改回 `GatewayReply`,替换 `handle_message`/`handle_text` 返回类型 + 73 处断言。

## Gate(push 前实测)

- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = **1858 / 0**(起手基线 1850/0,+8)。
- `cargo clippy --workspace --all-targets -- -D warnings` = **0**(workspace 与 ccteam-web 两段跑均 0 warning;`handle_message` 因 `selection` 第 8 参补 `#[allow(clippy::too_many_arguments)]`,同 daemon `deliver_progress` 先例)。
- `cargo fmt --all --check` = **clean**。
- ccteam-web chat_protocol round-trip 单测(含新 `Choice` 帧)`cargo test -p ccteam-web --lib chat_frame` = **3/0**(ws_* 集成测试仍留 CI,不计本机 baseline)。
