# AGENTS.md — ccteam 实现导引

> 本仓库实现导引的**权威文件 = `AGENTS.md`**;`CLAUDE.md` 是指向它的软链(symlink)。面向**下一次接手 ccteam 实现的 agent session**(Claude Code 读 `CLAUDE.md` → 本文,Codex 读本文),每次起手必读。
> 本文只留**定向图 + 红线 + 纪律 + 实战坑**;**同一事实只有一个家**,细节住 §二那几份文档与代码,此处只放指针;**协议细节(CLI / JSON / event / 路由 / 参数)一律以代码为准**。
> **治理脊柱 = `.loop/`**(state 焦点 / backlog 队列 / history 蒸馏史 / verify 门禁地图);**冷启动三读 = 本文 → `.loop/state.md` → `.loop/backlog.md`**,代码按卡面坐标按需读,不做全仓扫描。
> 历史里程碑 + 升级 migration = `docs-local/versions/v0-X-Y/README.md`(**gitignored 本机文档区**,不入库);本文只描述当前状态。

---

## 〇、架构总览(定向图;红线唯一清单 = §三,勿与本节混)

ccteam = 多 harness agent 团队的桥接与治理层:常驻 daemon(IM gateway + web + MCP)把 IM/web chat 路由到按需 spawn/resume 的 agent session;任意 session 经 8 个 MCP 工具委派任意其他 session(A2A)。**铁律:只做单 harness 做不到的(跨 vendor 身份/路由/账本/观测 + 跨机执行),永不做厂商能力。**

- **核心模型 `chat ⇄ project ⇄ session`**:session 是独立一等实体(持久 sid `s<N>`,单调、扛 daemon 重启、不复用);role / harness(claude|codex|grok|opencode|kimi,`AgentVendor` 可扩展)/ provider(model)/ protocol 都是 session 属性,同一 role 可并存多 session;host 是 project 属性,session 继承。roleless(空 role,裸 vendor 自读项目 `CLAUDE.md`)合法且是默认。
- **协议轴**:Claude 默认 `stream-json`(长驻子进程 + 双向 NDJSON,无 PTY/pane/hook);grok/opencode/kimi 走 ACP;`terminal`(tmux/rmux)维护期、规划淘汰(见 §三)。所有 adapter 归一 **`CanonicalEvent`**,gateway `spawn_event_pump` 单点消费。
- **数据面**:业务事件 = `progress.jsonl`(schema 唯一权威 `harness/progress_bridge`,`core` 只 re-export);对话原文 = `<project>/.ccteam/chat/<sid>/turns.jsonl`(按 sid;live daemon 唯一 turns writer = gateway);成本/委派树全入账本。
- **接口面**:8 个 MCP 工具 `mcp__ccteam__{status(+裸名发现别名),chat_send_file,session_*}` + `POST /mcp` streamable HTTP + REST `/api/v1`(OpenAPI = `/api/docs`)+ IM 斜杠命令面 + 统一 chat-shell web(per-session Chat|终端)。清单/参数/语义以代码与工具自描述为准(指针表 = tech-design 末尾)。
- **内容面**:引擎零内置 persona/skill/提示词;role = 项目 `.claude/agents/<role>.md`(vendor 原生 `--agent` 自读,init 不种);skill = 用户级全局库 `~/.ccteam/skills`(会话显式 attach)+ 项目 `.agents/skills`(`.claude/skills` 软链);一切内容从 ccteam-hub 装(sha256 校验、never-execute)或用户自建。
- **执行面**:daemon 不 tick、无 orchestrator 循环,只响应消息/排程;会话 = resume-by-sid + 容量挤停;编排智能 100% 用户空间(`ccteam-flow`/workflow.yaml 占位 deferred,倾向 prompt 层 skill over Rust 特性)。
- **安装面**:`curl install.sh | sh`(prebuilt binary)→ `ccteam config` 注册五 vendor 全局 MCP;**ccteam 是纯 CLI、不是 vendor 插件**。

