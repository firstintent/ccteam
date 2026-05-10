# V0.3 开发计划

> V0.3 主线版本实施计划。按依赖顺序拆 5 个 PR(每 PR 对一 milestone M5.x)+
> 一 chore ship gate。worktree-per-PR;每 PR 一份 subagent briefing 让派工拿到
> 模板 + 进 worktree 即可开干。
>
> 配套文档:
> - 需求决策:`docs/v0-3/prd.md`(12 节,~880 行)
> - 文档索引:`docs/v0-3/README.md`
>
> base = `origin/main` `2988de6`(V0.2.2 F44 ship);测试 baseline = **631/0**。
>
> 跟 V0.2.2 dev-plan 不同点:V0.3 是**主线版本**,milestone 而非 finding 编号
> (M5.0-M5.4);单一 feature(web UI)分阶段 ship,M5.1 / M5.2 各自独立可
> ship — 用户可在 dashboard / SSE 落地后即 dogfood,不必等 M5.3 写动作 + 鉴权
> 一并出。

---

## 1. PR 总览

| # | milestone | branch | 工程量估 | 主要前置依赖 |
|---|---|---|---|---|
| **PR #1** | M5.0 scaffold + write helper promote | `v0-3-scaffold` | ~400 LoC + ~10 测试,~3 天 | 无(立 dep 图) |
| **PR #2** | M5.1 read-only dashboard | `v0-3-dashboard` | ~500 LoC + 模板 + ~10 测试,~3 天 | **PR #1**(消费 ccteam-web crate scaffold)|
| **PR #3** | M5.2 SSE + 按需截图 | `v0-3-sse-screenshot` | ~600 LoC + ~12 测试,~4 天 | PR #1;软依赖 PR #2(模板) |
| **PR #4** | M5.3 写动作 + 鉴权 | `v0-3-write-actions` | ~600 LoC + ~15 测试,~4 天 | PR #1(actions helper);PR #2(project.html forms)|
| **PR #5** | M5.4 e2e + retro + workspace.version | `v0-3-ship-gate` | ~150 LoC + 文档,~2 天 | PR #1-#4 全部 merge,V0.3 ship gate |

**总计**:~2.25 kLOC + ~55 测试,~16 天(单人 5 天/周即 3 周;PR #3 / #4
并行可压到 ~12 天)。

### 依赖图

```
PR #1 (M5.0 scaffold + actions promote) ── 必先 merge
   ↓
PR #2 (M5.1 dashboard) ── 独立 ship,后续 PR 在其模板上加
   ↓ (软依赖,模板上 SSE / form 增量)
PR #3 (M5.2 SSE + screenshot) ───┐
                                  ├──► PR #5 (M5.4 ship gate)
PR #4 (M5.3 write + token auth) ─┘
```

并行机会:**PR #3 / #4 同时起 2 个 worktree**(SSE routes vs actions routes
冲突面只在 project.html 模板);PR #1 / #2 是 critical path 串行。

---

## 2. PR #1 — M5.0 scaffold + write helper promote

> **目标**:新 crate `crates/ccteam-web` 立起 + axum/askama/notify 依赖 pin +
> `ccteam web` 子命令 + 写动作 helper 从 `ccteam-cli::mcp_serve` 提到
> `ccteam-core::actions`。机械 + 解耦,先 merge 立基础。

