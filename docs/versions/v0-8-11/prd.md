# v0.8.11 PRD(候选,doc-first 待 user review)— 壳加厚:stream-json 默认通道 + 恢复 soak-clean + 终端寻址体验

> 状态:**候选初稿**(2026-06-10),doc-first;user review 冻结 scope 后才动代码。
> 来源:stream-json 协议研究(TG 1079–1087)+ user 决策:加入版本需求(TG 1088)、参考 alleycat(TG 1089)、web 创建默认 stream-json / rmux 高级选项(TG 1090)、定版号 v0.8.11 + 主线=「继续 v0.8.10 方向加码核心流程生产级稳定性 + 终端体验」(TG 1091)。
> 关系:上版 v0.8.10 = 零新功能 hardening(已落地 dev);原占位本号的 track-upstream 市场 PRD 顺移 `docs/versions/v0-8-12/prd.md`(正交,排序 user 定)。
> 本版立场:**延续「壳是唯一护城河」**(`references/ccteam-future-value-brainstorm.md`)—— 壳加厚的两面:把**默认 chat 通道做薄做稳**(E1/E2 stream-json:更少活动部件 = 更少故障面),把**恢复语义与终端通道做顺**(E3/E4)。这是壳变厚、别人抄不动的唯一路径。

---

## 〇、一句话

新增 **ClaudeStreamJson harness adapter** 并把 web/IM 新建 session **默认**切到它(rmux pane 降为「终端」高级选项)—— chat-only 主流路径整类甩掉 PTY/pane/hook 链故障面;同时**继续 v0.8.10 方向加码**:sid session 的断网 / daemon 重启 / pane-death(/child-death)恢复做到 **soak-clean**,web chat-shell + 逐字节终端的**多 session 寻址体验**打磨到「像本地终端一样顺」。

## 一、动机

1. **研究结论**(2026-06-10,`docs/research/cc-stream-json-protocol.md`,活体实验 + 逆向 + alleycat 对照):
   - 历史 blocker「TUI slash 在 stream-json 下怎么办」**不存在**:prompt/local 两类 slash(含 /compact /clear /context + 全部 skills)发纯文本即执行(CLI 端确定性合同,零模型参与);dialog 类(local-jsx)headless 不暴露,正解 = 客户端化(ccteam 已有 IM 命令面 + web Settings,与 VS Code「原生 UI + control_request」同构)。
   - **出站 hook 链路整条可省**:Stop→`result`(自带 cost/usage)、回复→流内 `assistant`、工具活动→流内 `tool_use/tool_result`、HITL→`can_use_tool` 同步 RPC;「hook→HTTP→daemon→文件」缩成「daemon 读 stdout→直写」。
   - **vendor 一等公民接口,非 hack**:VS Code 官方扩展即此协议(长驻进程 argv 实拍);`--session-id`/`--resume`/`--agent`/`--include-partial-messages`/`--replay-user-messages` 均为公开 flag(对 2.1.170 验证)。
2. **壳加厚 = 结构性工程**:恢复语义(不丢不重、resume 续上下文、失败必有人话信号)和多 session 终端体验不是 feature 清单,是 v0.8.10 已建度量(soak harness)上的持续加码。

## 二、交付物 E1–E5

