# v0.8.11 Wave 2 handoff — E1 slash bridge + HITL

> 范围:adapter 的 slash 三类 bridge gate + HITL(`can_use_tool` 同意/拒绝往返)。真 IM 接线(resolver → `permission/ask`)= Wave 3。

## Decided

1. **slash bridge = bridge posture**(非 VS Code 全透传),`bridge.rs::classify_slash` 纯函数:
   - `ALWAYS_PASSTHROUGH`(compact/clear/context 红线)或 在 init 命令表 → **Passthrough**(原样当 user text);
   - 否则 curated `DIALOG_COMMANDS`(model/mcp/login/permissions/…)→ **Reject**(人话:用 web Settings / `/role`);
   - 否则 unknown → **Passthrough as text**(绝不泄 "Unknown skill")。
   - 命令表来自 `system:init.slash_commands`(CLI 已过滤掉 dialog 类,所以表=安全集)。**live 表优先于 stale dialog 静态表**。
2. **ccteam IM 命令面优先 = gateway 层保证**:`/pair /cd /use /new /role @handle` 在 `is_gateway_command` 被 gateway 先拦,根本不到 `handle_directive`;adapter 只见 vendor slash。本 wave 无需在 adapter 重复。
3. **HITL = per-session dispatcher**:hitl session(`--permission-prompt-tool stdio`)spawn 时起一个 dispatcher task,订阅 transport,见 `can_use_tool` control_request → `CanUseToolResolver.resolve(sid, req)` → 回 `control_response{behavior:allow|deny}`。**deny 只挡该次工具、不杀 turn**(fake 验:deny 后 turn 仍 `TurnCompleted`)。skip session 不收 can_use_tool,不起 dispatcher。
4. **resolver 可插拔**:`CanUseToolResolver` trait(adapter 持 `Option<Arc<dyn>>`,`with_resolver` 注入)。无 resolver = default-deny(安全向)。`FnResolver`(闭包)给测试/简单策略用。生产 resolver(→ daemon `permission/ask` → IM)Wave 3 由 gateway 注入。
5. **control 往返基建**:transport `request_control`(req_id 关联 oneshot,Wave 1 已建)+ `can_use_tool_response_line`(protocol)。

## Rejected

- **adapter 内重判 IM 命令优先级**:gateway 已先拦,重复=耦合。
- **/model 走 `set_model` control RPC**:协议支持,但需 model picker 客户端 UI;本版按 PRD「dialog 人话拒」处理,指向 web Settings。set_model 集成留后续。
- **thread_status 加 context 累计**:仍 model-only(Default 合法);usage 累计留后续(translator 已出 usage 给 pump,adapter 侧不重复存)。

## Risks

- **resolver 是 per-vendor 单例共享**:所有 hitl stream-json session 共用一个 resolver,靠 `sid` 路由到对的 IM chat(对齐 `permission/ask` 的 firing-sid 语义)。Wave 3 接线时注意 sid→chat 映射。
- **真 can_use_tool wire 字段**:`tool_name`/`input`/`tool_use_id`/`permission_suggestions` 等真机字段未全覆盖;本版只取 tool_name/input/tool_use_id/request_id,真机 smoke 验全集。

## Files

- 新增 `bridge.rs`(classify_slash + DIALOG/ALWAYS 常量 + CanUseToolReq/parse + CanUseToolResolver/FnResolver/ApprovalDecision)
- 改 `mod.rs`(resolver 字段 + with_resolver;start_thread 起 HITL dispatcher;`spawn_hitl_dispatcher` fn;`handle_directive` 重写为 bridge gate)
- 改 tests `claude_stream_json_test.rs`(fake 加 can_use_tool;+3 e2e:slash bridge / HITL deny / HITL allow)

## Baseline

- `cargo test --workspace --exclude ccteam-web` = **1985 / 0**(Wave1 1975 + 10:7 bridge 单测 + 3 e2e)
- clippy / fmt 干净

## Remaining(给 Wave 3)

- gateway `adapter_factory` 按 `(vendor, protocol)` 选 adapter;`ClaudeStreamJsonAdapter::with_resolver(daemon_resolver)` 注入 —— resolver 实现转 daemon `permission/ask`(mcp.sock)→ IM。
- `is_real_claude_tui_handle` 放行 stream-json handle 走 start_thread 回退(resume)。
- `protocol` 创建参数 web/IM 同源 + cto 默认 stream-json + 隐藏终端 tab + screenshot 人话拒 + `host` 字段 + telegram 插件定点隔离。
