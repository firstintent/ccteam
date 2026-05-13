# PRD V0.3.2 — Web UI 重写（替换 htmx + askama → React SPA）

> 范围：V0.3.2 是 V0.3.1 ship 后的第二个 patch round。**头号诉求**：
> 把 V0.3 引入的 htmx + askama web UI **整体换成 React SPA**，参考
> `references/agent-of-empires/web`（MIT，2026 Nathan Brake）的前端，
> 复用其原子组件（terminal pane / diff viewer / command palette /
> 响应式 layout / PWA shell），数据层重写以贴 ccteam 的
> flex + workflow + harness 模型。
>
> base = `origin/main` `10634c0`（V0.3.1 ship 终点 + V0.3.1 docs PR）；
> workspace.version 起点 `0.3.1`，V0.3.2 ship 时 bump `0.3.2`；
> 测试 baseline `833/0`（`cargo test --workspace`，pre-existing 9
> clippy errors 不本轮处理）。
>
> 跟 V0.3.1 同：**F-numbered finding under one umbrella**，不 bump main
> version。但本轮的工作量预计 > V0.3.1（前端 ~12 kLOC TS + 后端 JSON
> 化 + 协议补齐），ship 周期对应拉长。

---

## 0. session 起手 onboarding（30 秒）

```bash
git rev-parse origin/main                                            # HEAD
cargo test --workspace 2>&1 | grep -E "^test result" \
  | awk '{p+=$4;f+=$6}END{print "passed:",p,"failed:",f}'            # baseline 833/0
```

读完本 PRD §1–§3 → 看 §3 决策点 → 用户 confirm 后才看 §4 实施。

---

## 1. 背景 — 为什么 V0.3.2 是 web UI

### 1.1 用户对话源（2026-05-11，Telegram message 337）

V0.3.1 ship 后次日，用户在 Telegram 直接拍板：

> 现在 web ui 很不丝滑，我想直接参考 references/agent-of-empires 项目，
> 复用这个项目的前端部分。替换现有的 web ui。整理 v0.3.2 的需求。
> 尽量用派 subagent 去干活。

诉求拆解：

1. **不丝滑** — V0.3 落地的 htmx + askama dashboard 体验差（snapshot-only
   ANSI pane、整页 303 重定向、events 表无 prepend、移动端不可用）
2. **复用 AoE 前端** — 已 clone 在 `references/agent-of-empires/`，MIT
   许可，前端代码 ~12 kLOC TS，React 19 + Vite 8 + Tailwind 4 + xterm
3. **整理 V0.3.2 需求** — 不是只做 UI，还要把 V0.3.1 PRD §10 已 confirm
   的 V0.3.2 deferred 项一并 surface（见 §3 Shape 决策）
4. **subagent 派工** — 跟 V0.3.1 流程一致（worktree-per-finding + Agent
   briefing），主 session 编排，不并行 main tree

### 1.2 当前 web UI 的实际短板（subagent 调研结论）

> 完整能力面见 `crates/ccteam-web/`，下文只列影响 UX 的硬伤。

| 项 | 现状 | 短板 |
|---|---|---|
| Terminal 渲染 | `/api/{slug}/pane-snapshot.ansi` 拉一次 ANSI bytes → xterm.js 渲染 | 不是 live PTY，用户点 refresh 才更新；scroll back 没了；不能输入 |
| 写操作 | POST form → 303 重定向回 detail page | 提交后整页刷，光标位置丢失；上下文断层 |
| Events 流 | SSE 推 `progress` event，但模板仅 init render，**前端不 prepend** | 实时性 = 0；需手动 reload 才看到新事件 |
| Mobile | 无 viewport meta tuning、无 sidebar collapse、按钮密、forms 不适配 | 手机基本不可用 |
| 多 session 切换 | 各自独立 page，无 tabs / 全局导航 | flex 团队下 N session 间切换体感差 |
| 视觉风格 | 默认 axum minimal CSS + 少量定制 | 跟用户期待的"丝滑 dashboard"差距大 |

### 1.3 AoE 前端的复用边界（subagent 调研结论 + 用户 confirm "技术栈完全一致"）

> 详细 inventory 见调研报告归档（task #2）；用户 2026-05-11 msg 349 进一步
> 拍板：**web 技术栈和 AoE 完全一致，不裁剪 deps**。下文区分**技术栈层**
> 与**组件激活层**。

