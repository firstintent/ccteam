# v0.8.5 PRD —— IM 命令面全量覆盖 + /sessions 状态 + 菜单 + skill 双通

> 状态:**doc-first**(动代码前出 PRD,本文 = 实现规约)。
> 设计来源:`docs/research/im-slash-coverage-decision.md`(已决策架构 D1–D6)+ 本轮 IM 讨论收敛(point-1/3/4、`claude-only → A` 翻案、多处细化)。
> 架构 SoT 仍是 `docs/tech-design.md` + `CLAUDE.md` §三红线;**协议以代码为准**。
>
> **⚓ 代码引用锚定版本(必读)**:
> - Codex:`references/codex/codex-rs` @ `origin/main b2344d8`(2026-06-03)。本 PRD §7 行号按此版核准;别的 checkout 行号会漂。
> - Claude:`references/claude-code` 是**逆向重建版**(非官方源)。D5/D6 语义**必须**对 PATH 上真实 `claude` binary smoke 验(§5.1),通过前不得依赖重建版行为。
> - `references/` gitignore、不入库、每机 checkout 不同 —— **行号脱离锚点版本即失效**,实现者起手先 `git -C references/codex log -1` 对版。

---

## 0. 范围与红线

### 0.1 本版包含(用户决策 A:im-slash 全方案 = v0.8.5,full dual-vendor)

| 模块 | 一句话 |
|---|---|
| **D1** 中立命令面 | `HarnessAdapter::handle_directive` + 中立类型 `Directive`/`DirectiveOutcome`/`ChoicePrompt`/`ChoiceSelection`;gateway 退纯路由;删 `TurnInput::SystemDirective` |
| **D2** Codex 命令面 | `CodexAppServerAdapter::handle_directive`:三层 resolution(内建映射表 → skills/list 动态 → Rejected);**含 tokenUsage 消费(净新增,见 §3-D2.4)** |
| **D3** ChoicePrompt 渲染/回填 | `SendMessage.options`;tg inline keyboard + `callback_query` 入站 / web chat WS chips / 纯文本兜底;三形态回填归一 |
| **D4** Codex 弹窗 | bare 弹窗命令两段式:list RPC → `NeedsChoice` → 重入 apply;带 args 直 apply |
| **D5** Claude 命令面 | `ClaudeTuiAdapter::handle_directive`:四通道 gate(prompt 透传 / local 安全集 / local-jsx 弹窗 / agent 弹窗→D6);**绝不盲发 bare 弹窗命令** |
| **D6** AskUserQuestion | PreToolUse hook `updatedInput`:「hook IS the interaction」;chat 变体 |
| **P1** 菜单 + /help | `setMyCommands` 注册 gateway 自有命令 + 新 `/help`;透传 slash 不进菜单 |
| **P3** /sessions 状态 | model + 上下文用量抽象成 session 属性,各 adapter 实现;显示 = **绝对值 + 百分比** |
| **P4** skill 双通 packaging | 补 `.agents/plugins/marketplace.json`(Codex 市场清单)+ 验 `codex plugin marketplace add` 端到端 |
| **F10**(前置·Codex 半边)| Codex chat 默认走 UDS、socket 无人创建且协议不兼容 → `/new codex` 100% 失败;改默认 **stdio**(fix A)。**D2/D4/P3-Codex 的硬前置**(Wave-3 第 0 步)|

### 0.2 本版**不**包含 / 已砍 / 正交不变

- **point-2(per-vendor 命令黑名单):砍** —— 被 D5 四通道 gate 取代(modal 卡死由「bare 弹窗→NeedsChoice、绝不盲发」根治,而非黑名单)。
- **subagent 统一:B(Claude-only)** —— 本就落 `.claude/agents`,无新活;Codex `.codex/agents/role.toml` 生成器推后。与命令面正交。
- **单项目专属 skill:Claude-only(`.claude/skills`)** —— 不做跨-vendor 文件投影。与 P4(插件级 skill 双通)不同层。
- **AGENTS.md+@import 指令统一:缓** —— Claude-only 下不挣钱(Claude 直接读 CLAUDE.md);等 Codex 真做多 agent 时再上,pre-v1 可重构、不留债。

> 注:`A` 翻的是**命令面**那块的 claude-only;上述三条与 slash 命令面正交,**仍按原决策**。

### 0.3 红线一致性(对 `CLAUDE.md §三`)

