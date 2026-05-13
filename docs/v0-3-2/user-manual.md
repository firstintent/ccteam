# V0.3.2 User Manual — Web SPA + WS PTY + 两条输入通道

> 配套 V0.3.2 ship(2026-05-14)。涵盖 SPA 用户面三块新内容:
>
> 1. 两条**互不相同**的输入通道(WS PTY 直接 attach vs `/btw` 注入)
> 2. token auth 流程(`?token=...` → cookie shim)
> 3. dev 环境跳过 SPA bundle 的开关
>
> 老版 V0.3 / V0.3.1 web 手册仍适用部分(SSE 流 / progress.jsonl /
> harness panel 行为不变),按需到 [`docs/v0-3-1/user-manual.md`]
> (../v0-3-1/user-manual.md) 翻历史段。

---

## 1. 启动

```bash
ccteam web                         # 默认 loopback 127.0.0.1:7331,无 auth
ccteam web --bind 0.0.0.0:7331     # LAN 暴露,**强制** token auth
ccteam web --bind 0.0.0.0:7331 \   # 关闭 auth(LAN 上 = RCE 风险)
  --no-auth                        # 启动后 5s grace 让 Ctrl-C 退
```

Loopback bind 默认无 auth(本机才能访问);非 loopback bind 自动生成
`~/.ccteam/web-token`(mode 0600)并把 `ccteam:<hex>` echo 到 stderr,
浏览器拼成 `https://<host>:7331/?token=ccteam:<hex>` 一次性带入即可。

启动后**唯一**用户面是 `/app/`(React SPA)。所有 V0.3 / V0.3.1 时代
的 HTML 路径(`/`、`/project/<slug>`、`/session/<slug>/<sid>`)在
V0.3.2 F59 后**301 重定向到 SPA**;旧 bookmark 仍可用,只是会跳一次。

---

## 2. 两条输入通道:**WS PTY** vs **`/btw` 注入**

V0.3.2 SPA 的项目详情页 / session 详情页同时展示两个写入入口。它们
在底层走**两条独立的传输 + 决策路径**,使用前必须明白区别。

### 2.1 直接 WS PTY 输入(= "在 web 内打字")

- **入口**:detail page 上的 xterm 终端区,鼠标点进去即可输入键序列
- **协议**:WebSocket `/ws/<slug>/pty`(workflow / 默认绑定主 tmux session)
  或 `/ws/<slug>/<sid>/pty`(flex session-scoped),WS subprotocol
  `ccteam-pty.v1`,二进制 frame 透传 raw keystroke bytes
- **后端**:`portable_pty::NativePtySystem::openpty` + 子进程
  `tmux attach-session -t <session_name>`,字节流双向桥接;Ctrl-C、
  方向键、`/exit`、`Esc` 等全支持(等价 `tmux attach`)
- **语义**:**绕开 ccteam orchestrator** — ccteam-core 不知道你打了
  什么字;Claude / Codex session 把它当人类直接 attach 处理。无
  idle-aware,无 phase 注入排队,无 `progress.jsonl` 记录(只有 Claude
  自己的 hooks 会记)

> 用户输入若包含 `/exit` 或 `Ctrl-C`,会**直接关掉**当前 session。
> ccteam 守护逻辑(silence / cost watcher)只观察,不会重启 — 这
> 是 V0.3.1 红线"永不主动 kill"的对偶面:**用户在 web PTY 内自己关
> session 是合法操作**,等同 `tmux attach` 里手动 `/exit`。

### 2.2 `/btw` 注入(= "通过 orchestrator 投递")

- **入口**:detail page 上的 "BTW (inject note)"(项目级)或
  "BTW (session inbox)"(flex session 级)表单
- **协议**:`POST /api/<slug>[/<sid>]/btw`,body JSON `{"text":"..."}`,
  返回 `{"ok":true}` 不重定向
- **后端**:写入 `~/projects/<slug>/.ccteam/inbox/btw-<ts>.md`(项目
  级)或 `~/projects/<slug>/.ccteam/sessions/<sid>/inbox/btw-<ts>.md`
  (flex session 级,V0.3.1 F50 范式);**ccteam-core 守护逻辑**会在
  下一次 phase 边界 / idle prompt 时把内容追加到 session 上下文里
- **语义**:走 **idle-aware 注入路径** — orchestrator 等 session 进入
  idle(Stop / SubagentStop / idle_prompt 信号)才注入,**不会**打断
  正在执行的工具调用;`progress.jsonl` 会写一条 `kind:btw_injected`

### 2.3 何时用哪条

| 场景 | 用 WS PTY | 用 `/btw` |
|---|---|---|
| 想立刻打断 session 并改方向 | ✓(Ctrl-C + 重输 prompt) | ✗(要等 idle 才注入) |
| 想在不打断当前工具调用的前提下追加提示 | ✗(打字 = 直接干扰) | ✓ |
| 想给 session "下班后看一眼" | ✗(WS 断开 keystroke 也丢) | ✓(写文件,持久) |
| 想 `/exit` 一个 session | ✓(直接打字) | ✗(`/btw` 不解析 slash 命令) |
| 想发 `**META-AGENT DECISION**:` 解 escalation | ✗ | ✓(走 InjectDecisionForm) |
| 想 pause / resume 守护 | ✗ | ✓(走 Pause/Resume 按钮) |

> 设计意图见 [`prd.md §3.2`](prd.md) 表格 "用户键盘输入 idle-aware"
> 与 "重要副作用" 段。

