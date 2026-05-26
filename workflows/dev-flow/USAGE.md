# dev-flow 用户手册

> dev-flow 让你**在 IM 群里跟 4 个 AI bot 协作开发**:你提需求,
> pm 接待,dev 写代码 + 开 PR,reviewer 合并,ops 健康巡检。
> 全程在你已有的 Telegram / Slack / Discord 群里完成。
>
> **设计目标**:把"需求 → 实现 → 评审 → 合并"这条 holon 式闭环
> 装到 ccteam V0.6.8 的 mode 3 chat substrate 上,跑起来后用真实
> UX 痛点驱动 V0.7 架构升级。
>
> **本文档 = step-by-step user 视角操作手册**;架构 / why / V0.7
> 路线图见 [README.md](./README.md)。

---

## §0 5 分钟极速预览

```
你在 IM 群:     @pm 我想给项目加 dark mode 切换
                ↓
pm:             好的,我了解一下背景 — 现在有 light mode 吗?用什么
                CSS 方案?用户期望保留偏好到 localStorage 吗?
                ↓
你:             有 light mode,CSS variables;偏好要保留。
                ↓
pm:             方案:加 ThemeToggle 组件,用 CSS variables 切
                <body> 上的 data-theme,偏好存 localStorage。验收:
                (1)切换按钮可见 (2)颜色立即更新 (3)刷新后保留
                偏好。go?
                ↓
你:             go
                ↓
pm:             已派 @dev,backlog id = 20260527-dark-mode-toggle
                ↓
dev:            (后台 30 分钟)切 worktree → 写组件 → 跑测试 → 开 PR #42
                @reviewer 请评审 PR #42(backlog/20260527-dark-mode-toggle)
                ↓
reviewer:       (10 分钟)看 diff + 跑 CI → @dev cleanup PR #42 的 worktree
                @pm PR #42 已 merged
                ↓
pm:             @你 ✓ PR #42 已 merged(dark mode 切换上线了)
```

**你做的:写 2 条 IM 消息**(初始需求 + go)。
**你看到的:全程在 IM 群里看 bot 之间对话**,无需 grep 文件。

---

## §1 前置准备

### 1.1 必装

| 工具 | 用途 | 检查命令 |
|---|---|---|
| ccteam V0.6.8+ | orchestrator + chat substrate | `ccteam --version` |
| `gh` CLI | dev/reviewer 开 PR / 评审 / 合并 | `gh auth status`(token 需 `repo` + `workflow` scope) |
| `jq` | agents 内多处用到 | `jq --version` |
| `git` 2.30+ | worktree 操作 | `git --version` |
| Claude Code | bot 跑的 harness | `claude --version` |

### 1.2 IM 平台 + 4 个 bot token

你要跟 4 个独立 bot 对话 — 每个 bot 一个独立 token,在同一 IM 群
里出现 4 个机器人头像。

**Telegram(最推荐,token 申请 60 秒)**:

1. IM 里搜 `@BotFather` → `/newbot` → 起名 → 拿 token
2. 重复 4 次拿 4 个 token,分别给 pm / dev / reviewer / ops
3. 把 4 个 bot 都拉进你的目标 IM 群,设成 admin(能读所有消息)
4. 拿群的 `chat_id`(负数,可在 [@RawDataBot](https://t.me/RawDataBot) 拉到)

**Slack**:每个 bot 走独立 Slack App,bot token + bot user OAuth scope `chat:write` `channels:history` `app_mentions:read`。

**Discord**:每个 bot 一个独立 Application + Bot,启 `MESSAGE CONTENT INTENT`。

### 1.3 目标项目 + sibling 隔离

⚠️ **不要在 ccteam repo 自己跑 dev-flow**(CLAUDE.md §六 红线:
循环引用排错地狱)。用一个 sibling project — 你自己别的 repo,
或新开一个空 demo repo 做练手。

```bash
# 推荐:先试一个空 repo 把流程跑通
gh repo create dev-flow-demo --public --clone
cd dev-flow-demo
echo "# dev-flow demo" > README.md
git add . && git commit -m "init" && git push -u origin main
```

---

## §2 安装(一次)

### 2.1 复制模板

```bash
CCTEAM_REPO=~/workplace/ccteam     # 改成你 clone 的 ccteam 路径
cd /path/to/your-target-project    # cd 到目标项目根

mkdir -p .ccteam .claude/agents

cp $CCTEAM_REPO/workflows/dev-flow/workflow.yaml         .ccteam/workflow.yaml
cp $CCTEAM_REPO/workflows/dev-flow/config.example.json   .ccteam/config.json
cp $CCTEAM_REPO/workflows/dev-flow/agents/pm.md          .claude/agents/pm.md
cp $CCTEAM_REPO/workflows/dev-flow/agents/dev.md         .claude/agents/dev.md
cp $CCTEAM_REPO/workflows/dev-flow/agents/reviewer.md    .claude/agents/reviewer.md
cp $CCTEAM_REPO/workflows/dev-flow/agents/ops.md         .claude/agents/ops.md
```

### 2.2 填 config.json

用编辑器打开 `.ccteam/config.json`,**把所有 `TODO-*` 字段全部
替换**。最少必填的字段:

```json
{
  "name": "your-project-slug",
  "local_path": "/absolute/path/to/your-project",
  "github": {
    "owner": "your-gh-org-or-user",
    "repo": "your-repo-name",
    "fix_base_branch": "main",
    "fix_branch_prefix": "feat/dev-flow-",
    "merge_strategy": "squash"
  },
  "test": {
    "pre_pr_commands": [
      "cargo fmt --all -- --check",
      "cargo clippy --all-targets -- -D warnings",
      "cargo test --workspace"
    ]
  },
  "worktree": {
    "base_dir": ".ccteam-worktrees",
    "cleanup_after_merge": true
  },
  "ops": {
    "stuck_pr_hours": 24,
    "fix_attempts_warn": 3,
    "cost_spike_pct": 50,
    "report_dir": ".ccteam/ops-reports"
  },
  "im": {
    "platform": "telegram",
    "group_handle_hint": "my-team-room"
  }
}
```

**`pre_pr_commands` 填什么?**项目类型决定:

- **Rust workspace**:`["cargo fmt --all -- --check", "cargo clippy --all-targets -- -D warnings", "cargo test --workspace"]`
- **Node + TypeScript**:`["npm run lint", "npm run typecheck", "npm test"]`
- **Python + pytest**:`["ruff check .", "mypy .", "pytest"]`
- **Go**:`["go vet ./...", "go test ./..."]`

dev 跑这些命令在 worktree 里,**全过才开 PR**。

### 2.3 填 workflow.yaml(只改 ACL)

打开 `.ccteam/workflow.yaml`,**只改一处** — `chat_acl.allow_groups`:

```yaml
chat:
  ...
  chat_acl:
    allow_groups:
      - "-1001234567890"   # 改成你的实际 IM 群 chat_id(Telegram 是负数,记得双引号)
```

**不填 ACL = 任何人 @bot 都能调用 = 生产环境强烈不推荐**。本机
测试可暂时全开放,生产必填。

其它字段(`turn_timeout_sec` / `hop_limit` / `compact_every_turns`)
默认值适合大多数项目,实际跑过一周再调。

### 2.4 加 .gitignore

在目标项目根的 `.gitignore` 加 2 行:

```gitignore
# ccteam dev-flow
.ccteam/
.ccteam-worktrees/
```

`.ccteam/` 是本地 orchestration state(backlog / PR 跟踪 /
progress.jsonl);`.ccteam-worktrees/` 是 dev 临时工作树。**两者
都不应入 git**。

### 2.5 注册 4 个 bot

每个 bot 一次 `ccteam admin register-bot`。

> **⚠️ Telegram chat_id 是负数**(super-group / channel 形如
> `-1001234567890`)。ccteam CLI 在 V0.6.8 patch 之前未在
> `--chat-id` 上声明 `allow_hyphen_values`,负数会被 clap 误认
> 为 flag → `error: unexpected argument '-1' found`。
> V0.6.8 patch 起已修;**老版本** workaround:用 `--chat-id=-1001234567890`
> 等号紧贴形式(不要 `--chat-id "-1001..."` 空格分隔)。

```bash
cd /path/to/your-target-project
SLUG=$(jq -r '.name' .ccteam/config.json)
CHAT_ID="-1001234567890"   # 同 workflow.yaml 里的群 chat_id

# pm
ccteam admin register-bot \
  --slug "$SLUG" \
  --role pm \
  --vendor claude \
  --platform telegram \
  --chat-id "$CHAT_ID" \
  --chat-handle pm \
  --project-dir "$(pwd)"

# dev
ccteam admin register-bot \
  --slug "$SLUG" \
  --role dev \
  --vendor claude \
  --platform telegram \
  --chat-id "$CHAT_ID" \
  --chat-handle dev \
  --project-dir "$(pwd)"

# reviewer
ccteam admin register-bot \
  --slug "$SLUG" \
  --role reviewer \
  --vendor claude \
  --platform telegram \
  --chat-id "$CHAT_ID" \
  --chat-handle reviewer \
  --project-dir "$(pwd)"

# ops
ccteam admin register-bot \
  --slug "$SLUG" \
  --role ops \
  --vendor claude \
  --platform telegram \
  --chat-id "$CHAT_ID" \
  --chat-handle ops \
  --project-dir "$(pwd)"
```

每个 register-bot 命令之后,ccteam 会让你**把对应 bot 的 token
粘进 keychain**(走 `ccteam-im-setup` skill 已经接好的安全存储,
不进 config.json / .env)。

确认 4 个 bot 都注册成功:

```bash
ccteam admin ls
# 应该看到 4 行:pm / dev / reviewer / ops,全部 chat_handle 显示
```

### 2.6 启动 ccteam

```bash
cd /path/to/your-target-project
ccteam start
```

ccteam 起 4 个 tmux session(`ccteam-chat-<slug>-{pm,dev,reviewer,ops}`),
每个绑定一个 IM bot,在 IM 群里待命。

**确认在跑**:

```bash
tmux ls | grep "ccteam-chat-$SLUG"
# 应该 4 行,每个 session 一行
```

如果只看到 1-3 个 session → §6 故障排查。

---

## §3 第一次跑通("hello world")

在你的 IM 群里发:

```
@pm 在 README.md 末尾加一行: "Built with dev-flow."
```

### 3.1 你会看到的 IM 序列

(时间约 5-15 分钟,取决于项目大小和 model 速度)

**T+0**:你发了上面那条消息。

**T+10s**:`@pm` 回:

```
收到。这是一个很小的 1 行改动,直接做。我把它打包成 backlog:

  objective: 在 README.md 末尾加 "Built with dev-flow."
  acceptance:
    - README.md 最后一行是 "Built with dev-flow."
    - 没有其它文件被改
  scope: S
  constraints: 无

go ?
```

**T+30s**:你回 `go`。

**T+1min**:`@pm` 回:

```
已派 @dev,backlog/20260527-093015-builtin-readme
@dev 请实现 backlog/20260527-093015-builtin-readme。要点:在 README.md 末尾加一行。
完成后请 @reviewer 评审。
```

**T+3min**(dev 在后台跑 worktree + 改文件 + 跑 pre_pr_commands + 开 PR):
`@dev` 回:

```
@reviewer 请评审 PR #1(backlog/20260527-093015-builtin-readme)。
本地校验全过,https://github.com/you/dev-flow-demo/pull/1

@pm 已开 PR #1,backlog/20260527-093015-builtin-readme
```

**T+5min**(reviewer 在后台跑 gh pr view + 看 diff + merge):
`@reviewer` 回:

```
@dev cleanup PR #1 的 worktree
@pm PR #1 已 merged(backlog/20260527-093015-builtin-readme 完成)
```

**T+5min30s**:`@pm` 回给你:

```
@你 ✓ PR #1 已 merged(README "Built with dev-flow." 加上了)
```

### 3.2 验证

```bash
cd /path/to/your-target-project
git pull origin main
tail -1 README.md
# 应该看到:Built with dev-flow.

gh pr list --state merged --limit 1
# 应该看到 PR #1
```

如果跑通了 → 进 §4 日常用法。
如果卡住 → §6 故障排查。

---

## §4 日常用法 cheatsheet

### 4.1 提需求(最常用)

```
@pm <你想要的功能 / 想修的 bug>
```

pm 会:
1. 跟你来回澄清(目标 / 验收 / 范围 / 约束),直到方案敲定
2. echo 完整方案让你确认,你回 `go` / `yes` / `干` / `approve` 才走
3. 写 backlog 文件 + `@dev` 派活
4. 之后 dev / reviewer 进展自动转告

**用户消息能多自由?**完全自由:`@pm 想要 X` / `给我加 Y` / `@pm 这块卡住了帮我看下` / `@pm 重构这个模块` 都行。pm 内部会做归类。

### 4.2 查 status

```
@pm status
```

或

```
@pm 进度
```

pm 会汇报:
- backlog open 多少
- PR in-flight 多少(链接给你)
- 最近一次 ops 报告

### 4.3 健康巡检

```
@ops scan
```

或

```
@pm /health
```

(`@pm /health` 会让 pm 自动 `@ops` 你不必直接 @ ops。)

ops 扫:
- `.ccteam/progress.jsonl`:fix-loop 撞顶 / turn 间隔异常 / cost 飙升
- `gh pr list`:stuck PR(>24h 没动)/ CI 红
- 写报告到 `.ccteam/ops-reports/<ts>.md`,headline 摘要给你

### 4.4 紧急停止某 PR

```
@reviewer 关掉 PR #42,原因: <你的理由>
```

reviewer 会 `gh pr close 42` 并更新 `.ccteam/prs/42.json::status =
closed`,然后 `@dev` 让他 cleanup worktree。

### 4.5 紧急停掉某 backlog item(还没开 PR 的)

直接编辑 backlog 文件:

```bash
ls .ccteam/backlog/
# 找到 id
$EDITOR .ccteam/backlog/<id>.md
# 改第一行 status: 从 open 改成 cancelled
```

下次 dev 唤醒时会看到 status=cancelled 跳过该项。

(理想 V0.7:`@pm cancel <id>` 一句话搞定;现状先改文件。)

### 4.6 修改某 bot 行为

bot 行为住 `.claude/agents/<role>.md`。改完文件 + `/clear` 那个
bot 即可:

```bash
# 改 dev 行为
$EDITOR .claude/agents/dev.md

# 让 dev 重新加载新 persona
ccteam admin change-persona --slug $SLUG --bot dev --new-persona-md - < .claude/agents/dev.md
```

或者直接在 IM 群里 @dev 说 "请 /clear 然后用新 persona" — bot 会
读到改动的 file 在下一 turn 生效。

### 4.7 暂停整个 workflow

```bash
ccteam stop $SLUG
```

4 个 tmux session 收到 SIGTERM 优雅退出。再 `ccteam start $SLUG`
恢复(F172 V2 `claude --resume <name>` lossless 续接,4 个 bot
带着 context 回来)。

### 4.8 完全卸载

```bash
ccteam stop $SLUG
ccteam admin unregister-bot --slug $SLUG --role pm
ccteam admin unregister-bot --slug $SLUG --role dev
ccteam admin unregister-bot --slug $SLUG --role reviewer
ccteam admin unregister-bot --slug $SLUG --role ops
rm -rf .ccteam .ccteam-worktrees
rm .claude/agents/{pm,dev,reviewer,ops}.md
```

bot token 在 IM 平台那边自己撤(BotFather `/revoke`)。

---

## §5 场景剧本(playbook)

### 5.1 场景 A:用户提模糊需求,pm 多轮澄清

**你**:`@pm 让 UI 更好看`

**pm**:
```
这个范围比较宽。能具体点?
1. 整体重 design(改色板 / 字体 / 间距)?
2. 某个具体组件(哪个)?
3. 响应式 / 移动端适配?
4. 暗色模式 / 主题切换?
```

**你**:`2,Header 那个组件,加 logo + 改字体 + 让导航 hover 有动效`

**pm**:
```
明确了。方案:
  objective: Header 组件迭代:加 logo / 改字体 / 导航 hover 动效
  acceptance:
    - Header 左侧出现 logo
    - 字体换成 <你期望的字体名,如果你之前没说我会问>
    - 导航 link hover 时有过渡动效(指明 duration)
  scope: M
  constraints:
    - 不改其它组件
    - 字体走 @font-face 而非 CDN(performance)
不过有 1 个问题没问到:换什么字体?
```

**你**:`Inter`

**pm**:
```
完整方案上面 + 字体 Inter。go?
```

**你**:`go`

→ 后续 dev/reviewer 流程同 §3.1。

### 5.2 场景 B:dev 提 PR 后 CI 红

**dev**(本地 pre_pr_commands 都过了,但 GitHub 上 CI 是更全的 matrix):

```
@reviewer 请评审 PR #15
```

**reviewer**(读 PR 看到 CI 红):

```
@dev PR #15 CI 红:
- check `e2e-firefox` failed,log: <链接>
- 看起来是 firefox 上 ResizeObserver polyfill 缺失
```

**dev**(进 fix-loop):

```
进 worktree:
  cd .ccteam-worktrees/dev-<id>
  npm install --save-dev resize-observer-polyfill
  <加 polyfill 引用>
  npm run test:e2e:firefox  # 验过
  git commit + push --force-with-lease
@reviewer PR #15 已根据 feedback 改第 1/3 轮,请再评审
```

### 5.3 场景 C:fix-loop 撞顶 3 次仍不过

**dev** 第 3 次改完 push,reviewer 还是发现新问题。

**reviewer**:

```
@dev PR #42 第 3 次了 acceptance.3 仍不满足:<具体>
```

**dev**(撞顶检测):

```
@pm @ops PR #42 撞顶(3 次修不过),需要人工介入。
最后一次失败 reason: 我对 acceptance.3 的理解可能跟 reviewer 不一致,
具体是 <详述差异>。
```

**pm**(转告你):

```
@<user> ⚠ PR #42 阻塞(撞顶 3 次):
- dev 和 reviewer 对 acceptance.3 解读不同
- dev 认为是 X,reviewer 认为是 Y
你倾向哪个?or 让 backlog 重写 acceptance?
```

**你做的决策**:
- `@pm 重写 acceptance,要求是 Y` → pm 改 backlog,再 `@dev` 重做
- `@reviewer 这次按 X 来,直接 merge` → reviewer 手动 approve + merge
- `@pm cancel #42,换方案` → 关掉 PR,重新提需求

### 5.4 场景 D:重启 ccteam 后所有 bot 恢复

电脑重启 / `ccteam stop && ccteam start`:

```bash
ccteam start $SLUG
```

4 个 bot 通过 `claude --resume ccteam-chat-<slug>-<role>` 拿回各自
最近 N=12 turn 的上下文(F172 V2 lossless),IM 群里发 `@pm
你还在吗?`,pm 应该带着昨天的 backlog 状态回复。

**不会丢的**:
- backlog / prs 文件状态(filesystem 控制平面)
- 每个 bot 最近 12 turn 对话(`compact_every_turns: 30` 之后会被压缩)
- progress.jsonl 历史

**会丢的**:
- 大 compaction 之前的细节对话(turns.jsonl 有归档但 bot 不读)
- 临时 worktree 如果重启时还在跑(用户得手动 `git worktree prune`)

### 5.5 场景 E:加新 bot 角色(qa)

想加一个 QA bot,跑 e2e 验收(dev 提 PR 后 reviewer 之前过一道):

1. 复制并改 `.claude/agents/qa.md`:
   ```bash
   cp .claude/agents/reviewer.md .claude/agents/qa.md
   # 改 frontmatter name,改 role 描述为 "跑 e2e 验收"
   ```
2. 编辑 `.ccteam/workflow.yaml`,在 `agents:` 段加:
   ```yaml
     qa:
       executor: claude
       trigger: manual
       max_parallel: 1
       chat_handle: qa
   ```
3. 注册新 bot:
   ```bash
   ccteam admin register-bot \
     --slug $SLUG --role qa --vendor claude \
     --platform telegram --chat-id $CHAT_ID \
     --chat-handle qa --project-dir "$(pwd)"
   ```
4. 改 dev.md 让 dev 完成后 `@qa` 而非直接 `@reviewer`,改 qa.md
   让 qa 完成 e2e 后 `@reviewer`
5. `ccteam stop && ccteam start` 让 daemon 重读 workflow.yaml

(理想 V0.7:`ccteam-creator` 支持 dev-flow + role-extend 一键化。)

### 5.6 场景 F:用户在 IM 急停某 PR 因为 production 突发

`@reviewer 紧急关 PR #88,production 出问题了`

reviewer 立即:
```bash
gh pr close 88
```
更新 `.ccteam/prs/88.json::status=closed_emergency`,`@dev cleanup`,
`@pm 通告` — 不评审,不解释,先关。

---

## §6 故障排查

### 6.1 bot 没回话

```bash
# Step 1: ccteam orchestrator 还在跑吗?
ps aux | grep ccteam | grep -v grep

# Step 2: 4 个 tmux session 都在吗?
tmux ls | grep "ccteam-chat-$SLUG"

# 缺少哪个 → 试单独重启:
ccteam start $SLUG --restart-bot <role>   # 如该子命令存在
# 或:
ccteam stop $SLUG && ccteam start $SLUG

# Step 3: 看 IM bridge log
tail -100 ~/.ccteam/logs/imd.log | grep -E "$SLUG|ERROR"

# Step 4: ccteam doctor
ccteam doctor
```

### 6.2 @-mention 没路由到对的 bot

```bash
ccteam admin ls
# 应该看到 4 行,chat_handle 全显示

# 用 V0.6.8 F184 unknown-handle UX:
# 你在 IM 群里 @ 一个不存在的 handle 时,daemon 回报哪些可用
```

如果 chat_handle 重复(两个 bot 都叫 `dev`,不同 slug):daemon 会
suffix collision 后者为 `dev__<slug>`(F183),你 @ 时要带 suffix。

### 6.3 PR 一直不合并(reviewer idle)

最可能:CI 异步还在跑,reviewer 看到 pending 就 stop 了 — **没人
唤醒他来 re-check**。

workaround:
```
@reviewer 看下 PR #N
```

或让 ops scan(它扫所有 open PR):
```
@ops scan
```

(V0.7 ExternalTriggerCapability 解决这个,见 README §四 痛点 A。)

### 6.4 dev 跑 cargo test 超时

F195 watchdog 默认 90s。改 workflow.yaml:

```yaml
chat:
  turn_timeout_sec: 600   # 10 分钟,够大 cargo test workspace
```

`ccteam stop && ccteam start` 重载。

### 6.5 cost 飙升

```bash
ccteam admin cost today
# 看每 bot 当日 USD
```

如果某 bot(常见是 dev / reviewer)异常高:

1. 看 `.ccteam/prs/*.json` 找 `fix_attempts >= 2` 的 PR(死循环嫌疑)
2. `@reviewer 看下 PR #N 是不是该止损`
3. 极端:`ccteam stop $SLUG` 先停,grep `.ccteam/progress.jsonl`
   找 cost spike turn

`workflow.yaml::budget.max_cost_usd_per_24h` 是硬上限,F84 触顶
auto-disable 整个 workflow,bot 不再 spawn turn。装机时按项目实际
调小(默认 30 USD 偏宽)。

### 6.6 .ccteam-worktrees/ 一堆没 cleanup 的目录

dev 在 fix-loop 撞顶或被强 stop 时没 cleanup worktree:

```bash
cd /path/to/your-target-project

# 看 git 视角有哪些 worktree
git worktree list

# 看文件系统残留
ls -la .ccteam-worktrees/

# prune 已经无 ref 的(safe)
git worktree prune

# 手动删特定 worktree(已合并 PR 的 worktree)
git worktree remove .ccteam-worktrees/dev-<id> --force
```

### 6.7 progress.jsonl 没产生事件

确认 ccteam orchestrator 真在跑 + 工作目录正确:

```bash
cd /path/to/your-target-project
pwd                                         # 必须等于 config.json::local_path
ls .ccteam/progress.jsonl
tail -5 .ccteam/progress.jsonl
```

如果文件缺失或为空 → `ccteam doctor` + 看 daemon log。

---

## §7 已知限制(V0.7 解)

每条限制都对应 `workflows/dev-flow/README.md §四` 一条 V0.7 substrate
路线 — 你跑得越多反馈越精准,直接驱动 V0.7 PRD。

### 7.1 GitHub event 不接

dev push 后 CI 异步跑 8 分钟,这 8 分钟里 ccteam 不知道 CI 完成,
reviewer 不会自动被唤醒看结果。

**workaround**:`@reviewer 看下 PR #N` 手工 ping;`@ops scan`
定期兜底;CI 全过的 PR reviewer 第一次 turn 就当场看到也行(如果
CI 快)。

### 7.2 ops 没有 schedule trigger

chat mode 不支持 cron。`@ops scan` 必须靠 `@pm` / 用户 / host
crontab 触发。

**workaround**(host crontab,绕开 ccteam):

```bash
# Telegram 示例
0 */6 * * * curl -s "https://api.telegram.org/bot$PM_TOKEN/sendMessage" \
  -d chat_id=<group-id> -d text="@ops scan"
```

### 7.3 backlog 没视图

`@pm status` 给你 backlog 数字,但要看详细列表只能 `ls
.ccteam/backlog/`。

**workaround**:在终端开一窗 `watch -n10 'ls -la .ccteam/backlog/
.ccteam/prs/'`。

### 7.4 backlog 没 priority 排序

dev 按文件名时间 FIFO。改优先级要手动改文件名前缀(`P0-` /
`P1-`)。

**workaround**:`@pm 现在优先做 backlog/<id>` — pm 会在自己 turn
内重写 backlog/<id>.md 的 priority 字段;不过 dev 当前不读 priority
字段,所以效果只是 pm 当下 @dev 时多说一句"优先"。

### 7.5 长 tmux session 资源占用

4 个 bot tmux session 24/7 跑,空闲也吃 baseline RAM(每 session
~150-300MB 含 Claude Code TUI)。团队规模扩到 6+ bot 时考虑
hibernate 机制(V0.7 / V0.8 候选)。

**workaround**:`ccteam stop $SLUG` 下班停,上班再 `start`。

---

## §8 FAQ

### Q1:能在 ccteam repo 自己上跑 dev-flow 吗?
不能(CLAUDE.md §六 红线:循环引用)。用 sibling project。

### Q2:能用 OpenAI Codex 当 dev 吗?
可以,workflow.yaml::agents.dev.executor 改 `codex`。但 codex chat
mode 走 `codex app-server` UDS(V0.6 F112 + V0.6.1 F122),与 Claude
tmux 行为有细节差异(`@reviewer` 路由走 daemon mpsc 相同,但
`.ccteam/chat/<bot>/turns.jsonl` 写入由 CodexAppServerAdapter
负责)。**实际跑过一周确认稳定再加大范围**。

### Q3:能给某个 bot 加 HumanApproval 吗(每个 merge 前用户必须 approve)?
可以。`workflow.yaml::agents.reviewer` 加:

```yaml
  reviewer:
    ...
    plan_approval:
      enabled: true
      outbox: telegram
      timeout_min: 60
      on_timeout: escalate
```

reviewer 在 merge 前会把 plan 写 `.ccteam/plans/reviewer-<ts>.md`,
ccteam 会通过 IM 发 plan 给你 approve/reject。详 F98 / F124。

### Q4:能跑 N > 4 个 bot 吗?
能。复制现有 agent.md → 改 frontmatter name → 编辑 workflow.yaml
加新 agents 段 → register-bot。建议保持 ≤ 8 bot,否则 IM 群里
@-routing 复杂度上升 + 资源占用线性增长。

### Q5:4 个 bot token 一定要 4 个独立 Telegram bot 吗?
是的。每个 bot 对应一个独立 IM 身份(头像 / handle / token),这
样用户在群里看得清谁是谁。**不支持** 1 个 token 多个 handle
(IM 平台限制,不是 ccteam 限制)。

### Q6:bot 之间能跨 slug @ 吗?
F183 chat_handle collision 之后可以(`@dev__<other-slug>` 显式跨
slug)。但 dev-flow 默认 4 个 bot 同 slug,不跨。

### Q7:用户在 IM 群同时跑多个 dev-flow 项目可以吗?
可以,每个项目独立 `chat_acl.allow_groups`(不同群)+ 独立
register-bot(不同 chat-id)。但**不要把多个 dev-flow 项目的 4
组 bot 装进同一个 IM 群** — handle 会冲撞,routing 错乱。

### Q8:dev 的 worktree 在哪儿?
`.ccteam-worktrees/dev-<backlog-id>/` 在项目根。每个 PR 一个,
合并后默认 cleanup(`worktree.cleanup_after_merge: true`)。

### Q9:能跨电脑(笔记本 + 远程服务器)跑同一个 dev-flow 吗?
不能。ccteam daemon 是 host-local。多设备同 IM 群:你在 IM 端
跟 bot 互动可以多设备,但 daemon / tmux 在一台机器。**V0.7 候选**
有 chat memory 跨设备同步(`docs/versions/...` 看 V0.7 Epic 列表)。

### Q10:能用本地 model 当 bot 吗?
ccteam 当前不支持本地 model,只接 Claude Code(Anthropic API)+
Codex(OpenAI)。

---

## §9 接下来

跑了一周后,你应该会发现 1-3 个 README §四 没列出来的新痛点。**把
痛点记下来**(开个 issue / 给 ccteam 团队反馈),正是 dev-flow 这
个模板的目标:UX 痛点 → V0.7 substrate。

- README:架构 / 设计 / 红线核验 / V0.7 借鉴 → [`./README.md`](./README.md)
- ccteam 全局手册:[`docs/user-manual.md`](../../docs/user-manual.md)
- holon 借鉴 research(扩展阅读):
  `references/holon-multi-agent-workflow-borrow.md`(本机 gitignore)
- ccteam 装机 / 配 IM 等基础流程:`/ccteam-im-setup` skill +
  `/telegram:configure` skill
