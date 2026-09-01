# 策略 hook 与 Flow

> English: [hook-dynamic-workflows.md](hook-dynamic-workflows.md) · 工具参考: [mcp-cn.md](mcp-cn.md) · 委派指南: [orchestration-cn.md](orchestration-cn.md)

把确定性代码放到 agent 团队周围的三种方式——三者可组合,因为任何模式下的每次雇佣都要过策略 hook:

| 模式 | 是什么 | 什么时候用 |
|---|---|---|
| **1. 策略 hook** | 给每一次委派把关的脚本 | 约束:额度、vendor 白名单、内置护栏之外的项目规则 |
| **2. ccteam Flow** | ccteam runner 执行的确定性 JS 脚本,驱动真实的跨 harness 雇佣 | 可重复、可恢复、headless、大规模的编排 |
| **3. Claude 原生桥接** | Claude Code 自家动态工作流经 MCP 雇 ccteam agent | 你住在 Claude Code 里,想要它的工作流 UI + 跨 harness 叶子 |

hook 约束 agent 做的选择;Flow 自己做选择;桥接借 Claude 的运行时做同一件事。

## 1. pre-agent 策略 hook

每一次 `agent` 调用(雇新会话或向既有 sid 派活)都可以被你的脚本把关,每次调用现场解析:

1. `<project>/.ccteam/hooks/pre-agent` —— 项目自己的策略
2. `~/.ccteam/hooks/pre-agent` —— 全机回落
3. 都不存在 → 放行(未配置的 daemon 行为与从前完全一致)

项目声明了策略就是声明了**全部**策略:项目文件**替换**全局文件,绝不合并——与 `routing.md` 同一条规则。零注册、零重启:改完文件,下一次 `agent` 调用就按新逻辑跑。`ccteam init` 从不代种;想要就自己建。

### 契约

hook 是任意可执行文件(shell、python、编译二进制都行)。运行时 cwd = 项目根(全局档则为 ccteam home),env 带 `CCTEAM_HOOK=pre-agent`,**3 秒预算**,自有进程组——它拉起的帮手随它一起死。stdin 收到一行 JSON:

```json
{"kind":"hire",
 "caller":{"sid":"s42","vendor":"claude","depth":0,"project":"myapp","context_pct":41},
 "request":{"vendor":"claude","model":"opus","wait":0,"task_head":"前 500 字…","task_chars":1234},
 "usage":{"claude":{"observed":"2026-09-01T04:10:00Z","windows":[{"w":"5h","pct":83,"resets":"…"}]}},
 "counts":{"children":3,"delegated":12,"cost_24h_usd":4.2}}
```

`usage` 就是 `status{detail:"usage"}` 报的那张 per-harness 额度图——直接喂进来,策略脚本**不需要 token、不需要回连 daemon**(hook 在锁外执行,真要 curl REST API 也安全)。未知事实省略,绝不补零。

裁决 = 退出码:

| exit | 含义 |
|---|---|
| `0` | 放行 |
| `2` | **拒绝**——你的 stderr(至多 2000 字节,逐字节原样,连空白都保留)作为拒绝理由返回给发起调用的 agent |
| 其他 / 超时 / 不可执行 | **脚本故障**——调用同样被拒(坏了就悄悄放行的护栏不是护栏),但以独立的 `policy_script_error` 呈现并点名脚本与故障形态,「策略说不」与「策略坏了」永远长得不一样 |

每次拒绝都进项目 progress 账本(`delegation_policy_denied`)与 daemon 拒绝计数——拦掉你一半委派的策略是看得见的,绝不静默。

### 示例:额度感知改道

这个机制存在的原因:让每个项目在自己的脚本里定自己的阈值,而不是 ccteam 养一套配置 DSL。

```sh
#!/bin/sh
# .ccteam/hooks/pre-agent —— claude 5h 窗口过热时把雇佣引去别家。
payload=$(cat)
vendor=$(printf '%s' "$payload" | jq -r '.request.vendor // "claude"')
pct=$(printf '%s' "$payload" | jq -r '.usage.claude.windows[]? | select(.w=="5h") | .pct // 0' | head -1)
if [ "$vendor" = "claude" ] && [ "${pct:-0}" -ge 80 ]; then
  echo "claude 5h 窗口已用 ${pct}% —— 这单请改雇 codex 或 kimi" >&2
  exit 2
fi
exit 0
```

