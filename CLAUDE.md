# CLAUDE.md — ccteam 实现导引

> 本文档面向**下一次接手 ccteam 实现的 Claude session**。每次起手必读。
> 历史里程碑 + 升级 migration 见 `docs/versions/v0-X-Y/README.md`,本文描述**当前状态 + 红线 + 纪律**。

---

## 〇、当前架构红线(v0.8.8)

本仓已落地 **「IM 通用模式 + session 独立一等实体」**(版本号 `0.8.8`;在 v0.8.7 模型上把 session 从「= role」升成**独立一等实体 + 持久 sid**、补 roleless / web config / web role 页 / status 重写 + 批实机 bug 修):架构 SoT 是 `docs/tech-design.md` 与本文(**协议细节一律以代码为准**,见 tech-design 末尾「协议 → 代码位置」指针表)。**下文若仍见 v8.3 / orchestrator / 多模式(模式 1/2/3)/ flex / session=role / AGENTS.md-替代 / 27-tool 残留,以本节为准 —— 那些已退役:**

- **核心模型 `chat ⇄ project ⇄ session`,role 是 session 的属性**:一个 chat = 你的终端(IM chat 或 web)→ 切 project → spawn/resume **session**。**session 是独立一等实体**,有持久 `sid`(`s<N>`,单调、扛 daemon 重启、不复用);**role 降为 session 的一个属性**(spawn 时绑 `--agent <role>` persona)。**同一 role 可并存多个 session**(去掉了 `(project,role)` dedup)。session 启动走 `claude [--agent <role>] --name|--resume <session-name>`(tmux send-keys 路径;hooks 全触发,含 `Stop`→`chat_turn_completed`)。role 库 = 项目级 `.claude/agents/<role>.md`;`ccteam init` 种默认 `cto` role(chat-first 管家:懂 ccteam、**推荐** work-role,本版只推荐,用户自己 `/role` 切)。
- **pane / turns / marker 全按 sid**:pane = `ccteam-chat-<slug>-<sid>`、turns = `.ccteam/chat/<sid>/turns.jsonl`、transcript cursor / active-session marker 同按 sid;`CCTEAM_CHAT_SID` 注入 pane env(daemon HTTP 路径加 `X-Ccteam-Sid` header)→ hook 与 in-pane forwarder 都拿得到 sid,写/读同键。gateway `spawn_event_pump` 的 ANSWER 分支按 sid `append_turn`(它是 live daemon 唯一 turns writer)。
- **roleless session 合法**:空 role → spawn **不加** `--agent`(裸 claude 自读项目 `CLAUDE.md`/`AGENTS.md` 当 brain);非注入(ccteam 本就不注入 system prompt,省略 `--agent` 不违反 No-prompt-injection)。
- **No prompt injection 由 `--agent` 兑现**:role 行为住 `.claude/agents/<role>.md`,vendor 原生 `--agent` 让它**自读**,ccteam **不**注入 system prompt —— 这条红线现在是**被满足**,不是被违反。
- **daemon = IM gateway + web + MCP Unix socket**(一个进程,**不 tick、无 orchestrator 循环**);会话 = resume-by-session-id(spawn-on-demand + 空闲释放 + 按 sid resume,**非**常驻吊着;chat 复用 context 是 feature)。`ccteam-flow` orchestrator **存在但 daemon 不跑**(deferred)。
- **harness × provider facet**:`harness` = agentic CLI adapter(本版 claude-code 跑通;codex best-effort;gemini-cli/grok-cli/其余 = future,**可扩展 `AgentVendor` enum**);`provider` = 子 facet(model,仅某 harness 多模型时有意义)。都是 session 属性、非顶层资源;`GET /capabilities` 按 PATH probe 动态列当前可用 harness(×provider)。
- **标准资源 API `/api/v1`**(web-token 鉴权):**project**(GET/POST `/projects`,GET/DELETE `/projects/{slug}` —— DELETE = 注销 + 停 session,**file-purge 留 CLI**)· **role**(GET `/projects/{slug}/roles`,GET/PUT `…/roles/{role}`)· **session**(GET/POST `/projects/{slug}/sessions`,GET `/sessions/{sid}`,POST `…/turn`,GET `…/events` SSE,POST `…/stop`)· **config/im**(web-token 门后:GET masked 状态 + PUT telegram/lark + telegram chat_id 异步轮询,**REST 路由非 MCP 工具**)· GET `/capabilities` + OpenAPI(`GET /api/docs` Scalar UI、`GET /api/v1/openapi.json`,单源 `OpenApiRouter`,同 web-token 门)。session-id = gateway `s{n}`;per-session SPA UI(`/chat/s/:sid`,历史从 `.ccteam/chat/<sid>/turns.jsonl` + 按 sid 过滤 SSE),外加 Settings(IM config)/ Roles(只读浏览)页。
- **progress 写入权威**:`harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export。

> 验证优先用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)+ 真实 WS/HTTP smoke;不退 baseline。起手/恢复先读本文 §一 + `docs/tech-design.md`(架构 SoT)。

---

## 一、当前状态

| 项 | 值 |
|---|---|
| Workspace version | `0.8.8` |
| 测试 baseline | `1998/0`(`cargo test --workspace --exclude ccteam-web`);`ccteam-web` = 224 pass / 4 env-gated `ws_*`(tmux pipe-pane PTY,留 CI/专机;`pane_snapshot` 已改 no-gateway→503 确定性)+ vitest 151/151(SPA)|
| Clippy | 0 errors + 0 warnings(`cargo clippy --workspace --all-targets -- -D warnings`,含 `ccteam-web`)|
| 当前在做 | **v0.8.8 已落地(session 独立一等实体 keystone + 能力补全 + 批实机 bug 修)**:**独立 session 模型**(session = 一等实体 + 持久 `sid`,role 降为属性;去 `(project,role)` dedup → 同 role 可并存多 session;pane/turns/marker 全按 sid;`CCTEAM_CHAT_SID` 注入;**gateway `spawn_event_pump` 补 turns writer** —— 改前 live daemon 根本不写 turns.jsonl)· roleless(空 role → spawn 不加 `--agent`,brain 走项目 `CLAUDE.md`)· status 重写(列所有 project + 各自 sessions:role/vendor/status/sid/last-event;删 recent events;两行 web token/url + LAN ip)· web config(`/api/v1/config/im`:telegram+lark,masked 不回显明文,chat_id 异步,restart_required)· web role 浏览页(只读 Roles tab)· 批实机 bug 修(project stop 走 ProcessBackend 真停 · web 新建项目入口走 REST · role 下拉拉真实 role · session ls 从 gateway map 取活性 codex 不再误报 · web 终端按 sid 解析 pane)· 清理(删 teams/skills/examples/qa-autoloop 等死目录)· **MCP 仍 17**(config/im 是 REST 非 MCP);PRD/handoff/归档见 `docs/versions/v0-8-8/` |

> 主分支 HEAD 以 `git rev-parse origin/dev` 为准(v0.8.8 在 dev 上);历史里程碑见 `docs/versions/v0-X-Y/README.md`(冻结归档)。

**ccteam 是 Claude Code(+ Codex)之上的元工具** —— 云端常驻的元 AI 团队,从 IM 和 web 驱动。架构 5 块:

- **配置**:每项目 `workflow.yaml` 声明 agent 拓扑(**无 prompt**,只 trigger + 并发上限 + `vendor`);role 行为 = 项目级 `.claude/agents/<role>.md`(`ccteam init` 种默认 `cto`)。**项目知识层(`CLAUDE.md`/`AGENTS.md`)归 vendor + 项目自己,ccteam 不生成/不桥接/不抑制**。
- **执行**:resident daemon = IM/web⇄session 路由网关(**不 tick、无 orchestrator 循环**)→ 按需 spawn / resume session(按持久 sid):Claude 走 `claude [--agent <role>]` tmux 长 session(send-keys + transcript + hook;空 role = roleless 裸 claude),Codex best-effort;两 vendor 归一成中立 `CanonicalEvent`。
- **状态 SoT**:`progress.jsonl` 业务事件(`harness/progress_bridge` 单一权威);chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<sid>/turns.jsonl`(按 sid;不依赖 Anthropic 内部 `~/.claude/projects/`)。
- **接口**:17 个 MCP 工具 `mcp__ccteam__{admin_,chat_,advise_,session_,screenshot}*`(代码 `STUB_TOOLS` + `ccteam doctor --verify-mcp` 自检)+ 标准资源 API `/api/v1`(含 `config/im` + OpenAPI `/api/docs`)+ IM 命令面(`/pair /cd /use /new /role @handle @ccteam`)+ web(含 per-session `/chat/s/:sid` + Settings + Roles 页)。
- **安装**:`curl install.sh | sh`(prebuilt binary,linux + macOS,Windows 走 WSL2)→ Claude `/plugin install ccteam` OR Codex `codex plugin marketplace add firstintent/ccteam`;`cargo install --git …` 是 fallback。

