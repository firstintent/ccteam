# Agent 编排 — 深度用户指南

> English: [orchestration.md](orchestration.md)

**写给已经把 Claude Code / Codex 当日常主力的人。** 这就是 Task 工具——只不过 subagent 是一个完整的 vendor 会话:任意厂商、任意机器、合上笔记本也继续跑,而且每一跳都有账。

ccteam 在 `ccteam` 这个 MCP server 下暴露 8 个工具。在 Claude Code 里它们叫 `mcp__ccteam__session_spawn` / `mcp__ccteam__session_dispatch` / `mcp__ccteam__session_collect` / `mcp__ccteam__session_list` / `mcp__ccteam__session_stop`,外加 `mcp__ccteam__status` / `mcp__ccteam__chat_send_file` / `mcp__ccteam__screenshot`。5 个 `session_*` 就是编排面,下文全部围绕怎么把它们用好。

---

## 1. 心智模型

```text
chat ⇄ project ⇄ session          host = 会话进程跑在哪台机器
                 └─ s1, s2, …     (local,或反向拨入的卫星)
```

- **session** — 一个有独立上下文的 vendor 进程,持久 id(`s1`、`s2`…)扛 daemon 重启、永不复用。每个 session 属于且只属于一个 **project**。
- **project** — 注册过的仓库(slug → 路径),是委派护栏、成本上限、访问控制的**归属单元**。同一个*逻辑*项目可以在多台机器上各有一份 checkout:在卫星上注册同一个 slug,意思就是「那台机器上的这个项目的工作副本」。
- **host** — session 的**执行位置属性**,不是 project 的属性。`session_spawn{host:"gpu02"}` 让子会话跑在那台机器上;transcript、成本、委派账本仍然全记在主 daemon。
- **daemon 只路由,从不调度。** 没有 tick、没有 orchestrator。存在什么拓扑,就是你的会话(或你自己)用这 5 个工具搭出来的拓扑。
- **委派是记录,不是注入。** 派的任务原文作为 user turn 转发;完成通知是投给父会话的一条普通 turn——和真人转告同构。永远不会写进任何会话的 system prompt。

## 2. 谁能调这些工具、以什么身份

两种身份,最终走同一道 daemon 校验门:

| 调用方 | 身份 | 权限范围 |
|---|---|---|
| **ccteam 拉起的会话** | spawn 时铸造的 per-session `(sid, secret)` principal,经会话专属 MCP 配置注入 | 只能操作**自己的 project**;委派护栏全生效(深度、扇出、环、预算) |
| **普通主会话** — 你自己启动的 `claude` / `codex` | 同用户 admin fallback:stdio forwarder 读 admin web token(`~/.ccteam/secrets/web-token`,0600),daemon 校验 | 全舰队;`session_spawn` 目标 = **当前工作目录**解析出的项目(或显式 `project` 参数);spawn 出的是新委派树的根 |

主会话场景的几个实际推论:

- 在**注册过的项目目录里**开会话,`session_spawn{vendor:"codex", task:"…"}` 直接可用——除了一次性的 `ccteam config mcp` 和一个在跑的 daemon,零配置。
- 主会话本身不是 ccteam 会话,**完成通知没有落点**。短任务用 `wait_seconds`,长任务轮询 `session_collect`;子会话本身两种方式下都全程入账。
- 在任何注册项目之外时,显式传 `project:"<slug>"`。

**永远不要用 `codex exec` / `claude -p` shell 直调来「叫另一个 agent」。** 裸 CLI 一跑:没有 sid、不写 `turns.jsonl`、成本无账、没有完成通知、`session_list` 和团队视图都看不见。值得委派的事,就值得上账本。

## 3. 60 秒验证工具面

```bash
ccteam doctor --verify-mcp       # 8 工具 / 0 stub,漂移退出码 1
claude mcp list                  # server `ccteam` — ✔ Connected
claude -p "list your tools containing 'ccteam'"   # 真实会话看到的名字
```

如果 `claude mcp list` 显示 Connected 但**某个会话**里没有 `mcp__ccteam__*` 工具:

- 会话早于 `ccteam config mcp` 启动——MCP server 在会话启动时读取;重启会话。
- 会话是 **ccteam 拉起的**,带 curated `--strict-mcp-config`——它只有一个 MCP 入口(ccteam 自己 + principal)。该入口的 HTTP 端点连不上就是零工具;查 `ccteam status` 和 daemon 日志。
- SDK 驱动的 harness 可能根本不加载用户级 `~/.claude.json` 的 server——用普通 CLI 会话编排,或把 server 显式写进 SDK 配置。

## 4. 五个工具

以下参数列表与代码一致;未标注的都是可选。

### `session_spawn` — 雇一个同事(顺手把第一个任务交了)

```json
{"vendor":"codex", "title":"impl-rfc12",
 "task":"按 RFC-12 实现,跑完测试套件,汇报 pass/fail 和 diff 摘要。"}
```

- `vendor`:`claude`(默认)| `codex` | `grok` | `opencode`。`model` / `effort`:vendor 特定覆盖。`protocol`:`stream-json`(默认)或 `acp`——grok/opencode 强制 `acp`;`terminal` 永不暴露给 agent。
- `host`:`local`(默认)或已注册的卫星 id。slug 必须在那台机器上注册过(在那边 `ccteam init` 一次);远程执行当前支持 Claude stream-json 会话。
- `role`:`.claude/agents/<role>.md` persona,由 vendor 原生机制加载。省略 = roleless——裸 vendor 自读项目 `CLAUDE.md`/`AGENTS.md`,多数时候这就是对的默认。
- `task` + `wait_seconds` + `notify`:spawn+派活一次调用。默认异步:子会话干活,干完你收到一条完成通知 turn(仅 ccteam 会话调用方)。`wait_seconds:120` 阻塞并内联返回 `result_text`。`notify:false` = 只记账。
- `title`:≤80 字符,只进账本/团队视图——永远不进任何 prompt。
- `permission_mode`:`skip`(默认)或 `hitl`——工具调用弹到绑定 IM 审批。
- `idempotency_key`:同 key 重试回放原 spawn(同 sid)而非二次创建——MCP client 可能超时重试时务必设置。
- 返回 `{sid, vendor_session_id, host, …}`,永远是**新** sid。

