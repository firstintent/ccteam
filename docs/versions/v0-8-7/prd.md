# v0.8.7 PRD —— 让团队活起来（cto 调度 + HITL）+ 补 v0.8.6 缺口

> **Doc-first**。基于 v0.8.6（已 ship `main`；review fix-round → `dev`）。**v0.8.7 基线 = `dev`（fix-round 合并后）**。协议以代码为 SoT；file:line 为 grounding 期结论，落地 Wave 内复核。讨论来源：2026-06-06 TG（结合 v0.8.6 review）。

---

## 0. Scope & 模型

v0.8.6 搭好骨架（session=role + cto + 标准资源 API + 清理）。v0.8.7 让它**活起来**（cto 真派活 + 可监督）并补 review 推后的前端。**4 items：**
- **A · cto 调度（B 档）** —— cto spawn work-role / 派活 / 收结果。
- **B · HITL 批准（per-session 可选）** —— 创建 session 时默认 `--skip-permissions`；**可选 HITL**（`PermissionRequest` hook → IM 批准）。
- **C · role picker/import** —— 从 agency-agents 浏览 + 选 → 装进 `.claude/agents/`。
- **D · per-session web UI** —— 补 v0.8.6 §5d 推后的前端（**依赖 fix-round SSE 修复**）。
- **E · OpenAPI 自动文档（utoipa-axum）** —— 一个 URL 看 `/api/v1` 全部端点的交互式文档。

**非目标 → v0.8.8**：多 harness（codex 全量 / gemini-cli / grok-cli）+ provider 轴；push-back-as-turn（cto 子结果直灌 context）。

**红线**：no prompt injection（`--agent`）/ `progress.jsonl` 是 state SoT / 永不**主动** kill（dispatch/stop = 用户显式命令）/ resume-by-id / ccteam 不生成/桥接项目 `CLAUDE.md`·`AGENTS.md`。

---

## 1. Item A —— cto 调度（B 档）

**现状**：v0.8.6 cto 只推荐（A 档）。W5b 已 ship **`pub` Gateway 方法** `create_session_api` / `submit_to_sid` / `session_views` / `stop_session`（gateway.rs:1655-1755）= spawn/dispatch/list/stop 表面**已在**；缺的只是"给 cto 用 + 把结果收回"。

**决策**：
- **DA.1 新 MCP 工具** `session_spawn{project?,role,vendor?}` / `session_dispatch{sid,task}` / `session_collect{sid,since?}`（+可选 `session_list`/`session_stop`）：新 `mcp_session_tools.rs` + group 前缀 `session_`（mirror `chat_`/`advise_`）；并入 `tool_definitions()`；`STUB_TOOLS` + `doctor --verify-mcp` 同步。
- **DA.2 wire**：把 `shared_gateway`（`Arc<Mutex<Gateway>>`，composition root main.rs:1727）传进 `serve_mcp_socket`（main.rs:1851）+ `handle_mcp_socket_connection`；spawn/dispatch 走 **`chat_send_file` 的 forwarder 模式**（stdio → mcp.sock → daemon handler，ambient `CCTEAM_CHAT_{SLUG,ROLE}`）；`collect` 可 stdio 侧直接 tail 子 session `turns.jsonl`（reuse `read_all_turns`）。
- **DA.3 权限分层**：① `cto_role.md` 加 `tools:` 行授予 session_*（work-role 模板不给 → Claude allow-list 强制）；② daemon handler **硬门**：caller role（ambient `CCTEAM_CHAT_ROLE`）== `cto`（或可配特权集）否则 isError —— 双保险。
- **DA.4 结果回收 = polled collect（MVP）**：子 session 自跑，answer 已 mirror 到其 `turns.jsonl`；cto 调 `session_collect` 读回。push-back-as-turn（子结果直灌 cto context，需新 GatewayEvent 路由模式）= **v0.8.8**。
- **红线**：这是 **gateway session map**，**不碰** deprecated registry/supervisor（`chat_*` 工具那套）—— 别混。dispatch/stop = cto 显式命令，不违永不主动 kill。

**验收**：cto 能 spawn 一个 work-role session + 派 task + collect 结果；work-role 调 `session_spawn` 被拒（权限门）；MCP count + `doctor --verify-mcp` 0 drift；deterministic fake 测试。

---

## 2. Item B —— HITL 批准（per-session 可选；默认 skip）

