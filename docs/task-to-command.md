---
audience: users
---

# Task → Command 决策树

> 一屏看完。你想做的事 → 直接对应一个 ccteam 命令,不必先理解 mode / preset / orchestration 概念。
>
> 不确定?最后一行总是对的:用自然语言问 `/ccteam`,它替你路由。

---

## 决策树(单屏)

```
你想做的事                                  → 用这个
──────────────────────────────────────────────────────────────────
摸底新代码库 / 仓库 audit                    /ccteam-scan
开发 / 修 bug / 重构(全程盯着干)            /ccteam-team "<task>"
review PR / 第二意见 / 对答案                /ccteam-advise "<PR 或 path>"
做个 IM 私聊助理(长期 24/7 在线)            /ccteam-creator "做个 X 助理"
做个团队 IM 圆桌(多 bot 互动)               /ccteam-creator "群里几个 bot"
夜里跑长任务(hands-off / 关电脑)            /ccteam-creator "<task>,睡前跑"
看 / 暂停 / 恢复 / 看花费                    /ccteam-control list / pause / cost
配 / 改 IM token(TG / Slack / Discord)      /ccteam-im-setup
不确定?用自然语言问                          /ccteam "<NL 描述>"
```

---

## 每条详解(用一行场景 + 一个最常见例子)

### `/ccteam-scan` —— 摸底新代码库

**啥时用**:刚 clone 一个仓库,想 30 秒知道:语言 / 框架 / 入口 / 主要 TODO / 有没有 CLAUDE.md。

```
cd path/to/repo
claude
/ccteam-scan
```

**输出**:三段报告(language/framework/entry · TODO hotspots · CLAUDE.md status)+ 建议下一步该跑哪个 ccteam 命令。

---

### `/ccteam-team "<task>"` —— 开发 / 修 bug / 重构

**啥时用**:你有个 1-3 小时能干完的工程任务,想要几个 agent 并行帮你干,**全程盯着** = 当前 Claude session 看实时通信。

```
/ccteam-team 3 "fix all TypeScript errors in src/"
/ccteam-team 5:reviewer "review the new auth design"
```

`<N>` 是 teammate 数(可省,默认 3)。`<task>` 是自然语言任务描述。

---

### `/ccteam-advise "<PR 或 path>"` —— 第二意见 / 对答案

**啥时用**:你拿不准一段代码 / 设计 / PR 是不是对的,想要 Claude + Codex 两个 LLM 并行各给一份分析,你看 diff 拿主意。

```
/ccteam-advise "PR #1234"
/ccteam-advise "src/auth/sso.rs 这个设计哪里有问题"
```

**前置条件**:装了 Codex(否则只有 Claude 单视角,失去 "双 LLM" 价值)。

---

### `/ccteam-creator "做个 X 助理"` —— IM 私聊助理(长跑)

**啥时用**:你想要一个 **手机 IM 里随时找它说话** 的私人 AI 助理。它跑在你电脑上(读文件 / 跑命令 / 推送),手机只是入口。

```
/ccteam-im-setup                                # 第一次,绑 TG bot token
/ccteam-creator "做个 TG 私聊助理:管邮件 + 早 7 点推 GitHub PR 摘要"
```

向导问 2-3 个问题(persona / 24h cost cap)后起 daemon,关电脑也继续跑。详 [quickstart.md](quickstart.md)。

---

### `/ccteam-creator "群里几个 bot"` —— IM 圆桌(多 bot)

**啥时用**:你想要个 IM 群,2-5 个 bot 各持职责(架构师 + 评审 + 文档 + 测试),你 @ 一个 bot 开局,bot 之间 @ 对方协作,群里给你共识。

```
/ccteam-creator "做个 TG 多 bot 团队:架构师 + 评审员 + 写手,讨论我贴的设计文档"
```

Creator 自动起群 / 配多 bot persona / 配 @ 路由 / 限 3 层 @ 链防 ping-pong。

---

### `/ccteam-creator "<task>,睡前跑"` —— 夜里长任务

**啥时用**:任务需长跑几小时到几天(test-fix-test 循环 / 全栈搭 todo app / 跨周做依赖升级),你 hands-off,关电脑去睡。

```
/ccteam-creator "夜里给我跑 qa-loop:测试失败自动 fix,每 fix 一轮 commit,直到 24h 或 $5 cap"
```

撞 cost cap 自动停,撞失败 N 次自动 TG 推送叫醒你。早上看摘要。

---

### `/ccteam-control list / pause / cost` —— 管已跑的 workflow

**啥时用**:你已经起了一些 workflow / bot,想看状态 / 暂停 / 恢复 / 查 cost / 改 persona / 加工具。

```
/ccteam-control list                    # 看所有在跑的 workflow
/ccteam-control pause helper-bot        # 暂停某个
/ccteam-control resume helper-bot
/ccteam-control show-cost               # 今日所有 workflow 花费
/ccteam-control change-persona helper-bot "改成英文 + 更幽默"
/ccteam-control add-tool helper-bot "WebFetch"
```

或在 IM 群里 `@ccteam pause helper-bot` / `@ccteam cost today` / `@ccteam stop everything`。

---

### `/ccteam-im-setup` —— 配 / 改 IM token

**啥时用**:第一次绑 TG(/Slack/Discord)bot,或换 token / 加新平台。

```
/ccteam-im-setup
```

向导自动开浏览器到 BotFather → 拿 token → 抓 chat_id → 起 daemon。详 [quickstart.md](quickstart.md) §Step 2。

> Slack / Discord onboarding 在 V0.7 ship(V0.6.x 仅 Telegram 端到端走通)。

---

### `/ccteam "<NL 描述>"` —— 不确定?自然语言问

**啥时用**:不知道该用上面哪个 sub-skill,或想一句话搞定。

```
/ccteam 帮我扫一下这个仓库            # 路由到 /ccteam-scan
/ccteam 修一下 TS 报错                # 路由到 /ccteam-team
/ccteam 我想要个 TG bot 帮我管邮件    # 路由到 /ccteam-creator
/ccteam 这个 PR 有问题么              # 路由到 /ccteam-advise
```

`/ccteam` 是**总入口 NL dispatcher**,它分析你的话路由到上面任一 sub-skill。万能,但比直接调 sub-skill 多 1-2 秒推理。**懒得记 sub-skill 名就用它**。

---

## 接下来读什么

- 跑通第一个 bot(5 分钟)→ [quickstart.md](quickstart.md)
- 全部 5 种用法详细手册 → [user-manual.md](user-manual.md)
- 抄一份现成 use case → [recipes.md](recipes.md)
- 想理解架构(mode / preset / orchestration pattern)→ [orchestration-patterns.md](orchestration-patterns.md)(面向 contributor)
- 出错 → [troubleshooting.md](troubleshooting.md)
