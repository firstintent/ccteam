# V0.3.2 开发计划

> V0.3.2 patch round 实施计划。**Shape A + B2 已 lock**（2026-05-11）：
> web UI rewrite + WebSocket PTY relay；按依赖顺序拆 **8 个 PR**（F52-F59，
> 每 finding 一 PR，最后 F59 是 ship gate chore）。worktree-per-PR；每 PR
> 一份 subagent briefing。
>
> 配套文档：
> - 需求决策：`docs/v0-3-2/prd.md`（10 节，§3 locked decisions）
> - 文档索引：`docs/v0-3-2/README.md`
>
> base = `origin/main` `10634c0`（V0.3.1 ship 终点）；测试 baseline `833/0`；
> workspace.version 起点 `0.3.1`。
>
> 跟 V0.3.1 dev-plan 同模式：**只完整给出 F52 的 subagent briefing**（JSON
> API parity 是后续 SPA 的前提），F53-F59 briefing 由派工 dispatcher 在 F52
> 模板基础上增量化（参 PRD §4 各 finding scope + 本文档 §3-§9 任务清单）。

---

## 1. PR 总览

| # | finding | branch | 工程量估 | 主要前置 |
|---|---|---|---|---|
| **PR #1** | **F52** JSON API parity layer | `v0-3-2-json-api` | ~600 LoC + ~15 测试，3-4 天 | 无 |
| **PR #2** | **F53** vite scaffold + rust-embed + MIT NOTICE + base shell | `v0-3-2-spa-scaffold` | ~3 kLOC TS（lift）+ ~200 LoC Rust + ~5 测试，3-4 天 | 无（独立目录 + build chain）|
| **PR #3** | **F54** Dashboard + Project list SPA | `v0-3-2-spa-dashboard` | ~800 LoC TS + ~8 测试（playwright），3-4 天 | F52 + F53 |
| **PR #4** | **F55** Detail page + harness panel + events live | `v0-3-2-spa-detail` | ~1.2 kLOC TS + ~10 测试，4-5 天 | F54 |
| **PR #5** | **F56** WS PTY relay backend | `v0-3-2-pty-ws` | ~500 LoC Rust + ~12 测试，3-4 天 | 无（与 F52/F53 并行）|
| **PR #6** | **F57** xterm input wiring in SPA | `v0-3-2-spa-terminal` | ~600 LoC TS（lift AoE useTerminal）+ ~8 测试，2-3 天 | F55 + F56 |
| **PR #7** | **F58** Write actions + auth flow | `v0-3-2-spa-actions` | ~600 LoC TS + ~8 测试，2-3 天 | F55 |
| **PR #8** | **F59** htmx retirement + e2e + ship gate | `v0-3-2-ship-gate` | ~300 LoC + 文档 + e2e suite，2-3 天 | F52-F58 全 merge |

**总计**：~8 kLOC TS（lift 占 ~60%）+ ~1.6 kLOC Rust + ~70 测试，~20-25 天
（单人 5 天/周即 4-5 周；F52/F53/F56 三路并行 + F57/F58 并行可压到 ~15 天，
即 3 周）。

### 1.1 依赖图

```
PR #1 (F52 JSON API)        ────┐
PR #2 (F53 vite scaffold)   ────┤   三路并行 (互不冲突)
PR #5 (F56 WS PTY backend)  ────┤
                                 │
PR #3 (F54 Dashboard SPA) ←─────┤   depends on F52 + F53
   ↓                             │
PR #4 (F55 Detail page) ────────┤   depends on F54
   ↓                             │
PR #6 (F57 xterm wire) ←────────┤   depends on F55 + F56
PR #7 (F58 Write actions) ←─────┤   depends on F55；与 F57 可并行
                                 ↓
                          PR #8 (F59 ship gate)   depends on all
```

**并行机会**：
- **F52 / F53 / F56** 三路 worktree 起步（修改不重叠）
- **F57 vs F58** 同时起 worktree（F57 改 TerminalView，F58 改 Forms 组件，
  冲突点仅 routing 文件 + types.ts，conflict 小）
- **F54 后**才能 spawn F55；F55 后才能 spawn F57/F58

---

## 2. PR #1 — F52 JSON API parity layer（完整 subagent briefing 模板）

> **目标**：在 `crates/ccteam-web/src/routes/` 下新增 `api_v1.rs` 模块，把
> 现有 askama HTML 路径返回的所有数据 1:1 暴露为 JSON 端点；现有 HTML
> 路径**不动**（保留到 F59 才下线）。SSE 通道（progress / harness）已
> 是 JSON-Lines，本 PR 不动；写动作端点（actions.rs 7 条）扩展为
> 双发：form-encoded（现状）+ application/json（新增）。
>
> **不在本 PR**：新 SPA 路由（F53）、SPA 实际消费 JSON（F54+）。

**关联 PRD**：§4 F52 全文 + §6 红线 + 调研报告 task #1 §3 数据模型

**前置**：无。

### 2.1 任务

