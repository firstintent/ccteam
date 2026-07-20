# AGENTS.md — ccteam 实现导引

> 本仓库实现导引的**权威文件 = `AGENTS.md`**;`CLAUDE.md` 是指向它的软链(symlink)。
> 面向**下一次接手 ccteam 实现的 agent session**:Claude Code 读 `CLAUDE.md`(→ 本文)、Codex 读 `AGENTS.md`(本文)—— 文中 Claude Code 特定机制,Codex 自行映射到等价工作流。每次起手必读。
> 历史里程碑 + 升级 migration 见 `docs-local/versions/v0-X-Y/README.md`(**gitignored 本机文档区** —— 版本归档 + 研究笔记已从 `docs/` 迁出、不入库),本文描述**当前状态 + 红线 + 纪律**。
> **治理脊柱 = `.loop/`**(state 焦点 / backlog 队列 / history 蒸馏史 / verify 门禁地图 + 写权守卫);**冷启动三读 = 本文 → `.loop/state.md` → `.loop/backlog.md`**,代码按卡面坐标按需读。

---

## 〇、当前架构总览

本仓已落地 **「IM 通用模式 + session 独立一等实体 + 插件市场 + 协议轴 + (v0.9.0) Agent2Agent 委派底座」**。**v0.9.0 核心 = A2A 基础能力**:任意 session 经 8 个 MCP 工具 spawn/dispatch/collect 任意其他 session —— **harness(claude/codex/grok/opencode/kimi)是 session 属性;host(local/卫星)自 v0.9.2 起是 project 属性,session 继承其 project 的绑定**,委派语义对两轴一致;引擎**零内置 persona**(废除 cto),编排智能全在用户空间;铁律「只做单 harness 做不到的,永不做厂商能力」。Claude 两条 spawn 路径:**`ClaudeStreamJsonAdapter`(`stream-json`,默认主路 —— 长驻子进程 + 双向 NDJSON,无 PTY/pane/hook)** 与 `ClaudeTuiAdapter`(`terminal`,tmux + 逐字节镜像);session facet `protocol`=`stream-json`(默认)| `terminal`(+ slash bridge / HITL / 故障矩阵)。架构 SoT = `docs/dev/tech-design.md` + 本文(**协议细节一律以代码为准**,见 tech-design 末尾「协议→代码位置」指针表)。本节 = **架构总览**(**不可触碰的红线清单见 §三**,勿与本节混)。**下文若仍见 orchestrator / 多模式(模式 1/2/3)/ flex / session=role / agent-team init,以本节为准 —— 已退役:**

