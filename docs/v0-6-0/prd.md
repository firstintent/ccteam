# V0.6.0 — PRD

**12 个 finding(F106-F118,F110 取消)**,按 3 Epic 组织:Epic A "5 min IM bot" / Epic B "bot 像真人助理" / Epic C "中文用户能用"。详 `README.md`。

> **本 PR 范围**:只 land 本 PRD + `README.md` + `dev-plan.md` + 5 用户面文档(`docs/quickstart.md` / `user-manual.md` / `recipes.md` / `troubleshooting.md` / `advanced/*`)。代码改动**全部走后续 wave PR**。

---

## EPIC A — 5 min 起第一个 IM bot

**Epic 验收**:普通用户(非 ccteam 开发者)在 5 分钟内完成:打开 Claude session → `/ccteam <NL>` 起 Pocket Assistant → 在 TG 收到 bot 第一条回话。**0 行 yaml 编辑;0 个新用户面概念**。

下属 finding:F113 / F114 / F117 / F109(Epic A 部分)/ F111 / F106(Epic A 部分)。

---

### F113 — `/ccteam` 总入口 NL dispatcher + 5 sub-skill

#### 痛点

V0.5.0 已经把 5 skill 砍到 3(`ccteam-team` / `ccteam-control` / `ccteam-creator`),但用户仍要记多个 slash 名。普通用户期望"一个总入口,我说人话,系统决定调啥"。OMC `README:231` "No commands to memorize, just describe what you want" 已经市场验证。

#### 需求

新建 `skills/ccteam/SKILL.md`(总入口):

```
/ccteam <NL>
```

skill body 是一个 NL dispatcher:
1. parse 用户意图(意图分类:start-team / monitor / configure-im / advise / create-workflow / status / debug)
2. 路由到下方 5 sub-skill 之一,自动透传用户 NL
3. 失败时 fallback "我没听懂,你想:(a)起一个 team 干活 / (b)做个 IM bot 助理 / (c)看 ccteam 状态 / (d)其他 — 请选或重新描述"

sub-skill 仍保留(高级用户 deep-link):
- `/ccteam-team <NL>` — 起临时 team(V0.5 已有,V0.6 加 vote mode + Codex auto-critic 路径)
- `/ccteam-creator <NL>` — NL 对话起 workflow(V0.5 砍掉,V0.6 复活 + auto mode 推断)
- `/ccteam-control <NL>` — 项目状态查看 / 暂停 / 恢复(V0.5 已有)
- `/ccteam-im-setup` — IM token 一次性 onboarding(V0.6 NEW,详 F117)
- `/ccteam-advise <hard question>` — Codex + Claude 并行 advisor 投票(V0.6 NEW,详 F112 §A)

#### 文件清单

| 文件 | 改动 |
|---|---|
| `skills/ccteam/SKILL.md`(新)| ~150 行 dispatcher;前 30 行 LLM 意图分类 prompt + 后 120 行 5 sub-skill 调用模板 |
| `skills/ccteam-creator/SKILL.md`(复活)| V0.5.0 砍掉,V0.6 重写 — auto mode 推断 + persona 预设库(详 F114)|
| `skills/ccteam-im-setup/SKILL.md`(新)| ~200 行 onboarding dialog(详 F117)|
| `skills/ccteam-advise/SKILL.md`(新)| ~100 行 Codex + Claude parallel advisor 模式 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | `--install-skill all` 装 5 skill(总入口 + 4 sub);保留 single-skill flag |
| `crates/ccteam-cli/src/mcp_chat_tools.rs`(新;Wave 1 落)| `chat_*` 5 工具 stub schema + dispatcher。`/ccteam <create-workflow>` 路径 NL 推断到 Pocket Assistant / IM Squad preset 后,sub-skill 调本 stub 接口(Wave 2 F108 填实)|
| `crates/ccteam-cli/src/mcp_advise_tools.rs`(新;Wave 1 落)| `advise_*` 2 工具 stub schema + dispatcher。`/ccteam <advise>` 路径走本 stub(Wave 3 F112 §A 填实)|

#### 验收

1. 用户 `cd && claude` 后第一次输入 `/ccteam "我想做个 TG 助理 bot"` → 自动路由到 `/ccteam-creator` → 启动 Pocket Assistant 配方 + 自动 IM onboarding
2. `/ccteam "fix all TS errors"` → 自动路由 `/ccteam-team 3:executor` 立刻起 team
3. `/ccteam "我哪些项目还活着"` → 自动路由 `/ccteam-control ls`
4. NL 失败 fallback → "我没听懂,(a)/(b)/(c)/(d)" 4 选项;用户选 → 路由
5. `host probe`:5 个 sub-skill 各 1 host test,intent classification accuracy ≥90%(基于 50 个 sample query)

#### 不在范围

- 不做 voice / 多模态 input(V0.7+)
- 不做 skill 自定义注册(用户不能 `/ccteam-add-skill`,固定 5 sub)

---

### F114 — `ccteam-creator` 复活 + NL 自动 mode 推断 + persona 预设库

#### 痛点

V0.5.0 F100 把 `ccteam-creator` skill 砍掉(5→3 skill),代价是用户**没有 dialogue path 起新项目**。模式 3 chat 加入后必须有:用户说"做个 TG 助理 bot",skill 走 NL dialogue 自动:
- 推断 mode(chat / bg / in-proc)
- 生成 workflow.yaml(用户不可见)
- 选 bot persona 模板(预设库挑)
- 调 `/ccteam-im-setup`(如果是 chat)
- 调 `ccteam init` 内部接口(daemon-internal,用户不见)
- 落 `.claude/agents/<role>.md` agent 定义

#### 需求

`skills/ccteam-creator/SKILL.md`(~400 行):

**Phase 1: Intent classification**(从用户 NL 提取):
- task type(coding / writing / research / support / scheduling / monitoring / chat-assistant / multi-bot-team / qa-loop / ...)
- presence(用户全程在 / 偶尔回来看 / 关上电脑跑 / IM 私聊 / IM 群)
- timeline(一次性 / 长跑几小时 / 长跑 24/7)

**Phase 2: Mode 推断**(规则:见 [架构 doc Mode 推断决策树](../architecture/mode-inference.md)):
- presence = "全程在" + timeline = "一次性" → in-proc team(Solo Sidekick / Team Sprint)
- presence = "关上电脑跑" + timeline = "长跑" → bg artifact-driven(Overnight Builder)
- presence = "IM 私聊" → chat DM(Pocket Assistant)
- presence = "IM 群多 bot" → chat group(IM Squad)

**Phase 3: Persona 预设库匹配**(5-10 个内置 prefab):
- 技术助手(中英 2 版本)
- 写作助手(中英 2 版本)
- 项目经理 / 监工
- 客服 / 翻译
- 学习辅导
- 心理咨询(谨慎,默认 off)
- 代码 critic / reviewer(自动接 Codex if available)
- 翻译 / 摘要
- 群组协调员

