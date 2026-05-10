# PRD V0.3 — Web UI(读 + 受限写)

> 范围:V0.3 主线 = 一个本地 / 局域网 web UI,展示 ccteam 全局项目 / agent /
> subagent 状态,并提供有限的写动作(`/btw` 派单、注入决策、pause/resume)。
> 新 crate `crates/ccteam-web` 暴露 `ccteam web` 子命令,内嵌 HTTP server。
>
> base = `origin/main` `2988de6`(V0.2.2 ship 终点,F44 反向回滚已 merge);
> 测试 baseline 631/0;workspace.version 起点 `0.2.2`,V0.3 ship gate 前 bump
> `0.3.0`。
>
> V0.3 是首个开"对外可视层"的版本,跟 V0.1 / V0.2 主架构红线兼容(progress.jsonl
> 仍是 SoT,session 仍 tmux 长跑,控制面板仍数据驱动)。本 PR 是 docs-only
> kickoff;实施跨 5 milestone(M5.0-M5.4)走 worktree-per-PR 派工。

---

## 1. 背景

### 1.1 痛点映射

V0.3 主修 `requirements.md` 两条痛点:

- **痛点 7「进度永远不透明」**:V0.2 之前用户唯一观测面是 tmux attach + outbox
  目录手翻,跨多项目并发(M3+ team factory ship 后用户实际跑 ≥ 3 项目)难一目了然
  全局态。痛点 7 用户原话「我不在的时候它自己跑,我在的时候用一句话就能知道
  '现在怎么样了'。所有进度都看得到、可追溯,但不需要我主动去查」。终端 UI 满足
  「自己跑」,但「一句话就知道」需要可视化聚合。Web UI 是直接答案。
- **痛点 9「AI 团队需要人来主持」**(部分):web UI 不是新主持人,但把「现在哪
  个项目卡了 / 谁在 idle / 谁要决策」聚合到一屏,把用户从 attach 多 session 解放,
  跟 V0.2 watchdog + V0.2.2 enriched outbox 形成「自检 → 用户一屏看 → 一键 nudge」
  闭环。

不修痛点(V0.3 显式排除 — 详 §10):跨项目记忆 / 长 session 守护 / 多 session
fan-out 之类已 V0.1-V0.2 ship 项,web UI 只「展示 + 受限写」,不替代任何已 ship
机制。

### 1.2 架构定位

参考 `tech-design.md` §6 扩展点表,V0.3 web UI 走以下接入面:

- **§6.4 channel layer**:web UI 是新 channel(继 telegram channel 后第二个;
  方向上是「桌面侧」channel,不上 cloud);消费 progress.jsonl 事件、写 inbox /
  control 文件,**不绕开 orchestrator 状态机**
- **§5.5 progress.jsonl SoT**:所有读端数据来源 = progress.jsonl + state.json,
  **绝不解析 tmux 终端输出**(F38 截图是显式 vt100 状态机渲染,不是文本解析)
- **§6.9 idle-aware 注入**:web UI 写 `/btw` 走跟 telegram channel + MCP
  `send_to_session` 完全相同的 inbox + idle dispatch 路径,不开新通路
- **§3.7 跨项目记忆零检索**:web UI 不读 `~/.claude/rules/`,不解析 memory
  文件;用户要看 lessons 走 Claude session 内 `/memory` 命令,不归 web UI

### 1.3 用户接入面增量

V0.2 / V0.2.2 用户接入面三层:

| 层 | M3 起 | V0.2 / V0.2.2 |
|---|---|---|
| 终端 attach | tmux attach session | + cct-control skill in meta-agent + cct snapshot(F38)|
| MCP `mcp__ccteam__*` | M2 ship | + watchdog + enriched outbox + screenshot(F38)|
| 文件系统 / `progress.jsonl` | M0+ | + outbox 富字段(F35)|

V0.3 加第四层:

| 层 | V0.3 |
|---|---|
| **本地 / 局域网 web UI** | dashboard 全项目状态 + 项目详情页(progress 事件流 / outbox / pane 截图)+ 写动作表单(/btw / inject_decision / pause / resume)|

四层并存,各有强项 — terminal 给 power user 全控,MCP 给 meta-agent 自动化,
filesystem 给 hooks / 调试,web UI 给「一屏看全局」。

---

## 2. 范围

5 milestone:

- **M5.0**:`ccteam-web` crate scaffold + `ccteam web` 子命令 + write helper
  promote 到 `ccteam-core::actions`(关键解耦,详 §3 / §8.1)
- **M5.1**:read-only dashboard(项目列表 + 项目详情)— **独立可 ship**
- **M5.2**:SSE 实时事件流 + 按需 PNG 截图(F38 reuse)— **独立可 ship**
- **M5.3**:写动作(/btw / inject_decision / pause / resume)+ 默认 token 鉴权
- **M5.4**:E2E + retro + workspace.version bump 0.2.2 → 0.3.0 + V0.3 ship gate

总规模估:~2.5 kLOC(新 crate + 模板 + handler + 测试)+ 端到端 ~3-4 周
(单人,5 PR 串行;M5.1 / M5.2 可独立 ship 让用户提前 dogfood)。

---

## 3. M5.0 — Scaffold + write helper promote

### 3.1 问题