- **核心模型 `chat ⇄ project ⇄ session`,role 是 session 的属性**:一个 chat = 你的终端(IM chat 或 web)→ 切 project → spawn/resume **session**。**session 是独立一等实体**,有持久 `sid`(`s<N>`,单调、扛 daemon 重启、不复用);**role 降为 session 的一个属性**(spawn 时绑 `--agent <role>` persona)。**同一 role 可并存多个 session**(去掉了 `(project,role)` dedup)。session 启动:**默认 `stream-json`**(长驻子进程,无 hook/pane);`terminal` 协议才走 `claude [--agent <role>] --name|--resume`(tmux send-keys + hooks,`Stop`→`chat_turn_completed`)。role 库 = 项目级 `.claude/agents/<role>.md`;**v0.9.0 起 `ccteam init` 不种任何 role**(废除 cto 内置工作流),默认会话 roleless(裸 vendor 读项目 `CLAUDE.md`);编排 persona 从 ccteam-hub 装(`fable-advisor`/`team-brain` 等示例配方)或用户自建。
- **turns / marker 全按 sid**:turns = `.ccteam/chat/<sid>/turns.jsonl`、transcript cursor / active-session marker 全按 sid;gateway `spawn_event_pump` 的 ANSWER 分支按 sid `append_turn`(live daemon 唯一 turns writer)。**terminal 协议**额外:pane = `ccteam-chat-<slug>-<sid>` + `CCTEAM_CHAT_SID` pane env(daemon HTTP 加 `X-Ccteam-Sid` → hook/in-pane forwarder 报 sid);**stream-json 默认路无 pane/hook**,sid 在 adapter 内。
- **roleless session 合法**:空 role → spawn **不加** `--agent`(裸 claude 自读项目 `CLAUDE.md`/`AGENTS.md` 当 brain);非注入(ccteam 本就不注入 system prompt,省略 `--agent` 不违反 No-prompt-injection)。
- **No prompt injection 由 `--agent` 兑现**:role 行为住 `.claude/agents/<role>.md`,vendor 原生 `--agent` 让它**自读**,ccteam **不**注入 system prompt —— 这条红线现在是**被满足**,不是被违反。
- **daemon = IM gateway + web + MCP Unix socket**(一个进程,**不 tick、无 orchestrator 循环**);会话 = resume-by-session-id(spawn-on-demand + 容量挤停(live ≤ 50,LRU 优雅停、可 resume)+ 按 sid resume,**非**常驻吊着;chat 复用 context 是 feature)。`ccteam-flow` orchestrator **存在但 daemon 不跑**(deferred)。
- **harness × provider × protocol facet**:`harness` = agentic CLI adapter(本版 claude-code 跑通;codex best-effort;gemini-cli/grok-cli/其余 = future,**可扩展 `AgentVendor` enum**);`provider` = 子 facet(model);**`protocol` = Claude 第三轴(v0.8.11)= `stream-json`(默认)| `terminal`**:`stream-json` = 长驻 `claude` 子进程 + 双向 NDJSON 管道(`ClaudeStreamJsonAdapter`,无 PTY/pane/hook 链,chat-only 主路);`terminal` = tmux PTY + TUI(`ClaudeTuiAdapter`,要逐字节终端镜像/attach/screenshot 时选)。`default_adapter_factory` 按 `(vendor, protocol)` 三路由(`crates/ccteam-im/src/daemon.rs`),两个 Claude adapter emit **同一 `CanonicalEvent`** → gateway `spawn_event_pump` 零改动消费。**`terminal` 协议(tmux/rmux/PTY)已进维护期,规划淘汰:不再为新 vendor / 新功能扩 terminal 路径**(仅维持既有 Claude terminal 会话);新 harness 一律长驻 stdio 协议(stream-json / ACP / app-server)。命名 `protocol` 非 `backend`(`backend` 留给 host 轴);session meta 的 `host` 字段自 v0.9.2 起是**继承值**(spawn 时从 project 绑定解析,记录用)。harness/provider/protocol 是 session 属性、非顶层资源;`GET /capabilities` 按 PATH probe 动态列当前可用 harness(×provider)。
- **插件市场(ccteam ↔ ccteam-hub ↔ project,v0.8.12 = track-upstream)**:role/agent/skill/workflow 的**内容**全不进 ccteam repo(唯一例外 `cto_role.md`),而住 **`firstintent/ccteam-hub`**(**v0.9.0 起零例外** —— 废除 cto 内置工作流,`cto_role.md` 已删,一切 persona 含 `fable-advisor`/`team-brain` 示例配方住 hub/用户空间)。v0.8.12 起 hub 从「vendor 拷贝每个文件」改成**跟踪 upstream 仓库**:`index.json` 只存**元数据 + 每条 `upstream`(可直接 raw 拉取的 URL @pinned-sha)**,**零 vendored body**(`sources.json` 声明整仓 @sha + glob,`scripts/sync.py` 幂等重建 index;**skill id 从目录名取**;**多文件 skill** 带 `manifest`(每文件 `relpath`+sha256);**第一方** `source=ccteam`(pk/autoloop)内容仍住 hub,`upstream` 指向 hub 自己的 raw tree)。ccteam 读 hub `index.json`(HTTPS github-raw + 本地缓存 `~/.ccteam/hub-cache/`),安装时从各条 `upstream` 拉内容 → **host 白名单**(只 `raw.githubusercontent.com` + loopback)+ **sha256 校验** → 装进用户项目:单文件复用 `write_role`/`write_skill`,**多文件 skill 落 `.claude/skills/<id>/<relpath>`**(整批 fetch+verify 后再落盘,失败不留半成品)。CLI `ccteam role search/add` 读 hub;backend = `ccteam-{im,core}/src/hub.rs` + `ccteam-web/src/routes/marketplace.rs`。**v0.8.14 加第四 type `plugin`(vendor-native Claude Code plugin,委托安装)**:hub 条目退化成**纯指针**(`sources.json` 顶层 `plugins[]` → index `{type:"plugin", marketplace:{name,source}, plugin_id}`,**无 upstream/content_sha/manifest**);安装**不 fetch/不拷贝/不执行**,改写项目 `.claude/settings.local.json` 两键(`extraKnownMarketplaces[name].source` + `enabledPlugins["<plugin>@<name>"]=true`,vendor settings schema 兜底,见 `ccteam_core::enable_marketplace_plugin`)→ Claude Code 下次启动自己 fetch/装原生依赖/跑 install.sh,**ccteam execute nothing**(红线「verbatim-copy、never execute」不破;sha 对 plugin 是 advisory,vendor 跟 live ref)。`installed_status` 对 plugin = 读 `enabledPlugins` 的二态(无 update_available);仅 Claude 轴(Codex 无 plugin 市场)。
- **标准资源 API `/api/v1`**(web-token 鉴权):**project**(GET/POST `/projects`,GET/DELETE `/projects/{slug}` —— DELETE = 注销 + 停 session,**file-purge 留 CLI**)· **role**(GET `/projects/{slug}/roles`,GET/PUT `…/roles/{role}`)· **session**(GET/POST `/projects/{slug}/sessions`,GET `/sessions/{sid}`,POST `…/turn`,GET `…/events` SSE,POST `…/stop`)· **marketplace**(GET `/marketplace` + `…/{id}/body` 预览 + GET `/projects/{slug}/marketplace`(带 installed_status)+ POST `…/marketplace/install`)· **hosts**(v0.8.18,GET `/hosts` + `/{host}` host-keyed agent 报告;POST `…/register-mcp` 唯一可写、幂等)· **status**(GET `/status` daemon 健康 + sessions live/idle + 今日 cost/budget + **per-session 成本**)· **config/im**(web-token 门后:GET masked 状态 + PUT telegram/lark + telegram chat_id 异步轮询,**REST 路由非 MCP 工具**)· GET `/capabilities` + OpenAPI(`GET /api/docs` Scalar UI、`GET /api/v1/openapi.json`,单源 `OpenApiRouter`,同 web-token 门)。session-id = gateway `s{n}`。
- **统一 chat-shell web UI + 逐字节保真终端**:两套分叉 SPA 布局收敛成**一个** chat 壳(`ChatConsole`;删旧 operator UI:Dashboard/ProjectDetail/SessionDetail/SessionsList/Teams*/WorkflowView + 侧栏/顶栏);底部全局导航 = **插件市场 / Status / 主机 / Settings**(v0.8.18 加主机页 + 界面语言中/英 + 头像个人设置),per-session Chat|终端 tab,顶栏 cost pill,轻量 Status view(backed by `GET /api/v1/status`);Roles 页被插件市场浏览器取代。终端:rmux backend 改流**裸 pane 字节**(`output_stream()`/`PaneOutputChunk::Bytes`,`capture` 排 `Oldest` backlog)→ 默认 rmux 即逐字节保真(修 v0.8.8 连上空白 + 换行歪);rmux pin **0.5**(byte API 自 0.3.1 起就有 → 保真**不依赖** 0.5;升 0.5 取 tmux-compat / window APIs,call-site 0.3→0.5 byte-identical),tmux backend 不变。
- **progress 写入权威**:`harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export。

> 验证优先用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)+ 真实 WS/HTTP smoke;不退 baseline。起手/恢复走**冷启动三读**(§二);改架构前再读 `docs/dev/tech-design.md`(架构 SoT)。

---

## 一、当前状态

| 项 | 值 |
|---|---|
| Workspace version | `0.9.6` |
| 测试 baseline | **口径**:确定性 `cargo test --workspace --exclude ccteam-web --lib` + `ccteam-web` 全量 + vitest + Playwright;**当前数字与 env-flake 族 = `.loop/state.md` + `.loop/verify/README.md`**(基线只增不减) |
| Clippy | 0 errors + 0 warnings(`cargo clippy --workspace --all-targets -- -D warnings`,含 `ccteam-web`)|
| 当前在做 | **v0.9.6 已落 `dev`(多 vendor 编排发现面:status vendor 面板(probe 下沉 `host_registry` + daemon-aware)+ advisory 模型目录三源(runtime last-seen/hub `models.json`/用户注释,永不当 spawn 白名单)+ routing notes(`<project>/.ccteam/routing.md` 覆盖 global、global 缺失生成不覆盖)透传 + spawn 失败发现面 + **compare 全链退役** + `is_claude_family`/`model_warning` 删除 + README 吉祥物入 web;未合 main、未 tag、未部署)**;v0.9.5 已落 main。铁律:**只做单 harness 做不到的**(跨 vendor 身份/路由/账本/观测 + 跨机执行),**永不做厂商能力**。当前焦点/基线/人工门 = `.loop/state.md`;任务队列唯一来源 = `.loop/backlog.md`;逐版蒸馏 = `.loop/history.md`(详档 `git log` + gitignored `docs-local/versions/`);**协议一律以代码为准** |

> 主分支 HEAD 以 `git rev-parse origin/main` 为准(开发直落 `main`;`dev` 分支已滞后停用);历史里程碑见 `docs-local/versions/v0-X-Y/README.md`(冻结归档,gitignored)。

**ccteam 是 Claude Code(+ Codex)之上的元工具** —— 云端常驻的元 AI 团队,从 IM 和 web 驱动。架构 5 块:

- **配置**:role 行为 = 项目级 `.claude/agents/<role>.md`(**v0.9.0 起 `ccteam init` 不种 role,默认 roleless**;编排 persona 走 hub/用户自建)。**项目知识层(`CLAUDE.md`/`AGENTS.md`)归 vendor + 项目自己,ccteam 不改写已有内容**(v0.8.9 owner 决策:仅对**真空项目** scaffold 占位 `AGENTS.md` + `CLAUDE.md`=`@AGENTS.md`,绝不覆盖;详见 §三红线)。**多-agent 编排已推迟**:`ccteam init` 仍 scaffold 一份 `workflow.yaml` 占位,但其声明的 agent 拓扑(trigger/并发/vendor)**当前不被驱动**(daemon 不 tick、不 orchestrate;`ccteam-flow` 未接);编排方式仍在探索 —— 倾向 **prompt 层 skill over `session_*` 工具**,非 Rust 特性。
- **执行**:resident daemon = IM/web⇄session 路由网关(**不 tick、无 orchestrator 循环**)→ 按需 spawn / resume session(按持久 sid):Claude **默认 `stream-json`**(长驻子进程 + 双向 NDJSON,无 pane/hook),`terminal` 协议才走 tmux 长 session(send-keys + transcript + hook);空 role = roleless 裸 claude,Codex best-effort;两 vendor 归一成中立 `CanonicalEvent`。
- **状态 SoT**:`progress.jsonl` 业务事件(`harness/progress_bridge` 单一权威);chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<sid>/turns.jsonl`(按 sid;不依赖 Anthropic 内部 `~/.claude/projects/`)。
- **接口**:**8 个 MCP 工具** `mcp__ccteam__{status,chat_send_file,screenshot,session_*}`(v0.9-T1 cull 15→8;`ccteam doctor --verify-mcp` 自检)+ `POST /mcp` streamable HTTP(v0.9-T4,admin bearer 必带)+ 标准资源 API `/api/v1`(含 `marketplace` + `status` + `config/im` + OpenAPI `/api/docs`)+ IM 命令面(`/cd /use /new /role /status /sessions @handle`;`@` 只指会话,无 meta-handle,确定性控制=斜杠面)+ 统一 chat-shell web(per-session `/chat/s/:sid` Chat|终端 + 底部导航 插件市场/Status/Settings)。
- **安装**:`curl install.sh | sh`(prebuilt binary,linux + macOS,Windows 走 WSL2)→ `ccteam config` 注册 MCP server(给 Claude `~/.claude.json` + Codex `~/.codex/config.toml` 都写)。**ccteam 是纯 CLI、不是 vendor 插件**,无 `/plugin` 步;`cargo install --git …` 是 fallback。

