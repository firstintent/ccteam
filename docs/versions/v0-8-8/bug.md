# bug.md — v0.8.8 实机 bug 记录(持续追加,归入 v0.8.8 批修)

> **归属 v0.8.8**:本 log 的修项收进 v0.8.8(scope + 设计见同目录 `prd.md`;BUG-3 由 prd.md F1「独立 session 模型」根治)。实现是另一个 dev session,**本文只记录 + 验证根因,不开发**。
> 用途:实机(IM + web)用着用着发现的 bug / gap,**持续追加**到这里,攒一批一起修。
> 格式(沿用 `docs/versions/v0-8-7/fix.md`):症状 / 根因(**file:line,代码为 SoT**)/ 影响 / 修法 / 验收 / 归属。
> 每条修完就地标 `✅ FIXED in <commit>`,不删(留追溯)。
> 红线照旧:no prompt injection、不 scrape pane、永不主动 kill 长 session、`cargo fmt --all` + clippy 0 + test baseline 不退。
>
> **当前基线 = `dev` HEAD `a6051cb`**。下列 file:line 已对 `a6051cb` 校准。

---

## BUG-1 · `ccteam project stop <slug>` 是 tmux-only,默认 rmux backend 下停不掉 session

**状态**:OPEN（2026-06-06 IM 讨论中发现）

**症状**:默认 backend = rmux 时,`ccteam project stop <slug>` 枚举/kill 的是 **tmux** session,而真实 chat session 在 ccteam 自托管的 **rmux** daemon 里 → 报告"停了 N 个"但实际一个没停(或停到 0 个),进程仍活着。

**根因(file:line 实证)= `project stop` 没跟 `ProcessBackend` 抽象走,硬编码 tmux**:
- **默认 backend 是 rmux**:`backend_kind_from_env()`(`crates/ccteam-harness/src/lib.rs:375-380`)对 unset / 空 / typo 一律回 `BackendKind::Rmux`;`default_backend()` doc 明确"operator opts out of rmux only with explicit `CCTEAM_MUX_BACKEND=tmux`"(`lib.rs:445-450`)。
- **但 `project stop` 走 tmux-only 路径**:`run_project_stop`(`crates/ccteam-cli/src/commands.rs:5348`)→ `stop_project_chat_sessions`(`commands.rs:5309`)用 `tmux_ops::list_sessions()` 枚举 + `TmuxSession::from_name(name).kill()` 杀(`commands.rs:5314-5331`)。`tmux_ops` 是**直接 shell `tmux`** 的模块(`tmux_ops.rs` header:`tmux new-session / has-session / kill-session`)。rmux session 在 `~/.ccteam/run/mux.sock` 的 rmux daemon,`tmux list-sessions` **根本看不到**。
- **对照(证明这是 `project stop` 单点遗漏,非系统性)**:`/role` 的 teardown `close_thread`(`crates/ccteam-harness/src/execution/claude_tui.rs:863-881`)走 `default_backend()`(rmux-aware)→ 能正确 kill rmux session。run_start 的 spawn/exists/kill 也都经 backend trait。唯独 `project stop` 绕过抽象直接 `tmux_ops`。

**影响**:rmux(= 默认)部署下 `ccteam project stop` 静默失效——用户以为停了,其实 claude 进程还在跑。会误导运维 + 浪费预算。

**修法**:`stop_project_chat_sessions` 改经 `default_backend()` 的 trait 方法枚举(`list_sessions()`)+ `kill()`,而非直接 `tmux_ops`。session 名仍用 `parse_chat_session_name` 过滤 `<slug>`。保留 `--dry-run`。注意 kill 后 session 要彻底消失(absent),使下次 spawn 走 fresh `--name` 而非 dead-pane `--resume`。

**验收**:`CCTEAM_MUX_BACKEND` unset(=rmux)时 `ccteam project stop <slug>` 真停掉该项目所有 chat session(rmux daemon 里消失、`session ls` 不再 ALIVE);`CCTEAM_MUX_BACKEND=tmux` 时仍工作;dry-run 列表正确。

**归属**:独立小修(harness backend 抽象已具备,只是 cli 这一处没用)。

---

## BUG-2 · web chat「新建项目」入口被 v0.8.7 W4 删除(回归)

**状态**:OPEN（2026-06-06 IM 讨论中发现）