**技术栈层（package.json deps）— lift 完整保留**：

- React 19.2.4 / Vite 8.0.3 / Tailwind 4.2.2 / TypeScript ~6.0.2
- 全量 AoE deps：`@assistant-ui/react`、`@assistant-ui/react-markdown`、
  `@wterm/*`、`cmdk`、`react-diff-viewer-continued`、`shiki`、`marked`、
  `remark-gfm`、`lucide-react`、`react-router-dom`、`tailwindcss`
- 全量 devDeps：`@playwright/test`、`vitest`、`eslint`、`typescript-eslint`、
  `geist` 字体
- F53 实施时**不删 deps**；仅 vite tree-shake 自动剔除 unused exports

**组件激活层 — 三桶（决定哪些组件挂入 SPA 主流程，本轮工作量参考）**：

| 桶 | 占比 | 处理 |
|---|---|---|
| **A** — lift-as-is（原子复用） | ~40-50% | 直接 use（terminal pane / diff viewer / palette / status glyph / mobile keyboard hook / toast bus / PWA shell） |
| **B** — lift & rewire（保留 UX，重写 data layer） | ~30% | TopBar / WorkspaceSidebar / RightPanel / SettingsView 框架 / 移动端 breakpoint |
| **C** — 代码保留但不挂主路由 | ~20% | SessionWizard / CockpitView / SwitchSubstrateAction / ProfileSelector / AgentStep / Docker 沙箱 UI — 跟 ccteam 模型对不上，本轮不挂；deps 保留意味着未来若做"类 AoE 创建 wizard"可激活 |

**为什么桶 C 保留代码而非删**：用户明确诉求"完全参考"，删了反而失去未来重
新激活的成本优势；vite tree-shake 处理 bundle 体积，CI 测试只跑挂在路由表
里的组件，未挂入 router 的组件不会 bloat 实际加载。

---

## 2. 已 confirm / 已知 / 待确认

### 2.1 用户已 confirm（来自 V0.3.1 PRD §1.2 + 2026-05-11 对话）

| Q | Answer |
|---|---|
| Web UI 重写方向 | **lift AoE 前端 + rewire 数据层** |
| 前端 stack | **完全沿用 AoE package.json 全量 deps**（React 19.2.4 + Vite 8.0.3 + Tailwind 4.2.2 + TS ~6.0.2 + `@assistant-ui/*` + `@wterm/*` + cmdk + react-diff-viewer-continued + shiki + marked + lucide-react + react-router 7 等）— 2026-05-11 msg 349 确认 |
| 后端 stack | 维持 axum + tokio-broadcast；askama → JSON / SSE / 可选 WS |
| MIT 复用方式 | fork `references/agent-of-empires/web/` 到 `crates/ccteam-web/web/`，保留 `NOTICE`，不 vendor 为 npm dep |
| 不破红线 | progress.jsonl 仍是 SoT；watcher 不变；写动作仍走文件系统；token auth 不退化 |

### 2.2 V0.3.1 PRD §10 已记录的 V0.3.2 deferred

> 完整列表见 task #3 调研报告，下表只列**已显式 deferred 到 V0.3.2** 的
> 6 项。

| 项 | 来源 | 备注 |
|---|---|---|
| `CodexAdapter` 完整实现（spawn/ingest/hook） | V0.3.1 PRD §10.3 + F47 tail | V0.3.1 仅 trait stub，所有路径 `Err(NotImplemented)`，错误消息含 "V0.3.2 deferred" |
| flex workflow promotion / demotion | V0.3.1 PRD §10.2 | UI/UX 把 N 累积事件 promote 为 frozen phase，反向 demote |
| flex 团队 retro_schema enable | V0.3.1 PRD §10.2 (line 1103) | flex 用户跑完 promote 后再评估 |
| session-level outbox/inbox 细分 | V0.3.1 PRD §10.4 (line 1123) | V0.3.1 默认项目级，V0.3.2 if 用户撞 cross-talk 再加 |
| codex statusline-equivalent 摄入 | V0.3.1 PRD §10.3 (line 1112) | 等价 Claude Code statusline JSON → `HarnessSnapshot` + web SSE |
| `mcp__codex__codex` MCP peer 注册 | V0.3.1 PRD §10.3 (line 1113) | 让 meta-agent 调 Codex |

