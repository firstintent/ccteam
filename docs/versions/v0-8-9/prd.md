# v0.8.9 PRD — web UI 整体改造(统一 chat 风格 + 清理旧界面)

> **状态:DRAFT,讨论中**。**流程:doc-first** —— 本 PRD + HTML 原型由"需求收集 + 文档"产出;实现交 dev session,user review 后才动代码。本文作者**只收集需求 + 写文档,不开发**。
> **来源**:v0.8.7/v0.8.8 ship 后,user 要对 web 界面做整体 UI 改进(TG 2026-06-07,2381)。
> **原型**:同目录 **`prototype.html`**(自包含,浏览器直接开,演示目标统一 UI + IA)。
> **代码基线**:dev(v0.8.8 已落,version 0.8.8)。

---

## 〇、一句话

现在 SPA 是**两套分叉布局**:新的 chat 风格 `ChatConsole`(`/chat`、`/chat/s/:sid`)vs 旧的 operator 壳(`/` Dashboard、`/p/:slug`、`/sessions`、`/teams/*` + 新加的 Roles/Settings)。v0.8.9:**收敛成一个统一的 chat 风格 shell** —— 删掉旧界面里没用的(agent-team / orchestrator-era operator 视图),把有用的迁进 chat 壳,统一 UI 风格。

## ★ 架构决策(2026-06-07,TG 2385):插件市场 + ccteam-hub + repo 零提示词内容

> v0.8.9 UI 方向 **user 已批,继续**。在此之上追加一条**架构级决策**(影响 F5 + C1,且立新红线):

1. **roles / skills / workflows → 一个「插件市场」**:F5 的 role 浏览页升级成**插件市场 UI** —— 浏览 + 一键装 role/agent、skill、workflow;**agency-agents 等开源可直接接入**(在线浏览 + 装)。**逻辑 / 开源 ingestion / UI 交互 详见同目录 [`marketplace-design.md`](marketplace-design.md)**(ccteam↔hub↔项目 三角 + ingestion 进 hub + 浏览→预览→装到项目 流程)。
2. **ccteam 仓库内不再包含任何「提示词类型」插件**:subagent/role agents、skills、workflow 的**内容**一律不进 ccteam repo。**ccteam = 纯引擎**(新红线;CLAUDE.md §三 实现时补)。
3. **`ccteam-hub` 仓库**:自建 + 开源插件(skills / agents·role / workflow)都放 hub;ccteam 从 hub(+ 开源源如 agency-agents)拉取 + 安装。**路径(TG 2386)= `/home/ubuntu/workplace/ccteam/ccteam-hub`**(已 `git init`、目前仅 stub README;嵌在 ccteam 工作目录内、有独立 `.git` + remote `git@github.com:firstintent/ccteam-hub.git`;ccteam 的 `.gitignore` **已含** `ccteam-hub/`(v0.8.8 C1 一并加的)→ 不会误纳入 ccteam)。hub 布局(已 scaffold,TG 2389):`agents/`(role/subagent .md)、`skills/`、`workflows/`、`index.json`(市场目录索引)。

**要清出 ccteam repo 的现存提示词内容(已核实清单)**:
- ~~`crates/ccteam-core/src/templates/cto_role.md`~~ **保留**(TG 2388:cto 先留作**唯一 bootstrap 例外**,`cto_role.md` 留 repo = 引擎自带默认管家;其余照清)
- `crates/ccteam-core/src/templates/meta_agent_role.md` + `workflow.agent-team.yaml` + `squad_roster.rs`(meta-agent / agent-team 模板·花名册,legacy)
- 根 `agents/`(__lead/explorer)+ `workflows/`(dev-flow/qa-autoloop)← **v0.8.8 C1 漏删的两个,现在必删**
- `crates/ccteam-core/src/templates/agency_agents_catalog.json`(catalog 索引)← **移出**(TG 2391:连索引也搬 hub,由 hub `index.json` + ingestion 承载;ccteam 不留任何市场目录)

**开放问题(要 user 拍)**:
- **① 种 cto 怎么办** — **✅ 已定(TG 2388):(b) cto 先留作唯一 bootstrap 例外** —— `cto_role.md` 留 repo(引擎自带默认管家、算引擎配置非插件),其余 prompt 内容照清。"先" = 暂定,后续可重审是否也搬 hub。
- **② scope 切分** — **✅ 已定(TG 2391):全压进 v0.8.9**。即 v0.8.9 = UI 整体改造 + F5 升级成市场 UI + 从 repo 清出提示词内容 + **ccteam-hub 填充(自建迁入)+ agency-agents 等开源 ingestion 管线**。(注:范围变大 → dev-plan 要好好分阶段。)
- **③ catalog 索引** — **✅ 已定(TG 2391):连索引也搬 hub**。`agency_agents_catalog.json` 移出 ccteam repo → 由 hub 的 `index.json`(+ ingestion 生成的条目)承载;ccteam repo **不留任何市场目录**,市场列举全读 hub。

