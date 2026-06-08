# v0.8.10 PRD — 核心流程做成生产级(production-grade STABILITY + 高质量 UX)

> **状态:DRAFT / 候选**(user TG 2409:v0.8.9 已在开发;v0.8.10 把核心流程打到生产级,从 md 文章提炼)。doc-first,实现交 dev session,user review + scope 冻结后才动代码。
> **来源**:user 判断「ccteam 现在很脆、非生产级」(TG 2407)+ 本会话实机 bug 流(几乎全在核心环路)+ 从 Matt Van Horn「Every Agentic Engineering Hack(2026-06)」文章提炼的生产要件(见 §三)。
> **目标精炼(TG 927)**:v0.8.10 = **生产级稳定性(STABILITY)+ 高质量用户体验(UX)优化**,两者**同等**。前者已成熟(soak gate D1 + 三类病根),后者(UX)是本版要补的主缺口 —— 见 §六。
> **与 v0.8.9 的关系**:v0.8.9(已在 dev,HEAD `9480ecc`)已含**部分**硬化(rmux 裸字节终端 / UI 收口删 forked+legacy / 死链清理 / 多个实机 bug 已修 + turn-timeout watchdog 现会 **Esc 打断**僵死 turn 并通知)。**v0.8.10 = 把核心环路的可靠性补完 + 把已修 bug 升级成"成类的回归门" + 立 soak gate + 补 UX 维度,不重复 v0.8.9。**
> **代码锚点纪律**:本 PRD 的代码引用一律用 `file.rs::symbol`(symbol 名跨编辑稳定);凡保留裸行号处皆标「as of 9480ecc,实现时重 grep」。实现前先 `git rev-parse origin/dev` 重测基线 + 重 grep 全部锚点(dev session 在并行 commit,行号会漂)。

> ## 🔒 铁律(本版唯一不可动摇的边界,双轴)
> **v0.8.10 只在两条同等的轴上把"已存在的核心环路"打到生产级:STABILITY(零丢 / 零串 / 零静默失败 / 零 STUCK 误报 / resume 精确 / 扛重启·空闲·suspend·断网,默认 rmux × {claude,codex} 下成立,语义按 vendor 分轴见 §二)与 这同一条环路的 USER-FACING 体验(每个失败都变成看得见、读得懂、可操作的信号;用户随时能看到 session 在干嘛 / 卡没卡 / 花了多少;错误信息可操作;上手能回答"然后呢";模型支持被诚实标注)。它只硬化 + 打磨已存在的 surface,零新增用户可达能力 —— 仅两个被明确承认的 micro-exception(§六.5 列出),其余一律出局。**
> **唯一判据**:本版结束后,**用户能做的事的清单不变** —— 变的只是这些事的可靠度与体验。**最锋利的 IN/OUT 测试**:「本版后,用户有没有一件以前做不到、现在能做的新事?」有 → OUT(= feature,出局);「同一件事,现在变得可靠 / 看得见 / 清楚 / 报错友好」→ IN。**每个 UX 条目都要在 §六开头的逐项裁决表里跑一遍这个测试,不靠脚注 fiat。**
> **关键 reframe(消解 UX 与铁律的张力)**:UX 那一维**大部分不是新工作** —— 它是本稿**已有的可靠性不变式的"用户可见的脸"**。「零静默失败」本身就是一条 UX 要求;可观测性(痛点 4)就是「零 STUCK 误报」对用户的可见面。所以 UX **折叠进已经在碰这些 surface 的交付物**(在 D5/D6/D4 上盖一条跨切 UX 验收 band + 一个聚焦的可观测性交付物),**不**另开一个无底的「UX pass」。
> **OUT-gate 不是 byte-identical(纠 audit 误解)**:§九 的"四样不变"是**条目集不变**(GATEWAY_COMMANDS 名集 / web route 路径集 / CLI 子命令名树 / AgentVendor 变体集),由 guard 测试钉死「集合不增长」 —— 不是文件字节不变(D8/D9 必然要在这些文件附近改文案/加分支,那是合法的)。它只是「无新用户可达能力」的**一个必要非充分**机械检查,配合 §六裁决表 + 「新增 user-facing 串只能是失败/上手消息」的 grep 断言一起守铁律。

---

## 〇、一句话

把核心环路 —— **inbound(IM/web)→ spawn/resume session → agent 干活(可 HITL)→ turn 完通知 → 人 steer** —— 打到"无聊地可靠 + 用着顺手":一条 golden-path **soak**(按 vendor 分轴)量稳定,一组 **UX 门**量体验。不是加功能,是让这条**已有**的路在真实失败态下**零意外**、对用户**零困惑、失败时看得见**。

## ★ 本版具体做什么(交付物 D1–D9)—— 直白版

> v0.8.10 不做新功能,做的是下面这 9 样「修 + 测 + 收口 + 打磨」。每样都可落地、可验收。下文 §一–§六 是它们的 spec/细节;这里是"到底写什么代码"的清单。**D1–D7 = STABILITY,D8–D9 = UX(本版同等的另一维)。UX 不是平行第三条线 —— 它盖在 D4/D5/D6 触及的 surface 上 + 一个聚焦交付物,见 §六开头的"折叠"原则。**

