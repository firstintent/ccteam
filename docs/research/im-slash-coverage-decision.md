# IM slash 命令全量覆盖方案决策(Codex 全量 + 弹窗命令 + AskUserQuestion)

状态:**已决策**(讨论收敛,未实现;动代码前按惯例出 PRD)
日期:2026-06-04
调研依据:`references/codex/codex-rs/`(app-server v2 协议 + TUI slash dispatch)、`references/claude-code/`(逆向重建版源码,见 §6 验证前置)、本仓现状代码

---

## 1. 背景与范围

IM 驱动 session 的命令面现状有三个缺口:

1. Codex session 仅 `/compact` `/review` 走 RPC,**其余 slash 静默降级为普通文本**(`/clear` 变成发给模型的字面文本,无回执);
2. 弹窗选择型命令两 vendor 都不可用:Codex 无人画列表,Claude bare `/model` send-keys 会让隐藏 TUI 卡进 modal、吞掉后续输入;
3. agent 发起的 `AskUserQuestion` 在 chat session 中弹 TUI picker,IM 用户不可见,turn 卡死。

本决策覆盖三者。设计约束:后续会持续增加 code agent(Gemini CLI / Amp / OpenCode…)与 IM channel(飞书 / 钉钉…),**命令面实现必须内聚于各 vendor adapter、交互形态必须内聚于各 channel,两轴经中立类型解耦**(D1)。

核心事实:Codex TUI 约 50 个 slash 中,headless 有意义的 ~18 个**全部有 app-server 原生 RPC 或 per-turn 参数对应物**;Codex 没有自定义 slash 概念,自定义扩展 = Skill(`UserInput::Skill` 是 `turn/start` 输入的一等 variant);Claude 弹窗命令带参数时官方行为就是不弹窗直接应用。

## 2. 决策清单

### D1 中立命令面:`handle_directive` 下沉各 adapter,gateway/IM 双向零知识

`HarnessAdapter` trait 新增命令面方法(与 `submit_turn` 并列,`crates/ccteam-harness/src/lib.rs`):

```rust
async fn handle_directive(&self, h: &ThreadHandle, d: Directive)
    -> Result<DirectiveOutcome, HarnessError>;

pub struct Directive {
    pub name: String,                    // 去掉 `/` 的命令名
    pub args: String,                    // 余下整串,参数语义 adapter 自解
    pub choice: Option<ChoiceSelection>, // NeedsChoice 二次重入时携带用户选择
}
pub enum DirectiveOutcome {
    Turn(TurnId),                // 命令成为一个 turn(Claude 透传 / Codex review…)
    Done { receipt: String },    // 即时完成(RPC / override),receipt 回 IM
    NeedsChoice(ChoicePrompt),   // 需用户选择;IM 渲染,选择后带 choice 重入
    Rejected { reason: String }, // 显式拒绝(TUI-only / 不支持 / 未启用)
    Redirect { hint: String },   // 语义重定向(指引 gateway /new 等)
}
```

- **中立类型住 harness**:`Directive` / `DirectiveOutcome` / `ChoicePrompt { token, title, options, multi }` / `ChoiceSelection { token, ids, free_text }` 与 `ThreadEvent`/`ApprovalIR` 同层;`ChoicePrompt` 即 ApprovalIR 的交互前身,HITL 批准日后复用同型
- **无 default impl**:新增 vendor 必须显式实现自己的命令面(对齐「vendor enum 无 default」红线),杜绝静默降级类 bug 在新 adapter 复发
- **gateway 职责收敛为纯路由**:slash 文本 → `Directive` → `handle_directive` → outcome 渲染回 IM;`NeedsChoice` → pending-picker(TTL)→ 选择回填重入。`turn_input_for_session` 的 vendor 分支与 `compact|review` allowlist 删除;`TurnInput::SystemDirective` 随迁移删除(callers 全量 grep,不留 alias)。gateway 自有命令集(`/pair /new /use /cd /sessions /projects /newproject`)不变;`codex_exec.rs`(bg 路径)`handle_directive` 全拒
- **解耦矩阵**:M 个 code-agent adapter × N 个 IM channel,两轴只见中立类型——新 vendor(Gemini CLI / Amp / OpenCode…)只实现 `handle_directive`,不感知任何 channel;新 channel(飞书 / 钉钉…)只实现 ChoicePrompt 渲染 + 回填,不感知任何 vendor

### D2 `CodexAppServerAdapter::handle_directive`:协议映射,全量覆盖

现 `submit_system_directive`(`crates/ccteam-harness/src/execution/codex_app_server.rs:581-626`)迁移为 `handle_directive` 实现,三层 resolution:

```
Directive(name, args, choice?)
  ├─ ① 内建映射表 match → RPC / 查询合成 / override(下表)
  ├─ ② miss → skills/list 缓存按 name 匹配(不区分大小写;skills/changed 通知失效缓存)
  │     命中 → turn/start { input: [Skill{name,path}, Text{args}?] } → Turn
  └─ ③ 仍 miss → Rejected(附近似候选 + 「/skills 查看可用」)
```

内建映射表(六类,全量;outcome 标注):

| 类 → outcome | 命令 → 动作 |
|---|---|
| RPC 直映射 → `Done`/`Turn` | `/compact`→`thread/compact/start`(已有);`/review [args]`→`review/start`(→`Turn`),target 解析:无参=`uncommittedChanges`、`branch <b>`、`commit <sha>`、其余整串=`custom{instructions}`;`/interrupt`(IM 专有,TUI 是 Esc)→`turn/interrupt`;`/fork`→`thread/fork`(新 threadId 回 gateway 注册新 session);`/rollback <n>`→`thread/rollback`;`/rename <name>`→`thread/name/set`;`/goal [obj]`→`thread/goal/set|get|clear`;`/stop`→`thread/backgroundTerminals/clean`;`/memories <mode>`→`thread/memoryMode/set`;`/diff`→`command/exec` 跑 `git diff`;`/init`→`turn/start` 固定 init prompt(→`Turn`);`/logout` `/login`→`account/*`(admin-gate) |
| 查询合成 → `Done{receipt}` | `/status`→`thread/read`+`account/rateLimits/read`+已桥接 tokenUsage 缓存;`/model`(无参)→`model/list`;`/skills`→`skills/list`;`/mcp`→`mcpServerStatus/list`;`/hooks`→`hooks/list`;`/apps`→`app/list` |
| per-session override → `Done`(adapter 内存 map,照 `bridges` 先例 keyed by thread_id;daemon 重启丢失可接受)| `/model <id> [effort]`→`turn/start.model/effort`;`/personality <p>`→`personality`;`/plan` `/collab <mode>`→`collaboration_mode`(EXPERIMENTAL);`/permissions <preset>`→`approval_policy`+`sandbox_policy`(admin-gate) |
| 语义重定向 → `Redirect` | `/new` `/clear` `/resume`→指引 gateway `/new` `/use`(Codex 无 in-thread 等价物) |
| TUI-only → `Rejected` | `/theme` `/vim` `/keymap` `/statusline` `/title` `/copy` `/raw` `/mention` `/ide` `/settings` `/realtime` `/quit` `/exit` `/feedback` `/rollout` `/ps` + debug 类 |
| 错误传播 | server 状态机报错(如任务中 `/compact`)→ `SubmitFailed` 原文回 IM;不复刻 TUI 的 `available_during_task` 守卫 |

附带:active turn 存在时普通 UserText 走 `turn/steer { expectedTurnId }` 而非 `turn/start`(任务中插话对齐 Claude send-keys 体验;active-turn map 与 `/interrupt` 共用)。

冲突规则:内建名优先于同名 skill;`enabled:false` 的 skill 回执提示未启用。

正交事项(不在本决策内):Codex bot 要让 `skills/list` 看到项目 skill,需 `ccteam init`/`ccteam-creator` 把 skill 装进 Codex 发现路径;adapter 只消费 `skills/list`,不感知磁盘布局。

### D3 ChoicePrompt 的 channel 渲染与回填(IM 侧唯一新面)

中立 `ChoicePrompt` 类型在 harness(D1);IM 侧只做形态转换,channel 不感知选项语义:

- 出站:`SendMessage`(`crates/ccteam-im/src/transport/mod.rs:111-125`)加 `options: Vec<ChoiceOption>`(空 = 普通消息,零破坏)。各 provider 自渲染:Telegram inline keyboard(入站补 `callback_query` 处理)/ web chat WS chips / Slack blocks / 兜底纯文本编号列表
- 回填:统一归一为 `ChoiceSelection { token, ids, free_text }`。三种回答等价:按钮回调 / 纯数字短回复 / 完整 arg 形态(无状态兜底,picker 过期仍可用)。pending-picker state 与数字短回复解析在 gateway(keyed by chat+session,TTL,单飞;gateway 自有交互面,不构成 vendor 命令知识)
- `ChoicePrompt` 有两类 producer,同一渲染/回填路径:① adapter 的 `NeedsChoice` outcome;② agent 发起的提问(D6 hook,即未来 ApprovalIR)

### D4 弹窗命令 — Codex:两段式「list RPC → `NeedsChoice` → 重入 apply」

bare 弹窗命令在 adapter 内拿协议数据铺成 `NeedsChoice(ChoicePrompt)`,选择经 `Directive.choice` 重入后 apply;带 args 跳过列表直接 apply:

| bare 命令 | 列表数据源 | apply |
|---|---|---|
| `/model` | `model/list`(含 `supportedReasoningEfforts`) | per-session override |
| `/collab` `/plan` | `collaborationMode/list` | override `collaboration_mode` |
| `/personality` | `Personality` 枚举(`supports_personality` 过滤) | override |
| `/permissions` | `AskForApproval`/`SandboxMode` 枚举 | override(admin-gate) |
| `/review` | 4 个 `ReviewTarget` 固定选项 | `review/start`(branch/commit 二跳追问) |
| `/skills` | `skills/list` | 展示 + `skills/config/write` |
| `/memories` | memoryMode 枚举 | `thread/memoryMode/set` |
| `/resume` | `thread/list` | gateway `/use` 切换 |

### D5 `ClaudeTuiAdapter::handle_directive`:四通道 gate(复刻官方 Remote Control bridge 语义)

官方判定逻辑对照搬(`references/claude-code/src/commands.ts:715-752`,PR #19134 → allowlist 放宽):

1. **`prompt` 型(skill / 自定义 / plugin 命令)→ 照旧零知识透传**(send-keys → `Turn`)。官方判定:expand to text,safe by construction。开放集红线原样成立
2. **`local` 安全集**(compact / clear / usage / summary / files…,对照 `BRIDGE_SAFE_COMMANDS`)→ 照旧透传(→ `Turn`)
3. **`local-jsx` 弹窗型**(有限内建名单,从 `references/claude-code/src/commands/` 同步):带 args → 直通(官方行为即不弹窗,`model.tsx:270-274` `<SetModelAndClose>`,→ `Turn`);bare → `NeedsChoice`(选项来源:`MODEL_ALIASES` 等已知枚举/静态配置),选择重入后改写成 arg-form send-keys(→ `Turn`);无 arg 等价物的管理面板(`/config` `/agents`)→ `Rejected`/`Redirect`,设置类指引官方 config surface(settings.json / spawn argv)。**绝不盲发 bare 弹窗命令**
4. **agent 发起的弹窗** → D6

逃生舱:`/esc` 管理命令 = send-keys `Escape`(写不读,合规)+ 已有 screenshot tool 自助诊断。

红线张力,明示:Claude 侧维护 local-jsx 名单是对「ccteam 不知道命令表」的有限例外——红线护的 prompt 型开放集不变;名单为内建有限集、有官方源码同步源;不拦截的代价是 modal 卡死透传通道本身。

### D6 AskUserQuestion → PreToolUse hook `updatedInput`(「hook IS the interaction」)

机制(参考源码逐级验证):AskUserQuestion 本质是 permission 流交互工具,答案经 `updatedInput.answers` 回填(`interactiveHandler.ts:395-397`);PreToolUse hook 输出 schema 一等支持 `updatedInput`(`types/hooks.ts:70-78`);hook 返回 allow + `updatedInput` 时 picker 完全不渲染——源码注释原文:「the hook IS the user interaction (e.g. **headless wrapper that collected AskUserQuestion answers**)」(`toolHooks.ts:356-364`)。

链路:

```
agent 调 AskUserQuestion
→ PreToolUse hook(matcher: AskUserQuestion;扩 ensure_chat_hooks_installed,
  现仅装 chat-progress,claude_tui.rs:144-171)
→ hook stdin 拿全量 questions/options/multiSelect
→ 经 daemon(mcp.sock 同路)构造同一中立 ChoicePrompt(producer=hook)发 IM
→ hook 阻塞等答案(blocking PreToolUse 已验证模式,600s 级窗口,timeout 可配)
→ 用户点按钮 / 回数字 / 回自由文本(自由文本 = Other)
→ hook stdout:
  {"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow",
   "updatedInput":{...原 input,"answers":{"<问题>":"<选中 label>"}}}}
→ picker 跳过,模型直接拿到答案
```

超时兜底:返回 `deny` + reason「用户未应答,按最佳判断继续」(现 bg 版 `intercept_ask.rs` 同机制,改 reason 内容;bg 版 deny 行为保持不变,chat 版为新变体)。`updatedInput` 不可用时的 fallback:deny + reason 携带「用户已通过 IM 回答:…」——老牌文档化行为,功能等价。

旁证:官方自己的 channels(CC 内置 TG 通道)因「回复只能 yes/no、无 updatedInput 通道」禁用了 AskUserQuestion(`interactiveHandler.ts:466-468`);ChoicePrompt 三形态回填恰好补掉这块。

## 3. 红线一致性

- 消费 Codex 原生协议,非 alleycat 式 codex-emulation;per-adapter best-fit 不变
- 不碰 agent prompt(No prompt injection);不 scrape pane(`/esc` 是写不是读;screenshot 只读例外已有)
- Claude prompt 型开放集零知识透传不变;Codex 命令面为有限内建枚举,adapter 持映射表与红线语义不冲突
- `handle_directive` 无 default impl,新 vendor 必须显式声明命令面(对齐「vendor enum 无 default」)
- 文件系统状态面、resume-by-id、progress.jsonl SoT 均不受影响

## 4. 实现落点

| 层 | 改动 |
|---|---|
| `ccteam-harness/src/lib.rs` | trait `handle_directive` + 中立类型 `Directive` / `DirectiveOutcome` / `ChoicePrompt` / `ChoiceSelection`(D1);删 `TurnInput::SystemDirective`(callers 全量迁移) |
| `ccteam-im/transport` | `SendMessage.options` + 各 provider 渲染/回填归一(tg inline keyboard + callback_query 入站 / ws chips / 文本兜底)(D3) |
| `ccteam-im/gateway` | 纯路由:slash→`Directive`→outcome 渲染;pending-picker + 选择重入(D1/D3);`/fork` 新 session 注册钩子 |
| `codex_app_server.rs` | `handle_directive` 实现:三层 resolution + 六类映射表(D2)+ bare 弹窗 `NeedsChoice`(D4)+ active-turn map(`/interrupt` + `turn/steer`)+ per-session override map |
| `claude_tui.rs` | `handle_directive` 实现:四通道 gate + bare→`NeedsChoice`→arg-form 改写 + `/esc`(D5);`ensure_chat_hooks_installed` 加 AskUserQuestion matcher(D6) |
| `codex_exec.rs` | `handle_directive` 显式全拒(bg 路径无交互命令面) |
| `ccteam-hooks` | `intercept-ask` chat 变体:截→daemon→IM→阻塞→`updatedInput` 回答案(D6) |

## 5. 验证前置(ship gate 之前)

1. **claude binary smoke(必做,~10min)**:`references/claude-code` 为逆向重建版,`updatedInput` 语义需对 PATH 真实 `claude` 验证——装 hook → 触发 AskUserQuestion → 断言 picker 未弹 + transcript tool result 含答案;同时确认最低版本。不通过则启用 deny-with-reason fallback
2. scripted JSON-RPC peer(`tests/codex_app_server_test.rs`)逐 RPC arm 扩脚本;skill resolution 加 `skills/list` 夹具
3. 既有 dual-vendor 测试更新(`gateway.rs:1920-1971`):断言改为 `DirectiveOutcome`(`/clear` 对 codex 从 UserText echo 改为 `Redirect`);`FakeAdapter` 补 `handle_directive`
4. trait 抽象自检:新增 vendor 的最小实现面 = `handle_directive` 一个方法;新增 channel 的最小实现面 = options 渲染 + 回填归一——两条各写一个假实现测试锁住解耦边界
5. Telegram inline keyboard 真机 smoke(callback_query 入站新路径)

## 6. 关键源码证据索引

| 事实 | 位置 |
|---|---|
| Codex TUI slash 全集 + 任务中可用性 | `references/codex/codex-rs/tui/src/slash_command.rs` |
| app-server v2 全方法面 | `references/codex/codex-rs/app-server-protocol/src/protocol/common.rs:435-1028` |
| `turn/start` per-turn override 全字段 | `.../protocol/v2/turn.rs:49`(model/effort/personality/collaboration_mode/approval_policy/sandbox_policy) |
| `ReviewTarget` 4 variant | `.../protocol/v2/review.rs` |
| `UserInput::Skill` 一等 variant | `.../protocol/v2/turn.rs:238-259` |
| `skills/list` 形状 + `skills/changed` 通知 | `.../protocol/v2/plugin.rs:21-36, 356-372`;`common.rs:1433` |
| TUI skill = `$mention` 客户端解析(server 不展开文本) | `references/codex/codex-rs/tui/src/chatwidget.rs:5521-5556` |
| Claude bridge 三型分类 gate | `references/claude-code/src/commands.ts:715-752` |
| `/model` args 直通不弹窗 | `references/claude-code/src/commands/model/model.tsx:255-274` |
| hook `updatedInput` schema | `references/claude-code/src/types/hooks.ts:70-78` |
| 「hook IS the interaction」 | `references/claude-code/src/services/tools/toolHooks.ts:356-389` |
| 官方 channels 禁用 AskUserQuestion | `references/claude-code/src/hooks/toolPermission/handlers/interactiveHandler.ts:466-468` |
| ccteam 现状接入点 | `gateway.rs:1422-1431`;`codex_app_server.rs:581-654`;`claude_tui.rs:144-171, 516-547`;`intercept_ask.rs:38` |