**关联 PRD**:§3(M5.0 全文)+ §8.1(crate 组织)+ §11(workspace.version V0.3
不 bump,留 PR #5 落)

### 任务

- [ ] **#1.1** 新 crate `crates/ccteam-web/`
  - `Cargo.toml`:depends `ccteam-core` + axum 0.8 + askama 0.12 + tokio
    workspace + notify workspace + serde / serde_json workspace + tracing
    workspace
  - `src/lib.rs`:`pub async fn serve(opts: ServeOpts)` 占位,bind +
    health endpoint 启动
  - `src/routes/{mod,health}.rs`:`GET /health` 返 200 JSON
  - 单元测试:`tokio::test` 起 server + reqwest hit `/health`
- [ ] **#1.2** workspace `Cargo.toml` 把 `ccteam-web` 加入 `members`,把 axum +
  askama pin minor 版本到 `[workspace.dependencies]`
- [ ] **#1.3** `ccteam web` CLI 子命令
  - `crates/ccteam-cli/src/main.rs`:`Commands::Web { bind, no_auth, token_file }`
  - `crates/ccteam-cli/src/commands.rs::run_web(opts)`:翻 clap struct → `ServeOpts`,
    `tokio::runtime` block on `ccteam_web::serve(opts)`
- [ ] **#1.4** 写动作 helper promote
  - 新 `crates/ccteam-core/src/actions.rs`,4 个 pub fn:
    `send_to_session(paths, slug, text)`、
    `inject_decision(paths, slug, DecisionInput)`、
    `pause(paths, slug)`、`resume(paths, slug)`
  - 现有 `ccteam-cli/src/mcp_serve.rs::tool_send_to_session`(line 456)+
    `tool_inject_decision`(line 500)内部 logic 提到 `actions::*`;mcp_serve
    fn 留 wrapper 拆 args + JSON encode
  - pause / resume 在 mcp_serve 内 inline 的 logic 同步抽
  - **dep graph 守恒**:验证 `ccteam-web` 不依赖 `ccteam-cli`(`cargo tree`
    检查)
- [ ] **#1.5** lib.rs re-export
  - `crates/ccteam-core/src/lib.rs`:`pub use actions::{send_to_session,
    inject_decision, pause, resume, DecisionInput};`
- [ ] **#1.6** doctor 集成(可选,本 PR 不强求)
  - `commands.rs::run_doctor` 加 web 检查段:检测 `~/.ccteam/web-token`
    存在 + mode 0600 → warn / OK 输出
- [ ] **#1.7** 测试
  - `actions.rs` 单元:`send_to_session` 写 inbox 文件 + 拼 InboxMessage 字段
    verify
  - `mcp_serve.rs` 现有 13 个测试不动(回归保证 wrapper 透传)
  - `ccteam-web` 端 health 200 e2e
  - `ccteam-cli/tests/web_subcommand_test.rs`:`ccteam web --bind 127.0.0.1:0`
    spawn,发 `/health` 200,SIGTERM 干净退出

### 验收(摘 PRD §3.4)

- [ ] `crates/ccteam-web/` workspace member,Cargo.toml 列依赖 + pin minor
- [ ] `cargo build --workspace` 全绿
- [ ] `crates/ccteam-core/src/actions.rs` 4 个 pub fn 落地 + 单元测试
- [ ] `mcp_serve.rs` 内 `tool_*` delegate 到 `ccteam_core::actions::*`;MCP
  测试套件全绿
- [ ] `ccteam web --bind 127.0.0.1:7331` 启动,`curl http://127.0.0.1:7331/health`
  返 200 JSON
- [ ] `cargo tree -p ccteam-web` 不出现 `ccteam-cli`
- [ ] `cargo test --workspace` ≥ 631 baseline + ≥ 4 new

### 文档同步

- `docs/interfaces.md` §10.6 维护命令加 `ccteam web` 行(基本 schema:bind /
  no-auth flag)
- `docs/dev-coupling-audit.md` F45 加(详 §10):"V0.3 M5.0 promote send_to_session
  / inject_decision / pause / resume from ccteam-cli::mcp_serve(private fn)
  to ccteam-core::actions(public)"
- `docs/tech-design.md` §6 扩展点表加 web layer 行(简短 placeholder,M5.4
  补全)

---

## 3. PR #2 — M5.1 read-only dashboard

> **目标**:`/` dashboard 项目列表 + `/project/<slug>` 详情页(state + recent
> events + outbox);askama HTML templates;htmx + 自包含 CSS / JS;无实时,
> 静态渲染。**独立可 ship**。

**关联 PRD**:§4(M5.1 全文)+ §8.7(前端 stack)

**前置**:**PR #1 必须先 merge**(消费 ccteam-web crate scaffold)。

### 任务

- [ ] **#2.1** routes
  - `routes/dashboard.rs::handle_index`:`GET /` 调
    `ccteam_cli::commands::collect_projects(paths)` → askama render
  - `routes/project.rs::handle_project`:`GET /project/<slug>`:调
    `collect_recent_events(paths, slug, 200)` + `ProjectState::load(slug)` +
    扫 outbox → askama render
  - `routes/assets.rs::handle_asset`:`GET /assets/{file}` static 路由,
    `include_bytes!` 编译期载入 htmx + CSS
- [ ] **#2.2** 模板
  - `templates/base.html`:HTML5 boilerplate + `<script src="/assets/htmx.min.js">`
    + `<link rel="stylesheet" href="/assets/style.css">` + nav
  - `templates/dashboard.html`:extends base,table 列(slug / team / phase /
    last event ts / status badge / cost)
  - `templates/project.html`:extends base,sections(state JSON / recent
    events / outbox messages / screenshot panel placeholder)
- [ ] **#2.3** Status badge 计算
  - 复用 V0.2.2 F35 silence_classifier **只读**分类;`Healthy` / `Terminal` /
    `SubagentBusy` / `MidToolHung` / `PostStopLimbo` 之一 → 渲染颜色 badge
  - 不在 web 层 trigger 任何 escalate(只读,即使分类是 Limbo)
- [ ] **#2.4** Outbox parsing
  - 复用 `ccteam_core::SessionMailbox` 已 public API 扫
    `~/projects/<slug>/.ccteam/outbox/`
  - 文件名 sort newest first,展示前 20 条;每条 frontmatter `kind` /
    `created_at` + body 头 200 char 摘要
- [ ] **#2.5** 静态资源
  - `crates/ccteam-web/assets/htmx.min.js`(vendored ~14 KB,从
    `https://unpkg.com/htmx.org@2.x` snapshot)
  - `crates/ccteam-web/assets/style.css`(~3 KB,monospace + dark-mode-friendly)
  - `include_bytes!` 编译期打包(模仿 F38 vendored TTF 模式)
- [ ] **#2.6** 测试
  - askama 模板编译期类型检查 — failing template 即编译 fail
  - `tests/dashboard_test.rs`:reqwest 起 server + `GET /` 200 + HTML body
    contains slug name
  - `tests/project_test.rs`:fixture state.json + progress.jsonl,`GET
    /project/<slug>` 200 + body contains "current_phase" + outbox markers
  - `GET /project/nonexistent` → 404
  - `GET /assets/htmx.min.js` 200 + Content-Type `application/javascript`

### 验收(摘 PRD §4.4)

- [ ] `GET /` 返 200 + valid HTML + 列出 `~/.ccteam/projects/` 全 slug
- [ ] `GET /project/<slug>` 返 200 + state + events + outbox 段
- [ ] 不存在 slug → 404
- [ ] askama template 编译期类型检查
- [ ] 新增 ≥ 6 e2e 测试(reqwest hit + HTML assertion)
- [ ] **独立可 ship** — M5.1 merge 后用户可 dogfood dashboard,M5.2 / M5.3 不 block

### 文档同步

- `docs/interfaces.md` §13(新章节):web routes 表(M5.1 阶段 read-only)
- `docs/tech-design.md` §3.8 加 dashboard 描述

---

## 4. PR #3 — M5.2 SSE event push + 按需截图

> **目标**:`notify` 文件 watcher 监 `~/.ccteam/progress/` 全目录 → broadcast
> channel → 两个 SSE 端点(`/sse/all` / `/sse/project/<slug>`);F38
> `render_screenshot` 在 `/screenshot/<slug>.png` 端点同步调用,Cache-Control
> no-cache + htmx button 手刷。**独立可 ship**(read-only + live)。

**关联 PRD**:§5(M5.2 全文)+ §8.2-§8.4(技术决策)

**前置**:PR #1(crate scaffold);软依赖 PR #2(模板加 SSE swap markup)。

### 任务

- [ ] **#3.1** Watcher 后台 task
  - `src/watcher.rs::{EventBus, EventMsg, spawn_watcher}`
  - `notify::recommended_watcher` + `RecursiveMode::Recursive` 监
    `<paths.progress_dir()>` + `<paths.state_dir()>`
  - `Event::Modify(Path)` callback → 计算文件 last-read offset → tail 新增 lines
    → parse slug from filename → broadcast `EventMsg { slug, event_json }`
  - `tokio::sync::broadcast::channel(1024)`;laggy receiver drop on overflow
  - file offset 维护用 `HashMap<PathBuf, u64>`(单 Mutex)
- [ ] **#3.2** SSE endpoints
  - `routes/sse.rs::handle_sse_all`:`GET /sse/all` → axum SSE response,subscribe
    `EventBus`,每条 msg 转 `Event::default().event("progress").data(json)`
  - `routes/sse.rs::handle_sse_project`:`GET /sse/project/<slug>` 同上,
    server-side filter `msg.slug == <slug>`
  - keepalive:`tokio::time::interval(Duration::from_secs(15))` 发 `: keepalive\n\n`
- [ ] **#3.3** 模板加 SSE markup(rebase 到 PR #2 后)
  - `dashboard.html`:`<table id="events" hx-ext="sse" sse-connect="/sse/all"
    sse-swap="progress">`
  - `project.html`:`<table id="events" hx-ext="sse"
    sse-connect="/sse/project/{{slug}}" sse-swap="progress">`
- [ ] **#3.4** 截图 endpoint
  - `routes/screenshot.rs::handle_screenshot`:`GET /screenshot/<slug>.png` →
    `ccteam_core::render_screenshot(slug, default opts)` → 返 PNG bytes,
    Content-Type `image/png`,Cache-Control `no-cache, must-revalidate`
  - F38 失败(Ok(None) / Err)→ 504 Gateway Timeout + plain-text reason
- [ ] **#3.5** 模板加截图 panel(rebase 到 PR #2 后)
  - `project.html`:
    ```html
    <button hx-get="/screenshot/{{slug}}.png" hx-target="#screenshot-img"
            hx-swap="outerHTML">Refresh</button>
    <img id="screenshot-img" src="/screenshot/{{slug}}.png" alt="pane">
    ```
- [ ] **#3.6** 测试
  - `tests/watcher_test.rs`:tempdir + manual append progress.jsonl line + 等
    `EventBus` 收到 + 字段 verify
  - `tests/sse_test.rs`:reqwest stream `/sse/all`,另一线程 append progress.jsonl,
    断 ≥ 1 event 收到
  - per-slug filter:`/sse/project/<slug>` 只收对应 slug
  - keepalive:监连接 16s 看到 `: keepalive` line
  - `tests/screenshot_test.rs`:fixture project + tmux mock(失败优先,因为
    CI 无 tmux)→ 504 + reason;若有 tmux,200 + PNG magic byte verify

### 验收(摘 PRD §5.4)

- [ ] watcher 单 instance dispatch 多 receiver 不 starve
- [ ] `GET /sse/all` 实时推送,append progress.jsonl 后 1s 内浏览器收到
- [ ] `GET /sse/project/<slug>` 只收对应 slug 事件
- [ ] keepalive 注释行 15s 周期发出
- [ ] `GET /screenshot/<slug>.png` 200 PNG / 504 plain-text
- [ ] 新增 ≥ 8 测试
- [ ] **独立可 ship**

### 文档同步

- `docs/interfaces.md` §13(扩):SSE wire format(`event: progress` + `data:
  one-line-JSON` + slug 注入语义)
- `docs/interfaces.md` §13:`GET /screenshot/<slug>.png` schema
- `docs/tech-design.md` §6.4 channel layer:加 web SSE 描述

---

## 5. PR #4 — M5.3 写动作 + token 鉴权

> **目标**:四个 POST endpoint(`/api/<slug>/{btw,inject_decision,pause,resume}`)
> 调 `ccteam-core::actions::*`;auth middleware 默认开 token(非 loopback bind),
> `--no-auth` 显式 opt-out。CSRF 防御 = `Authorization` header。

**关联 PRD**:§6(M5.3 全文)+ §8.5-§8.6 + §9 威胁模型

**前置**:PR #1(actions helper public);PR #2(project.html forms 加在
template 内)。

