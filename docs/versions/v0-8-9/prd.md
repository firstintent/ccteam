# v0.8.9 PRD — web UI 整体改造(统一 chat 风格 + 清理旧界面)

> **状态:DRAFT,讨论中**。**流程:doc-first** —— 本 PRD + HTML 原型由"需求收集 + 文档"产出;实现交 dev session,user review 后才动代码。本文作者**只收集需求 + 写文档,不开发**。
> **来源**:v0.8.7/v0.8.8 ship 后,user 要对 web 界面做整体 UI 改进(TG 2026-06-07,2381)。
> **原型**:同目录 **`prototype.html`**(自包含,浏览器直接开,演示目标统一 UI + IA)。
> **代码基线**:dev(v0.8.8 已落,version 0.8.8)。

---

## 〇、一句话

现在 SPA 是**两套分叉布局**:新的 chat 风格 `ChatConsole`(`/chat`、`/chat/s/:sid`)vs 旧的 operator 壳(`/` Dashboard、`/p/:slug`、`/sessions`、`/teams/*` + 新加的 Roles/Settings)。v0.8.9:**收敛成一个统一的 chat 风格 shell** —— 删掉旧界面里没用的(agent-team / orchestrator-era operator 视图),把有用的迁进 chat 壳,统一 UI 风格。

## 一、现状(已核实 routes + 数据源,代码为 SoT)

`App.tsx` 双 `<Routes>`:
- **新(chat 风格,裸渲染、自带 WorkspaceSidebar)**:`/chat`、`/chat/s/:sid` → `ChatConsole`(v0.8.7 W4 + v0.8.8 修)。
- **旧(operator 壳 = WorkspaceSidebar + TopBar)**:`/` Dashboard、`/p/:slug` ProjectDetail、`/p/:slug/s/:sid` SessionDetail、`/sessions` SessionsListPage、`/teams` + `/teams/:name`、`/roles` RolesPage、`/settings` SettingsPage。

| 页面 | 数据源 | 性质 | 处置 |
|---|---|---|---|
| `ChatConsole`(/chat) | gateway per-session(`/projects/{slug}/sessions`、SSE、PTY WS) | **新 chat 核心** | **KEEP** → 升级成全站统一 shell |
| `RolesPage`(/roles) | `/projects/{slug}/roles`(v0.8.8 F5) | 新、有用 | **MIGRATE** → 并进统一 shell 导航 |
| `SettingsPage`(/settings) | `/api/v1/config/im`(v0.8.8 F4) | 新、有用 | **MIGRATE** → 并进统一 shell 导航 |
| `Dashboard`(/) | `/api/v1/projects` | 旧 operator 首页;项目列表 chat sidebar 已有 | **REMOVE**(项目列表迁 sidebar;cost/health 看 §三 决策) |
| `SessionsListPage`(/sessions) | `/api/v1/sessions/active`(旧 claude-N 命名空间) | 旧;被 gateway per-session + sidebar 取代 | **REMOVE** |
| `SessionDetail`(/p/:slug/s/:sid) | claude-N operator + EventsLive/HarnessPanel/BTW/PauseResume | 旧 operator session 视图;被 ChatConsole(chat+终端)取代 | **REMOVE** |
| `ProjectDetail`(/p/:slug) | claude-N operator + CostSparkline/EventsLive/BTW/PauseResume | 旧 operator | **REMOVE**(有用片段见 §三) |
| `TeamsListPage`/`TeamDetailPage`(/teams*) | `/api/v1/teams` | **agent-team 模式(已弃,`teams/` 已删)** | **REMOVE**(死界面) |
| `WorkflowView` + ArtifactQueuePanel/FailureInspector | orchestrator/flow | ccteam-flow(deferred)operator | **REMOVE**(死;flow 未启用) |

> orchestrator-era 组件(CostSparkline/EventsLive/ArtifactQueuePanel/FailureInspector/HarnessPanel/BtwForm/PauseResumeButtons)只被上述旧页引用 → 旧页删则一并删(个别有用的按 §三 选择性迁移)。

## 二、目标 IA(一个统一 chat 风格 shell)

**一个 shell 承载全部**(删掉 `/chat` 与 operator 壳的分叉):
- **左 sidebar**:workspace → projects → sessions 树(每 session 显示 role + vendor 徽标 + 状态点,呼应 v0.8.8 的独立 session);底部导航 **Chat / Roles / Settings**;顶部 **＋新建**(session / 项目)。
- **顶 bar**:面包屑(项目 / role · vendor · sid)+ 连接状态 + 视图切换 **Chat | 终端** + Stop。
- **中区**:per-session **Chat**(transcript + composer + HITL 审批气泡)/ **终端**(xterm 实时 pane);切到 **Roles** = role 浏览(F5);切到 **Settings** = IM 配置(F4)。
- **风格**:深色 + amber 强调,一套设计 token,全站一致(落实 v0.8.8 §二 web UI 质量基线 + deferred「ChatConsole 配色统一」)。

详见 `prototype.html`(目标外观 + 四个视图态:Chat / 终端 / Roles / Settings)。

## 三、处置细节 + 开放问题

- **项目/session 列表**:确认 `WorkspaceSidebar` 覆盖 Dashboard/SessionsListPage 给的(项目列、session 列、状态)→ 覆盖则删旧页;缺啥补到 sidebar。
- **cost / 健康**:`CostSparkline` + 项目 STUCK/OK 是否值得留?**建议**:统一 shell 顶 bar 放一个轻量成本/状态指示(可选),其余 operator 面板删。**待 user 定**。
- **events / 进度**:per-session SSE 已喂 ChatConsole;是否还要全局 events 视图?**建议**不要(per-session 足够)。**待 user 定**。
- **移动端**:统一 shell 必须延续 v0.8.8 移动 hook(键盘/手势);窄屏 sidebar 收起。
- **开放问题**:① 是否保留任何 operator/调试视图(给运维)还是全砍?② cost 指示要不要?③ Roles 页要不要从只读升到可编辑 + catalog 在线装(v0.8.8 deferred)一起做?④ 死链清理(supervisor/outbound + `chat_history`/`send_input` 死工具,v0.8.8 deferred)是否并入本版?

## 四、设计系统(风格统一)

- **token 收敛**:深色 surface 阶梯 + amber 强调,一套间距/圆角/字号;清掉 ChatConsole 里散落的裸色值(deferred 项)。
- **四态**:每视图 loading/empty/error/success(v0.8.8 NFR)。
- **组件复用**:TopBar / WorkspaceSidebar / TerminalView / Toasts / KeyboardFab / MobileTerminalToolbar 留用;删随旧页失去引用的 operator 面板。
- **可达性 + 即时性**:键盘可达、SSE 保鲜(v0.8.8 NFR)。

## 五、验收

- 全站只剩一个 chat 风格 shell;`/teams*`、`/sessions`(active)、claude-N operator 视图删净(路由 + 页面 + 失引用组件);`cargo test` / vitest 不退;无死路由 / 无指向已删页的链接。
- Roles / Settings 进统一导航、可用;chat + 终端 + 切项目/session 一致顺手;移动端可用;UX 过关(NFR 清单走查)。
- 风格统一(无割裂裸色 / 杂控件)。

## 六、流程 & 变更记录

- doc-first → user review 原型 + PRD → scope 冻结 → dev-plan → dev session 实现(workflow,延续 v0.8.8 模式)。
- **2026-06-07 初版**:UI 改造范围(统一 chat shell + 删旧 + 迁有用 + 统一风格)+ 现状 keep/migrate/remove 表 + 目标 IA + `prototype.html`。
