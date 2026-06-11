# v0.8.11 dev-plan(wave 划分)— 协议轴:ClaudeStreamJson adapter + 壳加厚

> 配套 `prd.md`(E1–E5 + §四决策 + §七 v0.9 准备度 5 约束)。本文只定 **wave 切分 / 验收门 / 文件落点 / §七 实现形状映射**;协议事实见 `docs/research/cc-stream-json-protocol.md`,模式参考 `references/alleycat`。
>
> **起跑基线(实测 2026-06-11)**:`cargo test --workspace --exclude ccteam-web` = **1942 / 0**(修了 commit b18aade 遗留的 stale `/model` 测试,见 prep commit 87a614c)· ccteam-web 与 vitest/Playwright 另计。每 wave gate:`baseline ≥ 1942` + clippy `-D warnings` 0 + `cargo fmt --all` 干净。

---

## 〇、架构决策(贯穿全版)

1. **第二 adapter,不换协议**:`ClaudeStreamJsonAdapter` 与 `ClaudeTuiAdapter` 并存,**都实现 `HarnessAdapter` 并 emit 同一 `ThreadEvent`(CanonicalEvent)流**。⇒ gateway `spawn_event_pump`(唯一 turns/progress writer)**逐字不改**地消费两者 —— 这就是 §七 ④「SoT writer 随宿主走」在本版的兑现:写入逻辑留在既有 pump + `turns_mirror`(已在 ccteam-harness,satellite 可复用),adapter 只负责把 NDJSON 翻成 canonical 事件。
2. **live-child 注册表在 adapter 内**(镜像 `CodexAppServerAdapter`):adapter 是 per-vendor 单例,持 `Arc<Mutex<HashMap<vendor_uuid, LiveSession>>>`;`ThreadHandle`(可序列化、扛重启)只存 identity=vendor-uuid + raw_extras(sid/slug/role/project_dir),**不**持 Child。daemon 重启 → live map 空 → resume 走 `--resume <uuid>` 重建(§七 ② 的「消费端不持 Child」由此满足)。
3. **四条缝(§七 ①)各自成模块**,目录 `crates/ccteam-harness/src/execution/claude_stream_json/`:
   - `spawn_spec.rs`(缝①)— 纯函数 argv/env/cwd builder
   - `transport.rs`(缝②)— `StreamJsonTransport` trait + `LocalPipeTransport`(child 内部持有,对外只露 send_line / 订阅 inbound / control 往返);**NDJSON 消费端只见 transport,不见 Child**
   - `protocol.rs` — NDJSON wire 类型(in/out + control)
   - `translate.rs`(缝③)— NDJSON → `ThreadEvent`
   - `mod.rs` — `ClaudeStreamJsonAdapter`(HarnessAdapter impl)
4. **身份映射(§七 ⑤)**:`--session-id` mint 的 `sid ↔ vendor-uuid` 存成**可扩列**结构(不做一次性双键)——本版字段 `{ sid, vendor_uuid, host: "local" }`,v0.9 同结构挂 `Sandbox CR`。
5. **零注入红线**:spawn_spec **永不**产出 `--append-system-prompt`;不发 `initialize.systemPrompt`/`appendSystemPrompt`;role 仅经 `--agent`;空 role = 省 `--agent`。

---

## 一、wave 切分(6 wave,wave-per-phase,每 wave 一 handoff + 一组 commit 直推 dev)

### Wave 1 — E1 核心 adapter(协议引擎,自己写)
**交付**:`claude_stream_json/` 四模块 + `HarnessAdapter` 全 7+2 方法 + fake-vendor(NDJSON 脚本)+ 确定性 e2e。
- spawn_spec:`claude --input-format stream-json --output-format stream-json --include-partial-messages --verbose --replay-user-messages [--debug --debug-to-stderr] [--agent <role>] [--permission-prompt-tool stdio | --dangerously-skip-permissions] [--session-id <uuid> | --resume <uuid>]`;**不带 -p**;`--session-id` 与 `--resume` 互斥。
- transport:child spawn + writer task(mpsc→stdin,行尾 `\n`)+ reader task(stdout 逐行 → broadcast)+ init slot(等 `system:init` 再放行 turn)+ pending-control oneshot 表(req_id 关联)+ stderr drain(debug 弃);stop = 关 stdin 优雅退出。
- translate:`system:init`(命令表/models/session_id)、`assistant`(最终文本 → ItemCompleted/AgentMessage)、`stream_event`(增量,丢)、`tool_use`/`tool_result`(Item)、`result`(TurnCompleted + usage/cost)、`user`(replay 回显 → turns 权威)。**final-only 合同**:最终文本恰好一次走 ItemCompleted。
- HarnessAdapter:start_thread(mint uuid、spawn、登记 live map、等 init)、submit_turn(写 user NDJSON)、events(订阅 transport→translate→ThreadEvent)、resume_thread(`--resume`)、close_thread(关 stdin)、thread_status(init/result usage)、handle_directive(Wave 1 占位:known passthrough;完整 bridge 留 Wave 2)、thread_status。
- **验收(Wave 1 gate)**:fake-vendor e2e:spawn→init 握手→多轮 turn→idle 关 stdin→resume(`--resume`)续→child-death 探测重建;`name()=="claude-stream-json"`;`baseline ≥ 1942`。