### 任务

- [ ] **#4.1** auth middleware
  - `src/auth.rs::{AuthLayer, AuthState, validate_token}`
  - bind heuristic(`serve` 内决定 enabled):
    - loopback → enabled = false,不生成 token
    - 非 loopback + `!no_auth` → enabled = true,token 加载或生成
    - 非 loopback + `no_auth` → enabled = false,stderr warn
  - `subtle::ConstantTimeEq` 比对 token,4xx 不泄漏耗时
- [ ] **#4.2** token 文件管理
  - `~/.ccteam/web-token` 不存在 → `rand::random::<[u8; 32]>()` → hex →
    `OpenOptions::mode(0o600).create_new`
  - 已存在 → `metadata::permissions::mode()` 校验 0600,否则 stderr warn
  - console echo:
    ```
    ccteam web listening on http://0.0.0.0:7331
    Auth token (write actions): ccteam:<token>
    Reset with: rm ~/.ccteam/web-token && restart
    ```
- [ ] **#4.3** 写动作 routes
  - `routes/actions.rs`:四个 POST handler:
    - `POST /api/<slug>/btw`(form-encoded `text=...`)→ `actions::send_to_session`
    - `POST /api/<slug>/inject_decision`(form-encoded `path=...&body=...`)→
      `actions::inject_decision`
    - `POST /api/<slug>/pause` →`actions::pause`
    - `POST /api/<slug>/resume` → `actions::resume`
  - 成功 → 303 See Other 回 `/project/<slug>`(htmx 跟 swap)
  - 失败 → 4xx + plain-text error
  - axum extractor `Form<BtwForm>` validation `text.len() in 1..=4000`