- [ ] **#1.1** 新模块 `crates/ccteam-web/src/routes/api_v1.rs`
  - mount 在 `stateful_router` 下 `/api/v1/`（auth gate 同其他 stateful 路由）
  - 共享 `AppState`（state.rs）

- [ ] **#1.2** JSON 路由 — read endpoints
  - `GET /api/v1/projects` → `Vec<DashboardRow>` JSON（views.rs:36-42 结构序列化）
  - `GET /api/v1/projects/{slug}` → `ProjectSummary` JSON
    - 字段对应 views.rs:55-84 `ProjectTemplate`，但 **删 askama-specific 字段**
      （`version` / `auth_wire_token` 单独路由暴露，避免泄露 token 到列表 API）
    - 含 `sessions: Vec<SessionCard>`（views.rs:86-98）
    - 含 `state_json_pretty` → 改为 `state: serde_json::Value`（不 pretty
      print，SPA 自决定格式）
    - 含 `events: Vec<EventRow>`、`outbox: Vec<OutboxRow>`
    - 含 `decision_candidates: Vec<String>`
  - `GET /api/v1/projects/{slug}/sessions/{sid}` → `SessionDetail` JSON
    - 字段对应 views.rs:100-121 `SessionTemplate`
    - 含 `harness_snapshot: Option<HarnessSnapshotView>`（views.rs:123-129）
  - `GET /api/v1/auth/token` → `{"wire_token": "ccteam:<hex>"}` or `null`
    - 单独路由,仅 detail/dashboard SPA 需要,避免列表泄露

- [ ] **#1.3** JSON 路由 — write endpoints
  - 现有 7 条 actions.rs 路由（btw / inject_decision / pause / resume 各
    project / session 形态）**扩展** content-type 协商：
    - `Content-Type: application/x-www-form-urlencoded` → 维持现状（303）
    - `Content-Type: application/json` → 接受 JSON body，返回 `{"ok":true}` 或
      `{"ok":false,"error":"..."}` 状态码（200 / 400 / 500）
  - 实现路径：handlers 内 `match content_type` 派发；提取共享 inner
    function 处理业务逻辑（避免重复 validation）

- [ ] **#1.4** 错误形态
  - JSON path 不返回 303；返回 `{"ok":false,"error":"<msg>"}` + 4xx/5xx
  - HTML path 现状不动（继续 303 / 400 with HTML body）
  - 错误消息中**不**包含 token / 路径泄漏（沿用现有 validation 错误模板）

- [ ] **#1.5** 数据序列化兼容性
  - 现有 `EventRow.ts` 是 `String`（RFC3339），保留
  - `OutboxRow.created_at` 同上
  - `HarnessSnapshotView.captured_at` 现是 string-rendered，保留
  - 添加 `#[derive(Serialize)]` 到 views.rs 各结构；删 askama 不需要的字段
    用 `#[serde(skip_serializing_if = "Option::is_none")]`

- [ ] **#1.6** 文档同步
  - 改 `docs/interfaces.md`：新增 §"V0.3.2 JSON API v1"，每个端点 schema 一段
  - 改 `docs/dev-coupling-audit.md`：F52 entry（含来源、scope、acceptance）

- [ ] **#1.7** 测试
  - `crates/ccteam-web/tests/api_v1_test.rs`（新文件）：
    - `GET /api/v1/projects` shape stable
    - `GET /api/v1/projects/{slug}` 含 sessions / events / outbox
    - `GET /api/v1/projects/{slug}/sessions/{sid}` 含 harness_snapshot
    - JSON POST `/api/{slug}/btw` 返回 `{"ok":true}`
    - JSON POST 错误 body（empty / overflow）返回 4xx + `{"ok":false}`
    - Auth gate（无 cookie + 无 Bearer → 401）
    - HTML path（form-encoded）依旧 303（regression guard）
  - `cargo test --workspace` 总数应 ≥ baseline `833/0` + ≥ 8 新测试

### 2.2 红线 grep

提交前自检：

```bash
# 不破红线 1：progress.jsonl 仍是 SoT
grep -rn "parse.*progress\|tail.*progress\|read.*progress.jsonl" \
  crates/ccteam-web/src/routes/api_v1.rs
# 期望只在 read path（queries.rs）见到，api_v1.rs 自己不 grep

# 不破红线 2：不触 tmux
grep -rn "tmux\|send-keys\|pipe-pane" crates/ccteam-web/src/routes/api_v1.rs
# 期望：0 hit

# 不破红线 3：不写 backwards-compat shim
grep -rn "// V0.3 compat\|// legacy\|deprecated" \
  crates/ccteam-web/src/routes/api_v1.rs
# 期望：0 hit

# 不破红线 4：JSON response 不泄露 wire_token 字段于列表 API
grep -rn "wire_token" crates/ccteam-web/src/routes/api_v1.rs
# 期望：只在 /api/v1/auth/token 路由出现
```