- **No prompt injection**:Claude prompt 型开放集**零知识透传**不变(D5 通道1);不向 pane/app-server 注入 system prompt。
- **不解析终端输出**:消费 Codex 原生 RPC(非 alleycat codex-emulation)+ Claude transcript jsonl/hook;`/esc` 是 send-keys **写**(非读 scrape),screenshot 只读例外已有。
- **vendor enum 无 default**:`handle_directive` **无 default impl** —— 新 vendor 必须显式声明命令面,杜绝静默降级类 bug 复发。
- **progress.jsonl SoT / resume-by-id / 文件系统状态面**:均不受影响。
- **ChoicePrompt = ApprovalIR 交互前身**:与 `ThreadEvent`/`ApprovalIR` 同层住 harness;日后 HITL 批准复用同型。

---

## 1. 背景与痛点映射

映射 `requirements.md` 核心痛点「**从 IM 日常驱动、替代 TUI session**」。本版补三个命令面缺口(research §1):

1. **Codex session 仅 `/compact` `/review` 走 RPC,其余 slash 静默降级为普通文本**(`/clear` 变发给模型的字面文本、无回执)——D1/D2 根治。
2. **弹窗选择型命令两 vendor 都不可用**:Codex 无人画列表;Claude bare `/model` send-keys 让隐藏 TUI 卡进 modal、吞后续输入——D4/D5 根治。
3. **agent 发起的 `AskUserQuestion` 在 chat session 弹 TUI picker,IM 用户不可见、turn 卡死**——D6 根治。

附带补 IM 日常运维三件:命令可发现性(P1)、session 状态可观测(P3)、跨-vendor skill 分发打通(P4)。

设计约束(research D1):后续会持续加 code agent(Gemini CLI / Amp / OpenCode…)与 IM channel(飞书 / 钉钉…)——**命令面实现内聚于各 vendor adapter、交互形态内聚于各 channel,两轴经中立类型解耦**。

---

## 2. 目标 / 非目标

**目标**
- 两 vendor 的 IM 命令面**无静默降级**:每条 slash 要么有意义地执行(RPC/透传/skill),要么显式回执(Rejected/Redirect)。
- 弹窗型命令在 IM 可用(列表→选择→应用),不卡 modal。
- agent 提问(AskUserQuestion)在 IM 可答。
- `/sessions` 显 model + 上下文(绝对值+%)。
- ccteam 插件 skill 真·Claude+Codex 双装。

**非目标**
- 不做 HITL 批准(`ApprovalIR` 仍仅类型占位;`ChoicePrompt` 为其交互前身但本版不接批准语义)。
- 不做 point-2 黑名单、subagent 跨-vendor、per-project skill 投影、AGENTS.md 统一(§0.2)。
- 不复刻 Codex TUI 的 `available_during_task` 守卫(§3-D2.5)。

---

## 3. 决策与实现规约

> D1–D6 完整设计论证见 `docs/research/im-slash-coverage-decision.md`。本节为**实现规约 + 本轮细化 + 锚定 b2344d8 的协议确认**,可据此实现。

### D1 中立命令面(住 `ccteam-harness`)

`HarnessAdapter` 新增(与 `submit_turn` 并列):

```rust
async fn handle_directive(&self, h: &ThreadHandle, d: Directive)
    -> Result<DirectiveOutcome, HarnessError>;   // 无 default impl

pub struct Directive { pub name: String, pub args: String, pub choice: Option<ChoiceSelection> }
pub enum DirectiveOutcome {
    Turn(TurnId),                 // 命令成为一个 turn(Claude 透传 / Codex review/init…)
    Done { receipt: String },     // 即时完成(RPC/override),receipt 回 IM
    NeedsChoice(ChoicePrompt),    // 需用户选择;IM 渲染,选择后带 choice 重入
    Rejected { reason: String },  // 显式拒绝(TUI-only / 不支持 / 未启用)
    Redirect { hint: String },    // 语义重定向(指引 gateway /new 等)
}
pub struct ChoicePrompt { pub token: String, pub title: String, pub options: Vec<ChoiceOption>, pub multi: bool }
pub struct ChoiceSelection { pub token: String, pub ids: Vec<String>, pub free_text: Option<String> }
```

- **gateway 退纯路由**:slash 文本 → `Directive` → `handle_directive` → outcome 渲染回 IM;`NeedsChoice` → pending-picker(TTL,keyed by chat+session,单飞)→ 选择回填重入。
- **删除**:`turn_input_for_session` 的 vendor 分支 + `compact|review` allowlist;`TurnInput::SystemDirective`(callers 全量 grep 迁移,**不留 alias**)。gateway 自有命令集(`/pair /new /use /cd /sessions /projects /newproject`)不变。
- **解耦矩阵**:新 vendor 只实现 `handle_directive`(不感知 channel);新 channel 只实现 ChoicePrompt 渲染+回填(不感知 vendor)。