- [ ] **#4.4** 模板加 forms(rebase 到 PR #2 后)
  - `project.html` 加 sidebar:
    - `/btw` 自由文本 textarea form
    - `inject_decision` select(decision candidates)+ textarea form
    - pause / resume button-form
  - `<div id="flash"></div>` 占位接 swap 反馈
- [ ] **#4.5** Decision candidates 扫
  - `routes/project.rs` 加 helper 扫 `~/projects/<slug>/.ccteam/decision-*.md` →
    pass `Vec<PathBuf>` 给模板
- [ ] **#4.6** stderr 警告(--no-auth + 非 loopback 时)
  - `serve` 启动 banner 加大字 `WARNING: --no-auth on non-loopback bind = LAN-wide
    RCE on bypassPermissions sessions. Press Ctrl-C within 5s to abort.`
  - 5s 倒计时(`tokio::time::sleep`)再正式 listen — 给用户最后机会
- [ ] **#4.7** 测试
  - `tests/auth_test.rs`:
    - loopback bind 默认 → 无 Authorization header 也通
    - 非 loopback bind 默认 → 无 header → 401;带 `Authorization: Bearer
      ccteam:<token>` → 200/303
    - `--no-auth` 非 loopback → stderr 含 "LAN-wide RCE";server 仍服务
    - constant-time:不同长度 token 比对耗时差 < 10us(`std::time::Instant`
      多 round 取 max,绝对值 sanity check)
  - `tests/actions_test.rs`:
    - `POST /api/<slug>/btw text=hello` → 303 + inbox 文件落地;`InboxMessage`
      字段含 `text=hello`
    - `POST /api/<slug>/inject_decision path=...&body=...` → 303 + decision 写入
    - `POST /api/<slug>/pause` → state.json `paused: true`
    - `POST /api/<slug>/resume` 反向
  - `tests/csrf_test.rs`:模拟跨域 form POST `Origin: https://evil.com` 无
    Authorization → 401(并断 axum 不上 logic;路径短 circuit)
  - `tests/web_token_file_test.rs`:首次启动生成 token + mode 0600;再次启动复用
  - `~/.ccteam/web-token` 删除后重启重新生成

### 验收(摘 PRD §6.4)

- [ ] 四个 POST endpoint 各自走 actions::* 调 + 写文件 verify
- [ ] 默认非 loopback bind 强 token;loopback 不强 token
- [ ] `--no-auth` 大字警告 + 5s 倒计时
- [ ] constant-time 比对(单元 + 文档化)
- [ ] mode 0600 token 文件
- [ ] 新增 ≥ 12 测试

### 文档同步

- `docs/interfaces.md` §13(扩):POST endpoints schema + Authorization header +
  token file 协议
- `docs/tech-design.md` §3.8:加 web channel 写动作描述
- `docs/dev-coupling-audit.md` F45 close 标记

---

## 6. PR #5 — M5.4 e2e + retro + ship gate

> **目标**:V0.3 ship gate。e2e 测试 + retro 文档 + workspace.version bump
> 0.2.2 → 0.3.0 + CLAUDE.md baseline + docs sweep。

**关联 PRD**:§7(M5.4 全文)+ §11(version bump)

**前置**:PR #1-#4 全部 merge。

### 任务

- [ ] **#5.1** e2e_test.rs
  - `crates/ccteam-web/tests/e2e_test.rs`:tempdir `CCTEAM_HOME`,fixture
    state.json + progress.jsonl + outbox,spin server `127.0.0.1:0`,reqwest
    跑 happy path:GET / → GET /project/<slug> → SSE 收 1 event → POST
    /api/<slug>/btw → 等 1s 看 inbox 文件落