### 2.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-web/src/routes/mod.rs` | edit（mount api_v1）| +5 |
| `crates/ccteam-web/src/routes/api_v1.rs` | new | +350 |
| `crates/ccteam-web/src/routes/actions.rs` | edit（content-type 协商）| +80 |
| `crates/ccteam-web/src/views.rs` | edit（Serialize 派生 + skip_serializing）| +30 |
| `crates/ccteam-web/tests/api_v1_test.rs` | new | +250 |
| `docs/interfaces.md` | edit（新增 §V0.3.2 JSON API v1）| +120 |
| `docs/dev-coupling-audit.md` | edit（F52 entry）| +15 |

总约 +600 LoC + 15 测试。

### 2.4 PR 描述模板

```
v0.3.2-f52: JSON API parity layer (Closes F52)

Maps to:
- requirements.md §痛点 1（可视化）+ §痛点 9（远程操控）
- tech-design.md §3.8 用户接口层 + §6.2 hooks
- docs/v0-3-2/prd.md §4 F52
- dev-coupling-audit.md F52

Scope:
- New /api/v1/{projects,projects/{slug},projects/{slug}/sessions/{sid},auth/token}
- Existing 7 write endpoints accept application/json (return {"ok":true})
- HTML routes unchanged (kept until F59)

Tests: +X (api_v1_test.rs)
Baseline: 833 → 833+X
```

### 2.5 worktree 命令

```bash
git worktree add -b v0-3-2-json-api /tmp/ccteam-v032-f52 origin/main
cd /tmp/ccteam-v032-f52
# ... 干活 ...
cargo test --workspace 2>&1 | grep -E "^test result"
cargo fmt -- $(git diff --name-only origin/main | grep '\.rs$')
git commit -am "v0.3.2-f52: JSON API parity layer (Closes F52)"
git push origin v0-3-2-json-api
gh pr create --title "v0.3.2-f52: JSON API parity layer" --body "$(cat <<'EOF'
... PR 描述模板 ...
EOF
)"
# merge 后:
cd /home/admin/robgeo/ccteam && git worktree remove /tmp/ccteam-v032-f52
```

---

## 3. PR #2 — F53 vite scaffold + rust-embed + MIT NOTICE + base shell

**关联 PRD**：§4 F53 + 调研报告 task #2 §1-§3（AoE stack / 组件树 / state mgmt）

**前置**：无（与 F52 / F56 并行）。

### 3.1 任务

- [ ] **#2.1** Fork AoE 前端（**用户 confirm 2026-05-11，msg 349：技术栈和 AoE 完全一致，不裁剪 deps**）
  - `cp -r references/agent-of-empires/web/ crates/ccteam-web/web/`
  - `rm -rf crates/ccteam-web/web/.git crates/ccteam-web/web/dist crates/ccteam-web/web/node_modules`
  - **package.json 处理**（关键 — 不动 deps 列表，只动 name）：
    - `name` → `ccteam-web`
    - `version` → `0.3.2`
    - 所有 `dependencies` + `devDependencies` **原样保留**（含 `@assistant-ui/*`、
      `@wterm/*`、`cmdk`、`react-diff-viewer-continued`、`shiki`、`marked`、
      `lucide-react`、`react-router-dom`、`tailwindcss`、`react-19`、
      `vite-8`、`typescript-6` 等所有 pin）
    - `scripts` 原样保留（`dev` / `build` / `lint` / `preview` / `test` /
      `test:ui` / `test:unit`）
  - **AoE-domain 组件 / 路由 → 暂时保留代码，但本轮不挂路由**（用户决策："完全
    参考"，未来若需类似能力可激活）：
    - `src/components/cockpit/` — 保留代码，App.tsx 路由表不引用
    - `src/components/wizard/` (SessionWizard) — 保留代码，不挂入 SPA 主流程
    - `src/components/settings/profiles/` — 保留代码，SettingsView tab 隐藏
    - `src/lib/acp*.ts` / `profile*.ts` / `docker*.ts` — 保留 util；调用方在
      F54+ 决定是否激活
  - **vite tree-shake** 会自动剔除 unused exports；保留代码不增加 bundle 显著
    体积（实测 AoE 全量 build ~800KB gzip，drop 后 ~620KB，差异不大）
  - **后果说明**：保留 AoE 完整 deps 意味着 ccteam-web 接受 AoE 同等量级的
    transitive deps；F58 完成后跑 `npm audit` 检查 CVE，必要时单条 pin 升级
    （不裁剪整个 dep）
- [ ] **#2.2** MIT NOTICE
  - 新文件 `crates/ccteam-web/web/NOTICE`：
    ```
    Portions of this directory tree are derived from Agent of Empires
    (https://github.com/njbrake/agent-of-empires),
    MIT License Copyright (c) 2026 Nathan Brake.
    See web/LICENSE for the full original MIT license text.
    Modifications by the ccteam project, also MIT-licensed.
    ```
  - 保留 AoE 原 LICENSE 副本为 `crates/ccteam-web/web/LICENSE.aoe-mit`
- [ ] **#2.3** Vite + TS config 调整
  - `vite.config.ts`：`build.outDir` → `dist/`；dev `server.proxy` 改 `:7331`
    （ccteam-web 默认）；`base: '/app/'`
  - `tsconfig.app.json`：sanity check