新 crate 的 stand-up 工作 + 一个**关键解耦决策**:web UI 不能 depend on
`ccteam-cli`(CLI binary 是 library 是反模式 — 把可执行 entry 当 lib 再被 lib
crate 反向依赖,dep 图倒挂)。web UI 必须 depend on `ccteam-core`,所以写动作
helper 必须从 `ccteam-cli::commands` / `ccteam-cli::mcp_serve` 提到
`ccteam-core::actions`。

dispatcher 已审计当前 surface:

- **读侧 helper(已 public)**:`ccteam_core::ProjectState` /
  `ccteam_core::CcteamPaths` / `ccteam_core::render_screenshot`(F38)/
  `ccteam_core::tmux::{capture_pane_tail, capture_pane_with_ansi}` /
  `ccteam_core::check_daemon_health` + `DaemonHealth` /
  `ccteam_core::{InboxFrontMatter, InboxMessage, SessionMailbox}` /
  `inbox_filename` / `pick_unused_slug` / `bootstrap_project` /
  `ccteam_cli::commands::{collect_projects, collect_recent_events,
  run_resume, run_show, run_new}`(`run_resume` 是 pub fn,`run_new` /
  `run_show` 已被 `mcp_serve.rs` 跨模块调用,事实上算 pub)
- **写侧 helper(目前内部 fn,不 public)**:`tool_send_to_session` /
  `tool_inject_decision`(`crates/ccteam-cli/src/mcp_serve.rs::456`、`::500`,
  `fn` 非 `pub fn`)/ pause / resume 逻辑(MCP 只在 mcp_serve 内 inline 实现,
  没单独 pub fn)

### 3.2 设计

#### 3.2.1 新 crate `crates/ccteam-web`

- workspace member,sibling 于 `ccteam-cli` / `ccteam-core` / `ccteam-hooks`
- 依赖:`ccteam-core` only(**不 dep `ccteam-cli`**;dep 图保持 cli + web 都
  下沉到 core)
- 入口:`pub async fn serve(opts: ServeOpts) -> Result<()>` —
  `ccteam-cli::commands::run_web(opts)` 调用
- 不是独立 binary。`ccteam web` 是 ccteam-cli 子命令,run_web 内 `tokio::runtime`
  block on `ccteam_web::serve(opts)`

#### 3.2.2 Tech stack(pin minor 版本)

| 用途 | crate | 版本 |
|---|---|---|
| HTTP server | `axum` | `0.8`(latest stable) |
| async runtime | `tokio` | workspace `1.x`(已 pin) |
| HTML templating | `askama` | `0.12` |
| File watching | `notify` | workspace `8`(已 pin) |
| Tracing | `tracing` / `tracing-subscriber` | workspace |
| Serde / JSON | `serde` / `serde_json` | workspace |
| HTTP client(测试用)| `reqwest` | `0.12`,dev-deps |

**前端**:`htmx`(~14 KB minified,vendored 进 `assets/`,通过 `include_bytes!`
编译期打包)+ minimal CSS(<5 KB,inline 模板)。无 npm / 无 Vite / 无 build
toolchain。

模板树:

```
crates/ccteam-web/
├── Cargo.toml
├── src/
│   ├── lib.rs            # serve(opts) entry
│   ├── routes/           # axum router
│   │   ├── mod.rs
│   │   ├── dashboard.rs  # GET /
│   │   ├── project.rs    # GET /project/<slug>
│   │   ├── sse.rs        # GET /sse/all, /sse/project/<slug>
│   │   ├── screenshot.rs # GET /screenshot/<slug>.png
│   │   └── actions.rs    # POST /api/<slug>/{btw,inject_decision,pause,resume}
│   ├── auth.rs           # token middleware
│   ├── watcher.rs        # notify recursive watcher → broadcast channel
│   └── templates.rs      # askama template structs
├── templates/
│   ├── base.html
│   ├── dashboard.html
│   └── project.html
└── assets/
    ├── htmx.min.js       # vendored ~14 KB
    └── style.css
```

#### 3.2.3 Write helper promote(M5.0 关键解耦)

新建 `crates/ccteam-core/src/actions.rs` 模块,提 4 个函数 public:

```rust
// 全部按 M2 mcp_serve.rs 现有内部 fn 等价语义提
pub fn send_to_session(paths: &CcteamPaths, slug: &str, text: &str) -> Result<SendResult>;
pub fn inject_decision(paths: &CcteamPaths, slug: &str, decision: DecisionInput) -> Result<InjectResult>;
pub fn pause(paths: &CcteamPaths, slug: &str) -> Result<()>;
pub fn resume(paths: &CcteamPaths, slug: &str) -> Result<()>;
```

callsite 重写:

| 文件 | 改动 |
|---|---|
| `ccteam-cli/src/mcp_serve.rs` | `tool_send_to_session` / `tool_inject_decision` 内部 logic 全提到 `ccteam_core::actions::*`;mcp_serve 内 fn 留 wrapper 拆 args + JSON encode 输出 |
| `ccteam-web/src/routes/actions.rs` | 直接调 `ccteam_core::actions::*`,无中间层 |

**Dep 图**(M5.0 ship 后):

```
ccteam-hooks ──► ccteam-core ◄── ccteam-cli
                     ▲
                     └────────── ccteam-web   (新)
```

