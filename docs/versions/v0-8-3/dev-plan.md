# v0.8.3 dev-plan —— web 端进入层(WS)· `/goal` 一口气执行

> **设计 SoT**:`docs/versions/v0-8-3/prd.html`(5 tab,默认页 = Web 控制台原型;另有 概念/平行对照/WS 协议/接入缝)。架构总则:`docs/tech-design.md`(尤其 §12 协议→代码指针表)。
>
> **用法**:新 session 里 `/goal docs/versions/v0-8-3/dev-plan.md`。**严格 P0 → P6,每个 phase 必须先跑通「✅ 验收」(全绿)才进下一个**;被打断可从"第一个没过验收的 phase"续做。

## 目标(一句话)

把 web 端从**只读看板**升级成**与 IM 平行的用户进入层**:浏览器经新子协议 `ccteam-chat.v1` WebSocket,作为一条 `channel="web"` 的 transport 接进**同一个 Gateway 路由核**,复用 `handle_text`(@bot / /new / /use / /cd / submit_turn / event pump / 出站)。**Gateway 与执行层零改**;新增只有「web 的 chat WS 路由 + cli 层 mpsc 桥 + SPA chat 面板」。

## 贯穿红线(每个 phase 都守)

- **web ⊥ im**:`ccteam-web` 不引 `ccteam-im`,反之亦然(`grep ccteam-im crates/ccteam-web/Cargo.toml` = 0;反向同)。桥**只在 cli 层**(`run_start` 装配处)用 mpsc + 中立 JSON(`ChannelMessage`/`SendMessage` 镜像)。
- **不退 baseline**:`cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` pass ≥ 起手数(当前约 1743;起手先记 `BASELINE_PASS`)。
- **clippy 0 warning** + `cargo fmt --all -- --check` 干净(P6 强制,过程中尽量保持)。
- **复用,不另造**:`Channel` trait / `WsChannel` / `Gateway::handle_text` / `pty_ws` upgrade+背压套路 / SPA 壳 全复用。
- **不 scrape pane**:chat 视图读**结构化 turn 事件**(CanonicalEvent);裸字节终端走**已有**的 `pty_ws`(`ccteam-pty.v1`),两条 WS 并存、各司其职。
- **确定性 fake 优先**:`CCTEAM_{CLAUDE,CODEX}_BIN` 打桩驱动测试;真 claude/codex 只在 P5 可选 smoke。

## 接入缝(研究已锁定的精确挂点)

- **Channel trait + 消息类型** → `crates/ccteam-im/src/transport/mod.rs`:`trait Channel { name(); send(&SendMessage); listen(tx: Sender<ChannelMessage>); health_check(); }`;`ChannelMessage{ id, sender, reply_target, content, channel, timestamp, thread_ts }`;`SendMessage{ content, recipient, subject, thread_ts }`。
- **现成 WsChannel 参考实现** → `crates/ccteam-im/src/transport/providers/ws.rs`(`parse_frame` 入站、broadcast 出站;P5 e2e 已用它)。
- **Gateway 路由核** → `crates/ccteam-im/src/gateway.rs`:唯一入口 `handle_text(channel, chat_id, user_id, text) -> Result<Vec<String>>`;`ChatKey{channel,chat_id,user_id}`;`/new /use /cd /pair /sessions /projects` + `@mention`;`submit_turn` + `spawn_event_pump`。
- **daemon 装配(桥挂这)** → `crates/ccteam-cli/src/main.rs::run_start`:web / im / MCP 各自 `tokio::spawn`,共享 shutdown,**无共享状态**——在此建 mpsc 把 web chat WS 接到 Gateway 的 inbound mpsc + 出站。
- **web 现状** → `crates/ccteam-web/src/routes/pty_ws.rs`(WS upgrade + auth_layer + lag 背压,照抄)、`src/state.rs::AppState`(加 chat 桥 mpsc 端点字段)、`src/routes/mod.rs`(挂新路由)、`src/queries.rs`+`api_v1`(session 列表数据)、`web/src/`(SPA,加 chat 面板;终端视图复用现有 `useTerminal`/pty_ws)。

---

## P0 — 线协议类型 + 子协议常量(低风险热身)

**改**:定义 `ccteam-chat.v1` 帧类型(serde):
- client→server:`text{content,id}` / `switch{project?,session?}` / `attach{name,data}`
- server→client:`turn_started{session,vendor}` / `assistant_delta{text}` / `tool{name,summary}` / `reply{content}` / `turn_done{session}` / `sessions{items[]}` / `lag{behind}`
- 入站映射到 `ChannelMessage{channel:"web",...}`;出站由 `SendMessage`/`GatewayEvent` 映射。`pub const SUBPROTOCOL = "ccteam-chat.v1"`。

**✅ 验收**:`cargo test -p ccteam-web chat_frame`(所有帧变体 serde round-trip)+ grep 到 `ccteam-chat.v1` 常量。

## P1 — ccteam-web `chat_ws` 路由(net-new)

