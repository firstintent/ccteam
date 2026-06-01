# Recipes — ready-to-go 配方

> 这是一份"照菜单点"的快速起手册。每个配方告诉你 **一句话怎么起、起出来是啥、每天大约花多少钱**。
>
> 所有起步命令都在 Claude session 里跑 — 一句 `/ccteam <自然语言>`,剩下的 `ccteam-creator` 帮你搞定:agent 行为、IM 连接、Codex critic、预算上限,统统不用你写配置。
>
> **前置一次性**:
> 1. 装好 ccteam:`/plugin install ccteam` + `ccteam doctor`
> 2. 想要 IM bot?先跑一次 `/ccteam-im-setup` 绑定 TG / Slack token(2-3 分钟,后续所有配方都自动复用这个 token)
> 3. 想要 Codex critic?在终端跑过 `codex auth` 就行(可选 — 没装 ccteam 自动跳过 critic 那一档,只用 Claude)
>
> **看完想改?** 每个配方最后有 [进阶定制](advanced/customize-workflow.md) 链接。改名字、换 persona、调 IM 平台、加更多 bot — 都在那本手册。

---

## 配方 1 — 个人代码助手(TG 私聊)

### 场景

> "我想在 Telegram 私聊里随时问个技术问题,通勤路上、卧床都能用。最好支持中文。"

### 适合谁

独立开发者 / 学生 / 任何想"把 Claude 装进口袋"的人。

### 起步

在 Claude session 里:

```
/ccteam 做个 Telegram 私聊技术助手,中文回答
```

ccteam-creator 会问你 2-3 个 follow-up(用什么名字 / 回答风格偏正式还是随意),然后回:

```
PROJECT PLAN
============
Type:           Pocket Assistant(TG 私聊)
Bot:            @code_helper(persona: 技术助手 中文版)
Codex critic:   自动启用(检测到 codex auth)
预计 cost/day:  ~$0.4(Claude 主对话 + Codex 偶尔挑刺)

Reply 'go' 启动,或改 persona / 平台 / 语言。
```

回 `go`,30 秒后 ccteam 告诉你"在 TG 找 @code_helper 私聊试试"。

### 生成什么(你不用编辑)

- 一个 helpful-bot agent,中文技术助手 persona
- TG bot 注册(走你已经绑定的 token)
- Claude Opus 作主回答 + Codex 自动当 critic(检测到才启用,没装就纯 Claude)
- 每日预算 $1 上限,触顶 ccteam 自己 escalate 告诉你

### 预期 cost

- 轻度日用(20-30 条问答 / 天):**$0.3-0.6 / day**
- 重度日用(100+ 条 / 天):**$1-2 / day**

### 想改?