### 2.3 ✅ 用户 2026-05-11 拍板的两条 scope 决策

- **范围 = Shape A** — V0.3.2 = web UI only；CodexAdapter slip V0.3.3
- **终端 = B2** — "完全参考 agent-of-empires 项目"，引入 WebSocket PTY relay

详见 §3。

---

## 3. ✅ 已 lock 的 scope 决策（2026-05-11 用户 confirm，Telegram 343）

### 3.1 决策 ① — V0.3.2 范围 = **Shape A（web UI only）**

- F-finding 范围：**F52-F59**（8 个 finding，全部 web 相关，PTY relay 单
  独 split 为 F56 + F57）
- CodexAdapter 完整实现 → **V0.3.3**
- 已在 `docs/v0-3-1/README.md` 顶加 erratum 行
- 理由：UI rewrite 单体已超 V0.3.1 总量（前端 ~12 kLOC + 后端 JSON 化
  + WS PTY relay + 文档），叠 CodexAdapter 会让 round 不可 review；用
  户体感优先把"丝滑"立住，Codex 跟一轮独立 patch round
- V0.3.1 PRD §10.3 显式 "deferred to V0.3.2" 承诺通过 V0.3.1 README
  erratum 行明示 slip

### 3.2 决策 ② — 终端 = **B2（完全参考 AoE 的 WebSocket PTY relay）**

**用户原话**："完全参考 agent-of-empires 项目"。意味着不退化为 snapshot
loop，而是 lift AoE 的双向 PTY WS 协议。

**新 WS 路由**（参 AoE `references/agent-of-empires/src/server/ws.rs:25-122`）：

- `GET /ws/{slug}/pty`（项目级，绑定 workflow 团队的主 tmux session）
- `GET /ws/{slug}/{sid}/pty`（flex 团队 session-scoped）
- `GET /ws/{slug}/{sid}/paired-pty`（AoE 的 paired terminal 概念暂不实现，
  本轮 deferred V0.3.3+）

**协议**（**完全 lift AoE wire format**，subprotocol 改名避免混淆）：

- WS upgrade，二进制双向
- **Subprotocol**：`ccteam-auth`（AoE 用 `aoe-auth`；同 shape，name 差异）
- **Outgoing (client → server)**：
  - JSON 文本控制帧：`{"type":"resize","cols":N,"rows":N}` /
    `{"type":"pause_output"}` / `{"type":"resume_output"}`
  - 二进制 frame：raw keystroke bytes（直接写入 PTY master）
- **Incoming (server → client)**：raw PTY output bytes
- **Ping/Pong**：server-originated Ping 每 30s；client 静默 90s → drop

**服务端实现 — 关键决定：用 `portable_pty` + `tmux attach-session`，
NOT `pipe-pane` + FIFO**（参 AoE `ws.rs:175-235`）：

- 新依赖：`portable_pty = "0.8"`、`futures-util = "0.3"`（axum 0.8 WS）
- **每个 WS 一个独立 PTY**：
  - `NativePtySystem::openpty(PtySize { rows: 24, cols: 80, .. })`
  - `tmux attach-session -t <session_name>` 作为 PTY slave 子进程
  - `env_remove("TMUX")` 允许嵌套 attach（ccteam web 可能跑在 tmux 内）
  - `TERM=xterm-256color`
  - `tmux set-option -as terminal-overrides "*256col*:U8=1:smacs@:rmacs@:acsc@"`
    workaround wterm SCS 渲染问题（AoE ws.rs:191-220 已踩雷修复）
- Output 路径：spawn blocking task 读 `master.try_clone_reader()` → 8KB
  chunks → WS Binary frame；EOF 或 read error → close WS + reap child
- Input 路径：WS Binary → 直接 `master.try_clone_writer().write_all(bytes)`
  → tmux attach 解码键序列 → tmux session 收到（**不用 send-keys**，
  PTY 字节流原生支持所有键序列，包括 `\x03` Ctrl-C / `\x1b[A` 方向键 / 等等）
- Resize 控制帧：`master.resize(PtySize { rows, cols })` → SIGWINCH 到 tmux
  attach 子进程 → tmux 调整 pane size

**Primary-client 语义**（AoE ws.rs:142-153 / 多客户端共存关键）：