### D2 `CodexAppServerAdapter::handle_directive`(协议映射,全量覆盖)

现 `submit_system_directive`(`codex_app_server.rs`)迁为 `handle_directive`,三层 resolution:

```
Directive(name, args, choice?)
  ├─ ① 内建映射表 match → RPC / 查询合成 / override
  ├─ ② miss → skills/list 缓存按 name 匹配(不分大小写;skills/changed 通知失效缓存)
  │     命中 → turn/start { input:[Skill{name,path}, Text{args}?] } → Turn
  └─ ③ 仍 miss → Rejected(附近似候选 + 「/skills 查看」)
```

**D2.1 内建映射表(六类,锚定 b2344d8 协议,§7 给行号)**

| 类 → outcome | 命令 → 动作(RPC) |
|---|---|
| RPC 直映射 | `/compact`→`thread/compact/start`;`/review [t]`→`review/start`(→Turn),target:无参=`UncommittedChanges`、`branch <b>`=`BaseBranch`、`commit <sha>`=`Commit`、其余=`Custom{instructions}`;`/interrupt`→`turn/interrupt`;`/fork`→`thread/fork`(新 threadId 回 gateway 注册新 session);`/rollback <n>`→`thread/rollback`;`/rename <name>`→`thread/name/set`;`/goal [obj]`→`thread/goal/{set\|get\|clear}`;`/stop`→`thread/backgroundTerminals/clean`;`/memories <m>`→`thread/memoryMode/set`;`/diff`→`command/exec` 跑 git diff;`/init`→`turn/start` 固定 prompt(→Turn);`/logout /login`→`account/*`(admin-gate) |
| 查询合成 → Done{receipt} | `/status`→`thread/read`+`account/rateLimits/read`+**tokenUsage 缓存(D2.4)**;`/model`(无参)→`model/list`;`/skills`→`skills/list`;`/mcp`→`mcpServerStatus/list`;`/hooks`→`hooks/list`;`/apps`→`app/list` |
| per-session override → Done(adapter 内存 map,照 `bridges` 先例 keyed by thread_id;daemon 重启丢失可接受)| `/model <id> [effort]`→`turn/start.model/effort`;`/personality <p>`;`/plan` `/collab <m>`→`collaboration_mode`(EXPERIMENTAL);`/permissions <preset>`→`approval_policy`+`sandbox_policy`(admin-gate) |
| 语义重定向 → Redirect | `/new` `/clear` `/resume`→指引 gateway `/new` `/use`(Codex 无 in-thread 等价) |
| TUI-only → Rejected | `/theme /vim /keymap /statusline /title /copy /raw /mention /ide /settings /realtime /quit /exit /feedback /rollout /ps /pets` + debug 类 |
| 错误传播 | server 状态机报错(如任务中 `/compact`)→ `SubmitFailed` 原文回 IM;**不**复刻 TUI `available_during_task` 守卫 |

**D2.2 active-turn 插话**:active turn 存在时普通 UserText 走 `turn/steer{expectedTurnId}` 而非 `turn/start`(对齐 Claude send-keys 体验;active-turn map 与 `/interrupt` 共用)。

**D2.3 冲突规则**:内建名优先于同名 skill;`enabled:false` 的 skill 回执提示未启用。

**D2.4 ⚠️ tokenUsage 消费(净新增工作,非 freebie)**:Codex **发** `thread/tokenUsage/updated` 通知,但 **ccteam 现在不消费**(确认:adapter 的 `events()` 未订阅该通知)。`/status`(D2.1 查询合成)与 P3 的 Codex 上下文% **都**依赖一个 adapter 内存的 tokenUsage 缓存。⇒ **明确列为一项独立工作**:在 codex_app_server `events()` 流里消费 `thread/tokenUsage/updated` → 维护 per-thread token 缓存 → 喂 `/status` 与 P3。不是「顺 D2 白嫖」。

**D2.5 命令分类的权威与 drift 防护**:`available_during_task()` / `supports_inline_args()` 是 Codex **TUI 内部方法**(`tui/src/slash_command.rs`),**不**走 app-server 线、ccteam 运行时**无法调**。⇒ 用它们作**编写 ~18 内建表的参考依据**,分类表由 **ccteam 自己持有**。防漂移:dev-time 测试 pin 一份 Codex `SlashCommand` 枚举名快照(当前 67 个,常量 list,手动从 `references/` 同步),断言「内建表 + reject 名单」覆盖之;bump codex 参考时重同步;新命令未分类则测试报错。**不**做 codex crate runtime 依赖(红线)。

