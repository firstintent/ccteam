# v0.8.7 Dev-Plan

> 配 `prd.md`。**直接在 `dev` 上开发提交（无 worktree、不开 PR）+ workflow/subagent 派工**；每 wave verify-gate 过后直接 `git commit`（`v0.8.7:` 前缀）→ `git push origin dev` → 写一份 `wave-N-handoff.md`（Decided/Rejected/Risks/Files/Remaining）。verify-gated：每 wave baseline ≥ 上 wave、clippy 0 `-D warnings`、`cargo fmt --all` 干净，**不过测试不算完成**。
> **基线 = `dev`（v0.8.6 review fix-round 合并后 = `9a1c2dc`）**。起手 `git log`/`cargo test --workspace --exclude ccteam-web` 实测当前 baseline 记下，不退它。
> 注：`CLAUDE.md` 已是 v0.8.6 版（架构 SoT 有效）；本版增量改它（§一 baseline + §三 如需 HITL/调度补充）。

## 0. 顺序 + 理由
**W1 → W2 → W3 → W4 → W5 → W6。**
1. **W1 cto 调度（B）** —— 头部价值 + **复用 W5b 已 ship 的 pub Gateway 方法最多**（create_session_api/submit_to_sid/…）；低风险，先做、最快出"团队活起来"。
2. **W2 HITL（per-session）** —— 和 cto 调度成对（派出去的 work-role 可选监督）；但有 **`PermissionRequest` hook 未实证**风险 → **wave 内先 smoke-gate**，不成立停下报告。
3. **W3 role picker/import** —— 轻、独立（catalog manifest + import + CLI），可与 W2 并行（各自 worktree）。
4. **W4 per-session web UI** —— 最重 + **依赖 fix-round SSE 修复在 dev**；放后。
5. **W5 OpenAPI 自动文档** —— /api/v1 全端点定稿后（W3 catalog/import + W4 web 之后）用 utoipa-axum 标注 + serve `/api/docs`。
6. **W6 docs sync** —— tier-1 增量收尾。

---

## Wave 1 — cto 调度（B 档）
**目标**：cto 能 spawn work-role session、派 task、收结果（MVP=polled collect）。
**改动**（prd §1）：
- 新 `crates/ccteam-cli/src/mcp_session_tools.rs`：`session_spawn`/`session_dispatch`/`session_collect`(+`session_list`/`session_stop`)；group 前缀 `session_`（`mcp_tool_groups.rs` mirror chat_/advise_）；并入 `tool_definitions()`（mcp_serve.rs）。
- `serve_mcp_socket`（main.rs:1851）+ `handle_mcp_socket_connection`：接 `shared_gateway`（main.rs:1727）；spawn/dispatch 走 `chat_send_file` forwarder（stdio→mcp.sock→daemon，ambient `CCTEAM_CHAT_{SLUG,ROLE}`）；collect = stdio 侧 tail 子 turns.jsonl（reuse `read_all_turns`）。
- daemon handler 调 `create_session_api`/`submit_to_sid`；**权限门** caller role==cto（否则 isError）。`cto_role.md` 加 `tools:` 授 session_*。
**验收**：cto spawn+dispatch+collect 走通；work-role 调 session_spawn 被拒；`doctor --verify-mcp` 0 drift；fake-adapter 测试。

## Wave 2 — HITL 批准（per-session 可选）
**目标**：创建 session 可选 hitl 模式（默认 skip）；hitl session 非 allowlist 工具调用 → IM 批准。
**改动**（prd §2）：
- **① smoke-gate（先做）**：真 claude binary（`#[ignore]`-gated）在 `claude --agent <role> --name` 交互下，非 allowlist 工具是否触发 `PermissionRequest` hook？**不触发 → 停下报告**（HITL 换路）。
- ② per-session `permission_mode: skip|hitl` 穿 creation 路径（IM `/new … hitl`、API `POST /sessions` body、cto `session_spawn` param、gateway `start_session`/`create_session_api`）。
- ③ spawn：hitl → 去 `--skip-permissions` + 装 `PermissionRequest` hook + settings `ask` 默认；skip → 不变。
- ④ 新 `ccteam internal hook permission-request`：收 payload → **复用 v0.8.5 ChoicePrompt + pending registry** 推 IM 可点 approve/deny，block 等（≤600s）→ 返回 `{behavior}`。
- ⑤ IM 审批 UX（复用 pending.rs/ChoicePrompt）。
**验收**：hitl session 非 allowlist → IM 弹、同意才跑/拒绝则 deny；allowlist 内自动跑；默认 skip 不变；smoke 实证。