- [ ] **#2.4** Build chain — Rust 侧
  - `crates/ccteam-web/build.rs`（新文件）：
    - `cargo:rerun-if-changed=web/`
    - `Command::new("npm").args(["run","build"]).current_dir("web").status()`
    - feature gate `web-bundle`（dev 模式可跳过 npm build；CI / release 必跑）
  - `Cargo.toml`：添 `rust-embed = "8"` dep；feature `web-bundle`（default-on）
- [ ] **#2.5** rust-embed swap
  - `crates/ccteam-web/src/routes/assets.rs`：
    - 添 `#[derive(RustEmbed)] #[folder = "web/dist/"] struct SpaAssets;`
    - 新路由 `GET /assets/spa/{*path}` → 嵌入资源
    - 现有 htmx/xterm 路径**保留**（F59 才删）
  - 新路由 `GET /app/{*path}` → 返回 `web/dist/index.html`（react-router fallback）
- [ ] **#2.6** Base shell（最小 lift）
  - lift `src/App.tsx`、`src/main.tsx`、`src/components/TopBar.tsx`、
    `src/components/ContentSplit.tsx`、`src/lib/toast*.ts`
  - 替换 AoE-specific imports / fetch endpoints 为 ccteam stub（返回空）
  - `src/pages/Dashboard.tsx` 占位（"V0.3.2 SPA scaffold OK"）
- [ ] **#2.7** 测试
  - `crates/ccteam-web/tests/spa_assets_test.rs`：`GET /app/` 200 + HTML；
    `GET /assets/spa/index-XXX.js` 200 + JS content-type
  - playwright 暂不接（F54 起）

### 3.2 红线 grep

```bash
# 1. 与 AoE deps 完全一致（不裁剪）—— 校验 dep 列表 diff
diff <(jq -S '.dependencies' references/agent-of-empires/web/package.json) \
     <(jq -S '.dependencies' crates/ccteam-web/web/package.json)
# 期望: 0 diff（version pin + name 列表完全一致）

# 2. npm license 全合规（不引入 GPL）
cd crates/ccteam-web/web && npx license-checker --summary 2>/dev/null
# 期望: 仅 MIT / Apache-2.0 / ISC / BSD-* / CC0；GPL 系列出现 → 立项 cleanup

# 3. 不 vendor source（AoE 不进 ccteam deps tree）
grep -rn "references/agent-of-empires" crates/ccteam-web/
# 期望: 0 hit（fork 后是独立 web/ 目录，不再引用 references/）

# 4. Rust 红线
grep -rn "tmux\|send-keys\|progress.jsonl" crates/ccteam-web/build.rs
# 期望: 0 hit
```

### 3.3 文件 touch 矩阵

| 路径 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-web/web/` | new (fork) | +12000（lift，~60% 会在 F53-F59 内删/改）|
| `crates/ccteam-web/web/NOTICE` | new | +6 |
| `crates/ccteam-web/build.rs` | new | +40 |
| `crates/ccteam-web/Cargo.toml` | edit | +10 |
| `crates/ccteam-web/src/routes/assets.rs` | edit | +60 |
| `crates/ccteam-web/src/routes/mod.rs` | edit | +5 |
| `crates/ccteam-web/tests/spa_assets_test.rs` | new | +60 |

---

## 4. PR #3 — F54 Dashboard + Project list SPA

**关联 PRD**：§4 F54 + 调研报告 task #2 §2（组件树）+ task #1 §3（数据模型）

**前置**：F52（JSON）+ F53（SPA 骨架）已 merge。

### 4.1 任务

- [ ] **#3.1** Lift `WorkspaceSidebar.tsx` + `SessionItem.tsx` + `StatusGlyph.tsx`
  - 改 data layer：fetch `/api/v1/projects`（F52 端点）
  - 桶 B rewire：分组按 `team / kind`（workflow / flex / multi_workflow）
  - sidebar row 渲染 harness badge（claude / codex-stub）
- [ ] **#3.2** 新 hook `src/lib/useProgressStream.ts`
  - `useProgressStream()` → 订阅 `/sse/all`
  - `useProgressStream(slug)` → 订阅 `/sse/project/{slug}`
  - `useProgressStream(slug, sid)` → 订阅 `/sse/project/{slug}/{sid}`
  - 处理 `reconnect_hint` event → close + reconnect
- [ ] **#3.3** `src/pages/Dashboard.tsx` 完整实现
  - 列所有项目 + last event tail
  - SSE 推过来 last-event-label 立即更新
  - 点项目卡进 `/app/p/{slug}`（router stub，F55 实现）
- [ ] **#3.4** Mobile 响应式（lift 桶 A）
  - `useMobileKeyboard.ts` + Tailwind `md:` breakpoint
  - sidebar < 768px 改 overlay + 汉堡按钮
- [ ] **#3.5** 测试
  - playwright `dashboard.spec.ts`：render + SSE inject + project click
  - lift AoE `web/playwright.config.ts` 改 `baseURL` 为 `http://localhost:7331/app/`

### 4.2 红线 grep