> 正交事项(不在本版):Codex bot 要让 `skills/list` 看到项目 skill,需 `ccteam init`/`ccteam-creator` 把 skill 装进 Codex 发现路径(P4 解决插件级;项目级缓)。adapter 只消费 `skills/list`,不感知磁盘布局。

### D3 ChoicePrompt 的 channel 渲染与回填(IM 侧唯一新面)

中立 `ChoicePrompt` 住 harness(D1);IM 侧只做形态转换,channel **不感知选项语义**:

- **出站**:`SendMessage`(`ccteam-im/transport/mod.rs`)加 `options: Vec<ChoiceOption>`(空=普通消息,零破坏)。各 provider 自渲染:Telegram inline keyboard(入站补 `callback_query` 处理)/ web chat WS chips / Slack blocks / 兜底纯文本编号列表。新字段 `#[serde(default)]`。
- **回填**:统一归一为 `ChoiceSelection{token, ids, free_text}`。三种等价:按钮回调 / 纯数字短回复 / 完整 arg 形态(无状态兜底,picker 过期仍可用)。pending-picker state + 数字短回复解析在 gateway(keyed by chat+session,TTL,单飞)。
- **两类 producer 同一路径**:① adapter 的 `NeedsChoice`;② agent 发起提问(D6 hook)。

### D4 弹窗命令 — Codex 两段式

bare 弹窗命令在 adapter 内拿协议数据铺 `NeedsChoice(ChoicePrompt)`,选择经 `Directive.choice` 重入 apply;带 args 跳过列表直 apply:

| bare | 列表数据源 | apply |
|---|---|---|
| `/model` | `model/list`(含 `supportedReasoningEfforts`) | per-session override |
| `/collab` `/plan` | `collaborationMode/list`(EXPERIMENTAL) | override `collaboration_mode` |
| `/personality` | `Personality` 枚举 | override |
| `/permissions` | `AskForApproval`/`SandboxMode` 枚举 | override(admin-gate) |
| `/review` | 4 个 `ReviewTarget` 固定项 | `review/start`(branch/commit 二跳追问) |
| `/skills` | `skills/list` | 展示 + `skills/config/write` |
| `/memories` | memoryMode 枚举 | `thread/memoryMode/set` |
| `/resume` | `thread/list` | gateway `/use` 切换 |

### D5 `ClaudeTuiAdapter::handle_directive`(四通道 gate,复刻官方 Remote Control bridge 语义)

官方判定逻辑对照(`references/claude-code` 重建版,§7 行号;**语义须 §5.1 真 binary 验**):

1. **`prompt` 型(skill/自定义/plugin 命令)→ 照旧零知识透传**(send-keys → Turn)。开放集红线原样成立。
2. **`local` 安全集**(对照 `BRIDGE_SAFE_COMMANDS`:compact/clear/usage/summary/releaseNotes/files)→ 透传(→ Turn)。
3. **`local-jsx` 弹窗型**(有限内建名单,从 `references/claude-code/src/commands/` 同步):带 args→直通(官方行为即不弹窗,`model.tsx` `<SetModelAndClose>`,→ Turn);bare→`NeedsChoice`(选项来源:`MODEL_ALIASES` 等已知枚举/静态配置),选择重入后**改写成 arg-form send-keys**(→ Turn);无 arg 等价物的管理面板(`/config` `/agents`)→ `Rejected`/`Redirect`(设置类指引官方 config surface)。**绝不盲发 bare 弹窗命令**。
4. **agent 发起弹窗** → D6。

逃生舱:`/esc` 管理命令 = send-keys `Escape`(写不读,合规)+ 已有 screenshot tool 自助诊断。

红线张力(明示):Claude 侧维护 local-jsx 名单是对「ccteam 不知命令表」的**有限例外**——prompt 型开放集不变;名单为内建有限集、有官方源码同步源;不拦截的代价是 modal 卡死透传通道本身。

### D6 AskUserQuestion → PreToolUse hook `updatedInput`(「hook IS the interaction」)

机制(重建版逐级验,§7;**须 §5.1 真 binary 确认**):AskUserQuestion 是 permission 流交互工具,答案经 `updatedInput.answers` 回填;PreToolUse hook 输出 schema 一等支持 `updatedInput`;hook 返回 allow + `updatedInput` 时 picker **完全不渲染**(源码注释:「the hook IS the user interaction (e.g. headless wrapper that collected AskUserQuestion answers)」)。

