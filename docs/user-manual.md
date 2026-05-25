# ccteam User Manual

> ccteam 让你**用一句中文/英文召唤一个 AI 团队**,跑在你电脑上、接进你的 IM。本手册带你跑通典型场景。
>
> **零 yaml,零 CLI 命令记忆,零术语**。你只需要会用 Claude Code session 输入 slash 命令 + 自然语言。

---

## §0 我该用哪个命令?(决策树速查)

**先看决策树**,挑一条路再下读对应章节。完整决策树详解见 [task-to-command.md](task-to-command.md)。

```
你想做的事                                  → 用这个                  → 本手册章节
──────────────────────────────────────────────────────────────────────────────────
摸底新代码库(60s 零依赖)                    /ccteam-scan --quick      §2.0
仓库 navigability 体检                        /ccteam-scan              §2.0
开发 / 修 bug / 重构(全程盯着干)            /ccteam-team              §2.2 Team Sprint
跨 vendor 第二意见(Claude + Codex)          /ccteam-advise            §2.6 Advise
IM 私聊助理(长跑)                          /ccteam-creator           §2.4 Pocket Assistant
IM 圆桌(多 bot)                            /ccteam-creator           §2.5 IM Squad
夜里跑长任务(hands-off)                    /ccteam-creator           §2.3 Overnight Builder
程序化起 / 管 chat bot                        mcp__ccteam__chat_*       §4.7
看 / 暂停 / 改 persona / 加工具              /ccteam-control           §4.1-§4.6
诊断 / 验 Codex auto-critic                   ccteam doctor             §4.8
配 / 改 IM token                             /ccteam-im-setup          §2.4 + quickstart
daemon 优雅停止 / tmux reattach / 崩溃恢复    kill -TERM <pid>          §6
不确定?用自然语言问                          /ccteam "<NL>"            §3.1
```

> §1 介绍 ccteam 是什么 / 三种对话入口;§2 详跑每个场景;§3 是入口手册;§4 是 admin 操作 + MCP 工具参考;§5 cost;§6 daemon 运维。**只想用,跳到 §2 对应小节即可**。

---

## §1 ccteam 是什么

ccteam = "在 Claude session 里召唤一个 AI 团队,长跑 + 跨设备 + 接进 IM"。

对比同类工具:

| 工具 | ccteam 的差异 |
|---|---|
| ChatGPT app / Claude.ai web | ccteam 能动你电脑(读文件、改代码、跑命令);它们不能。 |
| Cursor / Cline / Aider | ccteam 不锁在 IDE,在 IM / 终端 / web 都能用,跨设备。 |
| OMC / Devin | ccteam 长跑 + 接进多平台 IM,关电脑也继续跑。 |

### 三种跟 ccteam 对话的入口

| 入口 | 用啥 | 啥时用 |
|---|---|---|
| 🟢 Claude session 内 | `/ccteam <NL>` 总入口 或 5 个 sub-slash | 已开着 Claude,顺手做事 |
| 🟢 IM(TG / Slack / Discord)| 私聊 bot,或群里 `@ccteam <NL>` | 离电脑,手机想跟 AI 互动 |
| 🟡 Web 仪表板 | 浏览器开 `http://localhost:7331` | 想看图监控,不"操作" |

> **不需要打开新 terminal 跑命令**。所有功能都通过 Claude session slash + NL + IM 触发。

---

## §2 用法(preset)

每种用法 = "一个 ccteam 团队的预设配方"。你只挑场景,不挑实现细节。

### §2.0 Code-Base Scan — 摸底新代码库

**啥时用**:刚 clone 一个仓库,或者第一次接触 ccteam 想 60 秒看到价值。**零依赖** — 不开 daemon、不需 IM token、不需 Codex,只调一次 Sonnet(失败兜底 Haiku)。

**怎么起**:

```bash
cd path/to/repo
claude
```

session 里:

```
/ccteam-scan --quick                  # 60 秒快速体检(首选)
/ccteam-scan                          # 完整 navigability audit(大型 monorepo 用,适合 contributor)
```

`--quick` 模式三问:
1. **主语言 / framework / 入口** — 一句话告诉你这仓库是干啥的
2. **TODO / FIXME 热点** — 哪几个文件 / 模块技术债集中
3. **`CLAUDE.md` / `README.md` / `AGENTS.md` 状态** — 是否齐全,需不需要 `claude /init` 写初版

