# ccteam MCP server — 工具全参考

> English: [mcp.md](mcp.md) · 白话委派指南: [orchestration-cn.md](orchestration-cn.md) · 人用手册: [usage-cn.md](usage-cn.md) · 策略 hook 与工作流: [hook-dynamic-workflows-cn.md](hook-dynamic-workflows-cn.md)

ccteam 只暴露**一个 MCP server,名字 `ccteam`**,走 streamable HTTP:daemon 的 `POST /mcp`(默认 `http://127.0.0.1:7331/mcp`)。工具名由你的 harness 加 server 前缀 —— Claude 里显示为 `mcp__ccteam__agent`,其他 harness 用各自的前缀。这套面刻意做成**菜单而非手册**:六个工具、每参数一行说明、紧凑 JSON 返回体、默认薄 + 旋钮 —— 因为 schema 的每个字节、默认返回的每一行,都记在 agent 的上下文账上。边界与失败语义住在服务端错误体里(踩到的人才付)和本页(人读一次就够)。

## 1. 接入:两种凭据家族

`POST /mcp` 永远要 bearer —— 没有 cookie / web-token 路,也没有 admin 层:

| Bearer | 谁 | 从哪来 |
|---|---|---|
| `ccteam-sid:<sid>:<secret>` | **ccteam 受管会话**为自己发声 | spawn 时写进该会话的专属 MCP 配置,无需任何操作 |
| `ccteam-enroll:<id>:<secret>` | **手起 client**(你自己启动的 CLI 会话、SDK、脚本) | `ccteam config mcp` 往各 vendor 全局配置写一份机器级凭据;web 控制台可铸项目级凭据(项目页 → external agent) |

enrollment 凭据只说明「这份配置是谁的」。进程级身份在 `initialize` 时签发:响应头带 `Mcp-Session-Id`,之后每个请求都要回带;`DELETE /mcp`(或 ~2 小时空闲清扫)结束该 binding。enrolled client 会成为一条真实的账本会话(`managed_by: external`),它雇的人是它的子节点,不是无主根。

**点名 workspace。**机器级凭据不含项目,所以 enrolled client 的第一个 `agent` / `agent_read` / `agent_stop` 调用必须带 `project:"<slug>"` —— 首次点名即终身绑定,只接受凭据 owner 可见的项目,ccteam 绝不从工作目录推断。拒绝时会列出你够得着的 slug。

## 2. 服务端注入什么(以及给谁)

工具列表与 server `instructions` 都在连接时**按 caller 组合**,所以会话永远不为用不到的雇人手册付费:

| caller | `tools/list` |
|---|---|
| 还能雇人的会话(深度低于 `delegation.max_depth`,默认 2) | `status` · `grok_claude_codex_kimi` · `agent` · `agent_read` · `agent_stop` |
| 深度封顶的子会话,或以 `tools:"read"` 雇的 | `status` · `agent_read` |
| 以 `tools:"none"` 雇的 | *(空)* |
| 手起 client(点名项目前后) | 全部六个 |
| …另外,任何有 chat 可回的 caller(root 会话,或当前绑着 IM/web chat 的会话) | 追加 `chat_send_file` |

`CCTEAM_DISABLE_TOOLS`(组名逗号表:`admin` / `chat` / `session`)在此之上再过滤。面在进程生命周期内固定;resume = 新进程 = 重算。**裁面只是列表决策,不是权限**:被藏起的工具硬调,仍走原有的全部鉴权门。

`initialize.instructions` 保持在 ~1 KB 内,同样按面组合:一句「ccteam 是什么」;「用 `agent`,绝不 shell 出去跑 `codex exec` / `claude -p`」的政策只给能雇人的面;chat 信封说明只给 chat 可达的会话;附件规则(`<channel …>` 标签或 `[attachment …]` 行带 `image_path=` / `file_path=` → 先读那些文件再回答)永远在;最后一行陈述身份事实 —— `You are s42 in project cct. Completion notifications from your hires arrive here.`;client-run 会话与尚未点名项目的手起 client 拿到的是反向事实(`notifications cannot be pushed to you; agent_read{sid,wait} awaits a turn instead`);深度封顶时补一句事实。送达是**陈述**而非留给推断:一个受管会话曾把 `notify_deliverable` 的缺省读成「我是手起的」,白搭了一套轮询旁路。ccteam 只写你是谁、在哪工作,绝不写你该怎么做。

