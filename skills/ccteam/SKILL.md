---
name: ccteam
description: ccteam 总入口 NL dispatcher。用户在 Claude session 内输入 `/ccteam <自然语言>` 触发,本 skill 把 NL 意图分类到 8 类(start-team / create-workflow / configure-im / monitor / code-scan / advise / status-debug / other)并路由到对应 sub-skill。Use when 用户说"起一个 team / 做个 TG 助理 bot / 跑 qa-loop / 看 ccteam 状态 / 扫一下代码 / 摸底新项目 / 投票决定 / second opinion / 暂停 X / 为啥撞 budget"等任何 ccteam 相关意图。所有 ship intent 对应实工 skill,不再有 placeholder fallback。
---

# /ccteam — NL dispatcher

V0.6.0 旗舰入口。用户**无需记多个 slash 名**,在 Claude session 内一句话描述需求,本 skill 自动判别意图 + 透传 NL 到对应 sub-skill。设计目标(详 `docs/versions/v0-6-0/prd.md` F113):

- 用户说"我想做个 TG 助理 bot" → 路由 `/ccteam-creator` → Pocket Assistant preset
- 用户说"fix all TS errors" → 路由 `/ccteam-team 3 "fix all TS errors"`
- 用户说"我哪些项目还活着" → 路由 `/ccteam-control ls`
- 用户说"Claude + Codex 各给一个方案" → 路由 `/ccteam-advise`

灵感来源:OMC `README:231` "No commands to memorize, just describe what you want" 已市场验证。

## skill 家族(本 skill 所处位置)

| 用户意图 | Skill | 状态 |
|---|---|---|
| **(本 skill)总入口 NL dispatcher** | **`ccteam`** | **已 ship** |
| 起临时 team 干活(in-session 多 teammate) | `ccteam-team` | 已 ship |
| 起新项目 / workflow / chat bot 配方 | `ccteam-creator` | 已 ship |
| 查项目状态 / pause / resume / 跨项目 ls | `ccteam-control` | 已 ship |
| TG / Lark / Slack 一次性 token 绑定 | `ccteam-im-setup` | 已 ship |
| Claude + Codex 并行 advisor + 投票 | `ccteam-advise` | 已 ship |
| 扫代码摸底 / 大码库 audit | `ccteam-scan`(V0.6.2 F141 + V0.6.5 F157)| 已 ship |

## When to invoke

用户在 Claude session 输入任一形式:
- `/ccteam <任何中英文 NL>`
- 自然语言提到 ccteam / "起 team" / "做 bot" / "我的项目" / "second opinion" 等,且当前 session 未在更专门的 sub-skill body 中

不要在以下场景触发:
- 用户已经 in-flight 调 `/ccteam-team` / `/ccteam-control` 等(sub-skill 自治)
- 用户直接 Bash 调 `ccteam ...` CLI(进 admin 路径,不 user-face)
- 用户问 "ccteam 是什么" 类纯知识问答(直接答,不路由)

## Step 1: 意图分类

对收到的 NL,落 8 类之一(优先级从上到下,匹配第一条即停):

| # | 意图 | 触发关键词 / 模式 | 示例 |
|---|---|---|---|
| 1 | **start-team** | "起 team" / "swarm" / "fix all X" / "并行 X" / "team N:role" / "重构 X" / "qa X" | `/ccteam "fix all TS errors"` |
| 2 | **create-workflow** | "做个 bot" / "做个 IM 助理" / "夜里跑 X" / "长跑监控 X" / "建 workflow" / "Pocket Assistant" / "Overnight" / "做个 TG 群多 bot" | `/ccteam "做个 TG 助理 bot"` |
| 3 | **configure-im** | "绑 TG token" / "换 Slack" / "我的 chat_id" / "IM 设置" / "怎么对接 Lark" | `/ccteam "绑 TG token"` |
| 4 | **monitor** | "我的项目状态" / "ls all" / "跑得怎样" / "看 X 项目" / "现在哪些 team 在跑" | `/ccteam "我哪些项目还活着"` |
| 5 | **code-scan** | "扫一下代码" / "摸底新项目" / "scan code" / "audit codebase" / "这仓库是个啥" / "看看这代码用了啥" / "navigability audit" / "我的 monorepo 怎么接 ccteam" / "workflow.yaml 的 scope 该填什么" | `/ccteam "扫一下代码"` |
| 6 | **advise** | "second opinion" / "投票决定" / "Codex + Claude 各给" / "两边都问下" / "advise X" | `/ccteam "Claude + Codex 各给一个方案"` |
| 7 | **status-debug** | "为啥撞 budget" / "看 log" / "stop X" / "pause X" / "resume X" / "X 卡住了" | `/ccteam "为啥撞 budget"` |
| 8 | **other** | 不在以上 7 类 | "你好 / ccteam 是什么 / hi" |

歧义启发式:
- 同时匹配 start-team + create-workflow:看是否提到"持久 / 长跑 / IM / bot" → create-workflow;否则 → start-team
- 同时匹配 monitor + status-debug:有具体 slug 提及 → status-debug;泛指 → monitor
- 同时匹配 code-scan + start-team:动词是"扫 / 看 / 摸底 / audit"(只读)→ code-scan;动词是"改 / 修 / 重构 / fix"(写)→ start-team
- code-scan default 走 `--quick`(60-90s 摸底);用户明说"大码库 / monorepo / scope / navigability / 完整 audit" → 不带 `--quick`,走 audit mode