每 persona = `.claude/agents/<role>.md` + `tone` / `guardrails` / `tools` frontmatter 全部预填。

**Phase 4: 输出 TEAM/PROJECT PLAN**(plan-first,严格不立刻 spawn):
```
PROJECT PLAN
============
Type: Pocket Assistant (private TG bot)
Bot: @helpful_assistant(persona: 技术助手 中文版)
Codex critic: 自动启用(检测到 codex binary + auth)
IM:Telegram via openhuman/channels(已 onboarded:✓)
预计 cost / day: ~$0.5 (Claude main + Codex critic ratio)

Reply 'go' 起,或描述要改的(persona / IM platform / language)。
```

**Phase 5: On 'go' execute**:
- 调内部 API 生成 `.ccteam/workflow.yaml`(用户不可见;`/ccteam-control show-workflow` 高级 user 可看)
- 调 `ccteam daemon` 起 chat workflow
- 通知 IM bridge 注册 bot
- 回 user "好了,在 TG 找 @helpful_assistant 私聊试试"

#### 文件清单

| 文件 | 改动 |
|---|---|
| `skills/ccteam-creator/SKILL.md`(复活 + 重写)| ~400 行 |
| `skills/ccteam-creator/personas/`(新目录)| 5-10 个 prefab persona 文件(每个含 `.claude/agents/<role>.md` 模板 + tone + guardrails)|
| `crates/ccteam-core/src/templates/workflow_templates/`(新)| 5 个 workflow.yaml 模板(对应 5 preset)|
| `crates/ccteam-core/src/mode_inferrer.rs`(新)| NL → mode 推断逻辑(rule-based + LLM 兜底)|

#### 验收

1. 50 个 sample NL query → mode 推断 accuracy ≥85%(host probe 验)
2. 5 preset 各跑通 `ccteam-creator` 全 dialogue → workflow.yaml + agent.md + daemon 启动一站式完成
3. user 看不到 yaml 字段名 / persona file 路径(技术细节藏在 skill body 内)
4. user 可中途打断 "等等,改成中文 persona / 加个 critic" → skill 回 Phase 2 / 3 重选

#### 不在范围

- 不做 user-defined 自定义 persona(用户写 markdown)— V0.7+
- 不做 persona marketplace 拉取(V0.8+)

---

### F117 — `/ccteam-im-setup` 一次性 IM token onboarding

#### 痛点

PM PM3 量化:模式 3 hello world 14 步,其中 5 个高危卡点 — TG @BotFather 注册 + 拿 chat_id + 设 env var 是前 3 个。普通用户 80% 在这里放弃。

需要:**一个 skill,user 在 Claude session 内 `/ccteam-im-setup`,5 句话内拿到 token + 验证 + 永久绑定**。后续任何 chat workflow 自动用 token,user 不再碰。

#### 需求

`skills/ccteam-im-setup/SKILL.md`(~200 行):

**Step 1: Platform 选择**
```
$ /ccteam-im-setup
> 你要绑定哪个 IM 平台?
  1. Telegram
  2. Slack
  3. Discord
  4. (V0.7) Lark / DingTalk / WeChat
```

**Step 2: 平台特定 onboarding**(平台脚本化,自动化能省的全自动):

TG path:
- 系统打印浏览器可点开 URL:`https://t.me/BotFather`
- 系统说"打开后发 `/newbot`,起个名字,@BotFather 会给你一段 token"
- user 粘贴 token → skill 验证 token(`getMe` API call)→ 成功显示 bot username
- skill 提示"现在用手机 TG app 私聊一下这个 bot 发 'hello' — 我帮你拿 chat_id"
- skill long-poll Telegram getUpdates → 捕获 chat_id → 验证 → 落地

Slack path:用 https://api.slack.com/apps + bot token + Socket Mode 验证流(skill 文档化每一步)

**Step 3: token 持久化**
- 写 `~/.ccteam/im/credentials.json` 0600 权限
- **不写 env var**(用户不学 .bashrc)
- **不入 workflow.yaml**(R8 红线 + 安全)

**Step 4: backup transport 切换**(用户 directive)
- 默认 transport = `openhuman` (统一路径)
- 用户可 `/ccteam-im-setup --transport official-telegram` 切官方 Anthropic MCP 路径(用户偏好"全 Anthropic 栈"或 openhuman bug)
- 切换后状态写 `~/.ccteam/im/transport.toml`,后续所有 chat workflow 使用新 transport

#### 文件清单

| 文件 | 改动 |
|---|---|
| `skills/ccteam-im-setup/SKILL.md`(新)| ~200 行 |
| `crates/ccteam-imd/src/onboarding.rs`(新)| getMe + getUpdates auto-detect chat_id |
| `crates/ccteam-imd/src/credentials.rs`(新)| `~/.ccteam/im/credentials.json` 0600 读写 |

#### 验收

1. 新用户 TG path 端到端:3-5 分钟拿到 bot 第一条回话(包含 BotFather 跳转 + 测试 chat_id)
2. token 落地 `~/.ccteam/im/credentials.json` 0600;`ls -la` 显示正确权限
3. backup transport 切换:`/ccteam-im-setup --transport official-telegram` 后,下一个 chat workflow 起 bot 走官方 `claude --channels plugin:telegram@claude-plugins-official`
4. 出错友好提示:`token` 错 / `chat_id` 拿不到 / network → 中文 / 英文 解释 + 修复指南
5. user 不学 env var / BotFather command 名 / chat_id 格式 — skill 全程引导

#### 不在范围

- 不做 OAuth flow(只 token-based)
- 不做 webhook 公网 URL 自动 setup(用户自行 ngrok / cloudflared,文档教)
- 不做 multi-account(一 bot per platform)

---

### F111 — MCP 工具粒度配置 + 项目级 `.mcp.json`(改进版,F110 取消)

#### 痛点

V0.6.0 后 MCP 工具总数 ≥22(17 现 + 5 chat + Codex 集成扩);默认全发到每 session tool list,context 预算占大。

但 V0.6.0 **取消** 上版 F110 ccteam → ct rename(理由详 README §4.5)。**保留** F110 的"子前缀分组"+ F111 的 disable env + 项目级 `.mcp.json`。

#### 需求

**A. MCP 工具子前缀(保留 F110 中此部分)**

```
mcp__ccteam__workflow_ls
mcp__ccteam__workflow_show
mcp__ccteam__workflow_send_to_session
mcp__ccteam__chat_send_input
mcp__ccteam__chat_lifecycle
...
```

server name **仍是** `ccteam`(不动 V0.5 用户肌肉记忆);子前缀让用户在 `/mcp` 列表一眼看清归属(workflow / chat / advise 等)。