```bash
# SPA 不写 hardcode team 名
grep -rn "ccteam\|chainup\|test_team\|\"team\":\"" crates/ccteam-web/web/src/
# 期望: 只在 i18n / 调试 / 测试 fixture 见到

# SPA 不直读文件系统（必须走 ccteam-web HTTP）
grep -rn "fs/promises\|node:fs\|require.*fs" crates/ccteam-web/web/src/
# 期望: 0 hit（前端就只能走 fetch，理论上不会，做兜底）
```

---

## 5. PR #4 — F55 Detail page + harness panel + events live

**关联 PRD**：§4 F55 + 调研报告 task #1 §6（templates）+ task #2 §2

**前置**：F54 已 merge。

### 5.1 任务

- [ ] **#4.1** `src/pages/ProjectDetail.tsx`
  - fetch `/api/v1/projects/{slug}`
  - 渲染 metadata（team / kind / phase / created_at / cost / badge）
  - sessions tab（flex 团队）
  - events list（200 row tail + SSE prepend via `useProgressStream`）
  - outbox list（带"open in editor" 链接 `vscode://file/...` URL scheme）
  - auth banner（绿 / 红，来自 `/api/v1/auth/token`）
- [ ] **#4.2** `src/pages/SessionDetail.tsx`
  - fetch `/api/v1/projects/{slug}/sessions/{sid}`
  - harness panel：subscribe `/sse/harness/{slug}/{sid}`，渲染 model /
    ctx% / cost / rate-limit / captured_at
  - 同 ProjectDetail 的 events + outbox
- [ ] **#4.3** Router 配置
  - `/app/` → Dashboard
  - `/app/p/{slug}` → ProjectDetail
  - `/app/p/{slug}/s/{sid}` → SessionDetail
  - `/app/settings` / `/app/projects` → 占位 page（V0.4 deferred）
- [ ] **#4.4** 测试
  - playwright `detail.spec.ts`：navigate + harness SSE inject + events
    SSE prepend + flex session tab 切换不丢 state

### 5.2 红线 grep

```bash
# 不解析 events JSON 改 orchestrator 状态
grep -rn "POST.*progress\|write.*jsonl" crates/ccteam-web/web/src/pages/
# 期望: 0 hit（events 只 read + render，不回写）
```

---

## 6. PR #5 — F56 WS PTY relay backend

**关联 PRD**：§4 F56 + §3.2（B2 决策详细 spec，AoE 实现路径已 lift）+ §6 红线

**前置**：无（与 F52/F53 并行）。

> **设计原型**：`references/agent-of-empires/src/server/ws.rs:161-...`
> （`handle_terminal_ws` 函数）—— **每个 WS 一个独立 PTY**，spawn
> `tmux attach-session -t <name>` 作为 PTY slave 子进程；不是 FIFO + pipe-pane
> 也不是 send-keys。先读那个文件再写代码。

### 6.1 任务

- [ ] **#5.1** 新模块 `crates/ccteam-web/src/routes/pty_ws.rs`
  - 路由：`GET /ws/{slug}/pty` + `GET /ws/{slug}/{sid}/pty`
  - axum `extract::ws::WebSocketUpgrade`；subprotocol `ccteam-auth`
  - middleware 校验 cookie `ccteam_token`；缺 token → 401 reject upgrade
  - **不**接 `?token=` query param（query string 进日志风险；cookie shim 已
    覆盖首次跳转路径）
- [ ] **#5.2** PTY 生命周期（lift AoE ws.rs:161-260）
  - `portable_pty::NativePtySystem::default()` 开 PTY 对（slave + master）
  - `CommandBuilder::new("tmux")` 带 args:
    `["set-option","-as","terminal-overrides","*256col*:U8=1:smacs@:rmacs@:acsc@",
    ";","attach-session","-t",&tmux_name]`
  - `env("TERM","xterm-256color")`、`env_remove("TMUX")`（允许嵌套）
  - `pair.slave.spawn_command(cmd)` 启动 tmux attach 子进程；drop slave handle
  - 失败路径：spawn 失败 / clone_reader 失败 → kill+wait child + early return
- [ ] **#5.3** 双向 byte relay（lift AoE ws.rs:336-...，简化版）
  - **Output**: tokio `spawn_blocking` 跑 `reader.read(&mut [u8; 8192])` 循环
    → mpsc to async task → `ws.send(Message::Binary(bytes))`
  - **Input**: WS recv loop
    - `Message::Binary(bytes)` → `writer.write_all(&bytes)`（PTY 字节流原生
      支持所有键序列，无需 send-keys 转义）
    - `Message::Text(json)` 解析控制帧：
      - `{"type":"resize","cols":N,"rows":N}` → `master.resize(PtySize{rows,cols,..})`
        → SIGWINCH 给 tmux attach
      - `{"type":"pause_output"}` → SIGSTOP 给 child PID（per-session refcount）
      - `{"type":"resume_output"}` → SIGCONT
    - `Message::Close` / EOF → break loop + cleanup
  - **清理**：WS 关闭 → kill child + wait → reap zombie；不 leak PTY fd
