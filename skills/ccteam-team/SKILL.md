---
name: ccteam-team
description: Start an Anthropic Agent Team in the current Claude session via the `/ccteam-team` entry. Use when the user says "起一个 team", "spawn a team to do X", "并行调研 X", "team N:role <task>", "make a 3-debugger swarm", or otherwise asks to spin up multiple teammates working under the current session as lead. V0.5.0 primary path — works in any git repo, no `ccteam init` / `workflow.yaml` required. Uses Anthropic native `TeamCreate` + `Task` tools; the current session becomes the team-lead in-process.
---

# ccteam-team — `/ccteam-team` in current session

V0.5.0 primary path (95% 用户). 用户已在项目 session 里(`cd ~/projects/blog && claude`),
直接输入 `/ccteam-team "<task>"` 就把当前 turn 升级成 team-lead,native `TeamCreate` +
`Task` spawn teammates。**不切 session、不出 terminal、零 ccteam workflow.yaml 依赖**。

## V0.5.0 skill family (you are here)

| 意图 | Skill |
|---|---|
| **当前 session 起 agent team(本 skill)** | **`ccteam-team`** |
| 想**新建一个 ccteam 项目**(`ccteam new` + 长 running workflow.yaml + ccteam start) | `ccteam-creator`(走 step 1/2/3/4 对话) |
| **管理已有 ccteam 项目**(ls / show / pause / resume / send / 注入决策) | `ccteam-control`(CLI + MCP wrap) |

跟 `ccteam-creator`(创 workflow / 项目)和 `ccteam-control`(管 daemon / MCP)互补:
本 skill 只负责"在当前 session 里起一个 team"。

## 入口语法

```
/ccteam-team <task>                              # auto N + auto roles
/ccteam-team N "<task>"                          # N teammates, you decide roles
/ccteam-team N:role "<task>"                     # N teammates, all role=<role>
/ccteam-team auto "<task>"                       # explicit alias of form 1
```

例:

| 输入 | 解析 |
|---|---|
| `/ccteam-team "fix all TS errors"` | auto N + auto roles |
| `/ccteam-team 3 "refactor auth with security + tests"` | 3 mixed teammates |
| `/ccteam-team 3:debugger "fix build errors in src/"` | 3 个 debugger 并行 |
| `/ccteam-team 5:reviewer "review the new API design"` | 5 个 reviewer debate |

## 协议

本 skill 是 **supplemental instructions**(注入到当前 turn 的额外提示),**不替换** Claude
session 的 system prompt;detach skill 后 session 回到正常 chat。

### 1. Parse args

- 若 args 形如 `<digit>:<role> "<task>"` → `N=<digit>`,`role=<role>`,task=quoted body
- 若 args 形如 `<digit> "<task>"` → `N=<digit>`,roles=自决,task=quoted body
- 若 args 形如 `auto "<task>"` 或 `"<task>"` → N + roles 全自决
- task body 可能 unquoted(单 token 时);取剩余整段当 task

derived slug:从 task 提 3-5 个 kebab-case token(如 "fix-ts-errors" / "next-blog-build")作 `team_name`。
slug 必须 ascii lowercase + `-` + digits。若 `~/.claude/teams/<slug>/` 已存在,加 `-2` `-3` 后缀。

### 2. Quick context analysis(可选,2 段落以内)

只在 task 含义模糊或要懂项目结构时跑(典型场景:用户说"refactor auth",不知道 auth 在哪)。
方法:

- 用 `Glob` / `Read` 扫 `README.md` / `package.json` / `Cargo.toml` / `src/` 顶层结构
- 或 dispatch 一个 `Task(subagent_type="general-purpose")` 做"扫项目并 summary"

context 产出**最多 2 段**,塞进随后的 TEAM PLAN 的 `Rationale` 字段。task 明确的时候(如
"fix TS errors")**跳过这步**,直接到 step 3。

### 3. Output TEAM PLAN — STOP,不要 Task 调用

**这一步是红线 — 必须先 plan,然后 STOP。不可绕过。**

Claude 第一条 assistant message 严格按下列格式输出,然后**不调任何 tool**:

```
TEAM PLAN
=========
Team name: <derived-slug>
Proposed teammates:
  1. <name> (kind=<definition|ad-hoc>, model=<sonnet|opus|haiku>, color=<color>) — <one-line brief>
  2. ...
  N. ...

Spawn order: <parallel|sequential>
Rationale: <为什么这些 role / 这种组合 / 项目 context 摘要>

Reply 'go' / 'yes' / 'approve' to spawn, or free text to revise.
```

kind 决策:

- **definition-backed**:`.claude/agents/<role>.md` 存在(scope 顺序:project `.claude/agents/` → user `~/.claude/agents/` → plugin → managed)。spawn 时 Claude 自动 append `.md` body 到 system prompt。
- **ad-hoc**:没有对应 `.md`,team-lead 必须 inline 整 prompt(Worker Preamble + spawn_brief)。

不确定时用 `Glob`/`Read` 检查 `.claude/agents/<role>.md` 是否存在;不存在就标 `kind=ad-hoc`。

**这一步禁止调 `TeamCreate` / `Task`** — 等用户回复。

### 3.5 Codex critic teammate(V0.6.0 Wave 3 F112 §D — 当 N ≥ 3 时自动加入)

When the parsed team size `N ≥ 3`, ccteam-team probes for the Codex
CLI **once** per `/ccteam-team` invocation:

```bash
codex --version 2>/dev/null && codex login status 2>/dev/null
```

- Both succeed → automatically reserve **one** of the N teammate
  slots for a `codex-critic` (the remaining `N-1` are Claude). The
  TEAM PLAN must list it as:
  `<N>. codex-critic (kind=ad-hoc, vendor=codex, color=red) — second-opinion
   reviewer on every artifact the other teammates produce`
- Either probe fails → silently fall back to all-Claude composition
  for this run; no error surfaced. (User can re-run after installing
  / `codex login`.)
- `N < 3` → no codex critic auto-added (too small a team to spare a
  slot for adversarial review). User can still ask "加个 codex
  critic" as free-text revision to force one in.

**Test override**: `$CCTEAM_CODEX_BIN` overrides the binary lookup —
unit / e2e tests use a fake binary so the probe doesn't depend on a
real `codex` install.

The Codex critic teammate is **not** spawned via the Anthropic
`Task` tool — that surface targets Claude-only subagents. Instead,
the team-lead session runs the Codex teammate from skill body
directly via Bash, in parallel with the Claude `Task` spawns:

```bash
CODEX_BIN="${CCTEAM_CODEX_BIN:-codex}"
"$CODEX_BIN" exec --json --skip-git-repo-check <<'PROMPT' &
You are the codex-critic teammate of team "<slug>". Your job is to
review every artifact the other teammates produce and surface
adversarial concerns (security, perf, edge-case correctness). Reply
in <= 200 words per artifact. Don't write files — text only.

Task: <task body>
PROMPT
echo "codex-critic spawned (PID $!)"
```

The team-lead session captures stdout/stderr to a temp file the
synthesis loop polls for a `turn.completed` JSONL frame. **No tmux**
— `codex exec --json` is one-shot process, output is captured
directly. This bash-spawn path is the **only supported route** in
V0.6.5; the spawn mechanics are guarded by
`crates/ccteam-cli/tests/team_3reviewer_codex_critic_test.rs` (F156)
against a stub codex via `$CCTEAM_CODEX_BIN`.

**Daemon-routed variant (deferred)**: routing this spawn through the
daemon's `CodexExecAdapter` (so cost accounting is unified with the
F84 budget rollup) is a follow-up to F112 §D. It is **explicitly
deferred** past V0.6.5 — the daemon-side `mcp__ccteam__advise_*`
dispatch is still a `NotImplemented` stub on `main` (F152/F153 land
its real impl in V0.6.5 Wave 2 parallel, but cost-accounting + the
team-3-reviewer routing on top sits behind that landing). Track in
the V0.7 epic backlog.

If `ccteam-imd` daemon is running and exposes
`mcp__ccteam__advise_parallel` (V0.6.5 F153 onwards), the skill MAY
prefer that path instead — daemon-side cost rollup + persistent log.
Detection: a previous turn in this session must have registered the
MCP tool; otherwise stay on the direct Bash spawn.