- **E1 · ClaudeStreamJsonAdapter(Claude vendor 第二 spawn 路径)**
  - argv:`claude --input-format stream-json --output-format stream-json --include-partial-messages --verbose --replay-user-messages --debug --debug-to-stderr [--agent <role>] [--permission-prompt-tool stdio | --dangerously-skip-permissions] [--session-id <uuid> | --resume <uuid>]`(flags 已对 2.1.170 `--help` + 活体实验验证;**不带 `-p`**,§四 Q1 决策)。长驻 child、stdin 常开;停 = 关 stdin 优雅退出。
  - **身份**:spawn 即 `--session-id` mint vendor-UUID ↔ ccteam sid 双射,登记 gateway session map —— 在新通道根除 tmux 路径的 transcript-discovery rendezvous 脆点(v0.8.10 §四.b 头号脆点)。
  - **outbound**:daemon 单点消费 stdout → `CanonicalEvent` → `progress.jsonl`/`turns.jsonl` 直写(此类 session **无** hook / settings.local.json hook 段 / pane-env / X-Ccteam-Sid forwarder 链);schema 权威仍 `harness/progress_bridge`,事件 taxonomy 复用不新增权威。
  - **HITL**:hitl session 走 `--permission-prompt-tool stdio`,`can_use_tool` control RPC → IM `[同意][拒绝]`(复用既有 permission/ask 面);deny 只挡该次工具不 kill turn(红线不变);skip session 走 `--dangerously-skip-permissions`(现状对齐)。
  - **slash 政策 = bridge 式**(非 VS Code 全透传式;依据:Anthropic 自家 IM 形态 Remote Control 也选保守侧):daemon 持 initialize response 命令表做前置校验 —— known prompt/local 透传为 user text;known dialog(local-jsx)人话拒绝;unknown 当纯文本;ccteam 自有 IM 命令面(`/pair /cd /use /new /role @handle`)优先于 vendor 表。`/compact /new /clear` 完全透传红线照守。
  - **红线兑现表**:No-prompt-injection 由 `--agent` 兑现(**禁用** `--append-system-prompt` / initialize 的 systemPrompt 字段 —— 接口存在 ≠ 使用);roleless = 省略 `--agent`;「不解析终端输出」天然满足(无终端);idle 释放 = 关 stdin + 按 sid `--resume`(≡ 既有 resume-by-session-id 红线语义,非主动 kill);budget auto-disable 例外不变。
- **E2 · web/IM 创建面:backend 选择**
  - 新建 session 增加 backend 枚举:**`stream-json`(默认)** | **`terminal`**(rmux pane,标注「高级:需要终端镜像 / attach / screenshot 时选」);默认值 = daemon 级单一常量,web/IM 同源。
  - stream-json session 的 web 视图只有 Chat tab(终端 tab 隐藏 + 一句提示);`screenshot` 工具对其人话报错。存量 session 不迁移。
  - v0.8.10 的「集合不增长」OUT-gate guard 是版本纪律,本版按新 scope 重置基线(guard 机制保留)。
- **E3 · 恢复 soak-clean(主线加码,继承 v0.8.10 D1/D5 度量)**
  - 故障 × 通道矩阵:{断网, daemon 重启, pane-death / child-death} × {terminal(pane), stream-json} 全矩阵 deterministic CI-fake 断言(`CCTEAM_CLAUDE_BIN` fake 喂 NDJSON 脚本)+ 真机短 smoke。
  - 验收语义:outbound 不丢不重(ledger 幂等);reset 事件必带 sid + reason;resume 续上下文;**stream-json 通道 in-flight turn 丢失必有人话信号**(选默认通道的诚实代价,播报而非掩盖 —— 这是与 tmux 通道的已知韧性差异:tmux 的 in-flight turn 扛 daemon 重启,stream-json 不扛,只恢复到 resume 粒度)。
- **E4 · 多 session 寻址 + 终端「像本地终端一样顺」**
  - IM:@handle / `/use` 切换摩擦点清单化(dogfood 驱动);turn 完成通知必达对的 chat(继承 D6 语义)。
  - web 终端(terminal session):键入回显延迟 / resize 重绘 / 滚动 backlog / 断线重连后屏幕一致性,给体验基线 + 量化指标(阈值见 §四 Q4)。
  - 多 session 并行的 chat-shell 寻址:会话列表活动态(继承 D9-③ label)+ 新回复指示。
- **E5 · 文档 + ship gate(常规)**:tech-design harness 节(两通道并存 + 协议→代码指针表补 stream-json 行)、usage.md、CLAUDE.md §〇/§一、版本归档;commit 英语、文档中文。

## 三、显式不做(OUT)

| OUT 项 | 理由 |
|---|---|
| codex app-server 同型适配 | 研究已有(`references/codex-desktop-app-analysis.md`),下版另起;本版只立 `HarnessAdapter` 第二实现的范式 |
| `sdkMcpServers` / `hook_callback` wire 注册、`initialize.agents` 注入 | 后续增强;首版 MCP 注册面与 role 加载面不变 |
| `--fork-session`、output_style、multi-client 单 session 镜像(remote_control/teleport 类) | 非本版 |
| 存量 session 迁移;tmux 通道行为变更(E4 打磨除外) | 两通道并存,terminal 是高级选项不是弃子 |
| track-upstream 市场 | 已独立成 `docs/versions/v0-8-12/prd.md`(正交) |
| 编排层(ccteam-flow)上线、新 IM 平台 | 既有 deferred 不变 |