---

## ★★ 决策锁定(TG 2399「确认」)

需求讨论收口,以下**全部确认**,据此出 dev-prompt:
- **IA**:统一 chat shell;session 列表 = 聊天导航(无「Chat」菜单);底部 = 插件市场 / Status / Settings;顶 bar cost pill;轻量 Status 视图。
- **插件市场**:按 `marketplace-design.md`(ccteam↔hub↔项目 三角;开源走 ingestion 进 hub;浏览→正文预览→装到项目)。
- **scope**:全压本版(UI 改造 + 市场 + agency-agents ingestion + 清 prompt 内容 + 填 hub + 死链清理 + **rmux 0.3→0.5 裸字节终端**,详见 [`rmux-update.md`])。
- **catalog**:索引搬 hub,ccteam 不留市场目录。
- **cto**:留作唯一 bootstrap 例外(`cto_role.md` 留 engine)。
- **Roles→市场**:本版**只读浏览 + 安装**(在线编辑后续)。
- **死链清理**:并入本版(supervisor/outbound + `chat_history`/`send_input` 死工具)。
- **运维视图**:不单留 operator UI → 轻量 Status 进 shell。
- **市场待定项 → 取默认**:取内容 = github raw + 本地缓存;ingestion = hub 侧 GitHub Action(+ 本地命令);安装 = **项目级**;更新 = 手动。

→ dev-prompt 见同目录 **`dev-prompt.md`**。

---

## 一、现状(已核实 routes + 数据源,代码为 SoT)

`App.tsx` 双 `<Routes>`:
- **新(chat 风格,裸渲染、自带 WorkspaceSidebar)**:`/chat`、`/chat/s/:sid` → `ChatConsole`(v0.8.7 W4 + v0.8.8 修)。
- **旧(operator 壳 = WorkspaceSidebar + TopBar)**:`/` Dashboard、`/p/:slug` ProjectDetail、`/p/:slug/s/:sid` SessionDetail、`/sessions` SessionsListPage、`/teams` + `/teams/:name`、`/roles` RolesPage、`/settings` SettingsPage。

| 页面 | 数据源 | 性质 | 处置 |
|---|---|---|---|
| `ChatConsole`(/chat) | gateway per-session(`/projects/{slug}/sessions`、SSE、PTY WS) | **新 chat 核心** | **KEEP** → 升级成全站统一 shell |
| `RolesPage`(/roles) | `/projects/{slug}/roles`(v0.8.8 F5) | 新、有用 | **MIGRATE + 升级** → 统一导航里的**插件市场**(见 ★ 架构决策:浏览+装 role/skill/workflow,接 ccteam-hub + agency-agents) |
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
- **左 sidebar = 聊天导航**:workspace → projects → sessions 树(每 session 显示 role + vendor 徽标 + 状态点)。**点某个 session = 进入它的聊天**(每 session 独立对话)—— 所以**没有单独的「Chat」菜单**(TG 2394 修正)。顶部 **＋新建**(session / 项目)。底部全局页导航 = **插件市场 / Status / Settings**(3 个,不含 Chat)。
  - **插件市场**(= 原 Roles **升级成市场浏览器**,TG 2394):浏览 + 一键装 role/agent · skill · workflow,来源 ccteam-hub + agency-agents 等开源;不再有独立的"Roles"概念,role 只是市场里的一个类目。
- **顶 bar**:面包屑(项目 / role · vendor · sid)+ 连接状态 + 视图切换 **Chat | 终端** + Stop。
- **中区**:per-session **Chat**(transcript + composer + HITL 审批气泡)/ **终端**(xterm 实时 pane);切到 **市场(Plugins/Roles)** = 插件市场(浏览 + 装 role/skill/workflow,接 ccteam-hub + 开源,见 ★ 架构决策);切到 **Settings** = IM 配置(F4)。
- **风格**:深色 + amber 强调,一套设计 token,全站一致(落实 v0.8.8 §二 web UI 质量基线 + deferred「ChatConsole 配色统一」)。

详见 `prototype.html`(目标外观 + 四个视图态:Chat / 终端 / Roles / Settings)。

## 三、处置细节 + 开放问题

