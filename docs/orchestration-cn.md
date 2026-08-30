# 用你的 AI 团队 — 人话版

> English version: [orchestration.md](orchestration.md)（同构英文版,附录含 skill 作者工具速查)

**你不用记工具名——直接说就行。** 对你的会话说一句「这个重构交给 codex,完了汇报」,它就替你雇一个 codex 会话、盯着跑完、把「改了哪些文件、测试过没过」的结论拿回来给你审。会话连上以后**无需额外安装步骤**:ccteam 的 MCP server 自带使用说明,它天生就会带队。合上笔记本它也接着跑,每一跳都有账。

这就是 Claude Code 里的 Task 工具——只不过被你指挥的「subagent」是一个完整的 vendor 会话:可以是 Codex、Grok、DSH、另一个 Claude,可以在另一台机器上,而且它做的每件事都记在账本里、随时能查。

---

## 1. 三个入口

| 你在哪 | 怎么用 |
|---|---|
| **手机 / IM**(Telegram、飞书/Lark) | 直接发消息;说一句「也问问 codex 和 grok」,它自己把问题扇给几个 vendor,再把几份答案比出结论。从插件市场装 `team-brain` persona,一个会话就是你的参谋长 |
| **Web 控制台** | 浏览器里开会话、看团队树、审 diff、看成本 |
| **你日常的 coding agent 里** —— Claude / Codex / Grok / OpenCode / Kimi / DSH / Pi(本文重点) | 用一句人话委派——任何连上 MCP 的会话天生认识这套团队工具 |

人的完整入口手册见 [usage-cn.md](usage-cn.md)。本文讲第三种——**怎么在你日常的 AI 里,用一句话指挥一整个团队。**

## 2. 心智模型(30 秒)

把它想成一个小团队,你是组长:

- **你** = 组长。你说要什么,审结果,拍板合不合。
- **Codex** = 埋头干长活的同事。多文件实现、迁移、修测试、机械苦活。
- **Grok** = 快问快答 / 第二意见。「哪儿是瓶颈」「这三个方案哪个对」——一两分钟给答案(这台机器装了 grok CLI 才有)。
- **Claude** = 最深的脑子。做分解、做裁决、合并前把关审稿。

每个同事是一个**会话**,有个编号(`s47`)。会话跑在它所属**项目**绑定的机器上(本机或一台卫星)。关掉你的笔记本,它照跑;它花了多少钱、改了什么,全记在主账本上。

**一条铁律:** 想「叫另一个 agent」时,**永远不要**自己去敲 `codex exec` / `claude -p`。那样跑出来的东西没有编号、不记账、干完你也不知道、团队视图里根本看不见。值得委派的事,就值得上账本——说出来,会话自己会走正规通道(`agent` 工具)。

## 3. 你只需要会说这几句话

你的会话把工具调用藏在背后。你说左边的话,右边的事就发生:

| 你说 | 发生什么 |
|---|---|
| 「RFC-12 的实现**交给 codex**,后台跑,完了给我 diff 摘要 + 测试结果」 | 起一个 codex 会话在后台干;任务完成、子会话转空闲时来**一条**通知,diff 你自己 `git diff` 审 |
| 「**问下 grok** 这个堆栈是怎么回事——等它答完」 | 起一个 grok 会话,等一两分钟,把答案直接贴回来 |
| 「这个设计问题**分别问 codex 和 grok**,各答各的,然后给我一致点 / 分歧 / 你的裁决」 | 扇出对比:两个会话背对背作答,你的会话权衡证据下结论 |
| 「合并前找**另一家 vendor 审**这个 diff:MERGE / BLOCK 加理由」 | 跨厂商审稿门:实现者永远不给自己盖章 |
| 「这台机器**有哪些 vendor 能用**?我的路由表怎么说?」 | 一次 `status`:这台绑定主机能雇哪些 vendor、团队今天花了多少;`status{detail:"routing"}` 再把项目覆盖 / 全局 fallback 中选中的那份路由原文带上 |
| 「现在**有哪些会话**在跑?刚才那波扇出花了多少?」 | 一次 `agent_read`:名册——在忙还是空、每个成员的模型和花费;`tree:true` 才画出谁是谁的下属 |
| 「把 **s47 停了**」 | `agent_stop` 显式关掉某个会话(记录留盘,`agent_read` 仍读得到) |