详 `docs/tech-design.md`。

## 二、必读文档(全局文档收敛到 3 份)

> **代码是唯一 SoT**。文档只留代码里没有的「为什么 / 架构论证 / 怎么用」;协议细节(CLI / JSON / event / 路由)一律以代码为准 —— 见 `tech-design.md` 末尾「协议 → 代码位置」指针表。

| 文档 | 角色 | 何时读 |
|---|---|---|
| `docs/tech-design.md` | 架构 SoT(gateway daemon + 独立 session/sid + role 属性 + harness×provider + 标准资源 API)+ 协议→代码指针表 | 改架构前 / 找协议在哪 |
| `docs/requirements.md` | 原始需求(核心痛点 = 验收基准) | 验收基准 / PR 痛点映射 |
| `docs/usage.md` | 用户命令手册(install→start→use→运维,纯命令) | 看怎么用 |

历史版本归档 `docs/versions/v0-X-Y/README.md`(冻结、按需);探索研究 `docs/research/`(不更新、按需)。这些都**不**自动进上下文。

**起手 30 秒**:`git log -1` 看 HEAD → `cargo test --workspace --exclude ccteam-web 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` 记基线 → 读用户诉求 → 干。

**对照参考**(`references/` gitignore 不入库):`references/claude-code/` + `references/codex/codex-rs/`。HarnessAdapter / 协议适配时翻;**不**当 ccteam 依赖。