被拒的 agent 拿到这句话作为工具错误,重新决策——约束是确定性的,再选择权留给模型。

### 诚实边界

- 同一 OS 用户下一切都是软隔离:在项目里干活的 agent 改得了本项目的 hook。这是项目自治,不是安全边界。
- 绑在卫星主机上的项目,其 `.ccteam/` 在那台机器上,daemon 读不到——远程项目的委派由**全局** hook 把关。

## 2. ccteam Flow(流程脚本)

> 原名「动态工作流」,为与 Claude Code 原生同名特性区隔而更名 **Flow**——CLI 本来就是 `ccteam flow`。

**Flow** 是一个 JavaScript 文件:脚本确定性地驱动大量雇佣——计划住在代码里,模型只做叶子。脚本里的每个 `agent()` 都是一次普通的 ccteam 委派:任意 harness、账本上真实的 sid、同样的深度/预算护栏、同样要过你的 pre-agent 策略 hook。

### 快速上手

```js
// flow.js
export const meta = { name: 'audit-routes', description: 'Audit route handlers for missing auth' }

const files = await agent('List every route file under src/routes/, one path per line, nothing else.')
const audits = await parallel(
  files.trim().split('\n').map(f => () => agent(`Audit ${f} for missing auth checks.`, { label: f }))
)
return audits.filter(Boolean)
```

```bash
ccteam flow run flow.js            # 在已 init 的项目里(否则 --project <slug>)
```

进度逐行流向 stderr;stdout 是最终 **RunReport** JSON——脚本返回值、每 agent 记录(sid、成本、是否缓存)、总计与缓存诊断。干净跑完 exit 0。

参数:`--args <json>`(脚本里读 `args`)· `--parallel <n>` · `--max-agents <n>` · `--max-cost <usd>` · `--budget <usd>` · `--run-dir <dir>` · `--resume <run-dir>` · `--watchdog <secs>`。

### 脚本住哪

`ccteam flow run` 接显式路径,放哪都行。约定:共享 flow 放 **`.agents/flows/`**(与 `.agents/skills` 同族),提交进 git,全队跑同一份编排。**别放 `.ccteam/`**——ccteam 会把该目录幂等加进项目 `.gitignore`,脚本会静默丢出版本库(per-checkout 的 *hook* 住那里恰恰因为这一点)。可跑样例:[`examples/flows/`](../examples/flows/)。

### 脚本面

| global | 契约 |
|---|---|
| `agent(task, opts?)` | worker 的最终文本;`opts.schema` 命中时为校验过的对象;**任何 worker 侧失败一律 `null`**(vendor 崩、护栏或策略拒——原因在 run report 里) |
| `parallel([...thunks])` | 栅栏;失败槽位 = `null`,调用本身永不 reject |
| `pipeline(items, ...stages)` | 阶段间无栅栏——A 在第 3 段时 B 可以还在第 1 段;某段抛错则该 item 置 `null` 并跳过其余段 |
| `phase(t)` / `log(m)` | 进度结构与叙述 |
| `args` | `--args` 原值 |
| `budget` | `{total, spent(), remaining()}`,美元计——children 成本实时求和 |
| `usage()` | 与 `status{detail:"usage"}` 同一张额度图——三行代码实现额度感知选 vendor |

`agent` opts:`vendor`(默认 claude)· `model` · `effort` · `role` · `sid`(向既有会话追派——跨步骤复用 worker 上下文)· `keep`(结果消费后不停掉 worker)· `label` · `phase` · `title` · `permission_mode` · `schema` + `retry:{max,prompt}`。未知选项是硬错误,不是静默忽略。

**刹车与失败的区别。** worker 失败 = 该调用 resolve `null`。**刹车**——`max_agents`、`max-cost`、墙钟、budget——拒的是*新*准入:直接 `await agent()` 会抛出点名刹车的错误,`parallel`/`pipeline` 槽位掩为 `null`,两种情况 `RunReport.brake` 都会点名;在飞的 worker 永远跑完——刹车从不取消进行中的工作。