报告写到 `<repo>/.ccteam/codebase-scan.md`(带 frontmatter `quick: true`)。24h 内复跑直接显示上次报告 + 建议加 `--force` 强制重扫。

**Cost 估**:每次 ≈ $0.01-0.03(单 Sonnet 调用)。

---

### §2.1 Solo Sidekick — 写代码时临时召唤一个帮手

**啥时用**:你在 Claude 写代码,临时需要个独立 agent 干 1 个小事(读完整个 `src/` 总结、扫漏洞、跑数据分析),不想污染主 session 上下文。

**怎么起**:

```
/ccteam <你的请求>
# 例:/ccteam 扫一下 src/ 找所有 TODO 注释,列成 markdown 表
```

ccteam 总 dispatcher 判定是 Solo Sidekick,自动跑一个 sub-agent,结果回当前 session。

**例 session**:

```
You:    /ccteam 扫一下 src/ 找所有 TODO 注释
Claude: 召唤 sidekick(工具:Read/Glob/Grep)...
        [1 分钟后]
        Found 17 TODOs across 8 files:
        | File | Line | Note |
        ...
```

**Cost 估**:每次 ≈ $0.01-0.05(中小型仓库)。

---

### §2.2 Team Sprint — 几小时冲一波

**啥时用**:你有一个 1-3 小时能干完的任务,要 3-5 个 agent 协作(fix 一批 TS error、分模块并行 refactor、多视角 review 设计)。

**怎么起**:

```
/ccteam-team <N> "<task>"
# 例:/ccteam-team 3 "fix all TypeScript errors in src/"
# 例:/ccteam-team 5:reviewer "review the new auth design"
```

Team Sprint 会起 N 个 teammate(role 你指或 ccteam 自决)+ 一个 lead 调度,当前 Claude session 看实时通信。

**例 session**:

```
You:    /ccteam-team 3 "fix all TypeScript errors in src/"
Claude: TEAM PLAN
        =========
        Team name: ts-fix-team
        Proposed teammates:
          1. fixer-a (sonnet, blue)   — src/auth/, src/api/
          2. fixer-b (sonnet, green)  — src/ui/, src/utils/
          3. reviewer (sonnet, yellow) — verify all fixes
        回 'go' 起。
You:    go
[3 个 teammate 并行跑,通信尽收眼底]
```

**Cost 估**:1 小时 sprint ≈ $1-3。

---

### §2.3 Overnight Builder — 丢任务睡觉去

**啥时用**:任务需长跑几小时到几天(全栈搭一个 todo app、夜里 test→fix→test 循环、跨周做依赖升级)。

**怎么起**:

```
/ccteam-creator "夜里给我跑 qa-loop:测试失败自动 fix,每 fix 一轮 commit,直到 24h 或 cost cap"
```

ccteam-creator 进入对话向导,自动判断是 Overnight Builder,问你:
- 24h cost cap 多少 $
- 失败 N 次后是否中途叫醒你(TG 推送)

确认后起长跑 daemon,关电脑(笔记本合盖,daemon 后台继续)早上你看摘要。

**例 session**(简化):

```
You:    /ccteam-creator "夜里跑 qa-loop,$5 cap,撞 3 次失败叫醒我"
Creator: 计划:
         - Preset: Overnight Builder
         - Loop: test → fix → test(max 5 轮 / 4 小时)
         - 24h cost cap: $5
         - 失败叫醒:TG @你
         回 "go"。
You:    go
[第二天早 TG]
@you 完成 ✓:5 测试修复,$1.82,详 http://localhost:7331/p/qa-loop
```

**Cost 估**:1 晚 ≈ $1-5(自动 cap 兜底)。

---

### §2.4 Pocket Assistant — 手机 IM 私聊一个 AI 助理 ⭐

**啥时用**:你想要一个**随时随地在手机上找它说话**的 AI 助理。它能读你电脑文件、跑命令、做长期记事、定期推送。

**怎么起**(第一次):

```
/ccteam-im-setup                                          # 一次性绑 TG bot
/ccteam-creator "做个 TG 私聊助理 bot:管邮件 + 早 7 点推 GitHub PR 摘要"
```

→ 详 [quickstart.md](quickstart.md)。

起完之后,打开 TG,搜你的 bot,私聊。bot 跑在你电脑(合盖也继续),手机只是入口。

**例 session**(在 TG):