### 4. 等用户回复

合法回复:

- `go` / `yes` / `approve` / `Y` / 中文「同意」「开始」/ 任意「肯定」语义 → 继续 step 5
- free text → 视为 revision,重新走 step 3 调整 plan,再 STOP 等
- `n` / `no` / 「取消」 → 礼貌中止,**什么 tool 都不调**

### 5. On approval:`TeamCreate` + `Task` 并行 spawn

5.1. 调用 `TeamCreate({team_name: "<derived-slug>", description: "<task 一句话摘要>"})`
    → 当前 session 升级成 lead,`~/.claude/teams/<slug>/config.json` 出现(5s 内)。

5.2. 对 plan 里每个 teammate,调 `Task`:

**definition-backed dispatch**(`.claude/agents/<role>.md` 存在):

```
Task({
  subagent_type: "<role>",       // 必须匹配 .md 文件名
  team_name: "<slug>",
  name: "<teammate-name>",        // 如 "reviewer-1" / "frontend-dev"
  prompt: "<spawn_brief>"         // 仅 task-specific brief;.md body 自动 append
})
```

**ad-hoc dispatch**(无 `.md`):

```
Task({
  subagent_type: "general-purpose",
  team_name: "<slug>",
  name: "<teammate-name>",
  prompt: "<Worker Preamble + spawn_brief>"   // 见 §6,~30 行
})
```

按 `Spawn order` 决定串/并:`parallel` → 一次 turn 多个 `Task` 调用;`sequential` →
spawn 第 1 个,等 SendMessage 回报 done,再 spawn 第 2 个。**默认 parallel**(Anthropic
Agent Team 设计取向)。

### 6. Worker Preamble(ad-hoc teammate prompt 头注入)

ad-hoc teammate 的 prompt 头部必须包含以下 ~30 行(中文化 OMC 模板)。**definition-backed
teammate 不加** — 它们的 `.md` body 已经定义了行为。

```
你是 team "<team_name>" 的 worker,名字 "<worker_name>"。你向 team-lead 汇报。

== 工作协议 ==
1. CLAIM:用 TaskList 看 owner==你的 pending task,TaskUpdate 标 in_progress
2. WORK:用你的工具(Read/Write/Edit/Bash/Glob/Grep)执行,**绝不 spawn sub-agent**
3. COMPLETE:TaskUpdate status=completed,带 result_summary 一句话总结
4. REPORT:SendMessage to team-lead "完成 #<task_id>: <一句话总结>"
5. NEXT:回 step 1。无更多任务 → SendMessage `{"type":"idle_notification","idleReason":"available"}`

== 红线 ==
- 不 spawn sub-agent / 不调 Task tool 起子任务(只有 team-lead 能 spawn)
- 不跑 team orchestration 命令(`$team` / `$autopilot` / `omc team` 等)
- 所有进度走 SendMessage 给 team-lead;不要默默工作
- 用绝对路径
- 不修改 `~/.claude/teams/` 或 `~/.claude/tasks/` 文件(Anthropic 自动维护)

== 错误处理(3-strike)==
- 第 1 次失败 → 读 stderr,定位根因
- 第 2 次同错 → 换方案
- 第 3 次 → SendMessage to team-lead 上报,等指令

== 项目上下文 ==
cwd: <project-cwd>
<spawn_brief 主体>
```

### 7. Monitor loop(spawn 后 team-lead 自动行为)

spawn 完成后,team-lead(= 当前 Claude session)进入 monitor 模式:

- **SendMessage relay**:teammate 发 `SendMessage to team-lead` → Claude 自动 deliver,
  team-lead 看 message,决定:回复 / re-dispatch / 标 task done
- **TaskList polling**:每隔 several turns 调 `TaskList({team_name})` 看进度。所有 pending
  task 都有 owner + status → OK;有 unowned pending task → 用 TaskUpdate 派给空闲 worker
- **idle handling**:`{"type":"idle_notification"}` 系统消息 → 该 teammate 待命,可派新
  task(若 backlog 还有);全员 idle + 无 backlog → 准备走 §8 completion
