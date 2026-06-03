# v0.8.4 handoff —— IM 日常驱动「最后一公里」

> 4 phase 各一段,固定五段式:**Decided / Rejected / Risks / Files / Remaining**。
> 设计权威 = `prd.md`;协议细节以代码为准(`tech-design.md` §12 指针表已补)。
> baseline 进程:1764/0(起点)→ P0 1772 → P1 1784 → P2a 1792 → P2b 1800,clippy 0 / fmt 干净 全程未退。

---

## P0 —— 长消息分片 (B2)

**Decided**
- `Channel::max_message_len() -> Option<usize>`(default `None`);只 `TelegramChannel` override = `Some(3900)`,4096 常量只活 `transport/providers/telegram.rs`(channel-neutral)。
- `sanitize::split_for_channel(text, max_units)`:按 **UTF-16 code unit** 预算(`char::len_utf16`);plain text 无损(`concat == 原文`);断点优先 段落 > 行 > 空白 > 硬边界(均在后半窗,避免碎片);代码 fence 跨片 **闭合 + 重开**(保留 lang)。
- 接入唯一 choke point `daemon::send_gateway_outbound`:`Some(limit)` 且超限 → 切片,每片独立 durable row `{inbound_id}-{seq}-{part}` 顺序发;1 片维持旧 id `{inbound_id}-{seq}`(行为不变)。
- 失败可见:多片部分失败 → 回 chat 一行 `⚠️ 部分消息发送失败 (part k/N)`;`finish_durable_outbound_send` 返回投递成功 bool。

**Rejected**
- 在 `TelegramChannel::send` 内部分片(藏多消息语义,丢 ledger 可观测性)。
- 截断 + 省略号(丢数据)。

**Risks**
- emoji / 补充平面按 char 计会超 → 已按 UTF-16 单元预算 + 保守 3900 留头。
- fence lang 串很长时 reopen 余量(`FENCE_REOPEN_MARGIN=24`)覆盖 ≤16 单元 info;超长 lang 极少见,记为已知边界。

**Files**: `sanitize.rs`、`transport/mod.rs`(trait)、`transport/providers/{telegram,mock}.rs`、`daemon.rs`、`tests/inbound_wiring_test.rs`。

**Remaining**: 真 bot-token 活体分片 round-trip(人工 gate)。

---

## P1 —— 进度可见 (B1)

**Decided**
- **前置 gate**:harness 测试 `read_new_surfaces_tool_and_reasoning_events` 钉死 `ToolCall`/`Reasoning`/`AgentMessage` 确从 `transcript_tail::read_new` 流出。**实测结论**:Claude 把每个工具表成 `ItemStarted/ItemCompleted{ToolCall{name}}`(Bash/Read/Edit…),**不**拆 `CommandExecution`/`FileChange`(那俩是 Codex-only),且 **无 Turn 事件** → 状态机按工具名分桶、按 item.id 去重、status epoch 以「上一条答案之后」划界。
- `Channel::edit_message`(default = 发新消息降级);Telegram `editMessageText`。
- `progress::ProgressFold`:借 claude-code `GroupedToolUseMessage`/`CollapsedReadSearchGroup` 做分组计数(`📖 read ×5 · 🔧 bash ×3`)+ `truncate_for_preview`(≤200)+ 最近 1–2 条明细 + 收尾 `✅ done · n tools · m files`。
- pump 改造:`async_event_text` 命中 → 答案(新消息,经 P0 分片,会 ping);否则 `fold.apply` → 进度(每 epoch 一条 status,send-first/edit-after)。节流 ≥1500ms(`CCTEAM_IM_PROGRESS_THROTTLE_MS`)+ 相同渲染去重(防 TG "message not modified")。
- `GatewayEvent.kind = Answer | Progress{status_key, done}`;daemon `deliver_progress` 维护 `status_key → message_id`。Codex `ItemUpdated{AgentMessage}` delta 喂 `✍️ drafting…`,**不**单独发。开关 `CCTEAM_IM_PROGRESS=off`。

**Rejected**
- 每个进度事件发一条独立消息(刷屏)。
- 把进度写 progress.jsonl 让 web 读(那是 state SoT,IM 进度是投递层 UX)。
- 进度计入 `visible_events`(会改 turn-timeout 语义)→ 维持答案-only 计数,不动既有超时机制。

**Risks**
- TG 编辑软限 ~1/s → 节流 + 合并缓冲(`tokio::select!` flush timer)+ 相同文本跳过编辑。
- 长 turn 无答案 > turn-timeout 仍会触发既有超时提示(本版未改 timeout 语义,记为既有行为)。

**Files**: `progress.rs`(新)、`transport/mod.rs`、`transport/providers/{telegram,mock}.rs`、`gateway.rs`(pump + helpers + `GatewayEventKind`)、`daemon.rs`(consumer 分支)、`tests/im_progress_test.rs`(新)、`crates/ccteam-harness/.../transcript_tail.rs`(前置 gate 测试)。

**Remaining**: 真 TG 编辑节奏 / 心跳观感人工 gate。

---

## P2a —— 入站图文 (B3-in)