详 `docs/dev/tech-design.md`。

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

**起手 30 秒(冷启动三读)**:本文(harness 自动加载)→ `.loop/state.md`(焦点/基线/人工门)→ `.loop/backlog.md` 文件头 + 所取卡 → 干。代码按卡面坐标按需读,**不做全仓扫描**;基线重记只在收口时(门禁地图 `.loop/verify/README.md`)。

**对照参考**(`references/` gitignore 不入库):`references/claude-code/` + `references/codex/codex-rs/` + `references/opencode/`(OpenCode ACP)+ `references/kimi-code/`(Kimi ACP)+ `references/OpenHands/`(同层竞品)+ `references/rmux/`。HarnessAdapter / 协议适配时翻;**不**当 ccteam 依赖。

## 三、不可触碰的架构红线

**本节是架构红线的唯一权威清单**(`docs/dev/tech-design.md` §0 只放速查 + 就地论证,引用本节)。两条用户进入层(IM + web)都守。任何 PR 不得违反:

| 红线 | 怎么守 |
|---|---|
| **No prompt injection** | role 行为住 `.claude/agents/<role>.md`,vendor 原生 `--agent <role>` 让它自读,**不**向 pane / app-server 注入 system prompt;**roleless session(空 role)= spawn 省略 `--agent`**(裸 claude 自读项目 `CLAUDE.md`)= 同一红线的合法形态(不注入 ≠ 必须有 role);`/compact /new /clear` 完全透传 |
| **`progress.jsonl` 是 state SoT** | `harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export;chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<sid>/turns.jsonl`(**按 sid**;gateway `spawn_event_pump` 是 live daemon 唯一 turns writer)|
| **session = 独立一等实体;role 是属性** | session 有持久 `sid`(`s<N>`,单调、扛重启、不复用);**同一 role 可并存多 session**(去掉 `(project,role)` dedup);turns/marker 全按 sid;**terminal 协议**额外:pane=`ccteam-chat-<slug>-<sid>` + `CCTEAM_CHAT_SID` pane env(hook/forwarder 据此报 sid,daemon HTTP 加 `X-Ccteam-Sid`);**stream-json 默认路无 pane/hook**,sid 在 adapter 内直传 |
| **session ACL = 多用户软分区(档0 IM own/web 池 + 档1 web 项目归属)** | `chat_can_access`(`chat_owner_visible`):own = `owner == canonical_owner(chat)` \|\| 运维池 `owner.channel == "user"` —— 一个 chat 见/可 `/use`·`/stop`·`/screen`·`/sessions` **自己的** session **+ 所有 web 控制台创建的** session(web 是单一共享操作台,到档1 才 per-user;支撑「web 建、手机驱动」单用户流)。**IM 之间各自建的 session 互相隔离**(两个 telegram `chat_id` 看不到对方 IM 建的;`web` 作 QUERIER 也不看 IM session —— IM session 只匹配 `owner==chat`)。删了「同-current-project 互看」(**反转 v0.8.13 跨前端按项目共享**,勿再加回)。`ProjectState.owner: Option<String>`(`channel:chat_id`,IM `/newproject` 时记;**显式字段、非路径派生**)。**诚实范围**:同 OS uid 下是**软隔离(UX)、非安全边界**(同 uid 仍可读他人文件/`/proc/<pid>/environ`);真隔离 = per-user OS user/sandbox(deferred)。**档1(per-user web)= 项目归属模型**(owner 拍板「项目绑定用户、会话属于项目→属于用户」):**project 是归属单元**(`ProjectState.owner`,web 建项目盖 `user:<tenant>`),**session 继承其 project 的归属**(不再单独记 owner 做 ACL;session.owner 只留作回信路由)。web REST 全按 project 鉴权 —— **单一 choke point = `auth::project_acl_layer` 中间件**(层在 `auth_layer` 内,从路径抽 slug → `can_see_project` 兜底**所有** `/api/v1/projects/{slug}/*`,任何 project 面都漏不了、新路由自动覆盖);`/projects` 集合面按身份过滤(`build_projects`);`/sessions/{sid}/*`(非 `/projects` 下)由 `gate_sid` 按其 project 门(history/status/turn/resolve/events/stop)。`can_see_project(identity,slug)` = admin 见全部 / tenant 只见自己 owner 的。全局·运维面(IM 凭据 `config/im*`、主机 `hosts`+`register-mcp`、`status`、用户 `users`)= **仅 admin**(`auth::deny_non_admin`→403);`GET /api/v1/me` 给 SPA 按身份显示(对 tenant 隐藏 Status/主机/Settings nav + IM 凭据段,fail-closed `useMe`)。共享核 `Identity::{owner_tag,can_see_owner}`(owner 轴 = 合成身份 `user:`,**非投递 channel**;v0.8.20 follow-up 从 `web:` 改名去歧义 —— owner 见 `owner=web` 误以为「web 前端」)。**IM 的 `chat_can_access` 不动**(owner 的 telegram 仍 own+web 池 = 运维视角)。web↔IM 同一人复联(`linked_chat`)deferred,tenant 当前 web-only|
| **不解析终端输出** | 读 transcript jsonl + 官方 hooks fast event;**不 scrape pane**(`tmux capture-pane` 仅 dev 调试 + screenshot tool 只读)|
| **terminal 协议(tmux/rmux)冻结 = 维护-only,规划淘汰** | 新 vendor / 新功能**不得**新增 tmux/rmux/PTY 依赖(OpenCode 起新 harness 一律长驻 stdio:stream-json / ACP / app-server);既有 Claude `terminal` 协议只修不扩;逐字节终端镜像不再作为新功能的验收条件 |
| **永不主动 kill 长 session** | `budgets.{claude,codex}.max_cost_usd_per_24h` 触顶 auto-disable 是预算例外;**容量挤停是第二个 sanctioned 例外(v0.9.2)**:live > `sessions.max_live`(默认 50)时优雅停最久无事件的合格会话(idle 优先,排除 HITL 挂起 + 新会话父链;全忙挤最旧;全被排除则超额放行,**创建永不因容量失败**),被停 sid 可 resume,记 `session_evicted` progress + `session_lifecycle` 广播;`project stop` / `project rm --force` 是用户显式命令;`/compact /new` 是合法 turn;`/new` 总铸新 sid |
| **HITL 批准边界 = vendor 原生批准门**(per-session,默认 `skip`) | 批准门走 vendor 原生通道(**stream-json 默认**:`can_use_tool` 反向 RPC control_request / **terminal**:`PermissionRequest` hook;两者皆**不**注入 system prompt/不 scrape)→ daemon `permission/ask`(转发 **ccteam sid**,非 Anthropic session UUID)→ IM `[同意][拒绝]`;hitl spawn 走 `--permission-mode default`(**绝不** skip,否则白嫖批准),skip session 仍 `--dangerously-skip-permissions`;deny **只挡该次工具、不 kill turn**(守「永不主动 kill」) |
| **session 调度门 = daemon 校验 per-session principal `(sid,secret)`(best-effort,非硬边界;v0.9.0 泛化,去 cto-only)** | 5 个 `session_*` 工具(spawn/dispatch/collect/list/stop)由 daemon 按 **principal** 校验(**任意有效 session,不再限 cto role**):spawn 时 mint per-session secret;Claude `mcp.json`、Codex `thread/start.config.mcp_servers`、ACP `mcpServers` **默认全走 HTTP** bearer `ccteam-sid:<sid>:<secret>`(Codex 本地自 0.144.3 真机验证,不再每 thread 起 `mcp-serve`),`verify_session_principal(sid,secret)` 常数时比对 → 返回 `CallerCtx{sid,slug,role}`,`_caller_slug` **服务端覆写**(不信 caller 自报,只能操作自己 project)。role 降为审计标签、不参与授权;`dispatch`/`stop` 是显式调度(非主动 kill),stop 限后代 + depth/children/delegated/cycle/预算护栏。**主会话 fallback(v0.9.1)**:非 ccteam 拉起的本机日常会话按 vendor 的全局注册以 Admin 语义服务 —— Codex `~/.codex/config.toml` 走 HTTP admin bearer;Claude 既有 stdio forwarder 读 `~/.ccteam/secrets/web-token`(0600,同 uid 证明)经本地 socket 转发。**诚实范围**:单 OS-uid 全信任模型下 agent 间**无硬边界**(同 uid 可读他进程 env/文件/ptrace → 拿 secret),secret 只**抬门槛**(defense-in-depth),**不 close**;真隔离 = per-agent OS user / sandbox(deferred) |
| **委派语义 = 路由,非新引擎;完成通知非注入** | `session_dispatch` 走 `submit_to_sid`→`submit_resolved`(与 IM `@handle` 同路);完成通知 = gateway 生成的一条**普通 user-role turn** 投给 parent(live=vendor steer / dead=pending FIFO + resume),与人转告同构,**非 prompt 注入**;**通知单位 = 任务(vendor turn 边界),非每条 assistant 消息**(v0.9.5 反馈修复:话痨子会话中途叙述只进账本,turn 完成→子会话转 idle 才发一条、文案明示 idle;`notify: final(默认)/all/off`,重启 reconcile 把积压卷成一条);child turns 按 child sid 落 `turns.jsonl`,委派关系写 `progress.jsonl`(schema 权威 `progress_bridge`),**不**伪造进对方 transcript。可靠性合同(F7):幂等 spawn/dispatch(`idempotency_key`)、at-least-once 通知扛重启(`delegation.json` 落盘 + 启动 reconcile,幂等键 `(child_sid,turn_id)`)、append→notify 顺序、崩溃一致原子写。诚实:单 daemon 无 HA,是**协议语义**可靠 |
| **跨机 = host 是 project 属性,session 继承(v0.9.2);网络方向 = 卫星拨入(反向连接)** | **slug 相同 ≠ 同一项目**:project catalog(`~/.ccteam/config.yaml`)每条含 `host`(serde default `local`)+ `remote_slug`(卫星侧线上身份)+ `remote_path`(仅展示;远程条目的 `path` = daemon 侧 data home,仅 `.ccteam/state.json`);**spawn 面无 host 参数**(MCP/REST 传入即硬错 `HOST_SPAWN_PARAM_REMOVED`),执行位置 = project 绑定。远程项目两条进入路径:web 选主机新建(控制通道 op 对 `project_init{nonce,path,slug}` → 卫星就地 bootstrap + 注册自身 config → `project_init_result` ack,nonce oneshot + 15s 超时可读错误)或 `POST /projects/import` 接入卫星已上报项目(**绝不自动接入**,catalog slug 撞名走累加,幂等);ExecSpec.slug 携 `remote_slug`,daemon 记账(turns/progress/cost)一律 catalog slug。**卫星零监听面**:统一进程 `ccteam start` 内嵌卫星客户端(出站长驻 `ccteam-host.v1` 到 daemon `:7331`,presence=通道活着 + report 25s 含 agents+projects),远程 spawn = `exec_open{nonce}` → 卫星拨回 `ccteam-exec.v1` WS(`HostChannelHub` rendezvous)→ ExecSpec 首帧;稳定性合同(20s ping/75s 拆链/指数退避/踢幽灵/未知 op 向前兼容)不变;卫星自己解析 binary + cwd(slug→自身注册表),主侧永不下发路径。**terminal 永不上多机**;rebuild 三路径 re-gate **project 绑定**(meta.host ≠ entry.host 或卫星 offline → 可读错误,**绝不本地重生**);HITL 跨流回主侧。远程 verdict 钉 claude,codex/opencode/grok/kimi 远程 = 显式 NotImplemented |
| **会话 = resume-by-session-id** | spawn-on-demand + 按 **sid** resume(粒度从 `(项目,role)` 升到 session id)+ 容量挤停(见「永不主动 kill」例外)+ 扛 daemon 重启,**非**常驻吊着;chat 复用 context 是 feature |
| **ccteam 不改写已有项目 `CLAUDE.md`/`AGENTS.md`(空项目 scaffold 除外)** | 项目知识层归 vendor 原生(Claude 读 `CLAUDE.md`、Codex 读 `AGENTS.md`)+ 项目自己;ccteam 唯一管的指令面 = `.claude/agents/<role>.md`(role 库)。**v0.8.9(owner 决策)放宽**:`bootstrap_project_at_dir` 对**真空项目**(`CLAUDE.md` + `AGENTS.md` 都不存在)scaffold 一份占位 `AGENTS.md`(提示 agent 提醒用户初始化、别空跑)+ `CLAUDE.md` = `@AGENTS.md`,**绝不覆盖**已有内容;并把 `.ccteam/` 幂等加进项目 `.gitignore` |
| **`ccteam-core` 零 team 名字面量** | core = primitives leaf,team 名不入 core |
| **ccteam repo 零提示词类型插件(v0.9.0 起零例外)** | role/agent/skill/workflow 的**内容**一律不进 ccteam repo —— **零例外**(v0.9.0 废除 cto 内置工作流,删 `cto_role.md` 模板 + `CTO_ROLE_MD` 导出 + 一切种子路径)。所有 persona(自建 + agency-agents 等开源 + `fable-advisor`/`team-brain` 示例配方)住 **ccteam-hub**(`firstintent/ccteam-hub`),ccteam 从 hub 读 `index.json` + 取内容 + 装进用户项目 `.claude/{agents,skills}/`;编排智能 100% 用户空间(项目 `CLAUDE.md` vendor 原生随 vendor 进化白拿 + 用户/hub role)。既有用户项目里的 `.claude/agents/cto.md` 是**用户文件**,ccteam 不删不改 |
| **跨项目记忆走官方接口** | Claude `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`;Codex `~/.codex/AGENTS.md` —— ccteam 只**读**,不代项目生成 |
| **init 布局(v0.9.0:不种任何 role)** | 项目 `.ccteam/` 只 `state.json` + `workflow.yaml`;**`ccteam init` / IM `/newproject` / API create 不再种任何 role**,`.claude/agents/` 留空(默认会话 = roleless 裸 vendor 读项目 `CLAUDE.md`);ccteam 托管设置(hook + base)写 `.claude/settings.local.json`(**绝不碰用户 `.claude/settings.json`**);`~/.ccteam` 规范布局 = `ccteam_core::canonical_home_dirs()`(doctor home-drift 检查)|
| **新建项目 slug = 目录名 + 数字累加** | `slugify(目录名)`,撞名累加 `demo`/`demo2`/`demo3`(弃 `-{4hex}`);`ccteam init` 可在任意现有目录**就地**初始化;`--slug` 显式覆盖 |
| **root README.md 必须英文 + 不含版本进展/状态** | README 始终反映当前能力,不夹版本时间轴 / baseline / shipped 日期 |

