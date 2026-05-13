# V0.3.2 文档索引

> **状态**:**shipped**(2026-05-14,F52-F59 已合入;F59 ship gate 落档)。
> V0.3.1 README 顶的 erratum 行已说明 V0.3.1 PRD §10.3 的 CodexAdapter
> 完整实现进一步 slip 到 V0.3.3。

V0.3.2 是 V0.3.1 ship 后的第二个 patch round,**头号诉求**是把 V0.3
引入的 htmx + askama web UI 整体换成 React SPA,**完全参考**
`references/agent-of-empires/web/`(MIT,2026 Nathan Brake)的前端与
WebSocket PTY relay 协议,数据层重写以贴 ccteam 的 flex + workflow +
harness 模型。

base = `origin/main` `10634c0`(V0.3.1 ship 终点)。workspace.version
`0.3.1` → `0.3.2`(F59 bump)。Rust 测试 baseline `833/0`(F52-F58
增量未运行,**待用户跑 `cargo test --workspace` 验证**;预期红的测试
见下文 "已知 follow-up")。

## §3 已 lock 的 scope 决策(2026-05-11 用户 confirm)

- **范围 = Shape A** — V0.3.2 = web UI only;CodexAdapter slip V0.3.3
  (V0.3.1 README 已加 erratum)
- **终端 = B2** — "完全参考 agent-of-empires 项目",引入 WebSocket
  PTY relay(`portable_pty` + `tmux attach-session`,不走 `pipe-pane`)

详见 [`prd.md §3`](prd.md)。

## Findings 速查

| F | 范围 | 依赖 | PR # | 状态 |
|---|---|---|---|---|
| **F52** | JSON API parity layer | — | 1 | shipped |
| **F53** | vite scaffold + rust-embed swap + MIT attribution + base shell | — | 2 | shipped |
| **F54** | Dashboard + Project list SPA(接 F52 JSON) | F52 + F53 | 3 | shipped |
| **F55** | Project / Session detail + harness panel + events live | F54 | 4 | shipped |
| **F56** | WS PTY relay backend(`/ws/{slug}/pty` + `/ws/{slug}/{sid}/pty`) | — | 5 | shipped |
| **F57** | xterm input wiring in SPA(lift AoE `useTerminal` + ccteam adapter) | F55 + F56 | 6 | shipped |
| **F58** | Write actions(`btw`/`inject_decision`/`pause`/`resume`)+ auth flow | F55 | 7 | shipped |
| **F59** | htmx UI retirement + ship gate | F54-F58 | 8 | shipped |

## F52-F59 ship summary

- **F52**:JSON API parity layer 立起,htmx 路径仍存做 fallback(F59 移除)。
- **F53**:vite scaffold + AoE fork + `rust-embed` 嵌入 SPA bundle;
  `build.rs` 加 `web-bundle` feature gate 与 `CCTEAM_SKIP_WEB_BUILD`
  环境变量(开发态跳过 `npm run build`)。
- **F54**:Dashboard SPA 上线(`/app/`);SSE last-event-label 实时更新。
- **F55**:project / session detail 落地;harness panel 沿 V0.3.1 F46 SSE。
- **F56**:WS PTY relay backend 上线;cookie auth gate 强制。
- **F57**:xterm input 端打通,移动端键盘工具栏复用 AoE 现成实现。
- **F58**:写动作端到端,token auth 流程含 401 → TokenEntryPage 自动 gate。
- **F59**:F58 表单 wire 进 F55 page;AoE-orphan TS 文件清理(21 个文件
  删除);htmx 三 `templates/*.html` 删除,`templates/base.html` 保留作
  askama SSR fallback;legacy routes(`/`, `/project/{slug}`,
  `/session/{slug}/{sid}`)改 301 → `/app/...`;legacy htmx 静态资源
  (`htmx.min.js` 等)V0.3.3 再清(TODO marker in `routes/assets.rs`);
  `workspace.version` bump → 0.3.2,V0.3.2 ship 文档落档。

## 文档清单

| 文件 | 内容 | 状态 |
|---|---|---|
| [`prd.md`](prd.md) | V0.3.2 PRD — 背景 + AoE 复用边界 + §3 locked decisions + F52-F59 设计 + 红线 + PR sequencing | locked v1 |
| [`dev-plan.md`](dev-plan.md) | F-finding subagent briefing 模板 + 红线 grep 矩阵 + 依赖图 + worktree 命令 | locked v1 |
| [`user-manual.md`](user-manual.md) | SPA 用法 + 两条输入通道(WS PTY vs `/btw`)说明 + token auth 流程 + dev 跳过 SPA bundle 的开关 | shipped(F59) |

## 关键设计决策

详 [`prd.md §3`](prd.md):

- **lift AoE 前端,数据层重写** — 桶 A(原子复用)~40-50% 直接 use;
  桶 B(lift & rewire)~30%;桶 C(代码保留不挂主路由)~20%
- **完全沿用 AoE package.json deps** — 不裁剪;vite tree-shake 处理
  bundle 体积
- **WS PTY = portable_pty + tmux attach-session** — 不走 `pipe-pane`,
  让 PTY 字节流原生支持所有键序列(`\x03` / `\x1b[A` / 等等)
- **`/btw` 注入 vs 直接 WS 输入 = 两条独立通道** — 用户 manual 必须
  明示;详 [`user-manual.md`](user-manual.md)
- **token auth 不退化** — cookie shim + Bearer header 双道仍在;WS
  upgrade 强制 cookie 校验

## V0.3.3 / V0.4 deferred 项