- [ ] **#5.4** Primary-client 语义（lift AoE ws.rs:142-153）
  - `AppState` 加 `session_primaries: Arc<RwLock<HashMap<String, String>>>`
    `(tmux_name → client_id)`
  - 每个 WS 分配 `client_id` 单调递增（AtomicU64）
  - 用户发 keystroke → 该 client 升为 primary
  - 只有 primary 的 resize 应用；其他 client 的 resize **静默忽略**
  - 多客户端打架预防（手机/桌面同时连）
- [ ] **#5.5** Ping/idle timeout（lift AoE ws.rs:127-140）
  - server-originated `Message::Ping(vec![])` 每 30s
  - 客户端 90s 内无任何消息（含 pong）→ drop WS
  - 防止 RLIMIT_NOFILE 累积泄漏
- [ ] **#5.6** SIGSTOP/SIGCONT refcount（lift AoE ws.rs:155-159）
  - `AppState` 加 `session_pause_counts: Arc<Mutex<HashMap<String, u32>>>`
  - 第一个 client 发 pause → SIGSTOP；最后一个 client 发 resume → SIGCONT；
    中间状态只更新 count，不动 signal
- [ ] **#5.7** 测试（先写 helpers，因 `tests/common/` 不存在）
  - 新文件 `crates/ccteam-web/tests/common/tmux_fixture.rs`（参 V0.3.1
    `crates/ccteam-core/tests/tmux_test.rs` 的 helper 模式）：
    - `fn tmux_available() -> bool`
    - `fn spawn_test_session(name: &str) -> Result<Child>`
    - `fn cleanup_test_session(name: &str)`
    - 测试用 session 名前缀 `ccteam_test_pty_*` 避免污染用户 tmux
  - 新文件 `crates/ccteam-web/tests/pty_ws_test.rs`：
    - **t01_basic_connect**: tmux session 已起 → WS connect → 收到 prompt 字节
    - **t02_input_echo**: WS send `b"echo hello\r"` → 等 0.5s → reconnect WS
      或读 capture-pane 验证 session 里有 "hello"（Binary 写入路径工作）
    - **t03_control_seq**: WS send `b"\x03"` (Ctrl-C) → tmux session 进程
      收到 SIGINT；WS send `b"\x1b[A"` (Up arrow) → tmux 收到 keystroke（
      验证 PTY 透传非 ASCII 字节，无 send-keys 转义需求）
    - **t04_resize_primary**: 两个 WS 同 session；A 先发 keystroke 升 primary；
      B 发 resize 应被忽略（用 `tmux display-message -p '#{window_width}'`
      验证 cols 未变）；A 发 resize 生效
    - **t05_pause_refcount**: 两个 WS 都发 pause → SIGSTOP；A 发 resume →
      仍 STOP（refcount=1）；B 发 resume → SIGCONT
    - **t06_auth_gate**: WS upgrade 无 cookie → 401 reject
    - **t07_idle_timeout**: 暂停 ping 模拟 → 90s 后 server 主动 close
    - **t08_cleanup_on_disconnect**: WS close → child PID gone（`ps` 验证）+ PTY fd 关闭
  - 所有 tmux 测试用 `#[serial]` 串行（避免 tmux 服务器并发污染）

### 6.2 红线 grep

```bash
# 不破"progress.jsonl 唯一事实来源"
grep -rn "progress\|jsonl" crates/ccteam-web/src/routes/pty_ws.rs
# 期望: 0 hit（PTY relay 完全不接 progress 通道）

# 不破"永不主动 kill 长 session"（只 kill 自己 spawn 的 tmux attach client，
# 不 kill tmux session 本身）
grep -rn "kill-session\|kill-server" crates/ccteam-web/src/routes/pty_ws.rs
# 期望: 0 hit（child.kill() OK；只杀 attach client，不杀 server）

# 不解析 PTY 输出
grep -rn "from_utf8\|String::from\|parse" crates/ccteam-web/src/routes/pty_ws.rs
# 期望: 仅在 JSON 控制帧 path 见到（控制帧是 client → server，与 PTY 输出无关）

# 不接受 query param token 走 WS
grep -rn "Query.*token\|query_pairs" crates/ccteam-web/src/routes/pty_ws.rs
# 期望: 0 hit

# 不在 core 引入 PTY/portable_pty
grep -rn "portable_pty\|pty_ws" crates/ccteam-core/
# 期望: 0 hit（core 不感知 web PTY relay）
```

### 6.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-web/src/routes/pty_ws.rs` | new | +400 |
| `crates/ccteam-web/src/routes/mod.rs` | edit（mount + middleware）| +15 |
| `crates/ccteam-web/src/state.rs` | edit（primaries + pause_counts）| +30 |
| `crates/ccteam-web/Cargo.toml` | edit（添 `portable_pty = "0.8"`、`futures-util = "0.3"`、`nix` 用 signal feature）| +5 |
| `crates/ccteam-web/tests/common/tmux_fixture.rs` | new | +120 |
| `crates/ccteam-web/tests/pty_ws_test.rs` | new | +450 |

