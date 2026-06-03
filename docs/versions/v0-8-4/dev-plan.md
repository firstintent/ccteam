# v0.8.4 dev-plan —— 执行编排

> 配套 `prd.md`。本文只讲**怎么把 4 个 phase 落地**:顺序、worktree、PR、gate、收尾。架构与验收以 `prd.md` 为准。

## 顺序与依赖

```
P0 分片(B2)  ──►  P1 进度(B1)  ──►  P2a 入站图文(B3-in)  ──►  P2b 出站文件(B3-out)
   止血/最便宜       依赖 P0 拓扑           高价值/中等             最重/socket 路由
```

- **P0 必须先合**:P1 的答案投递要复用 P0 的分片(长答案否则仍丢)。
- P1 依赖对「双出站路径互斥」拓扑的理解(prd §1.1),不依赖 P2。
- P2a / P2b 可在 P1 之后并行起 worktree(传输层字段不冲突即可;若都改 `transport/mod.rs` 的同段,串行更稳)。

## 每个 phase 的固定动作

1. **起 worktree**:`git worktree add -b v0.8.4-pN /tmp/ccteam-v084-pN origin/dev`(从 dev 起,不从 main)。
2. **起手复核拓扑**:按 prd §1 grep 复核 6 条事实没漂移(尤其双路径互斥 + choke point + MCP socket 句柄)。漂了就先在 PR 描述里记差异,再调设计。
3. **实现 + deterministic 测试**(fake adapter / MockChannel / fixture;**不**依赖真 TG token)。
4. **gate**(prd §4.3):`cargo test --workspace --exclude ccteam-web` ≥ 1759/0 + clippy `-D warnings` 0 + `cargo fmt --all -- --check`。退步不发 PR。
5. **PR**:描述映射 `requirements.md` 痛点(IM 日常驱动)+ `prd.md` 对应 phase + 列 AC 勾选。
   - ⚠️ 若改了 `.github/workflows/*` 用 SSH 推(`git@github.com:firstintent/ccteam.git`);本版大概率不碰。
6. **review/fix/merge** 后 `git worktree remove`。

## 各 phase 验收 gate(摘自 prd,PR 必须逐条勾)

- **P0**:5000 字符答案 → ≥2 条有序、拼接 == 原文;fence 跨片各自闭合;emoji-heavy(UTF-16>4096 但 char<4096)正确分片;`None` limit 行为不变;**ledger 断言 multiset+pairing 非 positional**(prd §4.2)。
- **P1**:fake 事件序列 → status 消息出现且被 edit、答案独立成消息、新消息数=1+1 不刷屏;`CCTEAM_IM_PROGRESS=off` 只发答案;Codex delta 0 条独立消息;**显式钉死 `ItemCompleted{ToolCall/CommandExecution/FileChange}` 确从 `events()` 流出**。
- **P2a**:图+caption → `<channel … image_path=…>` + 落盘;**无人工提示下 agent 主动 Read 该图**(= ccteam 自建的 Read 约定指令已生效,见 prd §3-P2a 静默失败陷阱);>20MB 拒收不崩;纯文本不变。
- **P2b**:`chat_send_file`(零寻址参数,身份取 `CCTEAM_CHAT_{SLUG,ROLE}` env)→ 用户收到图;不存在/超限结构化 error + chat 一行;同步返回 `delivered`/`failed`;MCP `--verify-mcp` + `STUB_TOOLS` drift 不触发。**设计已定 = socket 路由**(prd §3-P2b ④:stdio mcp-serve 转发到既有 `mcp.sock` + `run_start` 注入 `GatewayEvent` sink;**不**新建 file-watcher)。复用 `send_gateway_outbound`(白嫖 P0 分片 + ledger)。

## 收尾(最后一个 PR,ship-gate)

按 prd §6:版本 bump `0.8.3→0.8.4`、CLAUDE.md §一 baseline 回填、tech-design 协议→代码指针表、README(英)+ usage.md、各 phase handoff(五段)。

## 测试基础设施备忘(踩坑)

- env-mutating 测试(`CCTEAM_IM_PROGRESS` 等)放 `crates/*/tests/*.rs` integration(独立进程),**不**放 lib `#[cfg(test)] mod tests`。
- 节流测试用可配阈值 env(`CCTEAM_IM_PROGRESS_THROTTLE_MS`)注入,**不**靠真 sleep。
- 改 `Channel` trait / `SendMessage` / `ChannelMessage` 公共结构 → grep 全 impl(telegram/slack/discord/mock/ws)+ 全 caller 一起改;新字段一律 `#[serde(default)]`(持久化 ledger/state 向前兼容读)。
- `ccteam-web` 的 ws_* 测试留 CI/专机,本机 baseline 用 `--exclude ccteam-web`。
- WSL/inotify-busy 宿主的 watcher/SSE 502 是环境层,不计入 baseline。
