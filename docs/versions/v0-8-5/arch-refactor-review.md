# v0.8.5 arch-refactor 评审报告(归档)

> 产物来源:多 agent 评审工作流(5 lens 并行评审 → blocker/major 对抗核查 → 主席合成;15 agents)。
> 被审对象:`arch-refactor.md` @ cc65860。verdict = **APPROVE_WITH_FIXES**;5 条 must-fix 已全部落回 arch-refactor.md(同次 commit),其中含一处既有 bug 发现(web_chat_bridge 丢 v0.8.4 attachments)。
> 统计:confirmed=6 / refuted=3 / minors=18。

# v0.8.5 arch-refactor.md 评审报告(评审主席合成稿)

## 总评

被审文档 `docs/versions/v0-8-5/arch-refactor.md` 的**核心主张成立**:把 D1–D6 / P1 / P3 / F10 这批需求收敛为「补全两个既有核心概念 Harness 与 Session」、不造新顶层概念,是正确且克制的组织方式。文档对代码的锚定整体扎实——5 个 lens 抽查了 30+ 处行号引用,绝大多数命中或仅 ±1 漂移;对 PRD 提出的三处修订(harness per-daemon 单例、transport 单轴、tokenUsage 改 dispatcher)在代码上全部站得住,且比 PRD 原文更贴合真实接线(尤其纠正了 PRD「每 bot 缓存一 adapter」这个对 chat 路径本不存在的前提)。**红线合规 lens 全过,无任何红线违反**。

但文档有 **5 处 major 级问题必须在动工前修正**,集中在两类:

1. **几处「干净接缝 / 纯组装 / 天然获得」的论断与代码现状不符**——D6 External-origin 在现有单向 mcp.sock sink 上拿不到 gateway 注册表;web 路径用独立 wire 类型手工桥接(且已在丢 v0.8.4 的 attachments),并非「WS 同构」;gateway 仍残留两处文档没抓到的 vendor 知识(resume 策略 + 事件去重 cadence);skills 缓存失效依赖的 `skills/changed` 通知在握手里被显式 opt-out,机制断链。

2. **一处与 PRD/dev-plan 明文契约矛盾且未声明**——`SendMessage.options` 的元素类型,arch-refactor 写 `MessageOption`,PRD/dev-plan 写 `ChoiceOption`,文档却把它归为「PRD 留白处的形状收敛」。

这些都不破坏文档的概念骨架、不违红线、不在已知前提下与 PRD 实质矛盾(故无 blocker),但若不修文档,W1/W2/W3 实现者会撞到缺口、或按矛盾的两份规约各做各的。另有 1 处概念表格自相矛盾(§3 vs §2.1)和一批表述精度建议,列入「建议改进」。

**结论:APPROVE_WITH_FIXES。** 5 处 major 修文档后即可动工;它们都是 1–2 处文档修订量级,不改设计走向。

---

## 各 lens verdict

| Lens | Verdict | 一句话 |
|---|---|---|
| 全局概念质量 | CONCERN | 两概念代码锚点扎实、克制原则准入表逐项站得住;主要问题是 §3 概念表把 gateway 自有命令归到 Session 行,与 §2.1「分界即概念边界」自相矛盾(已降为 minor)。 |
| 代码实证 | CONCERN | 7 大论据核心事实绝大多数被代码证实;两处论证夸大需澄清(§1.3「双流双算」、§1.1「daemon 重启泄漏 + forget_client 自愈」),均表述层级,不改设计。 |
| PRD/dev-plan 一致性 | CONCERN | 三处声明的 PRD 修订全部成立;但 `SendMessage.options` 类型与 PRD/dev-plan 矛盾却未列入修订清单(major);修订#1 沿用了 PRD 被代码证伪的「events() 未订阅 tokenUsage」前提。 |
| 红线合规 | PASS | 逐条核对 CLAUDE.md §三,无违反;harness 单例 / kill_on_drop / No-prompt-injection 透传 / 两新 trait 无 default / 不碰 progress SoT 全部正确。仅 3 处 citation 实名建议。 |
| 全局架构演进基础 | CONCERN | 两概念收敛方向正确、作为演进基础合理;但多处「接入成本干净」的论断与代码不符(D6 ingress、skills/changed opt-out、gateway vendor 残留、web 三刻面),应动工前补正。 |

