# v0.8.4 派工提示词(粘贴到另一个 session)

> 复制下面整段给负责开发的 Claude session。它自包含:指向已提交的 PRD/dev-plan,带每 phase 的 gate 与基线,点明 worktree 流程与唯一开放设计题。

---

你在 `ccteam` 仓库实现 **v0.8.4 IM 日常驱动「最后一公里」**。这是 doc-first 已 review 的版本,设计权威是仓内已提交文档:

- **PRD(架构 + 决策 + 验收)**:`docs/versions/v0-8-4/prd.md` —— **起手必读全文**。
- **执行编排(顺序/worktree/gate)**:`docs/versions/v0-8-4/dev-plan.md`。
- 项目纪律:`CLAUDE.md`(尤其 §三 红线、§五 PR 流程、§七 fmt)。

## 起手 30 秒
1. `git log -1`(应在 dev,HEAD ≈ `a44402b` 或更新);记基线 `cargo test --workspace --exclude ccteam-web 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'`(应 ≈ 1759 0)。
2. 读 `prd.md` 全文,**重点复核 §1「关键架构事实」6 条**是否还成立(双出站路径互斥 / 出站 choke point `send_gateway_outbound` / `Channel` 无 edit+max_len / pump 丢非 AgentMessage / ack 可作 status 种子 / MCP socket handler 只拿 `paths`)。漂了先在 PR 描述记差异再调设计。

## 解决什么(3 硬阻断 → 4 phase,按序独立发)
- **P0 / B2 长消息分片**:出站超 channel 上限时有序分片多发,绝不静默丢失,代码块跨片不破。**先做,最便宜,立即止血。**
- **P1 / B1 进度可见**:turn 进行中 TG 能看到逐步骤进度(命令/文件/思考),答案单独成消息。UX = **live-edited status 消息 + 节流**(对齐官方 telegram 插件;已选定,被否方案见 prd §3-P1)。
- **P2a / B3 入站图文**:TG 发图/文件+caption → agent 能 Read 到落盘路径。
- **P2b / B3 出站文件**:新 MCP 工具 `chat_send_file` → `sendPhoto/sendDocument` 发回 TG。

## 每 phase 怎么做(详 dev-plan)
- 起独立 worktree:`git worktree add -b v0.8.4-pN /tmp/ccteam-v084-pN origin/dev`。
- 实现 + **deterministic 测试**(`CCTEAM_{CLAUDE,CODEX}_BIN` fake adapter / `MockChannel` / fixture JSON;**绝不**依赖真 TG token)。
- **gate(每 PR 必过,退步不发)**:`cargo test --workspace --exclude ccteam-web` ≥ **1759/0** 只增不减 + `cargo clippy --workspace --all-targets -- -D warnings` 0 + `cargo fmt --all -- --check`。
- PR 描述映射痛点 + prd phase + 勾选该 phase AC(见 prd §3 与 dev-plan「验收 gate」)。
- merge 后 `git worktree remove`。

## 三条最容易翻车的硬约束(prd §4 全文)
1. **channel-neutral**:gateway/daemon 里**禁止**出现 `4096` 或 `"telegram"` 分支;分片/编辑一律走 `Channel::max_message_len()`/`edit_message()` trait 多态,常量只活在 `telegram.rs`(web ChatConsole 经 WsChannel 同走这条,硬编会误伤)。
2. **ledger 顺序断言**:P0 把 1 条变 N 条;凡 `DurableOutboundRow` 顺序断言**一律 multiset+pairing,禁 positional/index**(否则复发 v8.2 那个 race flake),同一逻辑消息内串行、跨消息可并发。
3. **不解析 pane / 不动 progress.jsonl schema / 不写迁移兼容分支**;新加的 trait 方法给 default impl、新结构字段给 `#[serde(default)]`,改公共结构 grep 全 impl + caller 一起改。

## P1 的一个显式前置 gate
先写 fake-transcript 测试**钉死** `ItemCompleted{ToolCall/CommandExecution/FileChange}` 确实从 adapter `events()` 流出,再建进度状态机(别假设)。进度粒度 = 每步骤完成(transcript 不暴露 token 流),别去追流式。

## P2b 设计已定 = socket 路由(prd §3-P2b ④,**不要**新建 file-watcher)
通用原语 =「agent 主动向自己绑定 chat 发一条出站消息(文字 and/or 附件)」,发截图只是 instance。3 统一 + 1 桥:
- **统一信封**:`SendMessage`/`GatewayEvent` 加 `attachments`;Telegram `send` attachments 非空走 `sendPhoto/sendDocument`,否则 `sendMessage`。
- **统一寻址**:`chat_send_file(path, caption?, kind?)` **零寻址参数**,身份取 `CCTEAM_CHAT_{SLUG,ROLE}` env;daemon 按 slug/role 用 registry `(im_platform,im_chat_id)` 解析"home chat"。
- **统一出口**:构造 `GatewayEvent{attachments}` → 既有 consumer → `send_gateway_outbound`(白嫖 P0 分片 + ledger + 失败回显)。
- **传输桥**:stdio mcp-serve 把该工具**转发到既有 `mcp.sock`**;`run_start` clone `GatewayEvent` sink 进 `serve_mcp_socket`/`handle_mcp_socket_connection`,daemon 侧解析+入队+**同步**返回 `delivered`/`failed`。理由:daemon 不再 file-watch(inbound 走内存 mpsc,inbox 文件只存档),新建 watcher = 复活已退役 file-watch + inotify 老坑。
- **render ⊥ deliver**:`screenshot` 维持只渲染返回 path;`chat_send_file` 只投递;组合即"发效果图"。
- **显式假设**:`path` 假设 daemon/agent 共享文件系统(remote `ProcessBackend` 会破,本版只记假设);in-turn 寻址无歧义,out-of-turn/一 bot 多 chat 用 registry 兜底。

## 收尾(最后一个 PR = ship-gate,prd §6)
版本 bump `0.8.3→0.8.4` + CLAUDE.md §一 baseline 回填 + tech-design 协议→代码指针表 + README(英)/usage.md + 各 phase handoff(Decided/Rejected/Risks/Files/Remaining 五段)。

**红线**:每 phase baseline ≥ 上一 phase;clippy 0 warning;fmt 干净。开干前再读一遍 `prd.md`。
