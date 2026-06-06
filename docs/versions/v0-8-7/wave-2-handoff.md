# v0.8.7 Wave 2 — HITL 批准(per-session 可选,默认 skip)Handoff

> 直接 dev、无 PR。commit `v0.8.7:` 前缀。对应 PRD §2(Item B, DB.1–DB.5)/ dev-plan W2。
> **Smoke-gate = GO**(先行,详 `wave-2-smoke-gate` 结论 + `/tmp/v087-w2-smoke-verdict.md`):真 claude 2.1.167 交互下非 allowlist 工具**确触发** `PermissionRequest`、allowlist 内不触发(白嫖成立);**必须 `--permission-mode default`、不能 skip**。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1912/0**(基线 1886 +26 新测试;另 17 ignored 含新 HITL smoke)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `doctor --verify-mcp` 17/17、0 drift。

## 概要
session 可选 `hitl` 模式(默认 `skip` 不变)。hitl session 跑**非 allowlist** 工具 → IM 弹「session sX(role) 要跑:`<tool> <摘要>` [✅同意][⛔拒绝]」,同意才跑、拒绝则 deny(只 block 该次工具,**非 kill turn**);allowlist/自动放行的工具永不弹。

## Decided
- **`PermissionMode { Skip(默认), Hitl }`** 落 `ccteam-harness/src/adapter.rs`(挨 AgentVendor/ExecutionMode;dep 方向 core→harness,**不**入 core)。serde lowercase + `parse_opt`(None/""/"skip"→Skip,"hitl"→Hitl,坏 token→Err)。挂 `SpawnCtx.permission_mode`。
- **穿透所有创建路径**:gateway `start_session`/`create_session_api`(存 `GatewaySession` + `SavedGatewaySession`#[serde(default)] 扛重启 + `SessionView`)· IM `/new claude cto hitl`(尾 token,`GATEWAY_COMMANDS` arg_hint 更新)· web `POST /sessions` `CreateSessionForm.permission_mode: Option<String>` · cto `session_spawn` param。`/role` 切换 + 重启 resume **保留** mode;template/implicit-cto/bg/workflow/supervisor/orchestrator 默认 Skip。
- **spawn 改 argv**(`claude_tui.rs` `permission_args(mode)`):Skip→`--dangerously-skip-permissions`;Hitl→`--permission-mode default`(**丢 skip flag**,保住 ask-path;显式覆盖 user-global `defaultMode:auto`)。`ensure_chat_hooks_installed(mode)`:Hitl 注入 `PermissionRequest` hook(**无 matcher = 全工具、无 timeout 字段** 让 ~600s 人工审批不被杀)指向 `{hook_sh} permission-request`;Skip 清除残留 entry。静态 settings 模板**不**碰(无全局 PermissionRequest)。
- **新 `ccteam-hooks/src/permission_request.rs`**(克隆 intercept_ask 形):读 stdin(tool_name/tool_input/session_id/cwd + slug/role,stdin 优先再 env)→ 经 `permission/ask` JSON-RPC 走 mcp.sock(复用 `mcp_socket_roundtrip`,client 660s 守、daemon 600s TTL)→ map 响应。**FAIL SAFE = deny**:打印精确契约 `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"|"deny","message"?}}}`(**无 `interrupt`** —— deny 只挡一次工具)。
- **daemon `permission/ask` responder**(`execute_permission_ask`,镜像 D6 `execute_interaction_ask`):建 2 选项 Approve(id=allow)/Deny(id=deny) ChoicePrompt,token `p{:x}`,External-origin pending,**持锁外 block 在 rx**(§七-1 纪律),map click→behavior。复用新 `gateway.session_sid_for(slug,role)` 出「sX(role)」label + 置 `GatewayEvent.sid`(供 W4 per-session SSE)。新增 `summarize_tool_input`(command/file_path/path/url/pattern,截 160)。
- **IM UX 全复用** pending.rs + ChoicePrompt + v0.8.5 D6 External-oneshot(零重造);click 经既有 `resolve_selection→apply_pending→External{reply}.send`。
- **`#[ignore]` 真机 smoke** `claude_agent_hitl_permission_request_fires_smoke`(claude_agent_smoke_test.rs):真 claude `--permission-mode default` + persona 跑非 allowlist `rm` + hook-logger,断言 hook 触发;tmux/claude 不在则 green-skip。

## Rejected
- **不**把 PermissionMode 放 ccteam-core(dep 方向不对;无 caller 需要)。
- **不**复用/重载 `interaction/ask` —— 新 `permission/ask` 方法,语义(approval→allow/deny)更清晰、与 D6 对称。
- **不**用静态 settings 模板注入 PermissionRequest(必须 per-session、仅 hitl;chat session 本就覆写 hooks 块)。
- **不**注入 system prompt / 不碰 deprecated registry/supervisor(红线);用 gateway session map + pending/ChoicePrompt。
- deny **不**带 `interrupt`(只挡该工具,不杀 turn —— 守"永不主动 kill")。

## Risks
- **live vendor 决策兑现未 hermetic 验证**:`#[ignore]` smoke 只证 hook **触发**(且 smoke-gate 已 live 证 deny+interrupt 能中断);**「approve→真跑 / deny→真挡」全链(daemon+IM e2e)需真机 `--ignored` + 活 daemon/IM 复跑后才宣告 user-ready**。确定性测试覆盖 wiring,不覆盖活 vendor 兑现。
- `summarize_tool_input` 仅特化 command/file_path/path/url/pattern,余 fallback 工具名(MVP 够用)。
- **Codex 忽略 permission_mode**(claude-only lever;codex 自有 sandbox)。`/new codex <role> hitl` 接受但对 hitl 行为是静默 no-op(SpawnCtx/PermissionMode 已注释,未对用户告警)。
- 同工具快速二次触发 → 各自 token-keyed pending,用户见两个 prompt(可接受,同 D6)。
- mode 在 live pane 首次 spawn **固定**(reattach 不 respawn)—— 已在 `start_session` 注释,不静默改存储 mode。

## Files
- ccteam-harness:`adapter.rs`(PermissionMode + SpawnCtx)、`lib.rs`(re-export)、`execution/claude_tui.rs`(argv + hook install)、tests(claude_tui{,_test,_resume,_reattach,_env}_test.rs、claude_agent_smoke_test.rs、codex_app_server_test.rs)。
- ccteam-hooks:`permission_request.rs`(新)、`lib.rs`(dispatch arm)、`intercept_ask.rs`(roundtrip 提 pub(crate))。
- ccteam-im:`gateway.rs`(mode 穿透 + `session_sid_for` + /new 解析 + 持久 + 测试)、`supervisor.rs`(Skip)。
- ccteam-cli:`main.rs`(`HookCommand::PermissionRequest` + `permission/ask` responder + summarize + 测试)、`mcp_session_tools.rs`(session_spawn param)。
- ccteam-web:`routes/sessions_api.rs`(CreateSessionForm.permission_mode + 测试)、`routes/internal_hook.rs`。
- ccteam-flow:`orchestrator.rs`(Skip call-site)。ccteam-core:3 个 adapter/harness trait 测试 call-site 更新。

## Remaining
- 真机跑 `#[ignore]` HITL smoke + daemon/IM e2e 确认 approve→跑 / deny→挡(见 Risks)。
- **W4**:per-session web UI 渲染审批(`GatewayEvent.sid` 已置,per-sid SSE 可显)。
- **W6 docs**:tech-design「协议→代码」加 `permission/ask` + PermissionRequest hook;usage.md 写 `/new <vendor> <role> hitl` + API `permission_mode`;CLAUDE.md §三 HITL 红线行。