> **已退役的旧红线**(新架构打破,勿再引用):**「session = role」keystone**(session 已升为独立一等实体 + 持久 sid,role 是属性;`(项目,role)` dedup、pane/turns 按 role 都已废 —— 见上方「session = 独立一等实体」红线)、「每次 spawn = fresh 1M context」(chat 复用 context 是 feature)、「fix-loop 撞 3 次 escalate / AgentPath depth」(属 deferred `ccteam-flow`)、「HITL approval state SoT / `mode: human-approval`」(**workflow.yaml 编排级**批准仍推后;**per-session** HITL 已落地,走 `PermissionRequest` hook,见上行;非 hitl session 仍 `--dangerously-skip-permissions`)、「`ccteam init` 落 AGENTS.md → CLAUDE.md symlink」(ccteam 不接管项目知识层)、flex / kind:flex / `.ccteam/sessions/`(EOL 删除)、模式 1/2/3 分栏 / orchestrator tick。

**vendor 红线**(V0.6 F107 / F112):
- ccteam **不 vendor** Claude / Codex 二进制(`references/{claude-code,codex/codex-rs}/` git-ignore 不入库,仅协议参考;实际 spawn 走 `$PATH` 内 binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override)。
- `vendor: AgentVendor::{Claude, Codex, …}` 是 trait 一等公民(可扩展),无 default —— 必须 explicit(session spawn;`workflow.yaml` 的 vendor 同理,但编排已推迟,见 §一)。