## 三、不可触碰的架构红线

**本节是架构红线的唯一权威清单**(`docs/tech-design.md` §0 只放速查 + 就地论证,引用本节)。两条用户进入层(IM + web)都守。任何 PR 不得违反:

| 红线 | 怎么守 |
|---|---|
| **No prompt injection** | role 行为住 `.claude/agents/<role>.md`,vendor 原生 `--agent <role>` 让它自读,**不**向 pane / app-server 注入 system prompt;**roleless session(空 role)= spawn 省略 `--agent`**(裸 claude 自读项目 `CLAUDE.md`)= 同一红线的合法形态(不注入 ≠ 必须有 role);`/compact /new /clear` 完全透传 |
| **`progress.jsonl` 是 state SoT** | `harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export;chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<sid>/turns.jsonl`(**按 sid**;gateway `spawn_event_pump` 是 live daemon 唯一 turns writer)|
| **session = 独立一等实体;role 是属性** | session 有持久 `sid`(`s<N>`,单调、扛重启、不复用);**同一 role 可并存多 session**(去掉 `(project,role)` dedup);pane=`ccteam-chat-<slug>-<sid>`、turns/marker 全按 sid;`CCTEAM_CHAT_SID` 注入 pane env(daemon HTTP 路径加 `X-Ccteam-Sid` header),hook/forwarder 据此报 sid、写/读同键 |
| **不解析终端输出** | 读 transcript jsonl + 官方 hooks fast event;**不 scrape pane**(`tmux capture-pane` 仅 dev 调试 + screenshot tool 只读)|
| **永不主动 kill 长 session** | `budgets.{claude,codex}.max_cost_usd_per_24h` 触顶 auto-disable 是预算例外;`project stop` / `project rm --force` 是**用户显式命令**(非主动 kill,合法);`/compact /new` 是合法 turn;**去 dedup 后 `/new` 总铸新 sid、不再撞 `(project,role)` 复用而 close 旧 pane** |
| **HITL 批准边界 = `PermissionRequest` hook**(per-session,默认 `skip`) | hitl session 的批准门走 vendor 原生 `PermissionRequest` hook（**不**注入 system prompt/不 scrape）→ daemon `permission/ask`(转发 **ccteam sid**,非 Anthropic session UUID)→ IM `[同意][拒绝]`;hitl spawn 走 `--permission-mode default`(**绝不** skip,否则白嫖批准),skip session 仍 `--dangerously-skip-permissions`;deny **只挡该次工具、不 kill turn**(守「永不主动 kill」) |
| **cto 调度门 = daemon 校验 per-session secret(best-effort,非硬边界)** | 5 个 `session_*` 工具(spawn/dispatch/collect/list/stop)的特权由 daemon 校验:spawn 时 mint per-session secret 注入 pane env(`CCTEAM_CHAT_SECRET`),daemon 存 `sid→{role,secret}`,forwarder 转发,门校验 `(role,secret)` 对(**非**信明文 role)+ project 维度(只能操作自己 slug 的 sid);`session_*` 只用 gateway session map,**不**碰 deprecated registry/supervisor;`dispatch`/`stop` 是显式调度(非主动 kill 长 session)。**诚实范围**:单 OS-uid 全信任模型下 agent 之间**无硬边界**(同 uid 可读他进程 `/proc/<pid>/environ`/文件/ptrace → 拿到 secret),secret 只**抬高门槛**(defense-in-depth),**不 close**;真隔离 = per-agent OS user / sandbox(v0.8.8 deferred) |
| **会话 = resume-by-session-id** | spawn-on-demand + 按 **sid** resume(粒度从 `(项目,role)` 升到 session id)+ 空闲释放 + 扛 daemon 重启,**非**常驻吊着;chat 复用 context 是 feature |
| **ccteam 不生成/桥接项目 `CLAUDE.md`/`AGENTS.md`** | 项目知识层归 vendor 原生(Claude 读 `CLAUDE.md`、Codex 读 `AGENTS.md`)+ 项目自己;ccteam 唯一管的指令面 = `.claude/agents/<role>.md`(role 库) |
| **`ccteam-core` 零 team 名字面量** | core = primitives leaf,team 名不入 core |
| **跨项目记忆走官方接口** | Claude `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`;Codex `~/.codex/AGENTS.md` —— ccteam 只**读**,不代项目生成 |
| **init 布局** | 项目 `.ccteam/` 只 `state.json` + `workflow.yaml`;role 库 `.claude/agents/<role>.md`(种 cto);ccteam 托管设置(hook + base)写 `.claude/settings.local.json`(**绝不碰用户 `.claude/settings.json`**);`~/.ccteam` 规范布局 = `ccteam_core::canonical_home_dirs()`(doctor home-drift 检查)|
| **新建项目 slug = 目录名 + 数字累加** | `slugify(目录名)`,撞名累加 `demo`/`demo2`/`demo3`(弃 `-{4hex}`);`ccteam init` 可在任意现有目录**就地**初始化;`--slug` 显式覆盖 |
| **root README.md 必须英文 + 不含版本进展/状态** | README 始终反映当前能力,不夹版本时间轴 / baseline / shipped 日期 |