- [ ] **#5.2** workspace.version bump
  - `Cargo.toml::workspace.package.version` `"0.2.2"` → `"0.3.0"`
  - commit subject `v0.3.0:` 前缀
- [ ] **#5.3** CLAUDE.md baseline 回填
  - §一表格:`Workspace version 0.3.0`;`测试 baseline <实测>`;V0.3 milestone
    行加
  - §一 onboarding 30s tip 不动
  - **本 PR 是 V0.3 ship 唯一改 CLAUDE.md 的 PR**;PR #1-#4 都不改 CLAUDE.md
- [ ] **#5.4** retro 文档
  - `docs/v0-3/e2e-retro.md`:模仿 V0.2.2 e2e-retro 模板,4-suite 跨 multi-session
    项目验证 dashboard / SSE / write action / 截图 / 鉴权
  - 跨浏览器 spot-check(Chrome / Firefox / Safari macOS)
- [ ] **#5.5** docs sweep
  - `docs/v0-2/README.md`:V0.3 起始 pointer 改为 "已 ship V0.3:web UI"
  - `docs/dev-coupling-audit.md` F45 close
  - `docs/tech-design.md` §3.8 / §6.4 web 段终稿
  - `docs/interfaces.md` §13 web routes 终稿
- [ ] **#5.6** 红线 grep 矩阵全跑过(详 §8 矩阵)
- [ ] **#5.7** 测试 + clippy
  - `cargo test --workspace` 全绿
  - `cargo clippy --workspace --no-deps` 不新增 warning(4 pre-existing 不算)

### 验收

- [ ] e2e_test.rs 端到端 happy path 通过
- [ ] `Cargo.toml::workspace.package.version = "0.3.0"`
- [ ] CLAUDE.md §一 baseline 表格更新
- [ ] `docs/v0-3/e2e-retro.md` 落档
- [ ] `cargo test --workspace` 全绿,baseline ≥ 631 + V0.3 累计新增
- [ ] clippy 不新增 warning

---

## 7. Worktree subagent briefing 模板

每 PR 起 worktree 时,主 session 用 Agent 工具派,briefing 套以下模板:

### 7.1 通用前置(每 PR briefing 都加)

```markdown
## 起始

```
git fetch origin
git worktree add -b <branch> /tmp/ccteam-<topic> origin/main
cd /tmp/ccteam-<topic>
cargo test --workspace 2>&1 | tail -3   # confirm 631 baseline
```

## 必读(全 PR 通用)

- `CLAUDE.md` §一(状态)+ §三 红线 + §五 PR 纪律
- `docs/v0-3/prd.md` §<N>(本 PR 对应 milestone 全文)
- `docs/v0-3/dev-plan.md` §<N>(本 PR 任务 / 验收)
- `docs/tech-design.md` §6.4 channel layer + §5.5 progress.jsonl SoT
- `docs/interfaces.md` §1 文件系统布局 + §4 progress.jsonl 事件流

## 全 PR 红线

- progress.jsonl 是 SoT,**不解析 tmux 终端输出**
- 永不主动 kill 长 session
- ccteam-web **不 depend on ccteam-cli**(`cargo tree -p ccteam-web` 验证)
- 测试不退步(baseline 631);clippy 不新增 warning
- 文档同步(对应 dev-plan 段)

## PR 命令(完整 HEREDOC,见下)
```

### 7.2 PR #1 briefing(M5.0 scaffold)

```markdown
## 任务

V0.3 PR #1 — 新 crate `crates/ccteam-web` 立起 + axum 0.8 / askama 0.12 /
notify(workspace pin)依赖 + `ccteam web` 子命令 + 写动作 helper 从
`ccteam-cli::mcp_serve` 提到 `ccteam-core::actions`(关键解耦)。

## 触点代码

- `Cargo.toml`(workspace members 加 `ccteam-web`;workspace.dependencies pin
  axum / askama)
- `crates/ccteam-web/`(新建)
- `crates/ccteam-core/src/actions.rs`(新模块)
- `crates/ccteam-core/src/lib.rs`(re-export)
- `crates/ccteam-cli/src/mcp_serve.rs::tool_send_to_session`(line 456)+
  `tool_inject_decision`(line 500)— 内部 logic 提到 actions
- `crates/ccteam-cli/src/main.rs`(`Commands::Web`)
- `crates/ccteam-cli/src/commands.rs`(`run_web`)

## 实施步骤(详 dev-plan §2)

1. 新 crate scaffold(#1.1)
2. workspace deps pin(#1.2)
3. ccteam web 子命令(#1.3)
4. actions promote(#1.4)
5. lib re-export(#1.5)
6. doctor(#1.6,可选)
7. 测试(#1.7)

## 红线 grep

```bash
# ccteam-web 不依赖 ccteam-cli
cargo tree -p ccteam-web | grep -E 'ccteam-cli'
# 期望:0 命中

# actions 是 ccteam-core 模块,被 mcp_serve + ccteam-web 共消费
git grep -nE 'use ccteam_core::actions' crates/
# 期望:命中 ≥ 2(mcp_serve + ccteam-web)

