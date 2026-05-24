# Quickstart

> **第一节先看决策树** ── 你想做的事 → 直接对应一个命令,不必先理解架构。
>
> 第二节起是 "我要做 IM bot" 这条路的 5 分钟走通流程(从 0 到 **你手机 TG 收到 bot 第一条回话**)。其他路径详见 [task-to-command.md](task-to-command.md) + [user-manual.md](user-manual.md)。
>
> 卡住任何一步?跳到 [troubleshooting.md](troubleshooting.md) 搜对应章节。

---

## §1 我该用哪个命令?(决策树)

```
你想做的事                                  → 用这个
──────────────────────────────────────────────────────────────────
摸底新代码库(60s 零依赖,首选)              /ccteam-scan --quick     ↓ §1.5 详跑
仓库 navigability 体检 / 大型 monorepo audit  /ccteam-scan
开发 / 修 bug / 重构(全程盯着干)            /ccteam-team "<task>"
跨 vendor 第二意见(Claude + Codex)          /ccteam-advise "<问题>"   ↓ §4 详跑
做个 IM 私聊助理(长期 24/7 在线)            /ccteam-creator "做个 X 助理"  ↓ §2 详跑通
做个团队 IM 圆桌(多 bot 互动)               /ccteam-creator "群里几个 bot"
夜里跑长任务(hands-off / 关电脑)            /ccteam-creator "<task>,睡前跑"
程序化起 bot / 看历史 / 重置 session         mcp__ccteam__chat_*      ↓ §5 详跑
看 / 暂停 / 恢复 / 看花费                    /ccteam-control list / pause / cost
配 / 改 IM token(TG / Slack / Discord)      /ccteam-im-setup
验证安装 / 验 Codex auto-critic              ccteam doctor [--check-codex-auto-critic]
不确定?用自然语言问                          /ccteam "<NL 描述>"
```

每条详解 + 例子见 [task-to-command.md](task-to-command.md)。

---

## §1.5 60 秒上手:`/ccteam-scan --quick`(零依赖,首选)

第一次摸 ccteam,如果你不打算立刻起 bot,**最低门槛的体验**就是 `--quick` 扫一个本地仓库:无需 IM token、无需 daemon、无需 Codex,只调一次 Sonnet,1 分钟内给你三段报告(主语言/框架/入口 · TODO 热点 · `CLAUDE.md` / `AGENTS.md` 状态)+ 一句建议你下一步该跑哪个 ccteam 命令。

```bash
cd path/to/any-repo
claude
```

session 里:

```
/ccteam-scan --quick
```

输出大致这样:

```
ccteam-scan (quick) → <repo>/.ccteam/codebase-scan.md

[1/3] Language / framework / entry
  Rust workspace · tokio + axum · entry crates/ccteam-cli/src/main.rs

[2/3] TODO / FIXME hotspots
  crates/ccteam-imd/src/outbound.rs: 4 TODO
  docs/troubleshooting.md: 2 FIXME

[3/3] CLAUDE.md / README / AGENTS.md status
  CLAUDE.md ✓ (180 lines, current) · README.md ✓ (English) · AGENTS.md → CLAUDE.md symlink ✓
  建议下一步:用 /ccteam-team 处理 TODO,或 /ccteam-creator 起一个长跑 bot 监控这堆 hotspot
```

24h 内复跑会直接显示上次报告(`--force` 强制重扫)。**不**写 git / 不开 daemon / 不动 `~/.ccteam/`。

---

## §2 走通 "IM 私聊助理" 这条路(5 分钟)

下面是 ccteam **最有代表性的** 一条路 —— 起一个 Telegram 私聊 bot 当你的 AI 助理。

**你需要**:

