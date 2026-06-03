# v0.8.4 — IM 日常驱动「最后一公里」

v0.8.4 把「cd 进已有项目 → `ccteam init` → 在 Telegram 里日常开发」从「能跑通
smoke」推到「能丝滑替代 TUI」,解决三个每天都撞的硬阻断:**长消息分片 /
进度可见 / 图文 I/O**。基准线是用户已在用官方 `plugin:telegram` 驱动 Claude
Code——ccteam 网关在多项目/多 session 路由 + 双 vendor + 成本预算上更强,本版
补齐它在「分片」「图文」上的短板。

四个独立可发、独立 verify-gated 的 phase:

## 落地内容

- **P0 长消息分片 (B2)** — `Channel::max_message_len()`(常量只活 `telegram.rs`)
  + `sanitize::split_for_channel`(UTF-16 预算、plain-text 无损、代码 fence 跨片
  闭合/重开)接入唯一出站 choke point `send_gateway_outbound`,每片独立 durable
  row;部分失败回 chat 一行,不再静默丢数据。
- **P1 进度可见 (B1)** — `progress::ProgressFold`(分组折叠 `📖 read ×5 · 🔧 bash ×3`
  + ≤200 字符入参预览)+ `Channel::edit_message`;event pump 区分答案(新消息)
  与进度(每 turn 一条 live-edit status,节流 + 去重),收尾 `✅ done · n tools · m files`。
  Codex 流式 delta 不再单独刷屏。`CCTEAM_IM_PROGRESS=off` 可回退只发答案。
- **P2a 入站图文 (B3-in)** — TG 发图/文件 + caption → `getFile` 下载 staging →
  turn 文本包 `<channel … image_path=…>`;**Read 约定**经 ccteam 自己的 MCP server
  `instructions` 下发(裸 claude 不会自动读图,必须被教)。
- **P2b 出站文件 (B3-out)** — `chat_send_file(path, caption?, kind?)` 零寻址参数
  (身份取 `CCTEAM_CHAT_{SLUG,ROLE}` env,daemon 查 registry 解析 home chat),
  走既有 `mcp.sock` 转发 + `run_start` 注入 `GatewayEvent` sink(**非**新 file-watcher),
  复用 `send_gateway_outbound` 出站漏斗;Telegram `sendPhoto/sendDocument`。
  MCP 工具 27→28,0 stub。

## baseline 进程

`cargo test --workspace --locked --no-fail-fast --exclude ccteam-web`:
1764/0 起点 → P0 1772 → P1 1784 → P2a 1792 → P2b 1799。
clippy `-D warnings` 0、`cargo fmt --all -- --check` 干净,全程未退。

## 设计 / 验收

- 架构 + 决策 + 验收:`prd.md`
- 执行编排:`dev-plan.md`;派工说明:`dev-prompt.md`
- 五段式 phase handoff:`handoff.md`
- 协议 → 代码指针表:`docs/tech-design.md` §12(已补 max_message_len / edit_message /
  附件字段 / chat_send_file / Read 约定)

## 人工 gate(超出自动化)

各 phase 已用 deterministic fake(`MockChannel` / fixture / fake unix socket /
fake transcript)覆盖逻辑;真 bot-token 活体 round-trip(分片、进度编辑、发图读图、
`sendPhoto`)留作一次性人工 smoke。
