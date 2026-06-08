# v0.8.9 — rmux 0.3→0.5 升级 + 裸字节终端(W2b)

> **⚠️ 落地实况(两步)**:① **byte API 本就在 rmux-sdk 0.3.1**(`PaneOutputStream` / `PaneOutputChunk::Bytes` / 订阅起点 `Oldest`)—— 原「需 0.5 才有裸流」前提错了;Phase 3 先在 **0.3.1** 上把 `rmux_backend` 的 subscribe/capture 切到这套已有 byte API,即拿到逐字节保真终端(保真**不依赖** 0.5)。② 收尾后按用户要求**仍 bump 到 0.5**(根 `Cargo.toml` rmux `0.3→0.5`,`Cargo.lock` 0.5.0)—— clean additive、call-site 0.3→0.5 **byte-identical**、实测**零漂移**(`cargo build --workspace` 直接过),取 0.5 的 tmux-compat / window APIs。故下文「0.3→0.5 升级」标题准确;§三第 1 条「预期 API 漂移」**实测为零**。其余设计(裸流替换有损行流、`Oldest` 回放、pattern-matching 消费者不退、tmux backend 不变)均按本文落地。
>
> **状态:已落地(v0.8.9 Phase 3)**。原文为规划稿(doc-first),保留作设计记录;实况见上方修正。
> **来源**:v0.8.8 ship 后实机(TG 2026-06-07)发现 web 终端保真问题(bug4 连上空白、bug6 换行不对齐)→ 切裸字节流根治。排进 v0.8.9(与 web UI 改造同 wave)。
> **代码基线**:dev(v0.8.8 已落,ccteam pin rmux **0.3** → 解析 **0.3.1**)。相关:同目录 `prd.md`(v0.8.9 web UI 统一 chat 壳 —— 终端住在该壳里);v0.8.8 `bug.md` 的终端两条 + `wave-3-handoff.md`(B5)。

---

## 〇、一句话

把 rmux 依赖从 **0.3 升到 0.5**,并把 `rmux_backend` 的 pane 输出从**有损的行流**(`PaneLineItem::Line`,剥 `\r`、丢 ANSI)切到 **0.5 新增的裸字节流**(`PaneOutputStream` / `PaneOutputChunk::Bytes`)。结果:**默认 rmux backend 下 web 终端就是逐字节保真的交互终端**(根治 v0.8.8 bug4 空白 + bug6 换行歪),不再需要 `CCTEAM_MUX_BACKEND=tmux`。

## 一、背景 / 动机(W2b 缺口)

- v0.8.8 web 终端在默认 rmux 下:**bug4** 连上空白(subscribe 只流新输出、不回放当前屏幕)、**bug6** TUI 换行/对齐错(行流非裸字节)。faithful 路径当时只能整 daemon 切 tmux backend(raw pipe-pane)。
- 根因 = **W2b 缺口**:`crates/ccteam-harness/src/rmux_backend.rs` —
  - `subscribe`(~:429-481)消费 `PaneLineItem::Line { text }` → `MuxEvent::OutputChunk(text + \n)`,注释自述"strips `\r` … **NOT byte-faithful**"。
  - `capture`(~:565)从解析后的 cell-grid 取**渲染文本**(`with_ansi=true` 也无法还原,ANSI 转义在 daemon 解析网格那步就没了)。
- xterm.js 要**裸终端字节**(光标定位 + 精确换行 + 颜色)才能忠实渲染 claude 这类全屏 TUI;行流给不了。

## 二、关键发现(rmux 0.5 已解决)

rmux 仓 `github.com/Helvesec/rmux`,最新 **v0.5.0**(2026-06-04;ccteam 现 pin 0.3)。`crates/rmux-sdk/src/events/streams.rs` 新增**裸字节流**:
- **`PaneOutputStream` → `PaneOutputChunk::Bytes { bytes, sequence }`** —— 原话"**preserve every payload byte the daemon delivered, including NUL and non-UTF-8**",带单调 per-pane sequence。逐字节保真,ANSI 完整。
- 现用的 `PaneLineStream` / `PaneLineItem` 被明确写成"a strict superset built **on top of** the raw stream"(有损 UTF-8 行渲染)—— 即 ccteam 卡在有损层。
- 订阅起点 `Now` / **`Oldest`**(回放保留 backlog)→ 顺带解决"连上回放当前屏幕"(v0.8.8 bug4 在 `ccteam-web/src/pty.rs` 加的 capture-first snapshot-on-connect 可改用它)。
- proto:`SubscribePaneOutputRequest` / `SubscribePaneOutputRefRequest` / `PaneRecentOutput`。
- 0.4.3 另加 `rmux web-share`(浏览器终端)= 建在这个裸流上,反证它能忠实渲染 TUI。

