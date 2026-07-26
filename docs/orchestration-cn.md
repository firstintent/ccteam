# 用你的 AI 团队 — 人话版

> English version: [orchestration.md](orchestration.md)（同构英文版,附录含 skill 作者工具速查)

**你不用记工具名——直接说就行。** 对你的会话说一句「这个重构交给 codex,完了汇报」,它就替你雇一个 codex 会话、盯着跑完、把「改了哪些文件、测试过没过」的结论拿回来给你审。**没有安装步骤**:ccteam 的 MCP server 自带使用说明,任何连上的会话天生就会带队。合上笔记本它也接着跑,每一跳都有账。

这就是 Claude Code 里的 Task 工具——只不过被你指挥的「subagent」是一个完整的 vendor 会话:可以是 Codex、Grok、另一个 Claude,可以在另一台机器上,而且它做的每件事都记在账本里、随时能查。

---

## 1. 三个入口

| 你在哪 | 怎么用 |
|---|---|
| **手机 / IM**(Telegram、飞书/Lark) | 直接发消息;说一句「也问问 codex 和 grok」,它自己把问题扇给几个 vendor,再把几份答案比出结论。从插件市场装 `team-brain` persona,一个会话就是你的参谋长 |
| **Web 控制台** | 浏览器里开会话、看团队树、审 diff、看成本 |
| **你日常的 coding agent 里** —— Claude / Codex / Grok / OpenCode / Kimi(本文重点) | 用一句人话委派——任何连上 MCP 的会话天生认识这套团队工具,**零安装** |

人的完整入口手册见 [usage-cn.md](usage-cn.md)。本文讲第三种——**怎么在你日常的 AI 里,用一句话指挥一整个团队。**

## 2. 心智模型(30 秒)

把它想成一个小团队,你是组长:

- **你** = 组长。你说要什么,审结果,拍板合不合。
- **Codex** = 埋头干长活的同事。多文件实现、迁移、修测试、机械苦活。
- **Grok** = 快问快答 / 第二意见。「哪儿是瓶颈」「这三个方案哪个对」——一两分钟给答案(这台机器装了 grok CLI 才有)。
- **Claude** = 最深的脑子。做分解、做裁决、合并前把关审稿。

每个同事是一个**会话**,有个编号(`s47`)。会话跑在它所属**项目**绑定的机器上(本机或一台卫星)。关掉你的笔记本,它照跑;它花了多少钱、改了什么,全记在主账本上。

**一条铁律:** 想「叫另一个 agent」时,**永远不要**自己去敲 `codex exec` / `claude -p`。那样跑出来的东西没有编号、不记账、干完你也不知道、团队视图里根本看不见。值得委派的事,就值得上账本——说出来,会话自己会走正规通道(`session_*`)。

## 3. 你只需要会说这几句话

你的会话把工具调用藏在背后。你说左边的话,右边的事就发生:

| 你说 | 发生什么 |
|---|---|
| 「RFC-12 的实现**交给 codex**,后台跑,完了给我 diff 摘要 + 测试结果」 | 起一个 codex 会话在后台干;任务完成、子会话转空闲时来**一条**通知,diff 你自己 `git diff` 审 |
| 「**问下 grok** 这个堆栈是怎么回事——等它答完」 | 起一个 grok 会话,等一两分钟,把答案直接贴回来 |
| 「这个设计问题**分别问 codex 和 grok**,各答各的,然后给我一致点 / 分歧 / 你的裁决」 | 扇出对比:两个会话背对背作答,你的会话权衡证据下结论 |
| 「合并前找**另一家 vendor 审**这个 diff:MERGE / BLOCK 加理由」 | 跨厂商审稿门:实现者永远不给自己盖章 |
| 「这台机器**有哪些 vendor 能用**?我的路由表怎么说?」 | 一次 `status`:本项目绑定主机的 vendor 面板 + 项目覆盖/全局 fallback 中选中的路由原文 |
| 「现在**有哪些会话**在跑?刚才那波扇出花了多少?」 | 列出团队树:谁是谁的下属、在忙还是空、每个成员的模型和花费 |
| 「把 **s47 停了**」 | 显式关掉某个会话(状态留着,以后能恢复) |

经验法则:**长活 → 后台 + 完成通知**(合上笔记本没关系);**快问 → 内联等答案**。这些话在你日常会话里张口就说——什么都不用装(见 §7)。

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