**B. `CCTEAM_DISABLE_TOOLS` 改 group-name 枚举**(researcher R4#5)

弃 glob 模式 `chat_*,screenshot`。改 enum group:

```bash
export CCTEAM_DISABLE_TOOLS="chat,screenshot,parallelism"
```

支持 group:`workflow` / `chat` / `advise` / `screenshot` / `parallelism` / `admin`。`DISABLE_TOOLS_GROUP_MAP: HashMap<&str, ToolCategory>` 在 ccteam-cli 内维护。

**C. 项目级 `.mcp.json` 自动生成**

`ccteam-creator` skill `Phase 5: execute` 落项目 `.mcp.json`(merge 已有 `.mcp.json` 而非覆盖):

```json
{
  "mcpServers": {
    "ccteam": {
      "command": "ccteam",
      "args": ["mcp-serve"],
      "env": {
        "CCTEAM_PROJECT_ROOT": "${workspaceFolder}"
      }
    }
  }
}
```

`ccteam doctor --install-mcp` 保留(`~/.claude.json` user-global fallback)。

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-cli/src/mcp_serve.rs` | tool registration 加子前缀;读 `CCTEAM_DISABLE_TOOLS` group enum |
| `crates/ccteam-cli/src/mcp_chat_tools.rs`(新)| chat_* 5 工具注册 |
| `crates/ccteam-cli/src/mcp_advise_tools.rs`(新)| advise_* 2 工具(F112)|
| `crates/ccteam-core/src/templates/project_mcp_json.rs`(新)| `.mcp.json` 生成 + merge 逻辑 |

#### 验收

1. `ccteam` server name 不变,V0.5 用户配置零 break
2. `mcp__ccteam__workflow_ls` `mcp__ccteam__chat_send_input` 等所有工具按子前缀出现
3. `CCTEAM_DISABLE_TOOLS=chat,screenshot` → tool list 仅含 workflow_* + advise_*
4. `ccteam-creator` 起新 chat workflow → 项目目录有 `.mcp.json` 含 ccteam server 注册

#### 不在范围

- 不做工具类别 GUI 编辑器
- 不取代 `doctor --install-mcp`(保留作 user-global fallback)

---

## EPIC B — bot 在 IM 里像真人助理

**Epic 验收**:用户在 TG bot 跑通 hello world 后,**连续 3 天的高频日常使用**中体验"像真人"(对比 "AI 客服式僵硬"):
- DM + group + bot-to-bot @ 完整 IM Squad
- 错误回流 IM(bot 自己开口说错误,不让用户去翻 log)
- session 失效后 last-N turn 回放(bot 不"失忆灾难")
- rich media(图片 / 文件)传递
- Codex 自动当 critic / second-opinion(用户不感知)

下属 finding:F107 / F108 / F112 / F115 / F116 / F118 / F109(Epic B 部分)。

---

### F107 — 扩展 `HarnessAdapter` 对齐 Codex `ThreadManager`(架构基石,Wave 1 完成)

#### 痛点

V0.6.0 PRD 上版提"新建 `ExecutionAdapter` trait",但 `crates/ccteam-core/src/harness.rs:75` V0.4.0 F61/F62 已落 `HarnessAdapter` trait + `Executor::{Claude, Codex}` enum + orchestrator HashMap dispatch。**新建 trait = 重复抽象**。

同时:Codex 已经把"长跑 multi-agent"协议做完(`ThreadManager::{submit, next_event}` + `ThreadEvent` enum;`AgentPath` 层次树;`InterAgentCommunication { trigger_turn: bool }` 语义)。ccteam V0.6.0 应**对齐**而非自创。

#### 需求

**A. 扩展现有 `HarnessAdapter` trait**(5 方法,对齐 Codex `ThreadManager`):

```rust
// crates/ccteam-core/src/harness.rs(扩展)
pub trait HarnessAdapter: Send + Sync {
    /// thread = 一次会话生命周期(bg one-shot 也是一 thread,只跑一 turn)
    async fn start_thread(&self, spec: &AgentSpec, ctx: &SpawnCtx) -> Result<ThreadHandle>;

    /// 在已存 thread 上启动一个 turn(给 user input / artifact / system msg)
    async fn submit_turn(&self, h: &ThreadHandle, input: TurnInput) -> Result<TurnId>;

    /// 订阅 thread 上所有 event 流(turn boundaries + items)
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;

    /// 恢复已存 thread(session-id 持久化路径)
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle>;

    /// 终结 thread
    async fn close_thread(&self, h: &ThreadHandle) -> Result<()>;
}

pub enum TurnInput {
    UserText(String),
    Artifact(PathBuf),             // mode 2 inbox 文件
    SystemDirective(String),       // /compact /new /clear 退化为特殊 turn
    Image(PathBuf),                // rich media(Epic B)
    ToolResult { ... },            // external resolver 回 agent
}

pub enum ThreadEvent {
    ThreadStarted { thread_id: String },
    TurnStarted { turn_id: String },
    TurnCompleted { turn_id: String, usage: UnifiedTokenUsage },
    TurnFailed { turn_id: String, err: ThreadErrorEvent },
    ItemStarted { item: ThreadItem },
    ItemUpdated { item: ThreadItem },
    ItemCompleted { item: ThreadItem },
    Error(ThreadErrorEvent),
}

pub struct SessionHandle {
    pub vendor: AgentVendor,        // Claude | Codex
    pub mode: ExecutionMode,        // InProc | Bg | Chat
    pub identity: String,
}

pub enum AgentVendor { Claude, Codex }
pub enum ExecutionMode { InProc, Bg, Chat }
```

**B. 现有 Claude `BgSpawner` 改造为 `ClaudeBgAdapter` impl trait**(零行为变化,baseline 942/1 持平)。

**C. 现有 Codex tmux 路径(若存在,V0.4.0 F62)改造为 `CodexExecAdapter` impl trait**;模式 3 Codex(`CodexAppServerAdapter`)推 Wave 3。

**D. `UnifiedTokenUsage` 收 vendor 差异**:

```rust
pub struct UnifiedTokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,       // both vendors
    pub output_tokens: u64,
    pub cache_creation_input_tokens: Option<u64>,   // Anthropic 1h ephemeral cache 写入
    pub reasoning_output_tokens: Option<u64>,        // Codex o-series CoT
}
```

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/harness.rs` | 扩展 trait 加 5 方法 + 类型 |
| `crates/ccteam-core/src/execution/claude_bg.rs`(新,迁移自 spawn.rs)| `ClaudeBgAdapter` impl |
| `crates/ccteam-core/src/execution/codex_exec.rs`(新)| `CodexExecAdapter` impl(走 `codex exec --json`) |
| `crates/ccteam-core/src/orchestrator.rs` | trait 主循环改 ThreadHandle / TurnInput / ThreadEvent |
| `crates/ccteam-cost/src/lib.rs`(新或重构 `cost_summary.rs`)| `UnifiedTokenUsage` + 双 pricing table |
| `crates/ccteam-cost/pricing/anthropic.toml`(新)| Anthropic 定价 |
| `crates/ccteam-cost/pricing/openai.toml`(新)| OpenAI 定价 |
| `crates/ccteam-core/tests/harness_trait_test.rs`(扩)| 4-6 trait 行为测试 |