干净;`ccteam-web` 不依赖 `ccteam-cli`。

#### 3.2.4 `ccteam web` CLI 入口

`crates/ccteam-cli/src/main.rs` 加子命令(clap):

```rust
Commands::Web {
    /// Listen address. Default 127.0.0.1:7331 — auth disabled. 0.0.0.0:7331 enables
    /// token auth on write endpoints unless --no-auth.
    #[arg(long, default_value = "127.0.0.1:7331")]
    bind: String,

    /// Disable token auth (DANGEROUS on non-loopback bind).
    #[arg(long)]
    no_auth: bool,

    /// Custom path to read auth token from (default: ~/.ccteam/web-token).
    #[arg(long)]
    token_file: Option<PathBuf>,
}
```

`commands.rs::run_web(opts: WebOpts)` 把 clap struct 翻成 `ServeOpts`,
delegate 到 `ccteam_web::serve`。

#### 3.2.5 Health endpoint

`GET /health` → 200 JSON `{"status":"ok","version":"<pkg>","bind":"<addr>"}`。
M5.0 验收脚本调它确认 server up。

#### 3.2.6 Doctor 集成

`ccteam doctor` 加 web 检查段:

- 检测 `~/.ccteam/web-token` 存在 + mode 0600(若不是 warn)
- 检测最近 `ccteam web` 启动日志(可选 — V0.3 暂不做,V0.4 deferred)

### 3.3 不做(M5.0)

- 任何 dashboard / project view / SSE / screenshot endpoint(M5.1 / M5.2)
- 任何写动作 endpoint(M5.3)
- token 生成 logic(M5.3 一并 ship 鉴权时落)
- WebSocket(永久 deferred,SSE 足够)

### 3.4 验收

- [ ] `crates/ccteam-web/` workspace member,`Cargo.toml` 列依赖 + pin 版本
- [ ] `cargo build --workspace` 全绿(新 crate 编译过,dep 图无 cycle)
- [ ] `crates/ccteam-core/src/actions.rs` 新模块,4 个 pub fn 落地 + 单元测试
- [ ] `mcp_serve.rs` 内 `tool_send_to_session` / `tool_inject_decision`
  delegate 到 `ccteam_core::actions::*`;MCP 测试套件全绿(回归保证)
- [ ] `ccteam web --bind 127.0.0.1:7331` 启动,`curl http://127.0.0.1:7331/health`
  返 200 + JSON
- [ ] `ccteam doctor` 不退步;web 段 detection 不失败
- [ ] `cargo test --workspace` ≥ 631 baseline + ≥ 4 new(actions promote 单元)

---

## 4. M5.1 — Read-only dashboard

### 4.1 问题

用户要「一屏看全局」。M5.1 落最小可 dogfood 的 read-only 面 — 项目列表 +
项目详情页,模板渲染,无实时更新(M5.2 加 SSE 才有)。

### 4.2 设计

#### 4.2.1 路由

| Path | Handler | 说明 |
|---|---|---|
| `GET /` | `dashboard.rs::handle_index` | 项目列表(table)+ 顶部 nav |
| `GET /project/<slug>` | `project.rs::handle_project` | 项目详情(state + recent events + outbox + screenshot button)|
| `GET /assets/htmx.min.js` | static handler | vendored htmx |
| `GET /assets/style.css` | static handler | minimal inline-able CSS |
| `GET /health` | M5.0 已落 | health JSON |

#### 4.2.2 Dashboard view(`/`)

调 `ccteam_cli::commands::collect_projects(paths)` 返 `Vec<ProjectSummary>`;
askama 渲染 table 列:

| 列 | 来源 |
|---|---|
| Slug | `ProjectSummary::slug` |
| Team | `ProjectSummary::team` |
| Phase | `state.json::current_phase` |
| Last event | progress.jsonl 末事件 timestamp |
| Status badge | `daemon_health` + 静默时长(`Healthy` / `Idle 5m` / `Stalled 30m+`)|
| Cost | accumulated `cost_usd` |

**status badge 计算口径**:不重新发明,直接复用 V0.2.2 F35 silence_classifier
**只读**地分类(M5.1 不写)— `Healthy` / `Terminal` / `SubagentBusy` / `MidToolHung` /
`PostStopLimbo` 之一,渲染成颜色 badge。

#### 4.2.3 Project detail view(`/project/<slug>`)

调 `collect_recent_events(paths, slug, limit=200)` 返事件列表 + `state.json`
完整状态。模板渲染:

- **Header**:slug / team / phase / cost / created_at
- **Tabs / sections**:
  1. State(`state.json` JSON pretty-print,折叠)
  2. Recent events table(progress.jsonl tail 200)
  3. Outbox messages(`~/projects/<slug>/.ccteam/outbox/*.md` parse 前置)
  4. Screenshot panel(M5.2 加 button)
- **Sidebar**(M5.3 加 write actions form);M5.1 留占位 placeholder

#### 4.2.4 Outbox parsing

复用 `ccteam_core::{InboxFrontMatter, InboxMessage, SessionMailbox}` 已 public
接口扫 outbox。文件名 sort newest first,展示前 20 条。

#### 4.2.5 模板与样式

- `templates/base.html`:HTML5 boilerplate + nav + htmx script tag
- `templates/dashboard.html`:extends base,table 列
- `templates/project.html`:extends base,sections
- `style.css`:无 framework,~3 KB(monospace + dark-mode-friendly contrast)

