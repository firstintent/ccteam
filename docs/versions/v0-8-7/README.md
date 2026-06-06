# v0.8.7 — cto 调度 + per-session HITL + role picker + per-session web + OpenAPI(+ Lark config + 实机 bug 三修)

> 冻结版本归档。本目录 = v0.8.7 的完整开发记录:`prd.md`(为什么 + 决策 + 验收)、`dev-plan.md`(wave 顺序 + 理由)、`wave-1..5-handoff.md` + `fix-md-batch-handoff.md` + `lark-feishu-config-handoff.md`(各批次的 Decided/Rejected/Risks/Files/Remaining)、`fix.md`(实机 bug 清单,已逐条标 ✅ FIXED)。本 README 是里程碑索引 —— 一版交付了什么、baseline、推后项。

## 一句话

v0.8.7 是 **v0.8.6「IM 通用模式 + session=role」模型上的能力补全**(非架构改动):给单 session 模型补上 cto 调度子 session、可选的逐工具人工批准(HITL)、从角色库一键装 work-role、每个 session 的独立 web 页、以及 `/api/v1` 的自动 OpenAPI 文档;另把 Lark/Feishu 接入 `ccteam config` 配置面,并修掉三个实机使用暴露的 bug。MCP 工具 12 → 17。

## 模型(沿用 v0.8.6,无变)

核心仍是 **`chat ⇄ project ⇄ session ⇄ role`**:一个 chat(IM/web)→ 切 project → spawn/resume session → session 即一个 `claude --agent <role>`;daemon 是纯路由网关(不 tick、无 orchestrator 循环);标准资源 API `/api/v1`(project/role/session + capabilities)。v0.8.7 不动这些红线,只在其上加能力。

## 各批次交付

| 批次 | 主题 | 交付 |
|---|---|---|
| **W1** | cto 调度(B 档) | 5 个 `session_*` MCP 工具(`spawn`/`dispatch`/`collect`/`list`/`stop`,新 `session_` ToolGroup;MCP 12→17)走 gateway session map(**不**碰 deprecated registry/supervisor);daemon cto 门(据 spawn-env 注入的 ambient `_caller_role`;门先于 gateway 执行)— **review-fix(R-M1/R-M3)抬门槛**:改校验 per-session secret(`(role,secret)` 对)+ project 维度,best-effort defense-in-depth 非硬边界;新 `pub Gateway::session_resolve`;collect = polled MVP(tail 子 `turns.jsonl` + `since` 游标)。`shared_gateway`(同一 `Arc<Mutex<Gateway>>`)穿进 mcp.sock handler。|
| **W2** | per-session HITL 批准 | `PermissionMode { Skip(默认), Hitl }`(`ccteam-harness/adapter.rs` + `SpawnCtx`)穿所有创建路径(IM `/new claude cto hitl` 尾 token、web `POST /sessions` `permission_mode`、cto `session_spawn` param;`/role`/重启保留)。hitl spawn 走 `--permission-mode default`(**绝不** skip)+ 注入 vendor 原生 `PermissionRequest` hook → `ccteam-hooks/permission_request.rs` 经 `permission/ask` JSON-RPC → daemon `execute_permission_ask` 建 Approve/Deny ChoicePrompt 弹 IM。FAIL-SAFE=deny;deny 只挡该工具、**无 `interrupt`**、不 kill turn。先 smoke-gate(真 claude 2.1.167 实证:非 allowlist 工具触发、allowlist 不触发、必须 default 不能 skip)。|
| **W3** | role picker / import | `ccteam role search/add/list`:离线浏览 + 一键装 agency-agents(wshobson/agents,MIT)role 进 `.claude/agents/`。catalog = vendored 全量 **192 entries / 78 divisions**(`agency_agents_catalog.json` `include_str!`)。拓扑分层:纯 catalog(parse/search,**零 reqwest**)留 `ccteam-core`(leaf);网络 import(`import_role_from_catalog{,_with_base}`)落 `ccteam-im`。`role add` = `reqwest GET` raw → `write_role` **verbatim**(零 frontmatter 转换)。|
| **W4** | per-session web UI | 每 gateway session(`s{n}`)独立页 `/chat/s/:sid` + 历史 + 干净切换(不混流):后端 `GET /api/v1/sessions/{sid}` 重写为 `session_resolve` → 读 `<project_dir>/.ccteam/chat/<role>/turns.jsonl`(弃永不匹配的 `session_id==s{n}` progress 过滤);per-sid SSE tap(fix-round 已落)。前端整套 rewire(`ChatConsole.tsx` 按 sid keyed + per-sid localStorage + `sessionsApi.ts` + `useSessionEvents.ts`);HITL 审批 per-session 渲染。|
| **W5** | OpenAPI 自动文档 | `GET /api/docs`(`utoipa-scalar` 交互式 UI)+ `GET /api/v1/openapi.json`(OpenAPI 3.1),由**单聚合 `OpenApiRouter`**(`routes/openapi.rs` `split_for_parts()`,既出 live Router 又出 spec)生成 → 单源、anti-drift;每 `/api/v1` handler 挂 `#[utoipa::path]`(28 ops);两者同 web-token 门(无公开未鉴权 spec);drift 测试钉 op 数强制 dual-edit。新 deps:`utoipa 5` + `utoipa-axum 0.2` + `utoipa-scalar 0.3`(对 axum 0.8 干净)。|
| **Add-on** | Lark/Feishu 接入 `ccteam config` | `ccteam config` 菜单加独立第 3 项「set Lark/Feishu app credentials」(transport 早已在树,**仅补配置面**):`run_config_set_lark_creds` 先 live 校验(取 `tenant_access_token`)再 merge 进 `credentials.json`;fail-closed allowlist(空=拒绝所有人,`ou_...` open_id)双重提示;3 校验器解禁。|
| **fix.md** | 实机 bug 三修(935eb66) | **FIX-1** 出站文件发到 live session(新 `Gateway::reply_target_for` + 穿 gateway 进 file-send/ask,live 优先、registry 兜底)· **FIX-2** web 默认 role `assistant`→`cto` + create chokepoint `ensure_role_exists`(未种 role fail-fast、不留死 pane)· **FIX-3** `ccteam status` STUCK 误报(stall 时钟改从 `progress.jsonl` 末行 ts 取并回填 state)。|