#### 验收

1. `cargo test --workspace`: ≥942 通过(baseline 持平,允许 +新 trait 测试)
2. `grep -rn "trait HarnessAdapter" crates/ccteam-core/src/harness.rs` ≥1(扩展不新建)
3. `grep -rn "trait ExecutionAdapter" crates/` 0 命中(不新建 trait)
4. host probe:V0.5.1 跑过的 dex-ui workflow 在 V0.6.0 wave-1 zero behavior change(progress.jsonl event sequence diff = 0)
5. cost 计算双 vendor:Claude + Codex 同 workflow 跑,`progress.jsonl::agent_done.cost_usd` 各自从对应 pricing table 算 ±5% 准确

#### 不在范围

- 不实现 `ClaudeStreamJsonAdapter`(F108) / `CodexAppServerAdapter`(F112)— Wave 2/3
- 不改 MCP / CLI 用户面接口(纯内部重构)

---

### F108 — 模式 3 走 tmux 长跑 + `send-keys -l` 直送 + dual-track transcript polling(Wave 1 amendment)

> **Amended 2026-05-19** by Wave 1 architect after `references/` 比对(ccgram + OMC)
> + Claude Code 官方 hooks 文档化 + ccteam-imd supervisor 模式收敛。前版"Agent SDK / `claude
> -p --resume` + stream-json + JSON-mailbox-trigger"路径在工程上跟用户面 UX 冲突
> (用户输 slash 命令 `/compact /new /clear` 必须**透明透传**给 Claude TUI;`-p --resume`
> 每 turn 都冷启 + 重 parse session jsonl,prompt cache 利用率差;mailbox-trigger 让
> user content 走 Read 工具一跳,长链路 reasoning 多个 tool_use 步骤,turn cost 翻倍)。
> Wave 1 落 `ClaudeTuiAdapter` STUB(`crates/ccteam-core/src/execution/claude_tui.rs`);
> Wave 2 F108 填 impl。

#### 痛点

V0.6.0 上版"PRD F108"(Agent SDK / `claude -p --resume` + stream-json)在试做时暴露 3 个
工程不可控:

1. **slash 命令不透传 = 用户面 UX 退化**:`-p` 模式 prompt 进 stdin pipe,用户在 IM 发
   `/compact` 想触发 Claude 内置压缩走不通(`/compact` 是 TUI slash,`-p` 不识别)。
2. **每 turn 冷启 + 重 parse session jsonl = prompt cache 失效**:`-p --resume <sid>` 每次
   都重新加载 session(`~/.claude/projects/<...>/<sid>.jsonl`),turn 2 起 cache_read_tokens
   应该 ≥80% 但实测<20%(reload 时 ephemeral cache 不命中)。
3. **JSON-mailbox-trigger 让短消息 user content 走 Read 工具**= 多 1 个 tool_use step =
   每 turn cost ~2x,且在 chat 模式下让 reasoning trace 充斥 file read 噪音。

ccgram(production验证)+ OMC(production验证)走的是**完全相反**的路径:tmux 长跑
+ `tmux send-keys -l` 直送 user content + 用 Claude Code 官方 hooks(`UserPromptSubmit`
/ `Stop` / `SubagentStop` / `SessionStart` / `PostToolUse`)作 fast event 通道 + byte-offset
增量读 `~/.claude/projects/<encoded-cwd>/<session_id>.jsonl` 镜像到 ccteam-owned
`turns.jsonl`(R2 SoT)作 full content 通道。这是已经 production-verified 的 dual-track 模式。

#### 需求(改 — 落 tmux 长跑 + send-keys -l + dual-track)

**A. input = `tmux send-keys -l <session> <user_content>` + Enter 直送**

用户在 IM 发文本 → ccteam-imd 收到 → `tmux send-keys -l -- <user_content>` 直接送给
Claude TUI(literal 模式,0 escape 雷区,ccgram + OMC production 验证)。

- 文本 user content 全部走 send-keys -l(包括多行 code block / unicode emoji / 长消息)
- attachment(图片 / 文件)走 mailbox 模式 `<.ccteam/im/<bot>/attachments/<file>` + 一句
  send-keys "read the file I placed at <path>" 触发 Read tool
- **slash 命令 `/compact` `/new` `/clear` 透明透传** — 用户 IM 发啥 ccteam-imd 都 send-keys
  原样,不过滤;ccteam 通过 `SessionStart` hook 副作用观察 `chat_session_reset` 等事件并
  emit 到 progress.jsonl

```
ccteam-imd recv "@helpful-bot /compact please"
  ↓ tmux send-keys -l -t ccteam-chat-foo-helpful-bot -- "/compact please"
  ↓ tmux send-keys -t ccteam-chat-foo-helpful-bot Enter
  → Claude TUI 执行 /compact → SessionStart hook 触发 → ccteam-imd 写
    `progress.jsonl::chat_session_reset { reason: "user-compact" }`
  → bot 在 IM 回复"已压缩,我们继续"
```

**B. output dual-track**(借 ccgram pattern):

- **Track 1 — fast event(structured business events)**:Claude Code 官方 hooks
  `UserPromptSubmit` / `Stop` / `SubagentStop` / `SessionStart` / `PostToolUse` 配上
  ccteam 提供的处理器(`ccteam internal hook chat-progress <event>`)→ 写
  `progress.jsonl` 业务事件(`chat_turn_started` / `chat_turn_completed` /
  `chat_session_reset` / `chat_tool_use` / `chat_subagent_done` 等)。**已文档化**
  (Claude Code 官方 hooks reference)+ **低延迟**(hook 在 turn boundary 立即 fire)。

- **Track 2 — full content(conversation history mirror)**:byte-offset 增量读
  `~/.claude/projects/<encoded-cwd>/<session_id>.jsonl`,cursor 文件
  `<.ccteam/chat/<bot>/transcript-cursor.json` 记上次读到的字节偏移,每次只 read 增量,
  parse `message` / `tool_use` / `tool_result` 行 → 镜像到 ccteam-owned
  `<.ccteam/chat/<bot>/turns.jsonl`(schema = ccteam 控制,不依赖 Anthropic 内部格式):

  ```jsonl
  {"turn_id":"...","ts":"...","role":"user","content":"...","vendor":"claude"}
  {"turn_id":"...","ts":"...","role":"assistant","content":"...","usage":{...},"tool_calls":[...]}
  ```

  ccteam-imd 拿 `turns.jsonl` 增量行 → `send_message` 回 IM。session-id 失效 / Anthropic
  改 transcript 格式 → 从 `turns.jsonl` 重建 conversation(F118 详),**ccteam-owned SoT
  让 R1/R2 红线在模式 3 真正站住**(architect A5#5)。

**C. ccteam-owned `<.ccteam/chat/<bot>/turns.jsonl>` 作 conversation SoT**

同上 B Track 2。这是 R1/R2 红线在模式 3 的落地。

**D. `ClaudeTuiAdapter` impl `HarnessAdapter` trait**(Wave 1 STUB landed):

```rust
// crates/ccteam-core/src/execution/claude_tui.rs (Wave 1 STUB; Wave 2 fills body)
impl HarnessAdapter for ClaudeTuiAdapter {
    fn name(&self) -> &'static str { "claude-tui" }
    fn vendor(&self) -> AgentVendor { AgentVendor::Claude }

