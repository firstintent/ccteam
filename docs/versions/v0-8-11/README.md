# v0.8.11 — 协议轴:ClaudeStreamJson adapter + 壳加厚(冻结归档)

> 冻结里程碑。当前架构看 `CLAUDE.md` + `docs/tech-design.md`(代码是唯一 SoT)。本目录另含 `prd.md`(E1–E5 + §四决策 + §七 v0.9 准备度)、`dev-plan.md`(wave 划分)、`wave-{1..6}-handoff.md`。

## 一句话

新增 **`ClaudeStreamJsonAdapter`** —— Claude 第二 spawn 路径(长驻 `claude` 子进程 + 双向 NDJSON 管道,无 PTY/pane/hook 链),与既有 `ClaudeTuiAdapter`(tmux)并存;session 新增第三 facet **`protocol`**(`stream-json` 默认 | `terminal`),web/IM/SPA 创建面同源,cto 默认 stream-json。继续 v0.8.10 方向加码故障韧性(故障×通道矩阵 + in-flight 丢失人话信号)。两条 Claude 通道 emit **同一 `CanonicalEvent`** → gateway pump 零改动消费。

## 交付物(E1–E5)

- **E1 · ClaudeStreamJson adapter**:四缝模块(`spawn_spec` 纯 argv/env/cwd + 确定性 per-(slug,sid) uuid · `transport` 泛型 `(reader,writer)` 双向 NDJSON,消费端不持 `Child` · `translate` NDJSON→`ThreadEvent` · `mod` adapter + live 注册表 + `SessionIdentity{sid,vendor_uuid,host}`);不带 `-p`;`--session-id` mint sid↔uuid;init 握手存命令表;slash bridge 三类(known 透传 / dialog 人话拒 / unknown 当文本);HITL `can_use_tool` 同意/拒绝(deny 只挡该次工具,可插拔 `CanUseToolResolver`);idle 关 stdin + 按 sid `--resume`;**零注入**(persona 仅 `--agent`,禁 `--append-system-prompt`)。
- **E2 · 创建面**:`SessionProtocol` 枚举 + `default_adapter_factory(vendor, protocol)` 三路由;`protocol` 参数 web/IM 同源(`/new … terminal`);cto 默认 stream-json;stream-json session 隐藏终端 tab + `/screen` 人话拒;session schema 预留 `host` 字段(默认 `local`);telegram 插件定点隔离(`enabledPlugins.{telegram@claude-plugins-official}=false`,只关这一个)。
- **E3 · 故障×通道矩阵**:轴参数化夹具 `通道 × 故障`({IdleClose, ChildDeathMidTurn, ErrorResult, DaemonRestartResume});outbound 不丢不重(恰一 answer)· reset 事件带 sid+reason · **stream-json in-flight 丢失人话信号**(`on_close` → `TurnFailed`)。
- **E4 · 寻址 + 活动态**:turn 完成通知 / 新回复指示 / 终端一致性 = 继承 v0.8.10/v0.8.9(协议轴透明);**真缺口已补** = stream-json session(无 hook)的会话列表活动态 —— pump 为它写 `chat_turn_completed` 到 progress.jsonl。
- **E5 · 文档 + ship gate**:本归档 + tech-design / usage / CLAUDE.md / README 回填 + `0.8.11` version bump。

## §七 v0.9 准备度(实现形状)

E1 四缝按 v0.9 `SessionHost` 契约预折叠:transport 消费端不持 `Child`(WS 透明替换位);命名 `protocol` 非 `backend`(`backend` 留宿主轴);故障矩阵轴参数化(留 host 维);SoT writer 复用既有 pump(satellite runner 可复用);`SessionIdentity{sid,vendor_uuid,host}` 可扩列(v0.9 同表挂 Sandbox CR)。

## 已知 follow-up(owner 清单)

- **HITL 生产 resolver 接线**:stream-json adapter 在工厂里不带 resolver → hitl stream-json session default-deny(安全向);默认 posture=skip 常路不受影响。接线 = late-bind resolver → daemon `permission/ask` → IM。
- **真机/真 vendor smoke**:全部确定性走 python NDJSON fake;真 `claude` 2.1.170 stream-json 串(spawn→init→turn→/compact→HITL→idle→resume→daemon 重启)+ 系统级断网 / suspend 留真机。
- **MCP/web screenshot 工具对 stream-json**:走既有「无 pane→降级」消息(stdio mcp-serve / 已无 gateway 的 route 无 protocol 上下文);IM `/screen` 给 protocol 专属人话拒。

## 数据迁移

**无迁移**(pre-v1.0 纪律):pre-v0.8.11 state 文件无 `protocol`/`host` 字段 → serde-default 恢复为 `stream-json`/`local`。不兼容时清 `~/.ccteam/` + 各项目 `.ccteam/` → 重 `ccteam init`。

## Baseline(ship 时)

`cargo test --workspace --exclude ccteam-web` = **1994/0** · `ccteam-web` = **279/0** · vitest **145/0** · clippy `--workspace --all-targets -D warnings` = 0 · `cargo fmt --all` 干净。