- 同一 tmux session 多 WS 客户端时（手机 + 桌面同时连），resize 会打架
- AoE 方案：per-(slug,sid) Map 记录"primary client"（最近一次发送 keystroke
  的 client）；只有 primary 的 resize 被应用
- 单用户 ccteam 沿用此设计

**SIGSTOP/SIGCONT on scrollback**（AoE ws.rs:155-159 / 159 行起）：

- 客户端上滚进 scrollback → 发 `{"type":"pause_output"}` → server-side
  SIGSTOP 给 tmux attach 子进程（refcount 0→1 才真信号）
- 用户回 live → SIGCONT；多客户端独立计数

**Auth gate**：

- WS upgrade 前 cookie `ccteam_token` 已经被设置（auth.rs:215 现有路径
  覆盖）；upgrade handshake 在 middleware 校验同 cookie
- 不接受 `?token=` query param 走 WS（query string 进日志风险）
- WS subprotocol 含 `ccteam-pty.v1` 用于版本协商

**红线检查**（再次过一遍）：

| 红线 | 是否触动 | 解释 |
|---|---|---|
| progress.jsonl 是唯一事实来源 | **不破** | PTY 输出仅 UI 展示通道，不作为 orchestrator 决策输入；watcher 仍只读 progress.jsonl |
| 不解析 tmux 终端输出 | **不破** | tmux pipe-pane 是字节透传，ccteam-core 不接 pipe；ccteam-web 仅 broadcast bytes，不解析语义 |
| 永不主动 kill 长 session | **不破** | WS 关闭不 kill tmux session；pipe-pane 关闭也不 kill（tmux 不依赖 mirror 存在） |
| 用户键盘输入 idle-aware | **需新规则** | web PTY 输入视同 tmux attach 直接打字，**不**走 idle-aware 注入路径；ccteam-core 注入仍走 `/btw` / `inject_decision` 文件系统通道 |
| token auth 不退化 | **不破** | WS handshake 强制 cookie 验证；无 cookie → 401 reject upgrade |
| 项目级容器 / `--dangerously-skip-permissions` | **不变** | WS 不引入新执行 surface，user 输入等价于 tmux attach 后打字 |

**重要副作用**：用户可在 web 内直接 Ctrl-C / `/exit` / 发任意 keystroke
给 session。这跟 tmux attach 体感一致，是用户明确诉求。但需在 user-manual
明示 "web 输入 = tmux 直接输入，与 ccteam 的 /btw 注入是两条独立通道"。

### 3.3 锁定的 F-finding 编号 + PR 数

| Finding | 范围 | 状态 |
|---|---|---|
| F52 | JSON API parity layer | locked |
| F53 | vite scaffold + rust-embed swap + MIT attribution + base shell | locked |
| F54 | Dashboard + Project list（SPA 数据层接 F52 JSON）| locked |
| F55 | Project / Session detail + harness panel + events live | locked |
| **F56** | **WS PTY relay backend**（tmux pipe-pane + send-keys + WS auth）| **new, B2** |
| **F57** | **xterm input wiring in SPA**（lift AoE `useTerminal` + ccteam adapter）| **new, B2** |
| F58 | Write actions（btw / inject_decision / pause / resume）+ auth flow | locked（原 F57）|
| F59 | htmx UI retirement + e2e + ship gate | locked（原 F58）|

总计 **8 PR**，预计 ship 周期 **3-4 周**。

---

## 4. F-finding 设计（§3 已 lock：A + B2）

### F52 — JSON API parity layer（先于 SPA，保留 htmx 双发）

**Why**：SPA 出现前先把 askama HTML 接口的等价 JSON 立起来；htmx UI
**保留双发**做 fallback + e2e 对照，到 F58 才下线。

**Scope**：
- 新增 `/api/v1/projects`、`/api/v1/projects/{slug}`、
  `/api/v1/projects/{slug}/sessions/{sid}` 返回 JSON 等价于现有
  `DashboardRow` / `ProjectTemplate` / `SessionTemplate`（views.rs:36-129
  的结构 1:1 序列化）
- SSE 通道复用（`/sse/all`、`/sse/project/{slug}`、`/sse/project/{slug}/{sid}`、
  `/sse/harness/{slug}`、`/sse/harness/{slug}/{sid}`）— 现有就是 JSON-Lines wire，
  不动
