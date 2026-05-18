# V0.6.0 — 从开发者元工具到产品可用 AI 团队

> **立项主线**(转 product 视角的 3 Epic):
> - **Epic A — 5 min 起第一个 IM bot**(Pocket Assistant preset,V0.6 旗舰场景)
> - **Epic B — bot 在 IM 里像真人助理**(rich media / 错误回流 / session 恢复 / NL admin)
> - **Epic C — 中文用户能用**(铺底:lift `openhuman/channels` 14+ IM 平台抽象 → V0.7 国内 IM 专项零成本启动)
>
> **一句 value prop**:在 Claude session 里**一句话**召唤 AI 团队 — 接进你的 IM,跨设备 24/7 替你干活。
>
> **硬约束**(memory: `feedback_low_friction_claude_code_native`):用户禁止写复杂 yaml / 学多 ccteam CLI 命令;主 UX 入口 = Claude session 内 skill + slash + MCP tool。`ccteam` CLI 退守 install / health / system admin。
>
> **5 团队 review 收敛**(architect / cc-expert / pm / researcher / codex-expert)详 V0.6.0 review session;本文档为 synthesize 后的最终立项。

---

## 一、Epic 主线(替代上版 F-finding 顶层结构)

### Epic A — 5 min 起第一个 IM bot

**目标**:**普通用户在 5 分钟内**完成"打开 Claude session → 在 TG 收到 bot 第一条回话"的全过程。

衡量指标:
- 步数 ≤ 5(对比当前 PRD 14 步)
- 0 行 yaml 编辑(全部 skill dialogue 自动生成)
- 0 个用户面新概念(mode 1/2/3 / hop_limit / compact_every_turns 全藏)
- 第一个 wow ≤ 5 分钟

实施手段:
- F113 `/ccteam` 总入口 slash dispatcher + 5 sub-skill(Solo / Team / Overnight / Pocket / Squad)
- F114 `ccteam-creator` 复活 + NL 自动 mode 推断
- F117 `/ccteam-im-setup` 一次性 onboarding(TG token / Lark token 一次绑定,后续 chat workflow 自动复用)
- F109 lift `claude-plugins-official/telegram`(零代码 TG 路径)+ `openhuman/channels` Rust crate(14+ IM 多平台)

### Epic B — bot 像真人助理

**目标**:用户在 IM 里跟 bot 互动**感觉像真人**(对比"AI 客服式僵硬体验")。

衡量指标:
- DM + group + bot-to-bot @ 全场景支持(V0.6.0 完整 IM Squad 体验)
- 错误回流 IM(bot 自己开口说"我刚失忆了 / 撞 budget cap / 配置坏了",不让用户去翻 log)
- session 失效后 last-N turn 回放(F115 handoff 机制延伸)
- rich media(图片 / 文件)传递
- Codex 自动当 critic / second-opinion(用户不感知,F112 §C-D)

