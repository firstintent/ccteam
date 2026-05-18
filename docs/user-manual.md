# ccteam User Manual

> ccteam 让你**用一句中文/英文召唤一个 AI 团队**,跑在你电脑上、接进你的 IM。本手册带你跑通 5 种典型场景,5 分钟见效。
>
> **零 yaml,零 CLI 命令记忆,零术语**。你只需要会用 Claude Code session 输入 slash 命令 + 自然语言。

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

## §2 5 种用法(preset)

每种用法 = "一个 ccteam 团队的预设配方"。你只挑场景,不挑实现细节。

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
- 4-5 个 bot 各持 persona
- 配 bot-to-bot @ 路由
- 限 3 层 @ 链路(防 ping-pong 循环)

**例 session**(在 TG 群):

```
You:        @arch_bot 看下我贴的 API 设计 [paste...]
arch_bot:   @reviewer_bot 你怎么看 v3 endpoint 设计?
reviewer:   @writer_bot 我建议改回 v2,文档需要 ...
writer_bot: 已起草文档,贴 PR 链接 → #1240
            @you 共识达成,请你拍板
```

**Cost 估**:1 次 30 分钟 round-table ≈ $0.3-1。

---

## §3 怎么跟 ccteam 对话(三种入口详解)

### §3.1 在 Claude session 内

总入口 `/ccteam <NL>` 是万能的 — 发啥都行,它路由到对应 sub-skill。

直接跳过 router 也行,5 个 sub-slash:

| Slash | 干啥 |
|---|---|
| `/ccteam-team <N> "<task>"` | 起临时 team(Team Sprint)|
| `/ccteam-creator "<NL>"` | 起新 workflow / 改现有 workflow |
| `/ccteam-control <subcmd>` | 管已有 workflow(暂停 / 恢复 / 查 cost / 改 persona)|
| `/ccteam-im-setup` | 一次性绑 IM token(TG / Slack / Discord)|
| `/ccteam-advise "<hard question>"` | Claude + Codex 并行二答案(仅装了 Codex 才出来)|

### §3.2 在 IM 端

**私聊你的 bot**:直接说话,bot 走 Pocket Assistant 模式。

**群里跟 ccteam 总管对话**:发 `@ccteam <NL>`(NL admin),它走 meta-agent 路由:

```
@ccteam pause helper-bot           # 暂停某个 bot
@ccteam resume helper-bot          # 恢复
@ccteam list bots                  # 列所有跑着的 bot
@ccteam cost today                 # 今日 cost
@ccteam stop everything            # 紧急停所有
```

### §3.3 Web 仪表板

浏览器开 `http://localhost:7331`,看:
- 所有 workflow 实时状态
- 每个 bot 的对话历史
- 24h cost 趋势图
- 失败 / 告警列表

Web **只看不操作**。所有控制走 Claude session slash 或 IM。

---

## §4 Cost 透明

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

## §5 从 V0.5 升级到 V0.6 — 0 用户操作

V0.6 升级**完全无感**:

- 你的 V0.5 项目配置继续跑 — 兼容,**0 文件修改**。
- MCP 工具名字 `mcp__ccteam__*` **全保留** — V0.5 muscle memory 0 破坏。
- V0.5 的 `/ccteam-team` / `/ccteam-control` / `/ccteam-creator` slash 行为兼容。
- **新加**:`/ccteam` 总入口、`/ccteam-im-setup`(绑 IM)、`/ccteam-advise`(双 LLM 投票)。

**唯一新选**:想试 Pocket Assistant / IM Squad?跑一次 `/ccteam-im-setup` 绑 TG token 就行。

---

## §6 接下来去哪

| 想干什么 | 读这个 |
|---|---|
| 5 分钟跑通第一个 bot | [quickstart.md](quickstart.md) |
| 抄一份现成 use case | [recipes.md](recipes.md) |
| 改默认 persona / 加 MCP 工具 / 自定义 workflow | [advanced/customize-workflow.md](advanced/customize-workflow.md) |
| 装 Codex 让两个 LLM 互审 | [advanced/multi-llm-codex.md](advanced/multi-llm-codex.md) |
| 出错查不到 | [troubleshooting.md](troubleshooting.md) |
| 看代码 / 改 ccteam | [architecture/](architecture/) |