## 四、扩展机制速查

详 `docs/dev/tech-design.md` §6:

| 机制 | 用途 |
|---|---|
| **role 库**(`.claude/agents/<role>.md`)| ccteam 唯一管的**项目级**指令面;**v0.9.0 起 init 不种 role,默认 roleless**;role 用户自建 / 从 **ccteam-hub 插件市场**装(含 agency-agents 等开源 + `fable-advisor`/`team-brain` 示例配方)—— `ccteam role search/add/list` 读 hub `index.json`(`role add` sha256 校验后 verbatim 写 `.claude/agents/`)或 web 插件市场浏览器一键装,或手动丢入,`/role <role>` 原地换(daemon 内存态) |
| **插件市场(ccteam-hub)**(`firstintent/ccteam-hub`)| ccteam repo 之外的**内容**源:curated `index.json` + `agents/`/`skills/`/`workflows/`;幂等 ingestion verbatim vendor 开源插件(agency-agents 192,MIT pinned-sha);ccteam 经 HTTPS + `~/.ccteam/hub-cache/` 读 + sha256 校验 + 装进项目 `.claude/{agents,skills}/`;REST `GET /api/v1/marketplace(/{id}/body)` + `…/projects/{slug}/marketplace(/install)` |
| **CLAUDE.md / AGENTS.md** | 项目 / 用户级持久指令,**vendor 原生**(ccteam 只读,不生成)|
| **MCP** | `ccteam` **8 工具**,0 STUB(v0.9-T1 cull 15→8,删 advise 2 / admin_change_persona+add_tool / chat bot 3,`admin_ls`→`status`;`STUB_TOOLS` const + `ccteam doctor --verify-mcp` 自检,drift exit 1):`status`(v0.9.6 扩 vendor 面板:按 caller project 绑定 host 出 installed/version · auth 诚实枚举(unknown 不冒充、不阻断)· budget(含 unpriced)· advisory 目录三源(runtime last-seen/hub/用户,分标来源不合并、**永不当 spawn 白名单**)· routing notes(`<project>/.ccteam/routing.md` > `~/.ccteam/routing.md`,替换非合并;global 缺失时统一 home ensure 仅创建不覆盖)限幅原文透传;ambient caller 服务端锁自己 project)· `chat_send_file` · `screenshot` · **session 5**(**A2A 委派,任意 session 可调,非 cto-only**:`session_spawn`(vendor/model/effort/protocol/title;**无 host —— 继承 project 绑定,传入即错**)/`dispatch`(wait/notify(final|all|off)/idempotency_key;wait 等真 turn 边界,不被中途叙述提前结束)/`collect`(`max_chars` 默认 10k,字段级 head70/tail30 截断 + 账本指针 + `tokens_total` 无价表 vendor 也落账)/`list`(树;`project/activity/limit` 过滤,默认 30 行、空字段省略)/`stop`,daemon 校验 per-session principal `(sid,secret)` + project 维度 + 护栏(depth 2 / children 10 / delegated 50);wait 内联答案 10k、完成通知嵌入 4k 限幅防父会话膨胀;session_* 全带服务端超时(collect/list 15s · stop 30s · spawn/dispatch 60s+wait),daemon 忙时返回可读错误不悬挂);wire 名裸(`session_spawn` 等,客户端按 server key 加命名空间 → 模型见 `mcp__ccteam__session_spawn`,v0.9.1 修双前缀);ccteam-managed Claude stream-json / Codex / OpenCode / Grok / Kimi session 的自有 MCP **统一走 `POST /mcp` HTTP**(session bearer);普通主会话用全局 admin 注册;stdio `mcp-serve` 只留 Claude terminal/既有全局/显式 fallback(stdout 纯 JSON-RPC、tracing→stderr)|
| **Skills** | **0 个 ccteam repo 自带**(repo 根 `skills/` 已删;原 skill 功能落 MCP 工具 + hub role + `config` CLI)。可从 **ccteam-hub 插件市场**装 skill 到项目 `.claude/skills/`(同 role 路径);项目自有 skill 仍可直接放各项目 `.claude/skills/`(vendor 原生,ccteam 不管)|
| **Subagents** | agent 内 `Task(subagent_type=...)` ad-hoc 节流(work-role 可自带)|
| **Hooks** | `ccteam internal hook progress-append / load-context` 等(隐藏);写 `.claude/settings.local.json` 的 ccteam hook 段 |
| **Plugins** | **ccteam 是纯 CLI、不是 vendor 插件**:MCP server 由 `ccteam config` 注册 —— Claude `~/.claude.json` 保留既有 `internal mcp-serve` 主会话桥;Codex `~/.codex/config.toml` 写 `url + http_headers` 的 HTTP admin 条目(`install_codex_mcp`,0600),ccteam-managed thread 再用 session bearer 覆盖;**v0.9.3 对称注册 Grok(`~/.grok/config.toml`)+ OpenCode(`~/.config/opencode/opencode.json`),v0.9.5 加 Kimi(`~/.kimi-code/mcp.json` `mcpServers.ccteam`,headers 为 map 非 ACP 数组)—— 五 vendor 任何主会话可编排**(托管会话不被遮蔽:Grok 同名去重 ACP 注入胜、OpenCode `MCP.add` 按名覆盖,真机验证);per-project `.mcp.json` 仅由 web 按需写第三方 server(init 不写)。repo **不**带 `.claude-plugin`/`.codex-plugin`/`marketplace.json`/根 `.mcp.json`。提示词类插件(skill/role/workflow)住 ccteam-hub |

