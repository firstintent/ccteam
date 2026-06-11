# v0.8.11 Wave 4 handoff — E3 故障×通道矩阵(stream-json 韧性)

> 范围:stream-json 通道的故障韧性 + 轴参数化故障夹具(§七 ③)。terminal 通道的故障由既有 `claude_tui` soak 测试覆盖(不重复)。

## Decided

1. **in-flight 丢失 = 人话信号(核心补缺)**:`StreamTranslator::on_close()` —— transport 关闭(子进程死/EOF)时,若有 in-flight turn(已起、无 `result`),合成 `TurnFailed`(人话:「stream-json 会话回合进行中断开…再发一条会自动 resume 续上」)。pump 把 `err.message` 转 IM → 用户看到信号而非沉默。clean idle close(无 active turn)→ 静默(不误报)。events() 两条关闭分支(`Closed` / `wait_closed`)都先 drain 再 `on_close`。
2. **轴参数化故障夹具(§七 ③)**:`run_fault_case(tmp, channel, fault)` —— `channel × fault` 参数化,host 维是未来加参不重写。`fault ∈ {IdleClose, ChildDeathMidTurn, ErrorResult, DaemonRestartResume}`,channel 当前只 StreamJson(Terminal 由 claude_tui soak 覆盖)。
3. **故障语义断言**:
   - **outbound 不丢不重**:正常 turn 恰好一个 answer(no-dup);
   - **断网代理(ErrorResult)**:claude API 失败 → error-subtype `result` → TurnFailed 带 error kind;
   - **child-death mid-turn**:in-flight 人话信号,turn 不 complete;
   - **daemon 重启**:新 adapter 实例(live map 空)+ transcript 存在 → start_thread 走 `--resume` → 续上(恰一 answer);
   - **idle close**:无 spurious failure。
4. **reset 事件带 sid + reason**:resume→fresh 回退(`FAKE_SJ_DIE_ON_RESUME` 让 `--resume` spawn 死)→ fresh `--session-id` + `chat_session_reset` 写 progress.jsonl,断言含 `s1` + `resume_failed_fallback_to_fresh`。

## Rejected

- **重复实现 terminal 通道故障矩阵**:v0.8.10 D1/D5 已覆盖 tmux soak;夹具留 channel 参数位,不重写既有。
- **真断网 / ACPI suspend**:确定性夹具用 fake NDJSON 脚本(error-result / die-mid-turn / die-on-resume)代理;真机系统级断网/suspend 留 owner smoke 清单(不阻塞)。

## Risks / 诚实差异

- **stream-json 不扛进程中断**:in-flight turn 子进程一死即丢,恢复只到 `--resume` 粒度 —— 这是选默认通道的已知代价(对 tmux 通道的韧性差异);本 wave 把它从「沉默丢失」变成「人话信号 + 自动 resume」,但**不消除**丢失本身。
- **fake 故障档 vs 真机**:`error_during_execution` / die-mid-turn / die-on-resume 是协议层代理;真 claude 的 result error 子类型全集、API 超时行为留真机 smoke。

## Files

- `claude_stream_json/translate.rs`(`on_close` + 2 单测)
- `claude_stream_json/mod.rs`(events() 两关闭分支调 `on_close`)
- tests `claude_stream_json_test.rs`(fake 加 DIE_MID_TURN / ERROR_RESULT / DIE_ON_RESUME;`fault_matrix_stream_json` 参数化 + `resume_failure_emits_reset_event_with_sid_and_reason`)

## Baseline

- `cargo test --workspace --exclude ccteam-web` = **1993 / 0**(W3 1989 + 4)
- clippy / fmt 干净

## Remaining(给 Wave 5/6)

- **Wave 5(E4)**:大半**继承** v0.8.10 机制(turn 通知 reply_to / D6、会话列表活动态 / D9-③、byte-faithful 终端 / v0.8.9);stream-json session 走同一 pump → 同样进 progress.jsonl → 活动态/新回复指示自动继承。新增 = 验证 stream-json 进会话列表活动态 + @handle//use 摩擦清单 + Q4 量化阈值(文档)。
- **Wave 6(E5)**:tech-design 两通道 + 协议→代码指针、usage、CLAUDE.md §〇/§一、README、版本归档、Cargo.toml → 0.8.11。