- **D1 · golden-path soak 测试框架(本版核心交付物 + 完成门)**:写一套**自动化 e2e + fault-injection harness**,跑核心环路(消息→`cd`→`init`→spawn→聊 N 轮含 1 次 HITL→断开→重连→resume→stop),并注入故障。**先盘点复用既有 harness,不另起炉灶** —— 本仓已有 `claude_tui_reattach_test.rs` / `claude_tui_resume_test.rs` / `rmux_backend_reconnect.rs` / `rmux_backend_session_roundtrip.rs` / `codex_app_server_test.rs` / `smoke_rmux_sdk.rs`(8 个),D1 在它们上**扩展** reconnect/resume/roundtrip 断言,只有 kill-daemon-mid-turn / 杀 app-server socket / 空闲释放再唤醒 / host-suspend / 多轮长跑这几样是**真·新增**。**故障注入分两层(纠 audit:别把不可重复的当硬门)**:① **[CI-fake,确定性,硬门]** kill daemon / 杀 pane / 杀 app-server socket / 空闲释放再唤醒 / 断网(用 `CCTEAM_*_BIN` fake + monotonic-clock fake);② **[real-machine,best-effort,非硬门]** host-suspend + 多小时长跑(机制见 §二)。**= 既是"生产级"的度量,也是 bug 发现器,也是 UX「失败必有可见信号」+「可观测性诚实」两条 band 的断言载体。** 落点:CI-fake 层进 CI;真 rmux + 真 vendor + 真 fault-injection 那部分 **nas-box005 专机跑(短 smoke 是硬门,长跑 best-effort,见 §七 A1)**。**建议自成一个 phase 先建(它最大、又是门、又是 UX band 的挂载点)。**
- **D2 · 收口 backend 抽象(病根 a)**:grep 全仓所有绕过 `ProcessBackend`/`default_backend()` 直连 tmux 的地方,全改走抽象;加一条 **guard 测试**:`tmux` 字面只许出现在白名单文件(`tmux_backend/` 全目录 + `tmux_ops.rs`(`fifo_relay.rs` 在 `tmux_backend/` 下)+ `core/src/tmux.rs`(re-export 层)+ `lib.rs::default_backend` 的 selection 分支 + `*/tests/*` 夹具),其余文件 shell `tmux`(`Command::new("tmux")`/PATH 注入假 tmux 即 panic)就失败。**已知泄漏锚点(钉进验收,皆 as of 9480ecc 重 grep)**:`tmux_ops.rs::capture_pane_tail_from_session`(tmux-only `capture-pane -p`,**注意它在 `tmux_ops.rs` 顶层、不在 `tmux_backend/`**,且经 `core/src/tmux.rs` + `lib.rs::capture_pane_tail_from_session` re-export → `commands.rs` 的 status/peek 路径在默认 rmux 下走错 backend = bug5(peek 污染终端)的同类病根);`screenshot.rs`(`tmux capture-pane -e`,**已是 `CCTEAM_MUX_BACKEND` backend-aware**,D2 只需 guard 确认它确实走 backend 不硬调);`daemon.rs::reconcile_orphans`(`daemon.rs:589` 区,**已是 `CCTEAM_MUX_BACKEND=tmux` gate** 的合法分支,D2 确认列白名单而非误删);`silence_classifier.rs` 的 `tmux capture-pane` **只是注释描述它收到的 payload 文本,无 live shell-out**(白名单/无需动)。**产出 = guard 测试本身即枚举**(任何白名单外的 tmux shell-out fail build);不再写"约 N 处"这种未验证计数当目标(实测 `Command::new("tmux")` 真调点远少于裸 grep 的字符串命中,计数随实现重 grep)。(v0.8.9 修了终端那处;本版系统性扫尾。)
- **D3 · 收口 session 身份(病根 b)**:审计所有按 role(而非 session id)keyed 的残留路径,全部改按 gateway session-map(唯一 SoT);**named 收口对象(皆 as of 9480ecc 重 grep)**:① `claude_tui.rs` 的 `events()/tail_loop/marker/cursor` 四处 key 推导(`key = if sid.is_empty() { role } else { sid }`、`marker_key = sid…or(role)`、cursor 路径)必须**同源、对 roleless 取 sid**(bug6 已修在 dev,但**必须有专测守**,否则下次重构 `events()` 签名又回归);② **`chat_session_reset*` 三个 builder 全部 role-keyed(`progress_bridge.rs::build_chat_session_reset_event` / `build_chat_session_reset_event_with_reason` / `build_chat_session_reset_with_recovery_event`,live emit 在 `claude_tui.rs:685` 区 + hook `chat_progress.rs:184` 区)= 同 bug6 家族的身份漏**,roleless session 的 reset 事件会被空 role 键住 → 加入 D3 收口 + 测试断 reset 事件含 sid;③ **多 session-per-cwd 的 transcript 文件归属(头号脆点,见 §四.b)**。加测试:同 role 多 session 不串 + roleless 端到端有回复 + roleless resume-fail 的 reset 事件带 sid。
- **D4 · 修 state 失同步(病根 c)+ 统一 stall SoT(可落地的机制,纠 audit)**:config↔内存(unknown-project 那类)、progress↔state —— 补单向真相 + 同步点 + **每个状态字段必有生产写入者**(guard 测试:每个 stall/STUCK class 字段都能追到一条 live 写入路径,无孤儿事件)。**关键收口:系统里现有"两套 stall 概念"** —— ①live 生效的 turn-watchdog(`gateway.rs::after_turn_submitted` 区,按 `visible_events`(`gateway.rs:83` 的 `Arc<AtomicU64>`,**进程内**)判空转,v0.8.9 ship);②CLI 显示侧另一套(`commands.rs::stall_verdict` 按纯 age 判 STUCK,15min→STUCK,跨进程读文件)。**两者是两个不同的问题**(watchdog = "这个 in-flight turn 自起以来有没有产出";stall_verdict = "session 静默了多久"),且 `visible_events` 是 daemon 进程内存、CLI 是独立进程读不到 —— **"让 CLI 读 visible_events" 不可行**(要么造新 IPC/新 status RPC = 撞 OUT-gate)。**可行的单一 SoT 机制**:watchdog 把它的判定**落到既有 `progress.jsonl`**(复用既有 chat event taxonomy,**不新增 RPC、不新增 config key、不新增持久字段以外的 surface**),`stall_verdict`(CLI)与 web Status 都读**这条 file-backed 真相** + 既有 `progress.jsonl` last-event-age;CLI 与 web 共用**同一个 `ccteam-core` 分类函数**(不各判各的)。**死链收敛见 §九死代码表**(`marker_reporter` lookup 半 + `CHAT_BOT_MARKER_STUCK` builder 的处置,与 live `silence.observe` WARN 严格区分)。
- **D5 · 边界可靠性工程(带数值边界,纠 audit)**:给每个跨进程/网络边界(`mcp.sock`、gateway outbound、rmux daemon 调用、IM send、SSE/WS)加 **timeout + retry + 幂等**,且**每个边界给出具体 {timeout, max-retries+backoff, 幂等键}**(见 §五.x 边界表);timeout 预算必须 **< 无静默失败的兜底播报截止**(边界挂死前用户先看到失败消息)。**daemon 重启恢复**写测试。**最小测试矩阵(具体化,否则"有测试"不可验收)**:① restart 后 **≥2 个不同 sid** 的 live session 全部按 deterministic pane 名 reattach + `--resume` 续上下文;② dead-pane recreate 失败 → fresh spawn + emit `chat_session_reset{reason}`(不冒充 resume,且该事件带 sid,呼应 D3);③ outbound ledger 在 restart 前有未投递行 → 启动重放且 **at-least-once 不变 at-least-twice**(幂等键去重,测试断重复投递被吞);④ IM 入站在断网窗口内的消息重连后**不丢不重**。这四项进 D1 CI-fake fault-injection(kill daemon / drop network)断言。
- **D6 · 通知可靠(原语 3)+ flake 先定位再修(纠 audit)**:根除 turn 完成通知丢/错投 —— **第一步先定位 flake**:`cargo test -p ccteam-im -- --test-threads=… ` 多轮 stress 跑出**精确的失败测试名** + 判定它是**产品 reliability 缺陷**还是**测试 timing(sleep/poll race)**(本仓已有 `daemon_replays_queued_durable_outbound` / `daemon_replays_ws_outbound` 等过测;间歇 `fail=1` 既可能是产品也可能是 harness 计时);两种情形都在 D6 scope(产品缺陷 → 修产品;harness 计时 → 修测试确定性),**不预设它一定是产品 reliability 洞**。修 **outbound-ledger flake**(本会话工作树跑测试间歇性 `fail=1`,正是这一类,**它就是 §八 基线那个 fail=1**)+ file-send registry gap,并发下测;每个 turn-done 必达对的 chat。**并入"多 session-per-cwd 不串台"**:同一 cwd 起 ≥2 个 session + 期间触发 Task 子 agent,每个 session 回复只投自己 chat(见 §四.b transcript-discovery 脆点)。
- **D7 · 核心环路 bug backlog 清零 + 死代码收口(逐项已核实,见 §九处置表)**:v0.8.9 ship 后 pull dev,把"核心环路里"还没修的 bug 列出来逐个清(实机 dogfood 持续发现的也归这)。**实现前先 pull dev、重列当前未修项**(本会话 dogfood bug 已修的见 §五,作为"病根成类"证据,**不**重复 scope)。**死代码逐项决定删/留**(§九"死代码处置表"已逐项 grep 核实,**每行带真实 caller 集 + 删除验收 = `cargo build --workspace` clean 且 `cargo test` ≥ 基线**,杜绝"按 PRD 删了反而 red build")。
- **D8 · 零静默失败的用户信号(UX,本版同等新维)**:把"失败必有可见信号"红线从"daemon log 里能查"升级成"**用户在 chat/web 当场就看见、是人话**"。枚举核心环路全部失败态(见 §六.1 失败分级表),每个都映射一条**人话**的 user-facing message(中文 + "怎么办"),由测试断言"该失败态→**每个投递通道恰好一条**非空、含 next-step 的消息(IM 一条、active SSE 一条;不是全局一条)";覆盖本会话三类静默坑:no-reply(roleless / hook 缺失 / pane 死)、模型 burning-loop 无可见性、cryptic `hook.sh-not-found`/`unknown project`。**对今天 fail-safe 静默的路径(marker 未会合 / subagent-read 失败),改成 user-facing 必须带"健康但安静的 session 不得误报"的反假阳测试**(见 §六.1 表的"new-emit"列)——「零静默失败」不能反过来制造「零 STUCK 误报」的回归。**复用既有信号种子,不重造**:`claude_tui.rs::MarkerSilenceWatch`(F187 的 `silence.observe` 60s WARN,**今天就是 live 的唯一 marker-静默信号**,只是 log-only)= D8 把它升级成 user-facing 的种子,**不**新发明检测器。**这条是 §一不变式「零静默失败」的用户可见面,盖在 D5/D6 之上,不重复造。**
- **D9 · 上手 + 模型支持 + 可观测性 + 文案打磨(UX,本版同等新维)**:打磨**已有面**的体验(不加新面)—— ① **上手引导**:`init` 后 `commands.rs:258` 的 'next:' 块(现只列几条命令)升级成"最短上手序列 + 一句 roleless/cto/role 何时用 + 指向 usage.md";cto persona(`cto_role.md`)补一节"新用户问该干嘛 → 先给最短上路 3 步";② **模型支持诚实(关键 gap,= 承认的 micro-exception #1)**:非 Claude 模型经 claude CLI = **UNTESTED + 会乱来且无 warning**,补一条 **user-facing warn-once**;**触发判据 = 新建一个 `is_claude_family(model)` 前缀匹配(`claude-*`/`sonnet`/`opus`/`haiku`)`pub` 在 `ccteam-core`(或 cost),vendor==Claude ∧ ¬is_claude_family → 在 spawn 路径(`ccteam-harness`/`ccteam-im`,**非** `pricing.rs`)emit user-facing warn-once(key=(vendor,model-family))**;**绝不复用 `pricing.rs::warn_unknown_model_once`** —— 它私有、在 cost leaf crate、且键的是"pricing 表未知"≠"非 Claude 家族"(会对未来的新 Claude 模型如 `claude-future-99` 误报、又漏掉已定价的非 Claude 模型,语义错);warn-once **模式**可借,**predicate 必须是家族判断**;+ usage/README 加"支持矩阵"(claude×claude=first-class、codex=best-effort、claude CLI 上非 Claude 模型=未验证);③ **可观测性(对应痛点 4,= 承认的 micro-exception #2)**:把 CLI 已有的 STUCK/last-event 判定 + 已有 progress 数据搬到**已有** web Status/sessions 展示(per-session"最近活动 X 前 / working|idle|疑似卡")—— **今天 StatusView 每 session 只渲染 live/idle**(`StatusView.tsx:164` 区),补"活动态 + 最近活动"标签**是一个只读 label 的小新增**(复用既有 `progress.jsonl` 数据、不新增采集端点/页面);④ **错误文案质量**:**核心环路**(非全仓)的 user-facing 错误串过一遍"现象→原因→下一步"基线 + 终端/市场/cost-pill 末端打磨。**这条盖在 D4(STUCK 单一 SoT)之上,可观测性面与病根 c 同源(CLI 与 web STUCK 必须读同一 file-backed 真相)。**

> 一句话:**D1 建度量(分 CI-fake 硬门 + real-machine best-effort 两层)+ 找 bug + 挂 UX band;D2/D3/D4 收三类病根;D5/D6 硬化边界与通知;D7 清核心环路 backlog + 死代码(逐项已核实);D8/D9 把"能跑"打磨成"用着顺手 + 失败看得见"。soak(D1 CI-fake 硬门 + 真机短 smoke,含 UX band)全绿 + UX 门(§七 B)全过 = 本版完成。**

---

## 一、核心环路 + 不变式(永不破)

```
  IM/web 消息 ──▶ spawn 或 resume session ──▶ agent 干活(可 HITL)──▶ turn 完成通知回 chat ──▶ 人给方向 ──┐
       ▲                                                                                                   │
       └───────────────────────────────────────────────────────────────────────────────────────────────┘
```
**不变式(任何一条破 = 非生产级;括注 vendor 适用范围 —— Claude=持久 rmux pane,Codex=`codex exec`+`resume`、无常驻 pane,见 §二)**:
- **零丢数据**(both):每条 turn 入/出都不丢;通知不丢、不错投。
- **零串台**(both):会话间历史/事件严格隔离(v0.8.8 独立 session 的延续);**同一 cwd 下并存多 session + Task 子 agent 时,回复只投自己 chat**(transcript 文件归属正确)。
- **零静默失败**(both):失败必有**用户当场可见**的信号(不出现"看着像在跑、其实死了/没起");信号是**人话**(中文 + 现象 + 下一步),不是 stack trace;**且健康但安静的 session 不得被误报为失败**(反假阳)。← 这条同时是 STABILITY 与 UX(D8 是它的用户可见面)。
- **零误报**(both):STUCK / 状态 不撒谎(不按纯 age 误报);**全系统单一 stall SoT = 一条 file-backed(`progress.jsonl`)真相**,CLI 与 web 读**同一个 `ccteam-core` 分类函数**(不出现 CLI 说 STUCK 而 web 说 live)。
- **resume 精确续**(语义分轴):**Claude** —— 断开重连后回到原地(像 tmux attach,byte-exact reattach + `--resume` 续上下文);**Codex** —— 无常驻 pane 可"杀+attach",等价故障 = 杀 `codex exec` 子进程 / `mcp.sock` 中途,resume = 下一 turn 按 persistent id `codex resume <id>` 重放、不重复投递。recreate 失败时**诚实 reset**(emit `chat_session_reset`,带 sid,不冒充 resume)。
- **扛得住**(both):daemon 重启、空闲释放、host suspend、断网重连 —— 都不丢会话、不串、不卡死;restart 后所有 live session 按 sid 重挂。
- **默认成立**(both):默认 backend = **rmux** + **claude / codex 两 vendor** 下都成立(不靠 `CCTEAM_MUX_BACKEND=tmux` 兜底);**Codex 是架构 best-effort tier**(CLAUDE.md §0),故 Codex 的真机长跑 soak = best-effort 非硬门(见 §二 / §七 A1)。
- **(UX)零困惑**(both):用户**不必读代码/日志**就知道:现在在哪个 project/session、它在干嘛、卡没卡、花了多少、下一步该做什么、失败了怎么办。← 这条是痛点 4 的可见面,由 D8/D9 兑现。

## 二、golden-path SOAK(本版的"完成"门 · STABILITY · 分 vendor 轴 + 量化参数)

一条端到端 soak,绿了才叫生产级:
> 一条新手机消息 → `cd` → `init` → spawn → 聊 **N≥5 轮**(含一次 HITL)→ **断开** → **重连** → resume(验原地)→ stop。
> 全程穿过:**daemon 重启** + **空闲释放再唤醒** + **host suspend** + **断网重连** + **杀 pane / 杀 app-server socket**;**默认 rmux** + **claude 和 codex 各一遍**。

**故障注入分两层(纠 audit:不可重复的不当硬门)**:
- **[CI-fake,确定性,硬门]**:kill-daemon-mid-turn / 杀 pane(Claude)/ 杀 `codex exec` 子进程 + `mcp.sock`(Codex)/ 杀 app-server socket / 空闲释放再唤醒 / 断网(monotonic-clock fake 推进"空闲"与"超时",`CCTEAM_*_BIN` fake 驱动 vendor)。
- **[real-machine,best-effort,非硬门]**:**host-suspend**(机制:对 daemon + 其子进程组 `SIGSTOP` 持续 **T≥10min** 再 `SIGCONT`,或在 nas-box005 上 `systemctl suspend` + RTC wake;pass 判据 = **唤醒后所有 live sid 在 N≤30s 内 reattach** + **冻结窗口内的入站消息唤醒后 exactly-once 投递**)+ **多小时长跑**(取代"必须 24h 墙钟":硬门用 **M≥50 次 spawn/resume/notify 循环**的加速变体;真·24h 连跑 = 发布后 dogfood 确认,**非 per-ship tag blocker**)。

**断言(分轴)**:零丢数据 / 零串台 / 零静默失败 / 零 STUCK 误报 / **resume 精确(Claude=byte-exact reattach + 上下文续;Codex=下一 turn 按 persistent id 重放、不 dup)** / 通知每次到对的 chat。**"resume 精确"的可检观测(纠 audit 不可证)**:断开前发一条含可回指内容的 user prompt,重连后 agent 能正确回指(真 vendor 层)+ transcript 末 K 行在 reattach 后字节连续(Claude);**CI-fake 层只断 plumbing(pane 名/会话 id/事件不丢),不断上下文保真(fake bin 无真上下文)**。

> **额外注入态(本版补,皆真机才暴露)**:① **同一 cwd 起 ≥2 个 session**(各自 sid)+ 期间触发 Task 子 agent → 零串台、零静默;② **marker 滞后/丢失**(删 active-session-id marker 后发 turn)+ **transcript 轮转**(`/compact`)→ self-heal(下一 user-prompt 重写 marker 后恢复)+ **持续不会合才报一条"未在产出回复"**(不在健康但安静时误报);③ **roleless 分支**(NewSessionModal 的"无角色/裸 claude"选项)至少跑一遍 + 收到回复;④ **不支持模型不静默烧 token**(裸 claude + 非 Claude 模型 → user-facing warn + watchdog Esc 命中)。

就在 **nas-box005 dogfood** 真跑(本会话的 bug 流证明真机才暴露问题:WSL2/inotify-busy 沙盒跑不出 marker rendezvous / transcript 震荡类 bug)。**reliability + UX「失败必有信号」+「可观测性诚实」都用真 e2e + fault-injection 验,不止单测。** 确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)那层进 CI(硬门);真 rmux × 真 vendor 的**短 smoke(≤1h,一轮 restart+suspend+netdrop,Claude)= 硬门**,真机长跑 + Codex 真机长跑 = best-effort(沙盒不计入,见 CLAUDE.md §六)。