**CLI 分组**(W4 锁定):顶层扁平 `init / start / stop / status / config / doctor` + `project`(ls/show/new/rm/stop)+ `session`(ls/attach/pause/resume/register/unregister/persona/add-tool/bots/role)+ 隐藏 `internal`(mcp-serve/hook/peek/progress/send/spawn/probe-project/web/mux)。`config` = setup hub(交互菜单 + `config <key> <value>`/`get`/`show`:装 MCP + IM token + prefs)。删除 = `project rm <slug> [--purge --dry-run --force]`;`project stop <slug>` 停项目全部 session。6 个废弃顶层别名(hook/peek/progress/send/spawn/mcp-serve)已删。

## 五、PR / 实现纪律

1. **每个改动映射**(commit/PR 描述均可):`requirements.md` 某条痛点 + `tech-design.md` 某节;改协议**以代码为 SoT**(同步 tech-design 末尾「协议→代码」指针表)
2. **commit 用英语;agent prompt 用英语**(**产品化、简洁,非冗长**;hub vendored prompt 随上游);项目文档(CLAUDE.md / `docs/`)用中文
3. **Pre-v1.0 = 开发阶段,不留技术债**:无真实用户群,**允许大胆做更好的抽象**。**不做历史迁移** — 新旧状态数据不兼容时**不写迁移步骤/兼容分支**,直接「清旧数据(`~/.ccteam/` + 各项目 `.ccteam/`)→ 重 `ccteam init`」;deprecated 直接删,breaking rename 不留 alias。tier-1 文档**只描述当前架构**,EOL 内容去版本 dir
4. **不写 backwards-compat shim**
5. **优先编辑现有文件,不轻易新建**
6. **测试不过不算完成** — `cargo test --workspace` 退步 = block;clippy 不能新增 warning
7. **版本发布同步文档(ship gate)** — 每次 `vX.Y.Z` ship 必须同步:
   - **内部 SoT**:本文 §一(只更 version 行 + 一行 headline)+ `.loop/state.md`(焦点/基线回填)+ `.loop/history.md`(一行蒸馏)+ backlog 完成卡蒸馏移出(**队列只持现势**)+ `docs/dev/tech-design.md` + workspace `Cargo.toml` version bump
   - **用户面**:root `README.md`(英文,不含版本进展)+ `docs/usage.md` ── 把本版新能力融入**当前能力描述**,不写"V0.X.Y 新增"措辞
   - **版本归档**:`docs-local/versions/v0-X-Y/README.md` + handoff doc 落地(**留在 gitignored `docs-local/`,不入库不推送**)
