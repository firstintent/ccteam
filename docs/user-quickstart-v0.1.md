# ccteam 用户快速指南 V0.1

> **版本**:V0.1(2026-05-06)
> **覆盖功能**:M0(单项目 CLI)、M0.5(工具触发面)、M1(meta-agent + decisions)、M2(sub-skill / phase YAML / `ccteam-mcp`)、M2.3 follow-up(golden_rules)、M3(team abstraction + product-research team)、**M4.1–M4.4(跨项目记忆基础设施 — 注意 F22 阻塞实际生效,详见 §10)**
> **未覆盖(后续里程碑)**:M5 Critic / multi_session / TUI / web dashboard
>
> 本文档面向**第一次手动跑 ccteam 全流程的用户**。每一步给可复制的命令 + 预期看到的现象;
> 跟实测不一致 → 报 issue。
>
> **目标**:看完照做,从 zero 跑到一个 dev 项目 ship + 一个 product-research 项目出 verdict,
> 而且**全程通过 meta-agent 用自然语言对话**,不需要记 CLI flag。

---

## 0. 你需要什么

| 项 | 验证命令 | 期望 |
|---|---|---|
| Linux / WSL2 | `uname -a` | Linux …WSL2… 或裸 Linux |
| `tmux` | `tmux -V` | ≥ 3.0(`apt install tmux`) |
| `claude` CLI | `claude --version` | ≥ 2.1.59(auto-memory 起步版本) |
| `git` | `git --version` | 任意现代版 |
| `cargo` | `cargo --version` | stable rustc(`rustup default stable`) |
| (可选)`gh` | `gh --version` | M4+ PR 自动化 |

> **不需要 Docker**:V0.1 默认走 host 直跑 + `--dangerously-skip-permissions`。
> 容器隔离是 V1.0 才上的产品形态(详见 tech-design §6.1)。

---

## 1. 装 ccteam

```bash
git clone git@github.com:firstintent/ccteam.git ~/workplace/agents/ccteam
cd ~/workplace/agents/ccteam
make install
ccteam --version
```

**期望输出**:`ccteam <version>`。如果 `ccteam: command not found`,把 `~/.cargo/bin` 加进 `PATH`。

---

## 2. 一次性 setup(`ccteam doctor`)

ccteam 不存在"配置文件",所有装机状态由 `ccteam doctor` 子命令幂等管理。**按下面顺序跑一次,以后不用重复**(除非你换机器)。

```bash
ccteam init                                      # 写 ~/.ccteam/ 骨架(phases/ teams/ templates/)
ccteam doctor --install-recommended-agents       # ln -sf 8 个 plugin agent 到 ~/.claude/agents/
ccteam doctor --tool-surface                     # 体检:phase tools_required 跨参表,验证可达
ccteam doctor --install-skill                    # 装 ~/.claude/skills/ccteam-control/
ccteam doctor --install-mcp                      # 在 ~/.claude.json 注册 mcpServers.ccteam(9 个 tool)
ccteam doctor --install-memory-bridge            # 写 ~/.claude/rules/ccteam-lessons-{dev,product-research}.md(M4)
ccteam doctor --install-meta-agent <你的 handle> # 创建 ~/projects/<handle>-meta/(meta-agent 工作目录)
```

> **关于 `--install-memory-bridge`**:M4 跨项目记忆基础设施。装了之后,每次 dev / product-research 项目跑完 ship / REJECT verdict,Claude 会把跨项目 lessons append 到对应文件的 `<!-- ccteam-managed:lessons -->` marked section;下个项目启动时**官方 rules 机制**自动加载到上下文。**当前限制**:`paths:` frontmatter scope 到 `~/projects/<team>-*`,但 V0.1 bootstrap 仍创建 `~/projects/<slug>/`(无 team 前缀),所以**写入有效但下个项目不自动加载**(F22 待修;详见 §10)。装了不亏 — 写入会累积,F22 修完即自动激活。

把 `<你的 handle>` 换成你想叫自己的名字(snake-case,例:`rob` / `alice`)。这名字后面会出现在 tmux session 名 `ccteam-meta-<handle>`、meta-agent 项目目录、决策回执等地方。