### 2.4 表单速记

V0.3.2 F58/F59 在 detail page 暴露的写动作组件:

- **`BtwForm`** — textarea,1..=4000 字符;onSubmit 后清空 textarea,
  toast 提示 "BTW submitted"
- **`InjectDecisionForm`** — 仅 ProjectDetail 出现;`<select>` 列
  `~/projects/<slug>/.ccteam/` 下的待决策文件,textarea 写 body
  (推荐以 `**META-AGENT DECISION**:` 开头让 orchestrator 当作权威
  resolution)
- **`PauseResumeButtons`** — ProjectDetail 永远渲染;`paused` 状态
  从 `ProjectSummary.state.user_pause_pending` 读;SessionDetail 也
  渲染但 `paused` 暂固定 `false`(V0.3.1 F50 让 pause 仍是项目级标志)

---

## 3. Token auth 流程

```
浏览器                                ccteam web 后端
  │                                       │
  │  GET /?token=ccteam:<hex>             │
  ├──────────────────────────────────────►│
  │                                       │   auth.rs::auth_layer
  │                                       │   - 校验 token 匹配
  │                                       │   - Set-Cookie: ccteam_token=...; HttpOnly; SameSite=Strict
  │  301 Moved Permanently → /app/        │
  │◄──────────────────────────────────────┤
  │                                       │
  │  GET /app/                            │   (cookie 自动附带)
  ├──────────────────────────────────────►│
  │  200 OK, index.html                   │
  │◄──────────────────────────────────────┤
  │                                       │
  │  fetch /api/v1/projects               │   (cookie 自动附带)
  ├──────────────────────────────────────►│
  │  200 OK, JSON                         │
  │◄──────────────────────────────────────┤
```

要点:

- **首次访问**用 URL `?token=` 一次性把 cookie 烤进浏览器,之后
  bookmark `/app/` 即可(cookie 持续有效直到清浏览器数据 / 服务端轮换)
- **token 失效**时(`rm ~/.ccteam/web-token` 后服务端重启会生成新值)
  → 任何 `/api/*` 调用 401 → fetchInterceptor 派发
  `TOKEN_EXPIRED_EVENT` → App.tsx 顶层 `TokenEntryGate` 切到
  `TokenEntryPage`,提示重粘 `ccteam:<新 hex>`
- **WS PTY 也校验同一个 cookie**:WebSocket upgrade 时 axum 提取
  `Cookie` header,无 cookie 直接 401 拒绝 upgrade(不接受 `?token=`
  query string 走 WS,避免进 access log)
- **localStorage 路径**:PWA 在 iOS standalone 模式会丢 cookie,
  fetchInterceptor 把 token 也写一份到 `localStorage["ccteam-token"]`
  并以 `Authorization: Bearer ccteam:<hex>` header 附带,服务端通过
  `X-Aoe-Token` 响应头让 PWA 跟上轮换(沿 AoE 设计)

---

## 4. dev 环境跳过 SPA bundle

V0.3.2 F53 后 `cargo build -p ccteam-web` 默认会驱动 `npm run build`
拼 SPA bundle(经 `build.rs` 转 `rust-embed::RustEmbed` 嵌入二进制)。
本机开发时(改 Rust 不动前端 / 在另一个 worktree 已经 bundle 过)
可以跳过这步,两种方式:

```bash
# 方式 1:cargo feature flag —— 完全关掉 web-bundle
cargo build -p ccteam-web --no-default-features

# 方式 2:env override —— 保留 feature,但 build.rs 跳 npm
CCTEAM_SKIP_WEB_BUILD=1 cargo build -p ccteam-web
```

两种方式都会让 `build.rs` 在 `web/dist/index.html` 写一个 placeholder
(返回 "SPA bundle skipped — set CCTEAM_SKIP_WEB_BUILD=0 or default
features to rebuild" 的字串),保证 `rust-embed` 派生有目标可嵌。
跑 `ccteam web` 启动后访问 `/app/` 也会看到这条提示。

要重新 build 真 SPA:

```bash
# 方式 1:回 default features
cargo build -p ccteam-web    # 自动触发 npm run build

# 方式 2:直接在 web/ 里手动 build
cd crates/ccteam-web/web && npm install && npm run build
# 然后下一次 cargo build 会发现 dist/index.html 是真 bundle 不再覆盖
```

> 注:`npm install` 第一次跑会装 ~900 MB node_modules(`@wterm/*` +
> shiki + react-diff-viewer 等 deps);CI 设计已在 PRD §7 deferred,
> 本机首次 build 慢属正常。

---

## 5. 红线提示(再过一遍)

V0.3.2 的 web 改造**不**触动 V0.3.1 之前已落档的 4 条主红线
(详见 ccteam 项目根 `CLAUDE.md` §三 + `prd.md §6`):

- **`progress.jsonl` 仍是唯一 orchestrator 状态事实来源** — WS PTY
  输出仅 UI 通道,不参与守护决策;watcher 仍只读 `progress.jsonl`
- **不解析 tmux 终端输出** — WS PTY 字节透传,ccteam-core 不接 pipe;
  ccteam-web 仅 broadcast bytes
- **永不主动 kill 长 session** — WS 关闭不 kill tmux;cost / silence
  watcher 不触发 kill;用户在 WS 内 `/exit` 是显式授权
- **token auth 不退化** — cookie + Bearer header 双道;WS upgrade 强校验;
  CSRF defense 沿 SameSite=Strict