- Claude Code(`claude` CLI),装好登好。没装看 [code.claude.com/docs/install](https://code.claude.com/docs/install)。
- Telegram 账号 + 手机装 Telegram app。macOS / Linux 主机(Windows 走 WSL2)。

---

## Step 1 — 装 ccteam plugin(30 秒)

任意 terminal 起 Claude session:

```bash
$ claude
```

在 session 里输入:

```
/plugin install ccteam
```

预期输出:

```
✓ Installed ccteam
  • slash commands registered: /ccteam, /ccteam-team, /ccteam-creator, /ccteam-control, /ccteam-im-setup, /ccteam-scan, /ccteam-advise
  • mcp__ccteam__* tools available (workflow_* / chat_* / advise_* / admin_* / screenshot_*)
```

跑 `ccteam doctor` 验装(claude CLI / MCP / tmux / pidfile 路径都查一遍);加 `--check-codex-auto-critic` 验证 Codex 二审是否能开。

→ 卡了?见 [troubleshooting.md](troubleshooting.md) "plugin install 失败"。

---

## Step 2 — 绑定你的 TG bot(2 分钟)

```
/ccteam-im-setup
```

向导引导你:浏览器自动开 [@BotFather](https://t.me/BotFather) → 发 `/newbot` → 起名(如 `my_helper_bot`)→ 复制 token 粘回 Claude → 向导在 TG 监听 30 秒,你随便发条 `hi`,它自动抓 `chat_id`。

预期结束:

```
✓ Telegram bot 已绑定:@my_helper_bot
✓ chat_id 抓到:123456789
✓ Daemon ccteam-imd 已起
现在可以跟 @my_helper_bot 私聊:https://t.me/my_helper_bot
```

→ 卡了?见 [troubleshooting.md](troubleshooting.md) "TG token 拿不到 / chat_id 抓不到"。

---

## Step 3 — 让 ccteam 做一个 Pocket Assistant(1 分钟)

```
/ccteam-creator "做个 TG 私聊助理 bot,帮我每天读邮件 + 看 GitHub PR + 早 7 点发摘要"
```

ccteam-creator 自动判定是 **Pocket Assistant** preset,问 2-3 个问题:

```
Creator: persona 模板?推荐 "Personal Workflow Assistant"(中文)
         其他选项:Technical Helper / Writing Coach / Translator / Study Buddy
You:     用 Personal Workflow Assistant

Creator: 24h cost cap?建议 $2 起步(用得多再调)
You:     $2

Creator: 计划:Preset = Pocket Assistant · Persona = Personal Workflow Assistant(中文)
         · IM = TG @my_helper_bot(私聊)· cap = $2/24h。回 "go" 起。
You:     go
```

预期:

```
✓ Workflow created: helper-bot
✓ Bot @my_helper_bot is now live
✓ Web dashboard: http://localhost:7331/p/helper-bot
```

---

## Step 4 — 去 TG 私聊它(30 秒)

打开 Telegram,搜 `@my_helper_bot`(你 Step 2 起的名字),点 Start。

发条消息:

```
你好,你能帮我做啥?
```

预期:几秒内收到 bot 自我介绍 + 列能力清单。

🎉 **完成。你的第一个 AI 助理已经在 TG 里跑起来了。**

---

## Step 5 — 跨设备试试(可选)

笔记本继续开着,掏手机走开,继续在 TG 跟 bot 聊。bot 仍然回。bot 跑在**你电脑上**(能动文件 / 跑命令),手机只是入口 — 这是跟 ChatGPT app 拉开差异的关键。

---

## §4 跨 vendor 第二意见:`/ccteam-advise`(可选)

碰到拿不准的设计 / PR / 代码片段,想要 Claude + Codex 两边各给一个答案再自己拍板:

```
/ccteam-advise vote "这段 SSO 设计的 token-refresh 路径有没有竞态?"
```

ccteam 并行调用 Claude + Codex 两个 advisor,合成 verdict(majority / unanimous / split),附每个 vendor 的原始答复 + 估算 cost。`parallel` 模式不合成,直接给两份 raw answer 让你自己读:

```
/ccteam-advise parallel "重构这段 auth 中间件有几种方式?"
```

**前置**:Codex 装好(`codex --version`)+ `codex login` 跑过。没装 ccteam 会 graceful 降级跑单 Claude advisor,verdict prose 说 "Codex unavailable: <reason>",**不报错**。两个 vendor 各自走 24h cost cap(`<ccteam_root>/cost-budget.json`,撞顶自动跳过该 vendor)。

底层 MCP 工具:`mcp__ccteam__advise_vote` / `mcp__ccteam__advise_parallel`。

---

## §5 程序化起 bot:chat MCP 工具(可选)

`/ccteam-creator` 是新手 onboarding 通路;**已经懂 ccteam** 之后,从 Claude session 内直接调 MCP 工具更快:

```
mcp__ccteam__chat_register_bot { "slug": "helper", "role": "main", "vendor": "claude", "im_chat_id": "123456789" }
mcp__ccteam__chat_list_bots {}
mcp__ccteam__chat_send_input { "slug": "helper", "role": "main", "text": "summarize today's PRs" }
mcp__ccteam__chat_history { "slug": "helper", "role": "main", "limit": 10 }
mcp__ccteam__chat_reset { "slug": "helper", "role": "main" }
mcp__ccteam__chat_unregister_bot { "slug": "helper", "role": "main" }
```

6 个工具组成完整生命周期:register → list / send_input / history → reset → unregister。`chat_reset` 归档 `turns.jsonl` 到 `archive/turns-<ts>.jsonl` + 清 outbound cursor + 清 transcript cursor(daemon 内存与磁盘同步重置)。`vendor` 字段严格小写枚举:`claude` 或 `codex`。

适合场景:CI 编排起多个 bot 做 batch / 给已跑 bot 推一个程序化指令 / 抓 bot 历史做 audit。**仍然守红线**:per-bot tmux session、`progress.jsonl` 是 state SoT、不写 prompt。

---

## 接下来读什么

- 想跑别的命令(不是 IM bot)?→ [task-to-command.md](task-to-command.md)(决策树详解)
- 想跑别的 use case?→ [recipes.md](recipes.md)(代码审查 bot / 翻译 bot / 日报助手等 10 个现成模板)
- 想了解全部 5 种用法?→ [user-manual.md](user-manual.md)
- 想给 bot 换 persona / 加能力?→ [user-manual.md](user-manual.md) §2.4 Pocket Assistant
- 装 Codex 让两个 LLM 互审?→ [advanced/multi-llm-codex.md](advanced/multi-llm-codex.md)
- 出错?→ [troubleshooting.md](troubleshooting.md)
