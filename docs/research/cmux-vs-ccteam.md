# cmux vs ccteam:可借鉴点分析

> 探索性调研文档,不代表既定路线图,不自动进上下文,按需读。日期:2026-07-08。

## 一句话定位差异

`manaflow-ai/cmux` 是**本机 macOS 原生终端**(Ghostty + Swift,GPU 渲染,非 Electron),定位"a primitive, not a solution"——它不理解、不解析任何 agent 协议,纯粹给终端窗口加**多 tab + 通知 + 可脚本化**。ccteam 是**云端常驻 daemon**,定位"IM/web 驱动的元 AI 团队"——它解析各 vendor 协议成 `CanonicalEvent`,靠持久 sid 管理会话生命周期,靠 IM 做天然的跨设备"永远在线"。

两者解决的是同一个真实痛点的两端:**"我同时开着好几个 agent 会话,怎么知道哪个需要我"**。cmux 从本机终端体验切入,ccteam 从云端会话管理切入。cmux 5 个月做到 2.38 万 star,说明这个痛点确实旺——值得把 ccteam 现有优势(常驻 daemon + IM 推送 = 天然跨设备)讲得更响,同时补几个 UX 层的真实短板。

**注意**:manaflow-ai 名下还有个姊妹项目 `manaflow-ai/manaflow`(容器化云端 VS Code workspace + diff 热力图 + PR 一键流程),架构上其实更接近"完整编排面板"这条线,但用户问的是 `cmux` 本体,以下对比以 cmux 为主,manaflow 仅在相关处提及。

## cmux 关键事实(调研摘要)