经验法则:**长活 → 后台 + 完成通知**(合上笔记本没关系);**快问 → 内联等答案**。这些话在你日常会话里张口就说——什么都不用装(见 §8)。

## 4. 让委派值回票价(最佳实践,人话)

这几条是把「能用」变成「好用」的关键。每条就一句话,揉进你的原话里说:

1. **把活说清楚,并要求「简短汇报、别贴代码」。** 最大的杠杆。一句「≤25 行,分 STATUS / 改了哪些文件 / 测试结果 / 待定问题,别贴 diff」,能让同事的回复精炼十倍——否则它会把满屏日志灌进**你自己的**上下文。
2. **长活后台跑,快问快答等着拿。** 实现类交给 codex 异步跑(像同事干完来汇报);只有下一句话就要用的分钟级答案,才用 grok 内联等。
3. **结论你自己看 diff,别让它念给你听。** 让同事只汇报「改了哪些文件、为什么」,代码你 `git diff` 亲自看。
4. **合并前换个模型审一遍。** Codex 写完,合并前起一个 Claude 或 Grok 审同一个 diff——跨厂商互审能抓住同模型自审放过的坑。
5. **哪里有环境去哪里跑。** GPU 测试在 Linux 盒子上?把那台机器接成卫星、在上面注册这个仓库,然后往**那个项目**里派活——活自动跑在那台机器上。
6. **先设上限,然后信它。** 委派深度、扇出数、每日预算都有护栏,超了 daemon 会带理由拒绝。设一次,之后放心派。
7. **一次派一件事。** 一次塞三件事 = 一条含糊的汇报 + 一份要你自己拆的记录;拆成三次 = 三个清爽的检查点。

## 5. 一个真实例子(这份文档就是这么诞生的)

组长说:「把设置里『主机』和『Status』两页合并成一个自适应页面。」

1. 起了一个 **codex 会话 `s47`** 在后台干这活(异步)。
2. 几分钟后它汇报:改了 `SettingsView / App / CSS / i18n` + 测试,**Vitest 379 全绿、构建通过**,并说明「顺手修了 3 个历史 lint 错误」。
3. 组长(这里是编排的 Claude)**亲自 `git diff`** 审:确认合并干净,3 个 lint 修复是仓库里本来就红的、且改动安全。
4. 又起了一个 **claude 会话 `s49`** 做跨模型审稿,内联等了 1 分钟,拿到裁决:**MERGE,无阻断问题**。
5. 收工,`s49` 停掉,`s47` 留着以备继续改。

**组长全程只说了两句话。** 两个不同厂商的会话干活 + 互审,每一跳都在账本和团队视图里。

## 6. 模型路由(谁干什么,不靠猜)

挑谁干活靠三层,刻意分开:

- **事实,探测出来。** 默认那次 `status` 用两百来字节回答「雇谁」:你项目绑定的主机上装了哪些 vendor、团队最近 24 小时花了多少、这台主机是不是离线 / 快照是不是过期。要看面板就加 `status{detail:"vendors"}`——各 vendor 装没装、版本、诚实的 auth 信号(躺在 PATH 里绝不冒充已登录,拿不准也绝不拦你雇人)、预算态,以及这份数据是什么时候观测到的。远程主机经卫星通道上报;主机离线时给你最后一份快照并标 `stale`,绝不拿本机能力顶替。
- **目录,advisory。** 模型 id、显示名、别名档位,两个来源分开标注:**runtime 最近所见**(adapter 白拿的目录,带观测时间)和 hub **`models.json`**(社区维护)。`status{detail:"models"}` 把每个 vendor 自己的**思考强度梯**挂在旁边——它自报的档位,没自报就是 ccteam 用 CLI 实测钉死的那套。各家的梯**真不一样**(claude `low…max`、codex `low…xhigh`、grok `low|medium|high`、kimi `low|high|max`,opencode 干脆不公告共享梯,pi 的梯**按模型**走——它自报你选的那个模型到底支持哪几档),所以别拿另一家的拼写去猜,读一眼就是了。目录是参考,永不当 spawn 白名单:`model`/`effort` 在 spawn 时原文透传,不在目录里的模型照样能传,目录过期最坏是推荐过时——挡不住任何东西。但它也**绝不吞掉你的选择**:点名了 vendor 拒绝的模型或强度,spawn 直接报错,而不是悄悄按默认档跑起来。
- **观点,你的文本。** 全局分工写在 `~/.ccteam/routing.md`(缺失时由统一 home 初始化生成中立模板,绝不覆盖),可选的项目级覆盖写在 `<project>/.ccteam/routing.md`。项目文件存在时完整取代全局文件,二者不合并。它们都是 dumb markdown,无 schema。`status{detail:"routing"}` 把选中的一份原文带给任何开口问的会话(注明来源/sha/是否截断)——任何 vendor、任何主机上的规划者拿到同一份——ccteam 永不解析、不执行。

远程项目的 routing 仍是主 daemon 控制面配置:`<project>` 指 catalog 中的 daemon-side project data home;ccteam 不会偷偷同步或读取卫星工作树文件。

**流程 = 一次调用,然后雇人。** 先调 `status`(要模型 id 与强度梯就加 `detail:"models"`,要自己的笔记就加 `detail:"routing"`),再带显式 `vendor` / `model` / `effort` 调 `agent`,任务在同一次调用里交出去。真撞上没装的 vendor,这次雇佣会快速失败并附上那台主机**装了什么**——失败本身也是发现。

`routing.md` 长这样——只写例外:

```markdown
# 分工笔记

默认:不传 `model` —— vendor 默认值跟着厂商最新发布走。

| 任务类型 | vendor / model / effort | 为什么 |
|---|---|---|
| 长重构、迁移 | codex / sol-max / high | 能磨不晃 |
| 快速第二意见 | grok /(vendor 默认)/ low | 分钟级出答案 |
| 合并前终审 | claude / opus / high | 抓 builder 自己盖过章的坑 |
```

**多 vendor 对比是会话内动作,** 不是单独的产品功能。要把一个问题丢给全队:

1. **扇出** —— 同一个自足的问题,一个 vendor 一次 `agent` 调用,派给 2+ 家(异步、一次一事、`title` 标注这场对局)。
2. **各自独立作答** —— 各自独立会话,互不串味。
3. **在 turn 边界收集** —— 每个子会话转 idle 时完成通知各来一条;还缺的用 `agent_read{sid}` 补(缺席/失败的成员标记出来,绝不 kill)。
   每条完成通知只有一行头 —— `s12 done · turn 7 · ctx 19%`(`⚠` 从 85% 起;失败写 `s12 FAILED (<kind>) …`)—— 后面直接是答案节选(默认 2000 字符,`notify:"brief"` 是 500,全文永远一次 `agent_read{sid,tail:true}` 就能拿到),再无别的;`agent_read` 的名册行、transcript 与内联 `agent` 结果都给数字 `context_pct`,据此决定**复用还是新开**不用再多调一次:上下文还宽裕就继续派给它;接近告警带就为下个任务新开一个、旧的闲置。interim 通知不带状态,不多花 token。
4. **综合裁决你自己来** —— 共识、分歧、你的拍板。可选:把收来的答案回投给某个子会话互驳,或再起一个会话当裁判。

**账单始终可见。** `agent_read` 每行带累计 `cost_usd` / `tokens_total`,一场扇出花多少钱是可加总的数字,不是惊喜。

## 7. 编队(多 vendor 团队的起手式)

六个起手式在 web 控制台做成了卡片(首页,以及 团队 → 分工)——点「起手」预填 vendor 阵容;怎么打仍然是你一句人话的事:

- **总控-工班** —— 强推理总控做规划/拆解/验收;codex 开发,grok 跑生态调研;完成通知回流总控。贵模型只花在拆解与验收上,量活走便宜的专长工。
- **主力-顾问** —— grok/codex 日常主力;卡壳时在同一仓库 spawn 一个顾问会话,拿到方案让主力执行,顾问用完即停。贵模型只为难的那几分钟付费。
- **交叉互审** —— A 家写码,换 B 家冷眼 review diff,分歧回总控裁。不同模型的错误互不相关,交叉能兜住自审看不见的。
- **并行竞标** —— 同一道难题并行派给 2–3 家,对比择优、好点子合流。解空间宽的时候最值。
- **调研三角** —— grok 挖 X/实时舆情,claude 做深度网面综述,codex 读源码求证;总控汇总。没有哪个单 harness 同时有这三扇窗。
- **金字塔用工** —— kimi/opencode 磨机械量活(改名/格式化/测试分诊),失败升级贵模型。账本按成员摊开,省了多少看得见。

还有三式,不需要卡片:

- **监工模式** —— 危险操作会话用 `permission_mode:"hitl"`(批准弹到你的 IM),量产工人照跑 skip。风险有门,量活不减速。
- **定时值守** —— 给会话排程消息(输入框的时钟 / scheduled API):grok 每晨扫生态,claude 每周报仓库健康。daemon 只负责按点开火,思考都在会话里。
- **跨机编队** —— 重活项目绑到大机器卫星;拓扑带 host 徽章,记录与成本仍在一个控制台。

## 8. 装一次

对可写配置的 vendor,编排本身**无需再安装任何东西**:`ccteam config mcp`(装一次)把 ccteam server 注册进 Claude / Codex / Grok / OpenCode / Kimi,server 自带的使用说明会教任何连上的会话整套委派流程。DSH 是双向形态:你可以从 ccteam 直接雇它(`/new dsh` 或 `agent{vendor:"dsh", task:"…"}`)——雇出来的会话就跑在该身份自己的 DSH web 运行时里,实时出现在 DSH 页侧栏,插件已预载;也可以从 DSH 自己的 Web UI 出发,先跑 `dsh plugin --profile web add @ccteam/ccteam-ui`,再把 Settings → Access 里的 daemon URL 与 enrollment 凭据粘到 DSH Settings,让这个 DSH 会话成为委派父。若它还没绑定 ccteam 项目,第一次工具调用会要求点名项目 slug。Pi 不同:它也不让 ccteam 写配置,但只在 ccteam spawn 的 Pi 会话里挂 bridge——受管 Pi 会话能委派,你手起的 `pi` 一动不动。想在此之上加一个常驻指挥官 persona(路由习惯、审稿门内建)?从**插件市场**装 `team-brain`——那是口味选择,不是前提。真正的前提只有:

- 本机 `ccteam start` 起着 daemon。
- 你有一个**已注册的 ccteam 项目**并知道它的 slug;可写配置的 CLI 会话也可以从工作目录识别项目。
- 对可写配置的 vendor,用**普通 vendor 终端会话**——它读全局配置拿到 ccteam 工具(Grok 侧可 `grok mcp doctor` 验证);对 DSH,用已连接 `@ccteam/ccteam-ui` 的 DSH Web UI。(某些 SDK 驱动的会话不读用户级 MCP 配置,那种情况见 §9。)

## 9. 出问题时(人话)