无 JS framework;无 React / Vue / Svelte;htmx 引入只为 M5.2 / M5.3 的 SSE +
form swap,M5.1 阶段静态渲染足够。

### 4.3 不做(M5.1)

- 实时更新(M5.2)
- screenshot rendering(M5.2)
- 任何写动作(M5.3)
- 项目创建 / 删除 UI(永久 deferred — 走 meta-agent + cct-project-creator skill)
- 配置 UI(永久 deferred)
- 用户管理(永久 deferred)

### 4.4 验收

- [ ] `GET /` 返 200 + valid HTML + 列出 `~/.ccteam/projects/` 全 slug
- [ ] `GET /project/<slug>` 返 200 + state + events + outbox 段
- [ ] 不存在 slug → `GET /project/nonexistent` 返 404
- [ ] CSS / htmx assets `GET /assets/*` 返 200
- [ ] askama template 编译期类型检查
- [ ] `cargo test --workspace` 不退步;新增 ≥ 6 e2e(`reqwest` 起 server,断
  state code + HTML contains expected slug)
- [ ] **独立可 ship** — M5.1 merge 后用户已能看 dashboard,M5.2 / M5.3 不 block

---

## 5. M5.2 — SSE event push + 按需截图

### 5.1 问题

M5.1 是静态渲染,用户刷页才看到新事件。M5.2 加实时推送 — progress.jsonl 写入
→ 浏览器表格行新增,无需刷页。截图按 click 渲染,不自动 polling(F38 渲染百
ms 级,polling 浪费 CPU)。

### 5.2 设计

#### 5.2.1 文件 watcher 架构

- 单个 recursive `notify` watcher 监 `~/.ccteam/progress/`(目录,非 per-file)
- watcher 用 `notify::recommended_watcher(handler)` + `RecursiveMode::Recursive`
- 文件名约定:`<slug>.jsonl`(M0 起协议)— 新文件出现自动覆盖,不需要 re-arm
- 同样监 `~/.ccteam/state/`(项目元数据变更触发 dashboard refresh)

#### 5.2.2 事件 dispatch

```rust
// crates/ccteam-web/src/watcher.rs
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<EventMsg>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EventMsg {
    pub slug: String,        // server-side injected (parsed from filename)
    pub event_json: String,  // raw progress.jsonl line
}
```

Watcher 后台 task:

- `notify` callback 收到 `Event::Modify(Path)` → 计算 path 末次 read offset,
  read 新增 lines,parse 出 slug from filename,广播到 broadcast channel
- broadcast `bound = 1024`(broadcast 满 → laggy receiver 被 drop,客户端 SSE
  reconnect 续上;不为 backlog 占内存)

#### 5.2.3 SSE endpoints

**两个端点,不复用一条连接**(advisor 确认设计;复用易丢追踪):

| Path | Filter | 用途 |
|---|---|---|
| `GET /sse/all` | 无 | dashboard 全局 view |
| `GET /sse/project/<slug>` | server-side filter `msg.slug == <slug>` | 详情页 view |

**wire format**:每条 SSE event payload = 单行 JSON(progress.jsonl 一行 +
server 注入 `slug` 字段):

```
event: progress
data: {"slug":"dev-foo","ts":"2026-05-10T12:34:56Z","kind":"phase_done","phase":"plan-eng",...}

```

- `event:` 字段固定 `progress`(future-proof,新 event type 加新 `event:` 名)
- `data:` 必须单行(SSE protocol 限制);progress.jsonl 本就是 JSON-Lines,
  每行一 event 已天然兼容
- keep-alive:server 每 15s 发 `: keepalive\n\n` 注释行,防代理超时(对应
  反代 / nginx 默认 60s timeout)

#### 5.2.4 htmx hookup

dashboard.html / project.html 加:

```html
<div hx-ext="sse" sse-connect="/sse/project/{{slug}}" sse-swap="progress">
  <table id="events-table">{{...初始 200 条 server-render...}}</table>
</div>
```

新事件到 → htmx swap into `#events-table` first row。不需 JS,htmx ext 自带。

#### 5.2.5 Screenshot endpoint

| Path | 行为 |
|---|---|
| `GET /screenshot/<slug>.png` | 同步调 `ccteam_core::render_screenshot(slug, opts)`(F38)→ 返 PNG bytes,Content-Type `image/png`,Cache-Control `no-cache, must-revalidate` |

**触发**:project.html 加 button:

```html
<button hx-get="/screenshot/{{slug}}.png" hx-target="#screenshot-img" hx-swap="outerHTML">
  Refresh screenshot
</button>
<img id="screenshot-img" src="/screenshot/{{slug}}.png" alt="pane snapshot">
```

页面加载时一次自动渲染(`<img src>`),后续靠 button 手动刷。**不 polling**
(F38 单次渲染 ~200-500ms,polling 烧 CPU 且 png 大不必要)。

graceful degrade:F38 已 spec'd `render_screenshot` 失败返 `Ok(None)` /
`Err`;handler 包成 504 + plain-text reason,前端 alt text 显示 "screenshot
unavailable"。

### 5.3 不做(M5.2)