- **项目/session 列表**:确认 `WorkspaceSidebar` 覆盖 Dashboard/SessionsListPage 给的(项目列、session 列、状态)→ 覆盖则删旧页;缺啥补到 sidebar。
- **cost / 健康**:**建议(已写进下方 UI 决策②)**:顶 bar 放紧凑 cost pill;daemon 健康 + session 状态 + last-event 归入轻量 Status 面板(UI 决策①);旧 operator 面板(CostSparkline/EventsLive/HarnessPanel…)删。
- **events / 进度**:per-session SSE 已喂 ChatConsole;是否还要全局 events 视图?**建议**不要(per-session 足够)。**待 user 定**。
- **移动端**:统一 shell 必须延续 v0.8.8 移动 hook(键盘/手势);窄屏 sidebar 收起。
- **UI 决策(user 委托我建议,TG 2391;`prototype.html` 已加演示)**:
  - **① operator/运维视图 → 建议:不留独立 operator UI,把有用的运维信号并进统一 shell**。sidebar 已有 项目→会话 + 状态点(live/idle)+ vendor 徽标 = "什么在跑" 基本覆盖;再加一个**轻量 Status 面板/页**(daemon 健康 + 各 session 状态 + 今日成本 + last-event 龄)替代旧 Dashboard 的有用片段。砍掉深 operator 视图(SessionDetail 的 HarnessPanel/BTW/PauseResume、claude-N 命名空间、teams)。理由:再留一套 operator skin = 正是本版要消灭的分叉。
  - **② 顶 bar cost 指示 → 建议:要,一个紧凑 cost pill**(今日 $X.XX / 24h 预算,接近上限变色),点开看 per-vendor 拆分。理由:云端 24/7 + `budgets.*.max_cost_usd_per_24h` 自停红线,glanceable 花费对运营有用且便宜;复用现有 `cost-budget.json` / cost rollup。
- **其余开放**:③ **Roles 可编辑 + catalog 在线装** —— catalog 在线装已**在范围内**(F5 = 市场);web **在线编辑 role 建议本版只读浏览 + 安装**,编辑留后续。④ **死链清理**(supervisor/outbound + `chat_history`/`send_input` 死工具)—— **建议并入本版**(已大改 + strip,顺手清),你定。

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
- **2026-06-07 +★ 架构决策(TG 2385/2386)**:user 批 UI 方向、继续;追加 = roles/skills/workflows 做成**插件市场**(F5 升级,接 agency-agents 等开源)+ **ccteam repo 零提示词内容**(新红线,清单含 cto_role.md/meta_agent_role.md/workflow.agent-team.yaml/squad_roster + 根 agents//workflows/ + catalog.json)+ 新 **ccteam-hub** 仓库(路径 `…/ccteam/ccteam-hub`,已 stub)。3 个开放问题待 user 拍(种 cto 来源 / v0.8.9 scope 切分 / catalog 索引去留)。
- **2026-06-07 cto 例外 + hub scaffold**:①已定 —— cto 先留作唯一 bootstrap 例外(`cto_role.md` 留 repo);`ccteam-hub` 已 scaffold(README + `agents/`/`skills/`/`workflows/` + `index.json`,提交到 hub 仓库,TG 2388/2389)。
- **2026-06-07 scope + catalog + UI 建议**(TG 2391):②**全压进 v0.8.9**(含 hub 填充 + agency-agents ingestion 管线);③**连索引也搬 hub**(catalog 移出 ccteam,市场列举全读 hub)。UI 决策(我建议):operator 视图不单留 → 轻量 Status 面板进统一 shell;顶 bar 加紧凑 cost pill。`prototype.html` 已加 cost pill + Status 视图演示。Roles 本版只读+装、死链清理建议并入(待 user 终拍)。**dev-prompt 暂不出**(user:需求讨论清楚再出)。
- **2026-06-07 IA 修正(TG 2394)**:① 去掉「Chat」导航项 —— **左侧点 session 即进聊天**(session 列表 = 聊天导航);② 「Roles」**砍掉、升级成「插件市场」浏览器**(role 是市场里一个类目,不再独立);③ 底部全局页 = 插件市场 / Status / Settings。`prototype.html` 重做(市场视图 = 类目 Agents/Skills/Workflows + 来源 builtin/agency-agents + 安装/已装;去掉 Chat 菜单;session-点击=聊天 的模式)。
- **2026-06-07 决策锁定 + dev-prompt(TG 2399「确认」)**:需求收口(★★ 决策锁定段)→ 写 `dev-prompt.md`(workflow + opus、dev 直推不 PR、跨 ccteam + ccteam-hub 两仓、5 阶段:清 prompt 内容+死链 → hub 填充+ingestion → 市场后端 → web UI 改造 → 文档+版本)。出 dev-prompt,待 user launch。
- **2026-06-07 +rmux 升级并入(TG 2401)**:user 加了 `rmux-update.md`(rmux 0.3→0.5 + 裸字节终端,根治 v0.8.8 web 终端 bug4/bug6 = W2b 缺口)。已并进 dev-prompt 作 **Phase 3**(dep bump 先行 → subscribe/capture 改裸字节;⚠ 守 pattern-matching 行流链;可与 hub/市场并行、须在 web UI 终端前)→ 原 web UI / 文档顺延为 Phase 4 / 5。
