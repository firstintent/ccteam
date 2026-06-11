# v0.8.11 Wave 5 handoff — E4 寻址 + 活动态

> 范围:E4 大半**继承** v0.8.10/v0.8.9 机制(协议轴对它们透明);本 wave 的实代码 = 补一个真缺口:stream-json session 的会话列表活动态。其余为继承验证 + 文档。

## Decided

1. **stream-json session 进会话列表活动态(真缺口已补)**:stream-json session 无 chat-progress hook,故 gateway pump 是它**唯一**的 progress.jsonl writer(E1「直写 progress」本意)。pump 在 stream-json session 的 `TurnCompleted` 上写 `chat_turn_completed`(带 sid + usage)→ 活动态分类器(按最新 sid-tagged 事件算 `last_activity_seconds`)正常工作。**gate on `protocol.is_stream_json()`** —— tmux session 由 Stop hook 写,避免双写。
2. **cost 无回归(澄清)**:`cost_summary` 只 sum `agent_done.cost_usd`;chat session(tmux + stream-json 都)从不走这条 —— stream-json 与 tmux chat 等价,**无 cost 缺口**。
3. **turn 完成通知必达对的 chat = 继承**:pump 的 `reply_to`(谁最后驱动就回谁)+ owner fallback,protocol 无关 → stream-json 自动继承(v0.8.10 D6)。
4. **新回复指示 = 继承**:out-of-focus 前缀(`[sid project vendor role] …`)给非聚焦 session 的异步回复打标,protocol 无关 → 继承(v0.8.10 routing-isolation)。
5. **web 终端键入/resize/重连一致性 = 继承 + 不适用 stream-json**:终端通道(tmux)= v0.8.9 逐字节保真,本版不动;stream-json session 隐藏终端 tab(Wave 3),不踩终端面。

## @handle / /use 摩擦清单(dogfood,落 usage 文档 Wave 6)

- `/use <sid>` 切当前会话;`@handle` 单条寻址(不切当前)。stream-json 与 terminal session 寻址面**完全一致**(协议对寻址透明)。
- 摩擦点(观察,非 bug):① `/sessions` 列表需带 protocol 列让用户知道哪些能 `/screen`(SPA 已用终端 tab 隐藏表达;IM 端靠 `/screen` 人话拒兜底);② roleless session handle 回退 sid(v0.8.8),`@s3` 可寻址。

## Q4 量化阈值(终端通道,观察基线)

> 终端通道 = tmux byte-faithful(v0.8.9),本版未改;阈值为**观察基线**,非新增度量代码(真机量化留 owner soak)。

- 键入回显:本机 tmux send-keys p95 < ~50ms(SUBMIT_ENTER_SETTLE=1s 是提交节流,非回显延迟)。
- reconnect 屏幕一致性:rmux byte-stream(`output_stream`/`PaneOutputChunk::Bytes`)重连重放 backlog → 逐字节一致(v0.8.9 已立)。
- stream-json session:无终端,N/A。

## Files

- `gateway.rs`:pump 捕获 `progress_path` + stream-json `TurnCompleted` → `build_chat_turn_completed_event` 写 progress.jsonl(gate on protocol);FakeAdapter 加 `emit_turn_boundary` opt-in flag + `with_turn_boundary()`;新增 `stream_json_pump_mirrors_turn_to_progress_jsonl` 测试。

## Baseline

- `cargo test --workspace --exclude ccteam-web` = **1994 / 0**(W4 1993 + 1)
- `ccteam-web` = **279 / 0** · clippy/fmt 干净

## Remaining(给 Wave 6)

- **Wave 6(E5)= ship gate**:tech-design harness 节(两通道并存 + 协议→代码指针补 stream-json)、usage.md(/new … terminal、stream-json 默认、@handle//use)、CLAUDE.md §〇/§一回填、README(英文)、版本归档 README + handoff 收口、workspace `Cargo.toml` version → `0.8.11`。**绝不打 tag**。
- HITL 生产 resolver 接线(Wave 3 Decided 1 follow-up,owner 清单)。