**最后一条命令的预期输出**(节选):

```
created          ~/projects/<handle>-meta/.ccteam/
installed skill  ~/.claude/skills/ccteam-control/
linked agents    ~/.claude/agents/{code-architect, code-reviewer, ...}
tmux session     ccteam-meta-<handle>
attach with      tmux attach -t ccteam-meta-<handle>
```

> **常见坑**:`--install-mcp` 之后已经在跑的 claude session 看不到新 MCP server。**新开** claude 终端,或在已有 session 里用 `/reload-mcp`。

---

## 3. 起 orchestrator(常驻进程)

```bash
ccteam start --foreground       # 前台跑,看 log;开发推荐
# 或
ccteam start                    # 后台跑(detach 后留下 ~/.ccteam/state/orchestrator.pid)
```

**前台模式**会持续打印:

```
INFO orchestrator started        version=...
INFO scanning teams              dev product-research
INFO loaded 6 phases             dir=phases
INFO loaded 6 phases             dir=phases-product-research
INFO loaded meta-agent           handle=<handle>
INFO ensuring meta session       tmux=ccteam-meta-<handle>
INFO tick                        elapsed=... projects=0
```

看到 `tick` 周期出现就 OK。**保留这个终端**;关了 orchestrator 就停。

---

## 4. 跟 meta-agent 工作(核心)

这是 ccteam 的"操作面板"。**你不用记任何 CLI flag**,跟一个常驻 claude 用自然语言说话即可。

### 4.1 三个入口任你选

| 入口 | 怎么用 | 何时用 |
|---|---|---|
| **A. tmux attach 直接对话**(最直接) | `tmux attach -t ccteam-meta-<handle>` | 想跟 meta-agent 一对一深聊 / 看它的思考过程 |
| **B. daily-driver claude + skill**(最方便) | 任意目录开 `claude`,直接说话 | 已经在写代码,顺手 dispatch / 查状态 |
| **C. MCP 直调**(给程序用) | 让你的 daily claude 调 `mcp__ccteam__*` | 写脚本 / agent 时 |

V0.1 推荐 **A 跟 B 混用**:深度对话用 A,日常用 B。

### 4.2 入口 A:tmux attach 直接对话

```bash
tmux attach -t ccteam-meta-<handle>
```

你看到的是一个常驻 claude session,它读了 `meta_agent_role.md` 知道自己的职责。直接说话:

```
> 你能做什么?
```

它会列出 dispatch / status / decision walkthrough 三类能力。

**离开 session**:`Ctrl-b d` (tmux detach)。**绝对不要按 `Ctrl-c`**,那会杀 claude;orchestrator 会在下个 tick 重启它,但当前对话上下文就丢了。

### 4.3 入口 B:daily-driver claude + ccteam-control skill

任意目录(就你平时写代码的目录)开 claude:

```bash
cd ~/anywhere
claude
```

跟它说:

```
> ccteam 现在跑了什么项目?
```

claude 会自动加载 `ccteam-control` skill(已通过 `--install-skill` 装),用 `mcp__ccteam__ls` 拿项目列表,然后用自然语言告诉你。**它跟入口 A 的 meta-agent 看到的是同一份状态**(都通过 `~/.ccteam/` + MCP 看)。

差别是:
- 入口 A 是**常驻**的 meta-agent — 它有跨 session 累积的上下文
- 入口 B 是**临时**的 daily-driver claude — 每次重开 session 都是 fresh

### 4.4 NL 派单(经过 meta-agent dispatch tree)

无论哪个入口,跟它说:

```
> 做一个本地 todo cli,Rust 写,SQLite 存储
```

meta-agent / skilled claude 走 dispatch 决策树:

1. **是问答还是项目请求?** — 这是项目请求。
2. **派 dev 还是 product-research?** — Brief 具体(明确技术栈),派 dev。
3. **CLARIFY 一轮**(若 brief 太短) — 例如对"做个 todo"它会反问"web 还是 CLI?"。
4. **执行**:`mcp__ccteam__new(team="dev", brief="<refined>")`。
5. **回执**:它会告诉你 slug,例如 `slug = todo-cli-rust-sqlite-add-list-done-rm`。