```
You:   今天 GitHub 上有什么新 PR?
Bot:   找到 3 个新 PR:
       1. #1234 feat(auth): SSO support(@alice)— 大改,建议你看
       2. #1235 docs: typo fix(@bob)— 小改
       3. #1236 fix(api): rate limit edge(@you)— 你自己开的
       回 "1" / "2" / "3" 我帮你打开摘要;回 "diff 1" 看 1 号 diff。
```

**换 persona / 加能力**(随时,任意 Claude session):

```
/ccteam-control change-persona helper-bot "改成英文 + 更幽默"
/ccteam-control add-tool helper-bot "scan ~/Downloads for new PDFs and summarize"
```

**Cost 估**:轻度使用 ≈ $0.5-2 / 天。

---

### §2.5 IM Squad — IM 群里多个 bot 互相 @ 协作

**啥时用**:你想搞个 "AI 圆桌会"。一个 TG 群,2-5 个 bot 各持职责(架构师 + 评审 + 文档 + 测试),你 @ 任一 bot 开局,bot 间 @ 对方协作,在群里给你共识。

**怎么起**:

```
/ccteam-creator "做个 TG 多 bot 团队:架构师 + 评审员 + 写手,讨论我贴进去的设计文档"
```

Creator 自动:
- 起一个 Telegram 群(或让你加进自己的群)
- 4-5 个 bot 各持 persona,默认 handle 是 scientist nickname(`@curie`/`@galileo`/`@newton` …)── creator 会先列出来让你 review/override
- 每个 bot 的 persona 文件末尾自动注入 **squad teammate roster**(其他兄弟 bot 的 handle + role + persona label),所以它们知道 `@dev` 不是 Task subagent 而是真实兄弟 bot
- 配 bot-to-bot @ 路由(纯 daemon 内 Rust mpsc 通道,不经 TG round-trip)
- 限 3 层 @ 链路(防 ping-pong 循环)
- 群里多 bot 时每条回复自动加 `from <handle>:` 前缀,让你分清谁说话

**例 session**(在 TG 群):

```
You:        @arch 看下我贴的 API 设计 [paste...]
arch:       from arch:
            @reviewer 你怎么看 v3 endpoint 设计?
reviewer:   from reviewer:
            @writer 我建议改回 v2,文档需要 ...
writer:     from writer:
            已起草文档,贴 PR 链接 → #1240
            @you 共识达成,请你拍板
```

**意外路径**:
- 群里 @错 handle(如 `@unknownname`)→ ccteam 会回 `Unknown handle '@unknownname'. Available bots in this chat: @arch @reviewer @writer`
- DM 多 bot 共享 chat_id 时不 @ 直接说话 → 回 `Multiple bots in this chat. Specify one: ...`
- 群里查可用 bot:`@ccteam list bots`

**Cost 估**:1 次 30 分钟 round-table ≈ $0.3-1。

---

### §2.6 Advise — 跨 vendor 第二意见(Claude + Codex)

**啥时用**:你想拿一个**硬问题 / 设计决策 / PR 评审 / 棘手 bug** 去同时问 Claude 和 Codex,看两个 vendor 各给一份答案再自己拍板。**单视角不够**的场景。

**两个 sub-mode**:

| Sub-mode | 行为 | 何时用 |
|---|---|---|
| `vote` | 并行调多个 advisor,合成 verdict(majority / unanimous / split)+ 每 vendor 的 raw 答复 | 想看"两边到底同不同意" |
| `parallel` | 并行调,**不合成**,直接 dump 每 vendor 的 raw 答复 | 想自己读两份对比 |

**怎么起**:

```
/ccteam-advise vote "<question>"
/ccteam-advise parallel "<question>"
```

或显式选 vendor 子集:

```
/ccteam-advise vote --vendors=claude,codex "<question>"
```

**前置**:Codex 装好 + `codex login` 跑过(可用 `ccteam doctor --check-codex-auto-critic` 一条命令验)。**没装也能跑** — graceful 降级单 Claude advisor,verdict prose 写 "Codex unavailable: <reason>",**不报错**。

**Budget 守门**:每 vendor 单独 `max_cost_usd_per_24h` cap,记账落 `<ccteam_root>/cost-budget.json`(48h 自动 GC)。某 vendor 撞顶 → 静默跳过该 vendor 跑其余,verdict 标注 `budget_exhausted`。**Codex critic 路径** 同样走该 ledger ── 跨 vendor 调用都在同一账上,`@ccteam cost today` 与 `ccteam doctor --check-cost-orphan` 看到的数字是真实跨 vendor 累计(没有 spawn 路径绕开 ledger)。

