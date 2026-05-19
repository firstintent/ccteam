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
`0.3.1` → `0.3.2`(F59 bump)。Rust 测试 baseline 已验证为
`866/0`(`cargo test --workspace --locked`,F59 follow-up 后)。

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

## F59 verification follow-up(2026-05-14)

F59 ship 后的验证缺口已补齐:

- Rust workspace:`cargo build --workspace --locked` + `cargo test
  --workspace --locked` 通过;测试 baseline `866/0`。
- SPA toolchain:`npm install --no-audit --no-fund`、`npm run build`、
  `npm run lint`、`npm run test:unit`、`npm test` 通过。
- `Cargo.lock` 同步 V0.3.2 version bump + `rust-embed` / `mime_guess`
  / `serde_urlencoded`;`web/package-lock.json` 落档以符合 `.npmrc`
  的 pin exact versions 约束。
- Legacy route tests 已按 F59 合约改为 `301 Location: /app/...`;
  data/body 覆盖转到 F52 JSON API + V0.3.2 Playwright smoke。
- `/app/` 精确入口已补上,避免 `/` 301 后落 404。
- 裸 `POST /api/<slug>/{pause,resume}` 无 `Content-Type` 的 form
  兼容路径已恢复 303 合约;SPA JSON `{}` 路径保持 `{"ok":true}`。
- Session detail 已挂载 `TerminalView`,覆盖 F57 xterm/WS PTY 用户面。
- 默认 Playwright gate 收敛为 ccteam-owned `v032-spa.spec.ts`;
  AoE fork specs 暂留作参考,后续 surface promoted 时再逐项启用。

仍 deferred 到 V0.3.3/V0.4 的项见上节"V0.3.3 / V0.4 deferred 项"。

## 跟其他文档关系

- 主仓 `CLAUDE.md` §一 baseline 已由 F59 follow-up 回填(0.3.1 → 0.3.2,
  V0.3.2 milestone 行;测试 baseline `866/0`);§三
  红线 V0.3.2 不动(progress.jsonl SoT / 永不主动 kill / ccteam-core
  无 team 名字面量)。
- `docs/interfaces.md` §16 — F52 JSON API endpoints + WS PTY
  subprotocol(`ccteam-pty.v1`),如每 PR 同步则已落档。
- `docs/dev-coupling-audit.md` F52-F59 — F59 标记 close 状态。
- `docs/versions/v0-3-1/README.md` — V0.3.2 erratum 行已加(V0.3.3 slip 提示)。
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