> **已退役的旧红线**(新架构打破,勿再引用):**「session = role」keystone**(session 已升为独立一等实体 + 持久 sid,role 是属性;`(项目,role)` dedup、pane/turns 按 role 都已废 —— 见上方「session = 独立一等实体」红线)、「每次 spawn = fresh 1M context」(chat 复用 context 是 feature)、「fix-loop 撞 3 次 escalate / AgentPath depth」(属 deferred `ccteam-flow`)、「HITL approval state SoT / `mode: human-approval`」(**workflow.yaml 编排级**批准仍推后;**per-session** HITL 已落地,走 `PermissionRequest` hook,见上行;非 hitl session 仍 `--dangerously-skip-permissions`)、「`ccteam init` 落 AGENTS.md → CLAUDE.md symlink」(ccteam 不接管项目知识层)、flex / kind:flex / `.ccteam/sessions/`(EOL 删除)、模式 1/2/3 分栏 / orchestrator tick。

**vendor 红线**(V0.6 F107 / F112):
- ccteam **不 vendor** Claude / Codex 二进制(`references/{claude-code,codex/codex-rs}/` git-ignore 不入库,仅协议参考;实际 spawn 走 `$PATH` 内 binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override)。
- `vendor: AgentVendor::{Claude, Codex, …}` 是 trait 一等公民(可扩展),无 default — workflow.yaml 必须 explicit。

