# AGENTS.md — ccteam 实现导引

> 权威文件 = `AGENTS.md`;`CLAUDE.md` 是指向它的软链。面向下一次接手 ccteam 的 agent session(Claude Code 读 `CLAUDE.md`,Codex 读本文),起手必读。
> **代码是唯一事实源**:协议 / CLI / JSON / event / 路由 / 参数一律以代码与工具自描述为准;本文只留定向图 + 红线 + 纪律 + 实战坑,不复述代码能回答的事。
> **需求与发版由 `docs-local/`(gitignored 本机文档区)管理**(§一):版本 PRD 自上而下 + 本地 issue 自下而上;仓库内不再维护任何治理状态文件(`.loop/` 已于 2026-08-30 退役,勿复活)。
> 冷启动:本文 → `git log` + `Cargo.toml` 版本 → `docs-local/versions/<当前版本>/prd.md` + `docs-local/issues/`(如有)→ 代码按需读,不做全仓扫描。

## 〇、架构总览(定向图;红线唯一清单 = §三)

ccteam = 多 harness agent 团队的桥接与治理层:常驻 daemon(IM gateway + web + MCP)把 IM/web chat 路由到按需 spawn/resume 的 agent session;任意 session 经 8 个 MCP 工具委派任意其他 session(A2A)。**铁律:只做单 harness 做不到的(跨 vendor 身份/路由/账本/观测 + 跨机执行),永不做厂商能力。**

- **核心模型 `chat ⇄ project ⇄ session`**:session 是一等实体(持久 sid `s<N>`,单调、扛重启、不复用);role / harness(claude|codex|grok|opencode|kimi|pi|dsh,`AgentVendor` 可扩展)/ provider(model)/ protocol 都是 session 属性,同一 role 可并存多 session;host 是 project 属性,session 继承;roleless(空 role,裸 vendor 自读项目 `CLAUDE.md`)合法且是默认。
- **协议轴**:Claude 默认 `stream-json`(长驻子进程 + 双向 NDJSON,无 PTY/hook);grok/opencode/kimi/dsh 走 ACP;pi 走 `--mode rpc`(`LocalOnly`);`terminal`(tmux/rmux)维护期、规划淘汰(§三)。所有 adapter 归一 `CanonicalEvent`,gateway `spawn_event_pump` 单点消费。
- **数据面**:业务事件 = `progress.jsonl`(schema 权威 `harness/progress_bridge`,`core` 只 re-export);对话原文 = `<project>/.ccteam/chat/<sid>/turns.jsonl`(按 sid;live daemon 唯一 writer = gateway);成本/委派树全入账本。
- **接口面**:8 个 MCP 工具 `mcp__ccteam__{status(+裸名发现别名 grok_claude_codex_kimi),chat_send_file,session_*×5}` + `POST /mcp` streamable HTTP + REST `/api/v1`(OpenAPI = `/api/docs`)+ IM 斜杠命令 + chat-shell web(per-session Chat|终端);清单/参数/语义以代码与工具自描述为准。
- **内容面**:引擎零内置 persona/skill/提示词;role = 项目 `.claude/agents/<role>.md`(vendor 原生 `--agent` 自读);skill = 全局库 `~/.ccteam/skills`(会话显式 attach)+ 项目 `.agents/skills`(`.claude/skills` 软链);一切内容从 ccteam-hub 装(sha256 校验、never-execute)或用户自建。
- **执行面**:daemon 不 tick、无 orchestrator 循环,只响应消息/排程;会话 = resume-by-sid + 空闲释放 + 容量挤停;编排智能 100% 用户空间(`ccteam-flow`/workflow.yaml 占位 deferred)。
- **安装面 = 一个二进制引擎、两个安装/托管面**:CLI(`curl install.sh | sh` → `ccteam config` 注册五 vendor 全局 MCP;pi 例外走受管会话 bridge)与 DSH 插件 `@ccteam/ccteam-ui`(npm,自带引擎平台包 `@ccteam/engine-<os>-<cpu>`,`apply()` 自动装/自启/attach);二者共用同一 `$CCTEAM_HOME` 与**同一个 daemon**(谁先起谁赢、其余 attach;插件释放不停 daemon;`/health` 身份字段 + `run/daemon-endpoint.json` pid 门)。