### Wave 2 — E1 slash bridge + HITL(自己写)
**交付**:
- handle_directive bridge 三类(known prompt/local 透传为 user text · known dialog(local-jsx)人话拒 · unknown 当文本);命令表来自 init response;**ccteam IM 命令面优先**(`/pair /cd /use /new /role @handle` 在 gateway 层已先拦,adapter 只见 vendor slash);`/compact /new /clear` 透传红线守。
- HITL:`--permission-prompt-tool stdio`;reader 收 `can_use_tool` control_request → 经既有 `permission/ask` 面转 IM `[同意][拒绝]` → 回 `control_response {behavior: allow|deny}`;deny 只挡该次工具不 kill turn;skip session 仍 `--dangerously-skip-permissions`。
- **验收**:fake-vendor 断言 slash 三类逐条 + can_use_tool 同意/拒绝往返(deny 不杀 turn);`baseline ≥ Wave1`。

### Wave 3 — E2 创建面(自己写主干 + 可委派 SPA)
**交付**:
- `protocol` 枚举(`stream-json` 默认 | `terminal`),单一 daemon 级默认常量,web `CreateSessionForm` + IM `/new` 同源;gateway adapter 选择按 protocol(stream-json→新 adapter,terminal→claude_tui)。
- cto 默认 stream-json。
- stream-json session:SPA 隐藏终端 tab + 一句提示;screenshot 工具 + `/screen` 对 paneless session 人话拒绝。
- session schema 预留 `host` 字段(默认 `local`,不暴露 UI/CLI):`GatewaySession` / `SavedGatewaySession` / `SessionView`。
- telegram 插件定点隔离:spawn 经 `.claude/settings.local.json` 写 `enabledPlugins:{"telegram@claude-plugins-official":false}`(只关这一个,合并不动其余)。
- **验收**:创建 default→stream-json adapter;terminal→claude_tui;screenshot paneless 人话拒;settings.local.json 只多这一键;`baseline ≥ Wave2` + ccteam-web/vitest 不退。

### Wave 4 — E3 故障×通道矩阵(自己写)
**交付**:轴参数化夹具(`通道 × 故障`,§七 ③ 预留宿主维):{断网, daemon 重启, pane/child-death} × {terminal(pane), stream-json};`CCTEAM_CLAUDE_BIN` fake 喂 NDJSON 脚本;outbound 不丢不重(ledger 幂等)、reset 事件带 sid+reason、stream-json in-flight 丢失有人话信号。
- **验收**:全矩阵 CI-fake 全绿;`baseline ≥ Wave3`。

### Wave 5 — E4 寻址 + 终端打磨(可委派 SPA/IM 摩擦清单)
**交付**:@handle//use 摩擦清单落地 + turn 完成通知必达对的 chat(继承 D6);web 终端键入回显/resize/重连屏幕一致性基线 + 量化阈值(Q4 此处定数);会话列表活动态 + 新回复指示。
- **验收**:相应单测/e2e;`baseline ≥ Wave4`。

### Wave 6 — E5 文档 + ship gate(可委派)
**交付**:`tech-design.md` harness 节(两通道并存 + 协议→代码指针补 stream-json 行)、`usage.md`、`CLAUDE.md` §〇/§一回填、`README.md`(英文,不含版本进展)、版本归档 `docs/versions/v0-8-11/README.md` + handoff、workspace `Cargo.toml` version → `0.8.11`。
- **验收**:全 ship gate;`baseline ≥ Wave5`;**绝不打 tag**(owner 决定)。

---

## 二、§七 v0.9 准备度映射(逐条落到 wave)

| §七 约束 | 落点 |
|---|---|
| ① E1 四缝预折叠 | Wave 1 模块划分(spawn_spec/transport/translate/mod) |
| ② `protocol` 命名 + 预留 `host` | Wave 3(`protocol` 非 `backend`;schema 加 `host=local`) |
| ③ 故障矩阵轴参数化 | Wave 4(`通道 × 故障` 夹具,留宿主维) |
| ④ SoT writer 随宿主组件化 | Wave 1 决策 1(复用既有 pump + turns_mirror,不 fork) |
| ⑤ sid↔uuid 映射可扩列 | Wave 1 决策 4(`{sid, vendor_uuid, host}`) |

---

## 三、风险与纪律

- **真机/真 vendor smoke 不阻塞**:全部确定性验收走 fake-vendor(NDJSON 脚本);真机 smoke 项产出清单交 owner(prd §六验收的真机串)。
- **不碰 control session 的 MCP**:cargo 跑会重建 `target/debug/ccteam` → 可能掉 control session 的 telegram MCP(memory 在案);本 dev session 该跑就跑,但批量跑、不空跑。
- **红线一律以 `CLAUDE.md` §三为准**:No-prompt-injection(`--agent`,禁 systemPrompt)· 不解析终端输出(stream-json 无终端,天然满足)· 永不主动 kill(idle=关 stdin+`--resume`,≡ resume-by-session-id)· progress_bridge schema 单一权威 · ccteam-core 零 team 字面。
- **直推 dev 不开 PR**;commit 英文;文档中文;每 commit 前 `cargo fmt --all`。