**skill 自洽红线**:**本版 0 个 ccteam 自带 skill**(repo 根 `skills/` 目录已删,原 skill 功能落 MCP 工具 + cto role + work-role + `config` CLI)。规则保留供日后若引入 bundled skill 时遵守:`skills/*/SKILL.md` + body 随安装装到用户机器,**必须**自洽 ── 禁版本号(`V0.X.Y`)/ 禁 `docs/versions/v0-X-Y/` 引用 / 禁 Wave-N / 禁 F-tag / 禁 ship-status creep(shipped / "ship gate" 等)/ 禁 PR # / 禁 commit ref。允许:sibling skill 引用、MCP tool 名、CLI 命令、稳定 user-facing docs(`docs/usage.md`)。Ship gate:`grep -rnE "V\d+\.\d+|docs/versions|Wave [0-9]|F[0-9]+[a-z]?\b|ship gate|shipped" skills/*/SKILL.md` 必须 0 命中(无 `skills/` 目录 → 天然 0)。

## 四、扩展机制速查

详 `docs/tech-design.md` §6:

| 机制 | 用途 |
|---|---|
| **role 库**(`.claude/agents/<role>.md`)| ccteam 唯一管的指令面;`ccteam init` 种默认 `cto`(管家);work-role 用户自建 / 从 agency-agents(Claude 原生 .md,MIT)选 —— `ccteam role search/add/list`(catalog 192 entries,`role add` verbatim 写入 `.claude/agents/`)或手动丢入,`/role <role>` 原地换(daemon 内存态) |
| **CLAUDE.md / AGENTS.md** | 项目 / 用户级持久指令,**vendor 原生**(ccteam 只读,不生成)|
| **MCP** | `ccteam` **17 工具**,0 STUB(`STUB_TOOLS` const + `ccteam doctor --verify-mcp` 自检,drift exit 1):admin 3(`ls`/`change_persona`/`add_tool`)· chat 6(`register_bot`/`unregister_bot`/`list_bots`/`send_input`/`history`/`send_file`)· advise 2(`vote`/`parallel`)· **session 5**(cto 调度:`session_spawn`/`dispatch`/`collect`/`list`/`stop`,daemon 校验 per-session secret + project 维度)· screenshot 1;前缀 `mcp__ccteam__*`;wire 纪律:`mcp-serve` stdout 纯 JSON-RPC、tracing→stderr |
| **Skills** | **0 个 ccteam 自带**(repo 根 `skills/` 目录已删;原 skill 功能落 MCP 工具 + cto role + work-role + `config` CLI)。项目自有 skill 仍可放各项目 `.claude/skills/`(vendor 原生,ccteam 不管)|
| **Subagents** | agent 内 `Task(subagent_type=...)` ad-hoc 节流(work-role 可自带)|
| **Hooks** | `ccteam internal hook progress-append / load-context` 等(隐藏);写 `.claude/settings.local.json` 的 ccteam hook 段 |
| **Plugins** | **ccteam 自身分发**:repo 同时是 Claude + Codex plugin marketplace ── `.claude-plugin/{plugin,marketplace}.json` + `.codex-plugin/plugin.json` + 根 `.mcp.json`(`command: "ccteam"` 走 PATH binary,canonical `internal mcp-serve`);user 装路径 = install.sh(binary)+ `/plugin install ccteam` OR `codex plugin marketplace add firstintent/ccteam` |

