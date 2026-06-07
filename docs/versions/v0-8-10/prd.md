# v0.8.10 PRD — 核心流程做成生产级(production hardening)

> **状态:DRAFT / 候选**(user TG 2409:v0.8.9 已在开发;v0.8.10 把核心流程打到生产级,从 md 文章提炼)。doc-first,实现交 dev session,user review 后动代码。
> **来源**:user 判断「ccteam 现在很脆、非生产级」(TG 2407)+ 本会话实机 bug 流(几乎全在核心环路)+ 从 Matt Van Horn「Every Agentic Engineering Hack(2026-06)」文章提炼的生产要件(见 §三)。
> **与 v0.8.9 的关系**:v0.8.9(在开发)已含**部分**硬化(rmux 裸字节终端 / UI 收口删 forked+legacy / 死链清理)。**v0.8.10 = 把核心环路的可靠性补完 + 立 soak gate,不重复 v0.8.9。**
> **🔒 铁律:本版只加可靠性、不加功能。** 任何"新 surface / 新 channel / 新插件能力"自动出局(那是 expand,本版是 harden)。

---

## 〇、一句话

把核心环路 —— **inbound(IM/web)→ spawn/resume session → agent 干活 → turn 完通知 → 人 steer** —— 打到"无聊地可靠",用一条 golden-path **soak** 量。不是加功能,是让这条路在真实失败态下零意外。

## ★ 本版具体做什么(交付物 D1–D7)—— 直白版

> v0.8.10 不做新功能,做的是下面这 7 样「修 + 测 + 收口」。每样都可落地、可验收。下文 §一–§五 是它们的 spec/细节;这里是"到底写什么代码"的清单。

- **D1 · golden-path soak 测试框架(本版核心交付物)**:写一套**自动化 e2e + fault-injection harness**,跑核心环路(消息→`cd`→`init`→spawn→聊 N 轮含 1 次 HITL→断开→重连→resume→stop),并注入故障:**中途 kill daemon / 断网 / host suspend / 杀 pane**;跑 **默认 rmux × {claude, codex}**;断言不变式(零丢/零串/零静默失败/零 STUCK 误报/resume 精确)。= 既是"生产级"的度量,也是 bug 发现器。落点:确定性 fake(`CCTEAM_*_BIN`)那部分进 CI;真 rmux + 真 vendor 那部分 nas-box005 专机跑。**这是把"生产级"从口号变成可跑的绿灯。**
- **D2 · 收口 backend 抽象(病根 a)**:grep 全仓所有绕过 `ProcessBackend`/`default_backend()` 直连 tmux 的地方(BUG-1/5/6 那类),全改走抽象;加一条 **guard 测试**:非 backend 路径 shell `tmux` 就失败。(v0.8.9 修了终端那处;本版做系统性扫尾。)
- **D3 · 收口 session 身份(病根 b)**:审计所有按 role(而非 session id)keyed 的残留路径(历史/事件/终端/列举),全部改按 gateway session-map(唯一 SoT);加测试覆盖"同 role 多会话不串"。
- **D4 · 修 state 失同步(病根 c)**:config↔内存(unknown-project 那类)、progress↔state(STUCK 字段曾无生产写入者)—— 补单向真相 + 同步点 + 每个状态字段必有写入者;加测试。
- **D5 · 边界可靠性工程**:给每个跨进程/网络边界(`mcp.sock`、gateway outbound、rmux daemon 调用、IM send、SSE/WS)加 **timeout + retry + 幂等**;**daemon 重启恢复**(重启后按 id 重挂所有 live session)写测试;断网重连幂等。
- **D6 · 通知可靠(原语 3)**:根除 turn 完成通知丢/错投 —— 修 outbound-ledger flake + file-send registry gap(已知),并发下测;每个 turn-done 必达对的 chat。
- **D7 · 核心环路 bug backlog 清零**:v0.8.9 ship 后 pull dev,把"核心环路里"还没修的 bug 列出来逐个清(实机 dogfood 持续发现的也归这)。

> 一句话:**D1 建度量 + 找 bug;D2/D3/D4 收三类病根;D5/D6 硬化边界与通知;D7 清核心环路 backlog。soak(D1)全绿 = 本版完成。**

---

## 一、核心环路 + 不变式(永不破)

```
  IM/web 消息 ──▶ spawn 或 resume session ──▶ agent 干活(可 HITL)──▶ turn 完成通知回 chat ──▶ 人给方向 ──┐
       ▲                                                                                                   │
       └───────────────────────────────────────────────────────────────────────────────────────────────┘
```
**不变式(任何一条破 = 非生产级)**:
- **零丢数据**:每条 turn 入/出都不丢;通知不丢、不错投。
- **零串台**:会话间历史/事件严格隔离(v0.8.8 独立 session 的延续)。
- **零静默失败**:失败必有可见信号(不出现"看着像在跑、其实死了/没起")。
- **零误报**:STUCK / 状态 不撒谎。
- **resume 精确续**:断开重连后回到原地(像 tmux attach)。
- **扛得住**:daemon 重启、空闲释放、host suspend、断网重连 —— 都不丢会话、不串、不卡死。
- **默认成立**:默认 backend = **rmux** + **claude / codex 两 vendor** 下都成立(不靠 `CCTEAM_MUX_BACKEND=tmux` 兜底)。

## 二、golden-path SOAK(本版的"完成"门)

