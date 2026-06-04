# v0.8.5 Wave 3 handoff —— Codex 命令面(F10 + D2 + D2.4 + D4 + D2.5 drift)

> 范围:prd §3-F10/D2/D2.4/D4/D2.5 + arch-refactor §1.1/§1.2/§1.3 + §4-R1/R2/R3。第 0 步 F10 真 codex smoke gating → **PASS**。
> 分支:`v0.8.5-w3`(off `origin/dev` = W2 的 4f7e03b)。`references/codex` 重锚 **b2344d8**(§7 行号对版)。落地:gate 过后直接 commit dev + SSH 推(无 PR,F10+D2+D2.4+D4 同一 PR)。

## Decided

1. **F10 step-0(同 PR,§4-R2/R3)**:`resolve_codex_transport()` 纯函数**单轴**(`CCTEAM_CODEX_APP_SERVER_SOCKET` 有⇒Socket;无⇒Stdio,program=`CCTEAM_CODEX_BIN|codex`);**删 `APP_SERVER_TRANSPORT_ENV`**(全仓 grep 迁移 + smoke 测试改 socket-presence)。transport 构造期解析一次存 adapter;`client()` 纯 match;UDS 代码保留(Socket override)。`default_adapter_factory` **per-vendor 单例**(Arc::ptr_eq 测试)→ 每 daemon 一个 codex app-server 子进程。**接通 `forget_client`**(submit 错误路径自愈 re-dial)。`client()` 在 cache 前完成 `initialize` 握手(实测 defect 修:不握手则 server `experimental_api=false`,过滤掉 `thread/tokenUsage/updated` 等 ~30% 通知——D2.4 依赖之)。`raw_extras.transport` = 解析真值。
   - **真机 smoke PASS**:codex-cli 0.136.0,默认 stdio 路径 spawn `codex app-server --listen stdio://` + `initialize` + `thread/start` 成功,`/new codex` 默认路径连得上。
2. **D2 全量命令面(§3-D2.1,锚 b2344d8 §7)**:`handle_directive` 三层 resolution(内建表 → skills/list 缓存 → Rejected+候选)+ 六类映射,RPC method 名 + camelCase params 全部对 b2344d8 核(compact:541 / review:797[4 ReviewTarget] / interrupt:762 / steer:756 / fork:457 / rollback / rename / goal / stop / memories:524 / diff:command_exec / init / login / logout;query-synth status[tracker]/model/skills/mcp/hooks/apps;override model/personality/collab/permissions;redirect new/clear/resume;TUI-only Rejected;错误原文传播)。
3. **D2.2 active-turn**:tracker 有 active_turn 时普通 UserText 走 `turn/steer{expectedTurnId}`(否则 turn/start + overrides);`/interrupt` 共用。**per-session override map**(keyed by thread_id,照 bridges)。
4. **D2.4 CodexThreadTracker**:harness 级 dispatcher,client 握手后 spawn **一次**(不挂 events());usage **只从 `thread/tokenUsage/updated`**(`turn/completed` 真实 wire 无 usage);progress.jsonl 镜像**保留**,净新增 = 内存缓存;`thread_status`(codex)+ `/status` 读 tracker;两条 events() 流不重算(测试)。**`skills/changed` 移出 `initialize` optOutNotificationMethods** + dispatcher 消费失效缓存。
5. **D4 bare 弹窗两段式**:8 命令 bare → list RPC/静态枚举 → `NeedsChoice`;带 args → D2 直接 apply;choice 重入 → apply。token 唯一 `cx{nanos}`。`/model`(model/list)、`/collab`(collaborationMode/list,EXPERIMENTAL)、`/personality`(静态枚举)、`/permissions`(AskForApproval/SandboxMode 静态,admin)、`/review`(4 ReviewTarget,branch/commit 二跳 free_text)、`/skills`(skills/list)、`/memories`(enabled/disabled)、`/resume`(thread/list → `/use <id>` Redirect)。`/plan` 保留直接 apply(plan ModeKind 定向别名,非开放选择)。
6. **D2.5 drift 快照**:pin Codex `SlashCommand` 枚举名常量 + 断言「内建表 ∪ reject 名单」全覆盖;未分类 → 测试报错(已验真会 fail)。

## Rejected / 偏离(明示)