## 3. 六个工具

### `agent` —— 雇一个,或派下一件

`{task, sid?, vendor?, wait?, model?, effort?, role?, project?, title?, notify?, tools?, mode?, permission_mode?, idempotency_key?, parent_sid?}`

`task` **必填**,原文作为 user turn 转发(零注入);没有「只建不派」的形态。

- **不带 `sid` = 新雇。**`vendor` 选 harness —— `claude`(默认)/ `codex` / `grok` / `opencode` / `kimi` / `pi` / `dsh` —— 响应总是带**新** sid。`model` / `effort` 原样传给 vendor(省略走默认;vendor 拒绝的值 = 雇佣失败,绝不静默忽略)。`role` 指 `.claude/agents/<role>.md`(省略 = roleless,裸 vendor 自读项目 `CLAUDE.md`/`AGENTS.md`)。`mode` 仅 DSH(`standard` | `ptc` | `minimal` | `creator`)。`permission_mode:"hitl"` 把审批弹到你绑定的 chat;默认 `skip` 不弹。`tools` 设子会话自己的面(§2)。`title`(≤80 字符)只进账本与团队视图,绝不进任何 prompt。`parent_sid` 用于 ccteam 不管理你时保住委派边。
- **带 `sid` = 续派**;`released` 会话先按 sid 复活。此形态下雇佣类参数一律拒绝而非静默忽略。
- `wait` —— 内联等待秒数,0–240(默认 0 = async)。它等的是**这一次请求**的答案:同一子会话里另一个任务先完成,那是那次请求的完成、不是你的,照常推给你。超时回 `answered:false` + 下面那组送达事实,**绝不取消子任务**。
- `idempotency_key` —— 同 key 重试重放原调用而非翻倍(新雇按项目、续派按子会话;内存态,~1 小时)。重放响应多一个 `idempotent_replay:true`。
- **没有 `host`**(机器跟随项目绑定)、**没有 `protocol`**(信道由 vendor 推导);传了都是硬错,退役的 `wait_seconds` 同理(已改名 `wait`)。

**每一次调用都是一条有身份的请求。** ccteam 在写进 vendor **之前**就铸出 `request_id` 并落盘,它自带自己的 parent、`notify` 档位、`title` 与生命周期(`accepted` → `queued` | `submitted` → `executing` → `answered` | `failed`)。一条请求**只**由它绑定的那个执行 turn 来了结 —— 绝不按最新、按时间戳或按假定的排队位置匹配 —— 所以对同一个忙碌子会话的第二次派工,既拿不走第一次的答案,也改不掉它的名字、更改不了它通知谁。这条规则双向成立:没有任何请求绑在上面的 turn 边界(子会话自己的人类问了它一句;或事件 id 派发侧根本看不到的通道,例如冻结的 `terminal` 协议)**什么都不了结** —— 那些未决请求继续等,送达状态是 `unknown`,而不是被别人的答案顺手关掉。

响应(紧凑 JSON):async → `{sid, request_id, turn_id, status, delivery, queue_position?}`。

- `status` 是 adapter **实际做了什么**:`started`(子会话空闲,这条开了一个新 turn)、`injected`(汇入正在跑的那个 turn)、`queued`(独立的后续 turn,在排队 —— 任务排在重启前旧进程后面时也是这个形状,并附一条 `hint`)。
- `queue_position` 从 1 起算,只在 adapter 看得见自己的 FIFO 时出现。`turn_id` 指的是这个任务**将要**跑在哪个执行 turn 里,排队期间就已经给了。
- `delivery` 把四件不同的事分开:`accepted`(ccteam 已持久持有)、`queued`(还被 ccteam 留在 harness 前面)、`written`(字节已写进 harness)、`executing`(观察到承载它的 turn 开始了)。冲进 stdin 不等于模型读了,所以在 turn 真的开起来之前 `executing` 一律是 `"unknown"`;凡是 ccteam 观察不到的,都说 `"unknown"`,不给一个自信的 `false`。
- 完成通知到不了你时带 `notify_deliverable:false` —— 那就用 `agent_read{sid,wait}`。