**常见模式**:

| 你说 | meta-agent 怎么走 |
|---|---|
| "做一个 X(具体)" | 直接派 dev |
| "做个 todo"(模糊) | 反问一句 → 派 dev |
| "我有个想法,X 行不行" | 建议派 product-research 评估,等 verdict 再决定 dev |
| "帮我看看 ccteam 现在状态" | 拉 `ls` 总结,不派单 |
| "项目 X 卡住了怎么办" | `show` + `peek` 后给建议(attach / pause / resume) |

### 4.5 跟踪进度(在对话里问就行)

```
> 这个项目现在到哪步了?
```

meta-agent 调 `mcp__ccteam__show <slug>`,告诉你:

- `current_phase`(plan-eng / implement / test-author / test-run / fix / ship)
- `phase_state`(in_flight / idle)
- `cost_used_usd`
- 最近 5 个事件

或者跑 `ccteam show <slug>` 自己看。

```
> 我想看看它现在 claude 在干啥
```

→ `mcp__ccteam__peek` 抓项目 tmux pane 的当前内容回给你,不打扰它。

```
> 我要 attach 进去自己看
```

→ meta-agent 告诉你 `tmux attach -t ccteam-<slug>`(它自己不能帮你 attach,这是 TTY 操作)。**记得用 `Ctrl-b d` 退出**,别 `Ctrl-c`。

---

## 5. 走完一个 dev 项目(端到端)

跑通这一节 = ccteam 的 dev pipeline 你完整见过一次。预计 **30 分钟 – 2 小时**(取决于项目复杂度 + LLM 速度)。

### 5.1 派单

入口 B(daily claude),说:

```
> 做一个 todo cli,Rust 写,本地 SQLite 存储,支持 add / list / done / rm 子命令,带单元测试
```

收到回执 slug(假设 `todo-cli`)。

### 5.2 看它跑

打开第二个终端跑 `tail -f`:

```bash
tail -f ~/.ccteam/progress/todo-cli.jsonl | jq -c '{event, phase: .phase // "_", note: .note // .tool // .reason // ""}'
```

你会看到 phase 切换:

```
{"event":"phase_inject","phase":"plan-eng","note":""}
{"event":"phase_done","phase":"plan-eng","note":""}
{"event":"phase_inject","phase":"implement","note":""}
{"event":"PreToolUse","phase":"implement","note":"Edit"}
...
{"event":"subskill_started","phase":"implement","note":"code-reviewer"}
{"event":"subskill_done","phase":"implement","note":""}
{"event":"phase_inject","phase":"test-author","note":""}
...
{"event":"phase_done","phase":"ship","note":""}
```

**6 个 phase 顺序**(M3 后 dev pipeline):

| # | Phase | 它在做什么 |
|---|---|---|
| 02 | plan-eng | 反向面试你(若 brief 模糊)→ 写 plan-eng.md(技术栈、模块划分、todo 列表) |
| 03 | implement | 按 plan 写代码;**phase_done 后自动触发 code-reviewer sub-skill** |
| 04 | test-author | 写测试 |
| 05 | test-run | 跑测试,捕获结果 |
| 06 | fix | 失败则进 fix-loop(最多 3 轮);全绿走 ship |
| 09 | ship | 创建 git commit,写 retro.md,golden_rules 检查;**M4**:retro 同时写 `/memory`(本仓 auto-memory)+ `Edit ~/.claude/rules/ccteam-lessons-dev.md` marked section(跨项目) |

`phase 01` 是 product-research 才有的 kickoff;`phase 07/08` 在 V0.1 没启用(Score 已删,Critic 留 M5)。

### 5.3 ship 后

```
> todo-cli 跑完了吗?
```

meta-agent 告诉你"已 ship,在 `~/projects/todo-cli/`"。进去看:

```bash
cd ~/projects/todo-cli
ls
cat .ccteam/retro.md             # ship phase 写的回顾(项目内)
cargo run -- add "买菜"          # 实际能跑
cargo test                        # 测试全绿
git log --oneline                 # ccteam 自己 commit 的历史

# 跨项目 lessons(M4 — 装了 --install-memory-bridge 才有)
cat ~/.claude/rules/ccteam-lessons-dev.md
# marked section 内多了一段以本项目 slug + 日期为 H2,4 个 H3 字段(tech_stack /
# pitfalls / successful_designs / do_not_do_again);下次 dev 项目启动时官方 rules
# 机制自动加载这文件到 plan-eng phase 上下文
# (V0.1 注意:F22 阻塞 paths scope,自动加载暂不工作 — 详见 §10)
```

### 5.4 价值

这个项目你**全程没碰键盘**(除了最初一句 NL + 中间可能回答一两个 CLARIFY)。$5–$10 token cost,半小时到几小时,产出可用代码 + 测试 + commit。

---

## 6. 走完一个 product-research 项目(verdict 流程)

dev team 帮你**做**;product-research team 帮你**判断该不该做**。一个想法值不值得开发,先派 product-research,产出 PASS / CONCERN / REJECT / CLARIFY 决策。

### 6.1 派单

```
> 我有个想法:AI 菜谱生成器,用户上传冰箱照片,AI 给食谱建议。这个值得做吗?
```

meta-agent 识别这是不确定方向,建议 product-research,你确认派单。slug 假设 `ai-recipe-fridge-photo`。

### 6.2 6 个 phase

| # | Phase | 输出 |
|---|---|---|
| 01 | kickoff | brief.md(反向面试你的需求 + 用户画像 + 成功标准) |
| 02 | market-survey | market-survey.md(同类产品 / 市场规模) |
| 03 | differentiation-analysis | differentiation.md(你的差异化在哪;若无 → ESCALATE: LOW_DIFFERENTIATION) |
| 04 | value-proposition | value-prop.md(给谁、解决什么) |
| 05 | feasibility | feasibility.md(技术 + 商业可行;**可能触发 PHASE_DONE_PENDING** 等用户决策再继续) |
| 06 | verdict | verdict.md + rationale.md + next-steps.md(最终 PASS / CONCERN / REJECT / CLARIFY);**M4**:REJECT 分支会同时写 `/memory` + `Edit ~/.claude/rules/ccteam-lessons-product-research.md` marked section,把这次否决落进跨项目 lessons 库 |

### 6.3 中途的决策点(hybrid mode)

product-research kickoff / verdict 用 `decision_mode: hybrid` — Claude 会停下来用 `AskUserQuestion` 工具弹问题。你会在 attach session 看到一个多选 dialog,在那回答即可,**不用切回 meta-agent**。

最多 `max_clarify_rounds: 3` 轮;每轮一个问题。

### 6.4 PHASE_DONE_PENDING(异步决策)

feasibility 可能输出 `PHASE_DONE_PENDING` — 表示"我已经写完了,但**有问题需要你定夺**才能进 verdict"。这时:

- `current_phase` 仍 feasibility,`phase_state: idle`
- `~/projects/<slug>/.ccteam/outbox/` 出现 `clarify-<ts>.md`(decision_mode: async 协议)

```
> ccteam 有什么待决策?
```

meta-agent 调 `ccteam decisions` → 列出所有项目的 pending 问题:

```
| slug                     | phase       | kind    | summary                          |
|--------------------------|-------------|---------|----------------------------------|
| ai-recipe-fridge-photo   | feasibility | clarify | API 月成本预估超 $200,接受吗?   |
```

回答:

```
> 那个 API 成本问题,接受。我们用 GPT-4o-mini 控成本
```

meta-agent 把决策注入项目 inbox(`mcp__ccteam__inject_decision`),orchestrator 下个 tick 推给项目 session,phase 推进到 verdict。

### 6.5 verdict 出来后

```
> ai-recipe 评估完了吗?
```

