# v0.8.11 Wave 1 handoff — E1 核心 adapter(ClaudeStreamJson)

> 范围:E1 的协议引擎 —— `ClaudeStreamJsonAdapter` 四缝模块 + `HarnessAdapter` 全实现 + fake-vendor 确定性 e2e。slash bridge / HITL = Wave 2;创建面接线 = Wave 3。

## Decided(本 wave 定的事)

1. **第二 adapter,emit 同一 CanonicalEvent**:`ClaudeStreamJsonAdapter` 实现 `HarnessAdapter`,`events()` 产出与 `ClaudeTuiAdapter` 完全相同的 `ThreadEvent` 序列 —— 答案恰好一次 `ItemCompleted{AgentMessage}`、失败走 `TurnFailed`、tool/thinking 走 `ItemStarted{ToolCall}`/`ItemUpdated{Reasoning}`。⇒ gateway `spawn_event_pump`(唯一 turns/progress writer)**零改动**消费。这就是 §七 ④「SoT writer 复用既有 pump」的兑现。
2. **四缝模块**(§七 ①),`execution/claude_stream_json/`:`spawn_spec.rs`(纯 argv/env/cwd + uuid)· `protocol.rs`(NDJSON wire 类型)· `transport.rs`(泛型 `(reader,writer)` 双向 NDJSON,消费端不持 `Child`)· `translate.rs`(Outbound→ThreadEvent)· `mod.rs`(adapter + live 注册表)。
3. **transport 泛型 over `AsyncRead/Write`**(镜像 `codex_jsonrpc`):`spawn_from_io(reader, writer, child)` —— 测试用 `tokio::io::duplex` 驱动脚本 peer;v0.9 satellite 用 WS 半段直接替换。`Child` 私有持有,仅 `shutdown` 触碰。**§七 ② 兑现**。
4. **deterministic uuid = 无状态 resume key**(§七 ⑤):`vendor_uuid = det_uuid(slug, sid)`(FNV-1a,稳定 v4 形)。`start_thread` 按 Anthropic transcript jsonl 是否存在 选 `--resume`(已存)vs `--session-id`(新);resume 失败 → fresh 回退 + `chat_session_reset` 人话事件(镜像 claude_tui)。`resume_thread(uuid)` 在 live session 在时返回 handle,否则 `NotImplemented` → gateway 回退到(resume-aware 的)`start_thread`。
5. **身份映射可扩列**(§七 ⑤):`SessionIdentity { sid, vendor_uuid, host="local" }`,v0.9 同结构挂 Sandbox CR,不一次性双键。
6. **零注入**:`spawn_spec::build_argv` 永不产 `--append-system-prompt`(有单测守);role 仅 `--agent`;空 role 省 `--agent`;不带 `-p`(单测守)。
7. **close 信号**:transport 的 broadcast sender 常驻,子进程死后订阅者收不到 `Closed` → 加 `CloseSignal`(reader EOF / shutdown 时 fire),`events()` `select!` 它并先 drain 再终止(末轮答案不丢)。
8. **fake-vendor = python3**(显式 flush,易加故障档):`FAKE_SJ_{ARGV_LOG,DIE_AFTER_INIT,REPLY,INIT_COMMANDS}`;Wave 4 故障矩阵复用。

## Rejected(考虑过但没做)

- **显式 `initialize` control_request 握手**:改用 `system:init`(claude 自动广播)的 `slash_commands` 当命令表 —— 同一张表、少一次往返、fake 更简单。`request_control` 基础设施已建好(transport,带往返单测),Wave 2 若需富描述再发 initialize。
- **`--add-dir` / `--setting-sources`**:省略。`current_dir(cwd)` 足够;默认 setting-sources = user,project,local 正好让 Wave 3 的 telegram 插件 local 隔离生效。
- **adapter 内 thread tracker/usage 累计**:Wave 1 `thread_status` 只回 init 的 model;context-window 累计留 Wave 2。

## Risks / 诚实差异

- **in-flight turn 不扛子进程死**:stream-json 子进程 stdout 一断,in-flight turn 即丢(恢复只到 `--resume` 粒度)。这是选默认通道的已知代价(PRD E3),Wave 4 补「人话信号」。
- **deterministic uuid vs 真 claude `--session-id` 复用语义**:真机上首 spawn 用 `--session-id <det>`、后续 `--resume <det>`,靠 jsonl 存在判定。极少数竞态(jsonl 尚未落盘就二次 spawn)未覆盖;Wave 4 真机 smoke 验。
- **真机未验**:全部确定性走 fake;真 `claude` 2.1.170 stream-json 串(spawn→init→turn→/compact→resume)留 owner 真机 smoke 清单。

## Files(本 wave 落地)

- 新增 `crates/ccteam-harness/src/execution/claude_stream_json/{mod,spawn_spec,protocol,transport,translate}.rs`
- 新增 `crates/ccteam-harness/tests/claude_stream_json_test.rs`(5 e2e)
- 改 `crates/ccteam-harness/src/execution/mod.rs`(`pub mod` + re-export)
- 改 `crates/ccteam-harness/src/lib.rs`(re-export `ClaudeStreamJsonAdapter`)

## Baseline

- `cargo test --workspace --exclude ccteam-web` = **1975 / 0**(起跑 1942 + 33 新:28 单测 + 5 e2e)
- clippy `--workspace --exclude ccteam-web --all-targets -D warnings` = 0
- `cargo fmt --all` 干净

## Remaining(给 Wave 2+)

- **Wave 2**:handle_directive bridge 三类(known prompt/local 透传 · known dialog 人话拒 · unknown 当文本)+ ccteam IM 命令面优先;HITL(`--permission-prompt-tool stdio` + `can_use_tool` → IM 同意/拒绝,deny 不杀 turn)。命令表已存在 LiveSession.commands(`session_command_table()`)。
- **Wave 3**:gateway adapter_factory 按 `(vendor, protocol)` 选 adapter;`protocol` 创建参数 web/IM 同源;cto 默认 stream-json;`is_real_claude_tui_handle` 放行 stream-json handle 走 start_thread 回退;隐藏终端 tab;screenshot 人话拒;`host` 字段进 session schema;telegram 插件定点隔离。