**CLI 分组**(W4 锁定):顶层扁平 `init / start / stop / status / config / doctor` + `project`(ls/show/new/rm/stop)+ `session`(ls/attach/pause/resume/register/unregister/persona/add-tool/bots/role)+ 隐藏 `internal`(mcp-serve/hook/peek/progress/send/spawn/probe-project/web/mux)。`config` = setup hub(交互菜单 + `config <key> <value>`/`get`/`show`:装 MCP + IM token + prefs)。删除 = `project rm <slug> [--purge --dry-run --force]`;`project stop <slug>` 停项目全部 session。6 个废弃顶层别名(hook/peek/progress/send/spawn/mcp-serve)已删。

## 五、PR / 实现纪律

1. **每个 PR 描述映射**:`requirements.md` 某条痛点 + `tech-design.md` 某节;改协议**以代码为 SoT**(同步 tech-design 末尾「协议→代码」指针表)
2. **commit 用英语**;文档与 agent prompt 用中文
3. **Pre-v1.0 = 开发阶段,不留技术债**:无真实用户群,**允许大胆做更好的抽象**。**不做历史迁移** — 新旧状态数据不兼容时**不写迁移步骤/兼容分支**,直接「清旧数据(`~/.ccteam/` + 各项目 `.ccteam/`)→ 重 `ccteam init`」;deprecated 直接删,breaking rename 不留 alias。tier-1 文档**只描述当前架构**,EOL 内容去版本 dir
4. **不写 backwards-compat shim**
5. **优先编辑现有文件,不轻易新建**
6. **测试不过不算完成** — `cargo test --workspace` 退步 = block;clippy 不能新增 warning
7. **版本发布同步文档(ship gate)** — 每次 `vX.Y.Z` ship 必须同步:
   - **内部 SoT**:CLAUDE.md §一 baseline + `docs/tech-design.md` + workspace `Cargo.toml` version bump
   - **用户面**:root `README.md`(英文,不含版本进展)+ `docs/usage.md` ── 把本版新能力融入**当前能力描述**,不写"V0.X.Y 新增"措辞
   - **版本归档**:`docs/versions/v0-X-Y/README.md` + handoff doc 落地

### 多 session 并行编辑同一仓库