**症状**:web chat 的「＋ 新建」菜单里不再有「＋ 新建项目…」选项;现在只能在**已有项目**里建 session。web 端**完全没有**建项目的路径,只能回退 CLI `ccteam init` / `ccteam project new`。

**根因(file:line + git 实证)= W4 per-session UI 重写时把建项目 UI 删了**:
- **原有(v0.8.3)**:`b10c794`("web chat new-project (with path) + project dropdown")+ `66737ca`("harden web chat new-project create")。旧 ChatConsole modal 有 `NEW_PROJECT = "__new"` 选项 + name/path 输入框 → 提交发 WS `/newproject <slug> <path>` → 确认后 `/cd` + `/new`(`pendingNewProjectRef` 串联)。
- **删除(v0.8.7 W4)**:`081f051`("W4 per-session web UI — {sid} history from turns.jsonl + ChatConsole per-session rewire")。`git show 081f051 -- crates/ccteam-web/web/src/pages/ChatConsole.tsx` 显示删掉了:`kind:"new"` 分支、`pendingNewProjectRef`、`sendText('/newproject ...')`、`NEW_PROJECT`/`newPath`/path 校验、以及 select 里的「＋ 新建项目…」选项与"还没有 session。点「＋ 新建」选 项目 / code agent / role 创建"文案。
- **现状**:新 `NewSessionModal`(`crates/ccteam-web/web/src/pages/ChatConsole.tsx:565`)只选已有项目;注释自承"Brand-new project scaffolding is an operator action (`ccteam init` / the dashboard) — out of scope"(`ChatConsole.tsx:561-564`)。**但 Dashboard 也没有建项目入口**;前端只 `GET /api/v1/projects`(`crates/ccteam-web/web/src/lib/dashboardApi.ts:43`),从不 POST create。

**后端能力仍在(丢的只是前端入口)**:
- REST `POST /api/v1/projects` → `handle_create_project`(`crates/ccteam-web/src/routes/projects.rs:60-80`,scaffold + register → 201,重复 409),已挂 OpenAPI(`routes/openapi.rs:143`)。
- gateway WS `/newproject <slug> <path>`(`crates/ccteam-im/src/gateway.rs:419, 911`)也仍在。

**影响**:web-only 用户无法新建项目,产品完整性回退(v0.8.3 有、v0.8.7 没了);v0.8.7 review 未抓到。

**修法**:把「＋ 新建项目…」加回 `NewSessionModal`(select 加 `"__new"` 选项 + name/path 两个 field + 绝对/`~` 路径校验)。**升级**:不走旧的 WS `/newproject` 文本命令,直接打 REST `POST /api/v1/projects`(干净、有 201/409 语义、跟 OpenAPI 一致);新增前端 `createProject()` helper 对应已存在的后端路由;建完自动选中新项目并起 session。

**验收**:web chat「＋ 新建」可在任意目录 scaffold 全新项目并在其中起 session;重复 slug → 409 友好提示;坏路径前端拦截;vitest 覆盖新选项分支。

**归属**:前端为主(后端 `POST /api/v1/projects` 已具备);标记为 v0.8.7 W4 回归。

---

## BUG-3 · per-session web UI:历史按 (project,role) 共享、无 session 维度 → 同角色会话串台

**状态**:OPEN（2026-06-06 用户报告,且明确"还是没解决"）

**症状**:per-session UI 的**壳**是独立的(每 sid 一个视图/路由),但点进去每个会话显示的**聊天历史是串的**——共享同一 `(project, role)` 的会话能看到彼此(及同角色 IM 聊天)的全部历史。