# mcp_serve wrapper logic 不重复 actions 内部
git grep -nE 'fn tool_(send_to_session|inject_decision)' crates/ccteam-cli/src/mcp_serve.rs
# 期望:命中 2(wrapper);wrapper 内只 args 拆 + JSON encode + 调 actions::*
```

## PR 命令

```bash
gh pr create --base main --head v0-3-scaffold --title "v0.3 PR #1: M5.0 scaffold + write helper promote" --body "$(cat <<'EOF'
## Closes
- M5.0(`docs/v0-3/prd.md` §3)
- F45 部分(write helper promote;`docs/dev-coupling-audit.md`)

## 改动
- 新 crate `crates/ccteam-web`(axum 0.8 / askama 0.12 / notify workspace)
- `ccteam web --bind <addr> --no-auth` CLI 子命令
- `GET /health` 200 JSON
- `crates/ccteam-core/src/actions.rs` 模块,4 pub fn(send_to_session /
  inject_decision / pause / resume)从 mcp_serve.rs 私有 fn promote
- mcp_serve.rs 内 tool_* delegate 到 actions::*

## 测试
- 新增 ~10(actions 单元 / health e2e / web subcommand spawn)
- mcp_serve 现有测试套件全绿(回归保证)

## 关联
- Closes V0.3 M5.0
- 痛点 7(`docs/requirements.md`)+ tech-design §6 扩展点

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```
```

### 7.3 PR #2 briefing(M5.1 dashboard)

```markdown
## 任务

V0.3 PR #2 — read-only dashboard:`/` 项目列表 + `/project/<slug>` 详情页;
askama 模板;htmx + minimal CSS vendored;outbox 段;screenshot panel
placeholder(M5.2 加)。

## 前置

PR #1 必须 merge(消费 ccteam-web crate scaffold)。

## 触点代码

- `crates/ccteam-web/src/routes/{dashboard,project,assets}.rs`(新建)
- `crates/ccteam-web/templates/{base,dashboard,project}.html`(新建)
- `crates/ccteam-web/assets/{htmx.min.js,style.css}`(vendored)
- `crates/ccteam-web/src/lib.rs`(router 注册)

## 实施步骤(详 dev-plan §3)

1. routes(#2.1)
2. 模板(#2.2)
3. status badge(#2.3)
4. outbox parsing(#2.4)
5. 静态资源(#2.5)
6. 测试(#2.6)

## 红线 grep

```bash
# 模板用 askama,编译期类型检查
git grep -nE 'use askama' crates/ccteam-web/src/
# 期望:命中 ≥ 1

# 不解析 tmux 终端输出
git grep -nE 'capture_pane|tmux capture' crates/ccteam-web/src/
# 期望:0 命中(M5.2 截图通过 ccteam_core::render_screenshot,M5.1 不接)

# 状态 badge 用 silence_classifier 的 read-only 分类,不 trigger escalate
git grep -nE 'silence_classifier' crates/ccteam-web/src/
# 期望:命中只在 read-only path,不调 re-inject / escalate fn
```

## PR 命令

```bash
gh pr create --base main --head v0-3-dashboard --title "v0.3 PR #2: M5.1 read-only dashboard" --body "$(cat <<'EOF'
## Closes
- M5.1(`docs/v0-3/prd.md` §4)

## 改动
- `GET /` dashboard:项目列表表格(slug / team / phase / last event / status
  badge / cost)
- `GET /project/<slug>` 详情:state JSON / recent events 200 条 / outbox 20 条
- `GET /assets/{htmx.min.js,style.css}` static handler
- askama 模板:base / dashboard / project
- 静态 + 自包含,无 npm / 无 build toolchain

## 测试
- 新增 ~10(dashboard 200 + slug list / project state body / 404 / assets)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```
```

### 7.4 PR #3 briefing(M5.2 SSE + screenshot)

```markdown
## 任务

V0.3 PR #3 — `notify` 文件 watcher 监 `~/.ccteam/progress/` 全目录 →
`tokio::sync::broadcast` channel → 两个 SSE endpoint(/sse/all 全局,
/sse/project/<slug> per-slug filter);F38 `render_screenshot` 在
`/screenshot/<slug>.png` 同步调用,Cache-Control no-cache + htmx 手动刷 button。

## 前置

PR #1(crate scaffold);软依赖 PR #2(模板加 SSE swap markup)。

## 触点代码

- `crates/ccteam-web/src/watcher.rs`(新)
- `crates/ccteam-web/src/routes/{sse,screenshot}.rs`(新)
- `crates/ccteam-web/templates/{dashboard,project}.html`(rebase #2 后,加
  SSE markup + screenshot panel)
- `crates/ccteam-core/src/screenshot.rs`(F38 已 ship,只 reuse)

## 实施步骤(详 dev-plan §4)

1. watcher(#3.1)
2. SSE endpoints(#3.2)
3. 模板 SSE markup(#3.3)
4. screenshot endpoint(#3.4)
5. 模板 screenshot panel(#3.5)
6. 测试(#3.6)

## 红线 grep

```bash
# watcher 单 instance,不每文件起独立
git grep -nE 'watch\(' crates/ccteam-web/src/watcher.rs
# 期望:命中 1-2(progress dir + state dir)

# broadcast bound 防 OOM
git grep -nE 'broadcast::channel' crates/ccteam-web/src/
# 期望:命中含 bound 字面 1024

# 不 polling 截图
git grep -nE 'interval\(.*screenshot\|render_screenshot.*loop' crates/ccteam-web/src/
# 期望:0 命中(只在 GET handler 同步调)