> 已退役概念(勿从旧文档 / git 史复活):orchestrator tick、模式 1/2/3、flex、session=role(`(project,role)` dedup)、fresh-1M-context、cto 内置工作流、agent-team init、前台 `ccteam start`、stdio `internal mcp-serve`、`.loop/` 治理与「规划会话写权」。验证优先用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`),不退 baseline。

## 一、需求与版本管理(两条来源,都住 gitignored `docs-local/`)

| 项 | 家 |
|---|---|
| 当前版本 | `Cargo.toml` `workspace.package.version` + git tag(`git describe --tags origin/main`);插件 `dsh-plugins/ccteam-ui/package.json`(含 `ccteam.engine`)与引擎同版本锁步 |
| **版本需求(自上而下)** | `docs-local/versions/v0-X-Y/prd.md`(owner 拍板前 = DRAFT,拍板后即开发授权;设计稿 / 交接 / 探针记录同目录自由命名)|
| **本地 issue(自下而上)** | `docs-local/issues/<seq>-<YYYYMMDD>-<slug>.md`,模板 = `docs-local/issues/TEMPLATE.md`(seq 单调不复用;截图拷进 `assets/<同名目录>/`)。来源 = IM/web 反馈(原话逐字 + chat_id/message_id)、owner 口述、会话自报(sid)、真机探针、CI。流程:立 issue(`open`)→ 分诊(病根按层 / 规格 / DoD / 冲突域 → `triaged`)→ 小改直接修(`in-progress`)、大改挑进某版本 `prd.md`(`scheduled(v0-X-Y)`)→ 完成回写 `done(<sha>)` + 验证段;done 原文保留 |
| 待排需求池 | `docs-local/versions/backlog.md`(跨版本候选卡;owner 点名版本时从中挑进 `prd.md`)|
| 发版归档 | ship 后 `docs-local/versions/v0-X-Y/README.md`(Decided / Rejected / Risks / Files / Remaining 五段,只引 issue 编号)+ `docs-local/versions/history.md` 追加一行 |
| 探索研究 / 对照源码 | `docs-local/research/`;`references/{claude-code,codex/codex-rs,opencode,kimi-code,OpenHands,rmux,deepseek-harness}`(仅协议参考,**不**当依赖)|

`docs-local/` 与 `references/` 均 gitignored、不入库不推送(仓库只留代码 + tier-1 文档);它们**不**自动进上下文,`git worktree` 也看不到 —— 派工 briefing 必须把规格转述进 prompt 或显式 `cp -r`。

## 二、仓库内文档(只有用户面;架构与协议一律读代码)

| 文档 | 角色 | 何时读 |
|---|---|---|
| `README.md` | 英文用户面,始终反映当前能力(不含版本进展 / 时间轴 / 基线) | ship gate |
| `docs/usage.md`(+`-cn`) | 用户命令手册(install→start→use→运维) | 看怎么用 |
| `docs/orchestration.md`(+`-cn`) | A2A 编排指南(session_* 工具面 + 身份模型 + 多机语义) | 写/改 A2A 面 |
| `docs/dsh-plugin.md`(+`-cn`) | DSH 插件 `@ccteam/ccteam-ui` 安装与引擎托管 | 改插件面 |

**同一事实只有一个家**:内容住错家 = 搬家优先于续写;仓库不再保留架构设计 / 需求文档,「为什么」写进代码注释与版本 PRD(`docs-local/`),协议细节永远只在代码里。

## 三、不可触碰的架构红线(唯一权威清单)

两条用户进入层(IM + web)都守;任何 PR 不得违反。

| 红线 | 怎么守 |
|---|---|
| **No prompt injection** | ccteam 不向 pane / app-server 注入 system prompt(禁 `--append-system-prompt` / `initialize.systemPrompt`);agent 行为一律由 vendor 自读自己的文件得来,ccteam 只决定「消息路由到哪个 sid」;`/compact /new /clear` 完全透传 |
| **引擎零 LLM** | 引擎自身不调用任何 LLM(无中介模型 / judge / 智能摘要 / 智能路由);LLM 推理只发生在 agent session 内部,任何前端/网关不得引入新 LLM 层 |
| **daemon 无自主内容决策循环** | 只响应消息、排程与连接;引擎不产生任务、不选择工作;编排智能永在会话/用户空间 |
| **`progress.jsonl` 是 state SoT** | schema 权威 = `harness/progress_bridge`,`core` 只 re-export;对话原文 = `<project>/.ccteam/chat/<sid>/turns.jsonl`(按 sid;gateway `spawn_event_pump` 是 live daemon 唯一 turns writer) |
| **session = 独立一等实体** | 持久 `sid`(`s<N>`,单调、扛重启、不复用)是唯一身份,任何属性都不参与去重;turns/marker/pane 全按 sid;生命周期 = spawn-on-demand + resume-by-sid + 空闲释放/容量挤停,非常驻吊着;chat 复用 context 是 feature |
| **ACL = 一个身份解析器 + 一套归属策略,两个前端共用(fail-closed)** | ①身份:`Gateway::principal` 唯一解析器,三态 Operator(admin web token / 全局 bot 允许列表点名的 chat)/ Tenant(`<platform>@<tenant>` bot 或 per-user web token → `user:<tenant>`)/ Guest(进门未点名:只拥有自己建的,看不到任何 project);「够得着 bot」≠「是 operator」,`"*"` 通配点名零人(`bind_operator_allowlist`;空表 = 未配置,单人默认 + 启动告警)。②归属:project 是归属单元(`ProjectState.owner`),session 继承;唯一策略 = `ccteam_core::identity::{can_see_owner, can_see_session_owner}`,IM/web/MCP 全走它;session 可见 = 自己建的 ⊕ 自己身份的 web 控制台池;IM chat 互相隔离,web 不看 IM session;「同-current-project 互看」已反转删除,勿加回。③解析永不越界:`current_project_for` 只在本人可见集里落地(`/cd` 选的 → daemon 默认(可见时)→ 本人首个),无可见项目 = 拒绝并给下一步,绝不回落 daemon 默认项目。④门:REST 单一 choke point = `auth::project_acl_layer`(认全部项目寻址族 `/api/v1/projects/{slug}/*` · `/api/{slug}/*` · `/ws/{slug}/*`,新路由自动覆盖);`/sessions/{sid}/*` 按其 project 门(admin 同样过门);集合面与 SSE 按身份过滤;`/ws/chat` 身份取自认证态不认 query;admin 专属只剩 `/users*` + 全局 `/config/im`(tenant 自助面 = `/me/im`),deny→403;SPA 仅管理员 tab 按 `GET /api/v1/me` 显隐。诚实范围:同 OS uid 下是软隔离(UX)非安全边界;web↔IM 同一人复联 deferred |
| **不解析终端输出** | 读 transcript jsonl + 官方 hooks fast event;不 scrape pane(`tmux capture-pane` 仅 dev 调试;web 终端快照走 pane-snapshot 只读)|
| **terminal 协议(tmux/rmux)冻结 = 维护-only** | 新 vendor / 新功能不得新增 tmux/rmux/PTY 依赖(新 harness 一律长驻 stdio:stream-json / ACP / app-server);既有 Claude `terminal` 只修不扩;逐字节终端镜像不再是新功能验收条件 |
| **永不主动 kill 长 session(kill 的是进程,不是会话)** | 会话 = sid + 账本 + 焦点路由,永不被 daemon 自行终结;daemon 只管常驻(residency):`resident` / `released`(下条消息按 sid 冷 resume)/ `stopped`(用户显式,`meta.stopped_at`)/ `detached`(体外活)。空闲释放 = 主机制(`sessions.idle_release_secs` 默认 3600 = claude prompt-cache TTL;`idle_release_by_vendor.<harness>` 覆盖;0 = 永不;资格 = 无在飞 turn ∧ 无 HITL 等待 ∧ 无 outlives-turn 后台任务);容量挤停 = 次级硬上限(`sessions.max_live`,LRU;等待用户决定的会话与新会话父链绝不挤,创建永不因容量失败);两者同一条释放路(`session_evicted{reason:idle\|capacity}` + lifecycle 广播),都不删焦点路由;daemon 重启不重生进程(只探 body)。预算例外:`budgets.*.max_cost_usd_per_24h` 触顶 auto-disable。`project stop` / `/stop` / `rm --force` 是用户显式命令;`/compact /new` 是合法 turn,`/new` 总铸新 sid |
| **session 调度门 = daemon 校验 per-session principal `(sid,secret)`(best-effort,非硬边界)** | 5 个 `session_*` 由 daemon 按 principal 校验:spawn 时 mint per-session secret,受管会话走 HTTP bearer `ccteam-sid:<sid>:<secret>`;`verify_session_principal` 常数时比对;caller 的 project 服务端覆写(只能操作自己 project),授权只认 principal;`dispatch`/`stop` 是显式调度,stop 限后代 + depth/children/delegated/cycle/预算护栏。手起会话 = enrolled client:凭 vendor 全局配置里的 enrollment 凭据(`ccteam-enroll:<id>:<secret>`)在 `initialize` 领 `Mcp-Session-Id`,daemon 铸 ledger 节点 + principal 并 promote(同一条 Ambient 路);节点 `managed_by: external` 不进 gateway live map,`dispatch/stop` 对它一律拒绝;凭据没钉 project 时 caller 必须自己点名 workspace(首次点名即终身绑定),cwd / peer / 最近项目等推断一律不做。ACP 面无 `--strict-mcp-config` 同类物 → `/mcp` 按进程血缘(adapter 握手前登记子 pid + `/proc` 上溯)绑回 principal,两条路都没到才发 `identity_degraded`(只广播)。诚实范围:单 OS-uid 全信任模型下 agent 间无硬边界,secret 只抬门槛;真隔离 = per-agent OS user/sandbox(deferred) |
| **委派语义 = 路由,非新引擎;完成通知非注入** | `session_dispatch` 与 IM `@handle` 同路;完成通知 = gateway 生成的一条普通 user-role turn 投给 parent(live = vendor steer / dead = pending FIFO + resume),与人转告同构;通知单位 = 任务(vendor turn 边界),中途叙述只进账本;child turns 按 child sid 落 `turns.jsonl`,委派关系写 `progress.jsonl`,不伪造进对方 transcript。可靠性合同:幂等 spawn/dispatch(`idempotency_key`)· at-least-once 通知扛重启(`delegation.json` 落盘 + 启动 reconcile)· append→notify 顺序 · 原子写;单 daemon 无 HA,是协议语义可靠 |
| **跨机 = host 是 project 属性,session 继承;网络方向 = 卫星拨入** | host 住 project catalog(`~/.ccteam/config.yaml`,条目含 `host` + `remote_slug`;slug 相同 ≠ 同一项目);spawn 面无 host 参数(传入即硬错 `HOST_SPAWN_PARAM_REMOVED`);记账一律 catalog slug;远程项目进入 = web 选主机新建或 `POST /projects/import` 接入卫星已上报项目(绝不自动接入;撞名累加、幂等);卫星零监听面:`ccteam start` 进程内嵌卫星客户端出站长连 daemon,卫星自己解析 binary + cwd,主侧永不下发路径;terminal 永不上多机;rebuild 一律 re-gate project 绑定(host 不符或卫星 offline → 可读错误,绝不本地重生);远程 verdict 钉 claude,其余 vendor 远程 = 显式 NotImplemented |
| **不改写已有项目 `CLAUDE.md`/`AGENTS.md`** | 项目知识层归 vendor 原生 + 项目自己;唯一放宽:对真空项目(两文件都不存在)scaffold `AGENTS.md` + `CLAUDE.md` = `@AGENTS.md`,绝不覆盖已有内容;`.ccteam/` 幂等加进项目 `.gitignore` |
| **vendor 配置足迹 = 只写自家 MCP 注册** | 对 vendor 全局配置(`~/.claude.json`、`~/.codex/config.toml`、grok/opencode/kimi 对称面)的唯一写入 = 幂等注册/修复自己的 MCP server 条目;项目侧只写 `.claude/settings.local.json` 自己的段(绝不碰用户 `.claude/settings.json`);pi 不写任何配置 |
| **`ccteam-core` 零 team 名字面量** | core = primitives leaf,team 名不入 core |
| **repo 零提示词类型内容(零例外)** | agent/persona/skill/workflow 的内容一律不进 ccteam repo;住 ccteam-hub 或用户空间,ccteam 只读 index、按 sha 取内容;用户项目里既有 `.claude/agents/*.md` 是用户文件,不删不改 |
| **跨项目记忆走官方接口** | Claude `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`;Codex `~/.codex/AGENTS.md` —— ccteam 只读 |
| **init 布局** | 项目 `.ccteam/` 只种 `state.json` + `workflow.yaml`(`routing.md` 用户可选自建,`status` 原文透传);`~/.ccteam` 规范布局 = `ccteam_core::canonical_home_dirs()`(doctor 查 home-drift) |
| **新建项目 slug = `slugify(目录名)` + 撞名数字累加** | `ccteam init` 可在任意现有目录就地初始化;`--slug` 显式覆盖 |

**vendor 红线**:不 vendor Claude / Codex 二进制(`references/` 仅协议参考;spawn 走 `$PATH` + `CCTEAM_{CLAUDE,CODEX}_BIN` override);`AgentVendor` 无 default,spawn 必须显式。

## 四、PR / 实现纪律

> **总纲:治病根,通用解优先于补丁。** 先定位缺陷所在的层(身份解析 / 归属策略 / 资源解析 / 门 / 前端状态机 / adapter 契约),在那一层修一次。判据:① **同形扫一遍** —— 一个 fallback 漏了,同形的通常还有几处(实锤:ACL choke point 只认一种 URL 形状 · 身份 fail-open 成 admin · 项目解析回落 daemon 默认,三处同病一个修法);② **新入口自动被覆盖** —— 新路由 / 前端 / vendor / 租户还要再补一次 = 补在了症状点。测试同理:「登记为 flake」常常是病根没找到,定性前先问「宿主给了它什么」(env / 端口 / 真实 home)。

1. **每个改动映射到需求**:commit / PR 描述点名对应的 issue(`docs-local/issues/#<seq>`)或版本 PRD 条目;「为什么」写进代码注释,不另立设计文档。
2. **commit 英文;agent prompt 英文**(产品化、简洁);项目文档(本文 / `docs/`)中文。
3. **Pre-v1.0 不留债**:允许大胆抽象;不写历史迁移 / 兼容分支 / backwards-compat shim(新旧状态不兼容 = 清 `~/.ccteam` + 项目 `.ccteam/` 重 `ccteam init`);deprecated 直接删,breaking rename 不留 alias;tier-1 文档只描述当前架构。
4. **优先编辑现有文件,不轻易新建。**
5. **门禁唯一家 = 根 `Makefile`**(「完成」= 命令退出码,不是文字声称):
   | 改动面 | 必跑 |
   |---|---|
   | 任何收口 | `cargo fmt --all -- --check`(CI required) |
   | Rust(非 ccteam-web) | + `make check`(clippy `-D warnings`)+ `make test`;记基线数字用 `make test-baseline`(`--lib --bins`,排 `tests/*.rs` env-flake;`--bins` 必须,否则 binary-only crate 零覆盖) |
   | `crates/ccteam-web/src` | + `make test-web`(WS/PTY 需真终端) |
   | SPA / DSH 插件 | `make web-check`;`cd dsh-plugins/ccteam-ui && npm test`(typecheck + persona 扫描 + build + vitest) |
   | 性能敏感面(progress 读写 / journal / projection / gateway 锁 / status·sessions 热端点) | + `make perf-gate`(release + `CCTEAM_PERF_GATE=1`) |
   | docs-only | fmt 即可 |
   一键 = `make gate`。**测试不过不算完成**;`cargo test --workspace` 退步 = block;clippy 0 warning;基线只增不减。CI `check.yml`(fmt / clippy / test = `test-baseline` 同口径 `--locked`)只在 **main push + PR** 触发 → 推 dev 本身零 CI,周期开始即开 dev→main draft PR;**CI 绿 = 干净环境仲裁**,本机数字在并行 cargo 下不可信(§五)。新校验先证有牙(先红后绿);判 flake 前在干净环境复测,禁「瞬时红顺手改测试消红」。
6. **版本与发布(owner 点名版本号才 bump;默认 commit 无版本前缀)**:ship gate = workspace `Cargo.toml` version + `dsh-plugins/ccteam-ui/package.json`(含 `ccteam.engine` / `dsh-plugins/engine-packages` 同版本)+ 内嵌插件 tarball 重打 → `README.md` + `docs/usage*.md` 把新能力融入当前能力描述(不写「vX 新增」)→ `docs-local/versions/v0-X-Y/README.md` 归档 + `history.md` 一行 + 相关 issue 回写 `done` → PR ready → CI 绿 → owner merge(merge commit)→ tag → `release.yml`(四平台二进制 + npm `@ccteam/engine-*` + 插件)。**tag / 发布 / 红线修订 / 对外契约 = owner 显式令**;发布后实测产物(下载 → sha → `--version` 核 build sha),CI 绿不是交付证据。
7. **分支**:一律开发在 `dev`,`dev→main` PR 攒版本(`gh pr create` 可用),merge 方式 = merge commit(非 squash;main 含 dev 完整历史,合并后 dev 直接续用);main 不直推,owner 显式令除外。
8. **beta-gating 几乎不用**:功能面默认对全体登录用户开放,能否碰由后端「身份 × 归属」判;SPA 唯一 admin-only = 设置→管理员(`visibleSettingsItems` 不变量测试锁死);临时 `useMe().isAdmin` 隐藏非权限边界,毕业即删。
9. **日耗上限 15 USD / 自然日,自主连跑不问**:预算内不为「要不要继续」请示;逼近上限减规模(缩 wave / 少派 subagent / 降模型档)不停工;上限不换质量,门禁照旧。
10. **多 session 并行**:主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/dev`(基线 = `origin/dev`),完事 `git worktree remove`(只删自己建的,先看分支与会话状态);并行的唯一合法形态 = 不同路径前缀(同前缀串行)+ 一 worktree 一写者;主仓不变 dirty(见 dirty 先 `git stash push -m "<owner> WIP"`,别盲目 `checkout -- .`)。派工 briefing 自包含(规格 / 坐标 / 验收直接写进 brief;worktree 看不到 `docs-local/`);maker 交付后由独立 checker 复核再收口。

## 五、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** —— 循环引用排错地狱;本仓用 Claude Code 默认行为开发,只产出项目挂 ccteam hook。
- **ccteam 的 hook 写 `.claude/settings.local.json`**(gitignored、Claude 照读、与用户 `settings.json` 合并);`.claude/settings*.json` 的 `bypassPermissions` 是开发态便利,产品形态走 `--dangerously-skip-permissions`。
- **测试 `bootstrap_project` / `bootstrap_meta_project` 前必先 `disable_tool_surface_bootstrap_for_tests()`**,否则向真实 `~/.claude.json` 写垃圾,破坏 claude 登录。
- **env-mutating 测试**(`set_var/remove_var HOME/CCTEAM_HOME/CLAUDE_CONFIG_HOME` …)只放 `crates/*/tests/*.rs` integration(独立进程),不放 lib `#[cfg(test)]`(全 workspace 有禁 env 写守卫);新状态面优先 `_in(root)` 注入式 API 而非 home 派生全局函数。
- **测试绝不写真实生产状态**:只把 `HOME` 指到 tempdir 不够 —— `CCTEAM_HOME` 优先级更高(实锤:fixture bot 注册写进真实 registry → telegram 静默失联数小时);隔离必须同时 pin `HOME` + `CCTEAM_HOME`。
- **会写文件的复现 / 沙箱脚本必须写成脚本文件**,顶部 `export HOME=<sbx>/home CCTEAM_HOME=<sbx>/home/.ccteam CCTEAM_INSTALL_DIR=<sbx>/bin PATH=<sbx>/bin:/usr/bin:/bin`(不含 `~/.local/bin`)并断言 `$HOME` ≠ 真实 home;逐条命令内联 env 不算隔离(两起事故:owner `~/.dsh` profile 被写坏、真实 `~/.local/bin/ccteam` 被 debug 构建覆盖)。
- **共享 `CARGO_TARGET_DIR` 的并行 worktree 会静默给错测试数**(缺字段编译错 / 新测试「不存在」/ 同分钟 104 vs 105):裁决数字只认 CI 或「无其他 cargo 在跑 + `cargo clean -p <crate>`」的本地跑;计数不动先当污染;`-p` 构建报缺符号但源里有 = stale 缓存,同修。
- **控制会话的 MCP 跑在 `target/debug/ccteam` 上时,勿在主仓跑 cargo**(重建即掉线);重活进 worktree。
- **本地 toolchain 必须跟上 CI 的最新 stable**(`rust-toolchain.toml` = `stable`,CI 每次装当天最新):落后则新 lint 本地全绿、CI clippy 连红;收口前核 `rustc --version` = CI 日志版本,落后就 `rustup update stable`;修 lint 按仓库约定(小 Err、调用点再包),不用 `#[allow]` 盖。
- **构建成功 ≠ 已部署**:声称「已修复」前对齐磁盘 `ccteam --version` 与运行中 daemon 的 build(`/proc/<pid>/exe`;`ccteam daemon status` 显示 running-vs-binary);本机部署 = 实体拷贝(`make install`),禁软链构建产物。
- **env-flake 族**(live-daemon 宿主 / 全并行下偶发,干净环境应全绿,非生产 bug):`gateway::tests` 共享 `/tmp/alpha` 与 `FakeAdapter` 竞态、`inbound_wiring daemon_*`、`daemon_test register_*`、`im_progress_*`、`codex_streaming_delta`、ccteam-web `ws_*`(需真 PTY;WSL2 inotify 触顶亦 502)、hot-config 同秒改写 mtime 缓存;macOS 另有 tempdir `canonicalize`(`/var`→`/private/var`)与 UDS `SUN_LEN` 两族。`resume_*` / `hook_*` 曾被误记 flake,实为读错路径 + 泄漏 fault 开关 / 继承宿主 `CCTEAM_HOOKLESS` —— 先证据后定性。
- **改了 `ccteam-core` 公共 API** → grep 全 caller(tests / mcp / commands.rs / ccteam-web routes)。
- **(terminal 协议)`claude [--agent <role>] --name/--resume` argv 可能漂移**:`--agent` 非空 role 才加;pane/name 按 sid;生产改 `claude_tui.rs` 的 `spec_for_new`/`spec_for_resume`(stream-json 默认路在 `claude_stream_json/spawn_spec.rs`);`--agent` 顶层 turn 偶发也触发 `SubagentStop`,`Stop` 始终触发,不会双发 IM 回复。
- **SPA Sidebar 每工作区有 WS_SHOW 行数上限**:扩 vendor/session 测试行须跨 project 摆放,否则折叠断言假红。
- **改 `.github/workflows/*` 需 token 带 `workflow` scope**(缺则 HTTPS 推 403;fallback = SSH 推)。
- **`cargo fmt --all` commit 前必跑**(`rustfmt.toml` pin stable;CI `check.yml::fmt` 不过 PR 不能 merge)。
- **本文件 ≤150 行** —— 越长 cache 越贵,越被忽略。