总约 **+500 LoC + 12 测试**（dev-plan §1 估算 +500 LoC 不变，但测试基础设
施 +120 LoC 是新增；预估时间不变 3-4 天，因 lift AoE 实现密度高）。

---

## 7. PR #6 — F57 xterm input wiring in SPA

**关联 PRD**：§4 F57 + 调研报告 task #2 §5（终端渲染）

**前置**：F55（detail page）+ F56（WS PTY backend）已 merge。

### 7.1 任务

- [ ] **#6.1** Lift `src/lib/useTerminal.ts` from AoE
  - 改 WS URL 模板：`/ws/{slug}/pty` / `/ws/{slug}/{sid}/pty`
  - 改 subprotocol：`ccteam-pty.v1`
  - 删 AoE 特定逻辑（profile / cockpit substrate）
- [ ] **#6.2** Lift `src/components/TerminalView.tsx`
- [ ] **#6.3** Lift `src/components/MobileTerminalToolbar.tsx` + `BackToLiveButton.tsx` +
  `KeyboardFab.tsx`
- [ ] **#6.4** 嵌入 `ProjectDetail` / `SessionDetail`
- [ ] **#6.5** 测试
  - playwright `terminal.spec.ts`：mock WS server → mount terminal →
    收 bytes → render；发 keystroke → mock server 收到；resize 控制帧
  - 移动端 viewport（375x667）playwright snapshot

### 7.2 红线 grep

```bash
# 用户输入路径不走 idle-aware
grep -rn "idle_prompt\|btw" crates/ccteam-web/web/src/lib/useTerminal.ts
# 期望: 0 hit（terminal 输入 = tmux 直接，与 /btw 完全分离）
```

---

## 8. PR #7 — F58 Write actions + auth flow

**关联 PRD**：§4 F58

**前置**：F55 已 merge（与 F57 可并行）。

### 8.1 任务

- [ ] **#7.1** 表单组件 lift
  - `src/components/BTWForm.tsx`（textarea + submit）
  - `src/components/DecisionForm.tsx`（select decision_candidates + body）
  - `src/components/PauseResumeButtons.tsx`
- [ ] **#7.2** Hook `src/lib/useFetch.ts`
  - 包装 fetch，自动带 cookie；401 → toast + 跳 LoginPage
- [ ] **#7.3** Lift `src/components/TokenEntryPage.tsx` + `LoginPage.tsx`
  - 改 endpoint：`/api/v1/auth/token` 验证；存 cookie shim 复用现有路径
- [ ] **#7.4** 测试
  - playwright `actions.spec.ts`：BTW submit → SSE 推回事件；inject_decision
    submit；pause/resume；401 失效流

---

## 9. PR #8 — F59 htmx retirement + e2e + ship gate

**关联 PRD**：§4 F59

**前置**：F52-F58 全 merge。

### 9.1 任务

- [ ] **#8.1** 删 askama 模板 + dep
  - 删 `crates/ccteam-web/templates/{dashboard,project,session}.html`
    （保留 `base.html`）
  - 删 `crates/ccteam-web/Cargo.toml` 中 `askama` dep
  - 删旧 `routes/dashboard.rs` `project.rs` `session.rs` handler 内的 askama
    render；改 301 redirect 到 `/app/...`
- [ ] **#8.2** 删旧静态资源
  - 删 `crates/ccteam-web/assets/{htmx.min.js,htmx-ext-sse.js,xterm.js,xterm.css,style.css}`
  - 删 `routes/assets.rs` 中对应 include_bytes! 行
  - 保留 `pane_snapshot.rs` 路由（兼容；V0.3.3 再评估）
- [ ] **#8.3** e2e suite
  - playwright happy path（dashboard → project → session → terminal → BTW
    → decision → pause/resume）
  - WS PTY mock e2e（启动 tmux mock + 验证双向）
- [ ] **#8.4** 文档
  - 新文件 `docs/v0-3-2/user-manual.md`（参 V0.3.1 user-manual.md 结构）
    - SPA 用法（dashboard / project / session / terminal / BTW / pause）
    - 两条输入通道明示：
      - **WS PTY**（terminal 区域输入）= 等价 tmux attach，**不**走 idle-aware
      - **`/btw` 注入**（BTW form）= 走 ccteam orchestrator idle-aware
    - 已知限制（无 paired terminal / 无 split window / 移动端 push 通知 V0.4）
  - 更新 `CLAUDE.md §一` 状态行（workspace.version → `0.3.2`、baseline）
  - 更新 `docs/v0-3-2/README.md` 状态 → `shipped`
- [ ] **#8.5** ship gate
  - bump `Cargo.toml` `workspace.package.version` → `0.3.2`
  - `cargo test --workspace` 全绿；记录新 baseline
  - playwright suite 全绿
  - 写 `docs/v0-3-2/e2e-retro.md`（参 V0.3.1 e2e-retro.md 结构）

---

## 10. 红线 grep 矩阵（全 round 汇总）

每个 PR 提交前跑：