链路:`agent 调 AskUserQuestion → PreToolUse hook(matcher:AskUserQuestion;扩 ensure_chat_hooks_installed)→ hook stdin 拿全量 questions/options → 经 daemon(mcp.sock 同路)构造同一中立 ChoicePrompt(producer=hook)发 IM → hook 阻塞等答案(blocking PreToolUse 已验证模式,600s 级,可配)→ 用户点按钮/回数字/回自由文本(=Other)→ hook stdout allow+updatedInput.answers → picker 跳过、模型直拿答案`。

兜底:超时→`deny` + reason「用户未应答,按最佳判断继续」(现 bg 版 `intercept_ask.rs` 同机制,chat 为新变体,bg deny 行为不变)。`updatedInput` 不可用时 fallback:deny + reason 携「用户已通过 IM 回答:…」(老牌文档化行为,功能等价)。

旁证:官方自家 channels 因「回复只 yes/no、无 updatedInput 通道」禁用了 AskUserQuestion;ChoicePrompt 三形态回填恰补此。

### P1 菜单 + /help

- `setMyCommands`(Telegram Bot API,照 `telegram.rs` 的 `api_url(method)` POST 模式,startup 调一次)注册 **gateway 自有命令**:`/new /use /cd /sessions /projects /newproject` + **新 `/help`**。
- 透传给 agent 的 slash(Claude /compact 等)**不进菜单**(vendor 相关、会误导)。
- `/help`:gateway 命令解释 + 说明「其余 / 命令透传给当前 agent」。
- 注意:菜单对零参数命令体验好;带参的(`/use <id>`)从菜单点出仍需手填,价值有限——可接受。

### P3 /sessions 状态:model + 上下文(session 属性,各 adapter 实现)

- **抽象**:session 加状态属性(model + context_used + window + pct)。plumbing 半成品已在:`ThreadEvent::TurnCompleted{usage: UnifiedTokenUsage}` 字段在(未填),`SpawnCtx::model_id` 在(IM 传 None)。
- **Claude adapter(必做)**:从 transcript **直接读**——分子 = `message.usage`(input + cache_creation + cache_read,§7 tokens.ts)求和(**不算**,transcript 最全);model = `message.model`;分母(窗口)= 读 `message.model` 的 `[1m]` 后缀(在 transcript 里,如 `claude-opus-4-8[1m]`)→ 1,000,000,否则基线 200k。**唯一常量 = 200k 基线**(不在 transcript;Claude 内部 capability 表 ccteam 拿不到);**不写死 per-model 清单**,[1m] 从数据读。
- **Codex adapter**:消费 D2.4 的 tokenUsage 缓存(**依赖 D2.4 这项净新增工作**);model 从 spawn/thread 事件。
- **显示**:绝对值 + 百分比,如 `ctx 188k / 1M (19%)`。
- 滞后性说明:usage 是回合后值,空闲准、turn 中途旧。

### P4 skill 双通 packaging

- **现状(已核 b2344d8)**:ccteam 有 per-plugin 清单 `.codex-plugin/plugin.json`(声明 `"skills":"./skills/"`),但**无** Codex **市场清单** `.agents/plugins/marketplace.json`;Codex `marketplace_add` 只读后者(无备选)。⇒ `codex plugin marketplace add firstintent/ccteam` 当前**失败**。
- **做**:给 ccteam 补 `.agents/plugins/marketplace.json`(指向本插件)+ 验 `codex plugin marketplace add` 端到端装上 → `skills/list` 能看到 ccteam 的 7 个 skill。
- **利好**:Codex 的 `find_plugin_manifest_path` 同认 `.codex-plugin/plugin.json`(主)与 `.claude-plugin/plugin.json`(备),packaging 可收敛。
- **content≠container 提醒**:能装到两边 ≠ 正文在两边都对;skill 正文写「用 Edit 工具 / spawn Task subagent」在 Codex 会加载但引用 Codex 没有的东西。本版只保证**装得上**;skill 正文 vendor-中立是作者纪律,投影解决不了(三层通用)。

### F10(Wave-3 前置)· Codex 默认 transport 改 stdio —— 根治 `/new codex` 默认 100% 失败

> 来源:dev session live web-chat smoke(`/cd … → /new codex dev` → `connect … app-server-control.sock: No such file (os error 2)`)。非 v0.8.4 回归,是 Codex chat 路径既有潜伏缺陷,被 gateway `/new codex` 首次踩到。**本 PRD 已对 ccteam 代码 + codex README 核实**(§7)。