| 现象 | 怎么回事 → 怎么办 |
|---|---|
| 「工具用不了 / 没有这个工具」 | 这个会话没连上 ccteam。用普通 vendor 终端会话;DSH 则安装 `@ccteam/ccteam-ui` 并在 DSH Settings 粘贴 Access 凭据。SDK 会话可直接调 `POST http://localhost:7331/mcp` + `Authorization: Bearer ccteam-enroll:<id>:<secret>`(设置 → 接入 里签发,并带上 `initialize` 返回的 `Mcp-Session-Id`)——同一套工具,而且 caller 在账本里有自己的行,它 spawn 出来的是它的子会话而不是一堆根节点。 |
| 「它半天没动静」 | 它在**干活(working)**,不是卡住。去干别的,一会儿回来看结论。 |
| 「找不到项目」 | 你不在已注册项目目录里。`cd` 进去,或把项目名说出来让会话带上 `project:"<slug>"`。 |
| 「grok 用不了」 | 这台机器没装 grok CLI。`ccteam status` / capabilities 看这台机器实际有哪些 vendor。 |
| 「派活翻车 / 想确认没重复派」 | `agent` 支持 `idempotency_key`,同键重试重放原来那次调用而不是再派一次(雇人按项目、续派按子会话各自计域);链路不稳时要求带上,或重试前先 `agent_read` 看一眼。 |

---

## 附录:工具速查(给 persona / skill 作者与想手搓的人)

平时你不用报工具名——会话听懂人话自己调。但如果你在**写 persona / skill** 或想手动编排,ccteam 在 `ccteam` 这个 MCP server 下暴露 6 个工具,在 Claude 里叫 `mcp__ccteam__<名字>`:

- **`agent`** — 雇一个同事并在同一次调用里把第一件事交给它,或者给已有的同事再派一件。`{task, sid?, vendor?, wait?, model?, effort?, role?, project?, title?, notify?, tools?, mode?, permission_mode?, idempotency_key?, parent_sid?}`。`task` **必填**,原文转发成一个 user turn,零注入;没有「只建不派」的形态。**不带 `sid` = 雇人**:`vendor` 选 harness——`claude`(默认)/ `codex` / `grok` / `opencode` / `kimi` / `dsh` / `pi`——返回的永远是一个**新** `sid`。**带 `sid` = 续派**:那个会话接下一件事,`released` 的会先按 sid 恢复;此形态下只属于雇人的参数(`vendor` / `model` / `effort` / `role` / `mode` / `permission_mode` / `tools` / `parent_sid`)一律报错拒绝,而不是悄悄忽略。**没有 `host`、也没有 `protocol` 参数**——执行机器继承自项目绑定,wire 通道由 vendor 派生(claude/codex = stream-json;grok/opencode/kimi/dsh = ACP;pi = 它自己的 RPC),传了就是硬错误;`wait_seconds` 同理——内联等待这个参数叫 `wait`。
  - `wait` —— 内联阻塞的秒数,0–240;`0`(默认)= 异步。超时返回 `status:"pending"`,**绝不取消子会话**。
  - `notify` —— 子会话 turn 边界怎么叫醒你:`final`(默认,一条通知带 2000 字符的答案头尾节选,外加一个指向 `agent_read{sid,tail:true}` 的指针)、`brief`(同上,500 字符)、`all`(保留档 —— 目前行为等同 `final`:中途叙述从不通知,只进账本)、`off`(只记账本)。布尔值仍然认。
  - `tools` —— 子会话自己的 ccteam 工具面:`full`(默认)/ `read`(只有 `agent_read`)/ `none`。**撞到委派深度上限**的子会话自动降为 `read`——叶子不用背一本它根本用不上的雇人手册。
  - `model` / `effort` —— 原文透传给 vendor,不传就吃 vendor 默认。目录是 advisory,永不拦你传什么;但 vendor 说了算:点名它拒绝的模型或强度,这次雇佣直接报错,而不是悄悄按默认档跑起来。
  - `role` —— `.claude/agents/<role>.md` persona;不传 = roleless(裸 vendor 读项目自己的 `CLAUDE.md`/`AGENTS.md`,多数时候是对的默认)。grok/opencode/kimi/dsh 当前只支持 roleless,会忽略它。
  - `mode` —— 只有 DSH 收:决定工具集的 agent preset,`standard`(默认)| `ptc` | `minimal` | `creator`;雇佣会话另跑 `danger-full-access` 权限 preset,工具执行免审批。其它 vendor 传非空 `mode` 一律拒绝。
  - `permission_mode` —— `skip`(默认)或 `hitl`,后者把不在允许列表里的工具调用弹到你绑定的 chat 上批准 / 拒绝。
  - `title` ≤80 字符,只做账本 / 团队视图标签,永不进 prompt;`project` 点名 workspace(enrolled client 首次调用必填,之后钉死);`idempotency_key` 让同键重试重放原来那次调用而不是再派一次(雇人按项目、续派按子会话各自计域);`parent_sid` 是「ccteam 没托管你」时你自己的 sid,好让委派边不丢。
  - **返回体都是紧凑 JSON。** 异步:`{sid, turn_id, status:"pending"}`,你没有接收通知的回路时再加 `notify_deliverable:false`——那就改成轮询 `agent_read`。内联:`{sid, turn_id, turn, status:"completed"|"failed", context_pct?, cost_usd?, result_text, error_kind?, error?}`,其中 `result_text` 限 4000 字符(头 70% / 尾 30%,同样带指向全文的指针)。幂等重放的调用会多一个 `idempotent_replay:true`。
  - `dsh` 与 `pi` 只在 daemon 本机跑:把它们派进绑定卫星的项目会直接报错,绝不悄悄换台机器。雇出来的 DSH 会话跑在该身份的 DSH web 运行时里——DSH 页可见、可点开插话,插件已预载,同 sid 可冷恢复,原始 token 用量入账。