- 写动作端点（`/api/{slug}/btw` 等 7 条 in actions.rs）**接受 JSON body**
  作为 form-encoded 之外的备选；返回 `{"ok":true}` 不再 303
- auth_layer 复用（token / cookie shim 不动）
- 测试：每条 JSON 路由一个 assertion（shape stable）

**Acceptance**：`curl -H "Authorization: Bearer ..." /api/v1/projects` 返回
合法 JSON；现有 `/`、`/project/{slug}`、`/session/...` HTML 路径**完全不变**。

### F53 — vite scaffold + rust-embed swap + MIT attribution + base shell

**Why**：把 AoE 前端目录 fork 进来，立 build chain。

**Scope**：
- `cp -r references/agent-of-empires/web/ crates/ccteam-web/web/`（带 git
  history 清空；保留 `LICENSE` + 加 `NOTICE` 文件标 fork-from）
- 清理 AoE 特有目录：`src/components/cockpit/`、`src/components/wizard/`、
  `src/components/settings/profiles/`、`src/lib/acp*.ts`、`src/lib/profile*.ts`
- 改 `package.json` name → `ccteam-web`、删 `@assistant-ui/*`、`cmdk` 暂留、
  `react-diff-viewer-continued` 暂留、其余按需 keep
- `vite.config.ts` 改 build outDir → `dist/`；dev proxy 改 `:7331`（ccteam-web 默认）
- Rust 侧：`crates/ccteam-web/build.rs` 添 `cargo:rerun-if-changed=web/`；编译
  时 `npm run build` 或 cargo feature gate（discuss F53 子任务）
- 现有 `routes/assets.rs::include_bytes!` 切换 `rust-embed::RustEmbed` 嵌入
  `web/dist/`；htmx / xterm.js / style.css 静态资源保留**直到 F58 才下线**
- 新增路由 `GET /app/*` → 返回 SPA `index.html`（react-router fallback）；
  `GET /assets/spa/*` → 嵌入资源
- base shell（`TopBar` + `ContentSplit` + 空 routes）能 build 出
  `dist/`，访问 `/app` 看到 React 初始页（无内容）
- 测试：build 命中 `dist/index.html`；`/app` 返回 200 + HTML；assets 200

**Acceptance**：`cargo build -p ccteam-web` 自动跑 `npm run build`；
`ccteam web` 启动后 `/app` 进入 React 空壳。

### F54 — Dashboard + Project list（SPA 数据层接 F52 JSON）

**Scope**：
- `src/pages/Dashboard.tsx` — 调 `/api/v1/projects`，渲染 `WorkspaceSidebar`
  + 主列表卡片
- 接 SSE `/sse/all` → 增量更新 last-event-label（沿用 dashboard.html 当前
  EventSource 逻辑，迁到 React hook `useProgressStream`）
- 桶 B rewire：sidebar 按 `team / kind`（workflow / flex / multi_workflow）分组；
  每个 row 显示 harness badge（claude / codex stub）
- Status glyph（桶 A 原子）改用 ccteam `StatusBadge` 五态映射
- 测试：playwright（沿用 AoE 已 ship 的 playwright config）跑 dashboard 渲染
  + SSE 注入

**Acceptance**：`/app/` 看到所有项目；SSE event 推过来 last-event 立即更新；
点项目卡进 detail page

### F55 — Project / Session detail page + harness panel + events live

**Scope**：
- `src/pages/ProjectDetail.tsx`、`src/pages/SessionDetail.tsx` —
  接 `/api/v1/projects/{slug}` / `.../sessions/{sid}`
- Events 流 `useProgressStream(slug, sid?)`：初始 200-row tail（来自 JSON
  response）+ SSE prepend；行内高亮 phase / tool / kind 字段
- Harness panel（V0.3.1 F46 ship）：subscribe `/sse/harness/{slug}/{sid}`，
  渲染 model / ctx% / cost / rate-limit / captured_at
- Outbox 列表（沿用 `OutboxRow` 结构）+ "open in editor" 链接（点击 → `vscode://`
  / `cursor://` URL scheme，best-effort）
- 多 session（flex）tabs：URL 形态 `/app/p/{slug}` 列 sessions tab；
  `/app/p/{slug}/s/{sid}` 进单 session view
- URL 兼容：`/project/{slug}` HTML 路径仍在（F52 双发），但浏览器 bookmark
  应能 follow 新 SPA path；server 加 redirect `301 /project/{slug} → /app/p/{slug}`
  （F58 才启用）