- **3-strike escalation**:同一 teammate 3 条 SendMessage 都报错 → team-lead 把该任务
  re-assign 给另一 teammate 或拆小,**不要无限重试**
- **plan revision**:用户中途插话(`'add a security review step'`)→ team-lead 视为
  revision,可以新 `Task` spawn 或 `SendMessage` 给现有 teammate 调整方向

### 8. Completion

何时算完:**所有 plan 列出的 task 都 status=completed**(`TaskList` 查)且**没有新的
discoveries 需要新 task**。

清理流程:

1. team-lead 对每个 teammate `SendMessage` 发 `{"type":"shutdown_request"}` 系统消息
2. 等每个 teammate 回 `{"type":"shutdown_response","ok":true}`(或超时 60s)
3. 调 `TeamDelete({team_name})` 关 team
4. 给用户输出一段 markdown summary:做了什么 / 谁做了哪部分 / 关键 commit 或文件 / 任何
   open question

完成后 team-lead session 回到正常 chat 状态。

## ccteam daemon + web 怎么配合

本 skill 跟 ccteam daemon **完全解耦** — 不需要 `ccteam init` / `ccteam start <slug>`。
daemon 通过 F95 全局 watcher 自动发现 `~/.claude/teams/<new-slug>/` 出现 → web `/teams`
tab 5s 内出现该 team 卡片(若 daemon 在跑)。

用户只需要一次性:

```bash
ccteam doctor --install-skill all   # 装 ccteam-team / ccteam-creator / ccteam-control
ccteam start                        # 启 daemon(可选,纯 web 可视化用)
```

之后任意 git repo 跑 `/ccteam-team <task>` 都自动落 web。

## Skill 不做什么(刻意省略)

- **不写 `workflow.yaml`** — primary path 零 ccteam project 概念
- **不修改 `.claude/settings.json`** — F94 hook 注入是 F93b advanced 专属
- **不开 bg lead session** — 当前 session **就是** lead(用户关 session = 团队停,跟
  Anthropic / OMC native 行为一致)
- **不管 cost cap** — Anthropic 自有 usage limit;F84 budget cap 只 advanced path 用
- **不写 `~/.claude/teams/` 或 `~/.claude/tasks/` 文件** — Anthropic SoT,只读
- **不抄 OMC 5-stage pipeline**(`team-plan → team-prd → exec → verify → fix`)—
  ccteam 鼓励 shape-agnostic,pipeline / debate / parallel review / vote 都行,lead 自决

## 红线

- **Plan-first 不可绕过** — 必须先输出 `TEAM PLAN ===` 然后 STOP,然后才等 user
  approval。若 Claude 跳过此步直接 Task spawn,视为协议违反
- **No `system_prompt` injection** — skill body 是 supplemental,不是 system prompt
- **Worker Preamble 仅 ad-hoc 用** — definition-backed teammate 的 `.md` body 已定义行为,
  额外 inject Preamble 会破坏 frontmatter `tools` / `model` 限制
- **不动 ccteam-managed 文件** — `.ccteam/` / `~/.ccteam/config.yaml` 跟本 skill 无关

## What this skill cannot do

- 不能跨 session 跑 — 当前 session 关 / `claude` exit 后 team 也停。要 24×7 long-running
  team,走 advanced path:`ccteam init --mode agent-team <slug>` + `ccteam start <slug>`(F93b)
- 不能强制 plan approval gate — Claude session 一般在 user-turn 间不会"主动等";本 skill
  靠 Claude 看到"STOP, do NOT call Task yet"指令自觉停手。如发现绕过,要 fix skill body
- 不能监控 cost — 用 `ccteam web` (`http://localhost:7331/teams/<slug>`) 或 `claude
  /cost` 看

## Where to look in the repo

- `@docs/versions/v0-5-0/prd.md` §F93a — 本 skill 设计 SoT
- `@docs/versions/v0-5-0/dev-plan.md` Wave 1 — 实施细节
- `@CLAUDE.md` §三 — 架构红线
- `@skills/ccteam-control/SKILL.md` — sibling skill(管 daemon)
- `@skills/ccteam-creator/SKILL.md` — sibling skill(创 workflow / 项目)