- **架构**:Swift/SwiftUI 客户端 + libghostty 渲染;Zig 写的本机 daemon `cmuxd` + Unix socket 控制层;付费层才有 TypeScript/Effect 云端控制面(Cloud VM + iOS)。
- **不管 worktree/容器**:并行全靠用户自己开 tab、自己切 branch/worktree,GitHub issue 明确说"cmux does not manage git worktrees"。
- **UI**:侧栏竖直 tab,每个 tab 直接标注 branch / PR 状态 / cwd / 监听端口 / 最新通知摘要;新建任务 = 开个 tab 直接跑 CLI,无向导;监控 = agent 自己的 TUI 原样渲染在 pane 里,cmux 不提供自己的 diff/transcript 视图。
- **通知是旗舰功能**:pane 有请求时 tab 亮蓝圈,侧栏 badge,通知面板可"跳到最新未读",触发 macOS 原生通知;`cmux notify` CLI 供 agent 主动推送。
- **可脚本化的浏览器 pane**:嵌入 WKWebView,暴露 API 让 **agent 自己**抓页面 accessibility tree、按元素引用点击/填表/跑 JS——可做"改了 UI 自己验证"闭环。这是 ccteam 完全没有的能力。
- **"teams" 实验功能**:一个任务 fan-out 到 N 个并行 tab,是最接近"赛马对比"的东西,但没有自带的并排 diff/打分 UI。
- **会话恢复存疑**:文档声称重启后恢复 layout/元数据(`SessionWorkspaceSnapshot`),但独立评测(vibecoding.app, 2026)直说"目前没有真正的 live session restore"——即可能只恢复了 pane 外壳,没有真正重连一个还在跑的 agent 进程。两个说法我核对不出谁对,存疑。
- **跨设备**:免费版没有;付费 Founder's Edition(~$30/mo)才有 Cloud VM + iOS app,本质是在补"永远在线、随处可达"这个能力。
- **不解析任何协议**:能接 Claude Code / Codex CLI / Aider / Gemini CLI / Cline 等任意终端 agent,因为它就是个哑终端——不像 ccteam 有 per-vendor adapter 归一成 `CanonicalEvent`。
- **糙的地方**:仅 macOS;子 agent(Claude Code 原生 sub-agent)是否单独开 tab 有真实用户困惑(Discussion #884);开仓 5-6 个月已积压约 3250 个 open issue,权限弹窗 bug 等。

## ccteam 可借鉴的地方(按价值排序)

### 1. 把"哪个会话需要我"做成一等 UI 概念(最值得抄)

cmux 把这件事当**旗舰功能**,ccteam 目前没有对应物。IM 单会话场景下 Telegram 推送已经天然解决"有新消息通知我",但用户一旦**并行开好几个 session**(不同项目、不同 role),现在只能逐个 `/use` 或翻 web session 列表才知道谁在等批准、谁 turn 完了没读、谁卡住了。

**具体建议**:
- web ChatConsole 的 session 切换器上给每个 session 加"需要关注"态(等待 HITL 批准 / turn 已完成未读 / 报错)的 badge,顶部导航加一个全局未读汇总。
- IM 侧 `/sessions` 输出里把"等待批准中"的会话置顶高亮,而不是纯列表。
- 这个不需要新架构,`progress.jsonl` 已经是状态 SoT,只是没往 UI 层做汇总视图——是纯前端+聚合查询的活。

### 2. 会话列表要把"上下文"露出来,别让用户点进去才知道状态

cmux 每个 tab 直接标 branch / PR 状态 / cwd / 端口。ccteam 的 session 切换器/列表目前主要靠 sid + role 认;可以在列表项上直接加:vendor/protocol、当前 role、最近一次活动时间、本轮花费(cost pill 已有雏形,可以下沉到列表项级别),减少"点进去才知道这个 session 是干嘛的"的心智负担。

### 3. 把"赛马对比"从 MCP 工具变成产品化的 UI 流程

ccteam 已经有 `advise_vote`/`advise_parallel` 这两个 MCP 工具,而且去掉 `(project,role)` dedup 后**同一 role 天然可以并存多个 session**——底层原语其实比 cmux 的实验性 "teams" 更成熟。cmux 缺的是一个像样的对比 UI,ccteam 缺的是把这层原语**产品化**:一个"同一任务分发给 N 个 session(不同 model/role/vendor)+ 并排看结果/diff"的 web 视图。这是当前 ccteam 有底子但没露出来的机会,值得优先做。

### 4. 会话恢复:ccteam 架构上已经更扎实,但要让用户"看得见、信得过"

cmux 文档和独立评测在"重启后是否真的重连活进程"这件事上自相矛盾,说明这块光靠底层实现对不代表用户体感到位。ccteam v0.8.21 的 `meta.json` SoT + cold-resume + `rebuild_session_from_meta`(重启后 `--resume` 真恢复对话、sid 不变、secret 重铸)架构上其实**比 cmux 目前公开状态更可靠**。可借鉴的不是技术方案,而是**呈现方式**:web/IM 上应该显式展示"这个 session 正在重连中/已从历史恢复"的状态,而不是让用户凭感觉猜。这是巩固既有优势的小投入、高确定性的活。

### 5. 考虑一个"agent 可自驱动的浏览器预览 pane"

cmux 的嵌入浏览器 + accessibility-tree API,让 agent 能自己截取页面结构、点击验证自己刚改的 UI,是这次调研里**最具体的新能力点子**,ccteam 完全没有对应物。如果 ccteam 用户群里有相当比例做 web 前端/全栈开发,这值得评估:web ChatConsole 给 session 挂一个"检测到 dev server → 起预览 pane",再加一个 MCP 工具把 accessibility snapshot + click 暴露给 agent 自己调用,形成"改代码→自己在预览里验证→继续改"的闭环。成本不低,但差异化明显,值得作为一个探索方向记录,不建议短期排期。

### 6. 设计哲学的对照,不是抄,是提醒

cmux 刻意"不管 worktree、不管 diff review、不管 PR",赌"给开发者原语,workflow 自己长出来"。ccteam 更重(daemon 托管的会话生命周期、sid、批准门),这本身没问题——用户是通过 IM/web 远程指挥,不是本机敲终端,天然需要更多托管。但这个对照提醒:`ccteam-flow` 编排器(目前 deferred,daemon 不 tick)如果重启,别一上来就做成 cmux 反对的那种"自称解决了 workflow 问题"的顶层强编排,CLAUDE.md 里"倾向 prompt 层 skill over `session_*` 工具"这个方向本身是对的,建议坚持。

### 7. 子 agent 的可见性:确认现状是对的,不需要抄

cmux 因为 Claude Code 原生 sub-agent 是共享同一终端的子进程,导致"子 agent 有没有自己 tab"这件事让用户困惑(Discussion #884)。ccteam 目前把 `Task` 子调用折叠进父 session 的 transcript、不单独开 sid,这个选择在这次对照下反而是对的——不需要照抄 cmux 那种"给子 agent 单独开 tab"的模糊语义,维持现状即可。

## 不需要跟进的点

- **本机终端渲染性能**(cmux 主打 GPU 渲染、非 Electron):ccteam 的 rmux 字节级镶像终端 tab 已经是逐字节保真,这条轴上没有明显差距,不必额外投入。
- **免费核心 + 付费云端/跨设备**的商业模式:cmux 靠付费 Founder's Edition 补"永远在线+跨设备",而这恰好是 ccteam **架构默认自带**的东西(云端常驻 daemon + IM 天然跨设备)。这不是要借鉴的地方,而是可以拿来对外讲故事的验证——cmux 的付费转化点正是 ccteam 免费就有的架构优势。

## 结论 / 优先级建议

按"投入小、确定性高"到"投入大、差异化强"排序:

1. Session 列表/切换器加"需要关注"聚合 badge + 上下文信息露出(借鉴 1+2,纯前端聚合)
2. 会话恢复状态在 UI 上显式呈现(借鉴 4,巩固已有优势)
3. 把 `advise_vote`/`advise_parallel` + 多 session 并存原语产品化成一个对比视图(借鉴 3,ccteam 底子好,缺产品化)
4. Agent 可自驱动的浏览器预览 pane(借鉴 5,差异化功能,建议先记录、不排期)

编排层(`ccteam-flow`)保持 deferred、不重编排的判断不变(借鉴 6 是提醒而非行动项)。
