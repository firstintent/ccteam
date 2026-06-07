# v0.8.8 Phase 4 handoff — 文档 + 版本号 + 实机 bug 二修 + 收尾 gate

> 最后一阶段。Gate(收尾):`cargo test --workspace --exclude ccteam-web` **1998/0**(v0.8.7 基线 1975)· clippy --workspace --all-targets **0** · fmt 干净 · ccteam-web **224 pass + 4 env-gated `ws_*`** · vitest **151/151** · `doctor --verify-mcp` **17/0** · skill-gate 0 · CLAUDE.md **152 行**(≤200)。
> 编排:文档前台 opus agent ×2(CLAUDE.md+tech-design / README+usage)→ 主控版本 bump + 2 个实机 bug 修(用户测 dev 发现)+ 归档 + 收尾 gate(主控亲跑)。

## Decided(已定 + 落地)
- **文档红线重写**:CLAUDE.md §〇/§一/§三/§四 + tech-design.md 改成 **session = 独立一等实体 + 持久 sid;role 是属性;去 dedup;resume-by-session-id;turns/pane/marker 按 sid;gateway spawn_event_pump 唯一 turns writer;roleless**;协议→代码指针表补新锚点(chat_session_name(slug,sid)、tracked_chat_sessions、SessionResolve.vendor、CCTEAM_CHAT_SID、/api/v1/config/im、Settings/Roles 页)。CLAUDE.md §一 baseline 回填 1998/0 + web 224+4 + vitest 151。skills/ doc-drift(C1 删了)修正。session=role keystone 挪入"已退役"。
- **用户面**:README.md(英文、无版本时间轴)+ docs/usage.md 融入新能力(独立 session /new /use /sessions /role、roleless、status 新格式、session ls vendor、web Settings/Roles 页、新建项目、project stop 修复、迁移须知)。诚实:web 终端裸 ANSI 保真需 CCTEAM_MUX_BACKEND=tmux(默认 rmux 行文本);roleless 入口本轮 web/REST(IM /new 仍默认 cto)。
- **版本 0.8.7 → 0.8.8**:workspace Cargo.toml + **4 个 plugin manifest 版本站点**(.claude-plugin/plugin.json、marketplace.json ×2、.codex-plugin/plugin.json)—— `plugin_manifests_match_workspace_version` 测试守这条同步(收尾 gate 抓到忘 bump manifest → 已补)。
- **实机 bug 二修(用户测 dev 实时报)**:
  - **bug1**(新建项目后建 session 报 `unknown project`):`start_session` + `switch_current_role` 查项目前先 `ensure_project_loaded()`(从 config.yaml SoT 同步,跟 `/cd` 一致)。根因 = REST `POST /projects` 写 config.yaml 但没进内存 `projects` 缓存,而 create-session 路径只读缓存。加回归测试 `gateway_create_session_api_loads_project_from_config`。
  - **bug2**(`settings.local.json` 的 `permissions.allow:["*"]` 被新版 Claude Code 拒):init 模板 settings.json + settings.agent-team.json 的 `allow:["*"]` → 合法 `defaultMode:"bypassPermissions"`;test 同步。HITL 不受影响(ccteam spawn 走 CLI `--permission-mode default`/`--dangerously-skip-permissions` 覆盖 settings)。
- **MCP 仍 17**:F4 是 REST 路由非 MCP 工具;doctor --verify-mcp 17/0。

## Rejected(否决 + 因由)
- **bug2 用其它修法(删 permissions / 用 acceptEdits)**:否 → `defaultMode:"bypassPermissions"` 最贴原 `allow:["*"]` 意图(不弹权限)且合法。
- **bug2 改已有项目的 settings.local.json**:否(只改模板,新 init/重 init 生效;已有项目用户手删那行或重 init —— 已在 README/usage 写明)。
- **/role real-claude #[ignore] smoke 写进代码**:否(沙箱 fake 复现不了 --name collision,写个跑不了的 #[ignore] 是伪覆盖)→ 改为留给用户专机手验(用户正实机测 dev,直接验 /role 比伪测试可靠);文档明列为 ship-gate-pending。

## Risks(残留 + 监控)
- **bug2 只对新 init 生效**:已有项目(如用户的 `ideas`)的旧 `allow:["*"]` 仍在 → 文档提示手删或重 init。
- **/role --name-collision 真机行为未自动验**(沙箱 fake 不能):carry-context + death-probe 已实现 + F1 测覆盖 same-sid;真 claude 的 --name 复用语义留专机验(ship-gate-pending)。
- **4 个 `ws_*` env-gated**(tmux pipe-pane,沙箱)+ 真 per-session 字节中继:CI/专机。
- **W2 PermissionRequest 在 multiple same-role 下**:hook 报对 sid 已设计(CCTEAM_CHAT_SID),专机复跑确认。
- **doc↔code drift 防线**:plugin_manifests_match_workspace_version + skill-gate + doctor --verify-mcp 三道收尾 gate 守(本次 bug = 忘 bump manifest,已被 gate 抓 + 修)。

## Files(改了什么)
- 文档:CLAUDE.md(§〇/§一/§三/§四 重写,152 行)· docs/tech-design.md(架构 SoT + 指针表)· README.md(英文,新能力)· docs/usage.md(命令 + 迁移)· docs/versions/v0-8-8/README.md(本归档)+ wave-0..4-handoff.md。
- 版本:Cargo.toml(workspace 0.8.8)· .claude-plugin/{plugin,marketplace}.json · .codex-plugin/plugin.json。
- bug1:crates/ccteam-im/src/gateway.rs(start_session + switch_current_role 加 ensure_project_loaded + 回归测试)。
- bug2:crates/ccteam-core/src/templates/{settings.json,settings.agent-team.json}(allow:[*]→defaultMode:bypassPermissions)+ templates/mod.rs(test 断言更新)。

## Remaining(ship / 后续)
- **HOLD git tag**:收尾全绿、push dev,但 `v0.8.8` tag + main-merge **留给用户 sign-off**(ship-flow 纪律)。
- **用户专机验**:/role real-claude --name 行为;W2 PermissionRequest multiple same-role;web 终端真 pane(tmux)。
- **deferred(可 v0.8.9)**:dead-chain cleanup(supervisor/outbound + chat_history/send_input 死工具)· per-sid IM 路由 · catalog web 装 · ChatConsole 裸色统一。