    async fn start_thread(&self, spec, ctx) -> Result<ThreadHandle> {
        let tmux_session = format!("ccteam-chat-{}-{}", ctx.slug, spec.role);
        ensure_hooks_installed(&ctx.project_dir)?;   // 写 .claude/settings.json hook 段
        tmux_new_session_detached(
            &tmux_session, &ctx.cwd,
            "claude --dangerously-skip-permissions"
        ).await?;
        Ok(ThreadHandle {
            vendor: AgentVendor::Claude,
            mode: ExecutionMode::Chat,
            identity: tmux_session,
            ..
        })
    }

    async fn submit_turn(&self, h, input) -> Result<TurnId> {
        match input {
            TurnInput::UserText(s) => {
                tmux_send_keys_literal(&h.identity, &s).await?;
                tmux_send_keys_enter(&h.identity).await?;
            }
            TurnInput::Artifact(p) => {
                let s = format!("Look at the file I just placed at {}", p.display());
                tmux_send_keys_literal(&h.identity, &s).await?;
                tmux_send_keys_enter(&h.identity).await?;
            }
            TurnInput::SystemDirective(d) => {
                // slash 命令透明:用户发 `compact` 我们送 `/compact`
                tmux_send_keys_literal(&h.identity, &format!("/{}", d)).await?;
                tmux_send_keys_enter(&h.identity).await?;
            }
            ..
        }
        Ok(TurnId::new(generate_turn_id()))
    }

    fn events(&self, h) -> BoxStream<'static, ThreadEvent> {
        // Wave 2 merge:track 1 (hooks → progress.jsonl tail) + track 2
        // (transcript jsonl byte-offset polling → turns.jsonl mirror)
        merge(progress_jsonl_tail(h), transcript_polling_tail(h))
    }

    async fn close_thread(&self, h) -> Result<()> {
        tmux_send_keys_literal(&h.identity, "/exit").await?;
        tmux_send_keys_enter(&h.identity).await?;
        sleep(500ms).await;
        tmux_kill_session_if_exists(&h.identity).await
    }
}
```

#### `workflow.yaml mode: chat` schema(收紧版,藏字段 — 不变)

```yaml
version: 0.6
mode: chat
agents:
  - role: helpful-bot
    # bot_name, compact_every_turns, hop_limit 全部 Rust default,user 不见
```

所有 advanced 字段 `#[serde(default)]`;`ccteam-creator` skill 自动写,用户从不 vim。

#### 文件清单(Wave 2 — Wave 1 已 STUB land claude_tui.rs)

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/claude_tui.rs`(Wave 1 STUB → Wave 2 填)| ~500 行 |
| `crates/ccteam-core/src/execution/transcript_tail.rs`(新,Wave 2)| byte-offset incremental read transcript jsonl |
| `crates/ccteam-core/src/execution/turns_mirror.rs`(新,Wave 2)| 写 ccteam-owned turns.jsonl SoT |
| `crates/ccteam-core/src/execution/attachments.rs`(新,Wave 2)| 非文本附件 mailbox(图片 / 大文件)|
| `crates/ccteam-core/src/workflow_schema.rs` | `mode: chat` schema 字段全 `serde(default)` 收紧 |
| `crates/ccteam-core/src/progress_event.rs` | 加 chat_* event 类型(turn_started / turn_completed / session_reset / hop_escalate)|
| `crates/ccteam-hooks/src/chat_progress.rs`(新,Wave 2)| Claude Code hooks → progress.jsonl 业务事件桥接 |
| `crates/ccteam-core/tests/claude_tui_test.rs`(新,Wave 2)| 6 测试 |

#### 验收(Wave 2)

1. host probe:多轮 chat session,turn 2 起 prompt cache hit(transcript `cache_read_input_tokens > 0`)
2. `/compact` 透明透传 + `SessionStart` hook 副作用 emit `chat_session_reset`,后续 turn input_tokens 显著下降 < 前 turn 1/3
3. session-id 失效 mock test:删 Anthropic `~/.claude/projects/...` → 从 `turns.jsonl` 重建 conversation,无丢失
4. send-keys -l 直送:200 sample unicode / emoji / multi-line / code-block input,0 pane corruption / 0 escape error
5. `cargo test`: baseline +6 通过

#### 不在范围

- 模式 3 Codex 路径(`CodexAppServerAdapter`)— F112 Wave 3
- bot-to-bot @ 链路细节 — F109 详

---

### F109 — IM bridge 统一 `openhuman/channels` + `ccteam-imd` 独立 daemon(用户 directive 不造轮子)

#### 痛点

V0.6.0 PRD 上版 F109 自造 `ccteam-im-bridge` crate(tokio task + teloxide TG 实现 + Slack/Discord 留 trait 扩展点)— 约 1000 行 Rust。用户 directive:**不造轮子**;同时 architect / researcher 共识:im-bridge 应该是独立 supervisor daemon(借 OMC `reply-listener.ts` 模式)而非 tokio task。

#### 需求

**A. 统一 transport = `openhuman/channels` Rust crate**

`openhuman/channels` 已实现 14+ IM 平台(traits.rs:5-60 `Channel` + `SendMessage` + `ChannelMessage` 抽象):
- V0.6.0 启用 feature:`telegram` / `slack` / `discord` / `email`
- V0.7 启用 feature:`lark` / `dingtalk` / `qq` / `wechat`(灰)/ `signal` / `imessage` / `matrix` / `whatsapp` / `irc` / `mattermost`

实施:`crates/ccteam-imd/Cargo.toml` 引 `openhuman` 作 dep + feature gate;0 自己写 IM transport 代码。

**B. `ccteam-imd` 独立 supervisor daemon binary**(borrowed OMC `reply-listener.ts` 模式)

```
crates/ccteam-imd/
├── Cargo.toml                        # deps: openhuman/channels, ccteam-core (trait only)
├── src/
│   ├── main.rs                       # daemon 入口
│   ├── supervisor.rs                 # per-bot child process supervisor
│   ├── inbound.rs                    # openhuman::Channel inbound → HarnessAdapter::submit_turn
│   ├── outbound.rs                   # ThreadEvent → openhuman::Channel send_message
│   ├── router.rs                     # bot-to-bot @ routing
│   ├── hop_tracker.rs                # 借 Codex AgentPath 模式
│   ├── nl_admin.rs                   # @ccteam <NL admin> → MCP tool call
│   ├── credentials.rs                # ~/.ccteam/im/credentials.json 0600 读写
│   ├── sanitize.rs                   # OMC reply-listener 两层 sanitize(backtick/$()/${}/control/bidi)
│   ├── rate_limit.rs                 # per-user token bucket
│   └── acl.rs                        # workflow.yaml im_acl 允许列表
├── systemd/ccteam-imd.service        # Linux systemd unit
└── tests/...
```

**C. `claude-plugins-official/telegram` 作 backup transport**(用户 directive)

默认 transport:`openhuman/telegram` 走 `ccteam-imd`。
用户 `/ccteam-im-setup --transport official-telegram` 切官方 Anthropic MCP 路径(`claude --channels plugin:telegram@claude-plugins-official`)。

两 path 互斥(避免同 bot token 双重 webhook 竞争);切换走 onboarding skill 状态文件 `~/.ccteam/im/transport.toml`。

**D. bot-to-bot @ routing + hop_limit — 100% 走 IM group**(IM Squad 完整体验,Wave 1 amendment)

> **Amended 2026-05-19**:bot-to-bot 路由**不走 cross-tmux IPC / FleetView
> SendMessage** — SendMessage 是 Claude Code in-proc 限定的 teammate-comms API,
> 跨 tmux session(每 bot 一个 detached `claude` TUI 进程)物理上不工作。100%
> 通过 IM group message 链路。IM history 即完整对话链,无 hidden channel,user
> 可见每一步,debug 时直接翻 TG 群消息记录。

借 Codex `AgentPath` 模式(researcher R11.A,codex-expert CX6#1):

```
user 在 TG 群 @helpful_bot:"review my plan"
  ↓ ccteam-imd 收到 group msg @helpful_bot,path = /root/turn-1
  ↓ tmux send-keys -l → helpful_bot TUI
  → helpful_bot 输出 "@critic_bot please review my plan: <text>"
  ↓ ccteam-imd 解析 transcript 中的 @critic_bot mention
  → ccteam-imd 在 TG 群里发消息 "@critic_bot please review my plan: <text>"
    (来源标 helpful_bot,path = /root/turn-1/turn-2)
  ↓ ccteam-imd 收到 group msg @critic_bot,tmux send-keys -l → critic_bot TUI
  → critic_bot 回 "Looks good but..."
  ↓ ccteam-imd 发回 TG 群(来源标 critic_bot)
  → user 看见完整对话链(@helpful_bot → @critic_bot → 回 user)
