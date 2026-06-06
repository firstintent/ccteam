# Wave 5 Handoff —— 标准资源 API（最大 wave）+ flex EOL + MCP 深砍

> v0.8.6 W5,stacked 在 v086-w2 上。三提交:**W5a** `3d9f40f`(flex EOL + MCP 20→12)+ **W5b** `e08b32f`(/api/v1 资源 API + gateway spine)+ **W5c**(web 测试 triage + SPA flex 清理)。baseline 1848(W5a)→ 1861/0(W5b/c)。clippy 0 含 web,fmt 干净。每片 build/clippy **含 ccteam-web**;W5b 过**真实 HTTP smoke**。

## Recon(read-only,5 agent)先行
映射 axum 0.8 / web-token auth / SSE broadcast 模式 / FormOrJson / 既有只读 /api/v1 / gateway 会话所有权 / flex web consumers / capabilities 源。关键纠偏:recon 一度判 flex「安全删」,实测 flex **织进** core(progress_jsonl_for_context)+ ~30 web 站点 → 故 W2 只删 flex CLI,**类型 EOL 推到 W5**(本 wave 兑现)。

## W5a — flex 类型 EOL + MCP 深砍
- flex 类型全删(core + web):`TeamKind::Flex`、`SessionRecord`、`ProjectState.sessions/next_sid_seq`、`allocate_sid/reserve_sid`、`project_session_dir`、`progress_jsonl_for_context` 的 flex 分支 + 死 flex web 路由/测试。**保留 `HarnessKind`**(= W5 harness facet,非 flex)。共享 fn(`render_screenshot`、`build_workflow_session_detail`、chat_ws 会话列表)**改写非删**(只切 flex 子块)。
- MCP 深砍:退役 8 个 `workflow_*` 工具 → **12** survivors(admin 3 + chat 6 + advise 2 + screenshot 1);`doctor --verify-mcp`=12。
- 1864 → 1848/0(-16,reconciled)。

## W5b — /api/v1 标准资源 API + gateway spine
- **spine(ccteam-im)**:pub `SessionView` + `session_views / create_session_api / submit_to_sid / stop_session`;`GatewayEvent.sid` 作 SSE 过滤键。共享 `Arc<Mutex<Gateway>>` 从 daemon 注入 ccteam-web `AppState`(**acyclic web→im 依赖**);standalone `internal web` → gateway None → session 端点 503。
- **/api/v1**(走既有 web-token auth):
  - project:GET/POST `/projects`,GET/DELETE `/projects/{slug}`(**DELETE = 注销 + 停 session,不 file-purge**;破坏性 purge 留 CLI `project rm --purge`)。
  - role:GET `/projects/{slug}/roles`,GET/PUT `/projects/{slug}/roles/{role}`(新 core `.claude/agents` frontmatter reader)。
  - session:GET/POST `/projects/{slug}/sessions`,GET `/sessions/{sid}`,POST `/sessions/{sid}/turn`,GET `/sessions/{sid}/events`(SSE 按 sid 过滤),POST `/sessions/{sid}/stop`。
  - GET `/capabilities`:harnesses(claude-code/codex)+ available PATH-probe。
- **session-id 命名空间 = gateway `s{n}`**(flex `claude-N` 随 flex 死)。
- 1848 → 1861/0(+13)。**真实 HTTP smoke 全过**(capabilities 200 claude+codex available、projects CRUD、roles 含 cto、session 端点 standalone 优雅 503、DELETE 注销-only 保留文件树、SSE gateway_unavailable 帧)。

## W5c — web 测试 triage + SPA flex 清理
- **ccteam-web 测试**(gate-excluded,W5a/W5b 后首次跑):修 2 个 **STALE** 测试(`api_v1_workflow_test` running_count 1→0 = F80 liveness phantom-demote,自 v0.4.5 起就 stale,**非** W5 回归,git 史佐证;`project_test` btw source ccteam-web→ccteam-core = W5 引擎委托)。ccteam-web **230 pass / 5 fail**(5 个 = env-gated pipe-pane ws_*,pre-W5a 同样失败,sandbox 不能流 PTY)。
- **SPA flex 清理**:删 is_flex/sessions/SessionCard/SessionTab + flex-tab 条(ProjectDetail.tsx、detailApi.ts),对齐 post-flex Rust ProjectSummary;v032-spa.spec.ts mock 改 /api/v1 形。tsc / vitest(77) / eslint / vite build / `cargo build`(build.rs→npm→rust-embed)全绿。

## Decided / Rejected / Deferred
- **Decided**:drive 路径 = bridge + 小 gateway 方法(非动 HarnessAdapter trait);web→im 直依赖(acyclic);DELETE=注销+停(非 purge);roles 项目级;capabilities=AgentVendor + PATH probe。
- **Deferred(显式,见 Risks)**:**per-session web UI 改造**——既有 `/app/p/:slug/s/:sid` 页用 project-sid(claude-1,无 gateway,rich DTO),新 W5b 端点用 gateway-sid(s{n},需 live gateway,raw events);两 sid 命名空间不兼容,改造 = 重设计(需 live-gateway fixture 才能 smoke),**非 wire-up**。**API 已 live + smoke 过**,留前端 handoff(端点清单在 SPA agent 报告)。
- 其他 deferred:SessionDetail.tsx 的 isFlex 死分支(无害,kind 永 false);TS ProjectSummary 缺 W3 cost_24h_by_vendor(pre-existing drift)。

## Risks
- per-session UI 未改造 → 现 SPA 会话页仍走旧 project-sid 路径(/ws/chat 混流仍在);新 API 供 app/独立端集成(PRD 主诉求达成)。用户可指派后续把 SPA 接到新端点。
- 5 个 pipe-pane ws_* 仅 CI/专机能过(sandbox PTY 限制),非回归。
- session `/events` SSE + `/turn` 的 live drive 需 daemon+真 claude 才能端到端验(本 wave 验了 wiring + 503 + stateless 端点;live drive 由 spine 单测 + 既有 chat 路径覆盖)。

## Files(crate 粒度)
- ccteam-core:team.rs/state.rs/paths.rs/queries.rs/screenshot.rs(flex 删)、新 roles reader、lib.rs。
- ccteam-im:gateway.rs(spine + SessionView + GatewayEvent.sid)、daemon.rs、lib.rs。
- ccteam-cli:mcp_serve.rs/mcp_tool_groups.rs(MCP 12)、main.rs(gateway 注入)、相关 mcp/count 测试。
- ccteam-web:新 routes/{projects,roles,capabilities,sessions_api}.rs + mod.rs、state.rs、Cargo.toml(+im 依赖)、flex 删(api_v1/actions/pane_snapshot/pty_ws/chat_ws/screenshot)、web/ SPA(detailApi.ts/ProjectDetail.tsx/v032-spa.spec.ts)、测试。

## Remaining
- **W6**:tier-1 文档全量重写(CLAUDE.md/tech-design/README/usage 到新模型:session=role、harness×provider、3 资源 API、config、删除/停止、MCP 12、skill ~0)+ version bump 0.8.6 + ship gate。
- 后续(非本版必需):per-session SPA UI 接新端点(前端 handoff 已备);其他低优 deferred(D1.4 pid/heartbeat、D2.4 模板集中、SessionDetail isFlex 死分支)。