## 三、从文章提炼的生产要件(md → 可靠性 requirement)

作者手搓的**单人版**之所以被他信任,是因为几个原语 rock-solid。ccteam 这几条**都已有"功能"** —— v0.8.10 = 把它们打到同等可信:

1. **会话扛断网/断连/重启,reconnect 精确续**(他:tmux for airplanes「断 20 分钟,重连 attach,原地」+ Mosh 抗烂网)→ ccteam 的 **rmux 持久会话 + resume-by-id 必须同等可靠**(**注:此原语的"持久 pane + attach"直接对应 Claude;Codex 的等价是 `codex resume <id>` 重放**)。**= #1 信任原语**,soak 的核心。
2. **任何 session 随时随地可达、可 steer**(他:remote-control-every-window,手机接管 live run)→ ccteam 的 **gateway**(IM/web 到每个 session)。
3. **turn 完成可靠通知到对的 chat**(他:Stop sound hook「知道哪个 session 完了」)→ ccteam 的 `chat_turn_completed`→IM,**绝不丢/错投**(本仓已有 outbound-ledger flake + file-send registry gap = 正是这类,必须根除)。
4. **无人值守**(他:skip-permissions + "be the signal")→ 不被权限弹窗卡死;**HITL 不 600s 楔住**;skip 默认顺滑;**turn 僵死有 watchdog 兜底(v0.8.9 已落地:Esc 打断 + 通知)**。
5. **一条消息起一个会话,低摩擦**(他:terminal-defaults-into-Claude / email→session)→ `/new` + spawn-on-demand 必须秒起、不"unknown project"、不留死 pane。**(UX 延伸:低摩擦不仅是"能起",还是"用户知道怎么起、起完知道下一步" —— 见 §六.2 上手。)**
6. **入口有门**(他:allowlist + DKIM/SPF drop-before-session)→ web-token + IM pairing/allowlist 的鉴权稳。
+ **元课:简单 = 可信**。他的 rig 小,每块都身经百战(tmux/Mosh/一个简单 daemon)。生产级来自**少而硬的原语**,不是铺功能 —— 这正印证"先硬化核心,别加功能"。**v0.8.10 的纪律就是这条。** UX 同理:**打磨少数核心面到顺滑**,不是铺新面。

## 四、系统性病根 + 收口(reliability engineering)

本会话 bug 实证,病根就 3 类(修"类"不修个例;括注是代码佐证,皆 as of 9480ecc 实现时重 grep):