内联(`wait`)→ `{sid, request_id, turn_id, turn, status:"completed"|"failed", context_pct?, cost_usd?, result_text, error_kind?, error?}`,其中 `turn_id` 就是回答**这一条请求**的那行 transcript;`result_text` 保留 2000 字符头尾节选(与推送通知的 `final` 同档),标记里写明读全文的精确调用 `agent_read{sid,turn:<turn_id>,max_chars}`。等待超时回 `{sid, request_id, turn_id, status, state, delivery, answered:false}` —— `state` 是这条请求当下的生命周期状态,所以「还排在第三位」和「正在跑」分得开。你阻塞期间这条请求被移出账本了(`agent_stop`、parent 已不可达),回的是同一个形状但 `state:"unknown"`:现在没人会再了结它,而一个 ccteam 说不出名字的答案只会报「没有」,绝不拿别人的顶上。用 `wait` 拿到答案的那**条请求**不会再推完成通知:答案已在你手上,绝不会再送第二遍。查无此 sid 的错误会区分「这里从未有过」与「被用户显式 stop 过」。

### `agent_read` —— 名册,或一份 transcript

`{sid?, n?, tail?, since?, turn?, max_chars?, wait?, project?, activity?, tree?}` —— 只读;`sid` 决定你拿到什么。

- **不带 `sid` = 名册**,最近活跃在前 —— 受管会话看到的是**自己项目**(点名别的项目会被拒,与其他按 sid 寻址的调用一致),web/租户 caller 看到自己拥有的项目:`n` 行(默认 10,最多 500),过滤器 `project` 与 `activity`(`working` | `idle` | `stale` | `stuck` | `all`),`tree:true` **只对返回行**铺委派拓扑。行 = `{sid, vendor, model?, role?, title?, activity, residency?, context_pct?, parent_sid?, is_self?, waiting_approval?, host?, cost_usd?, tokens_total?}`,空字段省略;`is_self` 标你自己那行;`truncated:true` + `total` 只在截断时出现。`residency` 只在 ccteam 不持进程时出现:`released` 在你下次 `agent{sid}` 时复活 —— 复用它,别雇双胞胎;`stopped` 是被用户显式结束的。
- **带 `sid` = 该会话的 transcript**,默认**最新在前**(`tail` 默认 true;给了 `since` 则从 turn_id 游标向前翻页 —— **最旧的未读在前**,所以 `since` + `n:1` 拿到的是最旧一条未读,绝不是最新答案的捷径)。这里 `n` 默认 **1** 条 ——「它答了什么」就是最新那一条,花名册的 10 是给一行一个会话用的;`max_chars` 默认 1000(100–50000)。超长保留 70% 头 / 30% 尾节选,标记就是读全那一条 turn 的精确调用;当标记比它省下的文本还贵时整条返回;整页每行都会掉到 ~200 字符以下时,丢掉最旧的几行(计入 `remaining`)而不是把每行都剁碎。全文永远在账本里。返回体:`{activity, context_pct?, cursor?, remaining?, latest?, cost_usd?, tokens_total?, residency?, truncated?, requests?, resolved_requests?, unknown_requests?, turns:[{turn_id, content, outcome?, error_kind?, error?}]}` —— `cursor` = 本页最后一条;`remaining` = 本页没给的匹配 turn 数(`since` 读时 = 仍未读的条数);`latest` = 最新一条的 turn_id,只在本页没有落在它上面时出现;`truncated:true` = 某条返回文本被 `max_chars` 截断。空 `turns` = 还没答案;`activity:"working"` = turn 进行中。
- **`requests`** —— 这个会话还欠谁什么:未决的在前(按受理顺序),后面跟一段有界的已了结记录,至多十条。每行 `{request_id, parent_sid, state, notify, title?, queue_position?, turn_id?, answered_turn?, created_at, delivery}`,`delivery` 与派发响应里的四件事同义。派工方由此看见自己的队列,而不是靠猜。
- **`turn:<turn_id>`** —— 精确取那一条 turn,此后这个会话再完成多少个 turn 都不影响。每条被截断的节选,配方指的就是它:`n:1` 的意思是「最新那条」,而那只是节选被写出的那一瞬间成立。transcript 里没有的 turn 会报错,绝不给你别的一页。
- **`n:0` = 只要状态**:同一返回体但没有任何 turn 文本 —— `activity`、`context_pct`、`latest`,带 `since` 时还有未读条数 `remaining`。这是最便宜的「做完了吗?有新话吗?」读法。你很少需要它:自己雇的会话完成通知会自己到、以它为准;只轮询那些回了 `notify_deliverable:false` 的会话,而且优先用 `wait` 而不是循环。
- `wait`(随 `sid`)—— 目标 turn 在飞时挂住这次读的秒数,0–240(默认 0)。到边界 → 含最终 turn 的正常返回体,外加 `resolved_requests`:这次读**答复**了你的哪几条任务,点名给出,答案就不会被误当成另一条任务的。没被答复就不再可解的那些 —— 被 `agent_stop` 丢掉、parent 已不可达、turn 被中途打断 —— 单独列进 `unknown_requests`:现在没人会再答它,你手上也没有它的答案。超时 → 正常返回体 + `activity:"working"`;没有在飞 turn → 立即返回。超时绝不动那个 turn。循环 `agent_read{sid,wait:240,since:<cursor>}` 就是等一个比内联 `agent{wait}` 更久的子会话的正解 —— 不要自己去 tail 它的 `turns.jsonl`。若等到边界的正是派任务的 parent,该任务的完成通知会被抑制:答案已经在你手上。
- 退役的 `limit` 参数 = 硬错(已改名 `n`)。