[进阶定制 — 换 persona / 切英文 / 加额外 critic](advanced/customize-workflow.md#pocket-assistant)

---

## 配方 2 — 写作 critic 群

### 场景

> "我写文章 / 邮件 / 文案的时候想丢进群里,让两个 AI 帮我看 — 一个挑文笔,一个挑结构。"

### 适合谁

写作者 / 内容创作者 / 营销 / 任何需要"先放一边晾一下再回头看"的写作场景。

### 起步

先在 TG 创个群,把你的两个 bot 拉进去。然后在 Claude session:

```
/ccteam 做个 TG 群组写作 critic 双 bot:一个挑文笔风格,一个挑结构逻辑
```

ccteam-creator 推断:这是 IM 群组多 bot 互动场景。生成:

```
PROJECT PLAN
============
Type:           IM Squad(TG 群组双 bot)
Bots:           @style_critic(文笔 / 风格 / 用词)
                @structure_critic(结构 / 论证 / 逻辑)
互动模式:        你 @ 它们,它们会 @ 对方协同;最多 3 轮自动止住
Codex critic:   不启用(都是 critic 角色,不需要 critic 的 critic)
预计 cost/day:  ~$0.8(看文章长度;一篇千字稿 ~$0.05)

Reply 'go' 启动。
```

### 用法

```
你 → 群里发文章草稿
你 → @style_critic 帮我看看文笔
@style_critic → 5 处建议 + @structure_critic 这段结构你怎么看
@structure_critic → 回应 + 给出修订建议
```

### 生成什么

- 两个 agent,style-critic + structure-critic,各自 persona 已预填
- TG 群组 webhook
- bot 互相 @ 链路:最多 3 轮自动停(防止两个 bot 无限互相回应)
- 没有 Codex critic 自动接入(critic 角色不需要 second-opinion critic)

### 预期 cost

- 每天审 1-3 篇短稿:**$0.3-1 / day**
- 长稿(5k+ 字):每篇 **$0.1-0.3**

### 想改?

[进阶定制 — 换成单 bot / 加第三个 bot / 改 prompt 风格](advanced/customize-workflow.md#im-squad)

---

## 配方 3 — 夜跑 qa-loop

### 场景

> "我下班丢一个 bug fix 任务,睡一觉醒来看到结果。要 test 跑通才停,撞到 3 次失败必须告诉我。"

### 适合谁

任何"白天 review、晚上跑"的人。需要质量 gate 的长任务都吃这套。

> 注:这条配方的 `ccteam-creator` 能生成 overnight builder 的 workflow,但**无人值守的自动接力执行(test → fix → release 循环自己往下跑)依赖推后的 `ccteam-flow` 编排层**。当前 gateway daemon 只做 IM⇄session 路由、不 tick。

### 起步

```
/ccteam 做个夜跑 qa-loop:test → fix → release,失败 3 次必须叫醒我
```

ccteam-creator 推断:长跑、用户不在场、有质量 gate → Overnight Builder。

```
PROJECT PLAN
============
Type:           Overnight Builder(后台长跑)
Pipeline:       test 跑测试 → fix 修复失败 → release 打 tag
Codex critic:   自动启用(review 角色)— 每次 fix 后让 Codex 二审
失败上限:        3 次;撞到 ccteam 停手 + 发 TG 私信通知你
预计 cost/day:  ~$2-5(看任务复杂度;一晚 8 小时 ~$3)

Reply 'go' 启动。
```

### 用法

```
你 → 把任务说清楚,go
ccteam → 起 daemon,你关电脑去睡
夜里 → ccteam 跑 test,fail 就 fix,通过就 release tag
撞 3 次失败 → 停手 + TG @你 "卡在 X 模块,需要人审一下"
通过 → TG @你 "已 release,见 PR https://..."
```

### 生成什么

- test-runner agent + fix-er agent + release-er agent
- 任务文件接力:test 输出 → fix 读 → 修完触发 release
- Codex critic 自动接入 fix 一关(走"自动当 reviewer"路径)
- IM 通知:成功 / 失败都通过你绑定的 TG 推一条

### 预期 cost

- 简单 task(改 5-10 行,跑 1-2 轮)**$0.5-1.5 / 晚**
- 中等 task(几个模块,3-5 轮)**$2-5 / 晚**
- 大 task(整 feature,~10 轮)**$5-10 / 晚**
- 撞预算上限 ccteam 自动停 — 不会跑出天价账单

### 想改?

[进阶定制 — 调失败上限 / 换 critic / 加 security-review 一关](advanced/customize-workflow.md#overnight-builder)

---

## 配方 4 — GitHub issue triage 助理

### 场景

> "我有个开源项目,issue 越积越多。想要个助手帮我分类、贴标签、给优先级建议,有空我再回复人类。"

### 适合谁

开源维护者 / 内部工具 owner / 任何需要"先归类再处理"的 issue 接收方。

### 起步

前置:`gh auth login` 装好 GitHub CLI;或者装 GitHub MCP server(`/plugin install github`)。

```
/ccteam 做个 GitHub issue triage 助理,接到新 issue 自动分类贴标签,有困难的留给我
```

ccteam-creator:

```
PROJECT PLAN
============
Type:           Pocket Assistant + GitHub MCP
Bot:            @triage_helper(persona: 项目监工)
接入:            GitHub MCP(detected: ✓)
触发:            GitHub webhook → 你电脑 ngrok → ccteam → bot 处理
分类规则:        bug / feature / question / docs / dup
拿不准时:        TG 私信你,等你裁决
预计 cost/day:  ~$0.2-1(取决于 issue 流量)

Reply 'go' 启动。
```

### 用法

- 新 issue 进 GitHub → bot 自动读 issue body → 贴 1-3 个 label + 回一句中性的"thanks for reporting"
- 拿不准的(比如 "这是 bug 还是 feature?")→ bot TG 私信你 + 选项 a/b/c,你回一个数字它就给 issue 操作下去

> 注:webhook 入站端点(`POST /webhook/{project}/{token}`)当前可用,会把 payload 落 `<project>/.ccteam/webhooks/`;但"新 issue 自动触发 bot 处理"依赖推后的 ccteam-flow 编排层(agent `trigger: watch:.ccteam/webhooks/`),当前 gateway daemon 不 watch artifact。过渡期可在 IM 里把 issue 内容手动发给 `@triage_helper` 处理。

### 生成什么

- triage-helper agent 接 GitHub MCP 工具
- TG bot 走你绑定的 token(私信模式)
- 内置分类 prompt 模板;你可以在 [进阶定制](advanced/customize-workflow.md#github-triage) 里改

### 预期 cost

- 小项目(5-10 issue / 周):**~$0.5 / 周**
- 中项目(50 issue / 周):**~$3 / 周**
- 撞预算上限 ccteam 自动停

### 想改?

[进阶定制 — 接 Linear / Jira / 自定义 label 规则](advanced/customize-workflow.md#github-triage)

---

## 配方 5 — 学习辅导 bot

### 场景

> "我在学一门新东西(算法 / 第二语言 / 某框架),想要一个能耐心解释、出题、纠错的 bot 陪我练。"

### 适合谁

学生 / 自学者 / 任何"每天 30 分钟刷一下"场景。

### 起步

```
/ccteam 做个中文学习辅导 bot,我每天练 30 分钟算法题
```

ccteam-creator 推断:Pocket Assistant + 学习辅导 persona + 中文。

```
PROJECT PLAN
============
Type:           Pocket Assistant(TG 私聊)
Bot:            @algo_tutor(persona: 学习辅导 中文版)
风格:            苏格拉底式提问 + 提示而不直接给答案 + 复盘
Codex critic:   不启用(教学场景不需要 second-opinion)
预计 cost/day:  ~$0.3-0.8(30 分钟 / 天)

Reply 'go' 启动。
```

### 用法

```
你 → "今天讲讲二叉搜索"
bot → 出 3 题,从 easy 到 hard,你做完它逐题点评 + 标重点 + 一周末出复盘
```

### 生成什么

- algo-tutor agent,内置学习辅导 persona(中文版)
- 不接 critic(教学不需要)
- 不接 Codex(节省预算)

### 预期 cost

- 30 分钟 / 天:**~$0.5 / day**
- 1 小时 / 天:**~$1 / day**

### 想改?

[进阶定制 — 换学习领域 / 切英文 / 改风格](advanced/customize-workflow.md#tutor)

---

## 配方 6 — 翻译团队

### 场景

> "我有一批英文文档要翻成中文,想要 3 个 bot 接力:翻译、母语审、风格统一。"

### 适合谁

技术写作者 / 本地化 / 任何"先初翻再多轮校对"的翻译场景。

### 起步

```
/ccteam 做个 TG 群组翻译团队三 bot:translator 初翻、reviewer 母语审、style 风格统一
```

ccteam-creator:

```
PROJECT PLAN
============
Type:           IM Squad(TG 群组三 bot 接力)
Bots:           @translator(persona: 翻译 中文)
                @native_reviewer(persona: 翻译 中文 — 母语审视角)
                @style_critic(persona: 写作助手 中文 — 风格统一)
流程:            你贴英文 → translator 初翻 → reviewer 审 → style 统一 → 输出 final
Codex critic:   不启用
预计 cost/day:  千字 ~$0.05;每天 5 千字 ~$0.25

Reply 'go' 启动。
```

### 用法

```
你 → 群里贴英文段落
@translator → 初翻
@native_reviewer → 改 5-10 处不顺口
@style_critic → 统一用词术语
final 输出 → 你点 reaction 表示接受 / 让某个 bot 重做一段
```

### 生成什么

- 三 bot,各自 persona 中文预填
- 接力顺序:translator → reviewer → style;每 bot 处理完自动 @ 下一棒
- 不接 Codex(翻译质量靠多 bot 接力,不需 second-opinion vendor)

### 预期 cost

- 5 千字 / 天:**~$0.25 / day**
- 5 万字 / 天:**~$2.5 / day**

### 想改?

[进阶定制 — 反向翻译 / 加 SEO 关键词 review / 多语种](advanced/customize-workflow.md#translation-team)

---

## 配方 7 — 代码 review 集(with Codex)

### 场景

> "我要写一个 feature,过程中希望 Claude 做主力,Codex 当 critic 经常二审。一句话起一个临时 5 人小组。"

### 适合谁

想从一开始就用 Codex 集成的早期采用者;严肃多模块改动场景。

> 注:`/ccteam-team` 能生成 Orchestrator-Worker 的 team 配置,但**多 agent 自动分工 + 自动 join 结果的无人值守编排依赖推后的 `ccteam-flow` 编排层**。当前过渡期借 Claude session 内部的 `Task(subagent_type=...)` 直接派工(lead 在你当前 session 里拆任务、起 teammate、收 `SendMessage` 汇报),由你在 session 里逐步驱动。

### 起步

```
/ccteam-team 5 "实现 feature X,要 Codex critic 严审"
```

或 `/ccteam` 总入口:

```
/ccteam 起 5 人临时小组实现 feature X,Codex 当 critic
```

ccteam-team(或 creator)推断:Team Sprint + Codex auto-critic。

```
TEAM PLAN
=========
Type:           Team Sprint(几小时冲一波)
Workers:        5 agents(executor × 3 + reviewer + critic)
Critic vendor:  Codex(检测到 codex auth)
策略:            Claude 干主力,每个 PR-size 改动 Codex 二审;失败 3 次升级你
预计 cost:      ~$2-8(看 feature 规模;一个普通 feature 约 4 小时 $5)

Reply 'go' 启动。
```

### 用法

```
go → ccteam 起 team,5 个 worker 各跑一块
你 → 在 Claude session 内 watch SendMessage 流,或 /ccteam-control show-progress
完成 → 各 worker SendMessage 报告 + critic verdict
```

### 生成什么

- 5 个 in-proc Task agent(`Task(subagent_type=...)`)
- critic worker `vendor: codex` 自动设置(用户不见 yaml)
- Codex 写 verdict 文件 `.ccteam/verdicts/<task>.json`,Claude 主 session 读后决策接受 / 改

### 预期 cost

- 短任务(1 小时,500 行改)**~$1-2**
- 中任务(4 小时,2k 行改)**~$5-8**
- 大任务(全天,10k 行)**~$15-25**

### 想改?

[进阶定制 — 切 critic vendor / 调 team size / 接更多 reviewer 角色](advanced/customize-workflow.md#team-sprint-with-codex)

---

## 配方 8 — 项目监工(混合)

### 场景

> "我有个长期项目,想要白天用 IM 私聊跟它聊进展,夜里它自己跑测试 + 修 bug,撞墙就喊我。"

### 适合谁

solo 创业者 / 业余维护项目 / 需要 "24/7 替身" 场景。

### 起步

```
/ccteam 做个项目监工:白天 TG 私聊汇报进展,夜里自动跑测试和修 bug,撞墙叫我
```

ccteam-creator 推断:**混合** — Overnight Builder 跑长任务 + Pocket Assistant 收发 IM。

> 注:白天的 TG 私聊汇报这一半是当前 IM gateway 路径,可直接用(`@project_lead` 路由到一个长跑 chat session)。夜里的**无人值守自动 test → fix → release 接力**依赖推后的 `ccteam-flow` 编排层;当前 daemon 不 tick。

```
PROJECT PLAN
============
Type:           Pocket Assistant + Overnight Builder(混合配方)
Bot:            @project_lead(persona: 项目监工 中文版)
白天角色:        TG 私聊回答进度问题、收新需求、整理 backlog
夜里角色:        跑 test → fix bug → 生成晨报
撞 3 次失败:     TG @你 + 暂停夜跑等你裁决
Codex critic:   自动启用(夜里 review 角色)
预计 cost/day:  ~$3-8(白天聊 + 夜里 8h 长跑)

Reply 'go' 启动。
```

### 用法

```
白天 9-18 点:
  你 → TG @project_lead "今天进度怎样?"
  bot → 列 backlog 状态 + 昨夜测试摘要 + 推荐今天 priority
  你 → "把 X 加到夜里任务"
  bot → 记入 backlog

夜里 22:00 - 早 8:00:
  bot 自动跑 test → fix → release 循环
  早晨给你 TG 推一条 "昨夜过 3 个 PR,1 个卡 X 模块求救"
```

### 生成什么

- project-lead agent(白天 IM 端);test/fix/release 三个夜跑 worker(后台端)
- 共享同一个项目目录 + 同一份运行状态
- Codex critic 接夜跑 review 路径
- IM 通知频道 = 你绑定的 TG bot

### 预期 cost

- 白天 30 分钟 IM + 夜里 8 小时长跑:**~$5 / day**
- 撞 budget 自动停 + 通知你

### 想改?

[进阶定制 — 调白天 / 夜里时段 / 改通知策略 / 加多人协作](advanced/customize-workflow.md#hybrid-project-lead)

---

## 配方 9 — 60s 摸底新开源项目(零 token,零依赖)

### 场景

> "我刚 clone 一个开源项目,想 60 秒知道它是干啥的 / TODO 集中在哪 / 文档齐不齐,再决定要不要深读。"

### 适合谁

任何刚 clone 仓库的人 / 评估一个候选依赖 / 给团队做 due-diligence 的 lead。**第一次接触 ccteam 也用这条** — 不开 daemon、不绑 IM、不需 Codex,纯单 Sonnet 调用看价值。

### 起步

```bash
cd path/to/repo
claude
```

session 里:

```
/ccteam-scan --quick
```

### 用法

约 60 秒后,报告写到 `<repo>/.ccteam/codebase-scan.md`,session 内直接渲染:

```
[1/3] Language / framework / entry
  Rust workspace · tokio + axum · entry crates/foo-cli/src/main.rs

[2/3] TODO / FIXME hotspots
  crates/foo-core/src/worker.rs: 7 TODO / 2 FIXME
  docs/architecture.md: 3 TODO

[3/3] CLAUDE.md / README / AGENTS.md status
  CLAUDE.md ✗ (建议 claude /init)· README.md ✓ (English) · AGENTS.md 缺
  建议下一步:跑 claude /init 写一份 CLAUDE.md;或 /ccteam-team 干掉 worker.rs 那 7 个 TODO
```

24h 内复跑直接显示上次报告(`--force` 强制重扫)。

### 预期 cost

每次 ≈ $0.01-0.03(单 Sonnet 调用,小中型仓库)。

---

## 配方 10 — Claude + Codex 双 vendor 二审一个设计决策

### 场景

> "我在拿不准的设计决策上想要 Claude 和 Codex 两边各给一份分析,看两边同不同意,再自己拍板。"

### 适合谁

任何"拿不准"场景:架构选型、PR 评审、棘手 bug 根因、性能优化路径选择。

### 起步

前置:`codex login` 跑过(可用 `ccteam doctor --check-codex-auto-critic` 一条命令验)。

```
/ccteam-advise vote "我们的 SSO token refresh 路径,放 redis 缓存还是走 DB 行锁?各自的故障模式有哪些?"
```

### 用法

ccteam 并行调 Claude + Codex 两个 advisor,合成一份 verdict:

```
VERDICT: split
Claude (Opus): 推荐 redis,理由 (1) 延迟低 (2) 失效自动... [完整答复 ~500 字]
Codex (gpt-5.2-codex): 推荐 DB 行锁,理由 (1) 强一致 (2) 不依赖独立组件...

差异点:
  - 一致性 vs 延迟 trade-off 选不同
  - Claude 假设 redis 高可用;Codex 假设 redis 是 SPOF

Cost: Claude $0.02 + Codex $0.03 = $0.05
```

`parallel` 模式不合成,直接给两份 raw answer 让你自己读:

```
/ccteam-advise parallel "重构这段 auth 中间件有几种方式?"
```

### 生成什么

无文件落盘(单次跑完即用)。两 vendor 各自走 24h cost cap(记账 `<ccteam_root>/cost-budget.json`,48h 自动 GC)。某 vendor 撞顶 → 静默跳过,verdict 标 `budget_exhausted`。

### 预期 cost

- 单次 vote ≈ **$0.01-0.05**(两 vendor 各跑一次)
- 单次 parallel 同上

### 想改?

[Codex 集成进阶](advanced/multi-llm-codex.md):自定义 Codex sandbox / reasoning effort / 切默认 vendor。

---

## 配方 11 — 程序化起一个长跑 IM bot(MCP 路径)

### 场景

> "我已经懂 ccteam,想从 Claude session 内一句 MCP call 起 / 改 / 重置一个 chat bot,不走 /ccteam-creator 向导。"

### 适合谁

CI 编排 / 批量起多个 bot / power user 想跳过向导 / 自动化测试 / 给已跑 bot 推程序化指令。

### 起步

前置:`ccteam-im-setup` 跑过(token 落 `~/.ccteam/im/credentials.json`)。session 里直接调 MCP:

```json
{ "name": "mcp__ccteam__chat_register_bot",
  "args": { "slug": "helper", "role": "main", "vendor": "claude", "im_chat_id": "123456789" } }
```

### 用法

```json
mcp__ccteam__chat_list_bots {}
→ { "bots": [ { "slug": "helper", "role": "main", "running": true, "heartbeat_age_secs": 7 } ] }

mcp__ccteam__chat_send_input { "slug": "helper", "role": "main", "text": "summarize today's PRs" }

mcp__ccteam__chat_history { "slug": "helper", "role": "main", "limit": 5 }
→ { "turns": [ {"turn_id":"...", "user":"...", "assistant":"..."} ] }

mcp__ccteam__chat_reset { "slug": "helper", "role": "main" }
→ { "ok": true, "archived": "archive/turns-1716527890123.jsonl" }

mcp__ccteam__chat_unregister_bot { "slug": "helper", "role": "main" }
```

### 生成什么

`<ccteam_root>/imd/registry/<slug>/<role>.json` + `<role>.heartbeat`(daemon 自维护)。`vendor` 字段严格小写枚举(`claude` / `codex`),3 层校验(schema enum / dispatch `to_lowercase()` / serde `rename_all`)。

`chat_reset` 归档 `turns.jsonl` 到 `archive/turns-<unix-ms>.jsonl` + 清 outbound cursor + 清 transcript cursor(daemon 内存 + 磁盘同步重置 — 不留 race)。

### 预期 cost

MCP 工具本身 0 cost(纯文件操作 + daemon coordination);bot 跑起来后按 turn 计费。

### 想改?

[进阶定制](advanced/customize-workflow.md):workflow.yaml 字段 / multi-bot per slug / cross-vendor squad。

---

## 配方 12 — 新项目零配置上手(ccteam-creator 自动探测)

### 场景

> "我刚 clone 了一个仓库(或者 `cargo new` / `pnpm init` 起的新项目),想直接 `/ccteam-creator` 起一个团队,**不必手改 yaml**就能跑。"

### 适合谁

刚装 ccteam 的新用户;或在不熟悉的项目里临时起 reviewer / squad;或单纯懒得手填 scope。

### 起步

```bash
cd path/to/your/repo
claude
```

session 里直接:

```
/ccteam-creator "做个后端 reviewer 团队"
```

ccteam-creator 先调 `ccteam probe-project` 探测:

```
Project probe:
  kind: Monorepo (cargo workspace)
  languages: Rust, TypeScript
  probable scope: crates/api-core/src, crates/api-cli/src, services/web/src

Generating workflow with scope pinned to those subtrees ...
```

随后 yaml 生成阶段把 probe 结果喂入 ── `scope:` 段已按项目结构填好,role.md 也按 Rust workspace 默认值(`cargo test --workspace` 风格)预填。

### 用法

probe 结果不满意?向导对话里直接说:

```
You: scope 改成只 crates/api-core/src,别管 cli 和 web
Creator: ✓ scope updated → [crates/api-core/src]
```

或单独跑探测看 ccteam 怎么"看"你的仓库:

```bash
ccteam probe-project --json
```

输出 `kind` / `languages` / `probable_scope` 三段 JSON,适合 CI 编排在起 workflow 前先抓项目类型路由到不同 preset。

### 生成什么

- workflow.yaml `scope:` 段非空,按 monorepo 子树或 src/ + tests/ 分发
- role.md 主语言(Rust → cargo / TS → npm / Python → pytest 风格)默认值
- DocsOnly 仓库(只 *.md)→ scope = docs/,跳过 build/test 工具
- ScriptsOnly 仓库(只 *.sh / *.py)→ scope = 顶层脚本目录

### 探测启发式

- `Cargo.toml workspace.members` + `package.json workspaces` + `go.work` → Monorepo
- 单 `Cargo.toml` / `package.json` / `pyproject.toml` → SingleRepo
- 只有 `.md` 无 source dir → DocsOnly
- 只有 `.sh` / `.py` script → ScriptsOnly

漏判时 user prompt override probe 结果,probe 只提供合理初值。

### 预期 cost

probe 本身 0 cost(纯文件扫描)。生成 yaml + 起 team 按对应 preset 计算。

### 想改?

[进阶定制 — preset 默认值 / scope 字段语义](advanced/customize-workflow.md#sensible-defaults)

---

## 配方 13 — 长跑 cost 监控(IM + doctor 对账)

### 场景

> "我有几个长跑 bot,想随时看花了多少钱;每周做一次对账,确认 ledger 没漏(防 spawn 路径绕开计费)。"

### 适合谁

任何长跑超过 1 周的 daemon / overnight builder / 多 bot 团队 owner。

### 起步

实时查 24h cost,IM 端:

```
@ccteam cost today
```

bot 回:

```
ccteam cost today
  rolling 24h cost: Claude $1.8240 + Codex $0.4120 = total $2.2360
  cap: $5.00/24h · remaining: $2.7640
  active bots: 3 (filter: none)
  full breakdown: `/ccteam-control show-cost`
```

撞 80% cap 自动加 `⚠️ approaching daily budget cap` 前缀。

Claude session 内同样命令:

```
/ccteam-control show-cost                       # 全 workflow 24h 汇总
/ccteam-control show-cost helper-bot --days 7   # 单 workflow 7 天
```

### 周末对账

每周末终端跑一次:

```bash
ccteam doctor --check-cost-orphan
```

健康输出:

```
[ok] claude: 168 agent_done vs 168 ledger rows
[ok] codex:  42 agent_done vs 42 ledger rows
verdict: OK — ledger reconciled with progress.jsonl over 24h.
```

`WARN per vendor` 不对账 = 某 spawn 路径漏写 ledger ── 通常是新加自定义 adapter 没接 ledger hook,或外挂 codex bash 调用绕过(critic 路径已统一走 ledger,这种 leak 应消失)。

### 配合 `ccteam doctor --verify-mcp` 做 CI gate

CI 跑一次:

```bash
ccteam doctor --verify-mcp --json | jq .verdict
# 期望:"PASS"
```

任何 STUB 注册 → exit 1,fail CI build。

### 预期 cost

监控命令本身 0 cost(纯读 ledger + progress.jsonl)。

### 想改?

[Multi-LLM 进阶](advanced/multi-llm-codex.md):per-vendor budget cap / unified cost rollup 内部架构。

---

## 看完想自己捣鼓?

每个配方都是 `ccteam-creator` 帮你生成的 `.ccteam/workflow.yaml` + `.claude/agents/*.md`。你 95% 时间不需要看这些文件 — 改东西在 Claude session 里一句话就行:

```
/ccteam-control add-agent translator-to-french
/ccteam-control change-budget 2.0
/ccteam-control switch-persona "更正式的口吻"
```

但如果你是 power user 想真的看 yaml 内部长什么样、各字段默认值是啥 → [advanced/presets-reference.md](advanced/presets-reference.md) 有完整 schema。

如果你想接的平台 / 流程没在上面 8 个配方里 → 直接 `/ccteam <描述你要什么>`,ccteam-creator 会尽力推断。推断不出来时会回 fallback 问你 a/b/c/d 选项。