- 自动 polling 截图(永远不做)
- WebSocket(SSE 足够)
- per-project SSE 端点之外的 multiplexed channel
- 历史截图 archive view(V0.4 channel layer 评估)
- mobile-responsive(V0.4)

### 5.4 验收

- [ ] `GET /sse/all` 返 SSE stream;`curl -N http://127.0.0.1:7331/sse/all`
  在另一 shell 写 progress.jsonl event 后 1s 内收到
- [ ] `GET /sse/project/<slug>` 同上,只收对应 slug 事件
- [ ] notify watcher 多 receiver 不 starve(`broadcast` channel)
- [ ] `GET /screenshot/<slug>.png` 返 PNG bytes;F38 unavailable 时返 504
  text(不 panic)
- [ ] htmx SSE swap dashboard 实时刷新表格(浏览器手测;e2e 用 reqwest stream
  断 ≥ 1 event 收到)
- [ ] keepalive `: keepalive` 注释行 15s 周期发出
- [ ] `cargo test --workspace` 不退步;新增 ≥ 8 测试(watcher dispatch /
  per-slug filter / SSE wire format / screenshot 200 + 504 / keepalive)
- [ ] **独立可 ship** — M5.2 merge 后 dashboard 实时刷,M5.3 写动作可单独 ship

---

## 6. M5.3 — 写动作 + token 鉴权

### 6.1 问题

read-only dashboard 不够 — 用户经常需要在看到 stalled 项目后立刻 `/btw 改成 X`
或 inject decision,现状要切 terminal。M5.3 落写动作。

**关键安全决策**:用户原话「暂不考虑安全问题」+ 选 0.0.0.0 bind + 写动作。
组合 `--dangerously-skip-permissions` claude session = LAN-wide RCE。本 PRD
**默认开 token 鉴权**,用户可 `--no-auth` 显式 opt-out(详 §6.4 + §9 威胁模型)。

### 6.2 设计

#### 6.2.1 路由

四个 POST endpoint,**全部走** `ccteam-core::actions::*`(M5.0 promote):

| Path | Body | Action |
|---|---|---|
| `POST /api/<slug>/btw` | `text=<urlencoded>` | `actions::send_to_session(paths, slug, text)` |
| `POST /api/<slug>/inject_decision` | `path=<rel-path>&body=<urlencoded>` | `actions::inject_decision(paths, slug, DecisionInput { path, body })` |
| `POST /api/<slug>/pause` | (空)| `actions::pause(paths, slug)` |
| `POST /api/<slug>/resume` | (空)| `actions::resume(paths, slug)` |

成功 → 303 See Other 回 `/project/<slug>`(htmx 自动跟 swap);失败 → 4xx +
plain-text error。

#### 6.2.2 表单(显式分两个,不 conflate)

`/btw` 自由文本表单:

```html
<form hx-post="/api/{{slug}}/btw" hx-target="#flash">
  <textarea name="text" required minlength="1" maxlength="4000"></textarea>
  <button type="submit">Send /btw</button>
</form>
```

`inject_decision` 结构化表单:

```html
<form hx-post="/api/{{slug}}/inject_decision" hx-target="#flash">
  <select name="path" required>
    {{#each decision_candidates}}
    <option>{{this}}</option>
    {{/each}}
  </select>
  <textarea name="body" required minlength="1" maxlength="8000"></textarea>
  <button type="submit">Inject decision</button>
</form>
```

`decision_candidates` 来自扫 `~/projects/<slug>/.ccteam/decision-*.md`(读侧 fn,
不 promote 到 actions,本身就是只读)。

`pause` / `resume` 单 button:

```html
<form hx-post="/api/{{slug}}/{{action}}" hx-target="#flash">
  <button type="submit">{{action_label}}</button>
</form>
```

#### 6.2.3 Outbox view(read-only)

project.html 加 outbox section,渲染 `~/projects/<slug>/.ccteam/outbox/*.md`
parse 前置(`InboxFrontMatter`)+ body 头 200 char 摘要 + click 展开。无写动作。

#### 6.2.4 Auth middleware

新建 `crates/ccteam-web/src/auth.rs`,axum `tower::Layer`:

```rust
pub struct AuthLayer {
    enabled: bool,
    token: Option<String>,  // None = no-auth path
}
```

bind heuristic(`serve` 内决定):

| bind | --no-auth | enabled | token gen |
|---|---|---|---|
| `127.0.0.1:*` / `[::1]:*` | false | false(loopback 信任)| 不生成 |
| `127.0.0.1:*` / `[::1]:*` | true | false | 不生成 |
| 非 loopback | false | **true**(默认 enable)| 生成或读 `~/.ccteam/web-token` |
| 非 loopback | true | false(显式 opt-out) | 不生成 |

token 生成路径:

- 首次启动检测 `~/.ccteam/web-token` 不存在 → 生成 32-byte `rand::random` →
  hex 编码 → 写文件 `mode 0600` → stdout echo:
  ```
  ccteam web listening on http://0.0.0.0:7331
  Auth token: ccteam:<token>
  Reset with: rm ~/.ccteam/web-token && restart
  ```
- 已存在 → 校验 mode 0600(否则 warn)+ 加载

middleware 行为(**整体权限,不区分读写** — 用户 2026-05-10 决策,不留 `--read-public` 后门):