> 旧概念已全部退役、勿从旧文档/git 史复活:orchestrator tick、模式 1/2/3、flex、session=role(`(project,role)` dedup)、fresh-1M-context、cto 内置工作流、agent-team init。现行以本文 + 代码为准;验证优先用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`),不退 baseline。

## 一、当前状态

| 项 | 值 |
|---|---|
| Workspace version | `0.9.10`(**已发布** 2026-07-26:main `180e91b` + tag,四平台二进制在 `/releases/latest`;下版 tag 仍需 owner 显式发话) |
| 本版 headline | MCP 工具面治理(screenshot 全退役 + 裸名发现别名 + spawn `protocol` 参数删除(通道纯 vendor 派生)+ DX 净减法两轮)+ doctor 分组版式与五 vendor 启动自动注册 + web IA 改版(market 迁工作流 skills-first / ops 删 fleet 大表 / 新「接入 Access」聚合凭据四面)+ IM 回执「下一步」提示补全(/model + picker)+ restart-restore 竞态修复(恢复期 `/sessions` 确定性)+ 接入页宽屏改版(REST API 卡)+ web ACL 收敛(admin 专属只剩用户管理与全局 bot 凭据,权限 = token 身份 × 项目归属)+ 活跃消息 vendor 原生注入(adapter `TurnRouting` 契约;grok `_x.ai/interject` 真机实证 + vendor 自发 turn 合成收口防丢答;kimi/opencode 诚实降级 FIFO) |
| 焦点 / 基线 / 队列 | 唯一家:焦点·基线数字·人工门 = `.loop/state.md`;基线口径与 env-flake 族 = `.loop/verify/README.md`(**只增不减**);任务队列 = `.loop/backlog.md`;逐版蒸馏 = `.loop/history.md` |

> 开发一律落 `dev` 分支 + dev→main PR 攒版本,main 不直推(§五「分支与推送」);主分支 HEAD 以 `git rev-parse origin/main` 为准。

## 二、必读文档(tier-1 收敛 4 份 + `.loop/` 治理脊柱)

> **代码是唯一 SoT**。文档只留代码里没有的「为什么 / 架构论证 / 怎么用」;协议细节(CLI / JSON / event / 路由)一律以代码为准 —— 见 `tech-design.md` 末尾「协议 → 代码位置」指针表。**同一事实只有一个家**,其余位置只放指针;内容住错家 = 搬家优先于续写。

| 文档 | 角色 | 何时读 |
|---|---|---|
| `docs/dev/tech-design.md` | 架构 SoT(gateway daemon + 独立 session/sid + role 属性 + harness×provider + 标准资源 API)+ 协议→代码指针表 | 改架构前 / 找协议在哪 |
| `docs/dev/requirements.md` | 原始需求(核心痛点 = 验收基准) | 验收基准 / PR 痛点映射 |
| `docs/usage.md` | 用户命令手册(install→start→use→运维,纯命令) | 看怎么用 |
| `docs/orchestration.md`(+`-cn`) | 深度用户编排指南(session_* 工具面 + 身份模型 + 多机语义 + 最佳实践,owner 钦点独立成文) | 写/改 A2A 编排面 |
| `.loop/state.md` | 当前焦点 + 基线数字 + **人工门登记** + 未固化教训(每版回填,dev 只读) | 每次起手(三读之二) |
| `.loop/backlog.md` | **任务队列唯一来源**(文件头 = 取活/回写协议,卡面自包含) | 取活/排卡(三读之三) |
| `.loop/verify/README.md` | 门禁地图:改动面→Makefile target + 判据 + 运行纪律;`writeback.sh` = 队列结构校验(dev 收口必跑;写权执法 = 声明 + 复核,见 §五) | 收口前 |
| `.loop/history.md` | 每版一行蒸馏史(repo 内唯一版本时间轴) | 找版本脉络 |

历史版本归档 `docs-local/versions/v0-X-Y/README.md`(冻结、按需)+ 探索研究 `docs-local/research/`(不更新、按需)—— **均在 gitignored `docs-local/`,不入库不推送**(owner 决策:版本档案 + 研究笔记本机留存,仓库瘦身);进行中的版本 PRD/dev-plan 也落 `docs-local/versions/v0-x-y/`。这些都**不**自动进上下文。

**对照参考**(`references/` gitignore 不入库):`references/claude-code/` + `references/codex/codex-rs/` + `references/opencode/`(OpenCode ACP)+ `references/kimi-code/`(Kimi ACP)+ `references/OpenHands/`(同层竞品)+ `references/rmux/`。HarnessAdapter / 协议适配时翻;**不**当 ccteam 依赖。

## 三、不可触碰的架构红线

**本节是架构红线的唯一权威清单**(`docs/dev/tech-design.md` §0 只放速查 + 就地论证,引用本节)。两条用户进入层(IM + web)都守。任何 PR 不得违反:

| 红线 | 怎么守 |
|---|---|
| **No prompt injection** | role 行为住 `.claude/agents/<role>.md`,vendor 原生 `--agent <role>` 让它自读,**不**向 pane / app-server 注入 system prompt;**roleless session(空 role)= spawn 省略 `--agent`**(裸 claude 自读项目 `CLAUDE.md`)= 同一红线的合法形态(不注入 ≠ 必须有 role);`/compact /new /clear` 完全透传 |
| **引擎零 LLM** | ccteam 引擎自身不调用任何 LLM —— 无中介模型、无内置 judge、无智能摘要/智能路由;**LLM 推理只发生在 agent session 内部**,任何前端/网关不得引入新 LLM 层 |
| **daemon 无自主内容决策循环** | 只响应消息、排程与连接;**引擎不产生任务、不选择工作**,编排智能永在 agent 会话/用户空间 |
| **`progress.jsonl` 是 state SoT** | `harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export;chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<sid>/turns.jsonl`(**按 sid**;gateway `spawn_event_pump` 是 live daemon 唯一 turns writer)|
| **session = 独立一等实体;role 是属性** | session 有持久 `sid`(`s<N>`,单调、扛 daemon 重启、不复用);**同一 role 可并存多 session**;turns/marker 全按 sid(terminal 协议的 pane/env 定位同样按 sid,命名细节以代码为准);会话生命周期 = **spawn-on-demand + 按 sid resume(resume-by-session-id)+ 容量挤停**(见「永不主动 kill」例外),**非**常驻吊着;chat 复用 context 是 feature |
| **ACL = 一个身份解析器 + 一套归属策略,两个前端共用(fail-closed)** | **①身份**:`Gateway::principal` 是唯一解析器,三态 —— **Operator**(admin web token;或**在全局 bot 允许列表里被点名**的 chat)/ **Tenant**(`<platform>@<tenant>` bot 或 per-user web token → `user:<tenant>`)/ **Guest**(进了门但没被点名:只拥有自己建的,看不到任何 project)。**「够得着 bot」永不等于「是 operator」**:允许列表 = 点名册,`"*"` 通配**点名了零个人**故授权零个人(`bind_operator_allowlist`;空表 = 未配置,保留单人默认但 daemon 启动告警)。**②归属**:project 是归属单元(`ProjectState.owner` 显式字段),session 继承;唯一策略 = `ccteam_core::identity::{can_see_owner, can_see_session_owner}`,IM/web/MCP 全走它。session 可见 = **自己建的 ⊕ 自己身份的 web 控制台池**(admin `user:web-api` / tenant `user:<id>`);IM chat 之间互相隔离,web 不看 IM session;「同-current-project 互看」曾上线又被**反转删除,勿再加回**。**③解析永不越界**:`current_project_for` 只在**本人可见集**里落地(`/cd` 选的 → daemon 默认(可见时)→ 本人首个),无可见项目 = 拒绝并给下一步,**绝不回落 daemon 默认项目**(否则租户首条消息就在 owner 仓里开 agent)。**④门**:REST **单一 choke point = `auth::project_acl_layer`**,认全部项目寻址族(`/api/v1/projects/{slug}/*` · `/api/{slug}/*` 动作与 pane 快照 · `/ws/{slug}/*` PTY),新路由自动覆盖;`/sessions/{sid}/*` 按其 project 门(**admin 同样过门**,与 `can_see_owner` 不让 admin 进租户项目一致);集合面与 SSE 按身份过滤(operator 仅多收无 slug 帧);`/ws/chat` 身份取自认证态,**不认 query**。**admin 专属只剩两面**(owner 令 2026-07-27):用户管理 `/users*` + 全局 bot 凭据 `/config/im`(tenant 对称自助面 = `/me/im`),deny→403;其余面全体登录用户可用,SPA 仅 管理员 tab 按 `GET /api/v1/me` 显隐(fail-closed)。**诚实范围**:同 OS uid 下是**软隔离(UX)、非安全边界**;真隔离 = per-user OS user/sandbox(deferred)。web↔IM 同一人复联(`linked_chat`)deferred,tenant 当前 web-only |
| **不解析终端输出** | 读 transcript jsonl + 官方 hooks fast event;**不 scrape pane**(`tmux capture-pane` 仅 dev 调试;screenshot 面已整体退役 2026-07-26,web 终端快照走 pane-snapshot 只读)|
| **terminal 协议(tmux/rmux)冻结 = 维护-only,规划淘汰** | 新 vendor / 新功能**不得**新增 tmux/rmux/PTY 依赖(OpenCode 起新 harness 一律长驻 stdio:stream-json / ACP / app-server);既有 Claude `terminal` 协议只修不扩;逐字节终端镜像不再作为新功能的验收条件 |
| **永不主动 kill 长 session** | 预算例外:`budgets.*.max_cost_usd_per_24h` 触顶 auto-disable。**容量挤停例外**:live > `sessions.max_live` 时**优雅停**最久无事件的合格会话 —— **HITL 挂起与新会话父链绝不挤,创建永不因容量失败**;被停 sid 可 resume,记 `session_evicted` progress + lifecycle 广播(挑选顺序细节以代码为准)。`project stop` / `project rm --force` 是用户**显式**命令;`/compact /new` 是合法 turn,`/new` 总铸新 sid |
| **HITL 批准边界 = vendor 原生批准门**(per-session,默认 `skip`) | 批准门走 vendor 原生通道(stream-json = `can_use_tool` 反向 RPC;terminal = `PermissionRequest` hook;都**不**注入 system prompt、不 scrape)→ daemon `permission/ask`(转发 **ccteam sid**,非 vendor session UUID)→ IM `[同意][拒绝]`;hitl spawn 走 `--permission-mode default`(**绝不** skip,否则白嫖批准),skip session 仍 `--dangerously-skip-permissions`;deny **只挡该次工具、不 kill turn** |
| **session 调度门 = daemon 校验 per-session principal `(sid,secret)`(best-effort,非硬边界)** | 5 个 `session_*` 工具由 daemon 按 principal 校验(**任意有效 session**):spawn 时 mint per-session secret,受管会话默认走 HTTP bearer `ccteam-sid:<sid>:<secret>`;`verify_session_principal` 常数时比对,caller 的 project **服务端覆写**(不信自报,只能操作自己 project);role 只是审计标签、不参与授权;`dispatch`/`stop` 是显式调度(非主动 kill),stop 限后代 + depth/children/delegated/cycle/预算护栏。**主会话 fallback**:非 ccteam 拉起的本机会话按 vendor 全局注册以 Admin 语义服务(HTTP admin bearer;Claude stdio forwarder 读 0600 `web-token` = 同 uid 证明)。**诚实范围**:单 OS-uid 全信任模型下 agent 间**无硬边界**(同 uid 可读他进程 env/文件/ptrace → 拿 secret),secret 只**抬门槛**(defense-in-depth)**不 close**;真隔离 = per-agent OS user/sandbox(deferred) |
| **委派语义 = 路由,非新引擎;完成通知非注入** | `session_dispatch` 与 IM `@handle` **同路**(路由,不是第二引擎);完成通知 = gateway 生成的一条**普通 user-role turn** 投给 parent(live = vendor steer / dead = pending FIFO + resume),与人转告同构、**非 prompt 注入**;**通知单位 = 任务(vendor turn 边界)**,中途叙述只进账本;child turns 按 child sid 落 `turns.jsonl`,委派关系写 `progress.jsonl`(schema 权威见上),**不伪造进对方 transcript**。可靠性合同:幂等 spawn/dispatch(`idempotency_key`)· at-least-once 通知扛重启(`delegation.json` 落盘 + 启动 reconcile)· append→notify 顺序 · 崩溃一致原子写;诚实:单 daemon 无 HA,是**协议语义**可靠 |
| **跨机 = host 是 project 属性,session 继承;网络方向 = 卫星拨入(反向连接)** | host 住 project catalog(`~/.ccteam/config.yaml`,条目含 `host` + `remote_slug` —— **slug 相同 ≠ 同一项目**);**spawn 面无 host 参数**(MCP/REST 传入即硬错 `HOST_SPAWN_PARAM_REMOVED`),执行位置 = project 绑定;daemon 记账(turns/progress/cost)一律 catalog slug。远程项目进入 = web 选主机新建,或 `POST /projects/import` 接入卫星已上报项目(**绝不自动接入**;撞名累加、幂等)。**卫星零监听面**:`ccteam start` 统一进程内嵌卫星客户端**出站**长连 daemon(反向拨入),远程 spawn 由卫星拨回执行通道;**卫星自己解析 binary + cwd(slug→自身注册表),主侧永不下发路径**;通道 op/心跳/退避等 wire 细节以代码为准。**terminal 永不上多机**;rebuild 一律 re-gate project 绑定(host 不符或卫星 offline → 可读错误,**绝不本地重生**);HITL 跨流回主侧;远程 verdict 钉 claude,codex/opencode/grok/kimi 远程 = 显式 NotImplemented |
| **ccteam 不改写已有项目 `CLAUDE.md`/`AGENTS.md`(空项目 scaffold 除外)** | 项目知识层归 vendor 原生(Claude 读 `CLAUDE.md`、Codex 读 `AGENTS.md`)+ 项目自己;ccteam 唯一管的指令面 = `.claude/agents/<role>.md`。唯一放宽(owner 决策):对**真空项目**(两文件都不存在)scaffold 占位 `AGENTS.md` + `CLAUDE.md` = `@AGENTS.md`,**绝不覆盖**已有内容;并把 `.ccteam/` 幂等加进项目 `.gitignore` |
| **vendor 配置足迹 = 只写自家 MCP 注册** | ccteam 对 vendor 全局配置(`~/.claude.json`、`~/.codex/config.toml`、grok/opencode/kimi 对称面)的唯一写入 = **幂等注册/修复自己的 MCP server 条目**;项目侧托管设置只写 `.claude/settings.local.json` **自己的段**;除此之外不碰用户任何配置 |
| **`ccteam-core` 零 team 名字面量** | core = primitives leaf,team 名不入 core |
| **ccteam repo 零提示词类型插件(零例外)** | role/agent/skill/workflow 的**内容**一律不进 ccteam repo,**零例外**;一切 persona/skill 住 **ccteam-hub** 或用户空间,ccteam 只读 index、按 sha 取内容、装进用户项目/全局库;编排智能 100% 用户空间。用户项目里既有的 `.claude/agents/*.md`(含历史 `cto.md`)是**用户文件**,ccteam 不删不改 |
| **跨项目记忆走官方接口** | Claude `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`;Codex `~/.codex/AGENTS.md` —— ccteam 只**读**,不代项目生成 |
| **init 布局(不种任何 role)** | 项目 `.ccteam/` 由 init 只种 `state.json` + `workflow.yaml`(`routing.md` 用户可选自建 —— init 不种,`status` 原文透传);**`ccteam init` / IM `/newproject` / API create 不种任何 role**,`.claude/agents/` 留空;ccteam 托管设置(hook + base)只写 `.claude/settings.local.json`(**绝不碰用户 `.claude/settings.json`**);`~/.ccteam` 规范布局 = `ccteam_core::canonical_home_dirs()`(doctor 查 home-drift)|
| **新建项目 slug = 目录名 + 数字累加** | `slugify(目录名)`,撞名数字累加;`ccteam init` 可在任意现有目录**就地**初始化;`--slug` 显式覆盖 |

**vendor 红线**:
- ccteam **不 vendor** Claude / Codex 二进制(`references/{claude-code,codex/codex-rs}/` git-ignore 不入库,仅协议参考;实际 spawn 走 `$PATH` 内 binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override)。
- `vendor: AgentVendor::{Claude, Codex, …}` 是 trait 一等公民(可扩展),无 default —— 必须 explicit(session spawn;`workflow.yaml` 的 vendor 同理,但编排已推迟,见 §〇)。

## 四、扩展机制速查

详 `docs/dev/tech-design.md` §6;协议/参数以代码与工具自描述为准:

| 机制 | 用途 |
|---|---|
| **role 库**(`.claude/agents/<role>.md`)| ccteam 唯一管的**项目级**指令面;init 不种、默认 roleless;用户自建或从 hub 装(`ccteam role search/add/list` / web 市场),`/role <role>` 原地换 |
| **插件市场(ccteam-hub)**(`firstintent/ccteam-hub`)| repo 之外的唯一**内容**源,四型 agent/skill/workflow/plugin:track-upstream `index.json`(pinned-sha,零 vendored body)→ 安装 = host 白名单 + sha256 校验 + **never-execute**;agent 落项目、skill 落全局库、plugin 只写项目 `.claude/settings.local.json` 两键委托 vendor 自装;入口 = CLI(`role`/`skill` 组)+ web 市场 + REST `marketplace`/`skills` |
| **CLAUDE.md / AGENTS.md** | 项目 / 用户级持久指令,**vendor 原生**(ccteam 只读,不生成)|
| **MCP** | **8 工具 0 STUB**(`ccteam doctor --verify-mcp` 防 drift):`status`(发现面:vendor 面板 + 已装 vendor 的 spawn 配方/advisory 目录/routing notes,目录**永不当 spawn 白名单**)+ 裸名发现别名 `grok_claude_codex_kimi`(纯 alias 同响应,治「宿主只显工具名」的发现失败;名字 = owner 钦点字面 2026-07-26 二改:grok 打头对准搜索场景、去 `_status` 尾缀、opencode 出列,字面锁测试)· `chat_send_file` · `session_*` 5 个 A2A 委派面(调度门与护栏 = §三红线);工具用法住工具自描述(**MCP-DX 钢线:面向 agent,改进 ≠ 加法**);ccteam-managed 会话统一走 `POST /mcp` HTTP(session bearer),普通主会话走全局 admin 注册,stdio `mcp-serve` 仅 Claude terminal/既有全局/显式 fallback |
| **Skills** | repo 零自带;全局库 `~/.ccteam/skills` = hub/整仓 source 安装唯一落点(**只能会话显式 attach**,禁 link/copy 进项目);项目自有 skill = `.agents/skills/` 实体 + `.claude/skills` 软链(`skill ensure-project`/`migrate-project`)|
| **Subagents / Hooks** | subagent = agent 内 `Task(subagent_type=…)` ad-hoc 节流;ccteam hook 只写 `.claude/settings.local.json` 自己的段 |
| **MCP 注册(五 vendor)** | `ccteam config` / daemon-start 幂等写各 vendor 全局配置(Claude `~/.claude.json`、Codex `~/.codex/config.toml`、Grok/OpenCode/Kimi 对称),任何主会话可编排;per-project `.mcp.json` 仅由 web 按需写第三方 server;repo **不**带任何 vendor 插件清单(`.claude-plugin`/`marketplace.json`/根 `.mcp.json`)|

**CLI 分组**:顶层扁平 `init / start / stop / status / config / doctor` + `project` + `session` + `role` + `host` + 隐藏 `internal`;pre-v0.6 遗留命令面已整体退役、无 alias(现行命令面以 clap 定义为准,`--help` 自描述)。

## 五、PR / 实现纪律

> **总纲:治病根,通用解优先于补丁**(owner 令 2026-07-28)。先定位缺陷所在的**层**(身份解析 / 归属策略 / 资源解析 / 门),在那一层修**一次**;在症状点打补丁 = 制造债,且下一个入口还会再犯。两条判据:
> ① **同形扫一遍** —— 一个 fallback 漏了,同形的通常还有几处。实锤(2026-07-28 串用户扫荡):ACL choke point 只认一种 URL 形状 · 身份解析 fail-open 成 admin · 项目解析回落 daemon 默认 —— 三处同病(**把调用方从未被授权的东西发出去**),分开补三次都是补丁,合起来才是一个修法。
> ② **新入口自动被覆盖** —— 修完后新增路由 / 前端 / vendor / 租户若还要再补一次,说明补在了症状点。
> 同样适用于测试:**「登记为 flake」常常是病根没找到** —— `resume_*` / `hook_*` 挂了整族的账,实为确定性缺陷(读错路径 + panic 泄漏 fault 开关 / 继承宿主 `CCTEAM_HOOKLESS`)。定性前先问「宿主给了它什么」。

1. **每个改动映射**(commit/PR 描述均可):`requirements.md` 某条痛点 + `tech-design.md` 某节;改协议**以代码为 SoT**(同步 tech-design 末尾「协议→代码」指针表)
2. **commit 用英语;agent prompt 用英语**(**产品化、简洁,非冗长**;hub vendored prompt 随上游);项目文档(CLAUDE.md / `docs/`)用中文
3. **Pre-v1.0 = 开发阶段,不留技术债**:无真实用户群,**允许大胆做更好的抽象**。**不做历史迁移** — 新旧状态数据不兼容时**不写迁移步骤/兼容分支**,直接「清旧数据(`~/.ccteam/` + 各项目 `.ccteam/`)→ 重 `ccteam init`」;deprecated 直接删,breaking rename 不留 alias。tier-1 文档**只描述当前架构**,EOL 内容去版本 dir
4. **不写 backwards-compat shim**
5. **优先编辑现有文件,不轻易新建**
6. **测试不过不算完成** — `cargo test --workspace` 退步 = block;clippy 不能新增 warning
7. **版本发布同步文档(ship gate)** — 每次 `vX.Y.Z` ship 必须同步:
   - **内部 SoT**:本文 §一(只更 version 行 + 一行 headline)+ `.loop/state.md`(焦点/基线回填)+ `.loop/history.md`(一行蒸馏)+ backlog 完成卡蒸馏移出(**队列只持现势**)+ `docs/dev/tech-design.md` + workspace `Cargo.toml` version bump
   - **用户面**:root `README.md`(**英文**,始终反映当前能力,不含版本进展/时间轴/baseline/shipped 日期)+ `docs/usage.md` ── 把本版新能力融入**当前能力描述**,不写"V0.X.Y 新增"措辞(README 规则的唯一家 = 本条,原 §三行已迁出)
   - **版本归档**:`docs-local/versions/v0-X-Y/README.md` + handoff doc 落地(**留在 gitignored `docs-local/`,不入库不推送**)
8. **beta-gating(仅 UI 层,v0.8.20 起;2026-07-28 owner 令收窄至「几乎不用」)** — **功能面默认对全体登录用户开放**,能不能碰由后端「身份 × 项目归属」判,不靠藏菜单;SPA 唯一 admin-only 面 = 设置→**管理员**(用户管理),`visibleSettingsItems` 有一行不变量测试锁死。新/不稳定功能确需只对 admin 展示时可临时按 `useMe().isAdmin` 藏,但**必须是临时的**,且**非安全/权限边界**(真权限仍走 `deny_non_admin`/`can_see_project`);毕业即移除。历史例(已退役):terminal/rmux 协议与角色选择曾 admin-only,现全员。

### 角色与写权(治理骨架;执法 = 声明 + 复核,**不做脚本硬防护**)

| 角色 | 写权 |
|---|---|
| **owner**(人) | 下令 + 签核人工门(tag·部署·红线·降基线·对外契约,登记 `.loop/state.md`);不直接写仓 |
| **规划(控制)会话 = Fable 5** | **治理面唯一作者**:本文(AGENTS.md/CLAUDE.md)+ `docs/` + `.loop/`(卡片窄写回域除外)+ `.github/`;排卡、批 review、教训蒸馏。治理面写权**归属 Fable 5 规划会话**(owner 决策 2026-07-17) |
| **dev 会话**(任意 harness,可多个) | 代码面(crates / SPA / tests / README.md)+ backlog **窄写回**(只改所取卡状态行 + 追加验证/偏差段) |

执法两层:**声明**(本节 + backlog 文件头;dev 会话发现治理面需要改 = 停手偏差申报,不自己动手)→ **复核**(Fable 5 规划会话批 review 抓写权越界;`writeback.sh`(无参数)只做**队列结构校验**,dev 收口必跑,`--selftest` 证其有牙 —— 写权**不设脚本硬防护**,owner 决策)。冲突域约定:**卡面冲突域首段 = 路径前缀**(如 `crates/ccteam-harness`),前缀重叠即同域须串行。DoD 要求越出卡面授权 = **停手**,卡面偏差申报(附最窄解锁提议)等裁决;裁决只授权提议字面,不隐性扩 scope。

### 多 session 并行编辑同一仓库

主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/dev` 起独立工作树(基线 = `origin/dev`,非 main),完事 `git worktree remove`。**并行的唯一合法形态 = 不同冲突域**(backlog 卡面字段判定,同域串行)+ 一 worktree 一写者。**主仓不变 dirty**。跨 session 见主仓 dirty:`git stash push -m "<owner> WIP"` 再切;**别盲目 `git checkout -- .`**。

### 版本开发流程(版本化迭代不变;`.loop/` 只是承载)

- **大改 doc-first,小/中改 owner 直驱**:架构级 = PRD + dev-plan 落 `docs-local/versions/v0-x-y/`(gitignored)待 owner review;拍板后**规划把 PRD 拆成卡进 `.loop/backlog.md`**(冲突域/规格/DoD/建议入口),wave = 一组卡。owner `/goal` 直驱的小/中改 = 独立卡可直接 build(owner 选)。落地走 worktree-per-wave + subagent 派工(**briefing 自包含**:规格/坐标/验收直接写进 brief)→ `workspace.package.version` bump(commit `vX.Y.Z:` 前缀)→ §五.7 ship gate 回填。
- **分支与推送 = dev + PR 攒版本**(owner 决策 2026-07-22;合并方式 owner 决策 2026-07-24):新功能/修复一律提交并推送 `dev`;**周期开始(首个新提交)即开 dev→main draft PR**(`check.yml` 只跑 main push + PR —— draft PR 让每次推 dev 都过 CI 三 job),多个提交累计 = 一个版本,收口转 ready;**merge 仅由 owner 执行,方式 = merge commit(非 squash)**——main 含 dev 完整历史,合并后 dev 即 main 祖先,**免和解直接续用**(squash 时代每轮的平凡和解合并不再需要;每版一行时间轴的家 = `.loop/history.md`,main 历史不承担此职)。**main 不直推**;`gh pr create` 可用(改 `.github/workflows/*` 仍需 SSH push,见 §六)。**tag + 部署 HELD,等 owner 显式「部署」**(merge 到 main 不等于发布)。
- **wave 范式**:每 wave 一份 `wave-N-handoff.md`(Decided / Rejected / Risks / Files / Remaining 五段固定)+ 一个 commit;subagent briefing 必含 wave acceptance gate + 上 wave handoff link。**红线:每 wave baseline ≥ 上 wave**(test pass count + clippy 0 warnings),否则不推。架构级大改可把 tier-1 文档**全量重写**放最后一 wave(docs 反映已落地代码)。

## 六、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱;本仓用 Claude Code 默认行为开发,只产出项目挂 ccteam hook
- **ccteam 的 hook 写 `.claude/settings.local.json`**(不是 `.claude/settings.json`)— local 层 gitignored、Claude 照读、与用户 settings.json 合并;ccteam 只 merge/清自己的 hook 段,**不脏用户 git**。(doctor 的 legacy-hook scrub 仍按文件名碰 settings.json,是把旧 ccteam hook 从用户文件**清出去**的一次性迁移,与此一致。)
- **`.claude/settings*.json` 的 `bypassPermissions` 是开发态便利** — 产品形态走 `--dangerously-skip-permissions`,语义不同
- **测试 `bootstrap_project` / `bootstrap_meta_project` 前必先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` 写垃圾,破坏 claude 登录
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),**不**放 lib `#[cfg(test)] mod tests`
- **测试绝不写真实生产状态(`~/.ccteam` / `~/.claude`)** — 只把 `HOME` 指到 tempdir **不够**:root 解析里 `CCTEAM_HOME` 优先级更高,shell 导出它时"隔离"写入照样打进真实目录(实锤事故:fixture bot 注册写进真实 registry → telegram allowlist 中毒 → bot 静默失联数小时)。隔离助手必须同时 pin `HOME` + `CCTEAM_HOME`;新状态面优先用 `_in(root)` 注入式 API 而非 home 派生全局函数
- **改了 `ccteam-core` 公共 API**(如 slug / role-reader 签名)→ grep 全 caller(tests / mcp_serve.rs / commands.rs / ccteam-web routes)
- **(terminal 协议)`claude [--agent <role>] --name/--resume` argv 可能漂移** — `--agent` 非空 role 才加(空=roleless 裸 claude);pane/name 按 sid(`chat_session_name(slug, sid)`);`CCTEAM_CLAUDE_BIN` env override 让测试不依赖真实 binary;生产改 `claude_tui.rs` 的 `spec_for_new`/`spec_for_resume`(stream-json 默认路在 `claude_stream_json/spawn_spec.rs`)
- **(terminal 协议)`--agent` 顶层 turn 偶发也触发 `SubagentStop`**(session 被建模为 implicit-main 的 subagent);`Stop` 始终触发,turn 完成可靠 —— **不会双发 IM 回复**(回复只走 transcript-content track,hook track 仅写 progress)。stream-json 默认路无 hook,不涉此坑
- **WSL2 / inotify-busy 宿主** `fs.inotify.max_user_instances` 易触顶,本机跑见大批 watcher/SSE/web e2e 502;`ccteam-web` 的 4 个 `ws_*` 走 tmux pipe-pane PTY(sandbox 不能流)→ **环境层**,non-WSL / 大 limit 机 OR CI 复测;不计入 baseline
- **改 `.github/workflows/*` 需 token 带 `workflow` scope**(缺则 HTTPS 推 403;fallback = SSH 推 `git@github.com:firstintent/ccteam.git`)。本机 gh = `/opt/homebrew/bin/gh`(firstintent,含 repo+workflow scope,2026-07-26 核)
- **`cargo fmt --all` commit 前必跑,一律 fmt 干净**(`rustfmt.toml` pin stable rustfmt;CI gate `check.yml::fmt` 的 `cargo fmt --all -- --check` 不过 PR 不能 merge;单文件 `rustfmt --edition 2021 <files>` 直调等价;无「drift 维持现状」特例)
- **本文件 ≤200 行** — 越长 cache 越贵,Claude 越忽略