**根因(已核 b2344d8 + ccteam 代码)**:
1. `default_adapter_factory`(daemon.rs:60-66)对 Codex 一律 mount `CodexAppServerAdapter::new()` → 默认 transport = UDS。
2. `client()`(codex_app_server.rs:153-206):仅 `CCTEAM_CODEX_APP_SERVER_TRANSPORT=stdio` 时走 stdio 自 spawn(:160-164);默认 `connect_uds`(:184)到 `$CODEX_HOME/app-server-control/app-server-control.sock`。**没有任何 caller 起这个 daemon**(ccteam start / gateway / doctor 都不起)→ socket 永不存在 → os error 2(用户实测命中)。
3. 协议不兼容(为何手动起 daemon 也救不了):codex `--listen unix://` 是 **websocket-over-HTTP-Upgrade**(codex `app-server/README.md:28`),而 ccteam `connect_uds`+`call()` 是**裸 JSONL**(codex_jsonrpc.rs:210 `to_vec`+`\n`)。握手对不上。**唯一与 ccteam 裸 JSONL 兼容的是 stdio**(`--listen stdio://` = JSONL,README:26)。

**fix A(采纳)**:codex chat 默认改走 **stdio**。最小改 `client()`:无 socket override + 无 transport override → `connect_stdio_command`;UDS 仅在**显式** `CCTEAM_CODEX_APP_SERVER_SOCKET` 时启用(留给自带 daemon 的 power user)。supervisor 每 bot 缓存一 adapter + `client()` memoize 一 client ⇒ 每 codex bot 恰好一个常驻 codex app-server 子进程、跨 resume/turn 复用,契合常驻 gateway 模型;stdio 只需 PATH 有 `codex`(doctor 已 gate)。

**don't-touch**:不动 Claude 路径;不动 notification 翻译层(正交);UDS 连接代码**保留**(显式 socket override 用),只改默认选路;**不**上「ccteam start 起 codex daemon + 自实现 ws-over-UDS 握手」那套(fix B,脆弱,弃)。

**测试**:`default_adapter_factory_codex_arm_returns_app_server_adapter`(daemon.rs:1137)只断 vendor/name,未触 transport 默认 —— 补一条「默认无 env 时选 stdio transport」断言钉死。

**为何 Wave-3 前置**:D2/D4/P3-Codex 全建在「能 spawn 一个能连的 codex session」上;F10 不修,Wave 3 无可测对象。故列 Wave-3 第 0 步(同 D6 真-claude smoke 之于 Wave 2)。

---

## 4. 跨切面硬约束

1. **channel-neutral / vendor-neutral 双轴**:命令面逻辑只在 adapter,交互形态只在 channel,中间只见中立类型(D1)。新 vendor / 新 channel 各只动一轴。
2. **`handle_directive` 无 default impl**:Codex(app-server + exec 两个 adapter)、Claude 都必须显式实现;`codex_exec.rs`(bg 路径)`handle_directive` **全拒**。
3. **新结构字段 `#[serde(default)]`**:`SendMessage.options`、`ChannelMessage` 等公共结构改动 → grep 全 impl(tg/slack/discord/mock/ws)+ 全 caller 一起改(持久化 ledger/state 向前兼容)。
4. **ledger 纪律**:复用现有 outbound choke;断言 multiset + pairing,**非** positional(防 v8.2 race flake 复发)。
5. **baseline gate(每 wave)**:`cargo test --workspace --exclude ccteam-web` ≥ 上 wave 数 + clippy `-D warnings` 0 + `cargo fmt --all --check`。退步不发 PR。

---

## 5. 风险 + 验证前置

### 5.1 ⚠️ D5/D6 contingent on real-claude smoke(最紧约束,Claude wave 第 0 步)

`references/claude-code` 是**逆向重建版**;D5 的命令分类 gate 与 D6 的 `updatedInput`-skips-picker 机制**整套**建在它上。**Claude wave 起手第 0 步 = 跑下列真 binary smoke**(对 PATH 真实 `claude`,~10–15min):

1. `/model <id>` 带 args **不**弹 picker、直接应用(对照 `model.tsx` 行为);bare `/model` 弹 picker。
2. 装 PreToolUse hook(matcher AskUserQuestion)返回 `allow + {updatedInput:{answers:[…]}}` → 触发 AskUserQuestion → 断言 **picker 未渲染** + transcript tool result 含答案。
3. confirm 最低 claude 版本。
4. bridge 透传:`/compact` 等 BRIDGE_SAFE 命令经远控/headless 不弹 local picker。