**现状**：v0.8.6 所有 session 走 `--skip-permissions`（无门）。HITL 机制经源码核实（`references/claude-code` 2.2.1，**需真 binary 复核**）：
- **`PermissionRequest` hook**（比 PreToolUse 新）：Claude **先评 allowlist**，hook **只在 would-prompt（ask）触发**，allow 的直接跑、不进 hook → 白嫖 Claude 审批、**零 settings 解析**、**交互模式可用**、返回 `{behavior:allow|deny}`。
- PreToolUse hook payload **无 allow-status**（只全局 permission_mode）+ 对每调用触发 → **不用它**（否则得自己 parse allowlist，脆）。
- `--permission-prompt-tool` 语义对但 `--print` headless-only → 不适合 tmux 交互。

**决策**：
- **DB.1 per-session permission mode**：创建 session 取 `permission_mode: skip（默认）| hitl`。穿过 IM `/new`（如 `/new claude cto hitl`）、API `POST /sessions`（body 字段）、cto `session_spawn`（param）、gateway `start_session`/`create_session_api`。
- **DB.2 spawn**：`hitl` session → spawn **不带 `--skip-permissions`** + 装 `PermissionRequest` hook（+ 确保 settings 有 `ask` 默认让非 allowlist 进 hook）；`skip`（默认）→ 现状 `--skip-permissions` **不变**。
- **DB.3 hook handler**：新 `ccteam internal hook permission-request` —— 收审批 payload（tool_name/tool_input），**复用 v0.8.5 D6 的 ChoicePrompt + pending registry** 把"approve/deny"做成 IM 可点，block 等用户（≤ ~600s/10min），返回 `{behavior}`。
- **DB.4 IM 审批 UX**：渲染"session sX(role) 要跑：`<tool> <摘要>` [同意][拒绝]"；点 → resolve pending → hook 返回。复用 pending.rs + ChoicePrompt（v0.8.5 已有）。
- **DB.5 allowlist**：HITL 白嫖 Claude allowlist（`permissions.allow` 自动跑）；session 的 `settings.local.json` 给合理 `ask` 默认（或靠 default-ask-非-allow）。
- ⚠ **风险**：`PermissionRequest` 较新/不在老 docs → **W2 内先 smoke-gate**：真 claude binary 在 plain `--agent --name` 交互下确实触发 `PermissionRequest` 才继续；不触发就**停下报告**（HITL 需换路）。

**验收**：创建 `hitl` session → 非 allowlist 工具调用弹到 IM、点同意才跑、拒绝则 deny；allowlist 内的不弹（自动跑）；默认 `skip` session 行为不变；smoke 实证 `PermissionRequest` 触发。

---

## 3. Item C —— role picker/import

**现状**：role 库 = `.claude/agents/<role>.md`；`write_role` + `validate_bot_name`（admin_actions.rs:85/188）= create-or-replace primitive（import sink）；`/api/v1` 有 GET roles + GET/PUT {role}；**无 catalog/browse/add**（v0.8.6 = 手动丢 .md）。agency-agents = Claude-native .md+frontmatter，**零转换**（只 sanitize stem 到 `[a-z0-9_-]`），~209 个 division 子目录，无 root manifest。

**决策**：
- **DC.1 vendored catalog manifest** `ccteam-core/src/templates/agency_agents_catalog.json`（`include_str!`，~209 entries `{id,division,display_name,description,raw_path}`，无 body）+ `role_catalog.rs`（catalog/search/find）。刷新 = chore（`gh api git/trees` sweep），同 `workflow_templates/` 维护类。
- **DC.2 import primitive** `import_role_from_catalog(project_dir, catalog_id, target?)`：`reqwest` fetch `raw.githubusercontent.com/.../{raw_path}` → sanitize stem → `write_role`（零 frontmatter 转换）。core 加 `reqwest` dep（async + base_url override 保 test 确定性，mirror onboarding `telegram_setup_with_base`）。`--force` 守已存在。
- **DC.3 surfaces（MVP=CLI）**：`ccteam role search <q>`（本地 manifest、无网）/ `ccteam role add <id> [--as <role>] [--project|cwd]`（import + 提示 `/role <role>`）/ `ccteam role list`（wrap `list_roles`）。web 跟进：GET `/api/v1/catalog/roles?q=` + POST `/projects/{slug}/roles/import`。IM 可选 `/role-search`/`/role-add`（配 ChoicePrompt 两步 picker）。

**验收**：`ccteam role search` 列 catalog；`role add <id>` 装进 `.claude/agents/` + `/role` 可用；离线 browse + 在线 import（honest 网络错误）；`validate_bot_name` sanitize 大写/空格。

---

## 4. Item D —— per-session web UI（补 v0.8.6 §5d）

**现状**：`/api/v1` 有 `sessions/{sid}/{events SSE, turn, stop}` + GET `{sid}`；但 SPA 没接 —— `ChatConsole.tsx`（`crates/ccteam-web/web/`）一个全局 WS（`web-chat`）+ 单 flat localStorage transcript **混所有 session**（设计如此）；两套 sid 命名（gateway `s{n}` vs legacy `claude-N`/`codex-N`）不通。
**依赖**：v0.8.6 fix-round 的 **SSE 修复**（`/events` 改从 GatewayEvent 按 sid 取源）合并 `dev`。