- **(a) tmux 硬编码在默认 rmux 下露馅**(BUG-1 project-stop / BUG-5 session-ls-codex / BUG-6 web-terminal / bug5 peek `with_ansi`)→ 收口:所有路径走 `ProcessBackend` 抽象,禁止绕过去直连 tmux。**佐证**:合法 tmux 字面只该在 `tmux_backend/`(`tmux_ops.rs` 顶层 + `fifo_relay.rs`)、`core/src/tmux.rs`(re-export)与 `lib.rs::default_backend` 的 selection 分支;`tmux_ops.rs::capture_pane_tail_from_session`(tmux-only,经 `core/src/tmux.rs` + `lib.rs` re-export 暴露给 status/peek)+ `daemon.rs::reconcile_orphans`(`daemon.rs:589` 的 `CCTEAM_MUX_BACKEND=tmux` gate,合法但要白名单)是关键锚点;`screenshot.rs` 已 backend-aware(guard 确认)。→ **D2 guard 测试**钉死边界 + 钉死 capture_pane peek 路径(PATH 注入假 tmux,白名单外被调即 panic)。
- **(b) per-session 身份漏**(BUG-3 串台 / bug3 web 新 session 显示上个 session 历史 / bug6 roleless 无回复)→ 收口:**gateway session-map = 唯一 SoT**,历史/事件/终端/列举全按 session id。**佐证 + named 收口对象**:① `claude_tui.rs` 的 `key`/`marker_key`/cursor 路径 + `marker_reporter::lookup(slug, role)` 都是 role-fallback 残留(bug6 已修但需专测守 roleless 取 sid);② **`chat_session_reset*` 三 builder 全 role-keyed**(`progress_bridge.rs::build_chat_session_reset_event{,_with_reason,_with_recovery}`,live emit `claude_tui.rs:685` 区 + hook `chat_progress.rs:184` 区)= 同家族身份漏,roleless reset 被空 role 键住 → 收 sid;③ **【头号脆点,本版必须点名】同 cwd 多 session 的 transcript 发现机制 = 双半 rendezvous**:`transcript_tail.rs::discover_active_session`(挑 `~/.claude/projects/<encoded-cwd>/` 下 **mtime 最新**的 jsonl)+ `chat_progress.rs::refresh_active_session_marker`(hook 写 active-session-id marker:路径键=ccteam sid、内容=Anthropic session UUID,两个 ID 层)这一对。tail 读侧或 hook 写侧任一滞后 → marker 永不会合 → **健康 pane 也静默**(本会话 bot-静默 真因)。同 cwd 并存多 session + Task 子 agent 时,mtime-newest 会在 main/subagent/sibling 间震荡 —— 现靠 `transcript_tail.rs::is_subagent_jsonl`(读首行找 `"type":"agent-setting"` 子串)启发式兜底,schema drift 一变就漏(现有 `discover_skips_subagent` 测试仅覆盖 2 文件)。**收口边界(纠 audit:守 R6 不解析终端输出)**:硬化**不得**加深对 Anthropic 内部 transcript schema 的耦合 —— session→transcript 的 SoT 应优先靠 **ccteam 自己写的 active-session-id marker**(`refresh_active_session_marker` 这条 ccteam 控制的 rendezvous),mtime-newest + `is_subagent_jsonl` 仅 fallback。→ **D3 收身份 + D6 收通知不串 + D1 soak 注入 marker 滞后/transcript 轮转**。
- **(c) state 失同步**(unknown-project / STUCK 误报 / **两套 stall 概念**)→ 收口:config↔内存、progress↔state 单向真相 + 同步点 + 状态字段必有生产写入者。**佐证**:STUCK 那个曾按纯 age 误报(`commands.rs::stall_verdict`,15min→STUCK,**注意是 commands.rs / main.rs 显示侧,不是 queries.rs**);**且** —— live 的 `gateway.rs` watchdog 按 `visible_events`(进程内 `Arc<AtomicU64>`)判空转,与 `stall_verdict` 的 age 判定**是两个不同问题、且 CLI 读不到进程内存** → D4 用 **file-backed `progress.jsonl` 单一 stall 真相** + CLI/web 共用 `ccteam-core` 分类函数收敛(不造新 RPC)。
- **补可靠性工程**:每个跨进程/网络边界加 timeout + retry + idempotency(数值见 §五.x);daemon restart 恢复路径有测试(D5 矩阵);网络韧性(断连不炸、重连幂等)。

### 四.x 失败降级策略(零静默失败的前提,与不变式直接挂钩)

代码里现存三种不一致的降级风格,「零静默失败」不变式必须先定策略才可验收:(a) vendor-seam 的 warn-once(`vendor_compat.rs::warn_unknown_vendor_token` skip + stderr,**用户看不到**);(b) gateway 的 `gateway error: …` 透传到 IM/web(**用户可见但是 jargon**);(c) `transcript_tail.rs::is_subagent_jsonl` / read 失败的 fail-safe(**静默当 not-subagent / 静默不前进 cursor**)。本版用一张**失败分级表**(§六.1)统一规定每类失败 `{user-facing 必现 | 仅 warn-once 进 log | fail-safe 静默续}` + user-facing 文案最低质量(说清"发生了什么 + 下一步做什么")+ **凡从 fail-safe 升 user-facing 的行必带反假阳测试**。D5/D6/D8 的边界硬化按此表落地。

### 四.y 非 Claude / 不支持模型的 ROBUSTNESS(新收口,harden 非 expand,= 承认的 micro-exception #1)

本会话"roleless 模型无限 thinking loop"真因之一是 MODEL-side:裸 claude + 非 Claude 模型(deepseek-via-claude)+ 无明确任务。全仓**零 model-support / capability gate**;spawn 路径对"当前 claude CLI 背后挂不支持模型"零感知、零 user-facing warning;watchdog 文案(`gateway.rs:2425` 区)*提到* "the model may be looping" 但那是**事后**兜底。**这是 harden 不是 expand 的边缘案例 —— 诚实承认它新增了一个(很小的)用户可见信号(模型适配性提示),作为"把 silent-misbehavior 变 labeled-limitation"的两个 micro-exception 之一(见 §六.5)**。**不**新增模型适配(`AgentVendor` enum + adapter 集不变),只对*已存在*的"裸 claude + 任意模型"路径补**事前可见信号 + 文档支持矩阵**(D9-②),且 watchdog Esc 命中纳入 D1 注入态断言(既是"零静默失败"也是 onboarding 清晰度)。**判据是模型家族**(`is_claude_family`),与角色无关(role-bound 的非 Claude 用法同样提示)。

### 四.z 红线对齐:空项目 CLAUDE.md/AGENTS.md scaffold = 已被批准的受限例外(纠 audit)

本会话已修的"空项目 scaffold"(`projects.rs:364` 写 `@AGENTS.md`,测试 `projects.rs:1206`/`:1249` 守)**表面上**与 CLAUDE.md §三 / tech-design R8「ccteam 不生成/桥接项目 CLAUDE.md/AGENTS.md」冲突。**核实**:代码注释标注「v0.8.9(owner decision)」,且 scaffold **仅在项目既无 CLAUDE.md 又无 AGENTS.md 时触发、永不覆盖、幂等**(测试 `:1249` 守"已有 CLAUDE.md 则不动、不建 AGENTS.md")。**裁定 = 这是一个被 owner 明确批准的、受限的红线例外**(empty-project-only / never-overwrite / idempotent),不是失控破线。**本版的对齐动作(in-scope 文档任务,挂 D7)**:在 **CLAUDE.md §三 + tech-design R8 显式刻入这条 carve-out**(否则 D7/§九 的死代码清理可能把 scaffold 误删、反而破核心环路 roleless 启动),并在 §七 加断言守 scaffold 仍 empty-project-only + 永不覆盖(对齐 `:1265` 测试)。

## 五、bug backlog 分级(核心环路优先)

把本会话 + 后续实机 bug 列表,按"是否在核心环路"分级:核心环路 bug = block;功能 bug = 等。**本会话 dogfood 已修(在 dev,不再 scope,仅作"病根成类"证据)**:
- **bug1** hook.sh-not-found(web/daemon create 没 materialize hook.sh → `ensure_ccteam_home` 进所有 create 路径)——病根 c + UX(cryptic 文案)。
- **bug2** CLI-`ccteam init` 的项目在 web 新建 session 列表里**不可见**(列表从 sessions 派生而非 config.yaml 注册表)——病根 c。
- **bug3** web 新 session 显示**上个 session 的历史**(per-sid 状态挤在一个长命组件 → `<SessionView key={sid}>` 修)——病根 b。
- **bug5** `ccteam internal peek` **污染用户终端**(rmux capture 忽略 `with_ansi=false` → 倒出 raw mouse/alt-screen escape)——病根 a。
- **bug6** roleless session **零回复**(`ClaudeTuiAdapter::events()` 只在 `if !role.is_empty()` 才起 transcript tail loop,已修,修法 `key = if sid.is_empty() { role } else { sid }`)+ roleless 模型无限 thinking(MODEL 侧)——病根 b + UX(no-signal + 模型支持不清)。
- 其它已修:**rmux dep pin 升 0.5**(**注:byte API 在 rmux 0.3.1 就有,逐字节保真终端**不依赖**这次 bump;Cargo.toml 顶部 lines 13-14 仍有 "0.3" 的过期注释,顺手清 = trivial harden**);build.rs npm-install-on-lockfile-change;空项目 CLAUDE.md/AGENTS.md scaffold(= §四.z 受限例外);`.ccteam/`→项目 .gitignore;turn-timeout watchdog 现会 **Esc 打断**僵死 turn + 通知。