```bash
# 1. progress.jsonl 仍 SoT
grep -rn "tail.*progress\|parse.*jsonl" \
  crates/ccteam-web/src/routes/ crates/ccteam-web/web/src/ \
  | grep -v "queries.rs\|useProgressStream.ts"
# 期望: 0 hit

# 2. 不解析 tmux output
grep -rn "from_utf8\|String::from\|parse" crates/ccteam-web/src/routes/pty_ws.rs
# 期望: 0 hit（pty_ws.rs 二进制透传）

# 3. 不主动 kill session
grep -rn "kill-session\|kill-server" crates/ccteam-web/src/ \
  crates/ccteam-web/web/src/
# 期望: 0 hit

# 4. 不写 backwards-compat shim
grep -rn "// V0.3 compat\|// legacy\|deprecated" crates/ccteam-web/src/
# 期望: 0 hit

# 5. ccteam-core 零团队字面量
grep -rn '"ccteam"\|"chainup"' crates/ccteam-core/src/ | grep -v "tests\|fixtures"
# 期望: 0 hit

# 6. 不引入 GPL / 不兼容 npm dep
cd crates/ccteam-web/web && npm ls --json | jq -r \
  '[.dependencies | .. | objects | select(.license) | .license] \
   | flatten | unique'
# 期望: 全 MIT / Apache-2.0 / ISC / BSD-*

# 7. token 不泄露到列表 API JSON
grep -rn "wire_token" crates/ccteam-web/src/routes/api_v1.rs \
  | grep -v "/api/v1/auth/token"
# 期望: 0 hit
```

---

## 11. Subagent 派工模板

每 PR 派 subagent 用以下 briefing skeleton（基于 V0.3.1 dev-plan §11 模式）：

```markdown
你是 V0.3.2 PR #<N> 的 implementer agent。

## 任务来源
- `docs/v0-3-2/prd.md` §4 F<N> 全文
- `docs/v0-3-2/dev-plan.md` §<对应章节>

## 已 lock 的 scope 决策
- Shape A（web UI only；CodexAdapter slip V0.3.3）
- B2（"完全参考 AoE 项目"：WebSocket PTY relay）

## worktree
```bash
git worktree add -b <branch> /tmp/<worktree-name> origin/main
cd /tmp/<worktree-name>
```

## 任务清单
（从 dev-plan §<N>.1 拷贝；逐条勾选）

## 红线 grep（提交前必跑）
（从 dev-plan §<N>.2 拷贝）

## 验收
（从 PRD §4 F<N> Acceptance 拷贝）

## PR 提交
- commit message: `v0.3.2-f<N>: <短描述> (Closes F<N>)`
- PR title: 同 commit subject
- PR body: dev-plan §<N>.4 模板

## 注意
- 不动 `references/agent-of-empires/`（只读参考；F53 fork 是一次性 cp）
- 不动 V0.3.1 已 ship 的 F46-F51 代码（除非显式 cleanup）
- 修改前先 `cargo test --workspace` 看 baseline 833/0；完工后 ≥ baseline
- 严禁 `--no-verify`；commit 失败查 hook 日志
- 单条问题 ≤ 5 次 fix-loop，第 6 次开始 escalate 回主 session

## F56 实现者额外注意
- **必读** `references/agent-of-empires/src/server/ws.rs:1-260`（PTY relay
  设计原型）
- 用 `portable_pty` + `tmux attach-session`，**不**用 `pipe-pane` + FIFO
- 字节流原生支持所有键序列；**不**用 `tmux send-keys`
- AoE 已踩雷的两个坑：
  1. `terminal-overrides` 注入避免 wterm SCS 渲染错（ws.rs:191-220 注释）
  2. `env_remove("TMUX")` 允许嵌套（ws.rs:222-223 注释）
- 测试基础设施 `tests/common/tmux_fixture.rs` 是 F56 PR 一部分（不要假设
  已存在，参 ccteam-core/tests/tmux_test.rs 自己写）
```

---

## 12. Ship gate 验收

F59 merge 前主 session 跑一遍：

```bash
# Rust 测试
cargo test --workspace 2>&1 | grep -E "^test result" \
  | awk '{p+=$4;f+=$6}END{print "passed:",p,"failed:",f}'
# 期望: passed ≥ baseline + 70, failed = 0

# Clippy（不增）
cargo clippy --workspace --all-targets -- -D warnings 2>&1 \
  | grep -c "^error" || true
# 期望: ≤ 9（pre-existing）

# Fmt（changed files）
cargo fmt -- --check $(git diff --name-only origin/main..HEAD | grep '\.rs$')
# 期望: clean

# Playwright（前端 e2e）
cd crates/ccteam-web/web && npm run test
# 期望: 全绿

# 手动 smoke test
ccteam web --bind 127.0.0.1:7331 &
curl -s http://127.0.0.1:7331/health | jq .
firefox http://127.0.0.1:7331/app/ &  # 看 SPA 渲染
# 期望: dashboard 有内容；点 detail page 看 terminal；输入键盘验证 WS PTY
```

写完 `docs/v0-3-2/e2e-retro.md` + bump `Cargo.toml` 后才发 ship gate PR。