- 测试：playwright detail page + flex 多 session 切换

**Acceptance**：detail page live update；harness 数字随采样跳；events 不
reload；flex 项目 tab 切换不丢 state

### F56 — WS PTY relay backend（tmux pipe-pane + send-keys + WS auth）

**Why**：B2 决策核心；后端先立 WS 协议 + tmux 集成，让 F57 SPA 端只
管接线。

**Scope**：
- 新模块 `crates/ccteam-web/src/routes/pty_ws.rs`
- 新路由：
  - `GET /ws/{slug}/pty` — workflow / 默认绑定 tmux session
  - `GET /ws/{slug}/{sid}/pty` — flex session-scoped
- WS handshake：subprotocol `ccteam-pty.v1`；middleware 校验 cookie
  `ccteam_token`；缺 / 错 token → 401 reject upgrade
- tmux output mirror：
  - 启动时 `tmux pipe-pane -o -O -t <session>:0.0
    "cat >> ~/.ccteam/pty/<slug>-<sid>.fifo"`（FIFO 不存在先 `mkfifo`）
  - spawn tokio task: tail FIFO → broadcast(capacity 256) → 所有 WS 订阅者
  - 多 WS 订阅同一 session 共享一个 FIFO + 一个 broadcaster（refcount）
  - 所有 WS 断开后 stop pipe-pane（`tmux pipe-pane -t <session>:0.0`，
    去 args 关闭）+ unlink FIFO
- input 路径：WS 收 binary frame → `tmux send-keys -t <session>:0.0 -l --
  <bytes>`（`-l` literal）；控制帧 `{"type":"resize",...}` 走专门 channel
  （tmux refresh-client `-C`）
- backpressure：broadcast Lag(N) → 关闭 lagging client（不阻塞其他订阅者）；
  类似 SSE 现有 `reconnect_hint` 路径
- 测试（Rust 侧）：
  - tmux mock 用 `crates/ccteam-core/tests/common` 已有的 tmux runner
  - 启动 + WS connect + 收到 expected bytes
  - 多 WS 订阅同 session 共享 mirror
  - WS auth gate（无 cookie 401，有 cookie 200）
  - WS close → pipe-pane 停止（最后一个订阅者下线后）

**Acceptance**：本地 `ccteam web` 启动 → `wscat -c
ws://localhost:7331/ws/<slug>/pty -H "Cookie: ccteam_token=<hex>"` 看到
tmux 实时输出；发 keystroke 回去 tmux session 收到

### F57 — xterm input wiring in SPA（lift AoE `useTerminal` + ccteam adapter）

**Why**：F56 backend 立好后，SPA 端把 AoE 的 `useTerminal` hook 原样 lift，
adapter 层指向新 WS 路由。

**Scope**：
- 从 AoE `web/src/lib/useTerminal.ts` + `web/src/components/TerminalView.tsx`
  lift（桶 A），改 WS URL 模板：`/ws/{slug}/pty` 或 `/ws/{slug}/{sid}/pty`
- `@wterm/react` + `@wterm/core` + `@wterm/dom` 依赖保留（package.json）
- WS 重连指数退避（lift AoE 1s→30s cap，7 retries）
- Resize handling：ResizeObserver 监听容器 → 250ms debounce → 发
  `{"type":"resize"}` 控制帧
- 移动端 keyboard 处理：lift AoE `useMobileKeyboard.ts`（桶 A）+
  `MobileTerminalToolbar.tsx`（Ctrl modifier、Esc、Tab 等软键盘缺失键）
- "Back to live" FAB：用户上滚 → tracked → 显示 FAB；点击 scroll-to-bottom
- ProjectDetail.tsx / SessionDetail.tsx 嵌入 `TerminalView`
- 测试（playwright）：
  - 启动 mock WS（tests/fixtures/ws-mock-server.ts）
  - terminal mount + 接收 bytes + render
  - 发 keystroke + WS 回传
  - resize debounce

**Acceptance**：detail page 看到 live tmux pane；点击 terminal 区域即可输入；
Ctrl-C / 退出 / `/exit` 都能直接发；移动端软键盘弹出 terminal 不被遮

### F58 — Write actions（btw / inject_decision / pause / resume）+ auth flow