meta-agent 报告 `verdict.md` 内容:`verdict: REJECT` + 主要理由。完整文件在 `~/projects/ai-recipe-fridge-photo/.ccteam/`。

如果是 PASS / CONCERN,meta-agent 会主动建议:**要不要顺势派 dev 团队按 verdict 落地?**

如果是 REJECT,跨项目 lessons 已写:

```bash
cat ~/.claude/rules/ccteam-lessons-product-research.md
# marked section 末尾多一段 H2 = "<slug> (YYYY-MM-DD) — REJECT" + 4 个 H3 字段
# (market_signals / differentiation_findings / feasibility_assessment / verdict_rationale)
# 下次有相似 idea 进 product-research kickoff 时,Claude 会先扫这文件,
# 命中重复 idea 直接短路 5 phase 流程倾向 REJECT
# (V0.1 注意:F22 阻塞 paths scope,自动加载暂不工作 — 详见 §10)
```

### 6.6 (可选)claude-mem 增强:跨项目深度检索

如果你装了 [claude-mem](https://github.com/thedotmack/claude-mem) 插件:

```bash
npx claude-mem install
# 或通过插件市场:/plugin marketplace add claude-mem
```

claude-mem 会:
- 自动 hook(SessionStart / Stop / SessionEnd 等 5 个生命周期钩子)捕获每个 session 的对话与决策摘要,无需 ccteam 干预
- 暴露 4 个 read-only MCP tool(`search` / `timeline` / `get_observations` / `__IMPORTANT`),支持跨项目 FTS5 全文检索 + 类型过滤(bugfix / feature / decision / discovery / refactor / change)

ccteam phase prompt(plan-eng / kickoff / verdict)写了 conditional:**"如果工具列表里看到 `mcp__*claude-mem*search`,你可以调它做跨项目语义检索"** —— LLM 自看 tool surface 决定调不调,**ccteam 不写检测代码、不写集成代码**。

没装就完全跳过 — V0.1 默认机制(`~/.claude/rules/` + auto-memory)已够用(F22 修完后)。

---

## 7. 介入工具速查

跑久了不可避免要介入。把这张表收藏:

| 你想 | 入口 B 怎么说 / CLI 怎么写 |
|---|---|
| 看所有项目 | "ccteam 现在跑了什么?" / `ccteam ls` |
| 看一个项目细节 | "X 现在啥情况?" / `ccteam show <slug>` |
| 不打扰看 pane | "X 当前 claude 在做啥?" / `ccteam peek <slug>` |
| 进去自己介入 | `tmux attach -t ccteam-<slug>`(meta 不能代替你 attach) |
| 暂停一个项目 | "暂停 X" / `ccteam pause <slug>`(orchestrator 不再 tick 它,session 留着) |
| 恢复一个项目 | "恢复 X" / `ccteam resume <slug>` |
| 看待决策清单 | "有什么决策待定?" / `ccteam decisions` |
| 看实时事件 | `tail -f ~/.ccteam/progress/<slug>.jsonl \| jq -c '...'` |

---

## 8. 卡住时的诊断

**症状 A:`stall` 警告 5 分钟没新事件**

```
WARN stall ≥5min  silent_seconds=320 slug=todo-cli phase=implement
```

不一定真死。先看:

```
> X 卡住了吗,看看 pane
```

meta peek 后看:
- claude 在等用户输入(常见:phase prompt 让它确认某事)→ `tmux attach` 自己回
- claude 真在思考 / 跑工具(很慢但活着)→ 别动,等
- claude 死了(panic / crash)→ orchestrator 下 tick 会自愈;若 30min+ 没动静,`ccteam pause <slug>` 后 `ccteam resume <slug>`

**症状 B:`escalate` 事件出现**

```
{"event":"escalate","reason":"fix-loop hit 3 iterations without TESTS_GREEN"}
```

项目进 `phase_state: idle`,`current_phase` 留在出问题的 phase。`~/projects/<slug>/.ccteam/escalation.md` 写明原因。

```
> X 升级了,看看怎么回事
```

meta-agent 读 escalation.md 给你建议:
- 测试逻辑写错了 → attach 改测试 → `ccteam resume <slug>`(F8 修复后能正确清 terminal state)
- 真不该跑了 → 不 resume,在 ~/projects/ 留底,起一个新项目

**症状 C:成本爆了**

```
WARN soft cost warn  cost_used_usd=12.5 slug=...
```

V0.1 三档警告:soft($5)/ medium($50)/ hard($200,**强制 kill**)。

```
> X 现在花了多少?
```

meta 调 `show` 看 `cost_used_usd`。预算超就 `pause` 让它停。

---

## 9. 关停 / 维护

```bash
ccteam stop                              # 关 orchestrator,**保留** tmux sessions(reattach 即继续)
ccteam stop --kill-sessions              # 同时关所有项目 tmux session(慎用)
ccteam doctor --tool-surface             # 装机健康自检(每月跑一次)
```

**升级 ccteam**:

```bash
cd ~/workplace/agents/ccteam
git pull
make install
ccteam doctor --tool-surface             # 验证 phase tools_required 没被新增的破坏
```

**忘记某个 install-* 是否做过**:重跑就行,所有 doctor 子命令幂等。

---

## 10. V0.1 已知不能做的事

避免你抓狂:

| 想做 | 现状 | 何时能做 |
|---|---|---|
| 跨项目记忆("上次类似项目我们怎么做的") | **基础设施已 ship**(M4.1–M4.4,2026-05-06):retro 写入有效;**但 F22 阻塞实际生效** —— `~/.claude/rules/ccteam-lessons-<team>.md` 的 `paths: ~/projects/<team>-*` 跟 bootstrap 实际产 `~/projects/<slug>/` 不匹配,下个项目启动时 rules 不自动加载 | F22 修(slug 加 team 前缀;dev-coupling-audit.md F22 P0)— **即将到来的独立 PR**;修完即激活 |
| 多 agent 并行写代码(speed-up) | `parallelism: agent_team` schema 已落但 **enablement 永久 deferred**(spike A) | Claude Code 释出 first-class Agent Teams CLI 后 |
| 大项目子模块拆分 (`multi_session`) | 未 ship | M4 后期(M4.8) |
| Critic agent("接口不优雅"反馈) | 未 ship | M5 |
| 跨设备(Telegram / Feishu)入口 | 未 ship | M2+ Channel layer |
| TUI / web dashboard | 未 ship(底层 `ccteam-mcp` 已 ship,前端缺) | M4 / M5 机会主义任务(M4.9 / M5.6) |
| **claude-mem 跨项目深度检索**(可选增强) | 不在 ccteam 范围 — 用户自装即用 | `npx claude-mem install` 后 phase prompt 自动识别其 MCP tool |

---

## 11. 一份"今天就跑完"的 checklist

```
[ ] 装好 cargo / tmux / claude(§0)
[ ] git clone + make install + ccteam --version(§1)
[ ] ccteam init
[ ] ccteam doctor --install-recommended-agents
[ ] ccteam doctor --tool-surface(全绿)
[ ] ccteam doctor --install-skill
[ ] ccteam doctor --install-mcp
[ ] ccteam doctor --install-memory-bridge       # M4 跨项目 lessons rules 文件
[ ] ccteam doctor --install-meta-agent <handle>
[ ] 终端 1:ccteam start --foreground
[ ] 终端 2:tmux attach -t ccteam-meta-<handle> → 试一句"你能做什么"
[ ] 终端 3:cd ~/anywhere && claude → 试一句"ccteam 现在跑了什么"
[ ] 跑一个 dev 项目到 ship(§5)
[ ] 跑一个 product-research 项目到 verdict(§6)
[ ] 把 retro.md / verdict.md 看完一遍
```

跑完这张 checklist,你已经能用 ccteam 替你跑日常项目了。

---

## 12. 反馈 / 报 bug

文档跟实测不一致 → `gh issue create` 或在仓库 issues 里写一份;**复制实际看到的输出**,告诉我们 ccteam version + claude version。

V0.2 计划把 M4 跨项目记忆加进来,这份文档会同步更新。