- **事实,探测出来。** 一次 `status` 调用返回**厂商面板**——按你项目绑定的主机出:各 vendor 装没装、版本,诚实的 auth 信号(`ready` / `not_ready` / `unknown`——躺在 PATH 里绝不冒充已登录,`unknown` 也绝不拦 spawn),预算态,主机在线还是快照已过期。远程主机经卫星通道上报;主机离线时给你最后一份快照并标 `stale`,绝不拿本机能力顶替。
- **目录,advisory。** 模型 id、显示名、别名档位,两个来源分开标注:**runtime 最近所见**(adapter 白拿的目录,带观测时间)和 hub **`models.json`**(社区维护)。目录是参考,永不当 spawn 白名单:`model`/`effort` 在 spawn 时原文透传,不在目录里的模型照样能传,目录过期最坏是推荐过时——挡不住任何东西。
- **观点,你的文本。** 全局分工写在 `~/.ccteam/routing.md`(缺失时由统一 home 初始化生成中立模板,绝不覆盖),可选的项目级覆盖写在 `<project>/.ccteam/routing.md`。项目文件存在时完整取代全局文件,二者不合并。它们都是 dumb markdown,无 schema。`status` 把选中的一份原文带给任何开口问的会话(注明来源/sha/是否截断)——任何 vendor、任何主机上的规划者拿到同一份——ccteam 永不解析、不执行。

远程项目的 routing 仍是主 daemon 控制面配置:`<project>` 指 catalog 中的 daemon-side project data home;ccteam 不会偷偷同步或读取卫星工作树文件。

**流程 = 一次调用,然后 spawn。** 调 `status`,读面板和笔记,然后带显式 `vendor` / `model` / `effort` 去 `session_spawn`。真撞上没装的 vendor,spawn 会快速失败并附上那台主机**装了什么**——失败本身也是发现。

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

1. **扇出** —— 同一个自足的问题 `session_spawn` 给 2+ 个 vendor(异步、一次一事、`title` 标注这场对局)。
2. **各自独立作答** —— 各自独立会话,互不串味。
3. **在 turn 边界收集** —— 每个子会话转 idle 时完成通知各来一条;还缺的用 `session_collect` 补(缺席/失败的成员标记出来,绝不 kill)。
4. **综合裁决你自己来** —— 共识、分歧、你的拍板。可选:把收来的答案回投给某个子会话互驳,或再起一个会话当裁判。

**账单始终可见。** `session_list` / `session_collect` 每行带 model 和累计 `cost_usd` / `tokens_total`,一场扇出花多少钱是可加总的数字,不是惊喜。

## 7. 装一次

编排本身**无需安装任何东西**:`ccteam config mcp`(装一次)把 ccteam server 注册进**全部五个 vendor**——Claude / Codex / Grok / OpenCode / Kimi,server 自带的使用说明会教任何连上的会话整套委派流程。想在此之上加一个常驻指挥官 persona(路由习惯、审稿门内建)?从**插件市场**装 `team-brain`——那是口味选择,不是前提。真正的前提只有:

- 本机 `ccteam start` 起着 daemon。
- 你在一个**已注册的 ccteam 项目**目录里(会话从当前目录认出项目)。
- 用**普通 vendor 终端会话**——它读全局配置拿到 ccteam 工具(Grok 侧可 `grok mcp doctor` 验证)。(某些 SDK 驱动的会话不读用户级 MCP 配置,那种情况见 §8。)

## 8. 出问题时(人话)

| 现象 | 怎么回事 → 怎么办 |
|---|---|
| 「工具用不了 / 没有这个工具」 | 这个会话没连上 ccteam。用普通 vendor 终端会话;或 `ccteam status` 看 daemon 在不在。SDK 会话可退回 `POST http://localhost:7331/mcp` + `Authorization: Bearer ccteam:<hex>`(hex = `~/.ccteam/secrets/web-token`)——同一套工具,admin 身份(spawn 是根)。 |
| 「它半天没动静」 | 它在**干活(working)**,不是卡住。去干别的,一会儿回来看结论。 |
| 「找不到项目」 | 你不在已注册项目目录里。`cd` 进去,或把项目名说出来让会话带上 `project:"<slug>"`。 |
| 「grok 用不了」 | 这台机器没装 grok CLI。`ccteam status` / capabilities 看这台机器实际有哪些 vendor。 |
| 「派活翻车 / 想确认没重复派」 | `session_spawn`/`session_dispatch` 支持 `idempotency_key`,同键重试永不重复创建;链路不稳时要求带上,或重试前先 `session_list` 看一眼。 |

---

## 附录:工具速查(给 persona / skill 作者与想手搓的人)