**v0.8.10 收口剩余 + 继续 dogfood 新发现的核心环路 bug**(D7;实现前先 pull dev、重列当前未修项)。**已修项只作"病根成类"证据 + 升级成回归门(§七 B2),不重复 scope。**

### 五.x 边界可靠性表(D5,每边界给定数值,纠 audit)

> "有 timeout/retry" 不给数值 = 不可验收。每行 {超时预算, 最大重试+退避, 幂等键};超时预算必须 < 无静默失败的兜底播报截止(否则边界先挂死、用户看不到失败消息)。具体数值实现时定标,本表给**必填字段 + 不变式**:

| 边界 | 超时预算 | 重试/退避 | 幂等键 |
|---|---|---|---|
| `mcp.sock`(MCP Unix socket 调用)| 有界(< 播报截止)| 有限次 + 退避 | 请求 id |
| gateway outbound(IM 投递)| 有界 | 有限次 + 退避 | turn-id / 通知 id(去重,at-least-once 不变 at-least-twice)|
| rmux daemon 调用 | 有界 | 有限次 | pane/session id |
| IM send | 有界 | 有限次 + 退避 | 出站 ledger 行 id |
| SSE / WS | 有界(心跳)| 重连 | 序号 / cursor |

## 六、UX 维度(本版主缺口 · 高质量用户体验优化)

> 原稿 STABILITY 强、UX 缺。本节是补的那一维,**与 STABILITY 同等**。**铁律:全是打磨已有面,不加新面 —— 仅两个被明确承认的 micro-exception(D9-② 模型适配性 warn + D9-③ 每 session 活动态只读 label),其余出局。** 每条都映射本会话 UX pain,detail 末标 polish-not-feature 边界。
> **折叠原则(避免双算)**:UX **不是平行 D-stream** —— D8(失败信号)= 不变式「零静默失败」的用户可见面,盖在 D5/D6 上;D9-③(可观测性)= 病根 c「state desync」的用户可见面,盖在 D4(单一 stall SoT)上。两章交叉引用、**共用 D1 fault-injection harness 断言「失败必有可见信号」+「健康不误报」**,不各写一套测试。

### 六.0 逐项 IN/OUT 裁决表(纠 audit:每个 UX 条目当场跑铁律的 sharp test,不靠脚注)

| UX 条目 | "用户有没有一件以前做不到/看不到、现在能的新事?" | 裁决 |
|---|---|---|
| D8 失败信号 | 否 —— 把**已有**失败路径从静默/jargon 变 user-facing 人话;种子是已 live 的 F187 WARN | **IN**(可靠性的脸,无新 affordance)|
| D9-① 上手引导 | 否 —— 改 `init` println + cto persona 文案 + 文档对齐 | **IN**(改输出文案,非新 surface)|
| D9-② 模型适配性 warn | **是(小)** —— 用户得到一个以前没有的"模型未验证"提示 | **IN-承认例外 #1**(把 silent-misbehavior 变 labeled-limitation;不阻断、不改 spawn 行为、`AgentVendor` 不变)|
| D9-③ 每 session 活动态 label | **是(小)** —— StatusView 今天只有 live/idle,补"working/疑似卡/最近活动 X 前"是新只读 label | **IN-承认例外 #2**(复用既有 progress 数据、单只读 label、零新端点/页面;若做成实时 streaming 面板 → OUT)|
| D9-④ 错误文案 + 末端打磨 | 否 —— 改既有错误串文案 + 既有面 legibility | **IN**(文案/可读,非新视觉/新页)|

> 任一条若在实现中越过"承认的 micro-exception"边界(如模型 warn 变成"真让 deepseek work"、活动态 label 变成实时面板)= 立即 OUT,回 §九。

### 六.1 零静默失败的用户信号(D8)—— 失败"看得见" + 失败分级表

把"失败必有可见信号"红线落到"用户**当场**看见、是**人话**"。**核心环路失败分级表(每行皆有 user-facing message + 测试断言;`级别`列即 §四.x 策略;`new-emit?`列标该信号是否在今天静默的路径上新增 → 新增者必带反假阳测试)**:

| 失败态 | 现状(静默/cryptic 坑) | 级别 | new-emit? | 目标信号(人话 + 怎么办) |
|---|---|---|---|---|
| roleless session 无回复 | 曾整段静默(events 不起 tail,已修)| **user-facing 必现** | 复用(升 F187 WARN)| 任何 session 起不来 transcript tail → "会话未在产出回复,正在重试 / 请 `/new`" |
| 模型 burning-loop(裸 claude + 非 Claude 模型 + 无任务) | 烧 token 无可见性 | **user-facing 必现** | 复用(watchdog 已 emit)| turn 长时间无产出 → watchdog Esc(已有)+ 明示"疑似空转,已打断";叠加 D9-② 的模型 warn |
| hook.sh-not-found | cryptic 报错 | **user-facing 必现** | 复用 | 人话 + 具体路径 + "已自动修复 / 请重 `ccteam init`" |
| `unknown project`(`/cd`/`/new`,`gateway.rs` 区)| 已是错误但 jargon | **user-facing 必现** | 复用 | 人话 + "该目录没 init?跑 `ccteam init` 再 `/projects` 确认 / 重启 daemon" |
| pane 死 / app-server socket 断 | `gateway error: …`(`daemon.rs` 裸透传)| **user-facing 必现** | 复用 | 人话 + "会话连接断开,正在重连;失败请 `ccteam stop && start`" |
| resume 失败回退 fresh | README 承诺 emit reset | **user-facing 必现** | 复用 | 验证真发 `chat_session_reset{reason}`(带 sid,不冒充 resume) |
| turn 超时 | watchdog 已通知(`gateway.rs:2425` 区)| **user-facing 必现** | 复用(达标范本)| 保持(这条是全仓唯一达标范本)+ 纳入清单做回归 |
| **marker 滞后/丢失** | 健康 pane 静默(头号脆点)| **self-heal-primary,持续不会合才 user-facing** | **new-emit ⇒ 必带反假阳测试** | 先 self-heal(下一 user-prompt 重写 marker);**仅在超过有界重试预算仍不会合**才发"未在产出回复"——**绝不在健康但安静(刚起、还没出 output)的 session 上误报** |
| subagent-read 失败 | fail-safe 静默当 not-subagent | **fail-safe 静默续(保持)** | 否 | 保持(读失败不前进 cursor,不打扰用户;不升 user-facing) |
| vendor 未知字段 | `warn_unknown_vendor_token` 仅 stderr | **仅 warn-once 进 log** | 否 | 保持(forward-compat,不打扰用户) |
| 通知投递失败 | 见 D6 | **不静默丢** | 复用 | ledger 重放 + at-least-once(幂等键去重)|

**验收**:`grep` 得到的核心环路失败枚举,逐条有"该失败→**每个投递通道(IM / active SSE)恰好一条**非空、含 next-step 关键字(`/new`/`ccteam init`/`/projects`/重启/重试)的 message"的单测(确定性 fake 触发);**凡 new-emit 行额外有"健康但安静 session 在 N 秒内不触发该信号"的反假阳单测**;+ nas-box005 一张 **no-silent-failure checklist**(每项手动跑一遍、勾一遍);**断言绝不出现"提交了 turn 但此后 SSE/IM 零事件且无任何失败消息"**。已修 bug6/bug1 各补回归(roleless 必回、hook.sh 缺失必报具体路径)。
**polish-not-feature**:仅升级既有失败路径的文案/必达性,不新增成功路径行为、不新增通道;唯一"新 emit"是 marker 持续不会合那条,且严格 self-heal-primary + 反假阳测试守住「零 STUCK 误报」不被反噬。

### 六.2 上手引导(D9-①)—— "init 之后,然后呢?"

- **`init` 后**:`commands.rs:258` 的 'next:' 块升级成一条带"最短上手序列 + 一句 roleless/cto/role 何时用 + 指向 `docs/usage.md` §6"的引导,不让用户面对空白。**快照测试**钉死该块含最短序列 + 三态一句话 + usage 指针。
- **roleless vs cto vs role 讲清**:一句话说明默认 cto 是管家、roleless 是裸 claude 走项目 CLAUDE.md、work-role 怎么装/切 —— 落在 `/help`、web 新建会话弹窗、usage.md(已有面,补文案);三处表述一致(无矛盾)。
- **cto persona 引导**:`cto_role.md`(fresh user 首次对话对象)现文案偏"指挥/派活",补一节"新用户问'我该干嘛'时,cto 先给最短上路 3 步 + 推荐 work-role 的判据"(grep/快照钉死)。
**polish-not-feature**:改 init `println` 引导文案 + cto persona 文案 + 文档对齐,**零新代码路径、零新 surface**;不是交互 wizard / 新命令 / tutorial 模式(那是新 surface = OUT)。

### 六.3 模型支持清晰度(D9-②)—— 非 Claude 模型经 claude CLI 的诚实标注(承认例外 #1)

