---
name: ccteam
description: ccteam 总入口 NL dispatcher。用户在 Claude session 内输入 `/ccteam <自然语言>` 触发,本 skill 把 NL 意图分类到 7 类(start-team / create-workflow / configure-im / monitor / advise / status-debug / other)并路由到对应 sub-skill。Use when 用户说"起一个 team / 做个 TG 助理 bot / 跑 qa-loop / 看 ccteam 状态 / 投票决定 / second opinion / 暂停 X / 为啥撞 budget"等任何 ccteam 相关意图。V0.6.0 Wave 1 雏形 — 5 sub-skill 中 `/ccteam-im-setup` + `/ccteam-advise` + `/ccteam-creator` 复活版要 Wave 2/3 才落地;Wave 1 主要把 `/ccteam-team` + `/ccteam-control`(V0.5 已有)路由跑通。
---

# /ccteam — NL dispatcher

V0.6.0 旗舰入口。用户**无需记多个 slash 名**,在 Claude session 内一句话描述需求,本 skill 自动判别意图 + 透传 NL 到对应 sub-skill。设计目标(详 `docs/v0-6-0/prd.md` F113):

- 用户说"我想做个 TG 助理 bot" → 路由 `/ccteam-creator` → Pocket Assistant preset
- 用户说"fix all TS errors" → 路由 `/ccteam-team 3 "fix all TS errors"`
- 用户说"我哪些项目还活着" → 路由 `/ccteam-control ls`
- 用户说"Claude + Codex 各给一个方案" → 路由 `/ccteam-advise`

灵感来源:OMC `README:231` "No commands to memorize, just describe what you want" 已市场验证。

## V0.6.0 skill 家族(本 skill 所处位置)

| 用户意图 | Skill | Wave 1 状态 |
|---|---|---|
| **(本 skill)总入口 NL dispatcher** | **`ccteam`** | **Wave 1 雏形(本文件)** |
| 起临时 team 干活(in-session 多 teammate) | `ccteam-team` | V0.5 已有 |
| 起新项目 / workflow / chat bot 配方 | `ccteam-creator` | V0.5 砍掉,Wave 2 F114 复活 |
| 查项目状态 / pause / resume / 跨项目 ls | `ccteam-control` | V0.5 已有 |
| TG / Lark / Slack 一次性 token 绑定 | `ccteam-im-setup` | Wave 2 F117 新建 |
| Claude + Codex 并行 advisor + 投票 | `ccteam-advise` | Wave 3 F112 新建 |

**Wave 1 实用范围**:意图分类落 5 路;`ccteam-team` / `ccteam-control` 路由跑通;`ccteam-creator` / `ccteam-im-setup` / `ccteam-advise` 三 sub-skill body 占位,用户被告知"该路径 Wave 2/3 才落地,目前请直接调 `<sibling>`"。

## When to invoke

用户在 Claude session 输入任一形式:
- `/ccteam <任何中英文 NL>`
- 自然语言提到 ccteam / "起 team" / "做 bot" / "我的项目" / "second opinion" 等,且当前 session 未在更专门的 sub-skill body 中

不要在以下场景触发:
- 用户已经 in-flight 调 `/ccteam-team` / `/ccteam-control` 等(sub-skill 自治)
- 用户直接 Bash 调 `ccteam ...` CLI(进 admin 路径,不 user-face)
- 用户问 "ccteam 是什么" 类纯知识问答(直接答,不路由)

## Step 1: 意图分类

对收到的 NL,落 7 类之一(优先级从上到下,匹配第一条即停):

| # | 意图 | 触发关键词 / 模式 | 示例 |
|---|---|---|---|
| 1 | **start-team** | "起 team" / "swarm" / "fix all X" / "并行 X" / "team N:role" / "重构 X" / "qa X" | `/ccteam "fix all TS errors"` |
| 2 | **create-workflow** | "做个 bot" / "做个 IM 助理" / "夜里跑 X" / "长跑监控 X" / "建 workflow" / "Pocket Assistant" / "Overnight" / "做个 TG 群多 bot" | `/ccteam "做个 TG 助理 bot"` |
| 3 | **configure-im** | "绑 TG token" / "换 Slack" / "我的 chat_id" / "IM 设置" / "怎么对接 Lark" | `/ccteam "绑 TG token"` |
| 4 | **monitor** | "我的项目状态" / "ls all" / "跑得怎样" / "看 X 项目" / "现在哪些 team 在跑" | `/ccteam "我哪些项目还活着"` |
| 5 | **advise** | "second opinion" / "投票决定" / "Codex + Claude 各给" / "两边都问下" / "advise X" | `/ccteam "Claude + Codex 各给一个方案"` |
| 6 | **status-debug** | "为啥撞 budget" / "看 log" / "stop X" / "pause X" / "resume X" / "X 卡住了" | `/ccteam "为啥撞 budget"` |
| 7 | **other** | 不在以上 6 类 | "你好 / ccteam 是什么 / hi" |