- **PASS** → 按 D5/D6 设计实现。
- **FAIL** → D6 退 **deny-with-reason fallback**(功能等价、老牌文档化);D5 退「local-jsx 一律 Rejected + 指引」。PRD 标注:**D6 approach contingent on smoke**。

### 5.2 其他风险 / 验证

- **D2.4 tokenUsage 消费**:净新增,不是 freebie(§3-D2.4)。先确认 ccteam 现确实未消费 `thread/tokenUsage/updated`,再实现订阅。
- **F10(Codex 默认 transport)**:已核实为真(ccteam 代码 + codex README,§7),fix A(默认 stdio)采纳,列 **Wave-3 第 0 步前置**(§3-F10)。不修则 Codex 半边全瘫、Wave 3 无可测对象。UDS 连接代码保留(显式 socket override),只改默认选路;不上 fix B。
- **Codex 协议**:§7 已对 b2344d8 重核,17 项全在、无 breaking(`approvals_reviewer` 为 turn/start 新增 sibling,不破坏)。实现者起手再 `git -C references/codex log -1` 对版。
- **Telegram inline keyboard + `callback_query` 入站**:新路径,真机 smoke。
- **scripted JSON-RPC peer**(`tests/codex_app_server_test.rs`):逐 RPC arm 扩;skill resolution 加 `skills/list` 夹具。
- **既有 dual-vendor 测试**(`gateway.rs` 的 directive 测试):断言改 `DirectiveOutcome`(`/clear` 对 codex 从 UserText echo 改 `Redirect`);`FakeAdapter` 补 `handle_directive`。
- **trait 抽象自检**:新 vendor 最小面 = `handle_directive` 一个方法;新 channel 最小面 = options 渲染 + 回填归一——各写一个假实现测试锁解耦边界。

---

## 6. ship-gate(最后一 wave)

按 `CLAUDE.md §五.7`:
- **内部 SoT**:`CLAUDE.md §一` baseline 回填 + `docs/tech-design.md`(handle_directive/ChoicePrompt 架构 + 协议→代码指针表)+ workspace `Cargo.toml` `0.8.4→0.8.5`。
- **用户面**:`README.md`(英,不含版本进展)+ `docs/usage.md`(命令手册:菜单/`/help`、各 slash 在 IM 的行为、`/sessions` 字段、skill 双装)。
- **版本归档**:本 `docs/versions/v0-8-5/` + 每 wave handoff(五段)。
- **MCP 工具面**:若新增 tool / 改 `STUB_TOOLS` → `ccteam doctor --verify-mcp` 0 drift。

---

## 7. 源码证据索引(⚓ 锚定 codex b2344d8 / claude-code 重建版,本仓 checkout 同日)

> 实现者起手 `git -C references/codex log -1` 应见 `b2344d8`;否则行号需重定位。Claude 重建版语义须 §5.1 验。

**Codex app-server v2(`references/codex/codex-rs/`,b2344d8 已核 17 项全在)**

| 事实 | 位置 |
|---|---|
| RPC 方法面(method→params/response) | `app-server-protocol/src/protocol/common.rs`(thread/compact/start:541、review/start:797、turn/interrupt:762、turn/steer:756、thread/fork:457、thread/rollback:562、thread/name/set:492、thread/goal/{set:497,get:502,clear:507}、thread/backgroundTerminals/clean:556、thread/memoryMode/set:524、skills/list:608、skills/changed:1500、model/list:803、mcpServerStatus/list:898、hooks/list:618、app/list:683、thread/read:583、account/rateLimits/read:946、collaborationMode/list:864[experimental])|
| `turn/start` per-turn override 全字段 | `protocol/v2/turn.rs`(approval_policy:99、approvals_reviewer:103、sandbox_policy:106、model:114、effort:126、personality:132、collaboration_mode:145)|
| `ReviewTarget` 4 variant | `protocol/v2/review.rs:43-65`(UncommittedChanges / BaseBranch{branch} / Commit{sha,title} / Custom{instructions})|
| `TurnSteerParams.expected_turn_id` | `protocol/v2/turn.rs:160-176` |
| `UserInput::Skill{name,path}` 一等 variant | `protocol/v2/turn.rs:290-293` |
| `skills/list` 形状 + `skills/changed` | `protocol/v2/plugin.rs:34-36`(SkillsListResponse.data)、`:834-841`(notification)|
| `Model.supported_reasoning_efforts` | `protocol/v2/model.rs:90` |
| `AskForApproval` / `SandboxPolicy` 枚举 | `shared.rs:162` / `permissions.rs:430` |
| `thread/fork` 返回新 thread.id | `protocol/v2/thread.rs:553-580` |
| SKILL.md loader + `.agents/skills` roots | `core-skills/src/loader.rs`(SKILLS_FILENAME:107、AGENTS_DIR_NAME ".agents":108、SKILLS_DIR_NAME:111;roots:repo .agents/skills:376、~/.agents/skills:323、$CODEX_HOME/skills:313、/etc/codex/skills:345、plugin roots:263;MAX_SCAN_DEPTH 6:124)|
| TUI slash 全集(drift 快照源,67 变体)+ 元数据方法 | `tui/src/slash_command.rs`(`available_during_task()` / `supports_inline_args()` 等 = TUI 内部,非 RPC,仅作分类参考)|
| 插件市场清单只读 `.agents/plugins/marketplace.json` | `core-plugins/src/{loader.rs:258, marketplace_add.rs:245, manager.rs:1012}`;`find_plugin_manifest_path` 认 `.codex-plugin`+`.claude-plugin`(`marketplace_tests.rs:8`)|

