# v0.8.3 web chat 重做 + review 修复 — 实现提示词(给 dev session)

> 把当前 web chat(commit `5f479ca`,现为 dashboard SPA 的 `/chat` 路由)重做成**独立 chat-first 应用**,并修掉 code review 的真问题 + terminal bug。
> **原型(已确认)**:`docs/versions/v0-8-3/prd.html` 第一个 tab —— 独立外壳 / 全 session 左栏 / 连续会话流 / ＋新建弹窗(项目·agent·role)/ vendor 终端。

## 总原则(守住)
- web 与 IM 平行,复用**同一个 Gateway**(`handle_text` / resume-by-id / 出站 event pump),不改 gateway。
- **web ⊥ im**:桥只在 cli 层(`crates/ccteam-cli/src/web_chat_bridge.rs` + `main.rs` 装配),线缆是中立 JSON;两 crate 不互依赖。
- 红线:不 scrape 终端、不注入 system prompt、`progress.jsonl` / `turns.jsonl` 仍是 SoT;`/ws/chat` 留在 `stateful_router` 走 `auth_layer`。

## 一、UI 重做(新需求)

### N1 独立外壳,不嵌 dashboard
- 现状:`crates/ccteam-web/web/src/App.tsx:149` 把 `ChatConsole` 挂成 `/chat`,与 Dashboard/Projects/Teams 共用顶栏导航 —— 即"套在旧 web 里"。
- 做:web chat 独立成自己的 SPA 外壳(自己的布局/入口,不渲染 dashboard 导航)。顶栏极简:连接状态 + 今日成本 + 一个跳 dashboard 的链接。dashboard(监控/进度/成本/SSE)保留为独立入口。
- 验收:打开 chat 不出现 dashboard 的 Projects/Teams/Sessions 导航;dashboard 仍可独立访问。

### N2 左栏 = 所有 session(按 project 分组)
- 现状:`session_items()`(`crates/ccteam-web/src/routes/chat_ws.rs:268`)只对 flex 项目列 session;`SessionItem.current` 永远 false。
- 做:列出所有 project 的所有 session,按 project 分组,每条 `vendor·role·sid` + 在线点;**高亮当前焦点 session**(`current` 字段真用上,从 gateway 焦点来,别 hardcode false)。
- 验收:多 project 多 session 全列出;当前焦点高亮。

### N3 ＋新建 = 项目 + code agent + role
- 现状:`createSession` 写死 `/new claude assistant`(`ChatConsole.tsx:152`)。
- 做:「＋ 新建」弹窗三选 —— ① 项目(下拉,含"＋ 新项目")② code agent(Claude Code / Codex)③ role(可输入,建议用该 project `workflow.yaml` 的 roles 做 datalist)。提交 = `/cd <project>` + `/new <vendor> <role>`,创建后焦点切过去;弹窗实时显示等价命令(像原型那样)。
- 验收:三项可选;创建出对应 vendor/role 的 session 并切过去。

### N4 一条连续会话流 + 历史保留(最关键)
- 现状:切 session 清屏重开;`chat_outbound` 无历史回放;前端不持久。
- 做:
  - 一个 chat = **一条连续时间线**,不按 session 分屏、不清屏;切 session/project 只插一行 marker,会话继续;每条回复标注来自哪个 session(vendor·role)。
  - 连上 / 重连:从 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl` **恢复整段历史**(gateway 在 web 连上时回放;web 端也要重连 —— 见 P1-4)。
- 验收:切 session 不清屏;刷新/重连后整段历史还在;无"在项目间来回切"的割裂感。

### N5 终端按 vendor
- 做:仅 Claude/tmux session 显示「终端」页签;Codex(app-server)无 PTY → 禁用 + 提示。pane 映射要对(见 B1)。

## 二、review 必修(P1,来自 `5f479ca` code review)

**P1-1 outbound 广播 ↔ backlog 去重打架** — `chat_ws.rs:122-130` + `web_chat_bridge.rs:57-58`
- `send()` 永远先 push backlog 再 broadcast,`relay` 只在 `remove_backlog_message()==true` 才转发。broadcast 是多订阅者(同 chat_id 多 tab;前端 `WEB_CHAT_ID` 写死)→ 第一个 socket 摘走,其余丢;reconnect 竞争把 live 消息错投。
- 修:live 路径按 recipient **广播给所有匹配 socket**,别拿 backlog 当投递令牌;backlog 只在该 recipient **0 在线 socket** 时补发(加 per-recipient 连接计数/registry)。N4 历史回放与此统一设计。

**P1-2 一条坏帧断整条 socket** — `chat_ws.rs:79/97/98`
- `from_str::<ClientChatFrame>(&text)?` 的 `?` 让 `relay` 返 Err → 关 socket。一条非法 JSON / 未知 type / 非 UTF8 binary 就断聊天。
- 修:解析失败 log + continue(可回 error 帧),别 break。

**P1-3 backlog 无上限/TTL** — `web_chat_bridge.rs:57` / `state.rs` `chat_backlog: Vec`
- 修:加 cap + TTL,或随 P1-1 连接 registry —— 有在线 socket 就不入 backlog。

**P1-4 前端不重连**(已并入 N4)— `ChatConsole.tsx:81-128` 只连一次。
- 修:onclose backoff 重连,重连即触发历史 / backlog 补发。

## 三、terminal bug(用户实测:`reconnecting 1/7 → Connection lost`)

**B1** 根因:(a) Codex session 无 tmux pane(app-server)→ 终端不该出现;(b) chat session 真身 pane 是 `ccteam-chat-<slug>-<role>`,而 `ChatConsole` 用 `ptyUrlFor(project, sid)`(`terminalConfig.ts:23`)拼 `/ws/<slug>/<sid>/pty`,网关 sid(`s1`)与 pane 名对不上 → 7 次重连耗尽。
- 修:终端只对 Claude session 开(N5);pty 路由按真实 pane(`ccteam-chat-<slug>-<role>`)解析,或由 gateway 提供该 session 的 pane 标识;codex 不显示终端。

## 四、P2(可并入或记 backlog)
- `/sessions` 别 scrape 回复文本(`chat_ws.rs:243-266` 按 `:` `splitn(4)`):gateway 直接给结构化 sessions。
- 协议 streaming 帧(`TurnStarted/AssistantDelta/Tool/TurnDone`)要么接上(N4 连续流正需要),要么标 reserved;前端 `assistant_delta` 当前每 delta 新建气泡(`ChatConsole.tsx:104`),接流式时改成同一条 append。
- `Attach` 帧无落点(gateway 不认 `/attach`,只认 pair/new/use/cd/sessions/projects/compact/review):删或在 gateway 实现真 attach。

## 五、别动(已核过 OK)
Gateway `handle_text` / resume-by-id / 出站、harness 执行层、`auth_layer` 复用、acl `web→ws_user_ids`、crate 依赖方向(web 不依赖 im)、协议 round-trip 测试。

## 验收基线
- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` 不退步(当前 1759/0);
- `cargo clippy --workspace --all-targets -- -D warnings` 0 warning;`cargo fmt --all -- --check` 过;
- `cargo test -p ccteam-web --test chat_ws_test` 单独过;`npm run build`(`crates/ccteam-web/web`)过;
- 手验:开两个 tab 不丢消息;切 session 不清屏;重连历史在;codex session 无终端页签;新建弹窗三选可用。