## 三、改动计划

1. **依赖 bump 0.3→0.5**:根 `Cargo.toml`(~:42 `rmux-sdk`/`rmux-client`/`rmux-server`/`rmux-proto` 都 0.3→0.5)+ `Cargo.lock`。跨 2 个 minor,**预期 API 漂移** —— `rmux_backend.rs` 的 spawn/kill/exists/list_sessions/send_text/resize/capture/subscribe 全部对齐 0.5 API;`rmux_types_compile_link`(Cargo.toml:13-14 的 semver-drift 守)更新到 0.5。**仅用 SDK 裸流**,**不**引入 `rmux web-share` 整套(crypto/wasm/前端)。
2. **subscribe 改裸字节**:`rmux_backend::subscribe` 从 `PaneLineItem::Line` 换成 `PaneOutputStream` / `PaneOutputChunk::Bytes` → `MuxEvent::OutputChunk(原始字节)`;`Lag` → `MuxEvent::OutputDropped` 保留。
3. **capture 改裸字节**:用 `Oldest` 保留 backlog(raw bytes)→ 真裸字节 capture(替代渲染文本);`with_ansi=true` 现在能真给 ANSI。`ccteam-web/src/pty.rs` 的 snapshot-on-connect 改用 `Oldest`(更干净,免一次单独 capture)。
4. **web 终端受益**:默认 rmux 即逐字节保真交互终端(bug4 + bug6 根治);`pane_snapshot.rs` 同样拿到真 ANSI(去掉 W2b TODO + "rmux ANSI gap" 注释)。

## 四、风险 / 红线

- **【高】pattern matching 的行流依赖**:rmux 的 `PatternMatched`(行级正则,给 marker / `observe_marker` / `typed_event_tap` / tail-silence)依赖**行流**。换裸流后必须保证这些消费者不退 —— 方案二选一:(a) 订阅两路(裸流给终端 + 行流给 pattern),(b) 在裸流上重做行切分喂 pattern。实现时先 grep 全 `PaneLineItem` / `PatternMatched` / marker 消费者,确认不破 `chat_turn_completed` / silence / typed-event。
- **【高】沙箱跑不了 rmux daemon**:与 5 个 `ws_*` / `pane_snapshot` 同 env-gate —— 真终端渲染 + marker/pattern + capture 必须**专机/真机验**,沙箱只能保编译 + 非PTY测试。
- **【中】2-minor dep bump 漂移**:0.3→0.5 的 API 变化可能波及 rmux_backend 全部方法 + 任何直接用 rmux 类型的处。bump 后 `cargo build --workspace` 先过,再逐个对齐。
- **红线守**:不 scrape pane(裸流是中继、非 capture-pane 解析,合规);永不主动 kill;保真度升级不改 backend 选择语义(`CCTEAM_MUX_BACKEND` 仍可选 tmux)。

## 五、验收

- 默认 rmux(`CCTEAM_MUX_BACKEND` unset)下 web 终端**忠实渲染 claude TUI**(换行/对齐/颜色/光标对)+ 可交互(输入 + resize)+ 连上**回放当前屏幕**;v0.8.8 bug4 / bug6 关闭。
- marker / pattern 链不退:`observe_marker`(chat_turn_completed)、`typed_event_tap`、tail-silence 仍正常。
- `cargo test --workspace --exclude ccteam-web` 不退基线(v0.8.8 = 1999/0);clippy 0;fmt 干净。
- 专机 smoke:真 rmux + 真 claude 跑一轮终端交互 + marker。

## 六、归属

`ccteam-harness`(`rmux_backend.rs` + Cargo deps)+ `ccteam-web`(`pty.rs` snapshot 改 Oldest、`pane_snapshot.rs` 去 W2b TODO)。与 v0.8.9 web UI(终端在统一 chat 壳内)同 wave;dep bump 先行(其余 rmux 行为不变作回归基线),再上裸流改写。

> 参考:rmux 仓 `github.com/Helvesec/rmux`(`crates/rmux-sdk/src/events/streams.rs` = `PaneOutputStream`/`PaneOutputChunk::Bytes`);v0.8.8 `docs/versions/v0-8-8/{bug.md,wave-3-handoff.md}`(B5 终端 + 保真 caveat);本仓 `crates/ccteam-harness/src/rmux_backend.rs` + `crates/ccteam-web/src/{pty.rs,routes/pty_ws.rs,routes/pane_snapshot.rs}`。