### `agent_stop` —— 显式结束一个会话

`{sid}` → `{sid, stopped:true}`。显式命令,绝非主动 kill:transcript 留在盘上,`agent_read{sid}` 照读。agent 只能 stop 自己的后代 —— 手起 client 一旦重连就是**新**账本节点,先前雇的会话不再是它的后代:那些去 web 控制台或 `POST /api/v1/sessions/{sid}/stop` 停,拒绝体里就这么写。(ccteam 自身只有两个自动刹车:vendor 日预算触顶拒**新**活;live 容量满时优雅释放最久未活跃的空闲会话 —— 创建永不因容量失败。)

### `status` —— 能雇谁、花了多少;分级

`{detail?: "brief" | "models" | "vendors" | "routing" | "usage" | "full"}` —— 只读,默认 `brief`(~100–200 B):`{project, host, cost_24h_usd, hire:[…]}`,`hire` = 项目绑定主机上真正装了的 vendor;卫星离线或快照陈旧时补 `host_online:false` / `stale:true`;有 vendor 触顶时补 `budget_disabled:[…]`。`detail` 要多少买多少:

- `models` —— 每 vendor 观测到的模型 id + reasoning-effort 阶梯(runtime last-seen,带观测时间)+ hub `models.json` 目录,两个来源分开标注。均为参考,绝非雇佣白名单。
- `vendors` —— 每 vendor 的 installed / version / auth(`unknown` —— 诚实:在 PATH 上不冒充已登录,也从不拦雇佣)/ 预算姿态、观测时间戳、pi/dsh 桥接说明。
- `routing` —— 你的 routing notes 原文(`source` / `sha256` / `updated_at` / `truncated` / `text`;项目级 `<project>/.ccteam/routing.md` 完整替换全局 `~/.ccteam/routing.md`,不合并),或 `{missing:[…]}` 列出查过的两个路径。
- `usage` —— 还剩多少余量,两个轴。见下。
- `full` —— 以上全部 + daemon 健康 + 每个可见项目的 24h 成本。运维数据只住这里。

#### `detail:"usage"` —— 你自己的 context 余量 + 每个 harness 账号的剩余额度

一次调用拿到会话雇人前要看的两个数(`full` 也带):

```json
{"you": {"sid": "s42", "context_pct": 63},
 "usage": {
   "claude": {"observed": "2026-08-31T09:12:03+00:00", "source": "status card", "subscription": "max",
              "windows": [{"w": "5h", "pct": 8, "resets": "2026-08-31T14:00:00Z"},
                          {"w": "7d", "pct": 23, "resets": "2026-09-03T00:00:00Z", "severity": "warning"},
                          {"w": "7d", "model": "Fable", "pct": 16, "resets": "2026-09-03T00:00:00Z"},
                          {"w": "credits", "pct": 3}]},
   "codex":  {"observed": "…", "source": "session release", "windows": [{"w": "7d", "pct": 12, "resets": "…"}]}}}
```