**Scope**：
- React 表单组件接 7 条 write 端点（F52 JSON 双发）
- 表单提交不刷页；toast 反馈 success / error；表单 inline 验证（text length 1-4000 / 1-8000）
- Decision 候选选择器（subagent 调研 §3 `decision_candidates: Vec<String>`）
- **重要**：在 user-manual 里明示两条独立通道：
  - **直接输入**（F56/F57 WS PTY）→ 等价 tmux attach，不走 idle-aware
  - **/btw 注入**（本 finding）→ 走 ccteam orchestrator 的 idle-aware 路径
- Auth flow：URL `?token=ccteam:<hex>` → cookie shim 已有（auth.rs:215-229）
  → SPA 跑起来后 cookie 自动随 fetch / WS 走；token 失效 → `useFetch` /
  `useTerminal` hook 检测 401 → 弹 `TokenEntryPage`（桶 A lift）
- 测试：playwright 模拟提交 + 401 失效流

**Acceptance**：在 detail page 内提交 BTW 不刷页；invalid token → 弹回
登录页；提交后 events SSE 推回事件

### F59 — htmx UI retirement + e2e + ship gate

**Scope**：
- 删 `templates/dashboard.html`、`templates/project.html`、`templates/session.html`
  （`base.html` 保留作为 minimal SSR fallback）
- 删 askama dep；保留 axum + tokio-broadcast + axum WS
- `routes/dashboard.rs` / `project.rs` / `session.rs` → 301 redirect 到 `/app/...`
- 旧静态资源 `assets/htmx.min.js`、`htmx-ext-sse.js`、`xterm.js`、`xterm.css`、
  `style.css` 全删（SPA 自带打包）
- 旧 `pane_snapshot.rs` route 保留（兼容旧客户端 ANSI snapshot；V0.3.3
  评估是否清）
- e2e：playwright happy path（dashboard → project → session → BTW →
  decision → pause/resume → terminal input/output）；WS terminal e2e
  用 mock tmux
- 文档：`docs/v0-3-2/user-manual.md` 写 SPA 用法 + 两条输入通道（WS PTY
  vs `/btw` 注入）说明
- bump `workspace.package.version` → `0.3.2`，`cargo test --workspace` ≥
  V0.3.1 baseline + F52-F58 新增测试

**Acceptance**：`ccteam web` 启动后 `/` 自动 301 → `/app/`；htmx 路径全
404；playwright 全绿；`cargo test --workspace` 全绿

---

## 5. 不做（V0.3.2 内，V0.3.3+ deferred）

- **CodexAdapter 完整实现** — slip 到 V0.3.3（参考 V0.3.1 PRD §10.3 +
  `docs/research/ccteam-codex-integration.md` M1-M2）；V0.3.1 README 已
  加 erratum 行
- **flex workflow promotion** — V0.3.3 / V0.4（依赖 Fat Skills evolution，
  本轮不动；详 `docs/research/thin-harness-fat-skills-architecture-improvement.md`）
- **paired terminal**（AoE 概念）— V0.3.3+；本轮 WS PTY 仅暴露 session 主
  pane，不开第二 PTY 通道
- **WS PTY 多 pane / split-window**支持 — V0.4（tmux 多 pane 渲染复杂度高）
- **session-level inbox 细分** — V0.4（用户撞 cross-talk 再加）
- **flex retro_schema enable** — V0.3.3（依赖 promotion 落地）
- **`mcp__codex__codex` MCP peer 注册** — V0.3.3+（依赖 CodexAdapter）
- **mobile push notification 真实接入** — V0.4（manifest + sw 骨架本轮 ship，
  Web Push 协议端 V0.4）
- **subagent live progress API** — V0.4（依赖 Claude Code upstream API）
- **harness snapshot 历史 archive** — V0.4（F46 仅采样，不归档）
- **statusline wrapper 多用户自动检测** — V0.4

---

## 6. 不可破的红线

> 直接 inherit 自 `CLAUDE.md §三` + `docs/tech-design.md` + V0.3.1 PRD §三
> 红线表。V0.3.2 PR review 要 grep 矩阵（详见 dev-plan.md §红线 grep）。

- **progress.jsonl 仍是唯一 orchestrator 状态事实来源** — web SPA 仅 UI 展
  示通道，不参与 orchestrator 决策；pane-snapshot ANSI **不**回写 state