**Claude(`references/claude-code/src/`,逆向重建版 —— 语义须 §5.1 真 binary 验)**

| 事实 | 位置 |
|---|---|
| 命令类型 prompt/local/local-jsx | `types/command.ts:217-218` |
| bridge 安全判定 `isBridgeSafeCommand` | `commands.ts:747-751` |
| `BRIDGE_SAFE_COMMANDS`(compact/clear/usage/summary/releaseNotes/files)| `commands.ts:726-735` |
| `/model` args 直通不弹窗(`<SetModelAndClose>`)| `commands/model/model.tsx:270-275` |
| local-jsx 弹窗命令清单 | `commands/*/index.ts`(model/config/agents/plan/hooks…)|
| hook `updatedInput` schema | `types/hooks.ts:70-78` |
| 「hook IS the interaction」+ picker 跳过 | `services/tools/toolHooks.ts:356-392`(注释:358-362)|
| 官方 channels 禁用 AskUserQuestion | `hooks/toolPermission/handlers/interactiveHandler.ts:465-469` |
| transcript usage(P3 分子)+ model | `utils/tokens.ts:60-63`(input+cache_creation+cache_read sum)、`:24/41`(message.model)|
| 上下文窗口 / %(P3 分母,**仅作公式参考,ccteam 自实现**)| `utils/context.ts:10`(200k 默认)、`:74`([1m]→1M)、`:147`(% 计算)|

**ccteam 现状接入点**

| 事实 | 位置 |
|---|---|
| gateway slash 路由(待删 vendor 分支 + SystemDirective)| `crates/ccteam-im/src/gateway.rs`(`turn_input_for_session` ~1414-1423;gateway 自有命令 dispatch ~461-551)|
| Codex 现 directive 入口 | `crates/ccteam-harness/src/execution/codex_app_server.rs`(`submit_system_directive`)|
| Claude hook 安装点(D6 扩 matcher)| `crates/ccteam-harness/src/execution/claude_tui.rs`(`ensure_chat_hooks_installed`)|
| bg intercept-ask(D6 chat 变体源)| `crates/ccteam-hooks`(`intercept_ask.rs`)|
| Telegram API 调用模式(P1 setMyCommands)| `crates/ccteam-im/src/transport/providers/telegram.rs`(`api_url(method)` POST)|
| **F10** Codex adapter 工厂(默认 mount UDS adapter)| `crates/ccteam-im/src/daemon.rs:60-66`;测试 `:1137` 只断 vendor/name、待补 transport 断言 |
| **F10** Codex transport 选路(fix A 落点)| `crates/ccteam-harness/src/execution/codex_app_server.rs`(`client()` 153-206;UDS default :184;stdio-if-env :160-164;`APP_SERVER_TRANSPORT_ENV` "uds(default)" :74-75;`APP_SERVER_SOCKET_ENV` :71)|
| **F10** ccteam 裸 JSONL wire(为何 UDS-ws 不兼容)| `crates/ccteam-harness/src/execution/codex_jsonrpc.rs`(`connect_uds` :94 / `connect_stdio_command` :107 / `to_vec`+`\n` :210)|
| **F10** codex `--listen` transport 模式(unix=ws / stdio=JSONL)| `references/codex/codex-rs/app-server/README.md:26-28`(b2344d8)|