**底层 MCP 工具**:`mcp__ccteam__advise_vote` / `mcp__ccteam__advise_parallel`(可绕过 skill 直接调,适合 CI / batch)。

**Cost 估**:单次 vote ≈ $0.01-0.05(两 vendor 各跑一次)。

---

## §3 怎么跟 ccteam 对话(三种入口详解)

### §3.1 在 Claude session 内

总入口 `/ccteam <NL>` 是万能的 — 发啥都行,它路由到对应 sub-skill。

直接跳过 router 也行,7 个 sub-slash:

| Slash | 干啥 |
|---|---|
| `/ccteam-scan [--quick]` | 摸底新代码库(`--quick` 60s 零依赖,默认完整 navigability audit)|
| `/ccteam-team <N> "<task>"` | 起临时 team(Team Sprint)|
| `/ccteam-creator "<NL>"` | 起新 workflow / 改现有 workflow ── 第一次在仓库内跑时先自动调 `ccteam probe-project` 探测 monorepo / 主语言,生成 yaml 的 `scope:` 段直接按结果 pre-populate(不必手改即可跑)|
| `/ccteam-control <subcmd>` | 管已有 workflow(暂停 / 恢复 / 查 cost / 改 persona)|
| `/ccteam-im-setup` | 一次性绑 IM token(TG / Slack / Discord)|
| `/ccteam-advise vote\|parallel "<question>"` | Claude + Codex 并行二答案(没装 Codex 自动降级单 Claude)|

### §3.2 在 IM 端

**私聊你的 bot**:直接说话,bot 走 Pocket Assistant 模式。

**群里跟 ccteam 总管对话**:发 `@ccteam <NL>`(NL admin),它走 meta-agent 路由:

```
@ccteam pause helper-bot           # 暂停某个 bot
@ccteam resume helper-bot          # 恢复
@ccteam list bots                  # 列所有跑着的 bot
@ccteam cost today                 # 今日 cost(真 USD,跨 vendor)
@ccteam cost helper-bot            # 单 slug 24h cost
@ccteam stop everything            # 紧急停所有
```

`cost today` 返当日 rolling 24h 真 USD(从 `<ccteam_root>/cost-budget.json` ledger 读),分 Claude / Codex 两行 + 总额 + cap + remaining,撞 80% cap 自动加 "⚠️ approaching daily budget cap" 前缀。例:

```
ccteam cost today
  rolling 24h cost: Claude $0.1832 + Codex $0.0917 = total $0.2749
  cap: $0.50/24h · remaining: $0.2251
  active bots: 2 (filter: none)
  full breakdown: `/ccteam-control show-cost`
```

### §3.3 Web 仪表板

浏览器开 `http://localhost:7331`,看:
- 所有 workflow 实时状态
- 每个 bot 的对话历史
- 24h cost 趋势图
- 失败 / 告警列表

Web **只看不操作**。所有控制走 Claude session slash 或 IM。

---

## §4 Admin / Chat / Doctor MCP 操作参考

> 所有操作的首选路径是 **MCP 工具**(`mcp__ccteam__*`)。如果 MCP 未注册(尚未跑 `ccteam doctor --install-mcp`),可用对应的 Bash fallback。MCP 工具按 5 个子前缀分组:`workflow_*` / `chat_*` / `advise_*` / `admin_*` / `screenshot_*`。
>
> §4.1-§4.6 是 admin 路径(workflow + persona / tool 管理);§4.7 是 chat 生命周期 MCP;§4.8 是 doctor 子命令。

### §4.1 暂停 / 恢复 workflow

**暂停**(停止自动派任务,但不 kill tmux session):

```
/ccteam-control pause <slug>
```

底层调用:

| MCP 工具 | Bash fallback |
|---|---|
| `mcp__ccteam__workflow_pause { "slug": "<slug>" }` | `ccteam pause <slug>` |

返回:`{ "ok": true, "slug": "...", "user_pause_pending": true }`

**恢复**(清除暂停,下一个 orchestrator tick 重启派任务):

```
/ccteam-control resume <slug>
```

| MCP 工具 | Bash fallback |
|---|---|
| `mcp__ccteam__workflow_resume { "slug": "<slug>" }` | `ccteam internal resume <slug>` |