一条端到端 soak,绿了才叫生产级:
> 一条新手机消息 → `cd` → `init` → spawn → 聊 N 轮(含一次 HITL)→ **断开** → **重连** → resume(验原地)→ stop。
> 全程穿过:**daemon 重启** + **空闲释放再唤醒** + **host suspend** + **断网重连** + **24h 连跑**;**默认 rmux** + **claude 和 codex 各一遍**。
> 断言:零丢数据 / 零串台 / 零静默失败 / 零 STUCK 误报 / resume 精确 / 通知每次到对的 chat。

就在 **nas-box005 dogfood** 真跑(本会话的 bug 流证明真机才暴露问题)。**reliability 用真 e2e + fault-injection(kill daemon / drop network / suspend / 杀 pane)验,不止单测。**

## 三、从文章提炼的生产要件(md → 可靠性 requirement)

作者手搓的**单人版**之所以被他信任,是因为几个原语 rock-solid。ccteam 这几条**都已有"功能"** —— v0.8.10 = 把它们打到同等可信:

1. **会话扛断网/断连/重启,reconnect 精确续**(他:tmux for airplanes「断 20 分钟,重连 attach,原地」+ Mosh 抗烂网)→ ccteam 的 **rmux 持久会话 + resume-by-id 必须同等可靠**。**= #1 信任原语**,soak 的核心。
2. **任何 session 随时随地可达、可 steer**(他:remote-control-every-window,手机接管 live run)→ ccteam 的 **gateway**(IM/web 到每个 session)。
3. **turn 完成可靠通知到对的 chat**(他:Stop sound hook「知道哪个 session 完了」)→ ccteam 的 `chat_turn_completed`→IM,**绝不丢/错投**(本仓已有 outbound-ledger flake + file-send registry gap = 正是这类,必须根除)。
4. **无人值守**(他:skip-permissions + "be the signal")→ 不被权限弹窗卡死;**HITL 不 600s 楔住**;skip 默认顺滑。
5. **一条消息起一个会话,低摩擦**(他:terminal-defaults-into-Claude / email→session)→ `/new` + spawn-on-demand 必须秒起、不"unknown project"、不留死 pane。
6. **入口有门**(他:allowlist + DKIM/SPF drop-before-session)→ web-token + IM pairing/allowlist 的鉴权稳。
+ **元课:简单 = 可信**。他的 rig 小,每块都身经百战(tmux/Mosh/一个简单 daemon)。生产级来自**少而硬的原语**,不是铺功能 —— 这正印证"先硬化核心,别加功能"。**v0.8.10 的纪律就是这条。**

## 四、系统性病根 + 收口(reliability engineering)

本会话 bug 实证,病根就 3 类(修"类"不修个例):
- **(a) tmux 硬编码在默认 rmux 下露馅**(BUG-1 project-stop / BUG-5 session-ls-codex / BUG-6 web-terminal)→ 收口:所有路径走 `ProcessBackend` 抽象,禁止绕过去直连 tmux。
- **(b) per-session 身份漏**(BUG-3 串台)→ 收口:**gateway session-map = 唯一 SoT**,历史/事件/终端/列举全按 session id。
- **(c) state 失同步**(unknown-project / STUCK 误报)→ 收口:config↔内存、progress↔state 单向真相 + 同步点;状态字段必有生产写入者(STUCK 那个曾"全仓无写入者")。
- **补可靠性工程**:每个跨进程/网络边界加 timeout + retry + idempotency;daemon restart 恢复路径有测试;网络韧性(断连不炸、重连幂等)。

## 五、bug backlog 分级(核心环路优先)

把本会话 + 后续实机 bug 列表,按"是否在核心环路"分级:核心环路 bug = block;功能 bug = 等。v0.8.9 已修一批(BUG-1/2/3/4/5/6 + bug1/bug2);**v0.8.10 收口剩余 + 继续 dogfood 新发现的核心环路 bug**。(实现前先 pull dev、重列当前未修项。)

## 六、显式不做(本版边界)

**不加任何新功能 / 新 surface**:插件市场扩张、email channel、plan-first loop、catalog 等 —— 全**不**在本版(它们是 expand;本版是 harden)。新需求一律记到别的版本。

## 七、流程 & 验收

- **doc-first** → user review 本 PRD → scope 冻结 → dev-plan → dev session(workflow,延续既有模式)。
- **验收 = soak 绿**(穿全失败态,nas-box005 真跑)+ 病根 3 类收口 + bug backlog 核心环路清零;`cargo test`/clippy/fmt 不退。
- **纪律**:本版 PR/commit 只能是"修可靠性 / 收口抽象 / 加 fault-injection 测试";出现"加功能"立即 block。

## 八、变更记录
- **2026-06-07 初版**:v0.8.10 = 核心流程生产级(production hardening)。核心环路 + 不变式 + golden-path soak gate + 从文章提炼的 6 条生产要件 + 3 类病根收口 + "不加功能"铁律。候选,待 user review。
- **2026-06-07 +具体交付物 D1–D7(TG 2415)**:user 反馈"没看懂具体开发什么" → 加 ★ 章把原则翻成可落地工作清单:D1 soak 测试框架(核心交付,e2e+fault-injection,度量+找 bug)/ D2 收口 backend 抽象 / D3 收口 session 身份 / D4 修 state 失同步 / D5 边界 timeout+retry+幂等+daemon 重启恢复 / D6 通知可靠 / D7 核心环路 bug backlog 清零。