- **`agent_read`** — 读团队;给不给 `sid` 决定你读到什么。`{sid?, n?, tail?, since?, max_chars?, project?, activity?, tree?}`。
  - **不带 `sid` = 名册**:你够得着的会话,按最近活跃排序,`n` 行(默认 10,最多 500)。每行是 `{sid, vendor, model?, role?, title?, activity, residency?, context_pct?, parent_sid?, is_self?, waiting_approval?, host?, cost_usd?, tokens_total?}`,空字段省略(`is_self` 标出你自己那行);只有截断时才出现 `truncated:true` 与 `total`。用 `project` / `activity`(`working` | `idle` | `stale` | `stuck` | `all`)过滤;`tree:true` 只在**返回的这些行**上铺委派拓扑,不多铺一层。只有 ccteam 没握着进程的行才带 `residency`:`released` = 会话还在,下次 `agent{sid}` 自动恢复(**复用它,别再雇一个双胞胎**),`stopped` = 用户已结束。
  - **带 `sid` = 那个会话的 transcript**,默认**最新在前**:没传 `since` 时 `tail` 默认 true;传了 `since` 就从那个 `turn_id` 游标往后翻。`n` 默认 10 条 turn,`max_chars` 默认 4000(500–50000)且是这几条 turn 的总预算;超出的内容保留头 70% / 尾 30% 节选并显式标出指针,全文永远在账本里。返回体 = `{activity, context_pct?, cursor?, cost_usd?, tokens_total?, residency?, truncated?, turns:[{turn_id, content, outcome?, error_kind?, error?}]}`。`turns` 为空 = **还没有答案**;`activity:"working"` = 正在 turn 中(过会儿再来,或用 `since:<最后一个 turn_id>` 只取增量)。
  - `limit` 是硬错误——行数 / turn 数的上限这个参数叫 `n`。
- **`agent_stop`** — 进 `{sid}`,出 `{sid, stopped:true}`。这是**显式命令,绝不是主动 kill**:记录留盘,`agent_read{sid}` 照样读得到;agent 只能停自己的后代。ccteam 自己只有两个自动刹车:每日 per-vendor 预算触顶拒**新**活,live 容量超限优雅释放最闲的会话——**创建永不因容量失败**。
- **`status`** — 这个项目的主机能雇哪些 agent、团队今天花了多少,**分级**返回。默认 `brief` 只有两百来字节:`{project, host, cost_24h_usd, hire:[…]}`——`hire` 就是绑定主机上真正装了的 vendor——卫星离线或快照过期时多出 `host_online:false` / `stale:true`,有 vendor 触顶时多出 `budget_disabled:[…]`。要更厚就加 `detail`,而且只有你开口才付这笔字节:
  - `models` —— 各 vendor 观测到的模型 id 与思考强度梯(runtime 最近所见,带观测时间),**外加** hub `models.json` 目录,两个来源分开标注。都是 advisory,永不当雇人白名单。
  - `vendors` —— 各 vendor 装没装 / 版本 / 诚实的 auth 信号 / 预算态、观测时间,以及 pi、dsh 的桥接说明。
  - `routing` —— 你的分工笔记原文(`source`、`sha256`、`updated_at`、`truncated`、`text`),或者 `{missing:[…]}` 把它找过的两个路径都列出来。
  - `full` —— 以上全部,再加 daemon 健康和你可见的每个项目的 24h 成本。运维数据**只**住这里,不会搭顺风车混进普通调用。