歧义启发式:
- 同时匹配 start-team + create-workflow:看是否提到"持久 / 长跑 / IM / bot" → create-workflow;否则 → start-team
- 同时匹配 monitor + status-debug:有具体 slug 提及 → status-debug;泛指 → monitor

## Step 2: 路由到 sub-skill

| 意图 | 路由 | 透传 |
|---|---|---|
| 1 start-team | `/ccteam-team <NL>` | 原 NL 原样 |
| 2 create-workflow | `/ccteam-creator <NL>`(Wave 2 F114 复活 — Wave 1 fallback "请直接说 'wave2-not-ready' 由用户重述") | 原 NL 原样 |
| 3 configure-im | `/ccteam-im-setup`(Wave 2 F117 新建 — Wave 1 fallback 同上) | 透传 token / IM 平台 hint |
| 4 monitor | `/ccteam-control <NL>` | 原 NL,如有 slug 提取 |
| 5 advise | `/ccteam-advise <NL>`(Wave 3 F112 新建 — Wave 1 fallback 同上) | 原 NL 原样 |
| 6 status-debug | `/ccteam-control <NL>` | 提取 slug + 动作 |
| 7 other | Step 3 fallback dialog | — |

**Wave 1 缺失 sub-skill 处理**:当路由目标是 Wave 2/3 才落地的 `/ccteam-creator` / `/ccteam-im-setup` / `/ccteam-advise` 时,告诉用户:

> 这个意图对应的 sub-skill (`<name>`) 是 V0.6.0 Wave 2/3 才落地的部分。Wave 1 你可以:
> - 用 `ccteam new "<NL>"` CLI 起新项目(create-workflow 临时替代,见 `docs/v0-1/user-quickstart.md`)
> - 用 `ccteam doctor --install-mcp` 装 MCP 后让 daily-driver Claude 直接 `mcp__ccteam__chat_send_input(...)` 调用(stub 返 NotImplemented,但 schema 已锁)
> - 用 `/ccteam-team` 临时起一个 advisor teammate(advise 临时替代)
> 完整 NL 路径 V0.6.0 Wave 2/3 上线后请重试 `/ccteam <NL>`。

## Step 3: Fallback dialog(意图分类失败 / 用户 NL 含糊)

回:

> 我没完全听懂你想做啥。你想:
> (a) 起一个 team 干活(写代码 / 重构 / qa-loop)
> (b) 做个 IM bot 助理(私聊 / 群内多 bot)
> (c) 看 ccteam 项目状态 / 暂停 / 恢复
> (d) Claude + Codex 双方咨询(advise / second opinion)
>
> 选个字母回我(`a` / `b` / `c` / `d`),或重新描述一下你想做啥。

收到字母:
- `a` → 走 start-team 路径,问"具体要 team 干啥?(描述任务)"
- `b` → 走 create-workflow 路径(Wave 1:Wave 2 缺失说明)
- `c` → 走 monitor 路径
- `d` → 走 advise 路径(Wave 1:Wave 3 缺失说明)

收到完整重述 → 重跑 Step 1。

## What this skill cannot do

- **不直接调 MCP tool 或 Bash 命令** — 只做 NL 分类 + slash 路由;真正执行由 sub-skill 负责
- **不维护多轮对话状态** — 一次 turn 做一次分类 + 路由;后续多轮归 sub-skill
- **不做 voice / 图片 / 多模态 input**(V0.7+)
- **不接受 skill 自定义注册** — 5 sub-skill 固定,用户不能 `/ccteam-add-skill <name>`(由 V0.6.0 PRD F113 §"不在范围"锁定)

## Where to look in the repo

- `@docs/v0-6-0/prd.md` — F113 完整需求 + 验收
- `@docs/v0-6-0/README.md` §三 — 用户面入口 + 5 sub-skill 一览
- `@skills/ccteam-team/SKILL.md` — start-team 详细行为
- `@skills/ccteam-control/SKILL.md` — monitor / status-debug 详细行为
- `@skills/ccteam-creator/SKILL.md` — Wave 2 复活后填实(目前 V0.5 砍掉态)

## Wave 1 状态(本文件)

本 SKILL.md 是 **V0.6.0 Wave 1 雏形**:
- ✅ frontmatter + 意图分类提示词 + 路由表 + fallback dialog 全落
- ⏳ `/ccteam-creator` (Wave 2 F114) / `/ccteam-im-setup` (Wave 2 F117) / `/ccteam-advise` (Wave 3 F112) sub-skill body 由后续 Wave 填实
- ⏳ host probe 5 sub-skill 验收(F113 验收 #5,基于 50 sample query intent classification ≥90% accuracy)Wave 4 落地

Wave 1 用户主要还是直接调 `/ccteam-team` / `/ccteam-control`(V0.5 已有);本 dispatcher 在它们前面加一层 NL→slash 翻译,体验提升从"记 3 slash"→"说人话"。