# screenshot 失败不 panic
git grep -nE 'unwrap\(\)|expect\(' crates/ccteam-web/src/routes/screenshot.rs
# 期望:0 命中(全 ? + match)
```

## PR 命令

```bash
gh pr create --base main --head v0-3-sse-screenshot --title "v0.3 PR #3: M5.2 SSE + on-demand screenshot" --body "$(cat <<'EOF'
## Closes
- M5.2(`docs/v0-3/prd.md` §5)

## 改动
- `GET /sse/all` 全局 SSE 流;`GET /sse/project/<slug>` per-slug filter
- 单 notify recursive watcher 监 ~/.ccteam/progress/ + state/
- broadcast(1024)channel,laggy receiver drop
- keepalive 注释行 15s 周期
- `GET /screenshot/<slug>.png` on-demand,F38 reuse,Cache-Control no-cache
- 模板 SSE swap markup + screenshot panel button

## 测试
- 新增 ~12(watcher dispatch / per-slug filter / SSE wire / keepalive /
  screenshot 200 + 504)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```
```

### 7.5 PR #4 briefing(M5.3 write actions + auth)

```markdown
## 任务

V0.3 PR #4 — 四个 POST 写动作 endpoint(/api/<slug>/{btw,inject_decision,
pause,resume})调 ccteam-core::actions;auth middleware 默认 token 鉴权(非
loopback bind);--no-auth 显式 opt-out + 大字 stderr 警告 + 5s 倒计时;CSRF
防御 = Authorization header 本身。

## 前置

PR #1(actions helper public);PR #2(project.html 模板,加 form sidebar)。

## 触点代码

- `crates/ccteam-web/src/auth.rs`(新)
- `crates/ccteam-web/src/routes/actions.rs`(新)
- `crates/ccteam-web/templates/project.html`(rebase #2 后加 forms)
- `crates/ccteam-web/Cargo.toml`(加 subtle = "2")
- `~/.ccteam/web-token` 文件协议

## 实施步骤(详 dev-plan §5)

1. auth middleware(#4.1)
2. token 文件管理(#4.2)
3. actions routes(#4.3)
4. 模板 forms(#4.4)
5. decision candidates 扫(#4.5)
6. stderr 警告 + 倒计时(#4.6)
7. 测试(#4.7)

## 红线 grep

```bash
# constant-time token 比对
git grep -nE 'subtle::|ConstantTimeEq' crates/ccteam-web/src/auth.rs
# 期望:命中 ≥ 1

# token 文件 mode 0600
git grep -nE '0o600|mode\(0o6' crates/ccteam-web/src/auth.rs
# 期望:命中 ≥ 1

# 写 endpoint 全经 actions module,不重复 mcp_serve fn
git grep -nE 'send_to_session|inject_decision' crates/ccteam-web/src/routes/actions.rs
# 期望:全部 `ccteam_core::actions::*` 调用,无 inline 写 inbox 文件 logic