## Wave 3 — role picker/import
**目标**：从 agency-agents 选 role 一键装进 `.claude/agents/`。
**改动**（prd §3）：
- `ccteam-core/src/templates/agency_agents_catalog.json`（`include_str!`，~209 entries 无 body）+ `role_catalog.rs`（search/find）。
- `import_role_from_catalog`（reqwest fetch raw.githubusercontent → sanitize stem → `write_role`）；core 加 `reqwest` + base_url-override（test 确定性）。
- CLI：`ccteam role search`/`add`/`list`（web GET `/catalog/roles` + POST `/roles/import` 跟进）。
**验收**：search 列 catalog；add 装入 + `/role` 可用；离线 browse / 在线 import honest 错误；sanitize 大写空格。

## Wave 4 — per-session web UI
**目标**：浏览器每 session 独立页/历史 + 切换器，接 `/api/v1`。
**前置**：**确认 fix-round SSE 修复（/events 从 GatewayEvent 按 sid）已在 `dev`**。
**改动**（prd §4）：
- 后端 3 gap：① `gw_event` broadcast tap 进 AppState（main.rs:1799）；② GET `{sid}` history 读 `.ccteam/chat/<role>/turns.jsonl`（现 filter 永不匹配 s{n}）；③ 跨项目 session 聚合（可选）。
- SPA：`ChatConsole.tsx` 改 per-session（按 `s{n}` keyed，per-sid transcript）+ 路由 `/chat/s/:sid` + `sessionsApi.ts` + `useSessionEvents(sid)`（泛化 useProgressStream）；列表从 `GET /projects/{slug}/sessions`。legacy SessionDetail/sessions/active 保留不 repoint。
**验收**：每 session 独立、切换不混；turn/SSE/stop 走 /api/v1；vite→rust-embed build 过；`v032-spa` mock 扩新 shape。

## Wave 5 — OpenAPI 自动文档（utoipa-axum）
**目标**：一个 URL（`/api/docs`）看 `/api/v1` 全部端点交互式文档。
**前置**：W3 catalog/import + W4 web 端点定稿（覆盖全）。
**改动**（prd §5 Item E）：
- ccteam-web 加 `utoipa` + `utoipa-axum` + `utoipa-scalar` dep；`/api/v1` handler 标 `#[utoipa::path]` + 请求/响应类型 `#[derive(ToSchema)]`；`OpenApiRouter` 聚合所有 /api/v1 路由。
- serve `GET /api/v1/openapi.json`（spec）+ `GET /api/docs`（**Scalar** UI）；走现有 web-token。
- 漂移检查：测试断言 spec 端点计数 ↔ 路由。
**验收**：`/api/docs` 列全 /api/v1（含 v0.8.7 新端点）；spec OpenAPI 3.x 校验；鉴权一致；baseline/clippy/fmt 守。

## Wave 6 — docs sync（tier-1 增量）
- `CLAUDE.md` §一 baseline + version 0.8.7；§三 如需补 HITL（PermissionRequest hook、per-session mode）+ cto 调度（session_* 工具、权限门）红线行；§四 MCP 工具数更新。
- `docs/tech-design.md`：cto 调度 / HITL / per-session UI / role catalog 协议 + 「协议→代码」指针表刷新。
- `docs/usage.md`：`ccteam role search/add`、`/new … hitl`、cto 调度、per-session web。
- 版本归档 `docs/versions/v0-8-7/README.md` + wave handoffs 收尾。
**验收**：docs 反映 v0.8.7；`doctor --verify-mcp` 工具名一致；README 英文无版本进展；CLAUDE.md ≤200 行。

---

## Ship gate（prd §8）
`cargo test --workspace --exclude ccteam-web` ≥ dev 基线（不退）；clippy 0；`cargo fmt --all -- --check`；`doctor --verify-mcp` 0 drift；tier-1 docs sync；version → 0.8.7 + tag。

## Risks（详 prd §7）
- **HITL `PermissionRequest` 未实证** → W2 smoke-gate 先行；不成立换路、停报。
- per-session UI 依赖 fix-round SSE → W4 前确认在 dev。
- 两套 sid 命名 → 统一 `s{n}`，别 repoint legacy。
- cto 权限门双保险（frontmatter + daemon role==cto）。
- role import 联网 + manifest staleness（chore）。