8. **beta-gating(仅 UI 层,v0.8.20 起)** — 新/不稳定功能默认**只对 admin 展示**(SPA 按 `useMe().isAdmin` show/hide),普通用户只见生产稳定面;**非安全/权限边界** —— 真权限仍走 `deny_non_admin`/`can_see_project` 等既有 ACL(后端照常服务)。毕业为 stable 即移除该 UI 门。例:web 建-session 的 terminal/rmux 协议 + 角色选择 = admin-only,claude/codex stream-json = 全员。

### 角色与写权(治理骨架;执法 = 声明 + 复核,**不做脚本硬防护**)

| 角色 | 写权 |
|---|---|
| **owner**(人) | 下令 + 签核人工门(tag·部署·红线·降基线·对外契约,登记 `.loop/state.md`);不直接写仓 |
| **规划(控制)会话 = Fable 5** | **治理面唯一作者**:本文(AGENTS.md/CLAUDE.md)+ `docs/` + `.loop/`(卡片窄写回域除外)+ `.github/`;排卡、批 review、教训蒸馏。治理面写权**归属 Fable 5 规划会话**(owner 决策 2026-07-17) |
| **dev 会话**(任意 harness,可多个) | 代码面(crates / SPA / tests / README.md)+ backlog **窄写回**(只改所取卡状态行 + 追加验证/偏差段) |