**根因(file:line 实证)= 持久层无 session 维度,历史按 `(project, role)` 存,与 per-sid UI 不匹配**:
- **LIVE 侧是对的(已隔离)**:`useSessionEvents(sid)`(`crates/ccteam-web/web/src/hooks/useSessionEvents.ts`)按 sid 订阅 `/sessions/{sid}/events`、切 sid `setEvents([])`、不跨 sid 合并;前端 transcript 也是 **per-sid localStorage**(`loadRows(sid)`/`saveRows(sid)`,`ChatConsole.tsx:117/157`)。
- **HISTORY 侧串台(根因)**:`GET /sessions/{sid}`(`crates/ccteam-web/src/routes/sessions_api.rs:261 handle_session_history`)把 sid resolve 成 `(role, project_dir)` 后读**整份** `<project_dir>/.ccteam/chat/<role>/turns.jsonl`(`collect_session_turns`→`read_all_turns`,`sessions_api.rs:277/289`),**无任何 sid 过滤**。
- **数据模型本身没有 session id**:`TurnRecord`(`crates/ccteam-harness/src/execution/turns_mirror.rs:39-47`)只有 `turn_id/ts/vendor/role`,**无 session id**;turns.jsonl 路径按 `(project, role)`(`turns_jsonl_path` `turns_mirror.rs:79`)。gateway `s{n}` 是内存计数器(daemon 重启重置)、**从不写进 mirror**。
- **⇒ 串台机制**:前端一进会话,`getHistory(sid)` 一回来就用整份 per-role 历史 **REPLACE** 掉 transcript(`ChatConsole.tsx:123-124`)。任何共享 `(project, role)` 的会话——多次新建/stop 后重建、daemon 重启换 sid、**web+IM 同角色**——都被 seed 成同一份全量历史。per-sid localStorage 只在历史返回前给一瞬"独立"的假象。

**深层张力(这是为什么必须先决策、不能闷头修)**:per-role 一份历史其实**与架构 keystone `session = role` + dedup「单 (project,role) pane」+ 红线「chat 复用 context 是 feature」一致**。W4 的 per-sid UI 却暗示"多个独立会话",但持久层没给 session 一个身份。所以这不是"代码写错",是**模型缺一个 session 维度** vs **keystone 是 role**——两者要选一个对齐。

**修法(设计分叉,二选一)**:
- **A · 真·独立 session**:给会话一个**持久** session id(非内存 `s{n}`;建会话时 mint 落地)→ 写进 `TurnRecord` + 历史端点按它过滤;并允许同 role 多 pane。**破** 当前 dedup invariant + `session = role` keystone。大改。**与 ENH-1(roleless)同向**——roleless 会话无 role 可作 turns key,本就需要 session 身份。
- **B · 对齐 `session = role`**:接受"一个 `(project, role)` = 一个会话",rail 每 role 一条,历史 = 该 role 全量(= 故意的连续性)。小改(UI 诚实化:别再暗示多个独立 sid 会话),跟红线一致。

**✅ 方向已定 = A(用户澄清 2026-06-06,TG 2359)**:用户明确「聊天独立 = 跟 Claude Code 原生 session 一样,终端起多个会话互不串台,同一个 role 开两个也各聊各的」。⇒ 走 **A**(session 一等实体 + 持久 id + turns 按 session 存 + 去 dedup + role 降为 session 属性),**改 `session = role` keystone**;B 弃。仍待查:**不同 role** 之间若也串(现模型各自 turns.jsonl、**不该**串)= 另一个更严重 bug。

**归属**:架构级改动 → **doc-first**(先写 design:session 一等实体 + 持久 id + turns 按 session 存 + 去 dedup + 与 resume-by-id/红线对齐 + 迁移),user review 后再动代码。已向 user 提议、待 go。BUG-4 与方向无关可先做。

---

## BUG-4 · 新建 session 弹窗 role 要手输、无项目真实 role 列表(用户不知道有哪些 role)

**状态**:OPEN（2026-06-06 用户报告）

**症状**:web「＋ 新建」的 role 是手输文本框,用户不知道项目里到底有哪些 `.claude/agents/*.md` role 名。

**根因(file:line)**:`NewSessionModal`(`crates/ccteam-web/web/src/pages/ChatConsole.tsx:565`)的 role 是 `<input>`(`:640-644`)+ `<datalist>` 取 `roleOptions`,而 `roleOptions` 来自**静态** `ROLE_SUGGESTIONS`(`chatDefaults.ts`,`ChatConsole.tsx:258-259`),**不是**项目真实 role。后端**已有**列表 API:`GET /api/v1/projects/{slug}/roles` → `[{role, description, model}]`(`crates/ccteam-web/src/routes/roles.rs:54-66`)。

**修法**:modal 拉 `GET /projects/{slug}/roles` 填下拉(展示 role + description),保留手输/自定义;"空 role"项见 ENH-1。

**归属**:前端为主(后端 roles API 已具备)。

---

## ENH-1 · 新建 session role 为空时,不加 `--agent`(roleless vanilla session)

**状态**:OPEN feature（2026-06-06 用户要求）

