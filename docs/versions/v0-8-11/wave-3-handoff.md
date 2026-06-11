# v0.8.11 Wave 3 handoff — E2 创建面接线

> 范围:把 stream-json adapter 接进 gateway/web/IM 创建面 —— `protocol` 参数、factory 选 adapter、cto 默认 stream-json、session schema 预留 `host`、终端 tab 隐藏、screenshot/`/screen` 人话拒、telegram 插件定点隔离。

## Decided

1. **`SessionProtocol` 枚举**(harness `adapter.rs`,§七 ② 命名为 `protocol` 非 `backend`):`StreamJson`(默认)| `Terminal`;kebab serde(`stream-json`/`terminal`)+ `parse_opt`/`as_str`/`is_*`。core 不重导,im/web 直接从 harness 取。
2. **factory 按 `(vendor, protocol)` 选 adapter**:`AdapterFactory = Fn(AgentVendor, SessionProtocol) -> Arc<dyn HarnessAdapter>`;`default_adapter_factory` 持三 adapter 单例,(Claude,StreamJson)→claude-stream-json · (Claude,Terminal)→claude-tui · (Codex,_)→codex-app-server。
3. **`create_session_api` 不破签名 = 薄 wrapper**:保留 4-arg(默认 stream-json),新增 `create_session_api_proto(…, protocol)` 给 REST 路由。**零测试 churn**(~38 个 test caller 不动)。
4. **protocol 贯穿 session**:`GatewaySession` + `SavedGatewaySession`(serde default)+ `SessionView` 加 `protocol` + **`host`(默认 local,§七 ② 预留,不暴露 UI/CLI)**;save/load 持久 + 归一。`/role` 切换、codex `/clear` recycle、daemon 重启 resume 都保留 protocol。
5. **cto + IM/web 同源默认 stream-json**:default-cto spawn / template / `/new` 无参 = stream-json;`/new … terminal` 显式选终端(`hitl`/`terminal` token 顺序无关);web `CreateSessionForm.protocol`(默认 stream-json,SPA 加选择器);单一默认常量 = `SessionProtocol::default()`。
6. **resume 放行 stream-json handle**:`is_real_claude_tui_handle` 泛化为「tmux handle OR `adapter==claude-stream-json`,且有 cwd+project_dir」→ resume_thread 失败回退到 resume-aware 的 start_thread。
7. **终端 tab 隐藏(SPA)**:stream-json session `canTerminal = vendor==claude && protocol!="stream-json"`;缺字段=向后兼容当终端可用;隐藏时一句提示。create 弹窗加 protocol 选择器(stream-json 默认 / terminal 高级)。
8. **screenshot/`/screen` 人话拒**:IM `/screen`(gateway 有 session)对 stream-json 人话拒绝(无 pane)。**诚实差异**:MCP `screenshot` 工具 + web screenshot 路由都跑在无 gateway 上下文里(stdio mcp-serve / 已无 gateway 的 route),对 stream-json 走既有「无 pane → 降级」消息,非 protocol 专属文案 —— SPA 已隐藏终端 tab,用户面不踩。
9. **telegram 插件定点隔离**:`ensure_telegram_plugin_disabled(project_dir)`(claude_tui,pub)写 `.claude/settings.local.json` 的 `enabledPlugins.{telegram@claude-plugins-official}=false`,**只关这一个**、合并不动其余键;两条 Claude spawn 路径(tmux + stream-json)start_thread 都调。

## Rejected / Deferred

- **HITL 生产 resolver 接线**:stream-json adapter 在 factory 里 **不带 resolver** → hitl stream-json session default-deny(安全向)。理由:resolver → `permission/ask` → IM 需跨 crate gateway/sink/pending + 初始化序问题,且默认 posture = skip(常路不受影响),E1 HITL 往返已由 Wave 2 机制 + 测试满足。**follow-up**:late-bind resolver(改 resolver 字段为内部可变 + daemon 构造后注入)。
- **start_session 参数 bag 结构体**:7 字段都是独立 session 属性,bag 只是换名;保留扁平签名 + `#[allow(clippy::too_many_arguments)]`。

## Risks

- **pre-v0.8.11 state 文件**:无 `protocol` 字段 → 恢复为 StreamJson 默认,但其 live handle 是 tmux handle;resume 会按默认 protocol 重 spawn(dev 阶段不做迁移,可接受 —— CLAUDE.md §五纪律)。
- **MCP/web screenshot 对 stream-json 的降级**(见 Decided 8):非 protocol 专属文案,follow-up 可加。

## Files

- harness:`adapter.rs`(SessionProtocol + 测试)· `lib.rs`(re-export)· `claude_tui.rs`(`ensure_telegram_plugin_disabled` + TELEGRAM_PLUGIN_ID + tmux spawn 调用)· `claude_stream_json/mod.rs`(telegram 隔离调用)
- im:`daemon.rs`(AdapterFactory 签名 + default_adapter_factory 三路由 + 测试)· `gateway.rs`(factory 类型/closure/4 call site、GatewaySession/SavedGatewaySession/SessionView + 字段、start_session +protocol、5 caller、create_session_api wrapper + _proto、`/new` 解析、`/role`/recycle/template、save/load、is_real_claude_tui_handle 泛化、`/screen` 人话拒)
- web:`routes/sessions_api.rs`(CreateSessionForm.protocol + handler + 5 SessionView 字面量)
- web SPA(subagent):`lib/sessionsApi.ts`(SessionView/CreateSessionOpts +protocol/host)· `pages/SessionView.tsx`(终端 tab gate + 提示)· `pages/ChatConsole.tsx`(protocol 选择器)
- cli:`main.rs`(测试 factory closure 2-arg)· `web_chat_bridge.rs`(同)· 多个 im tests factory closure 2-arg

## Baseline

- `cargo test --workspace --exclude ccteam-web` = **1989 / 0**(W2 1985 + 4)
- `ccteam-web` = **279 / 0** · vitest = **145 / 0**(SPA,subagent 报告)
- clippy `--workspace --all-targets -D warnings` = 0 · `cargo fmt --all` 干净

## Remaining(给 Wave 4+)

- **Wave 4**:故障×通道矩阵({断网, daemon 重启, pane/child-death} × {terminal, stream-json}),轴参数化夹具(§七 ③);outbound 不丢不重 / reset 带 sid+reason / stream-json in-flight 丢失人话信号。
- **Wave 5**:@handle//use 摩擦清单、turn 通知必达、web 终端键入/resize/重连一致性、会话列表活动态。
- **Wave 6**:文档 + ship gate(tech-design 两通道 + 协议→代码指针、usage、CLAUDE.md §〇/§一、README、版本归档、Cargo.toml → 0.8.11)。
- **HITL resolver 生产接线**(Decided 1 follow-up)。
