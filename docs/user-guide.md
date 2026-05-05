# ccteam 用户使用指南

> 本文是**用户视角**的使用文档:怎么装、怎么用、怎么和它打交道。
>
> 想了解为什么这样设计,看 [requirements.md](./requirements.md)(痛点)和 [tech-design.md](./tech-design.md)(架构论证)。

---

## 1. ccteam 是什么

**一句话**:你提一句话需求 → 关电脑出门 → 回来收一个跑过测试的项目。

**它怎么运转**:
- 每个项目对应一个 [tmux](https://github.com/tmux/tmux/wiki) 长 session,session 内跑一个 Claude Code 长进程
- ccteam 是上层编排——按 9 个 phase 顺序推进、监控成本和进度、卡住时找你
- **ccteam 不是聊天客户端,它是被聊的对象**:用对话方式管它,**用你自己的 Claude Code session**(详见 §6)

**前提**:`claude` / `tmux` / `bash` / `jq` 能跑(macOS / Linux 标配 + `brew install jq`)。**任何 tmux 兼容终端都能 attach**——推荐 [Warp](https://github.com/warpdotdev/warp) / iTerm2 / Alacritty 等做更好的本地终端体验,ccteam 无需特殊集成。

---

## 2. 装

```bash
# A. brew(主推,macOS / Linux)
brew tap ccteam/tap && brew install ccteam

# B. install.sh(任意 Unix)
curl -fsSL https://ccteam.dev/install.sh | sh

# C. 从源码
cargo install ccteam
```

`ccteam --version` 能跑就行。binary 5–10MB,**单文件、零运行时依赖**(纯 Rust 静态链接)。

---

## 3. 首次启动:`ccteam init`

```bash
$ ccteam init

✓ 创建 ~/.ccteam/  (config.yml / phases/ / hooks/ / inbox/ / queue/ / memory/ / state/)
✓ unpack 9 个 phase 模板 + 5 个 hook 脚本
✓ 体检:
    claude:        v0.1.42  ✓
    tmux:          v3.3a    ✓
    jq:            v1.7.1   ✓
    telegram_token:  未配置  (M1+ 才用,可后续 ccteam config edit)

下一步:
  ccteam start                       # 启动 orchestrator
  ccteam new "你的第一个想法"         # 一句话立项

想用对话方式管 ccteam?在任意目录开 claude,告诉它你想做什么——
ccteam 提供了 ccteam-control skill,Claude 知道怎么调度。详见 §6。
```

`~/.ccteam/` 里有什么、为什么这么放,见 [interfaces.md §1.1](./interfaces.md#11-全局目录ccteam)。

---

## 4. 第一次用(完整流)

```bash
# 终端 1:启动 orchestrator(前台看日志)
$ ccteam start
✓ orchestrator running (PID 12345),no projects in queue

# 终端 2:提需求
$ ccteam new "做一个本地书签管理器,离线可用,按域名归类"
✓ Created project bookmark-mgr-a3f9
✓ Workspace: ~/projects/bookmark-mgr-a3f9
✓ Tmux session: ccteam-bookmark-mgr-a3f9

# 实时看团队在做什么
$ ccteam attach bookmark-mgr-a3f9
# (三 pane:主 pane 看 claude / 右上看事件流 / 右下看成本)
# Ctrl+B D 离开,team 继续跑

# 几小时后回来收
$ ccteam ls
SLUG                       PHASE        COST   AGE
bookmark-mgr-a3f9          ship         $1.23  3h45m  ✓ ready

$ ccteam show bookmark-mgr-a3f9
# (phase history / 测试报告 / scorecard / artifact 路径)
```

**关键体验**:`ccteam new` 一条命令,关电脑,回来 `ccteam ls`。

`ccteam new` 那一步**没有 LLM 在解析意图**——只是写一个 inbox 文件。意图理解发生在项目级 Claude 的 Phase 0(Seed),由 PASS / REJECT / CLARIFY 决定走向。CLARIFY 时通过 inbox(M0)/ telegram(M1+)/ 你自己的 claude(§6)和你多轮聊。

---

## 5. 三种"看团队在做什么"的姿势

| 命令 | 进入终端 | 能输入吗 | 适合场景 |
|---|---|---|---|
| `ccteam attach <slug>` | 是(三 pane 仪表盘) | 是;输入会自动暂停 phase | 想纠偏("等等,先别用 SQLite") |
| `ccteam peek <slug>` | 否(只截屏) | 否 | 怕手抖,只想瞄一眼 |
| `ccteam show <slug>` | 否(读 state.json) | 否 | 看进度/成本/历史,不在乎实时输出 |

### 5.1 `ccteam attach` vs 直接 `tmux attach`

底层等价(`ccteam attach <slug>` ≡ `tmux attach -t ccteam-<slug>`),但 ccteam 套了三层增量:

| | tmux attach 直接来 | ccteam attach 帮你做 |
|---|---|---|
| 命名 | 要记 `ccteam-bookmark-mgr-a3f9` 全名 | 只输 slug,`ccteam ls` 顺便列可 attach 项 |
| 布局 | 单 pane,只看 claude | 进来就是三 pane:主 pane = claude 交互;右上 = `tail -f progress.jsonl \| jq` 实时事件流;右下 = `watch state.json` 当前 phase + 累计成本 |
| 介入语义 | 你打字 = 普通 tmux 输入,orchestrator 不知情 | PreToolUse hook 检测输入源,**人输入会自动暂停 phase 推进**,等你 detach 或 `ccteam resume` |

### 5.2 attach 后看到的是什么

**主 pane 是 100% 原生 Claude Code 终端 UI**——模型输出、工具调用、思考、`/btw`、permission 提示全在,跟你直接跑 `claude` 一模一样。tmux 不套壳、不转译,**只增不减**。

### 5.3 用户介入会发生什么

attach 后键盘敲字 → orchestrator 立刻通过 PreToolUse hook 看到"输入来源是人,不是 send-keys" → 自动 `pause` 当前项目,**不会再注入下一个 phase**。等你:
- `Ctrl+B D` detach 后超过 N 分钟(视为放权,自动 resume),或
- 显式 `ccteam resume <slug>`(立刻接管)

不想冒着误输入风险打开,只想瞄一眼:用 `ccteam peek <slug>`,等价于 `tmux capture-pane -p`,**绝不会打断 claude**。

---

## 6. 用对话方式管 ccteam:用你自己的 claude

ccteam 不内置 chat 模式。**最优雅的办法是用你自己的 Claude Code session 当入口**——它本来就是顶级的对话 AI。

```bash
# 在任意目录(不是 ccteam-* tmux session 里)开你自己的 claude
$ cd ~
$ claude

> 我所有 ccteam 项目状态怎么样?
[claude 调 ccteam ls + ccteam show <每个 slug>]
[claude 用人话总结:bookmark-mgr 卡 fix-loop / notes-app 等你 ship]

> 我想做个图片去水印工具,只在我无聊时跑
[claude 多轮澄清:目标平台 / 模型选择 / 优先级]
[claude 调 ccteam new "..." --priority low]

> bookmark-mgr 卡了,帮我看看出什么事
[claude 调 ccteam peek + 看 progress.jsonl 末尾]
[claude 分析:看起来是 SQLite migration 撞了已有 schema,建议 attach 后给 hint "用 ALTER TABLE 而非 DROP"]
```

**为什么这样最好**:
- ccteam 不需要发明 chat 模式,边界更干净
- 你的 claude 已有你的偏好 / 习惯 / 长期记忆(memory),ccteam 不需要学
- 同一个 session 里可以让 claude 综合 ccteam + `gh pr list` + `git log` 等做联合判断
- 升级互不干扰

### 6.1 ccteam 提供给你的 claude 的扩展点

| 阶段 | 怎么用 | 谁来用 |
|---|---|---|
| **M0** | CLI 输出对 LLM 友好(`ccteam ls --format json` / `ccteam show --format json`) | 你的 claude 用 Bash 工具调,自己解析 JSON |
| **M1** | 装 `~/.claude/skills/ccteam-control/SKILL.md`(描述 CLI、典型工作流、何时该 attach vs peek) | 你的 claude 自动激活 skill,秒上手 |
| **M2+** | 暴露 `ccteam-mcp` MCP server(`ls` / `show` / `new` / `peek` 作 structured tool) | 你的 claude 通过 MCP 调,比 shell parse 鲁棒 |

skill / MCP 装一次,所有 claude session 都能用。

### 6.2 三种入口对比

| 入口 | 何时合适 |
|---|---|
| `ccteam new "..."` CLI | 你已经想清楚了,一句话就能讲明白 |
| 你自己的 claude session(本节) | 多轮澄清、跨项目汇报、综合判断后立项 |
| Telegram bot(M1+) | 出门在外,只能动手机 |

三个入口最终都写到同一份 `~/.ccteam/inbox/`,orchestrator 不知道也不需要知道是哪个入口提的。

---

## 7. 定制每个 phase 的工作流

ccteam 的 9 个 phase 是写死的(00-seed → 09-ship),但**每个 phase 内调谁、配什么角色、串什么外部 plugin,完全可定制**。

### 7.1 配置文件位置

| 位置 | 作用 | 谁改 |
|---|---|---|
| `~/.ccteam/phases/*.md` | ccteam 自带的 9 个 phase 默认模板 | 高级用户编辑改默认行为 |
| `~/projects/<slug>/.ccteam/phases/*.md` | 项目级覆盖,优先于默认(M2+) | plan-eng phase 自动按 spec 复杂度生成 |
| `~/.ccteam/config.yml` | 全局上限:`max_concurrent_projects` / `max_subagents_per_phase` / `hard_cost_kill_per_project_usd` | 用户偶尔改 |

### 7.2 phase 模板长什么样

```yaml
# ~/.ccteam/phases/03-implement.md
---
name: implement
required_inputs:                  # 必读上游产物
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
required_outputs:                 # 必产出物;缺则不视为 phase_done
  - src/**/*
  - .ccteam/implement-report.md
parallelism: agent_team           # solo | agent_team | multi_session
agent_team:                       # ↑ phase 内多角色议事
  - role: backend-dev
  - role: frontend-dev
  - role: reviewer
sub_skills:                       # ↑ phase 边界自动调外部 plugin
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done
    output_to: .ccteam/code-review.md
---

# 任务
(prompt body...)
```

| 字段 | 管什么 | 何时触发 |
|---|---|---|
| `parallelism` + `agent_team` | **同一 phase 内多角色协同**(架构师/批评家/安全一起议事) | phase 执行中 |
| `sub_skills` | **phase 边界调外部 plugin**(review 跑 pr-review-toolkit、security 跑 security-guidance) | `phase_start` 注入前 / `phase_done` 后 |

### 7.3 编排灵活度

**灵活的**:
- 每 phase 自由组 agent_team 角色
- sub_skills 任意挂载,三档引用粒度:
  - `claude-plugins-official:<plugin>/<path>` — 零安装,直接 `@文件引用`
  - `local:<path>` — 拷贝到项目,冻结版本
  - `installed:<plugin>/<command>` — 整 plugin 安装(M2+)
- multi_session 模式下子模块各自完整 9-phase 流(M3+)

**不灵活的(刻意)**:
- 9 个主干 phase 写死,不让插自定义 phase——避免每项目状态机长得不一样
- `parallelism` 三档枚举,不支持自定义并行模式
- phase 之间是线性的;分支只发生在 fix-loop 内和 plan-eng 选并行档

### 7.4 与 Claude Code 原生格式的关系

phase 模板 = **markdown + YAML front matter**,与 Claude Code 原生 plugin / skill / agent 同种格式。`sub_skills` 引用的就是 `@~/.claude/plugins/marketplaces/.../code-architect.md` 之类原生 `@文件引用`。

> 编排骨架(9-phase 线性 + 三档并行)由 ccteam 强约束,血肉(每 phase 调谁、配什么角色、引哪个 plugin)100% 用 Claude Code 原生格式。

完整字段定义见 [interfaces.md §5](./interfaces.md#5-phase-模板-schema)。

---

## 8. 长跑 / 多项目

```bash
ccteam start --daemon                # 守护模式
ccteam stop                          # 优雅停机(保留 tmux session)
ccteam stop --kill-sessions          # 同时关 session(慎用)

ccteam new "想法 A"
ccteam new "想法 B"
ccteam new "想法 C"
# orchestrator 按 max_concurrent_projects=3 排队,不打扰

ccteam memory ls                     # 跨项目记忆(M3+)
ccteam memory rebuild
```

**Telegram(M1+)**:配过 token 后,在 telegram 给 bot 发"做个 X" → bot 自动 `ccteam new`;团队卡住或交付时 bot 推送 `⚠️ X 卡住了,需决策` / `✅ X 已交付`。

---

## 9. 团队卡住时

orchestrator 永不主动 kill 长 session,只软告警。撞到这些情况会主动找你:

| 情况 | 你看到什么 |
|---|---|
| 5 min 无 hook 事件 | 桌面通知 / telegram:"看起来卡了,要不要 attach 看看?" |
| 15 / 30 min 持续 | 告警升级,但仍不 kill |
| fix-loop 撞 3 次顶 | escalate:附三次诊断 + capture-pane 快照,等你决策 |
| 累计 cost > $200 | **唯一会强停的情况**(物理上限) |

你的应对:
```bash
ccteam attach <slug>      # 进去看,直接键入纠偏
ccteam pause <slug>       # 暂停自动调度
ccteam resume <slug>      # 想清楚后恢复
ccteam kick <slug>        # 软重启 session(claude --resume)
ccteam reject <slug>      # 放弃这个项目
```

或者更省事:开你自己的 claude,问 "bookmark-mgr-a3f9 怎么了,该怎么救",让它综合 progress.jsonl + capture-pane 给你一句 attach 后可贴的纠偏 prompt(详见 §6)。

---

## 10. 升级 / 卸载

```bash
brew upgrade ccteam                  # binary 覆盖
ccteam doctor                        # 体检 + 报告 phase 模板 drift
                                     # 嵌入资源 hash 不一致时问:overwrite / merge / keep
                                     # config.yml / memory / queue 永远保留
```

```bash
ccteam stop --kill-sessions
brew uninstall ccteam
rm -rf ~/.ccteam                     # 干净卸载(项目目录 ~/projects/ 不动)
```

---

## 11. 常见问题

**Q: ccteam 会和我自己的 Claude Code 冲突吗?**
不会。ccteam 起的 tmux session 命名都带 `ccteam-` 前缀,和你直接 `claude` 起的进程互不干扰。你自己的 claude 装 `ccteam-control` skill 后只是多了"知道怎么调 ccteam CLI"这个能力。

**Q: 我能改 9 个 phase 的顺序或加新 phase 吗?**
不能,这是刻意的——状态机一旦自由化,orchestrator 就崩了。但每个 phase 内你可以任意配 agent_team / sub_skills(§7)。

**Q: 没装 telegram 也能用吗?**
能。M0 没有 telegram,所有交互通过 CLI(或你自己的 claude)。M1+ telegram 是可选入口,不是必需。

**Q: 我不信任 `--dangerously-skip-permissions`?**
理解。ccteam 默认开它是为了消灭弹窗(痛点 8),前提是项目跑在隔离的 `~/projects/<slug>/` 目录里。如果不放心,可以在 `~/.ccteam/config.yml` 里关掉(代价是 phase 推进会被频繁的 permission 弹窗打断,基本失去无人值守能力)。

**Q: 卡住了能直接看 claude 在想什么吗?**
能,`ccteam attach <slug>` 进去看到的就是原生 Claude Code 终端,模型的 thinking、tool call、输出全可见。

**Q: ccteam 跟 Warp / iTerm2 怎么集成?**
不需要集成。Warp / iTerm2 / Alacritty 等是用户终端选择,`ccteam attach` 在任何 tmux 兼容终端里行为完全一致——ccteam 自己只输出标准 tmux 协议,不感知终端 emulator。
