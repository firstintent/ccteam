# v0.8.7 Wave 6 — docs sync + version bump + ship gate Handoff

> 直接 dev、无 PR、commit `v0.8.7:` 前缀。对应 PRD §8 / dev-plan W6(tier-1 docs 增量收尾 + ship gate)。终篇。
> **Ship Gate(全绿)**:`cargo test --workspace --exclude ccteam-web` = **1942/0**(首跑无 flake —— spawn-flake 已 harden)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `cargo build -p ccteam-web` OK · `doctor --verify-mcp` = **17/17、0 drift** · CLAUDE.md **149 行**(≤200)· version **0.8.7**(全 5 处 product 串)。

## Decided
- **版本 bump 0.8.6→0.8.7**(5 处 product 串):workspace `Cargo.toml [workspace.package].version` · `.claude-plugin/plugin.json` · `.claude-plugin/marketplace.json`(plugins[0] + top-level)· `.codex-plugin/plugin.json`;两个 plugin.json 描述里「20/12 MCP tools」prose 更正为 **17**。`web/package.json` 留 0.3.2(独立 SPA 版本,无断言)。Cargo.lock 经 build 重生。`env!(CARGO_PKG_VERSION)` 消费方(--version / mcp_serve / health / openapi info.version / core VERSION)自动跟随。**无 version-asserting 测试需改**(`plugin_manifest_version_test` 相对比较 5 串、6/6 绿;无测试 pin 字面 "0.8.6")。
- **CLAUDE.md(149 行)**:§〇 版本戳 + per-session-web/OpenAPI 已落地;§一 version→0.8.7、baseline→1942/0(注 ccteam-web 230+ / 5 ws_* + vitest 108)、「当前在做」改 v0.8.7 一段式总结、MCP「12→17」+ session_ 前缀 + /api/docs + /chat/s/:sid;§三 加 2 红线行(HITL 边界 = PermissionRequest hook、hitl spawn `--permission-mode default` 非 skip、deny 不杀 turn;cto dispatch 门、session_* only、dispatch/stop 显式非主动 kill — 注:review-fix R-M1/R-M3 把该门改成校验 per-session secret + project 维度,best-effort 非硬边界)+ 校正退役-HITL 注(per-session HITL 已落 vs orchestration 级 deferred);§四 MCP 17 + session 5 组 + `ccteam role search/add/list`。
- **tech-design.md**:§6.4 MCP 12→17 + session 组 + 双层权限门;§6.5 安全 = per-session `PermissionMode{Skip,Hitl}` 全说明(skip=skip-permissions,hitl=--permission-mode default + PermissionRequest→permission/ask→IM approve/deny,FAIL-SAFE deny,无 interrupt,无注入)+ Lark fail-closed;§6.6 web 加 per-session {sid}(history-from-turns.jsonl + sid-filtered SSE + approval render)+ OpenAPI(单 OpenApiRouter,/api/docs + openapi.json,鉴权,anti-drift);§10「协议→代码」表 MCP→17 + mcp_session_tools.rs + **6 新行**(cto dispatch、HITL permission_mode、HITL permission/ask、role catalog/import、per-session web、OpenAPI);校正 5 处 §12→§10 cross-ref。pointer-style(file:symbol)。
- **usage.md**:`ccteam role search/add/list`(--as/--project/--force、离线 search、verbatim 写、上游漂移 404)· `/new <vendor> <role> hitl` + HITL [同意]/[拒绝] 段 · cto dispatch 子节(@cto + 5 session_* 工具、cto-only)· per-session web `/app/chat/s/:sid` · 交互式 API 文档 `/api/docs` + openapi.json · Lark/Feishu config 菜单 · §9 verify-mcp active 17。
- **README.md**:英文、无版本时间轴;刷新当前能力(HITL / role picker / per-session web / OpenAPI / Lark / 17 MCP)。
- **docs/versions/v0-8-7/README.md(新)**:冻结归档(同 v0-8-6 骨架)—— 索引全 handoff、各批次交付表、ship-gate baseline、红线、推后、迁移注(pre-v1.0 无迁移:清 `~/.ccteam` + 项目 `.ccteam` 重 init;serde-default 吸收旧 gateway-state;SPA v1 buffer 弃)。
- **fix.md ✅ 标注**:FIX-1/2/3 各加一行「✅ FIXED in 935eb66」(不改 bug 正文)。
- **flake harden(test-only,gate 卫生)**:`hook_script_test.rs` 加 `spawn_with_retry`(ETXTBSY os26 / EAGAIN os11 / WouldBlock 重试 10×、20ms,余错即 panic),3 处 spawn 站点改用之;断言不动,3/3 过。本 wave 首跑即 1942/0 无 flake = 验证有效。

## Rejected
- 不 bump `web/package.json`(独立 SPA 版本,非 product version,无断言)。
- 不在 docs 重复协议细节(代码为 SoT,pointer-style)。

## Risks
- 残留 "0.8.6" 扫描:**无 product 版本串遗留**;命中均为历史/叙述(CLAUDE.md lineage、tech-design 退役红线史、版本归档、`.rs` //! 落地时点注释)—— 正确保留。
- README/usage 为人写文档,随能力演进需维护(非自动)。

## Files
- 版本:`Cargo.toml`、`Cargo.lock`、`.claude-plugin/plugin.json`、`.claude-plugin/marketplace.json`、`.codex-plugin/plugin.json`。
- 文档:`CLAUDE.md`、`docs/tech-design.md`、`docs/usage.md`、`README.md`、`docs/versions/v0-8-7/README.md`(新)、`docs/versions/v0-8-7/fix.md`(✅ 标注)。
- 测试:`crates/ccteam-cli/tests/hook_script_test.rs`(spawn-retry)。

## Remaining(v0.8.7 范围外 / 已记 follow-up)
- **tag + main-merge HOLD**:ship-flow 红线 —— 推 dev 但 git tag `v0.8.7` 留用户 sign-off;main 合并待用户显式命令。
- v0.8.8:push-back-as-turn(cto 子结果直灌 context);web 审批经 resolve_numeric(W4 risk);多 harness(codex 全量/gemini/grok)+ provider 轴;跨项目 session_spawn/聚合端点;catalog refresh script;HITL daemon/IM e2e + #[ignore] 真机 smoke 复跑。