---

## 必改项(major,动工前修文档)

详见结构化字段 `must_fix`。共 5 条,按 wave 顺序:W1 涉 2 条(SendMessage.options 类型、web 平行 plumbing),W2 涉 1 条(D6 ingress),W3 涉 1 条(skills/changed opt-out),跨 wave 1 条(gateway vendor 残留校准)。

每条均经对抗核查(尝试驳倒未成功),evidence 全部在引用行号处逐字命中。其中原 D6 finding 由 blocker 降为 major:文档**确实**在 §2.2(line 158-159)规划了 oneshot 阻塞-回填机制并在 §6-W2 列为真实新增工作,故「按文档无法实现」过强;真实缺陷更窄——文档命名了 oneshot 两端却未规定 `Sender` 如何跨 tokio task 进到 `Arc<Mutex<Gateway>>`,且把整条链路定性为「纯组装」不实。

---

## 建议改进(minor / nice-to-have)

详见 `nice_to_have`。最值得做的一条是 **§3 概念映射表的自相矛盾**(虽降为 minor 但伤文档主旨):§3 行 219 把 `GatewayCommandSpec`/`GATEWAY_COMMANDS`/`register_commands` 列在「Session」行,而 §2.1 开篇即「分界即概念边界」,把这批命令明确归为「gateway 自己的事」(对立于「给 session 的命令」)。对一份以「概念纪律」为主旨的文档,这是名副其实的自相矛盾;一行修复(拆出 chat/gateway 行,或就地标注「此格属 chat 概念,Session 侧仅 handle_directive」)即可对齐。实现完全相同,故仅 minor。

其余为论据精确化:§1.3「双流双算」应改写为「broadcast 多订阅各自重算 + 测试 per-turn 流单流内重复」(生产只有 pump 一条,按 `event_sink` 互斥);§1.1 删「daemon 重启泄漏」半句(跨进程不泄漏,旧子进程随父进程被回收)、把 `forget_client` 标为「需本版补接的钩子(当前零 caller)」;修订#1/§1.3 补一句校正 PRD「events() 未订阅 tokenUsage」为伪(已经 `build_codex_notification_progress_line` 镜像进 progress.jsonl,真实 gap 是「无可同步查询的内存缓存」),免得 W3 第一步白做「确认未消费」;§5 的 citation 实名(`pending-picker`→`nl_admin.rs` per-target pending-confirm map;`outbound ledger`→`outbound.jsonl`/`DurableOutboundRow`);§7.1 锁模型补两点约束(D6 长 await 必须锁外、同步 vendor RPC 锁内串行化记为已知退化)。

---

## 被驳倒的 findings(附录,不构成必改)

- **§1.3「生产双流并发竞态」(原 major)**:文档原文(line 81)已自我限定为「无-sink **测试**路径」并已引 :973,未 literally 声称生产双流;且该 finding 自身对「第三条流」的定性(crate/F-tag/路径性质)三处误植。降为 minor 措辞建议。
- **flow durable plan_decision vs ephemeral PendingInteraction「埋雷」(原 major)**:误归因。文档的「复用同一注册表」明确锚定 ApprovalIR(per-tool-call HITL,本就是阻塞-oneshot 形态,与 External origin 同构);flow 的 plan-level durable gate 文档从未提议挂到 PendingInteraction(两 substrate 在代码上零交叉)。文档 §2.2 的分层(ApprovalIR=语义层 / ChoicePrompt=交互层 / 不合并 / 复用=注册表+回填)是 sound 的。至多补一句防混淆,minor。
- **W1 trait-churn ~20 处 impl「低估 + 触红线」(原 major)**:文档(line 111)是「Fake/claude_tui/codex_app_server/codex_exec/claude_bg」**示例列举**,从未量化声明「仅 5 个 impl」；no-default 的全部代价就是「每 impl 一行显式 stub」,正是 explicit>silent 的预期取舍,且 W1「baseline 不退」gate 已强制所有 stub 落地;finding 自己的 suggestion 也承认「与不动 bg/flow 不冲突」。至多 minor 表述精度(可把示例改成「全工作区 ~24 个 impl 各补一行 stub」)。