**Decided**
- `ChannelMessage.attachments: Vec<ChannelAttachment>`(`#[serde(default)]`,~30 构造点同步)。
- Telegram `listen`:解析 `photo`(取最大尺寸)/`document`/`caption` → `getFile` → 下载到 staging `~/.ccteam/imd/attachments/inbound/<cid>-<name>`;mime 决定 Image/File;20MB 拒收 + 文件名 sanitize(去 traversal/控制符)。
- `Gateway::handle_message(channel, chat_id, user, message_id, text, attachments)`;`handle_text` 退化成无附件薄封装(~70 既有测试不动)。附件非空 → turn 文本包 `<channel … image_path="/abs">caption</channel>`。
- **Read 约定(load-bearing)= ccteam 自己的 MCP server `instructions`**(`mcp_serve.rs::CCTEAM_MCP_INSTRUCTIONS`,`initialize` 下发):裸 claude 不会自动 Read 附件路径,必须被教;官方 telegram 插件即如此教。

**Rejected**
- base64/OCR 塞进 turn 文本(污染上下文、丢文件语义)→ 落盘给路径 + Read。
- 抄 CC `AttachmentMessage`/content-block(走 API 注入,**不适用** send-keys 纯文本)。

**Risks**
- 「agent 主动 Read」依赖 MCP instructions 真被 client 注入(官方插件机制同款,已知可行);确定性测试覆盖「turn 文本含 image_path」+「instructions 含 Read 约定」,真·读图是人工 gate。
- 共享文件系统假设(daemon 与 agent 同机);remote ProcessBackend 破,记为假设。

**Files**: `transport/mod.rs`、`transport/providers/telegram.rs`、`gateway.rs`(`handle_message` + `wrap_inbound`)、`daemon.rs`(consumer 透传)、`mcp_serve.rs`(instructions)、+ ~30 `ChannelMessage` 构造点。

**Remaining**: 真 bot 发图 → agent 主动读图的活体验收(人工 gate)。

---

## P2b —— 出站文件 (B3-out)

**Decided**(通用原语「agent 向自己 chat 发一条出站消息」,发截图是 instance)
- **统一信封**:`SendMessage.attachments` / `GatewayEvent.attachments`(`OutboundFile{path, caption, kind}`);Telegram `send` 附件非空 → `sendPhoto/sendDocument`(multipart,caption 在首个);空 → 旧 `sendMessage`。
- **统一寻址**:`chat_send_file(path, caption?, kind?)` **零寻址参数**;身份取 spawn 注入的 `CCTEAM_CHAT_{SLUG,ROLE}` env;daemon `resolve_home_chat(slug,role,bots)` 查 registry `(im_platform, im_chat_id)`。
- **统一出口**:解析 → `GatewayEvent{attachments}` → 既有 consumer → `send_gateway_outbound`(白嫖 P0 分片 + ledger;附件消息不分片)。
- **传输桥 = 既有 `mcp.sock`**(非新 file-watcher):stdio mcp-serve 检测 `chat_send_file` → 注入 slug/role → 转发到 `mcp.sock`;daemon `handle_mcp_socket_connection` 在 `handle_request` 前**拦截**(防回环)→ `resolve_home_chat` + 校验文件 + 入队 sink + 同步返回 delivered/failed。`ccteam start` 建共享 channel,sink clone 进 IM daemon 与 socket(`DaemonArgs.gateway_event_{tx,rx}`)。
- **render ⊥ deliver**:`screenshot` 仍只渲染;组合 `chat_send_file(path)` 即发效果图。MCP 工具 27→28,0 stub。

**Rejected**
- 新建 daemon outbox file-watcher(复活已退役 file-watch + inotify 老坑;fire-and-forget 无法同步报错)。
- `FileChange` 自动发文件(噪声爆炸 + 语义错)。
- sentinel 字符串塞答案被解析(脆弱、蹭红线)。
- 给工具加 `chat_id`/`slug`/`role` 显式寻址(把寻址泄漏进 agent;一 bot 多 chat 即破)。

**Risks**
- 首条 live「MCP→daemon 内存」路径 → 用既有 mcp.sock + sink 注入,同步返回错误;非新 watcher。
- `path` 共享文件系统假设(remote 破,记为假设)。
- out-of-turn / 一 bot 多 chat → registry home-chat 兜底;in-turn 精确 reply_to 推后。

**Files**: `transport/mod.rs`(`OutboundFile`/`OutboundFileKind` + `SendMessage.attachments`)、`transport/providers/telegram.rs`(multipart)、`gateway.rs`(`GatewayEvent.attachments`)、`daemon.rs`(consumer + `DaemonArgs` channel 字段,去 `Clone`)、`lib.rs`(`resolve_home_chat`)、`mcp_chat_tools.rs`(工具)、`mcp_serve.rs`(stdio 转发)、`main.rs`(socket sink 注入 + 拦截 + `run_chat_send_file`/`build_send_file_event` + run_start 接线)、`Cargo.toml`(reqwest `multipart`)。

**Remaining**: 真 TG `sendPhoto/sendDocument` multipart 活体(人工 gate)。