## 四、Open questions → 决策(user 拍板 2026-06-10,TG 1093;Q4 留 dev-plan)

1. **`-p`:不带**(决策)—— IM 场景是长命会话、非短命 one-shot;跟 VS Code 交互流模式(长驻、stdin 常开)。
2. **IM `/new` 默认:与 web 同步切 stream-json**(决策)—— 单一默认常量,terminal 经显式参数选。
3. **cto session 默认通道:stream-json**(决策)—— 纯 chat 角色,无终端需求。
4. **E4 量化阈值**(开放):输入往返 p95、reconnect 重绘一致性判据,dev-plan 时定数。
5. **插件隔离:定点隔离官方 telegram 插件**(决策)—— ccteam spawn 的 session 经项目 `.claude/settings.local.json`(ccteam 托管层,红线允许)写 `enabledPlugins: {"telegram@claude-plugins-official": false}` 定点关闭:它独占 bot-token getUpdates,与 ccteam 自身 IM 网关结构性冲突(研究文档 §6 实测互踢事故)。**其余用户插件不动**(用户环境归用户);doctor 对同类独占外部资源插件保留检测 + warn + 一行 fix。

## 五、参考

- `docs/research/cc-stream-json-protocol.md` — 本仓研究(协议细节、slash 确定性合同、control subtype 全集、两种远端政策、隔离旋钮、hook 链对比、生命周期代价)
- `references/alleycat`(gitignore,本地)— Rust 同型实现:`crates/claude-bridge`(`pool/process.rs` argv 与进程池 idle-TTL/LRU、`approval.rs` can_use_tool 桥、`translate/` 中立事件层、`tests/support/fake_claude.rs` 假 vendor、`bridge-conformance` 跨 agent 一致性测试)—— 模式可借,代码不 vendor
- `references/claude-code`(gitignore,本地)— 逆向源码:`processSlashCommand.tsx` 确定性合同、`isBridgeSafeCommand` 白名单、`controlSchemas.ts` initialize/can_use_tool schema
- VS Code 官方扩展进程 argv 实拍(研究文档 §1)

## 六、流程 & 验收

- **doc-first**:本 PRD 候选 → user review → scope 冻结(对抗式 review 深化,沿 v0.8.10 范式)→ dev-plan 落本目录 → wave-per-phase + handoff(每 wave baseline ≥ 上 wave)。
- **验收(初稿,review 后细化)**:
  - fake-vendor 确定性 e2e:spawn→initialize 握手→多轮 turn→slash 三类逐条断言(known-local 执行 / unknown 当文本 / dialog 人话拒绝)→ can_use_tool 同意/拒绝往返→idle 释放→resume→child-death 恢复;
  - E3 故障×通道矩阵 CI-fake 全绿 + 真机短 smoke(web 新建 stream-json session → IM 聊一轮 → /compact → /context → HITL → idle → 唤醒 resume → daemon 重启 resume);
  - 基线不退:`cargo test --workspace` ≥ 起跑实测 + clippy 0 + `cargo fmt --all` 干净 + vitest/Playwright 不退。

## 七、变更记录

- **2026-06-10 初版**:由 stream-json 协议研究催生(TG 1079–1087);user 定 scope:加入版本需求(TG 1088→更正 1091 为 v0.8.11)+ 参考 alleycat(TG 1089)+ web 创建默认 stream-json、rmux 高级选项(TG 1090)+ 主线=继续 v0.8.10 方向加码稳定性/终端体验(TG 1091)。原占位 v0.8.11 的 track-upstream 市场 PRD 顺移 v0.8.12。候选,待 review。
- **2026-06-10 决策回填(TG 1093)**:Q1 不带 `-p`(IM 非短命 session)/ Q2 IM `/new` 同步默认 stream-json / Q3 cto 走 stream-json / Q5 定点隔离官方 telegram 插件(settings.local.json `enabledPlugins` 定点 false,不碰其余用户插件);Q4 留 dev-plan。同日启动「云端托管 code-agent 控制中心」主战场需求探索(参考 `references/litellm-agent-platform`,workflow 产出落 `docs/research/`)。