---

## 评审主席合成说明

- 跨 lens 去重:tokenUsage「PRD 称未订阅实为已消费」一项在「代码实证」「PRD 一致性」「架构演进」三个 lens 各出一次,合并为单条 minor(置于 nice_to_have)。
- 主席独立复核了 5 个 major 的全部 load-bearing 代码锚点(main.rs:2180-2184 / gateway.rs:118-135 / chat_protocol.rs:57-74 / web_chat_bridge.rs:58-118 / codex_app_server.rs:240 / gateway.rs:284-319),以及 SendMessage.options 的 PRD/dev-plan/arch-refactor 三方原文——全部逐字命中。
- severity 采纳了核查员的调整:D6 blocker→major(downgrade)、GATEWAY_COMMANDS major→minor(downgrade);其余 major keep。最高存活 severity = major,故整体 verdict 不到 REWORK,定 APPROVE_WITH_FIXES。

---

## must_fix 清单(结构化展开,已全部落回 arch-refactor.md)

### 1. §2.2(刻面二 / D6 链路)+ §6-W2

删去 D6 链路的「纯组装」定性。把 External-origin 的 ingress 路径写清:mcp.sock handler 当前只持单向 sink(`handle_mcp_socket_connection` 签名仅 `sink: Option<GatewayEventSink>`,main.rs:2180-2184),`GatewayEventSink` 是单向 `UnboundedSender<GatewayEvent>`,而 `GatewayEvent`(gateway.rs:118-135)只含 id/channel/chat_id/thread_ts/content/kind=Answer|Progress/attachments——拿不到住在 `Arc<Mutex<Gateway>>` 里的 PendingInteraction 注册表。文档已命名 oneshot 两端(handler 持 Receiver / gateway 持 Sender),但未规定 Sender 如何跨 tokio task 进到 Mutex 守护的 gateway。明确补一条新增 plumbing(二选一并写进 §6-W2 真实工作项):(a) 新增 `GatewayEventKind::RegisterInteraction{reply: oneshot::Sender<ChoiceSelection>}` 让 hook handler 经现有 mpsc 把注册请求送进 gateway 消费者;或 (b) 一个独立的 `Arc<Mutex<pending registry>>`,hook handler 与 inbound consumer 共享(不复用 `Mutex<Gateway>`)。并为 D6 配独立端到端验收:注册→入站 resolve→回写 sock。

### 2. §2.2(传输贯通)+ §4(改为四处修订)+ §6-W1

把 `SendMessage.options` 元素类型的契约变更追加为 §4 第 4 处修订点。现状矛盾:arch-refactor.md:175 写 `options: Vec<MessageOption>`(transport 本地新类型),但 PRD prd.md:145 与 dev-plan.md:39 均明写 `Vec<ChoiceOption>`(harness 中立类型直入 transport)。文档却在 line 231 把它归为「PRD 留白处的形状收敛」——这不是留白,PRD 写死了 ChoiceOption。修订理由:保持 channel 轴不引 harness 轴类型(呼应 PRD §4 双轴解耦),分层为 ChoiceOption@harness + MessageOption@transport(此设计更优,应显式声明而非隐藏)。同步修订 dev-plan W1 落点(dev-plan.md:39)与 W1 验收。另:把 line 183「channel 轴在编译期就 import 不到 vendor 轴类型」改为「设计上不让 transport 引 harness 类型(纪律,非编译屏障)」——`ccteam-im` 已依赖 `ccteam-harness`(Cargo.toml:19),gateway.rs:17 已 import harness 类型,故 transport 不 import 是纪律而非编译器强制。

### 3. §2.2(传输贯通)+ 验收 §8 第 4 条