非 Claude 模型经 claude CLI 当前 **UNTESTED + 会乱来且无 warning**(本会话最隐蔽痛点之一)。**触发判据(精确)**:`vendor==Claude ∧ ¬is_claude_family(model)`,其中 `is_claude_family` = 新建的前缀匹配(`claude-*`/`sonnet`/`opus`/`haiku`),`pub` 在 `ccteam-core`(或 cost),由 spawn 路径(`ccteam-harness`/`ccteam-im`)消费;命中 → 在 session spawn 时发**一次性、非阻断** user-facing 提示("该模型经 claude CLI 驱动属未验证路径,可能不稳/无限 thinking;异常请 bind 一个 role 或换 supported 模型"),warn-once key=(vendor,model-family),与角色无关。**绝不复用 `pricing.rs::warn_unknown_model_once`**(私有 / cost leaf / 键的是"pricing 表未知"≠"非 Claude 家族":会对 `claude-future-99` 这类未来 Claude 模型误报、漏掉已定价的非 Claude 模型)——只借 warn-once **模式**,predicate 自建。并在 `docs/usage.md` + README 增 **supported-model matrix**(claude-code × claude = first-class、codex = best-effort、claude CLI 上的非 Claude 模型 = 未验证)。
**polish-not-feature(承认例外 #1)**:这是**承认的最小新信号**(模型适配性 warn),把 silent-misbehavior 变 labeled-limitation;**不新增模型支持能力、不改 spawn 行为、不阻断任何路径、`AgentVendor` enum + adapter 集不变**;§九 OUT-gate 把"user-facing 信号类"列为第 5 项,此 warn 是唯一被允许的新增。

### 六.4 可观测性(D9-③)—— "它在干嘛?卡没卡?花了多少?"(对应痛点 4,承认例外 #2)

打磨**已有**观测面的信噪 —— 把 CLI 已有的 STUCK/last-event 判定 + 已有 progress 数据搬到**已有** web 展示:
- **`/sessions`**:每行 `句柄:项目:vendor:role — model · ctx X/Y (Z%)`(已有)→ 收紧 working/idle/stuck 一眼可辨。
- **IM 进度行**:`⏳ working… · 📖 read ×5 · 🔧 bash ×3`(已有,usage.md §6)→ 节流/收尾文案打磨;**"卡住"与"在跑"用不同的字段/串、可程序化区分**(测试断两态串非相等 + 各带必需 token)。
- **Status 页(`StatusView.tsx`)**:**今天每 session 只渲染 `live`/`idle`(`StatusView.tsx:164` 区)** → 补"最近活动 X 前 / 当前态(working|idle|疑似卡)"标签(= 承认的只读 label 小新增);**疑似卡(STUCK)与 CLI `ccteam status` 读同一 file-backed 真相 + 同一 `ccteam-core` 分类函数**(单一 stall SoT,病根 c 呼应,**不出现 CLI 说 STUCK 而 web 说 live**;用同一 fixture 喂两侧、断相等)。
- **cost pill(`CostPill.tsx`)**:点开明细补 per-vendor 拆分(已有 `vendorCostSplit`);`budget_cap_24h=null` / loading 态可读占位(不闪、不裸 NaN)。
**polish-not-feature(承认例外 #2)**:把 CLI 既有判定 + 既有 progress 数据搬到既有 web 展示 + 一个只读活动态 label;**不新增采集端点、不新增页面**。越界阈:若做成实时 streaming 面板 = feature → 守住"读既有 `progress.jsonl` 最近事件 + 活动态标签"边界。

### 六.5 错误文案 + 末端打磨(D9-④)

- **错误文案质量基线(核心环路 ONLY,纠 audit)**:**不做全仓** —— 只把 **§六.1 失败表枚举的核心环路 user-facing 错误串** + named 的 gateway/route 错误(`project not found`/`session not found` 等)统一成"现象→原因→下一步"三件套(中文、含 next-step、不漏 jargon、不空白)。**可选**加一个集中 `user_facing_error` 构造器**仅服务这批枚举的 call site**(不强制"所有未来 Err 必须走它" —— 那是 tech-debt note,不进本版硬 scope)。
- **终端 / 市场 / cost-pill 末端**:已有面的视觉与连接稳健打磨(终端连接不秒断/不乱码、市场预览→装态、cost pill 边界态),**不加新页**;清 3 个 ChatConsole react-hooks eslint warning(`ChatConsole.tsx` 的 `useEffect`/`useCallback` exhaustive-deps,实现时定位精确 file:line)—— **必须用真·依赖数组修正**(NOT `eslint-disable` 抑制注释),且若 effect 重跑时机变化由(既有/新增)vitest 覆盖;终端 + 市场 install 的 **nas-box005 真机验证(见 §九:依赖 ccteam-hub 公开,best-effort)**。

> **UX 红线**:以上全是"已有面好不好用"。**新交互面 / 新 channel / 新能力一律出局**(那是 §九 的"显式不做");两个 micro-exception(模型 warn + 活动态 label)已在 §六.0 裁决表显式承认,不得再扩。

## 七、可量度验收 rubric(production-grade STABILITY + 高质量 UX 的"完成"定义)

> 不要"improve UX"这种含糊话。每条都**可验证**(测试 / 专机手测)。CI 可测(确定性 fake)与真机门(rmux × 真 vendor + 终端/市场目验)**分开标注**,避免 ship gate 卡在沙盒跑不了的项上。

**A. STABILITY 门(soak,§二 重申 + 补全失败态)**
- **A1** golden-path soak(§二):**[硬门]** CI-fake 切片全绿 + **nas-box005 真机短 smoke(≤1h,一轮 restart+suspend+netdrop,默认 rmux × Claude)全绿**;**[best-effort,非 tag-blocker]** 真机长跑(M≥50 循环 / 真·24h) + Codex 真机长跑(Codex 是架构 best-effort tier)。断言零丢/零串/零静默/零 STUCK 误报/resume 精确(分轴:Claude byte-exact、Codex 重放不 dup)/通知必达。
- **A2** 全失败模式注入覆盖:kill daemon、断网、host suspend([best-effort,SIGSTOP/RTC 机制 + 唤醒 N≤30s reattach + 冻结窗口 exactly-once 判据])、杀 pane(Claude)/ 杀 `codex exec`+`mcp.sock`(Codex)、杀 app-server socket、空闲释放再唤醒 —— 每个一个注入用例 + 恢复断言(按 sid 重挂、不串、turn 续/通知)。daemon-restart 专门断言**所有 live session 全部按 id 重挂**。
- **A3** 病根 3 类各有 named guard/回归测试:**(D2)** backend-literal guard(`tmux` 字面只许白名单文件 = `tmux_backend/` + `tmux_ops.rs` + `core/src/tmux.rs` + `default_backend` selection + tests;PATH 注入假 tmux 让白名单外被调即 panic;覆盖 `capture_pane`/peek 路径);**(D3)** 同 role 多 session 隔离 + roleless 端到端有回复 + `events/tail_loop/marker/cursor` 四处 key 对 roleless 取 sid + `chat_session_reset*` 事件带 sid + 同 cwd 多 session 不串 + `discover_active_session` 在 main+sibling+subagent 三 jsonl 共存选对目标(优先靠 ccteam marker、不加深 vendor schema 耦合);**(D4)** STUCK 不按纯 age + 每状态字段有 writer 审计 + **单一 file-backed stall SoT**(CLI `stall_verdict` 与 web 读同一 `progress.jsonl` 真相 + 同一 `ccteam-core` 分类函数,同 fixture 喂两侧断相等)。grep 确认 live 路径无 `if sid.is_empty() { role }` 残留。
- **A4** 每个跨进程/网络边界(§五.x 表)有 timeout(不挂死,且 < 播报截止)+ retry 幂等(不重复 turn/通知)测试;**D5 矩阵四项**(≥2 sid 重挂 / recreate 失败诚实 reset 带 sid / ledger 重放幂等 / 入站断网不丢不重)绿。
- **A5** `cargo test --workspace`(excl ccteam-web)**= 1907 pass / 0 fail(本会话实测,见 §八;CLAUDE.md §一 的 1898 是过期记录,本版顺手对齐)**,本版每 phase **≥ 该 phase 起跑时观测的 pass 数**;**0-flake 确定性 = D6 出口门**(D6 done 后该 suite 连跑 ≥10 次全绿),**不**把"0-flake"当 D6 之前每 phase 的前置(否则循环);+ ccteam-web 229 pass + vitest 不退 + `npm run lint` **0 warning(3 个 ChatConsole 用真依赖修正,非抑制注释)** + clippy 0 warning + `cargo fmt --all --check` 过。**flake 处置**:D6 落地前若 flake 阻塞某 phase,该 flake 测试 `#[ignore]`/标注隔离(named),D6 出口移除隔离。

**B. UX 门(本版同等新增、可测)**
- **B1 · 零静默失败可断言**:§六.1 失败分级表**每个** user-facing 级别的失败态,有"该失败→**每通道(IM/active SSE)恰好一条**非空、人话、含 next-step 的 message"单测(复用 D1 harness 确定性 fake 切片进 CI);**每个 new-emit 行另有反假阳单测(健康但安静 session N 秒内不误报)**;+ nas-box005 no-silent-failure checklist 全勾;0 个 user-facing 失败模式落"静默"分类。
- **B2 · 每个 dogfooding bug 类有回归测试**:bug1(hook.sh materialize)、bug2(config.yaml 注册表派生列表)、bug3(per-sid 隔离 `<SessionView key={sid}>`)、bug5(`capture(with_ansi=false)` strip 所有 ANSI/alt-screen escape)、bug6(roleless `events()` 起 tail + 有回复)各一条回归测试。
- **B3 · 上手可量(单一来源步骤,纠 audit)**:**唯一规范步骤序列(本 PRD 此处定义、别处引用)= install → init → config → start → /pair(+approve)→ /cd(自动 spawn)= 6 步**;`init` 'next:' 块快照测试断言含该最短序列 + roleless/cto/role 一句话 + usage 指针;cto_role.md 新增 fresh-user 引导节(grep/快照钉死);**真机判据 = nas-box005 走一遍到"session 会回复",每一步都被 usage.md 覆盖(零未文档化步骤)、每步有明确反馈(无"然后呢")** —— 计步只是上界,真判据是"无未文档化步骤 + 每步有反馈"。
- **B4 · 模型支持诚实**:`vendor==Claude ∧ ¬is_claude_family(model)` → user-facing warn-once(per (vendor,model-family))有测试断言(IM/web/CLI 至少一条);**`is_claude_family` 为真(如 `sonnet`/`opus`/未来 `claude-future-99`)→ 零提示**(断言不打扰正常 + 不对未来 Claude 模型误报);`deepseek-via-claude` → 必 warn(断言);emit 站点在 spawn/gateway 路径**非** `pricing.rs`;usage.md + README 各含 supported-model matrix(grep 钉死);`AgentVendor` enum + adapter 集 diff = 不变。
- **B5 · 可观测性可断言**:`/sessions` / `GET /api/v1/status` / StatusView / cost pill 的 working/idle/stuck/cost 字段有测试覆盖取值正确(recent-event 非 stuck、真 idle 过阈 = stuck,与 CLI 同分类函数);web 疑似卡有视觉区分且与 CLI `ccteam status` STUCK **读同一 file-backed 真相**(同 fixture 喂两侧、断相等,无 desync);IM 进度行"卡住 vs 在跑"两态串非相等可区分;cost-pill `cap=null`/loading 态各有 vitest 断言(不裸 NaN/不闪)。
- **B6 · 错误文案 lint(核心环路 ONLY)**:§六.5 枚举的核心环路 user-facing 错误串过"现象/原因/下一步"审计 —— **可程序化部分 = 正则**:必匹配 next-step 关键字白名单 ∧ 不匹配 jargon 黑名单(`gateway error`、裸 `{:?}` Debug、` at src/`、`thread '` 等 stack-trace 签名);抽样核心环路出错路径在测试里断言 next-step 关键字存在 + 无裸 Rust Debug;若加 `user_facing_error` 构造器则其单测断三段齐全。

**整体 pass/fail 定义**:**A1–A5 全绿 且 B1–B6 全过** = v0.8.10 完成(真机长跑 + Codex 真机长跑 = best-effort,**不**是 tag blocker)。任一 A 门红(CI-fake soak 不绿 / 真机短 smoke 不绿 / 基线退 / 病根无 guard)或任一 B 门缺(某失败态无信号或误报 / 某 bug 类无回归测试 / 上手有未文档化步骤 / 模型 warn 错判 / 观测性 desync / 文案没过审)= 不发 tag。

## 八、测试基线(实测)

- HEAD `9480ecc`(v0.8.9,dev,turn watchdog interrupts a stalled turn)。
- `cargo test --workspace --exclude ccteam-web 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` = **1907 pass / 0 fail**(本会话实测;间歇见 `fail=1` 一次 = outbound-ledger 通知 flake → 正是 **D6** 要根除/定位的目标)。
- **基线对齐**:CLAUDE.md §一 记 `1898/0` 与本实测 `1907/0` 有 9 测差(随 v0.8.9 ship 后增量);本版 ship gate **两处对齐到实测 1907/0**。
- ccteam-web = 229 pass(+ 4 env-gated `ws_*`)+ vitest 128/128。
- 本版基线**只能 ≥ 起跑实测数**(CLAUDE.md §五 红线:每 wave/phase baseline ≥ 上一);**0-flake 确定性是 D6 出口门,非每 phase 前置**(见 A5)。
- **注**:dev session 在并行 commit,实现前先 `git rev-parse origin/dev` 重测基线 + 重 grep 全部代码锚点(行号会漂)再起手。

## 九、显式不做(本版边界 · OUT 表)

> 唯一判据(§铁律):「本版后,用户有没有一件以前做不到、现在能做的新事?」有 = OUT(两个被承认的 micro-exception 除外,见 §六.0)。
> **CI/PR 软门(机制 = 集合不增长,非 byte-identical;纠 audit)**:版本结束时 guard 测试断言四样的**条目集合**与 v0.8.9 一致(无新增):① gateway 命令名集(`gateway.rs::GATEWAY_COMMANDS`)② web route 路径集 ③ CLI 子命令名树 ④ `adapter.rs::AgentVendor` 变体集。**这四样附近的文案/分支改动合法**(D8/D9 必然要改),门只挡"集合增长"。配套:⑤ 一条 grep 断言「本版新增的 user-facing 串只能是失败/上手/模型-warn 消息,不是新 affordance」。任一条目集增长(除 §六.0 承认的两个例外)= 该 PR block。

| OUT 项 | 理由 |
|---|---|
| email channel / 新 IM 平台 | 新 channel |
| 新 web 页 / 新 nav 入口 / web 内 role/skill 编辑器 | 新 surface |
| 交互式 onboarding wizard / tutorial 模式 / 新命令 | 新 surface(D9 上手仅限改 println + cto 文案) |
| per-session"当前在干嘛"实时 streaming 面板 | 新 surface(D9 可观测性仅限读既有 progress 最近事件 + 活动态只读 label) |
| 新模型适配 / 真让 deepseek-via-claude work | 新能力(D9 模型支持仅限诚实 warn + 文档矩阵) |
| plan-first loop / catalog 扩张 / schedule-cron / ccteam-flow 编排循环上线 / 自主 fan-out | 新能力(属推后编排层) |
| ChatConsole/市场/cost-pill 视觉重设计 | 超出"修到既有 UX baseline";修 legibility/四态/错误可读 = IN,全新视觉 = OUT |
| 新可配置项(config key) / 新 status RPC / 新持久采集端点 | 新 surface(D4 单一 stall SoT 走既有 `progress.jsonl`,不造新 RPC) |
| 全仓错误文案 normalization / "所有未来 Err 必须走 user_facing_error" 的强制 | 无底 scope(D9-④ 仅限核心环路枚举集)|

**死代码处置表(D7,逐项已 grep 核实 caller 集 + 删除验收 = `cargo build --workspace` clean 且 `cargo test` ≥ 基线;纠 audit 的三处 P0 错判已修正)**:

| 死代码 | 核实(as of 9480ecc) | 处置 |
|---|---|---|
| `marker_reporter` 注册表 + `MarkerReporter` trait(`adapter.rs:738` / `lib.rs:53` pub-export)+ `register()`/`lookup()`/`report_marker_*` | **wired-but-unregistered,非纯死**:无 `register()` caller → `lookup()` 恒 None;但 `lookup` 在 `claude_tui.rs::observe_marker:1389` 被调,且 `observe_marker` 有 **6 个 live call-site**(`:1238/:1275/:1288/:1335/:1589/:1602`),其 body **同时**调 live 的 `silence.observe`(F187 WARN)| **外科式删**:移除 `observe_marker` 里的 `marker_reporter::lookup` 半 + 删注册表/trait/`report_marker_*`;**`observe_marker` 是改写不是整删**,**保留 `silence.observe`(F187 `MarkerSilenceWatch` WARN)** 并交给 **D8 作 user-facing 信号种子**。验收:删后 `cargo build -p ccteam-harness` 无 unused 且 `silence.observe` WARN 仍在 |
| `build_chat_bot_marker_stuck_event`(`progress_bridge.rs:232`)+ 其 shape test(`progress.rs:1014`)| builder **无 live emitter**(只测试)| **删 builder + 删其 shape test**(`progress.rs:1014` 区)|
| `CHAT_BOT_MARKER_STUCK` const(`progress_bridge.rs:28`,re-export `progress.rs:34/43`)| **NOT 孤儿**:在 `progress.rs:376` 的**live event-taxonomy match arm**里 + re-export | **仅能与 match arm 一同删**:删 const 必须同步删 `progress.rs:376` 的 arm + re-export(`progress.rs:34/43`)并重验分类器契约。验收:删后 `cargo build -p ccteam-core` 编译(无 "cannot find value CHAT_BOT_MARKER_STUCK") |
| `render_project_settings_agent_team` + `PROJECT_SETTINGS_AGENT_TEAM_JSON` + `write_project_settings_agent_team`(`templates/mod.rs` + `lib.rs:271/272/275` pub-export)| **NOT pub-dead(纠 audit)**:唯一剩余 caller = `crates/ccteam-flow/tests/agent_team_spawn_test.rs:231/264/298`(agent-team-era 死测试)| **删三符号 + 同时删/重写 `agent_team_spawn_test.rs`(named)**(否则 red build 破 §五基线红线);`InitMode::AgentTeam` v0.8.9 已退役、live init 走 `ArtifactDriven`(`projects.rs::bootstrap` 区)。验收:删后 `cargo test --workspace` ≥ 基线 |
| `--restart-team` 残留(`ccteam-flow` `orchestrator.rs:232/1091/1107/1370/1444`(含 user-facing 串 "Run `ccteam start --restart-team {slug}`")+ `workflow.rs:214/217/314/348` + `state.rs:158` 注释)| 在**推后**的 ccteam-flow;**已核实 daemon 不 run orchestrator**(`ccteam-im`/`ccteam-web` 无 `--restart-team` 引用,grep 0 命中)→ 这些串 user 触不到 | **本版不动**(明写"属 ccteam-flow 推后、daemon 不 run、无 live 路径向用户 emit `--restart-team`";注释标注)|
| inbound dead fns(handoff 提及)| 待 D7 pull dev 时核 | pull dev 重核后逐项删/标注,**每项删前 grep 全 caller(含 tests)** |

**本版**不**碰、留作 deferred(已知,见 handoffs/memory,**不**在本版 scope,除非恰好挡 soak)**:make ccteam-hub PUBLIC(**发布动作,非 UX 需求,但它是市场 install 真机验证的硬前置 —— 见下行**)。
**依赖澄清(纠 audit)**:终端 + 市场 install 的真机验证**并入** D1 nas-box005,但**市场 install 真机验证硬依赖 ccteam-hub 公开**(私库无法经 HTTPS github-raw 拉)。故:**终端真机验证 = D1 IN**;**市场 install 真机验证 = best-effort,blocked-on hub-public**(hub 未公开则该项不阻 tag,显式标注、不静默当 IN)。

## 十、流程 & 验收

- **doc-first** → user review 本 PRD → scope 冻结 → dev-plan(落 `docs/versions/v0-8-10/`)→ dev session(workflow + opus subagents,延续既有模式)。
- **phase 编排(建议,延续 v0.8.8/8.9 phase-per-workflow)**:**D1 自成一 phase 先建**(它最大、是门、是 UX band 挂载点);D2–D7 收口 + D8/D9 UX 在其后 phase,用 D1 soak 当回归门。UX **不开平行 track** —— B1/B5 盖在 D5/D6/D4 上,B3/B4/B6 是一个聚焦的文案/清晰度交付物。**D6(flake 定位+修)宜与 D1 同期或更早**,使后续 phase 有确定性基线(否则"每 phase ≥ 基线"在 flake 未定位前难判)。
- **timebox 取舍预承诺(关键纪律)**:若 host-suspend / runner-death 吃时间(v0.8.8 已知 hazard),**纯视觉末端打磨(§六.5 终端/市场/cost-pill 视觉)= 第一个砍**;**"make-failure-visible"(D8)与 soak CI-fake 硬门(D1)= 最后才砍**(它们是 reliability 戴 UX 帽子,不是 cosmetic);真机长跑 + Codex 真机长跑本就是 best-effort(可降级为发布后 dogfood)。
- **验收 = §七 rubric**:A 门(CI-fake soak 绿 + 真机短 smoke 绿,穿全失败态,病根 3 类 guard + 基线 ≥ 起跑实测)**且** B 门(UX 6 条全过);`cargo test`/clippy/fmt/eslint 不退。
- **纪律**:本版每个 PR/commit 只能是"修可靠性 / 收口抽象 / 加 fault-injection 测试 / 打磨已有面的 UX 文案与信号 / 删核心环路死代码(按 §九处置表带验收)"之一;出现"加功能 / 加新面"(过不了 §九 集合-不增长门 + §六.0 裁决,两个承认例外除外)立即 block。

## 十一、变更记录
- **2026-06-07 初版**:v0.8.10 = 核心流程生产级(production hardening)。核心环路 + 不变式 + golden-path soak gate + 从文章提炼的 6 条生产要件 + 3 类病根收口 + "不加功能"铁律。候选,待 user review。
- **2026-06-07 +具体交付物 D1–D7(TG 2415)**:user 反馈"没看懂具体开发什么" → 加 ★ 章把原则翻成可落地工作清单:D1 soak 测试框架(核心交付,e2e+fault-injection,度量+找 bug)/ D2 收口 backend 抽象 / D3 收口 session 身份 / D4 修 state 失同步 / D5 边界 timeout+retry+幂等+daemon 重启恢复 / D6 通知可靠 / D7 核心环路 bug backlog 清零。
- **2026-06-07 +UX 维度 + 验收 rubric(TG 927)**:user 精炼目标 = **生产级 STABILITY + 高质量 UX**(同等)。**拆铁律为双轴**(硬化 + 打磨已存在 surface,零新增用户可达能力;唯一判据 = "用户能做的事清单不变";reframe = UX 是已有可靠性不变式的用户可见脸,折叠进 D4/D5/D6 不另开无底 pass)。新增 **D8**(零静默失败的用户信号 + 失败分级表)、**D9**(上手引导 + 模型支持诚实 + 可观测性 + 错误文案/末端打磨);新增 **§六 UX 维度**(polish-not-feature 红线,映射本会话 UX pain)+ **§七 A/B 双轴可量度 rubric**。
- **2026-06-07 +对抗式 review 终稿(adversarial-review workflow:synthesis DRAFT + 4 份 critique 全部对照 HEAD 9480ecc 代码核实)**:据四份审计(reliability gaps / scope-discipline / measurability / red-line+feasibility)深化并修正,核心改动 ——
  ① **修正 3 处 P0 死代码错判**(原稿会 red-build):`render/write_project_settings_agent_team` + `PROJECT_SETTINGS_AGENT_TEAM_JSON` **NOT pub-dead**(`ccteam-flow/tests/agent_team_spawn_test.rs:231/264/298` 是 live test caller → 删符号必同删该测试);`CHAT_BOT_MARKER_STUCK` **NOT 孤儿**(在 `progress.rs:376` live match arm + re-export → 仅能与 arm 同删,builder/const 分开处置);`marker_reporter` **wired-but-unregistered 非纯死**(`observe_marker` 6 个 live call-site,其 body 含 live `silence.observe` F187 WARN → 外科式删 lookup 半、保留 WARN 并交 D8 作信号种子)。§九处置表每行补真实 caller 集 + 删除验收(`cargo build/test` 绿)。
  ② **拆 cross-vendor soak 语义**(Codex = `codex exec`+`resume`、无常驻 pane,与 Claude 持久 rmux pane 不对称):§一不变式逐条标 vendor 适用、§二/A1/A2 分轴注入(Claude 杀 pane / Codex 杀 `codex exec`+`mcp.sock`),Codex 真机长跑降 best-effort(架构 best-effort tier)。
  ③ **host-suspend + 长跑给机制 + 分层**(纠"不可重复硬门"):host-suspend = SIGSTOP/RTC + 唤醒 N≤30s reattach + 冻结窗 exactly-once 判据,列 real-machine best-effort;"24h 墙钟"硬门改 **M≥50 循环加速变体 + CI-fake 切片**,真 24h 降发布后 dogfood;真机短 smoke(≤1h,Claude)= 唯一真机硬门。
  ④ **UX 门可证伪化**:B3 "≤6 步"单一来源 + 真判据改"零未文档化步骤 + 每步有反馈";B5 "同源"= CLI/web 读同一 file-backed `progress.jsonl` + 同一 `ccteam-core` 分类函数 + 同 fixture 断相等;D8/B1 "恰好一条"改"**每通道恰好一条**";B6 错误文案 = 正则白/黑名单(next-step 关键字 ∧ ¬jargon/¬stack-trace),且**限核心环路非全仓**。
  ⑤ **D4 单一 stall SoT 给可行机制**(纠"读进程内存 visible_events 不可行"):watchdog 落 `progress.jsonl` file-backed 真相,CLI/web 共用 `ccteam-core` 分类函数,**不造新 RPC/新持久 surface**(守 OUT-gate)。
  ⑥ **D9-② 模型 warn 纠 predicate + 诚实归类**:**不复用** `pricing.rs::warn_unknown_model_once`(私有/cost leaf/键"pricing 未知"≠"非 Claude 家族",会误报未来 Claude 模型),改新建 `is_claude_family` 前缀匹配 + spawn 路径 emit;**承认它是 micro-exception #1**(新用户可见信号);D9-③ 活动态 label = micro-exception #2(StatusView 今天只有 live/idle)。新增 **§六.0 逐项 IN/OUT 裁决表**(每 UX 条目当场跑 sharp test)。
  ⑦ **D3 补 `chat_session_reset*` 三 builder role-keyed 身份漏**(`progress_bridge.rs:168/176/185`)入收口 + reset 事件带 sid;§四.b transcript rendezvous 收口加"不加深 vendor schema 耦合(守 R6)"边界。
  ⑧ **新增 §四.z 红线对齐**:空项目 `@AGENTS.md` scaffold 裁定为 **owner 批准的受限例外**(empty-only/never-overwrite/idempotent),本版 in-scope 把 carve-out 刻入 CLAUDE.md §三 + tech-design R8(防 D7 误删)。
  ⑨ **D5 给数值边界表**(§五.x:每边界 {timeout<播报截止, retries+backoff, 幂等键});D6 先 stress 定位 flake(产品 vs 测试 timing 两情形都 in-scope,不预设产品洞)。
  ⑩ **OUT-gate 从 "byte-identical" 纠为 "条目集合不增长"**(guard 测试,四样附近文案改合法)+ 第 5 项 user-facing 串 grep 断言;**A5 基线对齐实测 1907/0**(CLAUDE.md 1898 过期)+ "0-flake = D6 出口门非每 phase 前置"解循环;D1 复用既有 8 个 reconnect/resume harness(非 greenfield);市场 install 真机验证标 best-effort-blocked-on hub-public(终端真机验证 IN);eslint 3 warning 用真依赖修正非抑制;rmux 0.5 注明 byte API 0.3.1 即有、保真不依赖 bump。整体完成定义不变 = **A1–A5 全绿 且 B1–B6 全过**(真机长跑/Codex 长跑 = best-effort 非 tag-blocker)。