## Step 2: 路由到 sub-skill

| 意图 | 路由 | 透传 |
|---|---|---|
| 1 start-team | `/ccteam-team <NL>` | 原 NL 原样 |
| 2 create-workflow | `/ccteam-creator <NL>` | 原 NL 原样 |
| 3 configure-im | `/ccteam-im-setup` | 透传 token / IM 平台 hint |
| 4 monitor | `/ccteam-control <NL>` | 原 NL,如有 slug 提取 |
| 5 code-scan | `/ccteam-scan --quick` (default) 或 `/ccteam-scan` (用户明说大码库 / audit) | 仓库路径(默认当前 cwd) |
| 6 advise | `/ccteam-advise <NL>` | 原 NL 原样 |
| 7 status-debug | `/ccteam-control <NL>` | 提取 slug + 动作 |
| 8 other | Step 3 fallback dialog | — |

## Step 3: Fallback dialog(意图分类失败 / 用户 NL 含糊)

回:

> 我没完全听懂你想做啥。你想:
> (a) 起一个 team 干活(写代码 / 重构 / qa-loop)
> (b) 做个 IM bot 助理(私聊 / 群内多 bot)
> (c) 看 ccteam 项目状态 / 暂停 / 恢复
> (d) Claude + Codex 双方咨询(advise / second opinion)
> (e) 摸底新代码库 / 扫一下代码(快速 60-90s 报告)
>
> 选个字母回我(`a` / `b` / `c` / `d` / `e`),或重新描述一下你想做啥。

收到字母:
- `a` → 走 start-team 路径,问"具体要 team 干啥?(描述任务)"
- `b` → 走 create-workflow 路径
- `c` → 走 monitor 路径
- `d` → 走 advise 路径
- `e` → 走 code-scan 路径(`/ccteam-scan --quick`)

收到完整重述 → 重跑 Step 1。

## What this skill cannot do

- **不直接调 MCP tool 或 Bash 命令** — 只做 NL 分类 + slash 路由;真正执行由 sub-skill 负责
- **不维护多轮对话状态** — 一次 turn 做一次分类 + 路由;后续多轮归 sub-skill
- **不做 voice / 图片 / 多模态 input**(V0.7+)
- **不接受 skill 自定义注册** — 6 sub-skill 固定(team / creator / control / im-setup / advise / scan),用户不能 `/ccteam-add-skill <name>`(由 V0.6.0 PRD F113 §"不在范围"锁定)

## Red line — 未实现 intent **直接隐藏不渲染**(V0.6.5 F159)

任何尚未 ship 的 intent **必须直接从 dispatcher 表面消失**,不得以下列任一形式向用户暴露:

- 路由表 / Step 1 意图表里出现该 intent 行
- Step 3 fallback dialog 4-options 列出该 intent
- "V0.7 即将支持" / "敬请期待" 等任何 forward-looking 文案
- 路由到未实现 sub-skill 后由 sub-skill 报 "尚未实现"(这种 dead-end 是用户视角最差的体验)

**Ship gate**:每个新 intent 进 dispatcher 前必须先确认 ──
1. 对应 sub-skill 的 SKILL.md body 已写完(真路径,非半成品)
2. sub-skill 依赖的 MCP 工具 dispatch 真实现(返真结果,不是错误 stub)
3. 真路径在 host probe 验过

满足 3 条才把 intent 加进 Step 1 表 + Step 2 路由表 + Step 3 fallback 字母选项。Dispatcher
4-options 动态按 NL 推断 + ship 状态选出最相关的 4 个(V0.6.5 ship 后 7 intent 全可见)。

V0.6.6+ 新 intent ship 前 4-options 表 / 路由表里**不能预占行**;ship 当天才加进来。

## Where to look in the repo

- `@docs/versions/v0-6-0/prd.md` — F113 完整需求 + 验收(dispatcher 初版)
- `@docs/versions/v0-6-5/prd.md` — F157 code-scan intent 接入
- `@docs/versions/v0-6-0/README.md` §三 — 用户面入口 + sub-skill 一览
- `@skills/ccteam-team/SKILL.md` — start-team 详细行为
- `@skills/ccteam-control/SKILL.md` — monitor / status-debug 详细行为
- `@skills/ccteam-creator/SKILL.md` — create-workflow 详细行为
- `@skills/ccteam-scan/SKILL.md` — code-scan 详细行为(quick + audit 两 mode)

## 当前状态

- ✅ frontmatter + 意图分类提示词 + 路由表 + fallback dialog 全落
- ✅ `/ccteam-creator` / `/ccteam-im-setup` / `/ccteam-advise` / `/ccteam-scan` sub-skill body 均已 ship
- ✅ 8 intents(7 work + 1 fallback)全部对应实工 skill,不再有 placeholder

本 dispatcher 在所有 sub-skill 前面加一层 NL→slash 翻译,体验从"记多个 slash"→"说人话"。