- enabled = false → pass through
- enabled = true → **所有路径**(GET dashboard / SSE / screenshot / POST `/api/...`)
  统一 check `Authorization: Bearer ccteam:<token>`,constant-time 对比
  (`subtle::ConstantTimeEq`);不匹配 → 401 plain-text "auth required"
- 浏览器 GET 通过 token cookie 注入:首次访问带 query string `?token=ccteam:<...>`,
  middleware set HttpOnly cookie + 302 redirect 去掉 query;后续 GET / SSE 走 cookie。
  POST `/api/...` 仍要 `Authorization: Bearer` header(同时也是 CSRF 防御 — §6.2.5)
- 用户嫌烦走 `--no-auth`(loopback 开发态默认无 auth,非 loopback 显式 opt-out + stderr warn)

#### 6.2.5 CSRF 防御

写动作用 htmx `hx-post` 发的是 form-encoded POST,但 **要求** `Authorization:
Bearer` header(浏览器跨域 form-submit 不会自动加 Authorization 头),所以
`Authorization` header 本身就是 CSRF token — 攻击者跨域 form 无法注入这个
header,fetch / XHR 跨域要 CORS preflight,默认拒绝。

不需要单独 CSRF token 字段。

#### 6.2.6 反馈 UI

post-submit:

- 200/303 → flash banner "Sent" + 2s 自动 fade
- 4xx/5xx → flash banner red + error text

htmx `hx-target="#flash"` swap into `<div id="flash"></div>` 占位。

### 6.3 不做(M5.3)

- OAuth(Google / GitHub)— V0.4
- HTTPS / TLS termination — V0.4(假设反代;loopback 不需要)
- per-project ACL — V0.4
- session-based auth — V0.4(token 足够 V0.3)
- write 速率限制 / rate-limit — V0.4
- 多用户 — 永久(ccteam 是单用户工具)

### 6.4 验收

- [ ] `POST /api/<slug>/btw text=hello` 返 303 + `~/projects/<slug>/.ccteam/inbox/`
  落消息文件
- [ ] `POST /api/<slug>/inject_decision path=...&body=...` 返 303 + decision
  写入
- [ ] `POST /api/<slug>/pause` 返 303 + state.json `paused: true`
- [ ] `POST /api/<slug>/resume` 同 pause 反向
- [ ] 非 loopback bind + 默认 → token 生成 + console echo;`POST /api/.../btw`
  无 token → 401;带 `Authorization: Bearer ccteam:<token>` → 200/303
- [ ] loopback bind 默认 → 无 token,无 Authorization 也通
- [ ] `--no-auth` flag + 非 loopback → stderr warn "WARNING: no-auth on
  non-loopback bind = LAN-wide RCE on bypassPermissions sessions",但仍服务
- [ ] constant-time 比对 token(单元测试 mock 不同长度 token 不泄漏耗时)
- [ ] `~/.ccteam/web-token` 文件 mode 0600(`stat -c %a` = 600)
- [ ] CSRF e2e:模拟跨域 form POST 无 Authorization → 401(浏览器 fetch CORS
  preflight 也拒)
- [ ] `cargo test --workspace` 不退步;新增 ≥ 12 测试

---

## 7. M5.4 — E2E + retro + ship gate

### 7.1 问题

V0.3 ship 前需要端到端验证 + retro 文档 + workspace.version bump。

### 7.2 设计

#### 7.2.1 E2E 测试

新建 `crates/ccteam-web/tests/e2e_test.rs`:

- spin up `ccteam-web` server in test(rand port,`127.0.0.1:0`)
- fixture project at temp `~/.ccteam/`(用 `CCTEAM_HOME` env)
- reqwest 跑端到端:`GET /` 200 → `GET /project/<slug>` 200 → SSE stream 收 1
  event → `POST /api/<slug>/btw` 200 → 等 1s 看 inbox 文件落
- 不依赖真 tmux session;mock daemon 用 fixture

#### 7.2.2 Retro 文档

`docs/v0-3/e2e-retro.md` —— 模仿 V0.2.2 retro 模板:

- 4-suite 跑 dev / research smoke 跨 multi-session 项目
- M5.0-M5.3 撞坑回顾 + dust patches(若有)
- 截图 / SSE / write action 跨浏览器(Chrome / Firefox / Safari)兼容性 spot-check

#### 7.2.3 workspace.version bump

`Cargo.toml::workspace.package.version` `"0.2.2"` → `"0.3.0"`。

#### 7.2.4 CLAUDE.md baseline 回填

`CLAUDE.md` §一 表格更新:

| 项 | V0.3 后 |
|---|---|
| Workspace version | `0.3.0` |
| 测试 baseline | 实测后填 |
| 已 ship 里程碑 | 加 V0.3 行 |

#### 7.2.5 docs 同步

- `docs/v0-2/README.md`:已在 doc-only kickoff PR 加 V0.3 起始 pointer;ship
  时不再改
- `docs/dev-coupling-audit.md`:F45(M5.0 write helper promote)close 标记
- `docs/tech-design.md` §3.8 用户接口层 + §6.4 channel layer:加 web UI 段
- `docs/interfaces.md`:加 §13 / §14 web UI route + SSE + token schema

### 7.3 不做(M5.4)