# 大字警告 + 倒计时
git grep -nE 'LAN-wide RCE|bypassPermissions' crates/ccteam-web/src/
# 期望:命中 ≥ 1(stderr 警告 fmt)
```

## PR 命令

```bash
gh pr create --base main --head v0-3-write-actions --title "v0.3 PR #4: M5.3 write actions + token auth" --body "$(cat <<'EOF'
## Closes
- M5.3(`docs/v0-3/prd.md` §6)
- F45 close(`docs/dev-coupling-audit.md`,搭 PR #1 actions promote)

## 改动
- POST /api/<slug>/{btw,inject_decision,pause,resume} 调 ccteam-core::actions
- auth.rs middleware:bind heuristic(loopback 免 token / 非 loopback 默认开
  token),--no-auth 显式 opt-out
- ~/.ccteam/web-token mode 0600 32-byte hex,首启自动生成 + console echo
- subtle::ConstantTimeEq 比对 token
- --no-auth + 非 loopback 启动时 stderr 大字警告 + 5s 倒计时
- project.html 加 forms sidebar(自由文本 textarea / 结构化 inject_decision /
  pause+resume button)
- CSRF 防御 = Authorization header 本身(浏览器跨域 form-submit 不自动加)

## 测试
- 新增 ~15(auth middleware 4 路径 / token 文件 / actions 4 endpoint / CSRF /
  constant-time)

## 安全说明
本 PR 引入写动作面;详 PRD §9 威胁模型。默认开 token 鉴权,用户可
--no-auth opt-out(stderr 大字警告 + 5s 倒计时)。

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```
```

### 7.6 PR #5 briefing(M5.4 ship gate)

```markdown
## 任务

V0.3 PR #5 — ship gate。e2e_test.rs 端到端 / retro 文档 / workspace.version
0.2.2 → 0.3.0 / CLAUDE.md baseline / docs sweep。

## 前置

PR #1-#4 全部 merge。

## 触点代码

- `crates/ccteam-web/tests/e2e_test.rs`(新)
- `Cargo.toml`(workspace.package.version)
- `CLAUDE.md` §一 baseline 表格
- `docs/v0-3/e2e-retro.md`(新)
- `docs/v0-2/README.md`(V0.3 pointer 改 ship)
- `docs/dev-coupling-audit.md`(F45 close)
- `docs/tech-design.md` §3.8 / §6.4
- `docs/interfaces.md` §13

## 实施步骤(详 dev-plan §6)

1. e2e_test.rs(#5.1)
2. workspace.version(#5.2)
3. CLAUDE.md baseline(#5.3)
4. retro(#5.4)
5. docs sweep(#5.5)
6. 红线 grep 矩阵(#5.6)
7. 测试 + clippy(#5.7)

## 红线 grep 矩阵(详 §8)

详 dev-plan §8 全 PR 跨维度 grep 矩阵。本 PR 是最后 gate,**全部跑一遍**。

## PR 命令

```bash
gh pr create --base main --head v0-3-ship-gate --title "v0.3.0: workspace.version bump + V0.3 ship gate" --body "$(cat <<'EOF'
## Closes
- V0.3 ship gate
- M5.4(`docs/v0-3/prd.md` §7)
- `docs/dev-coupling-audit.md` F45 close

## 改动
- `Cargo.toml::workspace.package.version` "0.2.2" → "0.3.0"
- CLAUDE.md §一 baseline 回填(测试数 / V0.3 milestone 行)
- docs/v0-3/e2e-retro.md 落档
- docs sweep:v0-2/README V0.3 pointer / dev-coupling-audit F45 close /
  tech-design §3.8 + §6.4 / interfaces §13

## 测试
- e2e_test.rs 端到端 happy path
- baseline 大于等于 631 + V0.3 累计新增,全绿
- clippy 不新增 warning

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```
```

---

## 8. 红线 grep 矩阵

每 PR commit 前必查;PR #5 ship gate 全跑一遍。

| 红线维度 | grep | 期望 | 跨 PR |
|---|---|---|---|
| **progress.jsonl 是 SoT** | `git grep -nE 'capture_pane\|tmux capture' crates/ccteam-web/src/` | 0 命中(M5.2 截图通过 `ccteam_core::render_screenshot` 调,内部已 vt100 化)| 全 PR |
| **永不主动 kill** | `git grep -nE '\bkill\b' crates/ccteam-web/src/` | 0 命中 | 全 PR |
| **ccteam-web 不依赖 ccteam-cli** | `cargo tree -p ccteam-web \| grep -E 'ccteam-cli'` | 0 命中 | PR #1 起守 |
| **actions module 共消费** | `git grep -nE 'use ccteam_core::actions' crates/` | ≥ 2 命中(mcp_serve + ccteam-web) | PR #1 / 全 PR ship gate |
| **token constant-time** | `git grep -nE 'subtle::\|ConstantTimeEq' crates/ccteam-web/src/auth.rs` | ≥ 1 命中 | PR #4 |
| **token mode 0600** | `git grep -nE '0o600\|mode\(0o6' crates/ccteam-web/src/auth.rs` | ≥ 1 命中 | PR #4 |
| **--no-auth 大字警告** | `git grep -nE 'LAN-wide RCE\|bypassPermissions' crates/ccteam-web/src/` | ≥ 1 命中 | PR #4 |
| **不 polling 截图** | `git grep -nE 'interval\(.*screenshot\|render_screenshot.*loop' crates/ccteam-web/src/` | 0 命中(只 GET handler 同步调) | PR #3 |
| **broadcast bound 防 OOM** | `git grep -nE 'broadcast::channel' crates/ccteam-web/src/` | bound 显式 1024 字面 | PR #3 |
| **status badge 不 trigger 副作用** | `git grep -nE 'silence_classifier' crates/ccteam-web/src/` | 命中只在 read-only 分类,不调 re-inject / escalate fn | PR #2 |

---

## 9. 文档同步矩阵

每 PR merge 时同步;PR #5 ship gate 校验完成。

| PR | docs/tech-design.md | docs/interfaces.md | docs/dev-coupling-audit.md | CLAUDE.md / 其他 |
|---|---|---|---|---|
| **#1 M5.0** | §6 扩展点表 placeholder | §10.6 `ccteam web` 命令 | F45 加 | — |
| **#2 M5.1** | §3.8 dashboard 描述 | §13 web routes(M5.1 read-only)| — | — |
| **#3 M5.2** | §6.4 channel layer | §13 SSE wire format / screenshot endpoint | — | — |
| **#4 M5.3** | §3.8 写动作 + token auth | §13 POST endpoints / Authorization / token 文件 | F45 close | — |
| **#5 M5.4** | §3.8 / §6.4 终稿 | §13 终稿 | F45 close 校验 | CLAUDE.md §一 baseline;v0-2/README V0.3 ship pointer;v0-3/e2e-retro.md 落档 |

跨版本 SoT 不动:`docs/v0-1/` `docs/v0-2/` `docs/v0-2-2/` 历史归档。

---

## 10. 测试 baseline

| PR | base | 增量 | 累计 |
|---|---|---|---|
| #1 M5.0 | 631 | +~10(actions promote 单元 / health e2e) | ~641 |
| #2 M5.1 | 641 | +~10(dashboard / project / 404 / assets) | ~651 |
| #3 M5.2 | 651 | +~12(watcher / SSE / per-slug filter / screenshot) | ~663 |
| #4 M5.3 | 663 | +~15(actions endpoints / auth middleware / token file / CSRF) | ~678 |
| #5 M5.4 | 678 | +~5(e2e happy path) | **~683** |

clippy 不新增 warning(4 pre-existing 不算)。

---

## Changelog

- 2026-05-10:初稿。基于 `docs/v0-3/prd.md` + V0.2.2 dev-plan 风格参考拆 5 PR
  (M5.0-M5.4),~2.25 kLOC + ~50 测试,~3 周。每 PR worktree subagent
  briefing 模板就位,subagent 拿到模板 + 进 worktree 即可开干。
