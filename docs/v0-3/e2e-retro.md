# V0.3 E2E Retro

> 范围:V0.3 全部 5 PR(M5.0-M5.4)post-ship 端到端验证 + ship 报告。
>
> base = `origin/main` 65de090(V0.3 PR #4 merge 终点);测试 baseline 起 631
> 终 738(+107 测试 / V0.3 自身贡献)。本 PR(PR #5)是 ship gate,把 retro
> 文档 + workspace.version bump + CLAUDE.md baseline 一波 ship。
>
> 方法:V0.2.2 retro 走 4-suite 并行 subagent;V0.3 全部 web crate 隔离 +
> tempdir,集成测试覆盖即可,**未起独立 e2e subagent**。retro 改成「PR-by-PR
> 验证矩阵 + 真浏览器 spot-check」。

---

## 1. 范围 + 时间线

V0.3 5 PR 顺序 ship,全 2026-05-10 当日完成(单人 dispatcher 5 worktree 串行
派工):

| PR # | milestone | 日期 / 时刻 | commit | 描述 |
|---|---|---|---|---|
| (kickoff) | docs-only | 2026-05-10 17:31 | `42561c5` | V0.3 PRD + dev-plan + README 初稿(`docs/v0-3/`) |
| **PR #1** | M5.0 scaffold + write helper promote | 2026-05-10 18:06 | `bb0ba16` | 新 crate `ccteam-web`;`ccteam_core::actions::*` 提取;`ccteam web` 子命令;`/health`;dep 图自检测试 |
| **PR #2** | M5.1 read-only dashboard | 2026-05-10 21:26 | `39b0890` | `GET /` + `/project/<slug>` + `/assets/{file}`;`ccteam_core::queries` 也 promote;outbox / state / events 渲染 + status badge |
| **PR #3** | M5.2 SSE event push + 截图 | 2026-05-10 21:51 | `a835d72` | `/sse/{all,project/<slug>}` + 单 watcher → broadcast(1024 cap);`/screenshot/<slug>.png` `spawn_blocking` 调 F38 |
| **PR #4** | M5.3 写动作 + token auth | 2026-05-10 22:19 | `65de090` | `POST /api/<slug>/{btw,inject_decision,pause,resume}`;`auth_layer` middleware(loopback bypass / 非 loopback 默认 token);URL shim cookie + CSRF 防御;LAN-RCE 5s grace |
| **PR #5** | M5.4 e2e + retro + ship gate | 2026-05-10 (本 PR) | (本 PR HEAD)| `tests/e2e_test.rs` 端到端 canary;`workspace.version 0.2.2 → 0.3.0`;CLAUDE.md baseline 回填;本 retro 落档;docs sweep |

总规模:~2.3 kLOC(scaffold + 模板 + handler + 测试)+ 107 新测试,覆盖
dep 图 / actions 模块单元 / dashboard / project 详情 / 静态资源 / SSE
wire format / 截图 endpoint / write actions / auth middleware / token 文件。

---

## 2. 端到端验证矩阵

| # | 场景 | 验证方法 | Verdict |
|---|---|---|---|
| E1 | Dashboard 渲染 | `dashboard_test.rs::dashboard_lists_one_project_slug` + 真浏览器 GET `/` | **PASS** |
| E2 | 空项目 root fallback | `dashboard_test.rs::dashboard_handles_empty_projects_root`("No projects") | **PASS** |
| E3 | Status badge 渲染 | `dashboard_test.rs::dashboard_renders_status_badge_html` + read-only(F35 复用)| **PASS** |
| E4 | Project 详情 — state/events/outbox | `project_test.rs::project_detail_renders_state_events_and_outbox` | **PASS** |
| E5 | Project 详情 — 未知 slug 404 | `project_test.rs::project_detail_returns_404_for_unknown_slug` | **PASS** |
| E6 | Project 详情 — 空数据 fallback | `project_test.rs::project_detail_handles_no_progress_or_outbox` | **PASS** |
| E7 | Vendored assets(`htmx.min.js` / `htmx-ext-sse.js` / `style.css`) | `assets_test.rs::*` 4 用例 + 真浏览器 view-source | **PASS** |
| E8 | SSE wire format(`event: progress` / `data: <json>`) | `sse_test.rs::sse_all_emits_synthetic_progress_event` | **PASS** |
| E9 | SSE per-slug filter | `sse_test.rs::sse_project_filters_to_matching_slug` | **PASS** |
| E10 | SSE end-to-end 文件 → 流(notify watcher path)| `sse_test.rs::sse_end_to_end_file_append_reaches_stream` | **PASS** |
| E11 | EventBus inert 优雅降级(watcher 启动失败)| `sse_test.rs::event_bus_inert_handles_no_publisher_gracefully` | **PASS** |
| E12 | Screenshot endpoint contract / 504 graceful degrade | `screenshot_test.rs::*` 3 用例 | **PASS** |
| E13 | POST /btw → inbox 写盘 + 303 redirect | `actions_test.rs::post_btw_writes_inbox_file_and_redirects` | **PASS** |
| E14 | POST /btw 长度校验(空 / 4001+ 拒)| `actions_test.rs::post_btw_rejects_empty_text` + `_overlong_text` | **PASS** |
| E15 | POST /inject_decision 写文件 | `actions_test.rs::post_inject_decision_writes_file_under_ccteam_dir` | **PASS** |
| E16 | POST /inject_decision path-traversal 防御 | `actions_test.rs::post_inject_decision_rejects_path_outside_ccteam_dir` + `_dotdot_traversal` | **PASS** |
| E17 | POST /pause → state.user_pause_pending=true | `actions_test.rs::post_pause_sets_user_pause_pending` | **PASS** |
| E18 | POST /resume → state.user_pause_pending=false | `actions_test.rs::post_resume_clears_user_pause_pending` | **PASS** |
| E19 | Auth gate — 非 loopback bind 默认开 token + Bearer header | `auth_test.rs::*`(8 用例)+ `web_token_file_test.rs::*`(5 用例)| **PASS** |
| E20 | `--no-auth` + 非 loopback → stderr 大字 LAN-RCE 警告 + 5s grace | `lib.rs::tests::*` + 手动 spawn 校验 stderr | **PASS** |
| E21 | URL shim cookie 流程(`?token=...` → 303 + HttpOnly cookie)| `auth_test.rs` 覆盖 | **PASS** |
| E22 | **跨层 happy path(本 PR e2e)**:GET / → GET /project/<slug> → SSE 收 1 event → POST /btw → inbox 落地 | `e2e_test.rs::v0_3_happy_path_dashboard_project_sse_and_btw` | **PASS** |

**矩阵 verdict**:22/22 PASS,V0.3 主路径全绿。

---

## 3. 跨浏览器 spot-check

V0.3 主线只 ship 桌面 web,M5.4 brief 要求至少 spot-check 主流浏览器。
本次实测覆盖范围:

| 浏览器 | 版本 | dashboard | project 详情 | SSE 实时 | 截图 | 写动作表单 |
|---|---|---|---|---|---|---|
| Chrome (Linux) | 130.x | ✓ | ✓ | ✓ | ✓ | ✓ |
| Firefox (Linux) | 132.x | ✓ | ✓ | ✓ | ✓ | ✓ |
| Safari (macOS) | — | (未验证)| (未验证)| (未验证)| (未验证)| (未验证)|

**说明**:dispatcher 工作环境无 macOS host,Safari 一栏 V0.3 ship gate 前
**未做** spot-check。已知 SSE / `EventSource` / `fetch` / `<form hx-post>`
全部 Safari 14+ 支持(2020 起),理论应 work,但 V0.3 不保证;若用户报问题
跟 V0.4 channel layer 一并评估。Linux 环境下 Chromium-derived(Edge / Brave
等)与 Chrome 同栈,未独立测。

**htmx + SSE ext** vendored 版本(`htmx.min.js` 2.0.4 + `htmx-ext-sse.js`
2.x)在 Chrome / Firefox 下 swap 行为符合 PRD §5.2.4 + interfaces.md §15.6;
表格 prepend + 滑动窗口截 200 行实测正常。

---

## 4. 已知 lingering issues(non-ship-blocking)

V0.3 ship 时已知边界 / 限制,**全部 by design**,不影响主路径,V0.4 评估或
保留 V0.3 行为:

| # | 限制 | 来源 / 设计依据 | 处理 |
|---|---|---|---|
| L1 | **SSE 不重放历史**:client connect 时 server 仅广播 connect-time 之后的事件,历史 200 条由 server-side template render 给出,不通过 SSE 重发 | `interfaces.md §15.6` watermark 启动语义("**不重放历史**...M5.4 retro 可评估") | V0.4 评估;若用户多次刷页对历史一致性有要求,加 `?since=<offset>` query | 
| L2 | **`ccteam_token` cookie 无 `Max-Age` / `Expires`**(`auth.rs:218-224`):浏览器 session-only cookie,关浏览器即丢 | 设计选择:无持久化 = 关浏览器即注销,降低 token 泄漏风口;dogfood 期 fine | V0.4 UX 评估,若用户嫌烦再加显式 `Max-Age` |
| L3 | **F38 截图在 tmux 高负载下可 504**:F38 graceful degrade 早 V0.2.2 ship,handler 翻成 504 + plain-text reason;**期望行为,不是 bug** | `tech-design.md §6.13 M5.2` + V0.2.2 F38 引入时同款 fallback | 维持 |
| L4 | **status badge 7-class 仅 read-only label**:即使 `silence_classifier` 分类为 `PostStopLimbo` / `SubagentRunaway`,web 层不调任何副作用 fn(re-inject / escalate)— orchestrator 走 F35 副作用路径 | `interfaces.md §15.2` 红线 + CLAUDE.md §三 read-only 红线 | 维持;V0.4 channel layer 后评估是否加「点击重新注入」按钮 |
| L5 | **per-project 写动作无 rate-limit**:`auth_layer` 仅 token 校验,无频次限制 | PRD §6.3 不做(V0.4 deferred)| V0.4 加 |
| L6 | **`/sse/all` + `/sse/project/<slug>` 不 multiplex**:advisor + PRD §5.2.3 决策"不复用一条连接,易丢追踪",一个 client 同时盯 dashboard + project 详情会开两条 EventSource | PRD §8.3 拓扑决策 | 维持;现代浏览器 HTTP/1.1 keep-alive ≥ 6 连接,正常用足够 |
| L7 | **Safari spot-check 缺**(详 §3) | dispatcher 无 macOS host | V0.4 或用户首报告时跟 |

**non-issues**(看似 lingering 但实际是 by design):

- "loopback 不需要 token" — PRD §6.2.4 + interfaces.md §15.8 显式策略
- "F38 截图同步阻塞 ~500ms" — `spawn_blocking` 已隔离,不阻塞 axum runtime
- "templates 内嵌 token 在 HTML attribute" — XSS tradeoff `auth.rs` 头注释已论证

---

## 5. V0.3 → V0.4 deferred 项

参见 `docs/v0-3/prd.md §10`(完整列表)。本 retro 浮上 1-2 高优先,V0.4
启动 PRD 时优先评估:

| # | 项 | V0.3 状态 | V0.4 优先级 |
|---|---|---|---|
| **D1** | **HTTPS / TLS termination** | V0.3 纯 HTTP,假设反代 | **P0**(用户自托管 LAN-only OK,但任何 internet-exposed 部署没 TLS = token 明文走 LAN)|
| **D2** | **OAuth(Google / GitHub)/ per-project ACL** | V0.3 单 token 整体权限 | **P1**(多人共用 dev box 时 token 共享 = 信任全开;OAuth 后才能 per-user 审计)|
| D3 | mobile-responsive layout | V0.3 桌面优先 | P2 |
| D4 | 隧道集成(ngrok / Cloudflare Tunnel) | V0.3 不做 | P2 |
| D5 | session-based cookie(替代 bearer) | V0.3 bearer + URL shim | P2(UX 评估)|
| D6 | 写动作 rate-limit | V0.3 无 | P2 |
| D7 | 多项目压测(>20 simul) | V0.3 ~3 项目验证 | P2 |
| D8 | SSE 历史回放(`?since=<offset>`) | V0.3 不重放(L1) | P2 |
| D9 | Safari spot-check / iOS 实测 | V0.3 缺(L7 / §3)| P2 |

V0.4 主线方向待用户确定(候选:Critic Agent / 多 audit 投票 / TUI / 跨项目
记忆官方接口深化等,详 `docs/tech-design.md §7` 里程碑路线图 + V0.2/README
deferred 列表)。

---

## 6. Verdict

| 维度 | 数 |
|---|---|
| **Ship-blocking issues** | **0** |
| **跨层 happy path(本 PR e2e)** | ✓ |
| **PR-by-PR 集成测试覆盖** | 107 新测试全 PASS;workspace 总 738/0 |
| **clippy 退步** | 0(4 pre-existing 仍 baseline) |
| **跨浏览器 spot-check** | Chrome 130 / Firefox 132 ✓;Safari 缺(L7) |
| **lingering issues** | 7 项,全 by design,V0.4 评估 |
| **V0.4 deferred** | 9 项(2 P0/P1 + 7 P2),详 §5 |

V0.3 **可 ship** —— web UI 主路径(dashboard / SSE / 截图 / 写动作 / token
auth)全部 PASS。第四接入层(继 terminal / MCP / filesystem 之后)落地,跟
现有三层并存,不替代任何已 ship 机制。

---

## 7. Numbers

- **PR 数**:5(M5.0-M5.4)+ 1 docs-only kickoff
- **耗时**:~5 hours wall-clock(2026-05-10 17:31 → 22:19,kickoff → PR #4
  merge;PR #5 ship gate 当日落,所有时间含 worktree subagent 派工 + dispatcher
  review + merge)
- **代码增量**:~2.3 kLOC + 107 测试(631 → 738)+ 1 新 crate `ccteam-web`
- **新模块**:`ccteam-core::actions`(M5.0)+ `ccteam-core::queries`(M5.1)
- **依赖图新增红线**:`cargo tree -p ccteam-web | grep ccteam-cli` = 0
  命中(`tests/dep_graph_test.rs` 自检)
- **F-finding 关闭**:F45(M5.0 write helper promote)整体 close;V0.3 未
  新增 F-finding(全部走 PR-内自合并)
- **Workspace version**:`0.2.2` → `0.3.0`(本 PR)
- **CLAUDE.md baseline**:`631` → `738`(本 PR)

---

## Changelog

- 2026-05-10:V0.3 ship gate retro 初版。base = `origin/main` `65de090`
  (V0.3 PR #4 merge 终点);测试 baseline 631 → 738(+107)。本 PR(PR #5)
  同 commit ship workspace.version bump + CLAUDE.md baseline + retro 文档 +
  docs sweep。
