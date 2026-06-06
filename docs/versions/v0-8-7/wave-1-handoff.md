# v0.8.7 Wave 1 — cto 调度（B 档）Handoff

> 直接在 `dev` 开发提交(无 worktree、无 PR)。commit `v0.8.7:` 前缀。
> **Gate**:`cargo test --workspace --exclude ccteam-web` = **1877/0**(baseline 1861 +16,新测试)· clippy 0 `-D warnings`(含 web)· `cargo fmt --all` 干净 · `doctor --verify-mcp` = 17/17 active / 0 stub / 0 drift。
> 对应 PRD §1(Item A)/ dev-plan W1。

## 概要
cto 现在能 **spawn work-role session → dispatch task → collect 结果**(MVP = polled collect)。5 个新 MCP 工具 `session_spawn/dispatch/collect/list/stop`(MCP 12→17),走 gateway session map(**不碰** deprecated registry/supervisor),daemon 侧 **role==cto 硬门**(双保险)。

## Decided
- **5 工具落新文件** `crates/ccteam-cli/src/mcp_session_tools.rs`,新 `session_` ToolGroup(mirror `chat_`/`advise_`);MCP 12→17,所有 drift 点同步(mcp_serve 3 处 + doctor_verify_mcp_test + mcp_e2e + mcp_disable_groups + mcp_subprefix + commands verify_mcp);`doctor --verify-mcp` = 17/17 active、session 5/0、0 stub。
- **5 工具全部 daemon-side 路由**(经 mcp.sock forwarder,复用 `forward_to_socket`/`forward_outcome`),**非**只 spawn/dispatch。理由:只有 daemon 持 gateway 的 `sid→role+project_dir` 映射,collect 无 daemon 做不了;5 工具同走 daemon 把权限门收在**一处**,且永不触碰 deprecated `chat_*`/registry/supervisor。
- **shared_gateway 穿线**:`Arc<Mutex<Gateway>>`(composition root main.rs ~1745)→ `serve_mcp_socket` + `handle_mcp_socket_connection`(新增 `Option<GatewayHandle>` 参数 + per-conn clone),复用 sink/pending 的 clone 模式;与 web AppState / DaemonArgs.gateway **同一 Arc**(廉价、无环、不起第二个 gateway)。
- **新 `pub Gateway::session_resolve(sid) -> Option<SessionResolve{sid,role,project,project_dir}>`**(只读、同步):collect 据此 tail 子 session `.ccteam/chat/<role>/turns.jsonl`(`read_all_turns`);collect **只在 resolve 时持锁,blocking fs 读前 DROP guard**(lock-across-await 纪律)。
- **权限门 DA.3 双层**:① `cto_role.md` frontmatter `tools:` 行授予 5 个 `mcp__ccteam__session_*`(+ cto 内建工具);work-role 模板不列 → Claude allow-list 第一道。② **硬门**:daemon `execute_session_tool` 据 ambient `_caller_role`(由 spawn env `CCTEAM_CHAT_ROLE` 注入,**绝不**取 caller args — 防伪)经纯函数 `session_caller_authorized(role)` 查 `SESSION_TOOL_PRIVILEGED_ROLES=["cto"]`;**门先于 gateway 使用执行**(gateway down 也拒非 cto),非 cto/空 caller 返回 MCP `isError`。
- **collect = polled MVP**:返回 assistant 侧 turns,支持 `since` turn_id 游标 + `n` 上限(默认 20),回吐 `cursor`;游标找不到时返回全部(永不静默丢 turn)。push-back-as-turn → v0.8.8。
- spawn 在 cto 绑定项目内建 session(ambient `_caller_slug`);`project` arg 接受但默认 caller slug(gateway 对 (project,role) 幂等)。

## Rejected
- **不**把 `cto_role.md` 做成只含 session 句柄的限制性 allow-list(会剥掉 cto 的 Read/Edit/Bash,破"就地帮忙"职责)→ 列内建 + session 句柄(agency-agents 惯例)。
- **不**做 stdio 侧 collect(PRD 建议项):stdio 进程无 `sid→role` 映射,解析仍要 daemon 往返 → collect 全 daemon。
- **不**碰 deprecated registry/supervisor/`chat_*`(红线);session 工具只用 gateway session map。
- **不**起第二个 gateway 实例 — 复用既有 shared Arc。

## Risks
- **Vendor 语义**:references/claude-code 2.2.1 中 MCP 工具**绕过** per-agent `tools:` allow-list(`agentToolUtils.ts:83` `name.startsWith('mcp__') → true`)→ DA.3 **第一层非硬边界**,真正边界是 **daemon role 门(第二层,稳)**。"work-role 连调都调不了"取决于 vendor 版本 → **W2 真 binary smoke 时一并验证 allow-list/PermissionRequest 行为**。
- `session_spawn` 暂忽略显式 `project`(只默认 caller slug);跨项目 dispatch(cto 在 A 项目 spawn 进 B)未接(gateway projects map 需登记)。PRD 称 project 为 informational,可接受;跨项目调度是 follow-up。
- spawn/dispatch/list/stop 在 gateway 方法 `.await` 期间持 gateway async Mutex(gateway 即锁目标,同 ccteam-web 模式)→ 串行化并发 session_* 调用;cto-scale(一 cto 驱动几个子)无碍,非高并发设计。
- **无**经真 mcp.sock forwarder→daemon→gateway 的 live e2e(需跑 daemon);改以(a)FakeAdapter gateway happy-path、(b)daemon handler gate/parse/shape 单测(gateway=None)、(c)stdio forwarder soft-degrade 覆盖;三者衔接 by-inspection(镜像已 e2e 的 chat_send_file)。
- 重并发负载下两个**既有**环境 flake(`hook_script_test` spawn、`cost_summary_test` t09 mtime-memoize)— 隔离跑均过,与本改无关。首跑亦见一次 1822/2 的并行 race(3 次复跑稳定 1877/0,详 gate 报告)。

## Files
- **新增**:`crates/ccteam-cli/src/mcp_session_tools.rs`(5 工具 + 6 单测)。
- ccteam-cli:`main.rs`(穿 shared_gateway + `execute_session_tool` + role 门 + 8 单测)、`mcp_serve.rs`(注册 + count 12→17)、`mcp_tool_groups.rs`(`session_` group)、`commands.rs`(verify_mcp live 12→17)。
- ccteam-im:`gateway.rs`(`pub session_resolve` + `SessionResolve` + happy-path 测试)。
- ccteam-core:`templates/cto_role.md`(`tools:` 行)、`templates/mod.rs`(layer-1 guard 测试)。
- 测试同步:`doctor_verify_mcp_test.rs`、`mcp_e2e_test.rs`、`mcp_disable_groups_test.rs`、`mcp_subprefix_test.rs`(均 12→17 + session 名/分组校验)。

## Remaining
- **W6 docs sync**:CLAUDE.md §一 baseline + MCP **12→17** + §四 工具表(加 session 组);`doctor_verify_mcp_test.rs` 头注释/inline 旧 breakdown 文案刷新(assertions 已 17、绿;仅注释漂移);tech-design「协议→代码」加 session_ 指针;usage.md 加 cto 调度用法。
- **v0.8.8**:push-back-as-turn(子结果经新 GatewayEvent 路由直灌 cto context);跨项目 `session_spawn`;`SESSION_TOOL_PRIVILEGED_ROLES` 改 config 驱动(PRD 允许"可配特权集")。