先看 `you.context_pct` —— 它决定**继续在这儿干还是开新会话**;再看各 harness 的 windows 决定**雇谁**:5h 窗口才用了 8% 的 harness 扛得住长任务,周窗口 92% 的扛不住。`pct` 是**已消耗**百分比(越大剩得越少)。带 `model` 的 `7d` 行 = 该模型自己的周窗口,点名这个模型的 spawn 受它约束;不带 `model` 的 `7d` 行 = 共享池。

诚实是构造出来的:**只有 ccteam 真的观测过的 harness 才会出现** —— 既没有它的在线会话、又没有未过期的观测,就干脆没有这一行,绝不给一个能被误读成「还有余量」的零行。每个窗口在 harness **自己声明的 reset 时刻**消失(不是某个人为的陈旧阈值),所以陈旧数字永远不会被当作当前值展示;`observed` 说明 ccteam 是什么时候听到的。读取不产生任何探针:同 harness 的在线会话只被问它内存里已有的状态(绝不发一个 turn),否则就读回已记录的观测。

同一份数据给脚本用:**`GET /api/v1/usage`** → `{"usage": {…}}`,形状完全相同,可选 `?vendor=claude`。它在普通 web-token 门内(任何已登录身份都能读):

```bash
curl -sS -H "Authorization: Bearer ccteam:$(cat ~/.ccteam/secrets/web-token)" \
     http://127.0.0.1:7331/api/v1/usage | jq '.usage.claude.windows'
```

token 文件里是裸 hex,`ccteam:` 前缀由调用方自己加。loopback 绑定时 web 门可能整个是关的,那这个 header 会被直接忽略。端口读 `~/.ccteam/run/daemon-endpoint.json`(`web_bind`)。别和 `GET /api/v1/vendors/quota` 搞混:那个是 admin-only、拿凭据打 vendor 计费 API 的**网络**探针;本路由不走网络、不碰凭据。

### `grok_claude_codex_kimi` —— 裸名发现别名

无参数;返回与 brief `status` 相同的载荷。它为只显示工具**名字**的 host 而存在 —— 面上其他地方没有 "grok" / "codex" 字样,这个名字把 vendor 关键词顶到最前面。

### `chat_send_file` —— 把文件发回你自己的 chat

`{path, caption?, kind?}` —— 把 daemon 文件系统上的文件发到绑定**你**的 chat(chat 用户打不开本地路径)。`kind`(`photo` | `document`)按扩展名推断。刻意零寻址参数;只列给 chat 可达的 caller(§2)。

## 4. 完成通知

每个 `agent` 任务都挂 watch(除非你退订),**在它自己那条请求绑定的那个 turn 的边界只报一次** —— 话痨子会话的中途叙述只进账本;同一子会话里另一个任务先完成,那是那条任务的回报、不是你的。通知 = 一行头 —— `s12 done · codex · turn 7 · ctx 19% · «核对修复» req-18d2… · 还排队 2`(85% 起带 `⚠`;失败写 `s12 FAILED (<kind>) …`)—— 加一段答案节选。头里点名**哪一条**请求答了、以及**它自己的** title(后来的派工绝不改前面那条的名字),`还排队 N` 是这个子会话还欠你多少。`turn N` 数的是**已完成**的 turn,不是被接受的消息数:给一个只完成了一个 turn 的子会话派三条任务,报的是 `turn 1`。

| `notify` | 节选 | 用途 |
|---|---|---|
| `brief`(默认) | 500 字符头尾 + 读全文的精确调用 `agent_read{sid,turn:<turn_id>,max_chars}` | 默认档:裁决 + 坐标;parent 是团队里最稀缺的上下文,全文永远一次精确调用可得 |
| `final` | 2000 字符,同样形状 | 想把全文推过来的 parent |
| `off` | 无(只记账本) | 发完不管 |