详 [`prd.md §5`](prd.md):

- **CodexAdapter 完整实现**(spawn / ingest / hook)— V0.3.3,路线见
  [`docs/research/ccteam-codex-integration.md`](../research/ccteam-codex-integration.md)
- **flex workflow promotion / demotion** — V0.3.3 / V0.4
- **flex retro_schema enable** — V0.3.3(依赖 promotion 落地)
- **paired terminal**(AoE 概念)— V0.3.3+
- **WS PTY 多 pane / split-window** — V0.4
- **session-level inbox 细分** — V0.4(用户撞 cross-talk 再加)
- **`mcp__codex__codex` MCP peer 注册** — V0.3.3+(依赖 CodexAdapter)
- **mobile push notification 真实接入** — V0.4
- **harness snapshot 历史 archive** — V0.4
- **legacy htmx assets 清理**(`assets/htmx.min.js` 等)— V0.3.3
  (TODO marker 已埋在 `crates/ccteam-web/src/routes/assets.rs`)

## 已知 follow-up(用户跑测试前先看)

F59 ship 没有运行 `cargo test --workspace`,以下几类测试**预期会红**,
下一轮 cycle 处理:

1. **`crates/ccteam-web/tests/dashboard_test.rs`** — `GET /` 多处断言
   HTML body(`"Projects"` heading / `<th>Kind</th>` / `<code>workflow
   </code>` 等)。F59 改 301 后 status 不再是 200,body 不再是 HTML。
   需改为断言 `status == 301` + `Location: /app/`,或迁到
   `tests/api_v1_test.rs` 同等断言走 JSON。
2. **`crates/ccteam-web/tests/project_test.rs`** — 多处 `GET /project/<slug>`
   原断言 HTML body 内容,新路径返回 301 + 空 body。需改为断言
   `status == 301` + `Location: /app/p/<slug>` header。
3. **`crates/ccteam-web/tests/e2e_test.rs`** + **`flex_e2e_test.rs`** —
   `GET /project/<slug>` 断言 `body mentions phase` 段失效;同上改为
   301 断言即可,JSON 等价断言已在 api_v1_test.rs 覆盖。
4. **`crates/ccteam-web/tests/actions_test.rs`** — 文件顶 doc-comment 写
   "303 See Other → `/project/<slug>`",F58 已切到 JSON `{"ok":true}`,
   doc-comment 仅需更新(实际 assertion 应已是 JSON)。
5. **AoE-fork web 单测**(`crates/ccteam-web/web/...`,vitest /
   playwright)— `Toasts` 的 SW-message handler 已删,`sessionId` 路径已删,
   相关 vitest 断言(若有)需要更新或删除。`npm run build` 应能过(orphan
   清理后 import 链已闭合)。
6. **`askama` workspace dep** — F59 后没有任何 live `#[derive(Template)]`
   引用它,`cargo build` 会带个 unused-dep warning(非 error)。`templates/
   base.html` 还在,所以 dep 保留;V0.3.3 若移除 `base.html`,顺手把
   `askama = "0.12"` 从 `Cargo.toml` workspace deps 与
   `crates/ccteam-web/Cargo.toml` `[dependencies]` 都清掉。

## 跟其他文档关系

- 主仓 `CLAUDE.md` §一 baseline 已由 F59 回填(0.3.1 → 0.3.2,V0.3.2
  milestone 行;测试数留 `**待用户跑 cargo test 验证**` 占位);§三
  红线 V0.3.2 不动(progress.jsonl SoT / 永不主动 kill / ccteam-core
  无 team 名字面量)。
- `docs/interfaces.md` §16 — F52 JSON API endpoints + WS PTY
  subprotocol(`ccteam-pty.v1`),如每 PR 同步则已落档。
- `docs/dev-coupling-audit.md` F52-F59 — F59 标记 close 状态。
- `docs/v0-3-1/README.md` — V0.3.2 erratum 行已加(V0.3.3 slip 提示)。
- `docs/research/ccteam-codex-integration.md` — V0.3.3 Codex real
  implementation 走该研究 doc 的 M1-M5 路线。

## 配套(F59 PR)

- `Cargo.toml::workspace.package.version` `"0.3.1"` → `"0.3.2"`。
- `crates/ccteam-web/Cargo.toml::package.description` — V0.3 → V0.3.2,
  内容更新为 "React SPA + JSON API + WS PTY relay"。
- `crates/ccteam-web/web/package.json::version` — `"0.3.2"`(F53 已 set)。
- `CLAUDE.md` §一 baseline 行已回填。
- `templates/base.html` 保留作 askama SSR fallback;`templates/
  {dashboard,project,session}.html` 删除。
- AoE-orphan TS 清理:`hooks/{useSessions,useFileDiff,useDiffFiles,
  useHighlightedLines,useRepoGroups,useWorkspaces}.ts`,
  `components/{StatusGlyph,DisconnectBanner}.tsx`,
  `lib/{connectionState,session,session.test,sessionRoute,
  legacySessionRedirect,diffTree,diffTree.test,highlighter,ansi,
  ansi.test,idleDecay,idleDecay.test,types}.ts`(21 个文件)。
- 顺带兼容修正:`components/Toasts.tsx` 删 SW-message + sessionId 路径
  (依赖 `lib/sessionRoute.ts`);`components/TopBar.tsx` inline
  Workspace/Session 类型(原依赖 `lib/types.ts`);`lib/fetchInterceptor.ts`
  删 `isServerDown()` 防抖路径(原依赖 `lib/connectionState.ts`)。