主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/dev` 起独立工作树,完事 `git worktree remove`。**主仓不变 dirty**。跨 session 见主仓 dirty:`git stash push -m "<owner> WIP"` 再切;**别盲目 `git checkout -- .`**。

### 版本开发流程

- **Patch / Minor 通用 doc-first**:PRD + dev-plan 落 `docs/versions/v0-x-y/`,用户 review 后才动代码 → worktree-per-wave/finding + subagent 派工(briefing 含 PRD section + 验收条目)→ per-wave PR review → `workspace.package.version` bump(commit 用 `vX.Y.Z:` 前缀)→ CLAUDE.md §一 baseline 回填。
- **wave 范式**:每 wave 一份 `wave-N-handoff.md`(Decided / Rejected / Risks / Files / Remaining 五段固定)+ 一个 PR;subagent briefing 必含 wave acceptance gate + 上 wave handoff link。**红线:每 wave baseline ≥ 上 wave**(test pass count + clippy 0 warnings),否则不发 PR。架构级大改可把 tier-1 文档**全量重写**放最后一 wave(docs 反映已落地代码)。

## 六、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱;本仓用 Claude Code 默认行为开发,只产出项目挂 ccteam hook
- **ccteam 的 hook 写 `.claude/settings.local.json`**(不是 `.claude/settings.json`)— local 层 gitignored、Claude 照读、与用户 settings.json 合并;ccteam 只 merge/清自己的 hook 段,**不脏用户 git**。(doctor 的 legacy-hook scrub 仍按文件名碰 settings.json,是把旧 ccteam hook 从用户文件**清出去**的一次性迁移,与此一致。)
- **`.claude/settings*.json` 的 `bypassPermissions` 是开发态便利** — 产品形态走 `--dangerously-skip-permissions`,语义不同
- **测试 `bootstrap_project` / `bootstrap_meta_project` 前必先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` 写垃圾,破坏 claude 登录
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),**不**放 lib `#[cfg(test)] mod tests`
- **改了 `ccteam-core` 公共 API**(如 slug / role-reader 签名)→ grep 全 caller(tests / mcp_serve.rs / commands.rs / ccteam-web routes)
- **`claude [--agent <role>] --name/--resume <session-name>` argv 可能漂移** — `--agent` 非空 role 才加(空=roleless 裸 claude);pane/name 按 sid(`chat_session_name(slug, sid)`);`CCTEAM_CLAUDE_BIN` env override 让测试不依赖真实 binary;生产改 `claude_tui.rs` 的 `spec_for_new`/`spec_for_resume` argv 即可
- **`--agent` 顶层 turn 偶发也触发 `SubagentStop`**(session 被建模为 implicit-main 的 subagent);`Stop` 始终触发,turn 完成可靠 —— **不会双发 IM 回复**(回复只走 transcript-content track,hook track 仅写 progress)
- **WSL2 / inotify-busy 宿主** `fs.inotify.max_user_instances` 易触顶,本机跑见大批 watcher/SSE/web e2e 502;`ccteam-web` 的 4 个 `ws_*` 走 tmux pipe-pane PTY(sandbox 不能流)→ **环境层**,non-WSL / 大 limit 机 OR CI 复测;不计入 baseline
- **`gh auth token` 没 `workflow` scope** ── 改 `.github/workflows/*` 的 PR HTTPS 推会被 403 拒绝。改用 SSH 推 `git push -u git@github.com:firstintent/ccteam.git <branch>:<branch>`
- **`cargo fmt --all` 已是 required**(`.github/workflows/check.yml` CI gate 守)── commit 前一律跑,CI `cargo fmt --all -- --check` 不过 PR 不能 merge
- **本文件 ≤200 行** — 越长 cache 越贵,Claude 越忽略

历史版本升级 migration 详各 `docs/versions/v0-X-Y/README.md`,不在此重复。

## 七、Rust 代码格式化约定

`rustfmt.toml` pin stable rustfmt(`max_width = 100` / `tab_spaces = 4` / `use_field_init_shorthand`)。**Workspace drift 已一次清零**(`cargo fmt --all` chore PR + CI gate 守),drift-zero policy:

- **commit 前必跑** `cargo fmt --all`(或 `cargo fmt -p <crate>` 局部目标 crate);CI gate(`.github/workflows/check.yml::fmt`)`cargo fmt --all -- --check` 不过 PR 不能 merge
- **`rustfmt --edition 2021 <files>` 直调仍 OK** ── 单文件场景照样能用,与 `cargo fmt` 等价
- **0 maintenance overhead** ── 不再有"drift 维持现状"或"小改 drifted 文件不 fmt-sweep"的特例;**一律 fmt 干净**