**确定性。** 脚本空间无文件系统、网络、进程;`Date.now()`、`Math.random()`、无参 `new Date()` 直接抛错——时间戳与随机数走 `--args` 传入。正是这条纪律让 resume 精确。

### Resume

每次调用都进 journal(`<run-dir>/journal.jsonl`,内容键;大结果在 `results/`)。`--resume <run-dir>`——或把 `--run-dir` 指向已有 journal 的目录——重新执行脚本:没变的调用从缓存重放、不碰 daemon;第一个变了的调用起全部转 live(report 说明在哪、为何);跑到一半的调用**按 sid 重挂回仍在运行的会话**。worker 是 daemon 管的 session:活得比 CLI 进程久,崩溃不会浪费任何已派出的工作。

### 调度,与诚实的边

- 准入经 per-run `--parallel`(默认 32)与 per-vendor slot 池;vendor 配额/限流错误让整个池指数退避,而不是锤 harness。
- worker 的结果被消费即停(`keep:true` 留活);run 结束扫尾其余。transcript 留在账本上——`agent_read` 照常可读。
- **结构化输出是提取,不是强制**:ccteam 对 worker 零注入,所以 `schema` = 确定性 JSON 提取 + 校验 + 有界的同会话重试;始终不从命的 worker 得 `null`。(Claude Code 能对自家 subagent 强推 schema 工具;跨 harness 的 runner 做不到。)
- **run 目前住在 CLI 进程里**:关掉它停止的是*驱动*——worker 跑完当前 turn,`--resume` 接着续。daemon 托管的后台 run 是下一阶段。
- 并行改文件的雇佣共享同一工作树——给 agent 分派不相交的文件,或让它们自建 worktree;per-hire 隔离尚未提供。

## 3. 桥接模式——Claude 原生工作流驱动 ccteam 队伍

如果 Claude Code 是你的主会话入口,它的**原生动态工作流**今天就能编排 ccteam agent:原生工作流里的每个 `agent()` 是一个 claude subagent,让它用 ToolSearch 装载 ccteam MCP 工具、雇一个真实会话。你白得 Claude Code 的 `/workflows` 进度视图与暂停/恢复——而叶子跑在 codex/kimi/grok 上、上账本、照过你的策略 hook。

```js
// .claude/workflows/ccteam-team-review.js —— 以 /ccteam-team-review 运行
export const meta = { name: 'ccteam-team-review', description: 'Cross-harness review via ccteam' }

const files = await agent('Run `git diff --name-only dev...HEAD`; one path per line, nothing else.')
const reviews = await pipeline(
  files.trim().split('\n').filter(Boolean),
  (f) => agent(
    'Load the ccteam tools with ToolSearch (select:mcp__ccteam__agent,mcp__ccteam__agent_read). ' +
    `Hire codex: mcp__ccteam__agent{task:"Review ${f} for correctness bugs. VERDICT first line.", vendor:"codex", wait:240}; ` +
    'poll mcp__ccteam__agent_read{sid, wait:240} if pending. Return ONLY the worker\'s final text.',
    { label: f },
  ),
)
return await agent(`Merge into one ranked list:\n${JSON.stringify(reviews.filter(Boolean))}`)
```

完整版见 [`examples/claude-native/`](../examples/claude-native/)。与 ccteam Flow 的诚实对照:

| | ccteam Flow | Claude 原生桥接 |
|---|---|---|
| 胶水成本 | 零——runner 直接调 MCP 面 | 每叶一个 claude subagent 做 MCP 转发 |
| 存活性 | 越过 CLI 进程——`--resume` 重挂;worker 是 daemon 会话 | 绑死 Claude Code 会话;退出即从头 |
| 编排者 | 任意 harness、headless、cron | 只能是 claude 会话 |
| 进度 | stderr 逐行 + RunReport JSON | `/workflows` 树、暂停/恢复按键 |

坐在 Claude Code 里、工作流 UI 值回票价 → 用桥接;run 要活得比你久、要 headless、要驱动几百个叶子 → 用 Flow。