增列 web 路径的平行 plumbing 工作项,并删去/细化「WS 同构」「grep 全 impl:tg/slack/discord/mock/ws」的同构叙事。web 不复用 transport 类型:它有独立 `WebSendMessage`(chat_protocol.rs:69-74,仅 content/recipient/subject/thread_ts)与 `WebChannelMessage`(:57-65,无 selection/attachments);`web_chat_bridge.rs` 的 `send()`(:58-64)手工逐字段拷贝、`to_im_message`(:118)硬写 `attachments: Vec::new()`——连 v0.8.4 已加的 attachments 都没拷,web 出站文件目前已被静默丢。因此新增的 options/selection 不会被 web 继承。明确列出:扩 `WebSendMessage`(+options)、`WebChannelMessage`(+selection)、`web_chat_bridge` send/to_im_message 双向字段映射、浏览器侧 `chat_protocol` chips + chip-click 帧(当前 ServerChatFrame/ClientChatFrame 无 chip/selection variant)。把验收 §8 第 4 条(`{token}:{idx}` 往返)的用例覆盖 web 一路,而非只 TG。

### 4. §1.3(tracker:『D2 的 skills/list 缓存同属此层』)

补一条 skills 缓存失效的前置依赖:文档把 skills/list 缓存归入 harness 运行态层并背书 PRD「skills/changed 通知失效缓存」,但 `skills/changed` 在 codex adapter 的 initialize 握手里被列入 `optOutNotificationMethods`(codex_app_server.rs:240)——服务端会抑制该通知(codex 侧 `should_skip_notification_for_connection` 语义),失效永不触发,缓存只增不失效,导致会话中途 skill 增删改 stale-forever。明确写:实现 skills 缓存失效前,必须先把 `skills/changed` 移出 codex_app_server.rs:228-244 的 opt-out 列表;并相应处理 `translate_notification`(该通知现会落到 line 827 的 `other =>` warn 分支,需新增臂消费)。若改走 TTL/每次重取规避,则 PRD/dev-plan 所述「skills/changed 失效机制」本身需改。(注:原 finding 旁注「line 818 也列入忽略集」不准,line 817-820 的显式忽略集不含 skills/changed,但核心证据 line 240 独立充分。)

### 5. §0 总览表 / §1.4(『本版 trait 全部增量 = 两个方法』)/ §3 映射表

把「新 vendor 最小面」如实校准。文档主张补完 handle_directive 后『gateway 对语义零知识』、新 vendor 最小面 = handle_directive + thread_status + 现有 5 方法;但 gateway 仍残留两处 vendor 知识:(1) resume 路径按 vendor 分流的恢复策略 match(gateway.rs:284-319,Claude arm 有 resume-fail→start_thread reattach 的兜底,Codex arm 没有;claude_tui.rs:636-642 证实这是 adapter→caller 契约)——新 vendor 走到 resume 会撞到无对应 arm;(2) `async_event_text`(gateway.rs:1341-1362)及 `event_text`(:1468-1485)隐含的『drop ItemUpdated delta、keep ItemCompleted』跨 vendor cadence 假设——这是隐式契约非显式 match,把最终文本只放 ItemUpdated 的未来 vendor 会被静默丢消息,且无 trait 方法可覆盖、无编译信号(恰是文档 §1.4 自称要在类型层杜绝的『静默降级』病)。在 §0/§3 把这两处列为当前 vendor 泄漏点;cadence 一项更建议上移为 adapter `events()` 的『final-only』契约,从 gateway 移除该隐式假设。


## nice_to_have(已择要落回)

- §3 概念映射表自相矛盾(降为 minor 但伤文档主旨,强烈建议改):行 219 把 GatewayCommandSpec/GATEWAY_COMMANDS/register_commands 列在「Session(可被指令)」行,而 §2.1 开篇『分界即概念边界』把这批命令(/new /use /cd /sessions /projects /newproject /pair /help)明确归为『gateway 自己的事』。代码佐证归类:is_gateway_command(gateway.rs:380-385)+ handle_command(:461-551)在路由层拦截,不经 GatewaySession/ThreadHandle。修复:§3 表拆出单列『chat/gateway(命令可发现性 + 菜单注册)』一行,或就地标注『此格属 chat 概念,Session 侧仅 handle_directive』。实现完全相同,故 minor。