**诉求**:role 留空 → spawn **不带** `--agent`(= 裸 claude,brain 走项目 `CLAUDE.md` 原生自读;即 v0.8.6 之前的 no-`--agent` 模式);非空才 `--agent <role>`。

**现状/影响(file:line)**:
- 前端 `effectiveRole = role.trim() || DEFAULT_ROLE`(`ChatConsole.tsx:588`)→ 空被默认成 cto,永远非空。
- `spec_for_new`(`claude_tui.rs:385`)/ `spec_for_resume`(`:347`)**无条件** `push("--agent")` + role(`session = role` keystone)。
- FIX-2 给 create 路径加了"role 必须存在"校验(防 `--agent <未定义>` 死 pane)——roleless 要把校验改成"**仅非空** role 才校验"。
- turns.jsonl 按 role 命名(`turns_mirror.rs:79`)→ roleless 用什么 bot 名(`default`/`claude`?)**未定**,且回到 BUG-3 的 session 身份问题(roleless 同项目会互相串)。

**修法**:create 路径允许空 role;空 → argv **跳过** `--agent` + turns bot 名用约定占位(联动 BUG-3 的 session 身份决策);非空 → 现行 + FIX-2 校验。也跟 `session = role` keystone 有张力(roleless ≠ role,需红线说明这是显式例外)。

**归属**:跨层(harness argv + gateway create + web modal);**依赖 BUG-3 的 A/B 决策**(roleless 的 turns 身份)。

---

## BUG-5 · `session ls` 看不到 codex 会话(误报"registered, not running")+ 不显示 vendor

**状态**:OPEN（2026-06-06 用户报告,TG 2365）

**症状**:cto 用 **codex** 起的,网关 API `GET /api/v1/projects/ideas/sessions` 显示 `s7 / role=cto / vendor=codex / status=live / current=true`;但 CLI `ccteam session ls` 显示 `ideas cto ccteam-chat-ideas-cto **no** registered, not running`。同项目的 architect(claude)显示 `yes`。且 `session ls` **整列没有 vendor**。

**根因(file:line 实证)= `session ls` 的活性来自 process backend 名枚举,看不到 codex(app-server)会话;真 SoT 是 gateway session map**:
- `session ls`(`crates/ccteam-cli/src/commands.rs:1660-1734`)的 `alive = live_set.contains(name)`,`live_set` 来自 `list_chat_sessions(backend)`(`commands.rs:1661`)。
- `list_chat_sessions`(`crates/ccteam-harness/src/lib.rs:459-467`)= `backend.list_sessions()` 过滤 `CHAT_SESSION_PREFIX` —— **只枚举 process backend(tmux/rmux)里的会话**。
- **codex 走 app-server、不是 tmux/rmux pane** → 不在 `backend.list_sessions()` → `alive=false`;它又在 gateway state 里 tracked → NOTE 落到 `"registered, not running"`(`commands.rs:1703-1704`)= **误报**(claude/tmux 会话因为有 pane 故 `yes`,所以只 codex 中招)。
- 真活性 + vendor + sid 的 SoT = **gateway 内存 session map**(API `GET /projects/{slug}/sessions` 用的就是它,见 `sessions_api.rs` `session_views`)。`session ls` **没查它**。
- 另:`session ls` 的 `Row`(`commands.rs:1688-1694`)无 `vendor` 字段 → 表头(`:1722-1724` SLUG/ROLE/SESSION/ALIVE/NOTE)无 vendor 列。

**影响**:codex 起的会话在 CLI 一律误报"未运行";跨 vendor 运维判断错;无 vendor 列分不清会话用 claude 还是 codex。

**修法**:`session ls` 的活性 + vendor(+ sid)改从 **gateway session map** 取(像 API:经 daemon 查 / 复用 `session_views`),backend 名枚举只留作标 `orphan`(untracked live pane);加 **vendor 列**。**与 F3(status 加 vendor)同源、与 F1(gateway = session SoT)一致** —— 建议两处共用一个"列 session(含 vendor/status/sid)"的取数。

**验收**:codex 起的 cto 在 `session ls` 显示 `alive=yes` + `vendor=codex`;claude 会话不变;orphan 仍能标;`status` 同样每会话带 vendor。

**归属**:CLI(`session ls`)+ 跟 F3 `status` vendor 同批;依赖 gateway session 查询(`session_views`/API 已有)。属 prd.md **B4**。

---