返回:`{ "ok": true, "slug": "..." }`

---

### §4.2 列出所有 workflow(查看状态)

```
/ccteam-control list
```

| MCP 工具 | Bash fallback |
|---|---|
| `mcp__ccteam__admin_ls {}` | `ccteam ls --format json` |

返回示例:

```json
{
  "projects": [
    {
      "slug": "dev-helper",
      "team": "dev",
      "phase_state": "idle",
      "cost_used_usd": 0.83,
      "cost_24h_usd": 0.83,
      "cost_active_usd": 0.0
    }
  ],
  "orchestrator": {
    "daemon_health": { "status": "healthy", "age_secs": 12 }
  }
}
```

---

### §4.3 今日 Cost(`@ccteam cost today`)

**Claude session**:

```
/ccteam-control show-cost
```

**IM 端**:

```
@ccteam cost today           # 全 workflow 24h 汇总
@ccteam cost <slug>          # 单 workflow 24h
```

`/ccteam-control show-cost` 底层调 `mcp__ccteam__admin_ls`,从响应里读每个 project 的 `cost_24h_usd`(= 最近 24h 内的 token 花费;`cost_used_usd` = 项目生命周期累计)。

`@ccteam cost today` 走 IM 路径:从 `<ccteam_root>/cost-budget.json` ledger 聚合 ── 该 ledger 是 advise / Codex critic / 跨 vendor 调用的统一账,**不是** bot 数估算;输出格式见 §3.2。两条路径数字应一致(同源 ledger);若 `ccteam doctor --check-cost-orphan` 报 warn 表示某 spawn 路径绕过了 ledger,见 §4.8。

---

### §4.4 紧急停止 workflow(`@ccteam stop everything`)

**单个 role 全停**:

```
/ccteam-control stop <slug> <role>
```

| MCP 工具 | Bash fallback |
|---|---|
| `mcp__ccteam__workflow_stop_agent { "slug": "...", "role": "..." }` | `ccteam internal stop <slug> <role>` |

`session_id` 省略 = 停该 role 下所有 session。  
底层写 `.ccteam/stop_signal/<role>___all__` 标记文件,orchestrator 在下一 tick 拆除 session。

**IM 端 stop everything**(需 CONFIRM 二次确认,防误触):

```
@ccteam stop everything
# bot 回: "Are you sure? Reply CONFIRM to stop all."
CONFIRM
```

---

### §4.5 修改 bot Persona(`change-persona`)

修改某个 chat-mode bot 的行为、语言、工具集:

```
/ccteam-control change-persona <bot> "<NL 描述你想改什么>"
```

skill 会:
1. 读 `<project>/.claude/agents/<bot>.md`(当前 persona)
2. 把你的 NL 合并进 persona body
3. 调 MCP 工具写回:

| MCP 工具 | Bash fallback |
|---|---|
| `mcp__ccteam__admin_change_persona { "slug": "...", "bot": "...", "new_persona_md": "<完整 md>" }` | `ccteam admin change-persona <slug> <bot> -`(full markdown via stdin)|

**`new_persona_md` 必须是完整文件内容**(YAML frontmatter + body),skill 负责组装;MCP 工具直接写文件,不做 NL parse。

成功后 `progress.jsonl` 追加 `persona_changed` 事件,bot 下次 turn 自动 pick up 新 persona。

---

### §4.6 给 bot 加工具(`add-tool`)

```
/ccteam-control add-tool <bot> "<NL 描述能力>"
```

skill 把 NL 翻译成 Claude Code 工具名(如 `WebFetch`、`Bash`)后调:

| MCP 工具 | Bash fallback |
|---|---|
| `mcp__ccteam__admin_add_tool { "slug": "...", "bot": "...", "tool_descriptor": "WebFetch" }` | `ccteam admin add-tool <slug> <bot> WebFetch` |

操作是幂等的 — 重复加同一工具不报错(返回 `already_present: true`)。  
成功后 `progress.jsonl` 追加 `tool_added` 事件。

---

### §4.7 Chat MCP 工具(程序化起 / 管 / 抓历史 / 重置 bot)

`/ccteam-creator` 是 NL 向导,适合新手 onboarding;**已经懂 ccteam** 后从 Claude session 内直接调 MCP 更快。6 个工具构成完整生命周期:

| 工具 | 用途 | 关键参数 |
|---|---|---|
| `mcp__ccteam__chat_register_bot` | 注册一个 chat bot 入 daemon registry | `{ slug, role, vendor, im_chat_id }`(`vendor` 严格小写枚举 `claude` / `codex`)|
| `mcp__ccteam__chat_list_bots` | 列所有 registered bot + heartbeat 状态 | `{ slug? }`(省 = 全列)|
| `mcp__ccteam__chat_send_input` | 给某 bot 推一段文本(走 inbox 文件,不直注 tmux pane)| `{ slug, role, text }` |
| `mcp__ccteam__chat_history` | 抓 bot 对话历史(从 `turns.jsonl` 读)| `{ slug, role, limit? }` |
| `mcp__ccteam__chat_reset` | 重置 session:归档 `turns.jsonl` + 清 outbound cursor + 清 transcript cursor | `{ slug, role }` |
| `mcp__ccteam__chat_unregister_bot` | 注销 bot(daemon 自行停 tmux,清 heartbeat sidecar)| `{ slug, role }` |

**Heartbeat 语义**:`chat_list_bots` 返回 `running: true` 当且仅当 `<root>/imd/registry/<slug>/<role>.heartbeat` mtime 在 30 秒内。daemon 重启时,bot tmux session 不丢,**自动 reattach**(详 §6)。

**Reset 行为**:`chat_reset` 把当前 `turns.jsonl` 移到 `archive/turns-<unix-ms>.jsonl`,然后清磁盘 cursor 文件 + 让 daemon 重置内存里的 cursor — 两边同步,**不存在 "磁盘清干净但 daemon 还在读旧位置" 的 race**。

**典型 JSON example**:

```json
{
  "name": "chat_register_bot",
  "args": { "slug": "helper", "role": "main", "vendor": "claude", "im_chat_id": "123456789" }
}
```

返回:`{ "ok": true, "slug": "helper", "role": "main", "registered_path": "..." }`

适合场景:CI 编排起多个 bot 做 batch / 给已跑 bot 推一个程序化指令 / 抓 bot 历史做 audit / 自动化测试。

---

### §4.8 Doctor 子命令

`ccteam doctor` 是诊断入口,在任意终端跑(也可 `Bash("ccteam doctor")` 从 Claude session 内调):

| 子命令 / flag | 用途 |
|---|---|
| `ccteam doctor` | 全套诊断(claude CLI 版本 / MCP 注册 / tmux / pidfile 路径 / web port)|
| `ccteam doctor --install-mcp` | 重写 `~/.claude.json` / 项目 `.mcp.json` 里的 `ccteam` MCP server 注册 |
| `ccteam doctor --verify-mcp [--json]` | 自检 MCP 工具表面齐全,输出 `total / active / stubs` 与 per-group 分项,任何 STUB 注册 → exit 1(CI gate)|
| `ccteam doctor --check-codex` | 验 codex binary 存在 + auth ok |
| `ccteam doctor --check-codex-auto-critic` | 验 codex 可用且能跑一次 `codex exec --json` canary(exit 0 = ok / 2 = binary 缺 / 3 = output 格式不对)|
| `ccteam doctor --check-cost-orphan` | 对账 24h 内 `<ccteam_root>/cost-budget.json` ledger 行数与每项目 progress.jsonl 的 `agent_done` event 数 ── 不对账 = 某 spawn 路径绕过 ledger,WARN per vendor,catch cost visibility leak |
| `ccteam doctor --full` | 全套 + 详细输出(收集成给 issue)|

`--check-codex-auto-critic` 是 `/ccteam-creator` Phase 3.5 内部调的同一条命令 ── 决定 critic 角色是否自动设 `vendor: codex`。手动验等价。

`--verify-mcp` 的预期输出:

```
MCP tool surface verification
total tools:    27 (expected 27)
active:         27
stubs:          0

per-group breakdown:
  workflow_:    7 active / 0 stub
  chat_:        9 active / 0 stub
  advise_:      2 active / 0 stub
  admin_:       8 active / 0 stub
  screenshot:   1 active / 0 stub

verdict: PASS — all 27 tools live, no production STUBs.
```

非 0 stub 或 unexpected stub 注册 → verdict 转 `FAIL` + exit code 1,适合放 CI。

`--check-cost-orphan` 的预期输出(健康):

```
ccteam doctor --check-cost-orphan
[ok] claude: 12 agent_done vs 12 ledger rows
[ok] codex:  3 agent_done vs 3 ledger rows
verdict: OK — ledger reconciled with progress.jsonl over 24h.
```