1. **PRD「67 个 SlashCommand 变体」过时** —— b2344d8 实测 **53** 个(code-is-SoT)。drift 快照 pin **53**(26 builtin + 27 rejected + 0 未覆盖)。PRD §3-D2.5 的 67 数应更新。
2. **EXPERIMENTAL `/collab`/`/permissions` wire 形态仅 fixture 验**:collab `CollaborationMode={mode,settings:{model 必填}}`(无 model 则跳过 + warn);permissions sandboxPolicy 内部 tagged `{type:readOnly|workspaceWrite|dangerFullAccess}`、approvalPolicy kebab-case;preset→policy 映射是本实现选择,未对真 codex 验。
3. **`/login` `account/login/start {}` 空 params**(未定位 LoginAccountParams 结构,宏生成);有必填则 server 报错(原文传播,不静默)。
4. **`/fork` 仅最小 gateway 钩子**(新 threadId 进 `Done` receipt 提示 `/use <id>`);`DirectiveOutcome` 无「注册 session」变体,自动注册需 gateway 改动(推后)。
5. **D2/D4 未对真 codex 跑**(F10 stdio /new smoke 除外):scripted in-process JSON-RPC peer + 从 b2344d8 协议源转写的枚举;`collaborationMode/list`/`thread/list`/`model/list` wire 形状 + Personality/AskForApproval/SandboxMode/ReviewTarget 枚举未对活 binary 验。`/review` 二跳 free-text + `/resume`→`/use` 依赖 gateway 重入契约(读 gateway.rs 1104/1206-1277 确认,未 e2e)。

## Risks

- EXPERIMENTAL collab/permissions wire 形态、多数命令仅 scripted-peer 验(非真 codex)。建议 W4 或专机真机逐命令 smoke。
- 53-名快照需随 codex 参考 bump 重同步(drift 测试守)。
- 多 session「每 daemon 一子进程」单例属性结构保证(Arc::ptr_eq)但无活的多 session e2e。

## Files

- `ccteam-harness/src/execution/codex_app_server.rs` —— F10 transport + D2 全量 handle_directive + CodexThreadTracker + dispatcher + override map + skills cache + D4 popup arms + drift classifiers(`is_builtin_command`/`is_rejected_command`)。
- `ccteam-harness/tests/codex_app_server_test.rs` —— F10 测试 + D2 14 scripted-peer arms + D4 两段式 + D2.5 drift 快照(53)。
- `ccteam-im/src/daemon.rs` —— `default_adapter_factory` per-vendor 单例 + 测试。
- `ccteam-im/tests/inbound_wiring_test.rs` —— 删 transport env(默认 stdio)。
- `docs/usage.md` —— socket-override 单轴文档。
- `scripts/smoke-im.sh` —— socket-presence 分支。

## Remaining(交下游)

- **W4**:P3 `thread_status` 渲染(codex 读 tracker 已就位;Claude 倒读 transcript [1m]→1M 仍待)+ gateway 单点 `188k/1M(19%)` + `/sessions`/codex `/status` 同源;P1 register_commands + TG setMyCommands;P4 `.agents/plugins/marketplace.json` + codex 市场真机;ship-gate(Cargo 0.8.5、CLAUDE.md baseline、tech-design、README/usage、doctor --verify-mcp)。
- **真机验证 follow-up**:D2/D4 EXPERIMENTAL 命令(collab/permissions/login)对真 codex 逐条 smoke;`/fork` 自动注册(gateway 变体);多 session 单子进程 e2e。
- PRD §3-D2.5 的「67」更正为 53。

## Gate(push 前实测)

- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = **1890 / 0**(W2 后基线 1865,+25:F10→1868、D2→1882、D4→1890)。
- `cargo clippy --workspace --all-targets -- -D warnings` = **0**(ccteam-web 本 wave 未改,沿 W2 clean)。
- `cargo fmt --all --check` = **clean**。
- 已核:`references/codex` @ b2344d8 的 `SlashCommand` 实测 **53** 变体(drift 快照 pin 53);D2 RPC 锚点对版命中(compact:541/review:797/steer:756/fork:457/model:803/memoryMode:524)。
- F10 `f10_real_codex_stdio_new_smoke` = `#[ignore]` 真机门(已手验 PASS,不计 CI baseline)。