## BUG-6 · web 终端(per-session PTY WS)一直断开重连 —— 路由没指向会话 pane + I/O 硬编码 tmux

**状态**:OPEN（2026-06-06 用户报告 + 截图 + 抓包,TG 2367/2368）

**症状**:web「终端」tab 打开后,WS `ws://<host>:7331/ws/ideas/s5/pty` 连上→立刻断→`[Disconnected, reconnecting in 1s... (1/7)]` 死循环(计数停在 1/7 = 每次连上即断)。客户端反复只发一个 `{"type":"resize","cols":149,"rows":50}` 就掉线。

**根因(file:line 实证)= 三处叠加**:
1. **per-session PTY 路由根本没解析 `sid`、退回项目级 pane(W4 遗留 TODO 没做)**:`handle_session_ws`(`crates/ccteam-web/src/routes/pty_ws.rs:99-119`)注释自承 "the per-session runtime registry is gone … Fall back to the project-level tmux session. TODO(V0.8.6 W5b/W5c): re-key this onto the new session record"(`:104-108`),直接用 `ProjectState.tmux_session`(`:113`)而非 `s5 → ccteam-chat-ideas-architect`。chat/session=role 下项目级 tmux_session 不是会话 pane(空 / `ccteam-ideas`,默认 rmux daemon 里不存在)→ `app.pty.subscribe(...)`(`:143`)失败/空 → `run` 返回 Err(`:126-133` 记 "relay loop exited with error")或 broadcast `Closed`(`:170-175`)→ WS 立刻关 → 前端重连 → 循环。
2. **输入/resize 硬编码 `TmuxBackend`**:`send_keys`(`pty_ws.rs:216-232`,`:228 TmuxBackend::new()`)+ `resize_window`(`:234-239`,`:236` 同)写死 tmux、**没走 `default_backend()`**。默认 rmux 时客户端的 resize/键击都打到不存在的 tmux 会话(resize 失败仅 warn、不直接断;但与 #1 叠加 → 终端整体不可用)。
3. **rmux 的流是行文本、不是裸 ANSI 字节**(=能不能"像本地终端"的核心):PtyRegistry 已改走 `terminal_from_env()`(rmux,`crates/ccteam-web/src/pty.rs:58`),但 rmux `subscribe` 把 pane 输出转成 `PaneLineItem::Line` 文本(`crates/ccteam-harness/src/rmux_backend.rs:17-20`),`with_ansi` 拿不到裸字节(W2b 已知 gap ~`rmux_backend.rs:36`)。xterm.js 要裸 ANSI/光标控制才能忠实渲染 TUI(如 claude 的 TUI)→ 即便修了 #1#2,rmux 路径"像本地终端"的渲染保真仍缺(需 rmux 裸字节流 = W2b,或用 tmux backend)。
- 另:模块 doc(`pty_ws.rs:5-27`)还在描述旧的"项目 pane + tmux pipe-pane"模型,stale。

**影响**:web 终端在默认(rmux)+ per-session 下完全不可用(连不上/秒断)。= v0.8.7 W4 per-session UI 留的 TODO(终端那一路没 re-key)+ rmux-default tmux 硬编码(同 BUG-1/BUG-5 一类)。

**修法**:① `handle_session_ws` 经 gateway 把 `sid → role/pane`(`chat_session_name(slug, role)`,像 API `session_resolve`)再 subscribe;② `send_keys`/`resize_window` 改 `default_backend()` 而非 `TmuxBackend::new()`;③ "像本地终端"的完整保真需 rmux 裸字节订阅(W2b)或显式 tmux backend —— 作为终端体验子项单列。与 **F1**(per-session pane 解析)同源。

**用户问题"最终能像本地终端一样操作吗?"**:能 —— 设计本就是裸字节双向中继(server→client pane 字节 + client→server send-keys + resize)= 全交互终端镜像;修好 #1#2 即可操作,#3 决定 TUI 渲染保真度。

**验收**:打开 web 终端稳定连住(无 1s 重连循环),像本地一样输入/看输出/resize,目标 = 当前 sid 的 pane(非项目级、不串别会话);codex 会话也适用(经 gateway 解析,不依赖 tmux 名)。

**归属**:`ccteam-web` pty_ws + pty registry;依赖 gateway session 解析(F1);rmux 裸字节(W2b)= 渲染保真子项。属 prd.md **B5**。