```

- 每条 bot output IM 消息都带 path header(`X-Ccteam-Path: /root/turn-1/turn-2`,
  group 显示时藏起来,debug 时用 `ccteam-control show-trail` 看)
- path depth ≥ `hop_limit`(默认 3,workflow.yaml `agents.<role>.hop_limit` 可覆盖)
  → ccteam-imd 在群里发 "⚠️ bot 间已链 3 轮,@<user> 请人工介入(链路:
  `/root/turn-1/turn-2/turn-3`)" + 不再 forward 给下一 bot
- bot 间通信走 IM = ccteam-imd 一处实现 routing / hop_limit / NL admin,**不需要**
  cross-tmux IPC mechanism(避免发明 `~/.ccteam/im/<bot>/inbox/` 文件通道 + 自造
  消息序号 + 自造去重 — 全部 IM 平台已经做完了)

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-imd/`(新 crate,~6 模块)| 详上 |
| `Cargo.toml`(workspace 根)| 加 member;加 dep openhuman `{ version = "0.x", features = [...] }` |
| `crates/ccteam-imd/tests/router_test.rs`(新)| 6 测试 |
| `crates/ccteam-imd/tests/dep_graph_test.rs`(新)| 守 ccteam-core 不依赖 openhuman / teloxide / slack_morphism / serenity |
| `crates/ccteam-cli/src/commands.rs::daemon_start` | 检测 chat workflow 存在 → spawn ccteam-imd child process |

#### 验收

1. TG host probe(开发者 own TG bot token + chat_id 走 `/ccteam-im-setup`):
   - DM hello world 跑通(`@helpful_bot hi` → 回复)
   - Group + bot-to-bot @:`@helpful_bot review my plan` → `@critic_bot` chain → critic 回 → 合成最终回
   - hop_limit:mock bot-to-bot 链 4 轮 → 第 4 轮 escalate,TG 收人工介入消息
2. `cargo tree -p ccteam-core | grep -E "openhuman|teloxide|slack"` 命中 0
3. `ccteam-imd` crash 自动重启:`kill -9` daemon → systemd / s6 / supervisor 拉起 → 状态恢复无丢失 turn
4. backup transport 切换:`/ccteam-im-setup --transport official-telegram` 后,新 chat workflow 走官方 MCP 路径
5. NL admin:TG 群里 `@ccteam pause helpful-bot` → bot 暂停;`@ccteam list` → 回 bot 列表

#### 不在范围

- 国内 IM 平台(`lark` / `dingtalk` / `wechat` / `qq`)— V0.7;V0.6.0 cargo feature gate **预留**但不启用
- DM 跨设备 sync — V0.7+
- IM webhook 公网 URL 自动 setup — 用户自行 ngrok / cloudflared

---

### F112 — Codex 集成 Option B 完整

#### 痛点 + 设计

详 README §4.3。Codex 用户面 4 个可见场景 + 内部架构 vendor 一等公民。

#### 需求

**A. trait `vendor: AgentVendor` 字段**(F107 已 cover)

**B. `CodexExecAdapter` impl(模式 2 codex)**

```rust
// crates/ccteam-core/src/execution/codex_exec.rs
impl HarnessAdapter for CodexExecAdapter {
    async fn start_thread(...) -> ThreadHandle {
        // spawn `codex exec --json --config sandbox=<...> --config approval=<...>`
        // stdout JSONL stream of ThreadEvent::ThreadStarted, TurnStarted, ...
    }
    async fn submit_turn(...) -> TurnId {
        // stdin write input (Codex stdin 接一次性 prompt)
    }
    fn events(...) -> Stream<ThreadEvent> {
        // tail stdout JSONL → ThreadEvent
    }
    async fn resume_thread(persistent_id) -> ThreadHandle {
        // spawn `codex resume <UUID>` 或 `codex resume --last`
    }
    async fn close_thread(...) {
        // kill process; sessions/<rollout>.jsonl 留存
    }
}
```

**C. `CodexAppServerAdapter` impl(模式 3 codex,Wave 3)**