实施手段:
- F108 模式 3 走 **Agent SDK / `claude -p --resume <sid>` + stream-json**(弃 tmux send-keys)+ **JSON-mailbox-trigger**(OMC `tmux-comm.ts` 模式)
- F112 Codex 集成 Option B 完整(`CodexExecAdapter` + `CodexAppServerAdapter` + `vendor: AgentVendor` enum)
- F115 `.ccteam/handoffs/<workflow>/<stage>.md` 决策摘要机制(researcher R4#3)
- F116 `ccteam-imd` 独立 supervisor daemon binary(borrowed OMC `reply-listener.ts` 模式)
- F118 chat session 失效 last-N turn 重建(基于 progress.jsonl + ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`)

### Epic C — 中文用户能用(V0.6.0 铺底,V0.7 专项)

**目标**:V0.6.0 ship 时**架构准备就绪**让 V0.7 国内 IM(WeChat / 飞书 / DingTalk / QQ)零成本接入。

**V0.6.0 范围(铺底)**:
- F109 lift `openhuman/channels` Rust crate 时**带全 14+ provider 模块**(Lark / DingTalk / QQ 等都已实现),只用 `--feature` 门控编译;V0.7 启用对应 feature 即可
- 中文文档先行(`docs/quickstart.md` / `user-manual.md` 中文版默认,英文翻译跟随)
- bot persona 预设库含**中文 prefab**(技术助手 / 写作 / 翻译 / 学习辅导 5 个中文 persona)

**V0.7 范围(不在本版)**:
- 国内 IM 平台启用(WeChat 个人灰路径 + 飞书 + DingTalk + QQ)
- 国内云部署文档(走 ngrok / cloudflared 替代)

---

## 二、5 个用户面 preset(替代"3 mode + 5 pattern"内部概念)

用户**只选 preset**,不直接选 mode / pattern。每 preset = 默认 surface × pattern × persona 组合配方,由 `ccteam-creator` skill 在 dialogue 中自动推断:

| Preset | 一句话场景 | hello world | 内部映射 |
|---|---|---|---|
| **Solo Sidekick** | 在 Claude 里临时召唤一个帮手干小事 | V0.5 已有(`Task` 工具)| in-proc / single agent |
| **Team Sprint** | 几小时冲一波,3-5 agent 并行干一件事 | `/ccteam-team 3 "task"`(V0.5 已有)| in-proc / Orchestrator-Worker |
| **Overnight Builder** | 丢个任务睡觉去,长跑几小时到几天 | `/ccteam-creator "夜里跑 qa-loop"` | bg / Chaining + Refine |
| **Pocket Assistant**(V0.6 旗舰)| 手机 IM 私聊一个 AI 助理 | `/ccteam-creator "做个 TG 助理 bot"` | chat (DM) / Routing |
| **IM Squad** | IM 群里多个 bot 互相 @ 协作 | `/ccteam-creator "做个 TG 多 bot 团队"` | chat (group + bot-to-bot) / Orchestrator-Worker |

---

## 三、用户面入口(全部 Claude session 内,0 ccteam CLI)

```
/ccteam <NL>                          # 总入口 NL dispatcher(F113)→ 路由到下方
/ccteam-team <NL>                     # 起临时 team(V0.5 已有,扩展 vote mode + Codex 自动)
/ccteam-creator <NL>                  # NL 对话起 workflow(V0.5 砍掉,V0.6 复活 + auto mode)
/ccteam-control <NL>                  # 项目状态查看 / 暂停 / 恢复(V0.5 已有)
/ccteam-im-setup                      # TG / Lark / Slack 一次性 token 绑定 onboarding
/ccteam-advise <hard question>        # Codex + Claude 并行 advisor(vote 模式,F112)
```

IM 端:
```
TG bot 群里:@ccteam pause helpful-bot          # NL admin,im-bridge 走 meta-agent NL 路由
TG bot 群里:@ccteam list bots                  # 同上
TG bot DM:                                     # 直接跟 bot 对话(F112 模式 3 DM)
```

`ccteam` CLI 仅保留 admin:
```
ccteam doctor                          # 健康检查 + 一次性安装
ccteam daemon {start|stop|status}      # 系统级 daemon 管理
```
其他 `ccteam new` / `ccteam start <slug>` / `ccteam internal *` 等命令仍存在(Rust binary 内),但 **user-manual.md 不教**,只在 troubleshooting / advanced 出现。

---

## 四、核心架构改动(转 product-ready)

### 4.1 ExecutionAdapter trait 对齐 Codex `ThreadManager`(F107)

弃 PRD 上版的 "新建 `ExecutionAdapter` trait + 三 LifecycleOp enum"。改为**扩展现有 `HarnessAdapter` trait**(`crates/ccteam-core/src/harness.rs:75` V0.4.0 已落)+ **对齐 Codex `ThreadManager::{submit, next_event}`**:

```rust
pub trait HarnessAdapter: Send + Sync {
    async fn start_thread(&self, spec: &AgentSpec, ctx: &SpawnCtx) -> Result<ThreadHandle>;
    async fn submit_turn(&self, h: &ThreadHandle, input: TurnInput) -> Result<TurnId>;
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle>;
    async fn close_thread(&self, h: &ThreadHandle) -> Result<()>;
}

pub enum TurnInput {
    UserText(String),
    Artifact(PathBuf),                // bg mode inbox
    SystemDirective(String),          // /compact /new /clear 退化为特殊 turn
    Image(PathBuf),                   // rich media
}

pub struct SessionHandle {
    pub vendor: AgentVendor,           // Claude | Codex
    pub mode: ExecutionMode,           // InProc | Bg | Chat
    pub identity: String,
}
```

`/compact /new /clear` 不再是独立 `LifecycleOp` enum,而是 `TurnInput::SystemDirective("compact")` 特殊 turn — adapter 内部翻译为 backend-specific 操作(Claude → `/compact` slash;Codex → `compact_remote` API)。**统一 mode 2 + mode 3 = "all chat is a turn sequence"**。

### 4.2 模式 3 执行路径:Agent SDK / `claude -p --resume`(F108 重写)

弃 PRD 上版的 "tmux send-keys + session jsonl tail"。改为:
- **input** 面:`claude -p --resume <sid> --input-format stream-json --output-format stream-json` stdin pipe(Anthropic 官方文档化路径)+ **JSON-mailbox-trigger 模式**(写 `<.ccteam/im/<bot>/inbox/msg-<ts>.md>` + 一个短 send-keys "read your mailbox" 触发,OMC `tmux-comm.ts` 抄)
- **output** 面:stream-json events 流(`stream_event` / `system/init` / `system/api_retry` 全部官方文档化 schema)
- **lifecycle** 操作:Agent SDK `compact` API 直接调,弃 `/compact` slash 命令模拟
- **tmux 退居** dev-time attach 调试入口,**不**作 production 控制平面

### 4.3 Codex 集成 Option B 完整(F112)

`vendor: AgentVendor { Claude, Codex }` 作 trait 一等公民。新增 adapter:
- `CodexExecAdapter`(模式 2 codex,走 `codex exec --json`)
- `CodexAppServerAdapter`(模式 3 codex,走 `codex app-server` UDS JSON-RPC v2)
- 复用 Codex `agent_max_depth` recursion guard
- 复用 Codex 50+ scientist nickname 池子作 bot_name 自动生成(`agent_naming.rs`)
- 借鉴 Codex `AgentPath` 层次树作 hop_limit 红线实现

cost 双 pricing table:`crates/ccteam-cost/pricing/{anthropic,openai}.toml`;`budgets.claude.max_cost_usd_per_24h` / `budgets.codex.max_cost_usd_per_24h` 各管各的。

**Codex 用户面 4 个可见场景**(其他全自动):
- A. `/ccteam-advise <hard question>` parallel voting(Claude + Codex 并行,合成显示)
- B. Auto-critic:用户起 critic / reviewer / architect role 时,如装 codex + auth ok → 自动赋 vendor: codex(`ccteam-creator` 内部决策,user 不见 yaml 字段)
- C. Claude quota 触顶 opt-in fallback(默认 off,prefs 启用)
- D. `/ccteam-team` 内置 Codex critic teammate(if available)

### 4.4 IM bridge:**lift,不造**(F109 重写)

**核心 directive**:不自造 channels 轮子。

**统一 transport = `openhuman/channels` Rust crate**(14+ IM 平台:Telegram / Slack / Discord / Lark / DingTalk / QQ / Signal / iMessage / Matrix / WhatsApp / Email / IRC / Mattermost / Web)。`Channel` trait + `SendMessage` + `ChannelMessage` 抽象在 `traits.rs:5-60`;每 provider 走 Cargo feature gate;event bus + supervisor pattern 已实现。

```
ccteam-imd(独立 daemon binary,V0.6.0 F116)
├── 依赖 openhuman/channels(Rust crate,统一 trait)
│   ├── feature=telegram  # V0.6.0
│   ├── feature=slack     # V0.6.0
│   ├── feature=discord   # V0.6.0
│   ├── feature=lark      # V0.7
│   ├── feature=dingtalk  # V0.7
│   ├── feature=qq        # V0.7
│   └── ...
├── bot-to-bot @ routing / hop_limit / mailbox 协调 / NL admin / @ccteam 解析
│   (ccteam-imd 一处实现,跨所有 IM 平台统一)
└── HarnessAdapter trait 调用(F107)— 把 inbound 消息翻译成 turn input
```

**`claude-plugins-official/telegram` 留作 backup**(用户 directive):
- 默认 V0.6.0 走 openhuman / `ccteam-imd`;`/ccteam-im-setup` 默认 transport = `openhuman/telegram`
- 用户偏好 Anthropic 全栈 / openhuman bug fallback:`/ccteam-im-setup --transport official-telegram` 切官方 MCP 路径(`claude --channels plugin:telegram@claude-plugins-official`)
- 两 path 互斥(避免同 bot token 双重 webhook 竞争);切换走 onboarding skill,不让用户编辑配置

**架构收益**(为啥统一比"TG 官方 + 其他 openhuman"好):
- bot-to-bot @ routing / hop_limit / `@ccteam` NL admin / mailbox 协调 **一处实现**,跨所有 IM 平台一致
- V0.7 国内 IM 启用零代码(只 cargo feature 切换 + token onboarding 测试)
- 单一 daemon(`ccteam-imd`)替代"ccteam daemon + Anthropic 官方 TG MCP server"双进程
- TG 特定逻辑(pairing/allowlist)若 openhuman/telegram 暂缺,ccteam-imd 上层补,**不分两套实现**

### 4.5 F110 MCP namespace rename 取消

**收 review 反馈**:`mcp__ccteam__*` → `mcp__ct__*` 节省 4 字符是 dev-cost 优化,**对 V0.5 用户是肌肉记忆破坏**;`ct` 1 年后用户记不住代表啥。**保留** `mcp__ccteam__*` namespace;F110 只保留 "**子前缀分组**" 部分:`mcp__ccteam__workflow_ls` / `mcp__ccteam__chat_send_input` 等。

### 4.6 F111 + F114 等其他改进

详 `prd.md`。

---

## 五、红线按"模式 × vendor"双轴 scope 重写(F106 重写)

原 V0.6.0 红线只按模式 scope。Codex 加入后必须按 vendor 也 scope:

| 红线 | 模式 1 in-proc | 模式 2 bg | 模式 3 chat |
|---|---|---|---|
| R1 文件系统是控制平面 | — | **Claude+Codex 同**(artifact)| Claude: stream-json + ccteam-owned `turns.jsonl`;Codex: app-server UDS + `turns.jsonl` |
| R2 progress.jsonl 唯一 state SoT | — | **守(双 vendor)** | **业务事件 SoT 守**;对话原文走 ccteam-owned `turns.jsonl`(取代依赖 Anthropic 内部 `~/.claude/projects/`)|
| R3 每次 spawn = fresh 1M context | — | **Claude 守;Codex `codex exec resume <tid>` 实现可复用,trait 决定,用户不见** | **不适用** — chat 复用 context 是 feature |
| R4 不解析 tmux 输出 | — | 守 | 守 — 读 stream-json + `turns.jsonl`,**不 scrape pane** |
| R5 永不主动 kill 长 session | 守 | 守(`--max-budget-usd` 平台兜底替代 F84 强制 kill)| 守 + `/compact /new` 是合法 turn,非 kill |
| R6 fix-loop 3 次必 escalate | 守 | 守 | 守 + **AgentPath depth limit**(借 Codex AgentRegistry 实现)替代平铺 fix_counts 红线 |
| R7 `ccteam-core` 零 team 名字面量 | 守 | 守 | 守 |
| R8 跨项目记忆走官方接口 | **Claude: `~/.claude/CLAUDE.md`** / **Codex: `~/.codex/AGENTS.md`**;`ccteam init` 落 AGENTS.md → CLAUDE.md POSIX symlink | 同上 | 同上 |
| R9 新建项目走 `<projects_root>/<team>-<slug>/` | — | 守 | 守 |

---

## 六、Findings 索引(F106-F118,Epic 下的实施手段)

| F | 主题 | 性质 | Epic | Wave |
|---|---|---|---|---|
| F106 | 红线按"模式 × vendor"双轴 scope 重写 | 文档 | A/B/C | 跟随 |
| F107 | 扩展 `HarnessAdapter` 对齐 Codex `ThreadManager`;5 方法 trait + vendor enum | 重构 | B | 1 |
| F108 | 模式 3 走 `claude -p --resume` + stream-json + JSON-mailbox-trigger(弃 tmux send-keys)| 新增 | B | 2 |
| F109 | IM bridge 统一 `openhuman/channels` Rust crate(主路径,14+ IM 平台);`claude-plugins-official/telegram` 作 backup | 新增 | A/C | 2 |
| ~~F110~~ | ~~MCP namespace ccteam → ct rename~~ | **取消** | — | — |
| F111 | MCP 工具子前缀 + group disable env + 项目级 `.mcp.json` | 改进 | A | 3 |
| F112 | Codex 集成 Option B 完整:trait + 2 adapter + 双 pricing + 4 用户场景 | 新增 | B | 2 |
| F113 | `/ccteam` 总入口 NL dispatcher slash + 5 sub-skill | 新增 | A | 1-2 |
| F114 | `ccteam-creator` 复活 + NL 自动 mode 推断 + persona 预设库 | 新增 | A | 2 |
| F115 | `.ccteam/handoffs/<workflow>/<stage>.md` 决策摘要机制 | 新增 | B | 2 |
| F116 | `ccteam-imd` 独立 supervisor daemon binary(borrowed OMC `reply-listener.ts`)| 新增 | A/B | 2 |
| F117 | `/ccteam-im-setup` 一次性 IM token onboarding skill | 新增 | A | 2 |
| F118 | chat session 失效 last-N turn 重建(ccteam-owned `turns.jsonl` SoT)| 新增 | B | 3 |

---

## 七、文档套件(本版产物)

PM 提议的 5 用户面文档全部产出:

```
ccteam/
├── README.md                              ≤80 行 [V0.6.0 REWRITE,产品 pitch + 5 行 install + link]
└── docs/
    ├── quickstart.md                      ≤120 行 [V0.6.0 NEW,Pocket Assistant 场景 5-step]
    ├── user-manual.md                     ≤300 行 [V0.6.0 REWRITE V0.5,5 preset 各一节]
    ├── recipes.md                         ≤500 行 [V0.6.0 NEW,8-10 ready preset 模板]
    ├── troubleshooting.md                 ≤400 行 [V0.6.0 NEW,50+ 故障条目]
    ├── advanced/                          [V0.6.0 NEW]
    │   ├── customize-workflow.md
    │   ├── multi-llm-codex.md             [Codex 集成手册]
    │   └── presets-reference.md
    ├── v0-6-0/                            [本 PR dev 归档,ship 后冻结]
    │   ├── README.md(本文件)
    │   ├── prd.md
    │   ├── dev-plan.md
    │   └── host-probe.md(wave 4 验证产物)
    └── architecture/(重组,纯 dev docs)
```

**用户面 13 项内部术语永久从用户面文档清出**(详 PM PM11):mode 1/2/3、progress.jsonl、send-keys、session jsonl tail、ExecutionAdapter、fix_count、escalation event、hop_limit、compact_every_turns、F-number、`mcp__ccteam__chat_session_reset_force`、`cleanup_on_stop: leave-running`、5 编排 × 3 执行矩阵。

---

## 八、不在本版

- **国内 IM 启用**(WeChat / 飞书 / DingTalk / QQ)— V0.7 专项;V0.6.0 铺底使 V0.7 几乎零成本
- **chat memory 跨设备同步**(`<project>/.ccteam/chat/<bot>/turns.jsonl` 已是 SoT,但跨机同步走 git / rsync / 云盘是 V0.7 工具支持)
- **6 号编排模式 HITL / Approval Gating**(researcher R10 提)— V0.6.1 立项 F119
- **monorepo-aware `.mcp.json`**(researcher R6#4 提)— V0.7+
- **`ccteam migrate-from claude` 反向 import**(codex-expert CX6#5 提)— V0.7+
- **V0.5.x 延期 F98**(plan-approval↔outbox 联动)— 与 F119 合并 V0.6.1

---

## 九、决策记录修订(对上版 README §七)

| 决策 | 上版选 | 本版改 | 理由 |
|---|---|---|---|
| 模式 3 execution runtime(Wave 1 amendment 2026-05-19)| Agent SDK / `claude -p --resume` + stream-json(B)| **tmux 长跑 + send-keys -l 直送 user content + dual-track(Claude Code 官方 hooks 快 event + transcript jsonl byte-offset 增量读 → 镜像 ccteam-owned turns.jsonl)+ slash 命令透明透传(C)** | 综合 ccgram + OMC 已 production 验证;`-p --resume` 每 turn 冷启 prompt cache 失效 + slash 命令不透传(用户面 UX 退化)+ mailbox-trigger 让短文本走 Read tool 增加 turn cost;tmux 长跑 + send-keys -l 直送是双方共识 + Claude Code hooks 官方文档化 fast event 通道 |
| 模式 3 input 面 | stream-json stdin pipe + JSON-mailbox-trigger | **`tmux send-keys -l` 直送 user content + Enter(text);`/compact /new /clear` 透明透传;附件走 attachments dir + send-keys "read file at ..."** | send-keys -l literal 模式 0 escape 雷区(ccgram + OMC 验证);slash 透传保用户面 UX 一致;attachment 仅在非文本场景用 |
| 模式 3 output 面 | stream-json 官方 schema + ccteam-owned turns.jsonl | **Claude Code 官方 hooks(UserPromptSubmit / Stop / SubagentStop / SessionStart / PostToolUse)作 fast event 通道 + byte-offset 增量读 transcript jsonl → 镜像 ccteam-owned `turns.jsonl`(R2 SoT)** | hooks 已文档化 + 低延迟 turn boundary;transcript polling 拿 full content;ccteam-owned `turns.jsonl` 让 R1/R2 红线在模式 3 站住(F118 重建从 turns.jsonl 读) |
| MCP namespace rename | breaking `ccteam` → `ct` | **取消;保 `ccteam` namespace,只加子前缀** | PM + cc-expert + codex-expert 共识 + 用户低门槛 override:rename 用户净损失 |
| F109 IM bridge 自造 | tokio task + `tgbot` crate | **统一 `openhuman/channels` Rust crate(全 14+ IM 含 Telegram);`claude-plugins-official/telegram` 作 backup transport** | 用户 directive:不造轮子;统一 transport 一处实现 bot-to-bot routing;V0.7 国内 IM 零代码启用 |
| Codex 集成深度 | Option A(模式 2 only)| **Option B(完整:trait + 2 adapter + 双 pricing + 4 用户场景)** | 用户拍板;不集成 V0.6.0 ship 后被定义为"只支持 Claude 的小众工具"(researcher R9 表)|
| user surface 主入口 | `ccteam` CLI 命令 | **Claude session 内 `/ccteam-*` slash + meta-agent NL + MCP tool** | 用户硬约束:"低门槛 + Claude-Code-native UX";13 lock decisions L2 |
| 用户面概念 | mode 1/2/3 + 5 编排 + 4 trigger + N MCP 工具 = 8+ 概念 | **5 preset(用户只选 preset)+ `/ccteam` 一个总入口 slash** | architect A9 + PM PM10 8→2-axis 压缩;用户低门槛 override |
| DM vs group | group 优先,DM 延 V0.6.1 | **DM + group + bot-to-bot @ 全起 V0.6.0** | 用户拍板:R6#1 完整 IM Squad 体验是 V0.6.0 锁定卖点 |
| 5 用户文档 | docs/v0-x-y/ 内 user-manual.md | **5 份文档独立 docs/ 根 + advanced/ + v0-6-0/(dev archive)分离** | PM PM11 |

---

详 `prd.md`(Epic 详细需求 + 12 finding 实施)+ `dev-plan.md`(4 wave 修订)。