## Baseline(ship gate)

- `cargo test --workspace --exclude ccteam-web` = **1942/0**(`ccteam-web` ws_* env-gated 测试留 CI/专机)。
- `ccteam-web` = 230+ pass / 5 env-gated `ws_*`(sandbox 不能流 PTY,非回归)· **vitest 108/108**(SPA)。
- `cargo clippy --workspace --all-targets -- -D warnings` = **0**(含 web)。
- `cargo fmt --all -- --check` 干净。
- `ccteam doctor --verify-mcp` = **17 工具**(admin 3 + chat 6 + advise 2 + session 5 + screenshot 1),drift 0。
- `cargo build -p ccteam-web` OK(vite SPA build via build.rs)。
- workspace version → **0.8.7**(+ 4 manifest 版本串同步;`plugin_manifests_match_workspace_version` 守)。

> baseline 轨迹:1861(v0.8.6 ship)→ W1 1877 → Lark 1886 → W2 1912 → fix.md 1919 → W3 1942 → W4/W5(测试在 ccteam-web,不入此计数)1942/0。

## 红线(保留 + 新增兑现)

保留全部 v0.8.6 红线(no prompt injection、`progress.jsonl` state SoT、不 scrape pane、resume-by-id、永不**主动** kill、`ccteam-core` 零 team 名、crate 拓扑、README 英文无版本进展、skill 自洽、不 vendor 二进制)。本版**新增两条就地红线**(已并入 CLAUDE.md §三):

- **HITL 批准边界 = `PermissionRequest` hook**:批准门走 vendor 原生 hook(不注入 system prompt)；hitl spawn 走 `--permission-mode default`(绝不 skip,否则白嫖批准);deny 只挡该工具、不 kill turn(守「永不主动 kill」)。
- **cto 调度门 = daemon 校验 per-session secret(best-effort,非硬边界)**:`session_*` 特权据 spawn-mint 的 per-session secret 校验 `(role,secret)` 对 + project 维度(review-fix R-M1/R-M3;单 uid 全信任下只抬门槛,真隔离 = per-agent OS user/sandbox v0.8.8 deferred),只用 gateway session map;`dispatch`/`stop` 是显式调度,非主动 kill。

## 推后 / Deferred

- **编排级 HITL / workflow.yaml 批准 state SoT**:`ccteam-flow` 的声明式审批节点仍推后(本版只做 **per-session** 交互式批准,两件事)。
- **web 审批 resolve**:per-session 页**渲染**审批正确,但 web 点击 resolve token 注册的 PermissionRequest pending 仍 best-effort(稳妥解析在 IM token path)→ turn 端点经 gateway `resolve_*` + SSE 带 token 是 follow-up。
- **cto 调度增强**:push-back-as-turn(子结果直灌 cto context)、跨项目 `session_spawn`、`SESSION_TOOL_PRIVILEGED_ROLES` 改 config 驱动 → v0.8.8。
- **role catalog/import 的 web 端点**(`GET /api/v1/catalog/roles` + `POST …/roles/import`):本版 MVP=CLI,web 端点推后(加时需带 `#[utoipa::path]`,drift 测试会强制)。
- **跨项目 `GET /api/v1/sessions` 聚合**:本版 SPA 用 per-project fan-out;聚合端点可后续收敛。
- **Codex role 对齐 / HITL**:Codex 仍只读项目原生 `AGENTS.md`,忽略 `permission_mode`(自有 sandbox)。
- 低优 chore:`session rm` 单 session 粒度删;`stall::silent_seconds` 旧 `last_progress_event_at` 写路径清理;无头 `config lark <...>` 子命令。

## 迁移(pre-v1.0:无迁移)

开发阶段不写迁移步骤。若旧状态与本版不兼容(如 per-session permission_mode 字段、per-sid localStorage key 变更),直接**清旧数据 → 重 init**:`rm -rf ~/.ccteam` + 各项目 `.ccteam/` → `ccteam init` → `ccteam config` → `ccteam start`。`SavedGatewaySession.permission_mode` 用 `#[serde(default)]` 扛旧 gateway-state.json(缺字段=Skip),无需手动迁移;SPA 旧 `ccteam.chat.rows.v1` flat buffer 直接弃用(新 key `…v2.${sid}`,无迁移)。