```rust
impl HarnessAdapter for CodexAppServerAdapter {
    async fn start_thread(...) -> ThreadHandle {
        // 检测 codex app-server 是否已起,若否:`codex app-server --listen unix:///tmp/...`
        // UDS 上 JSON-RPC `initialize` + `thread/start` → 拿 thread-id
    }
    async fn submit_turn(...) -> TurnId {
        // UDS JSON-RPC `turn/start { threadId, input: [...] }`
    }
    fn events(...) -> Stream<ThreadEvent> {
        // UDS 推送 `item/started`, `item/agentMessage/delta`, `turn/completed`
    }
}
```

**D. 4 用户可见场景**(详 README §4.3 A/B/C/D)

- A. `/ccteam-advise <hard question>` parallel voting skill(`skills/ccteam-advise/SKILL.md`)— Codex + Claude 并行 advisor,合成显示
- B. Auto-critic in `ccteam-creator`:用户起 critic / reviewer / architect role 时,如装 codex + auth ok → 自动赋 `vendor: codex`(用户不见 yaml)
- C. Claude quota 触顶 opt-in fallback:`~/.ccteam/preferences.toml` `fallback.on_claude_quota = "codex"` 默认 off
- D. `/ccteam-team` 内置 Codex critic teammate(`/ccteam-team N "task with critic"` if codex available)

**E. 双 pricing table**(F107 已 cover):`crates/ccteam-cost/pricing/{anthropic,openai}.toml`

**F. budget 按 vendor 拆**:

```toml
# workflow.yaml(系统生成,user 不见)
budgets.claude.max_cost_usd_per_24h = 5.0
budgets.codex.max_cost_usd_per_24h = 2.0
```

UI 展示给 user 只 1 个聚合数(`/ccteam-control show-cost`);内部分 vendor。

**G. `agent_max_depth` recursion bomb guard**(借 Codex)

`HarnessAdapter` trait 内置 default impl,任何 adapter 都 enforce `max_spawn_depth: u32`(默认 5)。撞 limit → escalate event(同 fix-loop 红线 R6)。

**H. agent_naming 池**(借 Codex 50+ scientist nicknames)

`crates/ccteam-core/src/agent_naming.rs`:bot_name 默认从池子取(Newton / Curie / Einstein / Turing / Feynman / ...),user 不起名;`ccteam-creator` skill `Phase 3` 可让用户自定义。

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/execution/codex_exec.rs`(新)| `CodexExecAdapter` ~400 行 |
| `crates/ccteam-core/src/execution/codex_app_server.rs`(新,Wave 3)| `CodexAppServerAdapter` ~600 行 |
| `crates/ccteam-core/src/agent_naming.rs`(新)| 50 nickname 池 |
| `crates/ccteam-cli/src/commands.rs::run_doctor` | `--check-codex-version` + `--check-codex-auth` |
| `crates/ccteam-cost/src/budget.rs` | per-vendor budget caps |
| `crates/ccteam-cost/pricing/{anthropic,openai}.toml`(新)| 双 pricing |
| `skills/ccteam-advise/SKILL.md`(新)| ~100 行 parallel voting skill |
| `crates/ccteam-cli/tests/codex_exec_test.rs`(新)| 5 测试 |

#### 验收

1. host probe(开发者装 codex):
   - `/ccteam-advise "what's the best approach to X"` → Claude + Codex 并行 → 合成 verdict
   - `ccteam-creator` 起 workflow,task 含 "review" → critic role 自动选 codex(`progress.jsonl` 记录 `vendor: codex`)
   - Codex `agent_max_depth` 递归保护:bot spawn bot spawn bot 链 5 层后触 escalate
2. cost 双 vendor 聚合:Claude $1 + Codex $0.5 → `ccteam-control show-cost` 显示 "$1.5(详:claude $1 / codex $0.5)"
3. bot_name 自动 Newton / Curie / 不 collide:`/ccteam-team 3` 起 3 个 worker,name 都从池子取且不重复
4. doctor 检测:codex 缺 → `vendor: codex` 的 agent 报"装 codex 或切 Claude";不 nag 已经全 claude 的 workflow

#### 不在范围

- Anthropic / OpenAI 之外的 vendor(Gemini / DeepSeek / Qwen)— V0.7+
- cross-vendor in-proc team(物理不可能,详 CC9)— 永不
- Auto-fallback(Codex / Claude 互切)— C 场景 opt-in,不默认

---

### F115 — `.ccteam/handoffs/<workflow>/<stage>.md` 决策摘要机制

#### 痛点

ccteam 5 拓扑模式(chaining / orchestrator-worker / evaluator-optimizer 等)间的"为什么这么决定"丢失。current progress.jsonl 只记业务事件不记 rationale。fix-loop 撞 3 次撞错原因看不到上一轮决策 trace;wave 间跨 session 重启 lead 不知道前任做了啥。

OMC `.omc/handoffs/<stage>.md` 已经验证(`skills/team/SKILL.md` 1040 行解释机制)。borrow。

#### 需求

每个 stage / wave / fix-loop iteration 完成时,触发 hook **prompt 当前 agent 写一份 10-20 行 markdown 落盘**:

```markdown
<!-- .ccteam/handoffs/<workflow-slug>/stage-<N>-<role>.md -->
# Stage N: <Stage Name>

**Decided**: 
- ...

**Rejected**:
- ...

**Risks**:
- ...

**Files changed**:
- `path/to/foo.rs` — what + why

**Remaining**:
- ...
```

后续 stage / fix-loop iteration spawn prompt **自动 include** 前序 stage 的 handoff markdown(`{{include_prev_handoffs}}` template directive)。