平时你不用报工具名——会话听懂人话自己调。但如果你在**写 persona / skill** 或想手动编排,ccteam 在 `ccteam` 这个 MCP server 下暴露 8 个工具,在 Claude 里叫 `mcp__ccteam__<名字>`:

- **`session_spawn`** — 雇一个同事(可顺手交第一个任务)。`{vendor, title, task?, wait_seconds?, notify?, idempotency_key?, role?, model?, effort?, permission_mode?, project?}`。`vendor`=`claude`(默认)/`codex`/`grok`/`opencode`/`kimi`。**没有 `protocol` 参数**——wire 通道由 vendor 派生(claude/codex = stream-json;grok/opencode/kimi = acp),传入就是硬错误,与 `host` 相同;`role` 指 `.claude/agents/<role>.md` persona,不传=roleless(裸 vendor 读项目自己的 `CLAUDE.md`/`AGENTS.md`,多数时候是对的默认);`model`/`effort` 原文透传给 vendor——不传吃 vendor 默认,模型目录是 advisory、永不拦你传什么;`title` ≤80 字符,只做账本/团队视图标签,永不进 prompt;`permission_mode:"hitl"` 把工具批准弹到绑定的 IM。**没有 `host` 参数**——执行机器继承自项目绑定,传了就是硬错误。`wait_seconds>0` 内联等答案;默认异步。返回永远是**新** `sid`;响应里的 `caller` 标明认证身份——`ambient:<sid>`(ccteam 会话调的,它就是子会话的 `parent_sid`)或 `admin`(owner 前门 / 主会话 fallback,**永远是根 spawn**、`parent_sid: null`)。期望有父边却看到 `caller: "admin"`,说明这次调用走的是 admin 鉴权的 MCP server 而非你会话自己的 bearer。
- **`session_dispatch`** — 给现有会话再派一件事(`{sid, task, wait_seconds?, notify?}`)。原文转发,零注入;派给自己或祖先会被拒(防环)。默认异步:**子会话一整个 vendor turn 干完、转 idle 时,只发一条完成通知**(话痨子会话的中途叙述不通知、只进账本);通知里明确写「已 idle、在等下一个 dispatch」——任务没真完,这就是你补派下一步的信号(「静默停摆」不再存在:idle 必有信号)。`notify` 选模式:`"final"`(默认)/`"all"`(每条消息都通知,调试用)/`"off"`(只记账本)。`wait_seconds`(≤600)阻塞到 turn 真正干完、返回**最终** `result_text`(中途叙述不会提前结束等待),超时返回 `pending`(子会话继续跑,绝不取消)。
- **`session_collect`** — 不进会话读它的输出(`{sid, tail?, n?, since?, max_chars?}`)。看 `activity`:`working`=在干(去轮询)/`idle`=干完了(去读)。返回限幅(默认 10k 字),长文本头 70% + 尾 30% 摘录,全文永在账本;并带累计账:`cost_usd`(有价表的 vendor)+ `tokens_total`(原始 token 数——只要 vendor 报 usage 就有,codex/grok/opencode/kimi 不再一片空白)。
- **`session_list`** — 委派树(谁是谁的下属、忙闲、成本/token、`parent_sid`),按最近活跃排序。支持 `{project?, activity?, limit?}` 过滤(默认最多 30 行,截断时带 `truncated`/`total`;空字段省略),大船队不再灌爆你的上下文。web 团队视图渲染的是同一张图。
- **`session_stop`** — 显式关掉一个 `sid`(状态留盘,可冷恢复)。ccteam 只有两个自动刹车:每日预算触顶拒新活、live 容量超限优雅挤停最闲的会话——**创建永不因容量失败**。
- 另加 **`status`**(daemon 健康 + 会话 + 今日成本,外加 caller 项目绑定主机的厂商面板——各 vendor 安装/auth/预算、已装 vendor 的 spawn 配方、advisory 模型目录、原文透传的分工笔记;见 §6)、其裸名发现别名 **`grok_claude_codex_kimi`**(响应完全一致;专治只显示工具名的宿主搜不到 vendor 关键词),与 **`chat_send_file`**(把 daemon 文件系统上的文件发回你绑定的 chat)。

**身份 & 信任(说实话):** ccteam 拉起的会话带 per-session `(sid, secret)`,只能操作自己项目;你自己的主会话走 admin token fallback,能管全舰队。per-session secret 是**单 OS 用户下的纵深防御,不是硬边界**——同 uid 进程终归能读到彼此的 env。它买到的是:agent 不会*误*跨项目、每个动作都归因到已认证的调用方。真隔离(per-agent OS 用户 / sandbox)当前刻意不做。