省略 `notify` 时**继承**你在这个子会话上仍未决的最近一次选择;显式给的覆盖;没有先例才用默认 `brief`。(续派时悄悄回落默认,正是一次刻意的 `final` 半路变成 443 字符 `brief` 的原因。)布尔仍认(`true`→final,`false`→off);退役的 `all` 传上来是可读错误(它的行为本来就等同 `final`)。答案被你内联取走的那次任务(`agent{wait}`,或在边界返回的 `agent_read{sid,wait}`)根本不发通知:这个决定在声明 wait 时就做了、不是事后撤销,所以两条路不可能都投递。这个抑制是按**请求**算的:你阻塞在 B 上,A 的完成照样推给你 —— 那条你并没有拿在手上。送达需要受管 parent:ccteam 把通知作为普通 user turn 追进 parent 的对话,**只送一次**。通知是独立的后续 turn,绝不是 steer:parent 正在 turn 中时,通知在该 turn 结束后立刻送达(claude 会把 turn 中写进 stdin 的一行给模型看两遍 —— 先作为 queued-command 预览,再作为下一条 prompt —— 所以边界才是它恰好被读一次的地方;暂存的那行落盘,daemon 重启不丢)。进程间隙 = 排队,resume 时送达。手起 parent 没有回程 —— 派发响应会说 `notify_deliverable:false`,`initialize` 的 instructions 一开始就讲明,用 `agent{wait}` 或 `agent_read{sid,wait}` 代替。派给不是你雇的会话 = handoff:**首次**接触照跑照记账,但不给你订阅,除非显式传 `notify`。继承是派发规则、不是子会话规则,这里同样成立 —— 你在这个 peer 上一旦点过名,后续续派就保持那个模式。

## 5. 协议细节

- **版本**:server 谈 `2025-06-18` / `2025-03-26` / `2024-11-05`。client 要别的版本 → 回 server 最新版(按规范,绝不报错)。请求头 `MCP-Protocol-Version` 若指名不支持的版本 —— 包括「有头但空值/非 UTF-8」 —— 一律 HTTP 400;不带头则由 `initialize` 谈判。
- **传输**:一个 `POST` 一条 JSON-RPC;notification 回 202 空体;`GET /mcp` = 405(无服务端推流);`DELETE /mcp` 关 enrolled binding。解析错误 = JSON-RPC `-32700` + HTTP 200。
- **annotations**:`status`、别名、`agent_read` 声明 `readOnlyHint`;`agent_stop` 声明 `destructiveHint`;`agent` 与 `chat_send_file` 声明 `destructiveHint:false`。
- **序列化**:全部紧凑 JSON(不 pretty);空/默认字段省略而非写出。
- **可观测**:daemon 对每次工具调用**以及**每次发现请求(`initialize` / `tools/list`)各打一行 INFO 日志(带 caller tier)—— 「这个会话调了几次什么」查日志即可,不靠回忆。

## 6. 护栏与信任(诚实版)

委派由 daemon 带理由地执法:深度(`delegation.max_depth`,默认 2)、扇出(每 parent 10)、每项目 50 委派、环拒绝(自己/祖先)、per-vendor 24h 预算。受管会话的 `(sid, secret)` principal 把它锁在自己项目内并归因每个动作 —— 但这是**单 OS 用户下的纵深防御,不是硬边界**:同 uid 进程终究能互读环境。它买到的是:agent 不会*误*跨项目、不会*误*冒充彼此。硬隔离(per-agent OS 用户 / 沙箱)现阶段刻意不做。

## 7. 接线与验证

- `ccteam config mcp` 向 Claude、Codex、Grok、OpenCode、Kimi 注册(各自全局配置;ccteam 只写自己那一条)。
- **DSH** 没有 ccteam 可写的配置:它的面是 DSH web 运行时里的 `@ccteam/ccteam-ui` 插件,加载时一次性注册同样六个工具(静态全面 —— 按 caller 出面只对直连 `POST /mcp` 的 harness 生效),且能把完成通知送回 DSH 对话。
- **Pi** 只在 ccteam 受管会话里拿到工具(内嵌 bridge;Pi 内名字带 `ccteam_` 前缀,只读工具自动放行);你手起的 `pi` 分毫不动。
- 随时验证:`ccteam doctor --verify-mcp` → **6 tools, 0 stubs**;`claude mcp list` 显示 `ccteam ✔`;某个会话列出的工具比别人少不是坏了 —— 那是它的面(§2)。