- **tmux 长 session 不主动 kill** — write 动作只投递文件系统（inbox / decision /
  user_pause_pending）；不引入 web 端 `tmux kill-session` 路径
- **default 1M context** + idle-aware 注入仍在 ccteam-core，web 不绕开
- **token auth 不退化** — cookie shim + Bearer header 双道仍在；SPA 不引入
  CSRF 漏洞（同源 + SameSite=Strict 已有）
- **不解析 tmux 终端输出** — pane-snapshot 仅 UI；不从 ANSI 提取语义
- **`ccteam-core` 零团队字面量** — SPA dashboard 按 `team` 字段渲染 badge，
  不 hardcode "ccteam" / 任何 team 名
- **`progress.jsonl` flex 路径** — `~/.ccteam/progress/<slug>/<sid>.jsonl` 仍
  按 V0.3.1 F50 落档；SPA SSE 通道与之 1:1

---

## 7. 测试 baseline & 策略

- Rust 侧：`cargo test --workspace` baseline `833/0`；F52-F58 每 PR 新增至少
  3 测试（JSON shape / SSE wire / 写动作）；ship gate 测试总数预计 +30~+50
- 前端侧：**playwright** 沿用 AoE 现成 config，迁 `crates/ccteam-web/web/tests/`；
  每个 F-finding 至少 1 端到端 happy path；CI 接入 deferred 到 V0.3.3（本轮
  本地跑过即可）
- 不引入 vitest（AoE 自带，但 ccteam 测试纪律以 Rust 为主，前端单测 V0.4
  再补）
- Clippy：9 pre-existing errors 不本轮处理，新增 0 warning（CLAUDE.md §五.5）

---

## 8. PR sequencing & worktree 拆分

> 跟 V0.3.1 一致：每个 finding 单独 worktree，subagent 派工，主 session
> review / fix / merge。

```
F52 (JSON API)         → /tmp/ccteam-v032-f52   ────┐
F53 (vite scaffold)    → /tmp/ccteam-v032-f53   ────┤ 互独立，并发
F56 (WS PTY backend)   → /tmp/ccteam-v032-f56   ────┤ 与 F52/F53 互独立，可并发
                                                     │
F54 (Dashboard SPA)    ← depends on F52 + F53 ──────┤
F55 (Detail page)      ← depends on F54         ────┤ 序列
F57 (xterm input wire) ← depends on F55 + F56   ────┤
F58 (Write actions)    ← depends on F55         ────┤ 与 F57 可并行
                                                     │
F59 (htmx retire)      ← depends on F54-F58     ────┘ ship gate
```

每 PR 描述映射 V0.3.1 PR template：

- `requirements.md` §二某痛点（多数映 §痛点 1 + §痛点 9，可视化 / 远程操控）
- `docs/v0-3-2/prd.md §<F-num>`
- `dev-coupling-audit.md` F<N>（V0.3.2 新增 F52-F58 entry）
- `interfaces.md`（每个 F-finding 改协议都需同步）

每个 PR commit subject 用 `v0.3.2:` 前缀；最后 ship gate commit
（F58 merge 后）bump `workspace.package.version` → `0.3.2`。

---

## 9. 待用户回复后才能动笔

`docs/v0-3-2/dev-plan.md` 在 §3 决策 confirm 后写：

- 每个 F-finding 的 subagent briefing 完整模板
- 红线 grep 矩阵
- 依赖图 + worktree 命令
- 测试增量预估
- review checklist

`docs/v0-3-2/user-manual.md` 在 F58 ship gate 前写（参考 V0.3.1 user-manual
结构）。

---

## 附 A — 参考资料

- `references/agent-of-empires/web/`（fork 源）
- `references/agent-of-empires/web/DESIGN.md`（AoE 自身的 UI 设计 notes）
- `references/agent-of-empires/docs/guides/web-dashboard.md`（AoE 后端协议
  讲解）
- `crates/ccteam-web/` 当前实现（subagent 调研报告 task #1 完整 inventory）
- V0.3 PRD `docs/v0-3/prd.md` §3.8 当前 web UI 设计 SoT
- V0.3.1 PRD `docs/v0-3-1/prd.md` §10 deferred 列表
- `docs/research/ccteam-codex-integration.md`（若 Shape B 取得 CodexAdapter
  scope）
