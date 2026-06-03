# v0.8.4 review findings(收敛 fix 清单 · 给 dev session)

> Review @ `dev` HEAD `9a11216`(v0.8.4 全 4 phase + ship-gate 已落)。reviewer:meta session(4 路 per-phase 独立审 + 自跑 gate)。
> **gate 实测(非信 commit message)**:`cargo clippy --workspace --all-targets -- -D warnings` = **0 warning**;`cargo test --workspace --exclude ccteam-web` = **1800 pass / 0 fail**。
> **总评:实现忠于设计、可发在即**。socket 路由(无 watcher)、Read 约定已接、ledger multiset+pairing(未复发 v8.2 flake)、channel-neutral —— 关键红线全过。下列为**收敛项**:1 个必修 + 3 个应修 + 若干可选 NIT。

---

## 🔴 必修(失一条 named AC + 反丝滑回归)

### F1 · P1:机器味 ack「submitted … turn …」未折叠,生产每 turn 多发一条
- **现象**:`submit_to_current`(gateway.rs:962-976)在生产路径(`event_sink.is_some()`)只 spawn watchdog、`replies` 恒空 → 974-976 仍 push `"submitted {session_id} turn {turn_id}"`,被 inbound consumer(daemon.rs:487-491)当普通消息发出。pump 另发 `⏳ working…` seed + 答案。⇒ 一个 turn = **ack + seed + 答案 = 3 条**,而 §3-P1 AC = 答案 + status 种子 = **2 条**;且这条 ack 正是 P1 本应消灭的噪声(PRD「顺带吃掉 ack」)。
- **测试为何漏**:`progress_edits_one_status_message_not_spam`(im_progress_test.rs:254)只数 `is_status` 前缀的 send,没数「新消息总数」,ack 落在 outbox 未断言。
- **收敛 fix**:`submit_to_current` 在 `event_sink.is_some()` 时返回**空 replies**(让 pump 的 seed 作唯一 working 指示);spam 测试加 `outbox 内非-edit 新消息数 == 2` 断言。
- **连带**:`inbound_wiring_test.rs` 多个 v0.8.3 老测试断言 ack **被投递**(约 :470 :944 :1032 :1065 :1272–:1440)——本属 ship-gate 该一起改的 reconcile,需同步更新(改成不期望 ack,或期望 seed)。

---

## 🟠 应修(正确性 / AC 字面)

### F2 · P2b:`forward_chat_send_file` 丢弃 daemon 的 `isError`,同步失败被当成功回给 agent
- daemon 侧 `execute_chat_send_file`(main.rs:2245-2257)对 oversized/missing/unregistered 正确置 `isError:true`;但 stdio 转发 `forward_chat_send_file`(mcp_serve.rs:600-606)只取 `/result/content/0/text` 返回 `Ok(...)`,外层 `call_tool` 盖上 `isError:false` ⇒ agent 收到「成功」结果(虽含原因文本)。
- **收敛 fix**:`Ok(resp)` 分支里若 `resp.pointer("/result/isError") == Some(true)` → `return Err(anyhow!(text))`,让 wrapper 产出 `isError:true`。

### F3 · P2a:>20MB 超大文件拒收走「agent 转述」而非直发 chat,弱于 AC「chat 收到拒收提示」
- `stage_attachment` 超限 → note 追加进 `content`(成为 turn 文本,telegram.rs:1477-1484),用户只在 agent 主动转述时才看到;对比 P0 split-failure 是 `channel.send(notice)` 直发。
- **收敛 fix**:超大拒收走与 P0 split-failure 一致的**直发 channel notice** 路径。

### F4 · P2a:`sanitize_attachment_name` 不过滤 `"`,带引号文件名破坏 `<channel image_path="…">` 属性
- telegram.rs:1422-1435 只滤控制符 + `/ \`,未滤 `"`(及 `< >`)。文件名 `foo"bar.pdf` → `image_path="/…/foo"bar.pdf"` 属性被截断 ⇒ 静默错读/读失败(正好踩 P2a 的失败模式),亦轻度注入面。
- **收敛 fix**:`sanitize_attachment_name` 一并 strip/替换 `"` `<` `>`。

---

## 🟡 可选(NIT,不阻发)

- **F5 · P1**:Codex 的 `CommandExecution`/`FileChange` 仅测了 IM 侧 fold(progress.rs:395),未钉死从 **Codex adapter `events()`** 流出(Claude 侧已钉死 transcript_tail.rs:813)。建议补一条 codex_app_server `events()` 测试。
- **F6 · P2b**:单条(非分片)附件 send 失败无 `⚠️` chat notice(failure-echo 只在多片分支 daemon.rs:691-712)——**与既有单条文本答案同行为,非本版回归**;若要彻底,单发失败也直发 notice。
- **F7 · P2a/P2b**:附件 `caption` >1024 UTF-16 单元不分片(附件消息按设计跳过 `split_for_channel`,Telegram caption 上限 1024 ≠ 4096)。caption 实际短,低危;如需,caption 超限截断 + 余文另发一条文本。
- **F8 · P0**:`FENCE_REOPEN_MARGIN=24` + `MAX_MESSAGE_UTF16=3900` 双重余量,安全;纯文档注记即可。
- **F9 · 文档**:handoff 注明「agent 真读图」属**人工 live-bot smoke**(PRD §5),绿测试套 ≠ 已验读图行为。

---

## 验收结论
功能完整、gate 全绿、设计红线全守。**修掉 F1(必)+ F2/F3/F4(应)即可收敛发布**;F5–F9 可随后或并入。

---

## 修复状态(dev session,collapsed)

> gate(实测):`cargo test --workspace --exclude ccteam-web` = **1802/0**;`cargo clippy --workspace --all-targets -- -D warnings` = 0;`fmt --check` 干净。

- ✅ **F1**(必)— `submit_to_current` 在 `event_sink.is_some()` 时返回空 replies(ack fallback 挪回同步分支)。reconcile 了 `inbound_wiring_test.rs`(routing/timeout/WS routes/WS restart/real-ws probe)与 `web_chat_bridge.rs` 三处断言「ack 被投递」的老测试。spam 测试加「无 `submitted` 前缀 + outbox.len()==3(created+seed+answer)」断言钉死回归。
- ✅ **F2**(应)— `forward_chat_send_file` 提取 `forward_outcome`,`isError:true` → `Err` 透传(agent 收到 tool error)。新单测 `forward_outcome_propagates_is_error`。
- ✅ **F3**(应)— 入站超大/失败附件改 `self.send(notice)` **直发 chat**(对齐 P0 split-failure);无 caption 时 `continue` 不产空 turn。(I/O 路径,随真 bot smoke 验。)
- ✅ **F4**(应)— `sanitize_attachment_name` 一并 strip `" < >`;测试覆盖 `foo"bar.pdf`→`foobar.pdf`、`a<b>c.png`→`abc.png`。
- ✅ **F7**(NIT,顺手)— 出站 caption `truncate_caption` 截到 1024 UTF-16 单元(附件不分片,caption 上限≠4096),防 400;新单测。
- ⏸ **F5**(NIT)— 跳过:Codex `CommandExecution`/`FileChange` 的 **parsing** 已有测试(`codex_exec.rs` / `codex_app_server.rs`);仅 events()-级钉死缺,真 nit,可随后补。
- ⏸ **F6**(NIT)— 跳过:reviewer 自评「非本版回归」(单条文本答案同行为)。
- ⏸ **F8/F9**(文档)— F8 双重余量纯注记;F9「真读图属人工 smoke」`handoff.md` 已注。