- **`grok_claude_codex_kimi`** — 裸名发现别名,专治只显示工具名、否则一个 vendor 关键词都露不出来的宿主。无参数,返回与 brief `status` 同一份载荷。
- **`chat_send_file`** — `{path, caption?, kind?}`,把 daemon 文件系统上的文件发回你自己绑定的 chat——chat 那头的人打不开一个路径。`kind`(`photo` | `document`)按扩展名推断。

**你到底看见几个工具,取决于你是谁。** 工具表是会话连上来时按它组合出来的:还能雇人的会话拿到全部 6 个;撞到委派深度上限(`delegation.max_depth`,默认 2)的子会话只拿 `agent_read`——叶子(一个团队里最多的那类会话)因此只为 1 个工具付费而不是 6 个;`chat_send_file` 只对「真有 chat 可发」的会话列出(根会话,或当前绑定着某个 IM/web chat 的会话);雇人时传 `tools:"read"` / `"none"` 还能再收窄。这张面在该进程的生命周期内固定不变——resume 是新进程,会重算。server 的 `instructions` 同理按面组合,总长压在 1 KB 以内:一句 ccteam 是什么、只在你能雇人时出现的「用 `agent`,别去敲 `codex exec` / `claude -p`」政策、只在你有 chat 时出现的信封说明、永远都在的附件规则(`image_path=` / `file_path=` 出现就先读那些文件再回答),外加一行身份事实——`You are s1394 in project ccteam-src.`,撞到深度上限时再加一句事实。**藏起一个工具不是权限**:`tools/call` 的门一字未改,不在你表上的工具对你就是「未知工具」。协议面上 server 谈判 `2025-06-18` / `2025-03-26` / `2024-11-05`——client 报的版本不认识就回自家最新版而不是报错,但请求头 `MCP-Protocol-Version` 点名一个 server 不会说的版本 = 400——工具本身带 MCP annotations(`status`、它的别名和 `agent_read` 是只读,`agent_stop` 是破坏性)。

**身份 & 信任(说实话):** ccteam 拉起的会话带 per-session `(sid, secret)`,只能操作自己项目,委派护栏(深度 2、每个 parent 扇出 10、每项目 50 个受派会话、防环、预算)由 daemon 带理由执行;你自己手起的会话在**第一次调用时完成注册**:vendor 配置里、或 DSH 插件设置里的 enrollment 凭据说明「这份配置是谁的」,daemon 在 `initialize` 时给这个**进程**签发身份,于是它是账本里的一行真会话,它雇的就是它的子会话。多数手起会话仍不是 ccteam 驱动的会话,完成通知没有落点(`notify_deliverable:false`)——短任务用 `wait`、否则轮询 `agent_read`;DSH 插件会话是例外,插件能把 follow-up 投回 DSH 对话里。用户域凭据不钉项目,故首个调用请带 `project:"<slug>"`(第一次点名的项目就是本次会话的 workspace,ccteam 绝不从工作目录猜,且只接受你本人可见的项目)。per-session secret 是**单 OS 用户下的纵深防御,不是硬边界**——同 uid 进程终归能读到彼此的 env。它买到的是:agent 不会*误*跨项目、每个动作都归因到已认证的调用方。真隔离(per-agent OS 用户 / sandbox)当前刻意不做。
