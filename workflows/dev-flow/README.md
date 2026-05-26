# dev-flow — Holon-inspired 4-bot dev loop

> 4 个长跑 chat bot(pm / dev / reviewer / ops)在同一 IM 群里通过
> @-mention 协作,把"用户提需求 → 写代码 → 评审 → 合并 → 巡检"这条
> holon-style 闭环跑通,**全部用 ccteam V0.6.8 现有原语,无新 substrate**。
>
> 跑起来后预期暴露的 UX 痛点直接列在 §四,作为 V0.7 架构升级输入。
> 这正是用户指导思想 — **从用户体验触发,驱动架构升级**。

---

## 一、这是什么

借鉴 [Holon](https://github.com/holon-run/holon) 的多 agent 协作模式,
但**不复刻 holon runtime**:dev-flow 是一份 ccteam workflow 模板,
跑在 ccteam V0.6.8 的 `mode: chat` 上,4 个 chat-squad bot 协作。

```
                ┌─────────────────────────────────────────────┐
                │       IM group(Telegram / Slack / ...)    │
                │                                             │
   user ─────@pm─┤  ←  (intake / 派活 / 进度报告)             │
                │       │ @-mention via daemon mpsc (F193)    │
                │       ▼                                     │
                │     @dev  ── git worktree + impl + gh pr ──▶│
                │       │                                     │
                │       ▼ @-mention                           │
                │     @reviewer ─ gh pr view + merge ────────▶│
                │       │                                     │
                │       ▼ @-mention(回报 pm)                │
                │     @pm ─→ 用户                            │
                │                                             │
                │     @ops ← scan-on-demand from anyone       │
                └─────────────────────────────────────────────┘

  状态平面(.ccteam/ filesystem):
    backlog/<id>.md          pm 写,dev 读
    prs/<num>.json           dev 写,reviewer 更新,ops 扫
    ops-reports/<ts>.md      ops 写,pm 读
    progress.jsonl           ccteam orchestrator 写,ops 扫
```

### 与 holon 4-agent 的对位

| holon agent | dev-flow bot | 差距 |
|---|---|---|
| holon-pm (TUI 聊) | pm (IM chat,V0.6.8 F193 + F172 V2) | UX 同构 |
| holon-developer | dev (chat bot + git worktree + gh) | dev 没有 hold context 等 CI 的等待原语 |
| holon-reviewer | reviewer (chat bot + gh pr view/merge) | 不接 GitHub webhook,只能轮询 |
| holon-ops (cron) | ops (chat bot,**手动 @ 触发**) | chat mode 无 schedule trigger |

### 与 ccteam qa-autoloop 的差异

| | qa-autoloop | dev-flow |
|---|---|---|
| mode | artifact-driven (mode 2 bg) | chat (mode 3) |
| spawn 形式 | 文件 marker drop 触发 fresh-spawn | 长跑 tmux session,@-mention 唤醒 |
| 跨 agent 通信 | `.ccteam/triggers/<role>/<id>.marker.json` | `@<handle>` daemon mpsc(F193) |
| context 跨 turn | ❌ 每 spawn fresh 1M | ✅ 长跑累积(F172 V2 lossless resume) |
| 用户接入 | CLI `ccteam spawn planner` | IM `@pm 我想要 X` |
| 状态可见性 | grep `.ccteam/` | IM 群里直接看 4 bot @ 对话 |

dev-flow **不取代** qa-autoloop — 后者仍适合"无人值守 / 文件驱动 /
overnight 跑"场景。两者用途不同。

---

## 二、装机步骤

### 0. 前置

- ccteam V0.6.8 或更新 (`ccteam --version`)
- 一个 GitHub repo(dev/reviewer 走 `gh` CLI 操作 PR)
- 一个 IM 平台已配置(Telegram / Slack / Discord 都行,走
  `/ccteam-im-setup` 或 `/telegram:configure`)
- `gh auth status` 已登录,token 含 `repo` + `workflow` scope
- 项目根 `.gitignore` 包含:
  ```
  .ccteam/
  .ccteam-worktrees/
  ```

### 1. 选目标项目

⚠️ **不要在 ccteam repo 自己跑 dev-flow**(CLAUDE.md §六 红线:循环
引用排错地狱)。用一个 sibling project:你自己别的活 repo,或者新
开一个空 demo repo。

```bash
cd /path/to/target-project
```

### 2. 复制模板

```bash
CCTEAM_REPO=/path/to/ccteam        # 你 clone 的 ccteam repo

mkdir -p .ccteam .claude/agents
cp $CCTEAM_REPO/workflows/dev-flow/workflow.yaml         .ccteam/workflow.yaml
cp $CCTEAM_REPO/workflows/dev-flow/config.example.json   .ccteam/config.json
cp $CCTEAM_REPO/workflows/dev-flow/agents/*.md           .claude/agents/
```

### 3. 填 config.json

打开 `.ccteam/config.json`,把所有 `TODO-*` 字段替换成项目实际值:

- `name` / `local_path`
- `github.{owner,repo,fix_base_branch,fix_branch_prefix,merge_strategy}`
- `test.pre_pr_commands`(数组,逐条是 shell 命令)
- `worktree.base_dir`(默认 `.ccteam-worktrees`,gitignore 别忘加)
- `ops.{stuck_pr_hours,fix_attempts_warn,cost_spike_pct}`
- `im.platform`(信息用,token 走 IM-setup skill)

### 4. 填 workflow.yaml

打开 `.ccteam/workflow.yaml`,把 `chat_acl.allow_groups` 里的
`TODO-replace-with-im-group-chat-id` 改成实际 IM 群的 chat_id:

```yaml
chat_acl:
  allow_groups:
    - "-1001234567890"    # Telegram group id (负数;别忘双引号)
```

不填 ACL = 全开放,生产强烈建议填。

### 5. 配 1 个 bot token + 注册 4 个角色映射

⚠️ **只要 1 个 Telegram/Slack bot,不是 4 个。** 4 个角色共用群里那
1 个 bot:入站靠 daemon 解析 `@handle` 文本路由,出站靠 F199
`from <handle>:` 前缀区分发言者。完整说明 + step-by-step 见
[USAGE.md §1.2 + §2.5-2.6](./USAGE.md)。

```bash
# (a) token 配一次,存 ~/.ccteam/im/credentials.json(走 /telegram:configure
#     skill,或手写 {"telegram":{"bot_token":"...","allowed_chat_ids":[...]}})

# (b) 注册 4 个角色的路由映射(不碰 token,4 次同一 chat-id):
SLUG=<project-slug>; CHAT_ID=-1001234567890
for ROLE in pm dev reviewer ops; do
  ccteam admin register-bot --slug "$SLUG" --role "$ROLE" \
    --vendor claude --platform telegram --chat-id="$CHAT_ID" \
    --chat-handle "$ROLE" --project-dir "$(pwd)"
done

# 确认:
ccteam admin list-bots --slug "$SLUG"
```

(V0.6.8 `ccteam admin register-bot` / `list-bots` 是 CLI 端点;
`register-bot` 不收 token —— token 在 (a) 单独配。)

### 6. 启动

```bash
cd /path/to/target-project
ccteam start
```

ccteam 起 4 个 tmux session(`ccteam-chat-<slug>-{pm,dev,reviewer,ops}`),
4 个角色共用群里那 1 个 IM bot(入站 @handle 路由 + 出站 from-prefix),
在群里待命。

### 7. 试一下

在你的 IM 群里发:
```
@pm 我想要一个 README 的中文翻译
```

预期序列:
1. **pm** 跟你来回澄清(用什么风格 / 哪些段保留英文等)
2. **pm** echo 方案给你,你回 "go"
3. **pm** 写 `.ccteam/backlog/<id>.md` + `@dev` 派活
4. **dev** 切 worktree + 写翻译 + 跑 `pre_pr_commands` + `gh pr create` + `@reviewer`
5. **reviewer** 看 diff + `gh pr view` 看 CI + 合或 request-changes
6. **pm** 收到 reviewer @ 后回报你 "✓ PR #N merged"

---

## 三、能跑通的丝滑场景 + 跑不通的卡点

### ✅ 能丝滑跑通的

- 用户 IM @pm 提需求 → backlog → dev → PR → reviewer → merge → pm 回报
  (本地校验过、CI 当场跑完、reviewer 一次过的情况)
- dev 撞墙时 @pm + @ops 接力诊断
- reviewer request-changes → dev 改 1-3 次 → merge
- 用户随时 `@pm status` / `@ops scan` 拿状态

### ⚠️ 跑不通需要 workaround 的

- **CI 异步完成不会自动唤醒 reviewer** —— 见 §四 痛点 A
- **PR 上人类的新 comment 不会自动唤醒 dev** —— 见 §四 痛点 A
- **ops 定时巡检** —— chat mode 无 schedule trigger,见 §四 痛点 B
- **backlog 优先级** —— 用户给 3 个需求,dev 怎么排?现状 FIFO,见 §四 痛点 D
- **PR 提了之后 reviewer 长时间没动** —— pm/用户得手动 @reviewer ping
- **长 turn 撞 watchdog**(F195 默认 90s,dev 跑大 cargo test 容易超)—
  config 里把 `chat.turn_timeout_sec` 加大

---

## 四、跑起来暴露的 UX 痛点 → V0.7 substrate 升级路线

跑 dev-flow 暴露的痛点,几乎 1-to-1 对应 holon 那 6 个 runtime 原语
(详 `references/holon-multi-agent-workflow-borrow.md`)。**dev-flow
是 V0.7 的 driver,不是 V0.7 的产出**。

### 痛点 A:GitHub 事件不接 ⚠️ **load-bearing**

**症状**:
- dev push 后,CI 跑 8 分钟才出结果。但 reviewer turn 早就结束了,
  没人通知 reviewer "CI 完事了来评审"。最早能再唤醒 reviewer 的时机
  是下次 `@ops scan` 跑(用户手动触发)发现这个 PR 然后 @reviewer。
- 人在 GitHub PR 上评论,dev 永远不知道。

**当前 workaround**(README 装机版可用):
- ops scan 兜底,但延迟取决于用户触发频率
- reviewer 提前预判 "CI 在跑,我 stop,@dev 再 ping 我"

**V0.7 substrate**:`ExternalTriggerCapability` per-agent + `WaitingIntent`
- 每个 bot 注册一个 stable URL: `/ccteam/callbacks/wake/<token>`
- GitHub Repo Settings 加 webhook → 这个 URL
- webhook 到 → daemon 看 payload 找 PR # → 查 `.ccteam/prs/<num>.json::pr_url`
  → @ 对应 bot
- WaitingIntent 落盘 `<project>/.ccteam/agents/<role>/waiting/<id>.json`
- 见 `references/holon-multi-agent-workflow-borrow.md §六 B2 + 六 B1`

### 痛点 B:ops 没有定时巡检

**症状**:
- 用户不 @ops 就不跑,健康监控完全靠人。
- 想"每 6h 自动巡检"得在 host 层 crontab 跑 `ccteam admin send-message`,
  装机麻烦。

**当前 workaround**(注意:`ccteam admin send-message` 不存在,要绕 ccteam
直接调 IM Bot API):
```bash
# Telegram 示例 — host 层 crontab,不依赖任何 ccteam 命令
0 */6 * * * curl -s "https://api.telegram.org/bot$TOKEN/sendMessage" \
  -d chat_id=<group-id> -d text="@ops scan"
```
缺点:token 暴露 crontab + cron 要知道 IM 平台细节。详 `agents/ops.md`
的 §V0.7 占位段。

**V0.7 substrate**:`mode: chat` 开 `trigger: schedule` + `schedule:` 字段
- F142 的 cron 调度器已经写好(artifact-driven mode 在用);只是
  `WorkflowMode::Chat` 的 `validate()` 当前没允许 schedule trigger
- 改 `crates/ccteam-core/src/workflow.rs::validate()` 让 chat agents
  可以挂 `trigger: schedule`
- 估算 ~200 LOC + tests

### 痛点 C:状态可见性

**症状**:
- 用户问 `@pm status`,pm 跑 `ls .ccteam/backlog/ | wc -l` + `ls .ccteam/prs/ | wc -l`
  汇报。**没有视图,只有 grep**。
- 多个 backlog item 在不同状态(draft / dev / review / blocked / done),
  IM 端看不到 dashboard。

**当前 workaround**:`@pm status` + ops 报告 + `ccteam-web` 4 面板(但
没有 backlog/PR 视图,只有 workflow/agent 视图)

**V0.7 substrate**:
- `WorkItem` 数据模型(借 holon)落盘 `.ccteam/agents/<role>/work-items/<id>.json`
- ccteam-web 加 backlog / PR / ops-report 三个新面板
- IM 端 `/status` 命令出 ASCII 表格(限 IM 字符)
- 见 `references/holon-multi-agent-workflow-borrow.md §六 B3 + 六 B5`

### 痛点 D:backlog 没有优先级排序 + readiness

**症状**:
- 用户给 5 个需求,dev 该先做哪个?现状按 backlog 文件名时间 FIFO。
- 某需求被 reviewer 打回,dev 该先修这个还是接新活?现状 dev 自己判断。
- 用户在 IM 改主意:"先做需求 3,需求 1 缓一下" — 没法标 priority。

**当前 workaround**:pm 跟用户敲定优先级时,人为按时间编号 backlog
(`20260101-001-X` / `20260102-001-Y`)。

**V0.7 substrate**:WorkItem 加 `priority` + `readiness()` 派生态
- pm 创建 backlog 时设 priority (P0/P1/P2)
- dev 每次唤醒选 priority 最高的 runnable item
- 见 `references/holon-multi-agent-workflow-borrow.md §六 B3`

### 痛点 E:fix-loop 撞顶 trace 丢失

**症状**:
- dev 撞 fix_attempts=3 后写 `.ccteam/prs/<num>.json::status=escalated`,
  IM 里 @pm + @ops 一句话。但具体每次失败 reviewer 提了啥、dev 改了啥,
  全在 progress.jsonl 散落 turn 之间,人工 grep 累。

**当前 workaround**:ops 手 grep。

**V0.7 substrate**:`Brief` envelope 分级(`Result/Failure/Ack` +
`OperatorVisibility`)+ escalation payload schema
- 见 `references/holon-multi-agent-workflow-borrow.md §六 B5`

### 痛点 F:long-lived bot 资源占用

**症状**:
- 4 个 tmux session 24/7 跑,即使空闲也吃 baseline RAM。
- 团队规模扩大(加 qa / design / data 角色)→ N 个 session 撑爆

**V0.7 / V0.8 substrate**:
- `embedded-mux-unified-architecture.md` 的 rmux 改造 ── 4 个 bot 通过
  同一 mux daemon 共享 PTY 资源池
- bot idle 时 daemon 可以 hibernate(暂停 tmux,丢 state.json,@-mention
  到再唤醒)

---

## 五、不在本模板范围内(避免 scope creep)

- **GitHub webhook 接入** —— V0.7 substrate,本模板的 ⚠️ 痛点 A
- **WorkItem 数据模型** —— V0.7 substrate,本模板用纯文件 backlog 模拟
- **chat mode schedule trigger** —— V0.7 substrate,本模板 ops 手动 @
- **HumanApproval 4 号 mode 接入** —— 想要"每个 merge 必须 user approve"
  的话,在 reviewer.md::Step 3.5.a 之前加 plan_approval gate,见
  workflow.yaml 注释里 reviewer 段
- **Codex vendor** —— 本模板 dev/reviewer 默认 Claude executor。
  reviewer 想用 codex(独立模型避同质 bias)就把 `executor: codex` 加上,
  但 codex chat mode 走 `codex app-server` UDS,UX 细节自验
- **ccteam-creator preset 化** —— V0.7 可以加 `ccteam-creator dev-flow`
  preset 一键 init,目前手工 cp + 填 TODO

---

## 六、调试 / 故障排查

| 症状 | 看哪里 |
|---|---|
| bot 没回话 | `tmux ls \| grep ccteam-chat`(session 在不?)→ `ccteam doctor` |
| @-mention 没路由到对的 bot | `ccteam admin list-bots --slug <slug>` 看 chat_handle 注册;daemon 路由日志在 `ccteam start` 的前台终端 stderr |
| dev 跑 `gh pr create` 报权限 | `gh auth status` 看 scope;`workflow` scope 缺会卡 `.github/workflows/*.yml` 改动 |
| backlog 文件写了但 dev 没动 | dev 不被文件 marker 唤醒,**只能靠 @dev**;pm 派活时记得 @dev |
| ops scan 报"progress.jsonl 缺失" | ccteam orchestrator 没跑,先 `ccteam start` |
| watchdog timeout(turn 超时) | `chat.turn_timeout_sec` 加大,默认 180s 对大 cargo test 不够 |
| 群里 @handle 路由不到角色 | `ccteam admin list-bots --slug <slug>`;F184 unknown-handle UX 修过,但群 chat_id 填错也会这样 |

---

## 七、给下一个接手 dev-flow 的 session

1. **不要把 dev-flow 装在 ccteam repo 自己上**(CLAUDE.md §六)
2. **不要给 dev-flow agent.md 加版本号 / F-tag**(CLAUDE.md §三 skill 自洽红线;agent.md 走类似哲学)
3. **添加新 bot 角色?**(qa / design / data 等)→ 加 `chat_handle` + 进 squad_roster.rs 的注入列表;ccteam-creator skill Phase 5.4 会自动渲染队友清单
4. **改 bot 行为?**不要直接动 workflow.yaml,改 `.claude/agents/<role>.md`
5. **跑 dogfood 实验时**:用 sibling test repo,**别**在 production code repo 实验
6. **暴露新痛点?**写到 §四 表格,作为 V0.7 PRD 输入,而不是改本模板 patch