**改**:`routes::chat_ws`,`GET /ws/chat`,复用 `pty_ws` 的 axum upgrade + `auth_layer` + lag 背压。连上即推 `{type:"sessions"}`(先从文件系统/api_v1 出);入站 text 帧 → `AppState` 注入的 `mpsc::Sender<ChannelMessage>`;出站 ← per-connection `Receiver`(由桥喂 `SendMessage`→帧)。

**✅ 验收**:集成测试 —— ccteam-chat.v1 握手(无 token 401、subprotocol echo、pre-upgrade 错误码);发 text 帧 → 落 inbound mpsc;喂一条 `SendMessage` → client 收到映射帧。**先用 fake mpsc loopback,不接 Gateway**。

## P2 — cli 层 mpsc 桥:web chat WS ⇄ Gateway(net-new,核心缝)

**改**:`run_start` 里建 mpsc;web inbound mpsc → Gateway inbound consumer(经 `WsChannel`,或一个直接读 mpsc 的 `Channel` impl);Gateway `channel="web"` 的出站 → web outbound。**桥在 cli**(它本就同时依赖 web+im),`ccteam-web`/`ccteam-im` 互不加依赖。

**✅ 验收**:cli 级 wiring 测试(仿 `crates/ccteam-im/tests/inbound_wiring_test.rs`)—— 一条 `channel="web"` 的 ChannelMessage 走 `handle_text` → fake harness `submit_turn` → `GatewayEvent` → 回到 web 出站;`CCTEAM_CLAUDE_BIN` fake。`grep ccteam-im crates/ccteam-web/Cargo.toml` 仍 = 0。

## P3 — chat ⇄ project ⇄ session in web

**改**:`switch` 帧 → 等价 `/cd` · `/use`(经 `handle_text` 或直接 gateway 调用);project/session 变更推新的 `{type:"sessions"}`;`@bot` / `/new` / `/compact` / `/review` 经 text 帧已通(`handle_text` 解析)。

**✅ 验收**:测试 —— switch 帧改当前 project/session;`/new` 起新 session 并设当前;`sessions` 帧反映注册表;两个 web 连接路由互不串(隔离)。

## P4 — SPA chat 面板(prd.html 的 UI)

**改**:Vite/React —— 项目侧栏(可达 project)+ session 标签(多 session + ＋/new)+ transcript(turn 事件 → 气泡 + tool 行,assistant 流式)+ composer(text + slash + @)+ 💬Chat/▦终端 切换(终端视图复用现有 `useTerminal` + `pty_ws`)。走 `ccteam-chat.v1`。对齐 `prd.html` 第 1 tab。

**✅ 验收**:`web-bundle` feature 构建通过 + `/app/chat` 路由 serve;对照 prd.html 手测清单(切 project/session、发消息见流式回、Chat⇄终端切换)过。

## P5 — 真 WS 端到端 smoke(双 vendor)+ 耐久

**改**:扩 smoke —— 真 `ccteam-chat.v1` 连上发 "hi" → fake claude 回;`/new codex` → fake codex 回;切 project/session;杀 daemon → 重启 → 重连 → `sessions` 恢复(resume-by-id)+ 补发未发出站。

**✅ 验收**:真 WS web-chat e2e 测试(仿 `inbound_wiring_test::real_ws_dual_harness_smoke` 的 web 版)绿;可选 `--real` 跑真 claude/codex。

## P6 — 质量门 + 文档(ship gate)

**改**:clippy 0 / `cargo fmt --all` / baseline 不退 / web ws 测试(需要时 `CCTEAM_MUX_BACKEND=tmux`)绿。文档同步:CLAUDE.md §一 baseline + 当前在做项;`tech-design.md` §12 指针表加 `ccteam-chat.v1` + `/ws/chat` 行 + §架构补 web 进入层一段;`usage.md` 加「web chat 控制台」用法;`docs/versions/v0-8-3/README.md` 落 ship 记录。版本 `0.8.2 → 0.8.3`。

**✅ 验收**:`cargo clippy --workspace --all-targets -- -D warnings`(0)+ `cargo fmt --all -- --check`(净)+ baseline ≥ BASELINE_PASS + web ws 测试绿 + 文档 grep clean。

---

## 总验收(全部完成的判据)

- P0–P6 每个「✅ 验收」全过。
- 真 WS web-chat e2e 绿:浏览器经 `ccteam-chat.v1` 收发、切 project/session、`/compact`·`/review`、daemon 重启 resume —— 与 IM **同一个 Gateway、同一批 session**。
- `grep ccteam-im crates/ccteam-web/Cargo.toml` = 0(web ⊥ im 不破);baseline 不退;clippy 0;fmt clean。

## 不在 v0.8.3(接口已留)

- **手机批准** —— `ApprovalIR` 仍类型占位,agent 走 `--dangerously-skip-permissions`。
- **ccteam-flow 编排** —— 仍推后。
- **附件经 web 进 vendor** —— 沿用 IM 既有附件路径,本版可选(`attach` 帧先落地,贯通推后)。