- §1.3『双流双算』论据精确化:文档(line 81)已限定『无-sink 测试路径』并引 :973,但『两条流都带副作用…直接竞态』易被误读为生产双流并发。实际生产期 pump(:733)与 per-turn 流(:973)按同一 event_sink 二值互斥(生产只有 pump 一条);真正能证明『副作用挂流内必重算』的是 broadcast 多订阅语义(events() 每次返回新 subscribe)。改写为『broadcast 多订阅者各自重算 + 测试 per-turn 流单流内按 turn 重复消费』,结论(状态放 dispatcher)不变。

- §1.1 泄漏论证收紧:保留并强化『单 daemon 多 session = 多实例 = 多子进程』(已足以支撑单例改造);删去/改写『每次 daemon 重启 resume 再造实例泄漏』半句——跨 daemon 重启不泄漏(旧子进程随父进程被 kill_on_drop 回收)。故障半径处把 forget_client(codex_app_server.rs:265-267,全仓零 caller)标为『需本版补接的自愈钩子(当前无 caller)』,而非既有兜底。

- 修订#1/§1.3 校正 PRD 被证伪前提:PRD §3-D2.4/§5.2 称『events() 未订阅 tokenUsage』不准——events() 经 build_codex_notification_progress_line(codex_app_server.rs:502→:1152-1175)已镜像该通知进 progress.jsonl。真实 gap 是『无可同步查询的内存缓存』,不是『未订阅』。补一句:净新增 = 内存 tracker(供 /status、P3 pull),progress 镜像保持不动；避免 W3 第一步白做『确认未消费』、或误删现有镜像逻辑。

- §1.2 / §8-1 补 real-binary smoke 影响:删 APP_SERVER_TRANSPORT_ENV 后,纯设 stdio 的测试删 env 即默认 stdio 成立;但 real-binary smoke(codex_app_server_test.rs:48-56)用该 env 作 stdio/UDS 分支,需改写为按 CCTEAM_CODEX_APP_SERVER_SOCKET presence 判定(对齐 resolve_codex_transport 单轴),否则 UDS 臂无法再被驱动。§8 增一条断言 raw_extras.transport == resolved 真值(无 socket env ⇒ "stdio"),补回 PRD §3-F10 要的 factory→stdio 钉死。

- §5 红线项 citation 实名(论点均 sound,仅 citation 落到真实代码):『既有 pending-picker』→ nl_admin.rs:196 的 per-target pending-confirm map(ccteam-im/src 内无 pending-picker/PendingInteraction 实现物);『outbound ledger』→ outbound.jsonl / DurableOutboundRow(daemon.rs:842)。

- §5『resume-by-id 不受影响』收紧:补精度限定——resume 恢复已落盘历史;forget_client/kill_on_drop 触发时进行中 turn 会被中断且不自动续跑(session 身份/历史可恢复,in-flight turn 需用户重发),属崩溃恢复语义,非红线所指主动 kill。与 §1.1 故障半径 bullet 交叉引用。

- §1.4/§2.3 钉死两个 no-default 方法的 bg『拒绝姿势』:handle_directive bg → Rejected{reason}(DirectiveOutcome 合法值,非 HarnessError);thread_status bg → Ok(ThreadStatus::default())。现状先例 codex_exec.rs:567-575 对 SystemDirective 返 Err(SubmitFailed),易让实现者混淆该返 Err 还是 Ok-空;在 §6-W1 点明 bg 两 stub 的返回形态。

- §7.1 锁模型补约束(不把整条锁模型笼统延期):(a) handle_directive 内任何可能长阻塞的交互(尤其 D6 oneshot await)必须锁外完成,锁内只做注册——否则 600s 级 External await 落锁内会死锁整个 daemon 入站(handle_message 全程持 Mutex<Gateway>,daemon.rs:473-484);(b) 承认同步 vendor RPC 在锁内串行化跨 chat,记为已知退化 + 给出超时常量。若 D6 走共享 registry,说明用独立锁、不复用 Mutex<Gateway>。

- §2.1 SystemDirective caller 名单分层:把『真实构造/匹配点』(codex_app_server / codex_exec:567 / claude_tui:524 / gateway:1427)与『注释提及』(session_recovery.rs / turns_mirror.rs / ccteam-flow/workflow.rs 均为 doc-comment)分开,并标注 web_chat_bridge.rs:261 为 test 模块的 FakeAdapter stub(非生产 caller),避免误判工作量分布。