不对账时 WARN 列出 vendor + 差额,提示哪条 spawn 路径漏写 ledger(典型是新加的自定义 adapter 没接 ledger hook)。

---

## §5 Cost 透明

ccteam 默认每个 workflow 有 24h cost cap(creator 起项目时问你)。

**实时查看**:

```
/ccteam-control show-cost                       # 所有 workflow 今日 cost
/ccteam-control show-cost helper-bot --days 7   # 单 workflow,7 天
```

或 IM 端:`@ccteam cost today`。

**读输出**:

```
Workflow         Today    24h cap   7d trend   Status
helper-bot       $0.83    $2.00     ↗ +12%     OK
qa-loop          $4.12    $5.00     ↘ -3%      ⚠ 82% of cap
```

撞 90% → 自动 TG 推送提醒;撞 100% → 自动暂停 workflow(不会偷偷烧钱)。

---

## §6 运维:daemon 优雅停止 + tmux reattach

ccteam 把 orchestrator + IM bridge(`ccteam-imd`) + web 仪表板都装进 `ccteam start` 这一个 tokio runtime,前台跑或加 `&` 后台。

**优雅停止**:

```bash
kill -TERM $(cat ~/.ccteam/ccteam.pid)   # 或 Ctrl+C 在前台 terminal
```

行为契约:
- daemon 收 SIGTERM / SIGINT → 5 秒内进 graceful drain(`TASK_DRAIN_TIMEOUT = 5s` 上限)
- web port(默认 7331)立即释放
- pidfile `~/.ccteam/ccteam.pid` 自动 unlink
- **tmux 长 session 不被 kill**(CLAUDE.md §三 "永不主动 kill 长 session" 红线)— bot 进程留着,等下次 `ccteam start` reattach

**不要用 `kill -9` / `SIGKILL`**。SIGKILL 跳过 graceful drain,会留孤儿 pidfile,可能伤进行中的 turn 状态。只有 graceful 卡死 5 秒以上才考虑 SIGKILL,且属于 bug — 请 issue。

**崩溃恢复(tmux reattach + lossless context resume)**:

升级 ccteam 或机器重启后,`ccteam start` 启动时探测每个已注册 bot 的 tmux session(`ccteam-chat-<slug>-<role>`):
- session 存在 + pane 内 `claude` 进程活 → **reattach**(不 spawn 新 claude,bot context 不丢)
- session 存在 + pane 死(被 OOM-killed / 用户手动 `tmux kill-pane`)→ kill stale tmux + spawn `claude --resume <name>`,**Anthropic 官方 CLI 直接 reload 上次 session 的 full API-level context**(tool-use 中间结果、推理链、cache 状态全在)── 模型脑子里还有上次的东西,不只是 last-N turns 字面回放
- session 不存在(首次启 bot)→ 起新 session,带 deterministic `--name`,以备未来 `--resume`
- `--resume` 失败(session jsonl 不存在 / corrupt / 用户清过 `~/.claude/projects/`)→ 退到 brand-new session + 显式 emit `chat_session_reset` event,bot 在 IM 端会"告诉你它忘了"(visible degraded,不冒充 resume)

也就是:**升级 ccteam 不会丢 bot 对话 context;dead pane recreate 也尽力 lossless 还原**。bot 仍在 IM 端 responsive,不需要人工 `tmux kill-session` 后再 `ccteam-creator` 重起。

实际验证:发 `刚才那个 X 怎么样?` 风格 follow-up,bot reply 若能直接引用早 turn 内容(不是 "对不起,能否再讲一下 X")= 真 lossless;若 reply 显示 `chat_session_reset` 提示 = fallback 走了,context 已重置。

---

## §7 接下来去哪

| 想干什么 | 读这个 |
|---|---|
| 5 分钟跑通第一个 bot | [quickstart.md](quickstart.md) |
| 抄一份现成 use case | [recipes.md](recipes.md) |
| 改默认 persona / 加 MCP 工具 / 自定义 workflow | [advanced/customize-workflow.md](advanced/customize-workflow.md) |
| 装 Codex 让两个 LLM 互审 | [advanced/multi-llm-codex.md](advanced/multi-llm-codex.md) |
| 出错查不到 | [troubleshooting.md](troubleshooting.md) |
| 看代码 / 改 ccteam | [tech-design.md](tech-design.md) + [interfaces.md](interfaces.md) |