**决策**：
- **DD.1 SPA `ChatConsole` 改 per-session（按 gateway `s{n}` keyed）**：per-sid transcript（`Map<sid,rows>` 或 per-sid localStorage key）替单 flat buffer；新路由 `/chat/s/:sid`；新 `sessionsApi.ts`（listSessions/getHistory/submitTurn/stopSession/createSession 对 `/api/v1`）+ `useSessionEvents(sid)` hook（泛化 `useProgressStream` 到 `/api/v1/sessions/{sid}/events`）；session 列表/切换器从 `GET /projects/{slug}/sessions`（**不用** `/sessions/active` 旧命名）。切换 = 换 sid 视图，不混流。
- **DD.2 后端 3 gap**（配合 fix-round）：① `/events` 从 GatewayEvent 按 sid（fix-round 修；需把 `gw_event` broadcast tap 进 AppState，main.rs:1799 今天只到 IM daemon）；② GET `{sid}` history 对 gateway session 可用（现 filter `session_id==sid` 永不匹配 `s{n}` → 读 `<project>/.ccteam/chat/<role>/turns.jsonl` 或 gateway in-mem ring）；③ 跨项目 session 聚合（可选新 `GET /api/v1/sessions` 或 SPA per-project fan-out）。
- **DD.3 legacy** SessionDetail/SessionsListPage/`sessions/active`（`claude-N` 命名）= 保留为 operator/bg 视图，**别 repoint** 到 `/api/v1/{sid}`（命名不符）；IM-session UI 走新 ChatConsole。

**验收**：浏览器每 session 独立页/历史 + 切换干净不混；发 turn / 收 SSE / stop 走 `/api/v1`；build（vite → rust-embed `/app`）过；`v032-spa` 测试 mock 扩到新 shape。

---

## 5. Item E —— OpenAPI 自动文档（utoipa-axum）

**目标**：一个 URL 看到 `/api/v1` 全部端点的交互式文档（给 web + 将来 app/独立端集成方）。

**决策**：
- **DE.1** `utoipa` derive 标注 `/api/v1` handler（`#[utoipa::path(...)]`）+ 请求/响应类型 `#[derive(ToSchema)]`；`utoipa-axum` 的 `OpenApiRouter` 聚合所有 /api/v1 路由 → 自动生成 OpenAPI spec（与路由**单一来源**，避免文档漂移）。
- **DE.2 serve**：`GET /api/v1/openapi.json`（spec）+ **`GET /api/docs`**（交互式 UI = **Scalar**，经 `utoipa-scalar`）。一个 URL 看全部。
- **DE.3 鉴权**：docs UI + spec 默认走现有 web-token（和 /api/v1 一致）；如需对集成方公开 spec 可单独放开（决策项）。
- **DE.4 覆盖**：v0.8.6 已有（projects / roles / sessions{,/turn,/events,/stop} / capabilities）+ v0.8.7 新增（role catalog/import）。**所有 HTTP 端点定稿后做**（W3 catalog/import + W4 web 之后）。

**验收**：`/api/docs` 一个 URL 列全 /api/v1（含 v0.8.7 新端点）；spec 校验通过（OpenAPI 3.x）；路由 ↔ spec 漂移检查（可加测试断言端点计数）；鉴权一致。
**风险**：utoipa derive 对复杂/泛型响应可能需手写 schema；SSE（/events）OpenAPI 表达有限 → 标 `text/event-stream` + 注明。

---

## 6. 横切 / 非目标
- HITL **白嫖 allowlist**（per-session 可选，默认 skip）；多 harness（codex/gemini-cli/grok-cli）+ provider 轴 + push-back-as-turn → **v0.8.8**。

## 7. Risks
- **HITL `PermissionRequest` 未实证**（newest hook，逆向参考）→ W2 smoke-gate 优先；不触发则换路（fallback：PreToolUse + 自 parse allowlist，或限 headless）—— 停下报告。
- per-session UI 依赖 fix-round SSE 修复 → W4 前确认 #2 已在 `dev`。
- 两套 sid 命名 → 新 UI 统一 `s{n}`，别 repoint legacy。
- cto 调度权限门：双保险（frontmatter + daemon role==cto）防 work-role 提权。
- role import 联网 + manifest staleness（chore 刷新）。

## 8. Ship gate
`cargo test --workspace --exclude ccteam-web` ≥ dev 基线（不退）；clippy 0 `-D warnings`；`cargo fmt --all`；`doctor --verify-mcp` drift 0；tier-1 docs sync；version → 0.8.7 + tag。