- 性能 benchmark(V0.4)
- 跨平台 binary release(已经 `cargo install` ship,无新载体)
- 翻译 / i18n(永久 deferred;ccteam 中英混用文档标准)

### 7.4 验收

- [ ] `cargo test --workspace` 全绿,baseline 大于等于 631 + V0.3 累计新增
- [ ] e2e_test.rs spin-up + reqwest 端到端 happy path 通过
- [ ] `docs/v0-3/e2e-retro.md` 落档
- [ ] `Cargo.toml::workspace.package.version = "0.3.0"`
- [ ] `CLAUDE.md` §一 baseline 表格更新
- [ ] clippy 不新增 warning(4 pre-existing 不算)
- [ ] `docs/v0-2/README.md` V0.3 pointer 改为 "已 ship"

---

## 8. 技术决策汇总

### 8.1 Crate 组织

- 新 crate `crates/ccteam-web`,workspace member,sibling 于
  ccteam-{cli,core,hooks}
- 依赖 `ccteam-core` only(**不依赖 ccteam-cli**)
- 入口 `pub async fn serve(opts: ServeOpts)`;`ccteam-cli::commands::run_web`
  通过 `ccteam web` 子命令调用
- write helper(`send_to_session` / `inject_decision` / `pause` / `resume`)
  从 `ccteam-cli::mcp_serve.rs` 私有 fn 提到 `ccteam-core::actions` 模块;
  mcp_serve 内 wrapper 留 JSON encoding/decoding,核心 logic 共享

### 8.2 文件 watching

- 单个 `notify` recursive watcher 监 `~/.ccteam/progress/`(目录,非 per-file)
- `tokio::sync::broadcast` channel 多 SSE consumer fan-out
- 文件名 → slug 解析(`<slug>.jsonl` 约定)
- 新文件出现自动覆盖,不需 re-arm

### 8.3 SSE 拓扑

- 两个端点 `/sse/all` + `/sse/project/<slug>`,不在一条连接 multiplex
- wire format:`event: progress` + `data: <one-line-JSON>`(progress.jsonl 直发,
  server 注入 `slug` 字段)
- `: keepalive` 注释行 15s 周期防代理超时

### 8.4 截图

- `GET /screenshot/<slug>.png` on-demand,**不 polling**
- 调 `ccteam_core::render_screenshot`(F38 ship);Cache-Control no-cache
- htmx button 手动刷 + 页面加载一次 `<img src>` 自动渲染
- F38 graceful degrade 由 handler 翻译 504 + plain-text reason

### 8.5 写动作 UI

- 4 endpoint:`/btw` / `/inject_decision` / `/pause` / `/resume`,全 POST
- form 显式分开(`/btw` 自由文本 textarea;`/inject_decision` 结构化 select +
  textarea),不 conflate
- htmx `hx-post` + `hx-target="#flash"` swap 反馈 banner

### 8.6 鉴权

- 默认 token 鉴权 + bind heuristic(loopback → 不需 token;非 loopback →
  自动生成 + console echo)
- token 文件 `~/.ccteam/web-token` mode 0600,32-byte hex
- `Authorization: Bearer ccteam:<token>` header(constant-time 比对,
  `subtle` crate)
- CSRF 防御 = `Authorization` header 本身(浏览器跨域 form-submit 不会自动加)
- `--no-auth` flag 显式 opt-out + stderr 警告

### 8.7 前端

- `htmx` ~14 KB vendored,无 JS framework
- `askama` HTML templates,编译期类型检查
- minimal CSS ~3 KB,无 framework
- 编译期 `include_bytes!` 把 htmx + CSS 打进 binary,`ccteam-web` 自包含,无
  separate static-files install step(模仿 F38 vendored TTF 模式)

---

## 9. 已知风险 / 威胁模型

### 9.1 LAN-wide RCE — V0.3 主要风险

ccteam 项目 session 跑 `--dangerously-skip-permissions` claude(tech-design
§6.1)。写动作(`/btw` / `inject_decision`)是 unsanitized prompt-injection
向量:任何能 POST 到 `/api/<slug>/btw` 的客户端能让 claude session 执行任意
shell 命令(因为 bypassPermissions),包括但不限于:

- `rm -rf ~`
- exfiltrate `~/.ssh/`
- crypto miner
- 横向移动到同 LAN 其他 ccteam 实例

组合「非 loopback bind(0.0.0.0)+ 无鉴权 + 写动作」= **LAN-wide remote code
execution on user's dev machine**。

**与 V0.2.2 F44 silent-shadow 对比**:F44 是命名碰撞,后果是用户跑 GIS 工具
跑到 ccteam binary,confusion 重于损失;V0.3 无鉴权 0.0.0.0 部署是真正的 RCE,
**威胁模型材料级别更高**。

#### 9.1.1 缓解 — token 鉴权默认开

PRD §6.2.4 设计:

- loopback bind(127.0.0.1 / ::1)→ 默认无 token(信任 loopback)
- 非 loopback bind(0.0.0.0 等)→ **默认 token 鉴权**,首次启动自动生成 32-byte
  hex token,console echo,文件 mode 0600
- `--no-auth` 显式 opt-out + stderr 大字警告

#### 9.1.2 用户决策点

用户原话「暂不考虑安全问题」+ 选 0.0.0.0 + 写动作。本 PRD 推翻这个默认,
**默认开 token 鉴权**。用户 review 本 PRD 时可:

- (A)接受默认(token 鉴权)— 本 PRD 不变
- (B)推翻默认(`--no-auth` 改成默认行为)— 需要在本节加用户显式签字
  「acknowledge LAN-wide RCE risk」

#### 9.1.3 deferred 安全增强(V0.4)

- HTTPS / TLS(假设反代;V0.3 纯 HTTP)
- OAuth(Google / GitHub)
- per-project ACL
- 隧道集成(ngrok / Cloudflare Tunnel)
- 多用户 / 团队权限模型

### 9.2 其他风险

- **新 crate 编译时长**:axum + askama + tower 加 ~30s cold build。可接受,
  不阻塞;`cargo check` 已分钟级。
- **askama 模板编译错误**:模板失败编译期报错,不会 runtime 翻车;反而比
  template engine + fallback string 更稳。
- **notify 跨平台**:Linux inotify / macOS FSEvents 已 spec'd;不开 Windows
  支持(ccteam tmux 假设 Unix)。
- **broadcast 满 / receiver lag**:`bound = 1024`,lag 客户端 SSE reconnect
  续上 — 不为 backlog 占内存。
- **F38 不可用 / 崩溃**:V0.2.2 已 ship 了 graceful degrade,handler 包成
  504 + plain-text 显示给用户。
- **web UI bug 让 dashboard 挂掉**:不影响 CLI / MCP / tmux;CLI / MCP / 文件
  系统三层都还在,web UI 是第四层,可降级到前三层。

---

## 10. 不在范围 / V0.4 deferred

- **认证升级**:OAuth / per-project ACL / multi-user
- **HTTPS / TLS terminate**:V0.4(假设反代)
- **WebSocket**:SSE 足够;若未来双向写需求 ≥ N 才上
- **mobile-responsive layout**:V0.4 channel layer 评估
- **项目创建 UI**:永久 — 走 meta-agent + cct-project-creator skill
- **配置 / 用户偏好持久化**:V0.4
- **截图历史 archive view**:V0.4 channel layer
- **隧道集成**(ngrok / Cloudflare Tunnel):V0.4
- **远程协作 / 团队共享**:永久 — ccteam 单用户工具
- **多语言 UI(i18n)**:永久 deferred
- **性能 benchmark / 大量项目压测**(>20 project simul)— V0.4
- **写动作 rate-limit**:V0.4
- **session-based 鉴权(cookie 替代 bearer)**:V0.4 UX 评估

---

## 11. Workspace version bump

`Cargo.toml::workspace.package.version` `"0.2.2"` → `"0.3.0"` 在 M5.4 chore PR
落地。

V0.2.2 起立的政策:每 minor/patch release 必须 bump + commit subject
`vX.Y.Z:` 前缀。

---

## 12. PR sequencing

| # | milestone | branch | 工程量估 | 主要前置 |
|---|---|---|---|---|
| **PR #1** | M5.0 scaffold + write helper promote | `v0-3-scaffold` | ~400 LoC + ~10 测试,~3 天 | 无 |
| **PR #2** | M5.1 read-only dashboard | `v0-3-dashboard` | ~500 LoC + 模板 + ~10 测试,~3 天 | PR #1 |
| **PR #3** | M5.2 SSE + 截图 | `v0-3-sse-screenshot` | ~600 LoC + ~12 测试,~4 天 | PR #1(软依赖 PR #2 模板)|
| **PR #4** | M5.3 写动作 + 鉴权 | `v0-3-write-actions` | ~600 LoC + ~15 测试,~4 天 | PR #1(写 helper);PR #2(模板;forms 加在 project.html)|
| **PR #5** | M5.4 e2e + retro + ship gate | `v0-3-ship-gate` | ~150 LoC + 文档,~2 天 | 全部 PR merge |

**总计**:~2.25 kLOC + ~50 测试,~16 天(单人 5 天/周即 3 周)。

依赖图:

```
PR #1 (M5.0 scaffold) — 必先 merge,立 deps + dep 图
   ↓
PR #2 (M5.1 dashboard) — read-only,可独立 ship
   ↓ (软依赖,PR #3 / #4 模板加在 #2 基础上)
PR #3 (M5.2 SSE + screenshot) ─┐
                                ├──► PR #5 (M5.4 ship gate)
PR #4 (M5.3 write + auth) ──────┘
```

并行机会:**PR #3 / #4 可并行**(SSE 改 sse routes,写动作改 actions routes,
冲突点只在 project.html 模板)。

worktree 用法(详 dev-plan §7 briefing 模板):

```
git worktree add -b v0-3-<topic> /tmp/ccteam-v03-<topic> origin/main
```

跟 V0.2.2 一致,subagent 派工 briefing 含 PRD 章节 + 验收条目。

---

## Changelog

- 2026-05-10:初稿。基于 dispatcher Telegram 对话用户决策(read+write / SSE +
  PNG / 0.0.0.0 / Rust embedded);advisor 审查通过;backing API 现状 audit
  完毕(读端 helper public,写端需 promote 到 ccteam-core::actions);威胁模型
  默认开 token 鉴权,用户原偏好「暂不考虑安全」改为 `--no-auth` 显式 opt-out。
  base = `origin/main` `2988de6`(V0.2.2 ship 终点)。