- P3 实现提醒(可补 §2.3):turn/completed 的 usage 字段对真实 wire 恒为空(translate_notification 注释已说明 Turn 对象无 usage),tracker 的 usage 缓存必须来自 thread/tokenUsage/updated 这条独立通知,不能从 TurnCompleted 事件取。

- §2.2 token 纪律钉死:idx 用十进制序号;token mint 时排除 ':' 字符(注册表按第一个 ':' split);对 idx 上界(选项数)做校验防越界反查 panic。


## 演进基础判断(evolution_foundation)

这次小范围重构**确实为整体架构演进打下了基础**,方向正确——把 D1–D6/P1/P3/F10 收敛为「补全 Harness(vendor 执行引擎)+ Session(harness 长会话)两个既有概念」,而非各自造类型,使 v0.9+/flow 落地时能直接继承,这是文档最大的价值。但「打基础」是否成立,取决于几个 must_fix 边界现在是否钉死。

【继承路径(锁定后 flow/新 vendor 怎么承接)】
1. **Harness 从『可丢弃 trait object』抬升为『每 daemon×vendor 长生命周期引擎』**(实例纪律 + transport 构造期属性 + vendor 运行态 tracker + 命令解释权)——这是本版真正的概念塑形(文档『不是 v0.8.5 的发明』措辞略过头,实为对既有轴的抬升,见 nice_to_have)。flow 接管编排后,直接复用这个有状态单例,不必重造 vendor 执行层。
2. **Session 三刻面(有属性/可被指令/能反问)** 落在中立类型层(adapter.rs,与 ThreadEvent/ApprovalIR 同层),flow 的 plan-level 调度可直接读 thread_status、发 Directive,无需感知 vendor。
3. **ApprovalIR 分层**:文档把 ChoicePrompt(交互层)与 ApprovalIR(语义/风险层)分开、不合并,日后 per-tool-call HITL 复用同一 PendingInteraction 注册表(origin=External),这条路径是 sound 的(被驳倒的『flow durable plan_decision 埋雷』finding 系误归因——flow 的 durable plan-gate 走 progress.jsonl SoT,是另一条 substrate,文档从未提议挂到 ephemeral 注册表)。

【现在必须钉死 vs 可以推后的边界】
**必须现在钉死(否则后续返工/咬人)**:
- **PendingInteraction 注册表的 ingress 契约**(must_fix D6):这是『chat 即时反问』『hook External』『未来 ApprovalIR HITL』三者共用的归一点,是演进的关键接缝。现在只钉了 oneshot 两端、没钉 Sender 如何跨 task 进 gateway——这个洞若 W2 临时补一个 variant 了事,后续 HITL 接入会再撞同一处。建议现在就把共享 registry / GatewayEvent::RegisterInteraction 的归属定死。
- **cadence 契约归位**(must_fix #5 的 (2)):把 final-only 事件去重上移为 adapter events() 契约,是『新 vendor 只动一轴』承诺的真实兑现点;留在 gateway 隐式假设里,等于给每个未来 vendor 埋一个无编译信号的静默丢消息 footgun,直接抵触文档自己的 anti-静默降级 thesis。
- **两个 no-default trait 方法的签名 + bg 拒绝姿势**:trait 形状一旦 W1 落地就是所有未来 vendor 的最小面,现在钉死(含 bg Rejected vs Ok-空 的姿势)成本最低。

**可以推后(文档判断正确,不必本版做)**:
- gateway 锁粒度重构(§7.1,但需补 D6 长 await 锁外约束这一最小护栏);
- per-bot codex 子进程隔离(§1.1,故障半径可接受 + 留了升级路径);
- HarnessSnapshot 与 ThreadStatus 合并(bg/web presentation 遗产,各管各);
- per-session override map 持久化(挂 SavedGatewaySession 扩展字段一跳可达)。

**一句话**:概念锁定的方向对、继承路径清晰;但「干净接缝」的几处论断目前是文档先于代码写出的『理想态』而非现状(D6 ingress / web 三刻面 / cadence / skills 失效)。把这 5 处 major 改成「现状缺口 + 本版真实新增 plumbing」,基础才算真正打牢——否则演进时会在这些被略过的接缝上返工。