#### 文件清单

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/handoff.rs`(新)| handoff hook trigger 逻辑 + markdown 模板 |
| `crates/ccteam-core/src/orchestrator.rs` | 在 `stage_done` event 触发 handoff prompt 注入 |
| `crates/ccteam-core/src/spawn_brief.rs` | spawn prompt 模板加 `{{include_prev_handoffs}}` |
| `crates/ccteam-core/tests/handoff_test.rs`(新)| 4 测试 |

#### 验收

1. workflow 跑 3 stage → `<project>/.ccteam/handoffs/<slug>/stage-{1,2,3}-<role>.md` 3 个文件生成,每文件 10-30 行
2. fix-loop 撞 2 次后,第 3 次 spawn prompt 含前 2 次 handoff(同 file glob)
3. fix-loop 3 次 escalate 时,user 看到 web UI 或 IM 展示最近 handoff(回答 "why did it loop")

#### 不在范围

- 不做 handoff 自动摘要(全 agent 写)
- 不做 handoff 跨 workflow share(每 workflow 独立目录)

---

### F116 — `ccteam-imd` 独立 daemon binary(F109 子 finding)

#### 痛点 + 设计

详 F109 §B。

#### 必须验收

- daemon crash 自动重启(systemd / s6 / Mac launchd)
- 0600 secret state(`~/.ccteam/im/credentials.json`)
- env allowlist(daemon spawn 子进程 PATH / HOME / USER / TMUX,**不传 ANTHROPIC_API_KEY / TELEGRAM_BOT_TOKEN**)
- 两层 sanitize(OMC `sanitizeReplyInput` + `sanitizeForTmux` lift)
- per-user rate limit(默认 10/min)
- workflow.yaml `chat_acl: { allow_user_ids: [...] }`(可选,允许列表)

---

### F118 — chat session 失效 last-N turn 重建

#### 痛点 + 设计

详 F108 §C。

`turns.jsonl` 失效场景下 ccteam 不再"fail-open + 沉默丢失"。当 `~/.claude/projects/...<sid>.jsonl` 失效:
- 从 `<project>/.ccteam/chat/<bot>/turns.jsonl` 读最后 N(配置)turn
- 重新拉起 `claude` 进程(fresh `claude -p`,无 `--resume`)
- 把 last-N turn 作 system prompt 注入(`<conversation_history>...</conversation_history>` block)
- emit `chat_session_reset_with_recovery` event
- bot 在 IM 主动发"我刚做了一次小检修,记忆已经从 `turns.jsonl` 恢复;有什么需要继续的?"

#### 验收

1. mock test:删 Anthropic session jsonl 文件 → `turns.jsonl` 重建后 bot 第一句话能正确引用上一轮对话
2. last-N 默认 20 turn;workflow.yaml `agents.<role>.recover_last_n_turns` 可覆盖
3. emit event 进 progress.jsonl,web UI 红色 banner 显示 "bot 失忆重建"

---

## EPIC C — 中文用户能用(V0.6.0 铺底)

**Epic 验收**:V0.6.0 ship 时**架构准备就绪**让 V0.7 国内 IM 零成本接入;中文文档先行;persona 库含中文。

下属 finding:F109(Epic C 部分 — openhuman feature gate 含国内 IM)+ F114(persona 含中文)+ 文档套件中文版默认。

---

### F109 Epic C 部分:openhuman feature gate 含国内 IM

V0.6.0 cargo feature 配置:

```toml
# crates/ccteam-imd/Cargo.toml
[features]
default = ["telegram", "slack", "discord"]
intl = ["telegram", "slack", "discord", "email", "irc"]
china = ["lark", "dingtalk", "qq", "wechat"]  # V0.7 启用,V0.6.0 预留 cargo feature 名
all = ["intl", "china", "signal", "imessage", "matrix", "whatsapp"]

telegram = ["openhuman/telegram"]
slack = ["openhuman/slack"]
discord = ["openhuman/discord"]
lark = ["openhuman/lark"]
dingtalk = ["openhuman/dingtalk"]
qq = ["openhuman/qq"]
wechat = ["openhuman/wechat"]
# ...
```

V0.6.0 binary 编译时 `--features default`;V0.7 加 `--features china` 即启用所有国内 IM,**零 ccteam 代码改动**(只 `/ccteam-im-setup` skill 加 4 个 platform option + 4 个 host probe 验证)。

---

### F114 Epic C 部分:persona 预设库含中文 prefab

`skills/ccteam-creator/personas/` 内置:

| Persona | 中文版 | 英文版 |
|---|---|---|
| 技术助手 / Code Helper | ✓ | ✓ |
| 写作助手 / Writing Assistant | ✓ | ✓ |
| 翻译 / Translator(双向)| ✓ | ✓ |
| 学习辅导 / Tutor | ✓ | ✓ |
| 项目监工 / Project Lead | ✓ | ✓ |
| 客服 / Customer Support | ✓ | ✓ |
| 心理咨询 / Therapist(谨慎)| ✓ | ✓ |
| 代码 critic / reviewer | ✓ | ✓ |

每 persona = `personas/<name>/{en,zh}/{role.md,tone.md,guardrails.md}` 三文件。`ccteam-creator` skill `Phase 3` 按用户 NL 语言自动选 zh / en 版本。

---

### 文档套件中文版默认

详 README §七。`docs/` 根 5 用户面文档(quickstart / user-manual / recipes / troubleshooting / README)中文版**默认**,英文翻译版按需(命名约定:`<doc>.en.md`)。

理由:用户画像主要为中文独立开发者(`docs/requirements.md` §用户画像)。英文用户社区国际化在 V0.8 专项(`docs/i18n/`)。

---

## F106 — 红线按"模式 × vendor"双轴 scope 重写

详 README §五矩阵表。改 `docs/tech-design.md` §0 + `CLAUDE.md` §三红线表。

#### 文件影响

| 文件 | 改动 |
|---|---|
| `docs/tech-design.md` §0 | 红线表加 vendor 列(Claude / Codex)|
| `CLAUDE.md` §三 | 红线表加"模式 × vendor"列,精简表 |
| `docs/architecture/orchestration-patterns.md` §一 | 5 拓扑模式 × 3 执行模式 × 2 vendor 适用矩阵(30 cell;✓ 17 / ⚠ 9 / ✗ 4)|

---

## 文档影响清单(全 V0.6.0)

| 文档 | 改动 |
|---|---|
| `CLAUDE.md` §一(状态表) | Workspace version → 0.6.0;baseline wave 3 ship 后回填;红线表加 vendor 列 |
| `docs/tech-design.md` | §0 红线表;§2.1 HarnessAdapter trait + 5 方法;§3.3 Codex 集成 + 双 pricing;§ 新建 chat-mode-design.md |
| `docs/architecture/orchestration-patterns.md` | §一加 5 × 3 × 2 = 30 cell 矩阵;§五加 V0.7 + V0.6.1 待立项 sugar(`vote`, `approval_gate`, `agent.router`)|
| `docs/interfaces.md` | MCP 工具子前缀;workflow.yaml `mode: chat` + `vendor:` schema;handoff 文件格式 |
| `docs/dev-coupling-audit.md` | F106-F118 各 1 finding |
| `docs/claude-code-best-practices.md` | 新章节 "Agent SDK / `claude -p --resume` + stream-json 模式 3 模式" |
| `docs/v0-6-0/host-probe.md`(新)| wave 4 验证产物归档 |
| `README.md`(repo 根)| ≤80 行,产品 pitch + value prop + 5 行 install + 链 quickstart |
| `docs/quickstart.md`(新,≤120 行)| 5 step Pocket Assistant 教程 |
| `docs/user-manual.md`(重写,≤300 行)| 5 preset 各一节;不出现 13 项内部术语 |
| `docs/recipes.md`(新,≤500 行)| 8-10 ready preset 模板 |
| `docs/troubleshooting.md`(新,≤400 行)| 50+ 故障条目 |
| `docs/advanced/{customize-workflow,multi-llm-codex,presets-reference}.md`(新)| advanced 文档,2-3 文件 |