执法两层:**声明**(本节 + backlog 文件头;dev 会话发现治理面需要改 = 停手偏差申报,不自己动手)→ **复核**(Fable 5 规划会话批 review 抓写权越界;`writeback.sh`(无参数)只做**队列结构校验**,dev 收口必跑,`--selftest` 证其有牙 —— 写权**不设脚本硬防护**,owner 决策)。冲突域约定:**卡面冲突域首段 = 路径前缀**(如 `crates/ccteam-harness`),前缀重叠即同域须串行。DoD 要求越出卡面授权 = **停手**,卡面偏差申报(附最窄解锁提议)等裁决;裁决只授权提议字面,不隐性扩 scope。

### 多 session 并行编辑同一仓库

主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/main` 起独立工作树,完事 `git worktree remove`。**并行的唯一合法形态 = 不同冲突域**(backlog 卡面字段判定,同域串行)+ 一 worktree 一写者。**主仓不变 dirty**。跨 session 见主仓 dirty:`git stash push -m "<owner> WIP"` 再切;**别盲目 `git checkout -- .`**。

### 版本开发流程(版本化迭代不变;`.loop/` 只是承载)

- **大改 doc-first,小/中改 owner 直驱**:架构级 = PRD + dev-plan 落 `docs-local/versions/v0-x-y/`(gitignored)待 owner review;拍板后**规划把 PRD 拆成卡进 `.loop/backlog.md`**(冲突域/规格/DoD/建议入口),wave = 一组卡。owner `/goal` 直驱的小/中改 = 独立卡可直接 build(owner 选)。落地走 worktree-per-wave + subagent 派工(**briefing 自包含**:规格/坐标/验收直接写进 brief)→ `workspace.package.version` bump(commit `vX.Y.Z:` 前缀)→ §五.7 ship gate 回填。
- **推送 = direct-on-main no-PR 是常态**(`gh` 不能开 firstintent PR,见 §六 → SSH push);PR 仅当远端支持时。**tag + 部署 HELD,等 owner 显式「部署」**(push 到 main 不等于发布)。
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
- **`gh auth token` 没 `workflow` scope** ── 改 `.github/workflows/*` 的 PR HTTPS 推会被 403 拒绝。改用 SSH 推 `git push -u git@github.com:firstintent/ccteam.git <branch>:<branch>`
- **`cargo fmt --all` 已是 required**(`.github/workflows/check.yml` CI gate 守)── commit 前一律跑,CI `cargo fmt --all -- --check` 不过 PR 不能 merge
- **本文件 ≤200 行** — 越长 cache 越贵,Claude 越忽略

历史版本升级 migration 详各 `docs-local/versions/v0-X-Y/README.md`,不在此重复。

## 七、Rust 代码格式化约定

`rustfmt.toml` pin stable rustfmt(`max_width = 100` / `tab_spaces = 4` / `use_field_init_shorthand`)。**Workspace drift 已一次清零**(`cargo fmt --all` chore PR + CI gate 守),drift-zero policy:

- **commit 前必跑** `cargo fmt --all`(或 `cargo fmt -p <crate>` 局部目标 crate);CI gate(`.github/workflows/check.yml::fmt`)`cargo fmt --all -- --check` 不过 PR 不能 merge
- **`rustfmt --edition 2021 <files>` 直调仍 OK** ── 单文件场景照样能用,与 `cargo fmt` 等价
- **0 maintenance overhead** ── 不再有"drift 维持现状"或"小改 drifted 文件不 fmt-sweep"的特例;**一律 fmt 干净**
