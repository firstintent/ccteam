# bug.md — 实机使用滚动 bug 记录(持续追加,待批量一起修)

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