### `session_dispatch` — 给现有会话派活

```json
{"sid":"s7", "task":"现在 rebase 到 dev,只重跑失败的用例。", "wait_seconds":0}
```

原文 user turn,零注入。默认异步 + 完成通知;`wait_seconds`(≤600)阻塞返回 `{status:"completed", result_text, cost_usd}` 或超时 `{status:"pending"}`——子会话继续跑,永不取消。派给自己或祖先会被拒(环检测)。`idempotency_key` 语义同 spawn。

### `session_collect` — 不进会话读输出

```json
{"sid":"s7", "tail":true, "n":3}
```

tail 子会话的 ccteam 侧 transcript。关键字段:`activity` — `working`(turn 进行中:去轮询,别猜沉默)/ `idle`(turn 结束:读结果)/ `stale` / `stuck`;`cost_usd`;`vendor_session_id`(原生 resume key)。增量轮询传 `since:<上次见过的 turn_id>`;长任务只要结论用 `tail:true`。默认 oldest-first,`n:20`。

### `session_list` — 委派树

返回所有 live 会话(`sid` / `project` / `vendor` / `activity` / `waiting_approval` / `host` / `cost_usd` / `title` / `parent_sid`)外加 `tree`(根 → 子)。这就是工具形态的舰队面板;web 团队视图渲染的是同一张图。

### `session_stop` — 显式,永不主动

停一个 sid。状态留盘,之后可按 sid 冷恢复。ccteam 从不主动杀会话——唯一自动刹车是每日 per-vendor 预算上限,而且是**拒绝新活**,不是杀在跑的。

## 5. 值回票价的用法

**按长处路由,深度只买一次。** 最深的模型做分解和裁决;长途苦活给 Codex,快问快答给 Grok:

```json
{"vendor":"codex", "title":"impl",  "task":"按以下约束实现 RFC-12…跑测试,汇报。"}
{"vendor":"grok",  "title":"probe", "task":"剖析 src/ingest 热路径,给前 3 个瓶颈。", "wait_seconds":120}
```

苦活用异步(完成通知像同事来汇报),只有下一句话就要用的分钟级答案才内联等。

**用对手模型守合并门。** Codex 实现;合并前在**同一项目**再 spawn 一个 Claude 审稿人:

```json
{"vendor":"claude", "title":"review-rfc12",
 "task":"审 rfc12 分支的 diff:正确性 + API 契约破坏。裁决:MERGE 或列出 blocker。"}
```

然后 `session_collect{sid, tail:true}` 直接拿裁决,不用翻整个 transcript。跨厂商互审能抓住同模型互审会放过的东西。

**哪里有环境去哪里跑。** GPU 测试在 Linux 盒子上:join 成卫星一次、在那边 `ccteam init` 这个仓库,然后 spawn 带 `host:"linux-box"`。卫星向 daemon 出站拨号——NAT 后面的笔记本也是合格卫星,只有 daemon 需要可达端口。host 选择器(和 daemon 的门)只接受真的上报了这个 slug 的主机——slug 没注册会快速报错「先去那边 ccteam init」,绝不静默本地兜底。

**认真轮询。** `working` = turn 进行中——先干别的,带 `since` 再来。`idle` = turn 结束——读。不要从沉默推断完成,有游标就不要整篇重拉。

**先设上限,然后信它。** 委派深度、per-parent 扇出、per-project 委派会话上限、环拒绝、每日 per-vendor 预算,全部由 daemon 带理由拒绝——失控扇出死在 spawn 时,不是死在账单上。配置里设一次;写 prompt 时假设「可能被拒」。

**一次 dispatch 一件事。** 完成通知按 turn 触发。一次塞三件事 = 一条通知 + 一份要自己拆的 transcript;拆成三次 = 三个检查点 + 三行成本。

## 6. 信任模型,说实话

per-session secret 和 admin-token fallback 是**单 OS 用户下的纵深防御,不是硬边界**——同 uid 进程终归能读到彼此的 env 或 token 文件。这道门买到的是:agent 不会*误*跨项目、不会*误*冒充彼此;每个动作都归因到已认证的调用方;远程主机看不到不属于它的 secret。硬隔离(per-agent OS 用户 / sandbox)当前刻意不做。HTTP `/mcp` 永远要 bearer(admin 或 per-session)——同用户 fallback 只存在于本地 socket。

## 7. 排障速查

| 症状 | 原因 → 处置 |
|---|---|
| 工具在列表里,但 `session_*` 回「not in a ccteam session … no admin web token」 | 这台机器 daemon 没启动过 → `ccteam start` 后重试 |
| `session_spawn: missing project` | 主会话在注册项目之外 → `cd` 进项目,或显式传 `project` |
| `project X is not registered on host Y` | 去那台卫星的仓库里 `ccteam init`,等一个心跳(~25s)重试 |
| client 超时后怀疑 spawn/dispatch 双发 | 设了 `idempotency_key` 就没有;从现在开始设 |
| 子会话「没动静」 | `session_collect` 看 `activity` —— `working` 不是没动静,是在干活 |

人用的三个入口(web 控制台、Telegram/Lark、CLI)见 [usage-cn.md](usage-cn.md) · [English](usage.md)。
