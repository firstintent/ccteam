# ccteam 技术实现方案

> 本文档基于 [requirements.md](./requirements.md)(已确认的用户痛点)给出 ccteam 当前的技术架构、组件分解、数据协议、扩展点映射。
>
> **产品定位**:「云 Claude Code(+ Codex)+ IM + Web」—— 把一个常驻 gateway daemon 架在你机器上的真实 agentic CLI 之上,让你在 IM(Telegram 等)或 web 控制台里像用一台终端一样,跨多个项目、多个**独立 session**(各有持久 sid,role 是 session 的属性)操作真实 agent。

---

## 0. 架构红线

**权威红线清单 = [CLAUDE.md §三](../../CLAUDE.md)**(always-loaded 的开发宪法:红线表 + vendor / README / skill 红线;任何 PR 不得违反)。本文档**不重复**该清单 —— 只在下面各组件章节就地给红线的**架构论证**(为什么这么定),并用下表的 R-code 简写引用 §三 的条目。

**R-code 速查**(简写 ↔ CLAUDE.md §三):

- `R1` 文件系统是状态面(非 chat 命令面)· `R2` `progress.jsonl` 是 state SoT(+ `turns.jsonl` 对话原文,**按 sid**)· `R3` No prompt injection(`--agent` 让 vendor **自读** role.md = 这条的**兑现**,不是违反;roleless = 省略 `--agent` 的合法形态)· `R4` 会话 = resume-by-session-id(粒度 = 持久 sid;§三已并入「session = 独立一等实体」行)· `R5` 永不**主动** kill 长 session(例外:`project stop` / `rm --force` 是用户**显式**命令)· `R6` 不解析终端输出(不 scrape pane)· `R7` `ccteam-core` 零 team 名字面量 · `R8` 跨项目记忆走 vendor 原生接口 · `R9` crate 拓扑 `core → harness → cost` · `R10` 新建项目 slug = `slugify(目录名)` + 撞名数字累加,`ccteam init` 可就地初始化 · `R11` root README.md 英文且不含版本进展(家已迁 = CLAUDE.md §五.7 ship gate,不再是 §三行)

> 已退役概念的一行清单在 CLAUDE.md §〇 尾注(本文档不再重复);R-code 与 §三行名的全量对齐校对 = TD-SYNC-1 卡。

---

## 1. 设计原则

每条原则都直接挂钩 `requirements.md` 中的某条痛点。

| 原则 | 对应痛点 | 落地约束 |
|---|---|---|
| **守护进程化** | 痛点 9:AI 团队需要人来主持 | gateway daemon 独立于任何 Claude Code 主对话;**pid-detach 自管**(`ccteam daemon start`,setsid,v0.9.7 起废 systemd/launchd,借鉴 codex `app-server-daemon`);SIGTERM / `ccteam daemon stop` → daemon 优雅 drain(web / IM gateway / MCP socket,每任务 ≤ 5s)。前台 `ccteam start` 仍在(dev/容器)。**不 tick、无 orchestrator 循环**。 |
| **文件即状态机** | 痛点 7:进度永远不透明 | 一切状态可从文件系统恢复:`progress.jsonl` 业务事件 SoT + chat 原文 `turns.jsonl`(按 sid)+ Claude session jsonl + Codex thread;daemon 重启按持久化 sid 续接,不丢上下文 |
| **session = 独立一等实体** | 痛点 12 + 痛点 13 | 一个 chat/IM session 是**独立一等实体**,有持久 `sid`(`s<N>`,单调、扛重启、不复用);**role 是 session 的属性**(spawn 时绑 `claude [--agent <role>] --name|--resume <session-name>`),**同一 role 可并存多 session**。role 库住项目级 `.claude/agents/<role>.md`(Claude Code first-class spec);空 role = roleless(省略 `--agent`)。ccteam **不注入** system prompt —— 只决定「消息路由到哪个 sid」+「以哪个 role 启动」(详 §2) |
| **vendor 原生项目知识** | 痛点 10 + 痛点 13 | 项目知识层归 vendor:Claude session 自动读项目 `CLAUDE.md`,Codex 自动读 `AGENTS.md`。ccteam **不生成、不修改、不抑制**这些文件(R8);ccteam 的自定义只在 **role** 层(roleless session 则纯靠项目知识层当 brain)|
| **resume-by-session-id 会话** | 痛点 8/9 | session 按需 spawn(首条消息触发)+ 按持久 sid resume + 空闲释放;不常驻吊着(state 落盘,重启后 reconnect),避免影子 SoT(R4) |
| **零交互沙盒** | 痛点 8:每一步都点允许 | IM 路径的 Claude session 走 `--dangerously-skip-permissions`;只把 bot 暴露给可信 chat(`allowed_chat_ids` allowlist) |
| **失败必须可见** | 痛点 7 + 痛点 11 | gateway 的原则是失败不能静默挂住:submit/turn 超时、Claude tmux pane 死、Codex app-server 断,都翻成中文人话 + 下一步建议回到 IM / web |
| **预算硬上限** | 痛点 5 + 自激励 loop 防失控 | 成本 per-vendor 记账(`ccteam-cost`);`budgets.{claude,codex}.max_cost_usd_per_24h` per-vendor cap;CLAUDE.md $200 全 ccteam 物理上限兜底 |
| **headless 状态引擎** | 通知层不能改状态 | ccteam 核心是 headless 引擎,IM / web / 标准 API 都是可插拔前端,共用同一 gateway dispatch;任何前端**不得**引入新 LLM 层 —— LLM 推理只发生在 agent session(Claude/Codex)内部 |

---

## 2. 总体架构

### 2.1 核心模型:chat ⇄ project ⇄ session(role 是 session 的属性)

ccteam gateway 把 IM / web chat 消息路由到真实 Claude/Codex session。核心对象有三个 + 一个属性:

| 对象 | 含义 |
|---|---|
| **chat** | 一个 IM 私聊、群聊或 web chat。每个 chat 有自己的当前项目、当前 session 和 session 列表。 |
| **project** | 本地一个已 `ccteam init` 的项目目录,用 slug 标识。 |
| **session** | **独立一等实体** —— 一个可继续上下文的 agent 会话,有持久 `sid`(gateway 命名空间 `s<N>`,单调、扛 daemon 重启、不复用)。属于某个 chat × project × vendor,**以某个 role 为属性**启动。它是一个常驻 `ThreadHandle`,有独立 context(`/compact` `/clear` 各自独立);pane=`ccteam-chat-<slug>-<sid>`、turns=`.ccteam/chat/<sid>/turns.jsonl` 都按 sid。**同一 role 可并存多 session**(无 `(project,role)` dedup)。 |
| **role**(session 属性)| 一个可复用的 persona 定义 = 项目级 `.claude/agents/<role>.md`(frontmatter:persona + tools + disallowedTools + skills + mcpServers + model + effort + permissionMode + initialPrompt + memory)。session spawn 时 `claude --agent <role>` 让 vendor **自读**该文件 → session 以该 role 的 persona 跑。**空 role = roleless**:省略 `--agent`,裸 claude 自读项目 `CLAUDE.md`/`AGENTS.md` 当 brain。 |

一个 chat 可以同时活多个项目、多个 session(含同 role 的多个);另一个 chat 状态独立,不会串。

**两层知识/控制模型**:

- **① 项目知识层(vendor 原生,ccteam 不碰)**:Claude session 自动读项目 `CLAUDE.md`;Codex 自动读 `AGENTS.md`。老项目用自己的,新项目有啥读啥 —— ccteam 既不生成也不抑制(R8)。roleless session 纯靠这层当 brain。
- **② role / persona 层(ccteam 控制面)**:session 启动走 `claude [--agent <role>] --name <session-name>`。**v0.9.0 起 `ccteam init` 不种任何 role,默认 roleless**(空 role = 省略 `--agent`);用户用 `/role <role>` 换角色干活。role 只是 spawn 时绑的属性,不再是 session 的身份。

**persona 全在用户空间**(v0.9.0 废除内置 cto,引擎零 persona):role 来源 = 用户自建 .md 或从 **ccteam-hub 插件市场**(含 [agency-agents](https://github.com/wshobson/agents) 等开源 Claude 原生 .md 库;编排 persona 如 `team-brain` / `fable-advisor` 示例配方)装,落同一 `.claude/agents/`;装 role 走 `ccteam role search/add/list`(读 hub `index.json` + sha256 校验后 verbatim 写入)或 web 插件市场浏览器或手动丢 .md(详 §6.8)。编排 = 任意 session 经 `session_*` MCP 工具 spawn/dispatch(daemon 校验 per-session principal,best-effort 非硬边界,详 §6.4),不再限任何 role。

**chat-local 路由命令**(由 gateway 路由表 `is_gateway_command` 拦截,源自单表 `GATEWAY_COMMANDS`):

| 命令 | 作用 |
|---|---|
| `/cd <project>` | 当前 chat 切到项目(只切当前 chat) |
| `/newproject <slug> <path>` | 现场注册并建一个新项目 |
| `/new [vendor] [role]` | 在当前项目创建新 session(默认 vendor=claude、role=cto;**总铸新 sid**,同 role 也不复用)|
| `/use <session-id>` | 当前 chat 切到已有 session(按 sid)|
| `/role <role>` | **原地换当前 session 的 role**(底层 = 带新 `--agent` 重启,**保持同一 sid + pane**)|
| `/sessions` / `/projects` | 列当前 chat 的 session / daemon 已知项目 |
| `@handle <text>` | 路由到指定 session 并设为当前;不带 `@handle` 则发给当前 session |

> `@` 只指会话,**无 meta-handle**(v0.8.21 删除 `@ccteam`/nl_admin 遗留管线):确定性控制 = 上面的斜杠命令面;自由形式运维问题 = 普通聊天,由 session(如 cto)用工具回答。

`/compact` `/review` `/clear` 这类**不是** gateway 命令,会作为一个普通 turn / directive 透传给当前 session 的 adapter,由 adapter 翻译成 vendor-native 操作(详 §2.5)。

> **`/role` 的实现**(`switch_current_role`,gateway.rs):carry-context 原地换 —— 同 sid + 同 pane 名(`ccteam-chat-<slug>-<sid>`)以新 `--agent <role>` re-spawn,**保持同一 gateway sid**(`/use <sid>` 不失效);fresh-spawn 分支带 death-probe(失败不误报成功);无活动 session → 报错;同 role → no-op(不白扔 live context);保留原 vendor。`--agent` 是**启动期绑定**,换 role = re-spawn 该 session 的 persona。

### 2.2 daemon = IM/web⇄session 路由网关

无 slug 的 `ccteam start` 是一个常驻 gateway daemon,**不是** tick loop / orchestrator 循环(后台化由 `ccteam daemon start` 的 setsid pid-detach 负责,§10 daemon 生命周期行)。它在同一个 tokio runtime 内、共享一条 shutdown 信号(Ctrl-C / SIGTERM;v0.9.7 起 trigger-file 通道已废,`ccteam daemon stop` 发 SIGTERM),启动以下任务:

| 组件 | 位置 / 说明 |
|---|---|
| IM gateway | `ccteam-im::run_daemon_with_shutdown`;Telegram(等)long-poll 入站 + 出站发送;chat⇄project⇄session⇄role 路由表 |
| Web chat WS | `GET /ws/chat`(`ccteam-chat.v1`);CLI 层 mpsc bridge 把 browser frame 翻成 `ChannelMessage{channel:"web"}` 后接入同一个 Gateway |
| 标准资源 API | `/api/v1/*`(web-token auth);project / role / session 三资源 + `config/im`(IM 凭证)+ `/capabilities` + **`/hosts`(host-keyed agent 报告 + register-mcp,v0.8.18)** + `/status`(含 **per-session 成本**)(详 §2.6) |
| MCP socket | `~/.ccteam/run/mcp.sock` —— daemon-local line-delimited JSON-RPC handler,供 Claude/Codex plugin 调 ccteam 工具 |
| Web server | axum + SSE,默认 `http://127.0.0.1:7331`,服务 SPA bundle |

**关键约束**:此路径**不构造** `ccteam-flow::Orchestrator`,**不跑** supervisor tick(`ccteam-flow` 是推后的编排层,当前未接进运行中的 daemon,详 §7)。daemon 退出时**不 kill** tmux session(R5):下次 `ccteam start` 按持久化 sid 重新接管(Claude 按 deterministic 的 sid pane 名 `ccteam-chat-<slug>-<sid>` reattach;dead pane recreate 走 `--resume` lossless);未发送 / 失败的 IM 出站回复保存在 `~/.ccteam/imd/outbound.jsonl`,启动后重放。

**多用户 / per-tenant IM bot(v0.8.20)**:除全局/admin bot(`~/.ccteam/im/credentials.json`),daemon 还按 `~/.ccteam/tenants.json` 给**每个配了 IM 凭据的租户**起一个监听(`build_tenant_channels`),channel 命名 **`"<platform>@<tenant_id>"`** —— 唯一 channel-map key 让出站回信(`channels.get(reply_to.channel)`)落到**正确的 bot**,不串(gateway 早把 `reply_to`/每-turn 与 `owner`/ACL 分开,故 channel 名随 `ChatKey` 自然流过)。入站 ACL 用 `platform_of()`(剥 `@` 后缀,`ThreeLayerSec` 对未知 platform fail-closed);`chat_can_access` 把租户 bot 收敛到**只见自己的** session(不见共享 web 池/别租户;纯函数 `chat_owner_visible`)。热重载按 changed-scope(creds vs tenants,不 blip owner 活 bot),web `PUT /api/v1/me/im` 经 gateway `request_im_reload()` 触发即时起。诚实:同 OS uid 下软隔离、非安全边界;web↔IM 收敛已落地(W6):`canonical_owner(frontend)→identity` 把租户 web + 其 IM bot 统一成 `user:<tenant>` owner(`owner`=ACL 与 `reply_to`=投递 分开,回信不变),两前端互看自己全部会话。代码:`ccteam-im/src/{daemon,gateway,transport/*}.rs`。

### 2.3 执行层两轴:HarnessAdapter × ProcessBackend

执行层正交两轴,组合是 N+M 不是 N×M:

- **`HarnessAdapter`(vendor 怎么驱动)** —— Claude **两条 spawn 路径(v0.8.11 协议轴)**:① **`stream-json`(默认)** = 长驻 `claude` 子进程 + 双向 NDJSON 管道(`ClaudeStreamJsonAdapter`,无 PTY/pane/hook 链;daemon 读 stdout→直写 progress/turns.jsonl);② **`terminal`** = tmux TUI + `send-keys -l` + transcript-tail + 官方 PreToolUse / Stop 等 hook(`ClaudeTuiAdapter`,要逐字节终端镜像/attach/screenshot 时选)。Codex = `codex app-server` JSON-RPC。
- **`ProcessBackend`(进程跑哪)** —— tmux / inproc / remote 等承载位置。tmux pane 操作(capture / resize / pane_pid)**只**住在 `PaneBackend` 子 trait,不在 base trait。

一个 session = `HarnessAdapter(vendor, protocol)` × `ProcessBackend(host)`。`default_adapter_factory` 按 `(vendor, protocol)` 三路由:`(Claude, stream-json)`→`ClaudeStreamJsonAdapter`、`(Claude, terminal)`→`ClaudeTuiAdapter`、`(Codex, _)`→`CodexAppServerAdapter`。两个 vendor / 两条 Claude 通道都归一成**同一**中立 `CanonicalEvent` + `ApprovalIR`(每个用自家原生通道:Claude stream-json 走 NDJSON,Claude terminal 走 hook + transcript,Codex 走 JSON-RPC)→ gateway `spawn_event_pump`(唯一 turns/progress writer)零改动消费。

**v0.8.11 协议轴四缝**(`crates/ccteam-harness/src/execution/claude_stream_json/`,按 PRD §七 预折叠 v0.9 SessionHost):`spawn_spec`(纯 argv/env/cwd builder + 确定性 per-(slug,sid) uuid = 无状态 resume key)· `transport`(泛型 `(reader,writer)` 双向 NDJSON,消费端**不持** `Child` —— v0.9 WS 透明替换位)· `translate`(NDJSON→`ThreadEvent`,in-flight 关闭→人话 `TurnFailed`)· `mod`(adapter + live-session 注册表 + `SessionIdentity{sid,vendor_uuid,host}`)。slash = bridge 三类(known 透传 / dialog 人话拒 / unknown 当文本);HITL = `--permission-prompt-tool stdio` + `can_use_tool` 反向 RPC → 可插拔 `CanUseToolResolver`(deny 只挡该次工具)。**零注入**:persona 仅 `--agent`,禁 `--append-system-prompt` / `initialize.systemPrompt`。

**`HarnessAdapter` trait**(`crates/ccteam-harness/src/adapter.rs`)—— 生命周期/命令方法的中立契约。末两个方法 `handle_directive` / `thread_status` 是命令面 + 状态面,**无 default impl**(论证见 §2.5)。

```rust
#[async_trait::async_trait]
pub trait HarnessAdapter: Send + Sync {
    fn name(&self) -> &'static str;                 // "claude-tui" / "codex-app-server" / ...
    fn vendor(&self) -> AgentVendor;                // Claude | Codex
    async fn start_thread(&self, spec: &AgentSpecBrief, ctx: &SpawnCtx) -> Result<ThreadHandle, HarnessError>;
    async fn submit_turn(&self, h: &ThreadHandle, input: TurnInput) -> Result<TurnId, HarnessError>;
    fn events(&self, h: &ThreadHandle) -> BoxStream<'static, ThreadEvent>;
    async fn resume_thread(&self, persistent_id: &str) -> Result<ThreadHandle, HarnessError>;
    async fn close_thread(&self, h: &ThreadHandle) -> Result<(), HarnessError>;
    // 命令面 + 状态面(无 default impl,§2.5):
    async fn handle_directive(&self, h: &ThreadHandle, d: Directive) -> Result<DirectiveOutcome, HarnessError>;
    async fn thread_status(&self, h: &ThreadHandle) -> Result<ThreadStatus, HarnessError>;
}
```

- **`AgentSpecBrief`** 携带 `role`(spawn argv 用它拼 `--agent <role>`,**空 role = roleless 省略 `--agent`**)+ slug + cwd + sid(session-name 按 sid)。role-as-attribute 的落点就在这里:`spec.role` → claude argv 的 `--agent` 槽位(可空)。
- **`CanonicalEvent`** 是 `ThreadEvent` 的中立别名(`ThreadStarted` / `TurnStarted` / `TurnCompleted{usage}` / `TurnFailed` / `Item{Started,Updated,Completed}` / `Error`);schema 对齐 Codex `ThreadEvent`,两 vendor 的 emitter 1:1 映射进 gateway。`events()` 守 **final-only 契约**(rustdoc 钉死):一个 turn 的最终 agent 文本**恰好一次**经 `ItemCompleted(AgentMessage)` 吐出,`ItemUpdated` 仅 delta/presentation、consumer 可丢;token usage / plan / rate-limit 等非终态遥测**不**进此流(旁路镜像 `progress.jsonl`,经 `thread_status` 查询)。gateway 的取文本逻辑依赖此契约,故契约住 adapter trait。
- **`ExecutionMode`** 三态(`ThreadHandle::mode`):`InProc`(in-process subagent,占位)/ `Bg`(`claude --bg` / `codex exec --json` 单轮 fresh-context)/ `Chat`(tmux + claude TUI / `codex app-server` UDS,多轮 context 复用 —— IM/web 路径全走这个)。
- **resume 语义**:`Chat` 形态 resume-by-session-id —— Claude 用 deterministic 的 sid session 名 lossless 续接,Codex 用 thread id app-server resume;`Bg` 形态每次 spawn fresh,`resume_thread` 返回 `NotImplemented`。
- **`close_thread`** 是唯一杀长 session 的路径:正常只由用户主动触发(`project stop` / `project rm`、`/role` 换角色),绝不静默(R5)。

**Vendor-seam forward-compat**:ccteam 读 sub-harness 吐出的 `state.json` / `codex app-server` JSON-RPC 通知 —— 这些是 vendor 自有 schema,按自家节奏新增字段 / enum 值,ccteam 清不掉也管不了(与「不做历史迁移」红线只管 ccteam 自有 state 不冲突)。`ccteam-harness::warn_unknown_vendor_token(seam, token, detail)` 是 process-wide warn-once helper(`(seam, token)` 作 dedup key);未知 token → skip + warn,不中断 event stream。

### 2.4 harness × provider facet + 可扩展 AgentVendor

执行层的「vendor 选型」在 v0.8.6 抽象为两个 facet,都是 **session 属性、非顶层资源**:

- **harness = agentic CLI 驱动器**:每个一套 `HarnessAdapter`。当前五 vendor:**claude-code**(stream-json 主路,smoke 实证)· **codex**(app-server)· **grok / opencode / kimi**(共享 ACP core 薄壳);gemini-cli / 其它逐个 adapter 后续接入。
- **provider = 子 facet(模型)**:仅某 harness 支持多模型时有意义(主要 claude-code);多数 CLI 自带固定模型。

**`AgentVendor` enum**(`adapter.rs:106`,trait 一等公民,**无 default**)是这个可扩展性的类型锚 —— 当前 `{ Claude, Codex }`,新 vendor 加一个 variant + 一套 adapter 即可贯穿整个 spawn / pricing / cost roll-up / UI label / MCP wire 路径。「enum 无 default → workflow.yaml 必须 explicit / 由推断写入」是 vendor 红线。

per-adapter best-fit,不强行统一:

| Vendor | Harness | 为什么 | slash 行为(全量命令面见 §2.5) |
|---|---|---|---|
| Claude | tmux TUI session(`claude --agent <role>`) | 全 TUI + 耐久 + send-keys/transcript/hooks 已成熟;`--agent` 顶层可 resume(smoke 实证) | 开放集(skill/自定义/compact/clear/…)字面 `send-keys -l` 透传;弹窗型(model/…)bare→inline 选项、panel-only→`Rejected` |
| Codex | app-server JSON-RPC | 原生、文档化的控制平面;每条 slash 映射 native RPC / 查询合成 / override。**v0.8.6 Codex role 绑定推后**,只保证读 `AGENTS.md` 项目知识层 | `/compact`→`thread/compact/start`、`/review`→`review/start` 等六类映射;`/new` `/clear`→`Redirect`;弹窗 bare→两段式 inline 选项 |

两种 harness 可在同一个 chat 并发存在。输出统一成 IM 回复:gateway 先回 `submitted <session> turn <id>`,随后把 assistant / error 事件经同一条 outbound ledger 发回。当前可用 harness(×provider)由 `GET /api/v1/capabilities` 经 PATH probe 动态列出(§2.6)。

### 2.5 命令面 + 交互面 + 状态面(slash / 弹窗 / 反问 / `/sessions`)

把「在 IM 里像用 TUI 一样驱动 agent」做全,需要三个跨切面:**任意 slash 命令**有意义地执行或显式回执(无静默降级)、**弹窗选择型**命令(选 model / review target / …)在 IM 可答、**agent 主动反问**(`AskUserQuestion`)在 IM 可答,外加 `/sessions` 看每个 session 的 model + 上下文用量。设计约束:后续持续加 code agent 与 IM channel —— 故 **命令面实现内聚于各 vendor adapter、交互形态内聚于各 channel,两轴经中立类型彻底解耦**(新 vendor 只实现 `handle_directive`/`thread_status`、不感知 channel;新 channel 只实现选项渲染 + 回填归一、不感知 vendor)。

**中立词汇**(住 `crates/ccteam-harness/src/adapter.rs`,与 `ThreadEvent`/`ApprovalIR` 同层):`Directive{name,args,choice?}`(一条 slash,gateway 零知识转交)→ adapter 答 `DirectiveOutcome`(穷尽 5 值:`Turn`=成为一个 turn / `Done{receipt}`=即时完成 RPC|override、receipt 回 IM / `NeedsChoice(ChoicePrompt)`=需用户选、渲染后带 `choice` 重入 / `Rejected{reason}`=显式拒绝 TUI-only|不支持、一等回执非 `Err` / `Redirect{hint}`=语义重定向指向 gateway 命令如 `/new`)。选择型走 `ChoicePrompt{token,title,options,multi}` → 用户回 → 归一成 `ChoiceSelection{token,ids,free_text?}`。状态面 `ThreadStatus{model?,context?:ContextUsage{used,window}}`,`ContextUsage::render()` 是绝对值+百分比的**单一渲染点**(`188k / 1M (19%)`)。`handle_directive`/`thread_status` 两个 trait 方法**无 default impl** —— 新 vendor 漏写命令面/状态面就**编译失败**,杜绝「slash 静默降级成模型字面文本 / `/sessions` 静默空白」。`ChoicePrompt` 是 `ApprovalIR` 的交互前身,日后 HITL 批准复用同一 pending-registry + 回填路径。

**gateway 退纯路由**(`crates/ccteam-im/src/gateway.rs`):单行 slash → `Directive` → `handle_directive` → 渲染 5 outcome;gateway 自有命令由单表 `GATEWAY_COMMANDS` 派生(`is_gateway_command` + `/help` 渲染 + `Channel::register_commands` 注册到 native 菜单的子集都从这张表来);`NeedsChoice` 进 `PendingInteractions`(单飞 per (chat,session) + TTL + token 守卫),回填经 `resolve_selection` **token-global** 统一处理(一条 callback 路径既解 Directive 重入、也解 D6 hook 的 External 反问)。`/sessions` 是状态面单点:逐 session 调 `thread_status` → `status_suffix()` 拼到行尾(无状态的 bg adapter 答 `Default` → 不追加)。

**交互面在 channel**(`crates/ccteam-im/src/pending.rs` + `transport/mod.rs`):`PendingInteractions` 用**自己的锁**(独立于 gateway 锁,让 D6 的 600s-class External await 绝不持 gateway 锁),双 origin(Directive 重入 live / External oneshot)。transport 加 `MessageOption` + `SendMessage.options` + `ChannelMessage.selection` + `Channel::register_commands`,全 `#[serde(default)]` 零破坏:Telegram 渲 inline keyboard + 收 `callback_query`;web chat 渲 chips;其余 provider 走 content 内嵌编号文本兜底。

**Claude 命令面**(`crates/ccteam-harness/src/execution/claude_tui.rs`):`handle_directive` 四通道 gate —— ① prompt 开放集(skill/自定义/plugin)+ ② BRIDGE_SAFE local(compact/clear/usage/…)+ 未知 → 零知识透传 send-keys(开放集红线 R3 不变);③ arg-applicable popup(model/effort)带 args 直透、bare → `NeedsChoice`(**绝不盲发 bare 弹窗**);panel-only popup(config/agents/…curated)→ `Rejected`;④ agent 反问 → D6。逃生舱 `/esc` = send Escape。

**Codex 命令面**(`crates/ccteam-harness/src/execution/codex_app_server.rs`):`handle_directive` 六类映射(RPC 直映射 / 查询合成→`Done` / per-session override / 语义 `Redirect` / TUI-only `Rejected` / server 错误原文传播)+ 三层 resolution(内建表 → `skills/list` 缓存动态匹配 → `Rejected` 附候选);弹窗两段式(bare→list RPC/静态枚举→`NeedsChoice`→重入 apply)。`CodexThreadTracker` 是 harness 级 dispatcher,消费 `thread/tokenUsage/updated` 维护 per-thread token 缓存,喂 Codex `/status` 与 `/sessions` 的 `thread_status`。app-server transport **单轴** `resolve_codex_transport()`:有 `CCTEAM_CODEX_APP_SERVER_SOCKET` 走 UDS、否则默认 stdio;这只是 ccteam↔Codex 控制通道。**Codex 内的 ccteam MCP 另行统一走 HTTP**:`thread/start|resume.config.mcp_servers.ccteam={url,http_headers}` 携 session bearer 直连 daemon `/mcp`,不再每 thread 拉 `ccteam internal mcp-serve`。全局 `~/.codex/config.toml` 同为 HTTP admin 条目(0600);两边必须同 transport,因为 Codex 0.144.3 会深合并同名 global/thread 配置,混合 `command+url` 将拒绝 thread/start。`default_adapter_factory` 是 **per-vendor 单例**(`crates/ccteam-im/src/daemon.rs`)→ 每 daemon 恰好一个常驻 codex app-server 子进程跨 resume/turn 复用。

**D6 agent 反问**(`AskUserQuestion`):机制是「hook IS the interaction」—— `intercept_ask` 的 chat 变体(`crates/ccteam-hooks/src/intercept_ask.rs`)在 `mode: chat` session 把 `AskUserQuestion` 转成同一中立 `ChoicePrompt`,经 daemon `mcp.sock` 的 `interaction/ask` op 发 IM、阻塞等答案,返回 `allow + updatedInput.answers` 让 picker 跳过、模型直拿答案;超时/无 chat → 降级 bg deny-with-reason。`ensure_chat_hooks_installed` 追加一条 matcher `AskUserQuestion` 的 PreToolUse 条目(写 `.claude/settings.local.json`,见 §3.2)。

> 协议细节(命令→RPC 映射表、Claude local-jsx 名单、Codex 命令 drift 快照、各 channel 渲染)一律以代码为准,见 §10 指针表。

### 2.6 标准资源 API(`/api/v1`)

把 web 现用接口抽成一套**标准资源 API**(web 现用 → 将来 app / 独立端可直接集成)。核心是 **3 资源 + facet**,全走既有 web-token auth、版本前缀 `/api/v1`:

| 资源 | 端点 | 说明 |
|---|---|---|
| **project** | `GET /projects`(每项带 `host`+`host_online`)、`GET /projects/{slug}`(`api_v1.rs`,只读)· `POST /projects`(可带 `host`= 卫星 id,经 `project_init` op 远程建,§2.7)、`POST /projects/import`(接入卫星已注册项目)、`DELETE /projects/{slug}`(`routes/projects.rs`) | `DELETE` = **注销 + 停 session(deregister),不 file-purge、不触卫星**;破坏性 purge 留 CLI `project rm --purge`(§4) |
| **role** | `GET /projects/{slug}/roles`、`GET/PUT /projects/{slug}/roles/{role}`(`routes/roles.rs`) | 读 `.claude/agents` 库;core `ccteam_core::roles::{list_roles,read_role}` 解析 frontmatter |
| **session** | `GET/POST /projects/{slug}/sessions`、`GET /sessions/{sid}`、`POST /sessions/{sid}/turn`、`POST /sessions/{sid}/resolve`(HITL token-resolve)、`GET /sessions/{sid}/events`(SSE)、`POST /sessions/{sid}/stop`(`routes/sessions_api.rs`) | session = 独立一等实体(role 是属性),resume-by-session-id;`{sid}` = gateway `s{n}`(持久);POST 空 role = roleless;`/resolve {token,selection}` 走 `Gateway::resolve_web_selection`(= IM 点击同路,**非** turn) |
| **marketplace** | `GET /marketplace`、`GET /marketplace/{id}/body`(install 前预览)、`GET /projects/{slug}/marketplace`(带 per-project `installed_status`)、`POST /projects/{slug}/marketplace/install`(`routes/marketplace.rs`) | ccteam-hub 插件市场(track-upstream,详 §6.8):GET 读 hub `index.json`(经 `~/.ccteam/hub-cache/`,`?refresh=true` 强刷);install 从条目 `upstream` 拉内容(host 白名单 + sha256 校验)—— **agent → 项目 `.claude/agents/`;skill → 用户级全局库 `~/.ccteam/skills`(v0.9.9,项目零写入)**;`installed_status` 对 skill 按库三态、agent/plugin 按项目;`force` 覆盖 |
| **skills(全局库)** | `GET /skills`(`routes/skills.rs`,admin-only) | 用户级全局 skill 库 `~/.ccteam/skills` 清单(嵌套 id + description + 绝对 path;core `list_library_skills`);会话引用走 turn 附件 `{kind:"skill",name,scope:"global"}`(admin-only,远程 host 项目可读拒,详 §2.6 session 行 + §6.8);**无**任何「装进项目」通道 |
| **status** | `GET /status`(`routes/status.rs`) | daemon-wide 快照:`{daemon_healthy, sessions_live, sessions_idle, cost_today_usd, budget_cap_usd, ...}`;喂顶栏 cost pill + Status view(§6.6);无 gateway 时优雅 503 |
| **config/im** | `GET /config/im`(masked)、`PUT /config/im/{telegram,lark}`、`POST /config/im/telegram/chat-id/start` + `GET /config/im/telegram/chat-id`(异步轮询)(`routes/im_config.rs`) | **REST 配置面,非 MCP 工具**;GET 响应类型**根本不含** bot_token/app_secret(类型层杜绝明文回显,只 `*_last4`+counts);PUT 先 getMe / tenant_access_token 验过再落盘 0600;telegram chat_id 走背景 long-poll task 捕获;所有写返回 `restart_required`(creds 仅 daemon 启动时 load,无热生效) |
| **facet** | `GET /capabilities`(`routes/capabilities.rs`) | 动态列当前可用 harness(×provider):`HarnessCapability{id,vendor,available,providers}`;`available` = PATH probe(`<bin> --version` exit 0,进程级缓存) |

**gateway spine**(`crates/ccteam-im/src/gateway.rs`)—— 标准 API 的 drive 路径不动 `HarnessAdapter` trait,而是在 gateway 上加薄方法:

- `pub struct SessionView{ sid, ... }` + `session_views()` —— 列 daemon 内所有 tracked session;`tracked_chat_sessions(state_path)` → `TrackedSessionRow{slug,sid,role,vendor,...}` 从持久化 gateway state 文件读(CLI `session ls` / `status` 共用,无新 RPC);
- `create_session_api(...)` —— API 建 session(等价于 IM `/new`;空 role = roleless);
- `submit_to_sid(sid, text)` —— 向指定 sid 发 turn(等价于 `@handle`);
- `stop_session(sid)` —— 停指定 session;`session_resolve(sid)` → `SessionResolve{sid,role,vendor,project,project_dir}`(404 闸 + per-session pane/turns 解析);
- `GatewayEvent.sid: Option<String>` —— **SSE 过滤键**:per-session SSE handler 只保留 `sid` 匹配自己的事件。`None` = 不绑 session 的事件(如 `chat_send_file` MCP 路径、D6 `interaction/ask` hook prompt);IM 投递路径**忽略** `sid`(按 channel + chat_id 路由),故这是 additive。

**依赖方向(acyclic)**:共享 `Arc<Mutex<Gateway>>` 从 daemon 注入 ccteam-web `AppState`(**web → im 单向依赖**,无环);standalone `internal web`(无 daemon)→ gateway None → session 端点优雅 **503**(`gateway_unavailable`),只读 project/role/capabilities 仍可用。

> **前端落地**:统一 chat-shell SPA(§6.6)—— per-session `/chat/s/:sid`(走 gateway-sid `s{n}`)Chat|终端 tab + 底部全局导航 **插件市场 / Status / Settings** 三视图(Roles 页被插件市场浏览器取代)。standalone `internal web`(无 daemon)→ gateway None → session/status/marketplace-install/config 端点优雅 **503**,只读 project/role/capabilities/marketplace-catalog 仍可用。

### 2.7 多机 host 轴(v0.8.24 Track D;v0.9.0 反向连接)

**目标**:把「主机」从预留字段升级为可注册、可在线、可远程 spawn 的资源轴。本地机始终是 `local`;卫星机经 join-token 登记后出现在 `GET /hosts`。

**v0.9.2 归属反转:host 是 project 属性,session 继承**。「slug 相同 = 同一项目」的跨机隐式关联被废除(slug 撞名会误导:可能属不同用户/不同项目)。project catalog(`~/.ccteam/config.yaml`)每条 `ProjectEntry` 含 `host`(serde default `local`)、`remote_slug`(卫星侧线上身份,exec wire 用)、`remote_path`(仅展示);远程条目的 `path` = daemon 侧 data home(仅 `.ccteam/state.json`,turns/progress/cost 记账仍按 catalog slug 全在主 daemon)。**spawn 面(MCP `session_spawn` / REST create-session)无 host 参数,传入即硬错**;执行位置一律由 project 绑定解析。远程项目两条进入路径:① web 新建时选主机 → `POST /api/v1/projects{path,host}` → 控制通道 op 对 `project_init{nonce,path,slug}` / `project_init_result{nonce,ok,slug,path,error}`(nonce oneshot rendezvous,15s 超时可读错误)→ 卫星就地 bootstrap + 注册自身 config.yaml;② `POST /api/v1/projects/import{host,remote_slug}` 接入卫星心跳已上报的项目(**绝不自动接入**;catalog slug 撞名走数字累加;幂等)。`GET /projects` 每项带 `host`+`host_online`;`GET /hosts/{host}` 的 projects[] 带 `cataloged`+`catalog_slug` 反查。

**反向连接(v0.9.0 网络反转)**:**只有 main daemon 需要可达地址**(单端口 `:7331`)。卫星**零监听面** —— 所有流量出站:一条长驻 `ccteam-host.v1` 控制 WS(presence + report + exec 信令)+ 每次远程 spawn 一条 `ccteam-exec.v1` 拨回 WS。给 daemon 前置 HTTPS 反代即全链路 wss,卫星无证书无端口。**统一进程**:daemon 与卫星是同一个 `ccteam start`;是否卫星只取决于本机有没有 `state/hosts/self.json`(join 过),是否 daemon 只取决于有没有别人 join 它(registry 非空)——A join B、B join A 对称合法。注册状态只存 daemon 侧。

#### 拓扑

```text
[IM / web SPA] ──► main daemon (gateway + REST :7331)  ←—— 唯一需要可达的端口
                      │  host registry  ~/.ccteam/state/hosts/registry.json(只在 daemon)
                      │  join tokens    ~/.ccteam/secrets/host-join-tokens.json
                      │  HostChannelHub(内存:live 控制通道 + exec rendezvous)
                      │
                      ├── local spawn (stream-json / acp / terminal*)
                      │
                      │   ┌─── satellite『ccteam start』(零监听,全出站)───┐
                      ◄────┤ ① GET /api/v1/hosts/channel(ccteam-host.v1 长驻)│
                      │    │    report 周期 25s(agents/projects/version)     │
                      │    │ ◄─ exec_open{nonce} 信令                        │
                      ◄────┤ ② GET /api/v1/hosts/exec/{nonce}(ccteam-exec.v1)│
                      │    └── 每 spawn 一条,nonce 单次 + host 绑定 ─────────┘
```

\* `terminal` 协议(tmux/rmux/PTY)**永不**上多机(冻结红线);远程只接受 `stream-json` / `acp` / app-server 类长驻 stdio。

#### Join / 凭据

| 步骤 | 谁 | 动作 |
|---|---|---|
| mint | admin | `POST /api/v1/hosts/join-token` 或 `ccteam host mint-token --daemon … --web-token …` → 得到 join-token |
| join | satellite | `ccteam host join --daemon <url> --token <join-token>` → `POST /hosts/join`;落 `~/.ccteam/state/hosts/self.json`(host id + long-lived `agent_token`);本机 `ccteam start` 30s 内自动拨出上线 |
| 控制通道 | satellite | 出站 `GET /api/v1/hosts/channel`(Bearer = agent_token,子协议 `ccteam-host.v1`);`{"op":"report"}` 帧周期 25s 更新 registry(TTL 90s 判 offline);**在线 = 通道活着**(hub),秒级 presence |

join-token 是**运维登记面**(防误连),**不是**安全边界(同 OS-uid / LAN 信任模型不变;ACL 行诚实声明不变)。

#### 稳定性合同(生产级跨网)

- **保活/半开检测**:daemon 每 20s ping 控制/exec 两类 socket;任一侧 75s 无入站帧 → 判半开、主动拆链(exec 侧卫星 kill child,daemon 侧读到 EOF → 下次 re-gate + `--resume`)。
- **重连**:卫星指数退避 1s→60s(±20% 抖动防 fleet 同拍),连接稳定 ≥30s 后清零;同 host 新连接**踢掉**旧幽灵连接(NAT rebind 场景,hub generation + CancellationToken 水平触发)。
- **协议向前兼容**:控制通道未知 `op` 双向忽略不拆链;子协议版本经 `Sec-WebSocket-Protocol` 协商(`ccteam-host.v1` / `ccteam-exec.v1`)。
- **exec 不做字节级续传**:断链 = EOF → re-gate + vendor `--resume`(与本地语义一致);控制通道与 exec 连接互相独立,通道闪断不 kill 运行中 session。

#### Remote spawn(拨回 rendezvous)

1. 创建面**不带 host**(v0.9.2):gateway 以 `project_host_binding(slug)` 从 catalog 解析 `(host, wire_slug)`(`wire_slug` = `remote_slug` else slug;旧条目/空值规范化为 local);MCP/REST 若传 host → 硬错 `HOST_SPAWN_PARAM_REMOVED`。
2. Gateway `prepare_host_for_spawn(host, wire_slug, …)`:
   - `local` → 本机 adapter;
   - remote + `terminal` → 硬拒;
   - remote + unknown / offline(TTL) → **可读错误**,**不**创建 session、**不**删既有 session;
   - remote + online → `gate_remote_spawn_project`(wire_slug ∈ 该 host 上报 projects)→ `HubRemoteHostProxy`(要求 hub 里有 live 通道)→ `RemoteExecTarget{host_id, wire_slug, hub}` 进 `SpawnCtx.remote`。
3. Adapter `remote_exec::connect`:`hub.open_exec` mint 单次 nonce → 控制通道推 `exec_open{nonce}` → 等卫星拨回(15s 超时)→ 配对 `ExecBridge` → 发 `ExecSpec` 首帧(`slug` = wire_slug,卫星按自身注册表解析 cwd)→ `ExecStarted` → `(reader, writer)` 进 `spawn_from_io`(transport law 不变;帧角色按 daemon/卫星定,与谁拨 TCP 无关)。
4. Session `meta.json.host` = 继承的 project 绑定;rebuild/resume 三路径 `ensure_session_host_binding`:meta.host ≠ 当前 catalog 绑定 或 卫星 offline → **可读错误、绝不本地重生**;registry 与历史 session 保留。

#### ACL

- `GET /hosts` / mint-token / register-mcp = **admin-only**(`deny_non_admin`)
- join = join-token bearer 或 admin
- 控制通道 + exec 拨回 = host agent-token bearer(auth 层 `host:<id>` 身份;loopback no-auth 时 handler 内 bearer 兜底解析);nonce 单次 + host 绑定
- tenant 访问 hosts/join 面 → **403 fail-closed**

#### 代码指针

| 面 | 位置 |
|---|---|
| registry / join / report / gate | `crates/ccteam-core/src/host_registry.rs` |
| HostChannelHub(通道注册 + exec rendezvous)| `crates/ccteam-harness/src/execution/host_channel.rs` |
| 卫星 exec 引擎(协议盲字节泵)| `crates/ccteam-harness/src/execution/satellite_exec.rs` |
| 卫星客户端(出站控制通道 + 拨回)| `crates/ccteam-web/src/satellite.rs` |
| daemon 侧 WS(channel + exec 拨回泵)| `crates/ccteam-web/src/routes/hosts.rs` |
| remote proxy seam | `crates/ccteam-im/src/remote_host.rs` |
| gateway host gate + `create_session_api_on_host` | `crates/ccteam-im/src/gateway.rs` |
| CLI | `ccteam host {join,mint-token,ls}`(serve/heartbeat 已删——卫星即 `ccteam start`)|

---

## 3. 核心组件

### 3.1 Crate 拓扑

```
ccteam-cli (bin)
  ├── ccteam-im        (IM gateway + 路由 + gateway spine + 出站 ledger)
  ├── ccteam-flow      [推后的编排层 —— 不接进运行中的 gateway daemon,详 §7]
  ├── ccteam-web       (SPA dashboard + 标准资源 API,axum + SSE)
  ├── ccteam-hooks     (hook dispatch → progress.jsonl)
  ├── ccteam-harness   (执行层:HarnessAdapter × ProcessBackend × PaneBackend)
  ├── ccteam-core      (primitives leaf:paths / state / roles / progress re-export / vendor / ...)
  └── ccteam-cost      (pricing / budget / token usage —— leaf,无 ccteam 依赖)
```

**依赖方向**(权威,以各 crate `Cargo.toml` 为准,R9):

- `ccteam-cost` 是叶子,不依赖任何 ccteam crate。
- `ccteam-harness` 只依赖 `ccteam-cost`。
- `ccteam-core` 依赖 `ccteam-harness` + `ccteam-cost` —— 即 **`core → harness → cost`**(core 在上,cost 在底)。
- `ccteam-im` / `ccteam-flow` / `ccteam-web` / `ccteam-hooks` 依赖 `ccteam-core` + `ccteam-harness` + `ccteam-cost`;`ccteam-web` 额外直依赖 `ccteam-im`(标准 API 的 gateway spine,acyclic)。
- `ccteam-cli` 是 bin,依赖以上全部。

> 拓扑只能是 `core -> harness -> cost`,**不要**翻成 `harness -> core`。`cargo tree` 验环:`ccteam-web` → `ccteam-im` 单向(`tests/dep_graph_test.rs` 锁)。

**progress 写入权威**:`ccteam-harness::progress_bridge` 是 `progress.jsonl` 业务事件 schema 的单一权威,`ccteam-core` 只 re-export(R2)。chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<sid>/turns.jsonl`(**按 sid**),**不**依赖 Anthropic 内部 `~/.claude/projects/`;live daemon 的唯一 turns writer 是 gateway `spawn_event_pump` 的 ANSWER 分支(按 sid `append_turn`)。

### 3.2 项目布局与初始化

`ccteam init` 把任意 cwd 变成 ccteam 项目,**只写 ccteam 自己的东西**:

```
<project>/
├── src/ tests/ ...                   # 业务代码,永远不动
├── CLAUDE.md / AGENTS.md             # 项目知识层(vendor 原生)—— ccteam 不生成/不改/不抑制
├── .claude/
│   ├── agents/cto.md                 # ccteam 种的默认 role(唯一 ccteam-managed 指令面)
│   └── settings.local.json           # ccteam 托管设置(hook + base);gitignored,与用户 settings.json 合并
└── .ccteam/                          # ccteam 项目状态(gitignored)
    ├── state.json                    # per-project ccteam 元数据
    └── workflow.yaml                 # 拓扑声明(vendor + trigger;无 prompt)
```

**v0.8.6 关键变化**(相对 orchestrator-era):

- **ccteam 不再生成/桥接/抑制 `CLAUDE.md` / `AGENTS.md`**(R8):项目知识层归 vendor + 项目。删除了 `render_project_claude_md` 等全部生成逻辑;老项目 init 这些文件原样不动。
- **`.ccteam/` 只写 `state.json` + `workflow.yaml`**:停写 spec.md、`.ccteam/agents/`(中立拷贝,0 reader)、`.ccteam/skills/`、各 `.gitkeep`。
- **`.claude/agents/cto.md` = 唯一 ccteam-managed 指令面**:`cto.md` 单一源 = `ccteam_core::CTO_ROLE_MD`(`crates/ccteam-core/src/templates/cto_role.md`);CLI scaffold(`DEFAULT_AGENT_SCAFFOLDS = [("cto.md", CTO_ROLE_MD)]`)+ core `bootstrap_project_at_dir`(IM `/newproject` / API create_project)**都**种它 → 无论哪条建项目路径,默认 `--agent cto` 都有文件可加载。
- **ccteam 的 hook 写 `.claude/settings.local.json`,不碰用户 `.claude/settings.json`**:本地层 gitignored、Claude settings 层级照读、与用户 settings.json 合并 → 零冲突、不脏用户 git;ccteam 只 merge/清自己的 hook 段。真实 `claude --agent cto` 下 hooks(SessionStart/UserPromptSubmit/**Stop**)在 settings.local.json 全触发(smoke 实证)。
- **slug 撞名 = 数字累加**:默认 slug = 目录名(slugify);撞名 → `demo` / `demo2` / `demo3`(弃旧 `-{4hex}` 后缀,可读)。非交互可 `--slug` 显式;同一 path 重复 init = re-init 刷新。

业务代码 / `.git/` / `.env` 永远保留。`progress.jsonl` 业务事件 SoT **不落项目内**,而在全局 `~/.ccteam/progress/<slug>.jsonl`(按 slug 分文件;全局根布局见 §3.3)。

### 3.3 全局目录布局

`~/.ccteam/` 是单一根,规范布局收敛到 `ccteam-core/src/paths.rs` 单一 manifest(`canonical_home_dirs() == ["hooks", "progress", "run", "state"]`),所有 path accessor 从此派生:

| 路径 | 用途 |
|---|---|
| `hooks/` | hook 脚本(`ccteam internal hook` 子命令落地处) |
| `progress/<slug>.jsonl` | per-project 业务事件 SoT(R2) |
| `run/mcp.sock` | daemon-local MCP socket |
| `state/{pid}` | daemon liveness |
| `config.yaml` | 项目注册表 + 全局配置(`projects[]` / `budgets` / retention) |
| `imd/outbound.jsonl` + `imd/registry/<slug>` | IM 出站 ledger + bot 注册 |
| `im/credentials.json` | IM token + `allowed_chat_ids` allowlist |
| `web-token` | 非 loopback web auth token(mode 0600) |
| `cost-budget.json` | per-vendor cost ledger |

**v0.8.6 变化**:init **停建** orchestrator-era 死目录(queue / memory / log / phases / control / templates);其余按需 mkdir。`ccteam doctor` 加 home-layout drift 检查(报告偏离 manifest 的目录)。

### 3.4 Cross-project Memory(差异化护城河)

主路径完全复用 Claude Code / Codex 官方记忆机制,**检索发生在 agent session 内部,ccteam-core 零 memory 检索代码**(R8):

| 通道 | 路径 | 加载方式 |
|---|---|---|
| 项目内累积 | per-repo auto-memory(`/memory`)+ `<project>/CLAUDE.md`(Claude)/ `AGENTS.md`(Codex) | 每 session 启动加载(vendor 原生) |
| 跨项目共享(Claude) | `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`(支持 `paths:` frontmatter scope) | 每 session 启动加载,匹配路径才生效 |
| 跨项目共享(Codex) | `~/.codex/AGENTS.md` | Codex 加载机制注入 |

`<team>-<slug>` 项目目录前缀(R10)让 `~/.claude/rules/*.md` 的 `paths:` frontmatter 正确 scope 到该项目。reviewer/work-role 的 prompt 模板引导 agent 自写经验;ccteam 不写检索代码。

**可选增强**:用户装了 [claude-mem](https://docs.claude-mem.ai) 则它自带 hook 自动捕获 + 暴露 read-only MCP search 工具;ccteam 不写检测 / 集成代码,**LLM 自看 tool surface 决定调不调**;没装则 100% 走默认路径。

---

## 4. 关键流程

### 4.1 一条 IM / web chat 消息的端到端路径

```
用户在 IM 或 `/app/chat` 发 "@reviewer 看一下这个项目的 README,给我三条风险"
   │
   ▼  IM gateway 收入站(经 allowed_chat_ids allowlist 校验)/ web `/ws/chat` 收入站(web token auth)
   │
   ▼  Gateway::handle_text:解析 @reviewer → 定位/创建 chat 当前 project 的 reviewer session(分配新 sid)
   │     (首条消息触发 HarnessAdapter::start_thread,以 `claude --agent reviewer --name <session-name>` 启动;
   │      已存在则按 sid 复用 ThreadHandle)
   │
   ▼  HarnessAdapter::submit_turn(handle, TurnInput::UserText("看一下..."))
   │     Claude: tmux send-keys -l "<text>" Enter
   │
   ▼  gateway 立刻回入口:"submitted <session> turn <id>"
   │
   ▼  HarnessAdapter::events 流(Claude: hooks fast event + transcript tail;Codex: JSON-RPC)
   │     → CanonicalEvent::ItemCompleted{AgentMessage} / TurnCompleted{usage}
   │     → 镜像写 .ccteam/chat/<sid>/turns.jsonl(R2,gateway spawn_event_pump 按 sid)+ progress.jsonl + cost ledger
   │     → GatewayEvent{ sid: Some("s3"), ... } 兼供 IM 投递 + per-session SSE
   │
   ▼  outbound ledger 把 assistant 文本发回 IM / web(at-least-once;失败重放)
```

> **turn 完成检测实证细节**(smoke):`--agent <role>` 顶层 turn 触发 `Stop`(→ `chat_turn_completed`,turn 完成检测命根)+ `SessionStart`/`UserPromptSubmit`/`Pre+PostToolUse`;有时**也**触发 `SubagentStop`(顶层被建模为 implicit-main 的 subagent)—— 但**不会双发 IM 回复**:回复只走 transcript-content track(`ItemCompleted{AgentMessage}`),hook track(stop/subagent-stop)只写 `progress.jsonl`,两轨解耦。roleless session(无 `--agent`)同样触发这套 hooks。

### 4.2 失败与恢复

gateway 的原则:失败必须可见,不能静默挂住。

| 故障 | 用户看到 | 恢复方式 |
|---|---|---|
| agent 启动失败 | `会话启动失败: ... 下一步: ...` | 看 daemon log,确认 `claude` / `codex` 在 `PATH` 且已登录 |
| 缺 `cto.md` 的旧项目 | `--agent cto` 启动失败 | pre-v1.0「清旧数据 + 重 `ccteam init`」策略;init 必种 cto.md |
| Claude tmux pane 死 | `发送失败: ... 下一步: ...` | `ccteam stop && ccteam start` 再发同一 `@handle`(deterministic 名 reattach;dead pane recreate + `--resume` lossless,失败回退 fresh spawn + emit `chat_session_reset{reason}`) |
| Codex app-server socket 断 | `发送失败: ... 下一步: ...` | 确认 `codex app-server` 可用;重启 daemon(app-server resume 接回 thread) |
| turn 超时 | `发送失败: turn timed out ... 下一步: ...` | 稍后重试;反复出现则 `/compact` 或 `/new` |
| daemon 被 kill | 短暂离线 | 重新 `ccteam start`;session 和 outbound ledger 恢复 |
| 某 session context 不可用 | —— | `/new` 建新 session;旧 session 不影响同 chat 其他 session |

> **resume 与 persona**:`--resume` 沿用历史里展现的 persona(in-context 模仿,非缓存);`change-persona` / 改 `cto.md` 只在 **fresh start** 生效。`/role` 本就 fresh-spawn,无影响。

---

## 5. 数据与文件协议

完整字段、JSON schema、文件命名规则、事件类型清单**以代码为准**(见 §10「协议 → 代码位置」指针表)。本节只保留架构约束:

| 子节 | 架构约束 | 代码位置(SoT) |
|---|---|---|
| 全局目录布局 | `~/.ccteam/` 单一根;`canonical_home_dirs()` 是布局 manifest | `ccteam-core::paths`(`CcteamPaths` / `canonical_home_dirs`) |
| 项目级 state.json | 原子写(`.tmp` + rename);per-project 元数据;损坏走 backup | `ccteam-core`(`ProjectState`) |
| progress.jsonl | **唯一业务状态事实来源** —— 写 schema 单一权威 = `ccteam-harness::progress_bridge`,core re-export;终端输出不参与状态判定 | `ccteam-harness::progress_bridge` / `enriched_event::EventKind` |
| turns.jsonl | chat 对话原文 SoT,ccteam-owned `<project>/.ccteam/chat/<sid>/`(**按 sid**)| `ccteam-harness`(turns mirror)+ gateway `spawn_event_pump`(唯一 live writer)|
| role 定义 | `.claude/agents/<role>.md` frontmatter | `ccteam-core::roles`(`list_roles` / `read_role`) |
| outbound ledger | `~/.ccteam/imd/outbound.jsonl`;at-least-once IM 投递,daemon 启动后重放 | — |

**关键论证**:「progress.jsonl 唯一事实来源」是架构红线(R2)。曾考虑解析 tmux `capture-pane` 输出做状态判断 —— 拒,因为终端文本格式不稳定、ANSI 转义难、对 prompt cache 敏感(R6)。所有状态转移走 hook + transcript jsonl + app-server event,deterministic 且可重放。

---

## 6. Claude Code / Codex 扩展点映射

### 6.1 Tmux 长 session(Claude chat session = 独立 sid)

chat session = per-sid 一个 tmux session + `claude [--agent <role>]` TUI 长跑;dual-track 观测,**不 scrape pane**(R6)。

```bash
SESSION="ccteam-chat-<slug>-<sid>"       # tmux session 按 sid deterministic 命名
# fresh spawn:--agent <role> 让 vendor 自读 .claude/agents/<role>.md(R3 兑现;
#             空 role = roleless → 省略 --agent,裸 claude 读项目 CLAUDE.md);
#             --name 让 Anthropic 把 session jsonl 落在 deterministic 名下(按 sid),
#             使日后 recreate 能 --resume(argv 顺序以 claude_tui.rs spec_for_new 为准);
#             CCTEAM_CHAT_SID/CCTEAM_CHAT_SECRET 注入 pane env(chat_spawn_env_owned)
tmux new-session -d -s "${SESSION}" -c "${PROJECT_DIR}" \
  "claude [--agent <role>] --dangerously-skip-permissions --name <session-name>"
# 输入面:tmux send-keys -l 直送 user content + Enter(literal 模式,0 escape 雷区);
#         /compact /new /clear 经 handle_directive 透传(§2.5)
# 输出面 Track A:官方 hooks(SessionStart/UserPromptSubmit/Stop/SubagentStop/PostToolUse)
#                 作 fast event 通道(低延迟 turn boundary;hook 从 CCTEAM_CHAT_SID 取 sid)
# 输出面 Track B:byte-offset 增量读 transcript jsonl → 抽 full message content →
#                 镜像写 ccteam-owned .ccteam/chat/<sid>/turns.jsonl(R2 SoT,按 sid)
```

**关键约束**:✅ 用 `--dangerously-skip-permissions`(消灭弹窗,痛点 8;hitl session 例外用 `--permission-mode default`,见 §6.5);✅ `--agent <role>` = role-as-attribute 的 persona 绑定(R3 兑现;空 role 合法省略);❌ **不**用 `claude -p`(失去 attach + 每 turn 冷启 cache 失效);❌ **不**设 `--max-turns`(长跑,由超时 + 成本上限兜底)。

**daemon-restart 上下文恢复**:dead-pane recreate 走 `claude [--agent <role>] --dangerously-skip-permissions --resume <session-name>` lossless lookup(`spec_for_resume`,resume 路径**也带** `--agent`,roleless 时同样省略)—— Anthropic 自己 reload session jsonl,cache + 推理链续接;失败才 fallback fresh spawn(`spec_for_fresh` 委托 `spec_for_new`,继承同样的 `--agent` 条件)+ emit `chat_session_reset{reason}`(user-visible degraded,不冒充 resume)。tmux 命令包在 `tokio::process::Command` 异步 spawn —— 单 binary 零额外运行时依赖。

### 6.2 Role 库(`.claude/agents/<role>.md`)

agent 行为住 `.claude/agents/<role>.md`(Claude Code first-class spec),Claude 起 session 时按 `--agent <role>` 读对应文件:

```markdown
---
name: reviewer
description: Review the project and surface risks
tools: [Read, Grep, Bash]
model: claude-opus-4-5
---

You are a reviewer agent. ...
```

`ccteam init` 种默认 `cto.md`(管家 role,3 职责见 §2.1);work-role 由用户自建 / 从 **ccteam-hub 插件市场**装(§6.8),落同一目录。**这是 ccteam 唯一管理的"指令面"** —— 与「No prompt injection」红线不冲突:ccteam 不向 pane / app-server 注入 system prompt,而是让 vendor 用原生 `--agent` 机制自读 role.md(R3 兑现)。**roleless session 省略 `--agent`**(空 role 短路 `ensure_role_exists`),裸 claude 读项目知识层当 brain —— 同样不违反红线(不注入 ≠ 必须有 role)。`session persona` / API `PUT /roles/{role}` 改写 role.md body(保留 frontmatter),下次该 session fresh start 生效。

### 6.3 Hooks 配置

完整 settings 模板、Hook 事件用途**以代码为准**(`ccteam init` 落 `.claude/settings.local.json`;hook impl = `ccteam internal hook` 子命令,见 §10)。本节只保留架构论证:

**为什么 hooks 是可观测性命脉**:Claude Code hooks 是 deterministic 的 —— 同一事件触发同一脚本,这是把「AI 的随机推理」转成「系统可处理的事件流」的桥。ccteam 把工具调用 / turn 边界事件经 hooks 落到 progress.jsonl + turns.jsonl,**完全不解析 tmux 终端文本**(R6)。

**实现形态**:hook 实现是 `ccteam internal hook <name>` 子命令 —— 单 binary 分发,与 daemon 共享同一份 serde schema(progress 事件、state.json 字段),不依赖独立 bash / python 运行时。可选地 `CCTEAM_HOOK_VIA_DAEMON=1` 把 hook 事件经 `~/.ccteam/run/hook.sock` 转给 daemon,使 daemon 成为 progress.jsonl 单一 writer,消除两-writer race。

**写在 `settings.local.json`**(v0.8.6,§3.2):ccteam 的 hook 段注入项目本地层(gitignored、与用户 settings.json 合并),**不碰用户的 `.claude/settings.json`**。`ensure_chat_hooks_installed` merge ccteam 的 hook 条目(含 D6 的 `AskUserQuestion` PreToolUse matcher);`remove_chat_hooks` 在 `project rm --purge` 时外科剔除(塌成 `{}` 才删文件)。

**cost 来源**:Claude bg 形态 cost 由 Claude Code 自己写 `~/.claude/jobs/<job_id>/state.json::cost_usd_total`;chat / Codex 形态由 adapter 从 `TurnCompleted{usage}` event 取 `UnifiedTokenUsage`(`ccteam-cost`)按 per-model pricing 估算,写进 per-vendor ledger `~/.ccteam/cost-budget.json`。`ccteam doctor --check-cost-orphan` 扫近 24h vendor 完成事件对账 ledger,缺失即 WARN。

### 6.4 MCP servers

#### 消费的 MCP(ccteam 不写,只接)

| MCP | 用途 |
|---|---|
| Telegram | 统一走 `ccteam-im` gateway transport;凭证 `~/.ccteam/im/credentials.json` |
| claude-mem | 跨项目记忆**可选增强**(read-only search / timeline + 自带 hook);ccteam 不写集成代码,LLM 自看 tool surface 决定用不用 |
| Playwright / GitHub | E2E 测试 / PR 管理(优先 `gh` CLI) |

#### 提供的 MCP:`ccteam`(**7 工具**,0 STUB)

MCP 工具共 **7**(v0.9-T1 cull 15→8:删 advise 2 / admin_change_persona+add_tool / chat bot 3,`admin_ls`→`status`;2026-07-26 再删 tmux 时代遗留 `screenshot` → 7,web `/screenshot/<slug>.png` 路由与 IM `/screen` 命令保留)。wire 名裸(`session_spawn` 等),客户端按 server key 加命名空间(模型见 `mcp__ccteam__session_spawn`),**server name 不变**(`ccteam`):

| Group(子前缀) | 工具数 | 工具 |
|---|---|---|
| `status`(admin 组,无子前缀) | 1 | 发现面:项目/daemon 健康/今日成本 + caller 项目绑定主机的 vendor 面板 + advisory 模型目录 + routing notes |
| `chat_` | 1 | send_file(把 daemon 文件系统上的文件发回 caller 绑定 chat,与文本回复同 outbound funnel) |
| `session_` | 5 | A2A 调度:spawn / dispatch / collect / list / stop(**daemon 校验 per-session principal + project 维度**,best-effort defense-in-depth 非硬边界;调度门与护栏 = AGENTS §三红线) |

`STUB_TOOLS: &[&str] = &[]`(`crates/ccteam-cli/src/mcp_tool_groups.rs`)是 invariant 守门员;`ccteam doctor --verify-mcp` 自检 stub-counter parity + 总数(7),drift → exit code 1。`CCTEAM_DISABLE_TOOLS` 用 group enum(非 glob,防 typo):`CCTEAM_DISABLE_TOOLS=chat,session`。完整 tool schema = `tool_definitions()`(单一权威 `ccteam-im/src/mcp/protocol.rs`,cli 薄包装 re-export)。

**`session_` 调度门(defense-in-depth,非硬边界)**:① cto role.md `tools:` 行授予 5 个 `mcp__ccteam__session_*`(work-role 模板不列 → Claude allow-list 第一道;**注**:MCP 工具可能绕 vendor per-agent allow-list,故此层 best-effort、非承重);② **per-session secret 校验(安全相关层)**:spawn 时 daemon mint 128-bit secret 注入 pane env(`CCTEAM_CHAT_SECRET`,随 `CCTEAM_CHAT_ROLE`),存 `sid→{role,secret}`;stdio forwarder 转发 `_caller_secret`;daemon `execute_session_tool` 先跑廉价 role 预筛(`session_caller_authorized`,gateway down 也拒明显非 cto),再 `Gateway::verify_session_caller` 用 constant-time 比对认证 `(role,secret)` 对(**不信明文 role**,缺/错 secret fail-closed);③ **project 维度**:`session_spawn` 只在 caller 自己 slug 建 session(无 project 参数),`dispatch`/`collect`/`stop` 先 `assert_caller_owns_session`(`session_resolve(sid).project == _caller_slug`),跨项目 sid 拒。collect = polled MVP(tail 子 session `turns.jsonl`,`since` 游标 + `n` 上限)。④ **主会话 fallback(v0.9.1)**:非 ccteam 拉起的本机日常会话(无 principal env)由 stdio forwarder 读 admin web-token(0600 同 uid 证明)转发 `_caller_admin_token` → daemon 常数时比对后按 Admin 语义服务(root spawn 无 parent,project = 显式参数或 cwd 前缀解析;仅本地 socket,HTTP 路剥该参数;无 token fail-closed)——「调用 codex」从任何本机会话可走账本内路径。**诚实范围**:单 OS-uid 全信任模型下 agent 间**无硬边界**(同 uid 可读他 pane 的 env / 文件 / ptrace → 拿到 secret),secret 只**抬高门槛**,**不 close** 漏洞;真隔离 = per-agent OS user / sandbox(v0.8.8 deferred)。

> **曾经的 `workflow_*`(15→8→0)与 4+3 bundled skill 均已退役**:推后编排的 marker 工具 v0.8.6 无 consumer,session 生命周期改走 CLI(`project`/`session` 组)/ IM(`/new` `/role` `/stop`)/ 标准 API(§2.6)。原 skill 功能落 MCP 工具 + cto role + work-role + config CLI。

**Wire 命名纪律(v0.9.1)**:工具 wire 名**裸**(`status` / `session_spawn` …)——MCP 客户端按 server key 加命名空间,模型见 `mcp__ccteam__session_spawn`(旧的内嵌 `ccteam__` 前缀会双前缀成 `mcp__ccteam__ccteam__*`,已删无 alias);server key 恒 `ccteam`。

**Wire 协议纪律**:`ccteam internal mcp-serve` stdout 是 line-delimited JSON-RPC frame channel,**所有 tracing / 日志走 stderr**,否则污染 frame parse。两条 transport(stdio + daemon 的 `~/.ccteam/run/mcp.sock`)共用同一 handler。

#### admin actions:change-persona + add-tool

- daemon-side **只做文件 mutation**:`change_persona` 读 `.claude/agents/<bot>.md` 替换 body(保留 frontmatter)写回 + emit `persona_changed`;`add_tool` 读 `workflow.yaml` parse `agents[bot].tools:` 去重 append + emit `tool_added`。**不调 LLM**(R3)。
- 生效路径:bot 下次 fresh start 读新 `.claude/agents/<bot>.md`(因 session 真的 `--agent <role>` 加载它,W1 起天然生效)。

### 6.5 安全

- **per-session 权限模式**:每个 session 有 `PermissionMode { Skip(默认), Hitl }`(`ccteam-harness/adapter.rs`,挂 `SpawnCtx.permission_mode`,穿所有创建路径:IM `/new claude cto hitl` 尾 token、web `POST /sessions` `permission_mode`、cto `session_spawn` param;`/role` 切换 + 重启 resume 保留)。
  - **`Skip`** → spawn 走 `--dangerously-skip-permissions`(YOLO,无手机批准门;agent 按本机 Claude Code 权限直接执行)。
  - **`Hitl`** → spawn 走 `--permission-mode default`(**绝不** skip,否则 ask-path 被白嫖)+ per-session 注入 vendor 原生 `PermissionRequest` hook(无 matcher = 全工具、无 timeout 让长审批不被杀)→ 非 allowlist 工具触发 hook → `ccteam-hooks/permission_request.rs` 经 `permission/ask` JSON-RPC 走 mcp.sock → daemon `execute_permission_ask` 建 Approve/Deny ChoicePrompt(token-keyed External pending)弹到绑定 chat → 用户点同意才放行。**回应路两条同源**:IM 点击 = `resolve_selection`,web 点击 = `POST /sessions/{sid}/resolve` → `resolve_web_selection`,两者都 `take_by_token`→`apply_pending` 同一 pending(非 turn)。**FAIL-SAFE = deny**;permission prompt 用比 600s interaction/ask **更短的 TTL**(`PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT`,env `CCTEAM_PERMISSION_PROMPT_TTL_SECS`)+ outstanding 时写一行 `chat_permission_prompt_outstanding` progress(operator 知道 parked);deny **只挡该次工具、不带 `interrupt`、不 kill turn**(守 R5)。**不注入 system prompt**(R3:批准门是 vendor 原生 hook,非注入)。Codex 忽略该 lever(自有 sandbox)。
- **Telegram allowlist**:`~/.ccteam/im/credentials.json` 的 `allowed_chat_ids` 是第一层边界,生产不留空;bot token 不进 git;daemon 只跑在你控制的机器上;Web UI 只绑 `127.0.0.1` 除非明确配反代 + 鉴权。Lark/Feishu allowlist `allowed_user_ids`(open_id)**fail-closed**(空=拒绝所有人,与 telegram 相反)。

### 6.6 Web 仪表盘 + 标准 API

`crates/ccteam-web/` 是 Vite + React SPA(`build.rs` 在 `cargo build` 时跑 `npm run build` → rust-embed,`CCTEAM_SKIP_WEB_BUILD=1` 跳过);backend axum + SSE,服务 SPA bundle + 标准资源 API(§2.6)+ SSE + WS。

**统一 chat-shell 布局(v0.8.9+,v0.8.10 补 UX 状态)**:两套分叉的 SPA 布局(旧 operator UI 的 Dashboard/ProjectDetail/SessionDetail/SessionsList/Teams*/WorkflowView + 各 panel + operator 侧栏/顶栏)**已删**,收敛成**一个** chat 风格外壳(`App.tsx` shell + `ChatConsole.tsx`):顶栏 = crumb + 连接态 + **cost pill**(`CostPill.tsx`,读 `GET /api/v1/status`)+ per-session **Chat | 终端** tab;底部全局导航(v0.8.18 = 四视图)= **插件市场**(`MarketplaceView.tsx`,§6.8)/ **Status**(`StatusView.tsx`,daemon 健康 + sessions live/idle/疑似卡 + 最近活动 + 今日 cost/budget + **per-session 成本列**,读 `GET /api/v1/status` + `/sessions`)/ **主机**(`HostsView.tsx` + `lib/hostsApi.ts`,host-keyed agent 报告 + register-mcp,读 `GET /api/v1/hosts`,v0.8.18)/ **Settings**(`SettingsPage.tsx`,IM config + 多用户档0 段)。界面语言 **中 / English**(默认中,`lib/i18n.ts` + `hooks/useWebSettings.ts`,导航/面包屑随语言)+ 点头像个人设置(`components/AvatarMenu.tsx`:显示名/头像/语言/登出)。旧 Roles 只读页被插件市场浏览器取代。统一 dark+amber 设计 token。

**Authentication**:loopback 免 token;非 loopback 自动生成 `~/.ccteam/web-token`(mode 0600)+ LAN-RCE 倒计时;URL shim `?token=ccteam:<hex>` → HttpOnly cookie + 303 干净 URL。标准 API `/api/v1/*` 全走同一 web-token auth。

**Chat 控制台**:`/app/chat` 通过 `GET /ws/chat` + 子协议 `ccteam-chat.v1` 进入 Gateway。`ccteam-web` 持有 web-local JSON 帧和 mpsc 端点;`ccteam-cli::web_chat_bridge` 在 `run_start` 装配处把它翻译成 `ccteam-im::transport::ChannelMessage` / `SendMessage`,所以 web 与 IM **共用同一个 `Gateway::handle_text`、同一批 session、同一个 outbound ledger**。

**per-session web 视图**:`/chat/s/:sid`(`ChatConsole.tsx` 按 `s{n}` keyed)= 每个 gateway session 独立页 + 历史 + 干净切换(不混流):历史走 `GET /api/v1/sessions/{sid}`(`session_resolve(sid)` 作 404 闸 → 读 `<project_dir>/.ccteam/chat/<sid>/turns.jsonl`,**按 sid**、**非**按 `session_id==s{n}` 过滤 progress.jsonl);事件走 `GET /api/v1/sessions/{sid}/events`(按 `ev.sid==Some(s{n})` 过滤的 gateway broadcast SSE);per-sid localStorage(`ccteam.chat.rows.v2.${sid}`),sid 变即 reseed + 重订阅。HITL 审批 ChoicePrompt 经 SSE 带 `sid`+`token`+每 option `{label,id}` 渲染成 per-session 琥珀色行;点击 `[Approve]`/`[Deny]` → `POST /sessions/{sid}/resolve {token,selection=id}` → `Gateway::resolve_web_selection`(`take_by_token`→`apply_pending`,= IM 点击同路,**非** turn)→ 阻塞的 `permission/ask` hook 即收 allow/deny(工具真跑 / 即时拒,无 600s 超时)。终端(`ccteam-pty.v1`)经 `resolve_session_pane(sid)` 共享 helper 解析 per-session pane(claude=`chat_session_name`/codex=`codex_chat_session_name`;no-gateway→503、unknown sid→404),`send_keys`/`resize` 走 `default_backend()`;终端 UI 当前 claude-only(codex pane 后端已支持,真机验后再放开)。**v0.8.9 起默认 rmux backend 逐字节保真**(见下「byte-faithful 终端」),不再需 `CCTEAM_MUX_BACKEND=tmux`。NewSessionModal 含「(无角色 / 裸 claude)」选项(`ROLELESS` sentinel + `resolveRole` 守显式 roleless 不 fallback cto)。

**OpenAPI 自动文档**:`GET /api/docs`(`utoipa-scalar` 交互式 UI)+ `GET /api/v1/openapi.json`(OpenAPI 3.1 spec)。spec 由**同一套路由注册**生成(单聚合 `OpenApiRouter`,`routes/openapi.rs` `split_for_parts()` 既出 live `Router` 又出 spec → 单源、anti-drift);每 `/api/v1` handler 挂 `#[utoipa::path]`,加/删路由不注解会改 op 数、drift 测试红(强制 dual-edit)。两者 mount 在同一 `auth::auth_layer` web-token 门后(无公开未鉴权 spec)。**Scalar UI 自托管(去 CDN)**:vendored `@scalar/api-reference` standalone JS(`crates/ccteam-web/assets/scalar-standalone.js`,pin 版本)经同源 `GET /api/docs/scalar-standalone.js` 提供,custom-html loader 指向它而非 `cdn.jsdelivr.net` —— `/api/docs` 离线/锁网机可渲染,零外部 host(产品主打自托管)。

**Settings(IM config)页**:`SettingsPage.tsx`(独立 tab)配 telegram + lark 凭证,经 `/api/v1/config/im`(§2.6,web-token 门后)—— GET 永不回显明文(响应类型不含 secret 字段,只 `*_last4`);token/secret input 永 password + **不预填**已存值,覆盖已配 secret = 内联两态确认(非 `window.confirm`);telegram chat_id 走背景 long-poll task 捕获 + 前端 cheap status 轮询(可取消);所有写提示 `restart_required`(creds 仅 daemon 启动时 load,无热生效);LAN 明文警示 + lark 空 allowlist fail-closed 警示。复用 onboarding 拆出的 `telegram_validate_token_with_base` + `telegram_poll_chat_id_with_base` 两 pub fn。**这是 REST 配置面,不增 MCP 工具**(MCP 仍 15)。

**byte-faithful 终端(v0.8.9)**:`rmux_backend`(`crates/ccteam-harness/src/rmux_backend.rs`)的 pane 输出从**有损行流**(`PaneLineItem::Line`,剥 `\r`、丢 ANSI)切到**裸字节流**(`output_stream()` / `PaneOutputChunk::Bytes`),`capture` 改排 `Oldest` 字节 backlog(回放当前屏幕)→ **默认 rmux backend 即逐字节保真**(修 v0.8.8 连上空白 + 换行/对齐歪),不再需 `CCTEAM_MUX_BACKEND=tmux`。rmux pin **0.5**(`PaneOutputStream`/`PaneOutputChunk::Bytes`/`Oldest` 自 **0.3.1** 起就有 → 保真本不依赖 0.5;升 0.5 取 tmux-compat/window APIs,call-site 0.3→0.5 byte-identical,实测零漂移);`pattern matching`(marker / `chat_turn_completed` / tail-silence)消费者不退;tmux backend 不变。xterm.js 拿真裸字节渲染全屏 TUI(光标 + 颜色 + 精确换行)。

**架构红线**:web 守 R2(SSE watcher 仅读 progress.jsonl)/ R5(不 kill 长 session)/ R6(不解析 tmux 终端;裸终端字节只走 `ccteam-pty.v1`)/ R8(不写跨项目记忆)。写控制走跟 IM channel **完全相同**的 gateway dispatch 路径。ccteam 核心是 **headless 状态引擎** —— 任何前端**不得**引入新 LLM 层:web/API 输入 = 经 gateway 写控制,不经任何 ccteam 中介 LLM;LLM 推理只发生在 agent session 内部。

### 6.7 透明度与可观测性

| 你要看 | 命令 / 文件 |
|---|---|
| daemon 活性 + 所有 project/session(role/vendor/status/sid/last-event)+ web token/url | `ccteam status`(嵌套列每 project 各自 sessions;两行 web token + url 带 LAN ip)|
| 每 session 一行(SLUG/SID/ROLE/VENDOR/STATUS,从 gateway tracked map)| `ccteam session ls` |
| 安装和依赖 | `ccteam doctor` / `ccteam doctor --verify-mcp` |
| 最近 daemon 日志 | `tail -120 /tmp/ccteam.log` |
| outbound ledger | `~/.ccteam/imd/outbound.jsonl` |
| 项目状态 / 业务进度 | `~/.ccteam/progress/<slug>.jsonl` / `<project>/.ccteam/state.json` |
| 当前可用 harness | `GET /api/v1/capabilities` |
| 一屏看全 | web SPA(SSE 实时) |

**Stall 检测(软告警,不强制 kill)**:`agent_spawn` 后无对应完成事件超阈值 → 软告警。**永远不主动 kill**(R5)—— 除非命中物理上限(per-vendor cap 或全 ccteam $200),或用户显式 `project stop` / `rm --force`。相信长跑、相信用户能介入。

### 6.8 插件市场(ccteam ↔ ccteam-hub ↔ project,v0.8.12 = track-upstream)

把 role/agent/skill/workflow 的**内容**从 ccteam repo 里彻底搬走(repo 零提示词类型插件,**v0.9.0 起零例外**),换成一个 **curated marketplace + track-upstream ingestion** 的三角。**v0.8.12 起从「vendor 拷贝每个文件进 hub」改成「跟踪 upstream 仓库」**:hub 只存指针,内容在**安装时**从上游 raw URL 拉进本机 —— **agent 进项目、skill 进用户级全局库(v0.9.9)**(顺带让**多文件 / 目录型 skill** 天然可装 —— 正是旧 vendor-copy 模型卡住的地方)。

- **内容源 = `firstintent/ccteam-hub`**(独立 repo):一个 `index.json`(plugin 条目:`id` / `type`(agent|skill|workflow)/ `name` / `description` / **`upstream`**(可直接 raw 拉取的 URL @pinned-sha)/ **`content_sha`** / `source` / `license` / `tags` / 可选 **`manifest`**(多文件 skill 的每文件 `relpath`+sha256))+ `LICENSES/`。**零 vendored body**:外部源经**幂等 ingestion**(`sources.json` 声明整仓 @sha + glob → `scripts/sync.py` clone @sha → glob → 算 `content_sha`(读 upstream,**不拷**)→ 写 pointer 条目;**skill id 从目录名取**(修 `*/SKILL.md` stem='SKILL' dup 崩),多文件 skill 枚举目录得 `manifest`,**全局 id 去重**;一个 GH Action 周期重跑)—— agency-agents(`wshobson/agents`,MIT,192 agent)+ mattpocock-skills(MIT,29 skill 含 9 多文件)是已登记源。**第一方**(`source: ccteam`,如 pk/autoloop)内容仍住 hub,`upstream` 指向 hub 自己的 raw tree(hub 即 upstream)。幂等 + 同 sha 重跑 byte-identical。
- **ccteam 端读取 + 缓存**:ccteam 经 HTTPS github-raw 读 hub `index.json`,本地缓存 `~/.ccteam/hub-cache/`(`CcteamPaths::hub_cache_dir`,纳入 `canonical_home_dirs()`);`?refresh=true`(REST)/ 首次访问触发拉取。
- **install 落盘(v0.9.9 双目标)**:从该 plugin 的 **`upstream` URL** 拉内容(**非** hub base),**host 白名单**(只 `raw.githubusercontent.com` + loopback,表外 host → `HubError::HostNotAllowed`,拉前就拒)+ 落盘前 **sha256 校验**(对账 `content_sha`,防篡改/半截)。**agent** → 项目 `.claude/agents/<id>.md`(`write_role`,不变);**skill** → **用户级全局库** `~/.ccteam/skills/<id>/`(单文件 `write_library_skill`;多文件按 `manifest` **整批 fetch+verify 后再落盘** `write_library_skill_file`,中途 sha 失败不留半成品),**项目零写入**。`installed_status`:skill 对**库**三态(单文件 sha / 多文件整 manifest 比对),agent/plugin 仍对项目。`force` 才覆盖。签名 = `install_plugin(project_dir, library_root, plugin, target_stem, force)` / `installed_status(project_dir, library_root, plugin)`(库根显式注入,测试免 env)。
- **全局 skill 库(v0.9.9)**:仓根唯一 `~/.ccteam/skills`(`CcteamPaths::skills_dir`,纳入 `canonical_home_dirs`);id = 相对仓根 POSIX 路径(可嵌套,`validate_skill_library_id`,段字符集 `[a-z0-9][a-z0-9_-]*`);递归扫 `**/SKILL.md`(跳隐藏段)= `list_library_skills`。**两线不相灌**:全局 skill 只能会话显式 attach(路径指针),**禁止** link/copy 进项目;项目自有 skill = `.agents/skills/` 实体(可进 git)+ `.claude/skills` → 软链(`ccteam skill ensure-project` / `migrate-project`,init 不种);整仓登记 `ccteam skill source add/update/ls/rm`(元数据 `~/.ccteam/skills/.sources.json`);远程/卫星项目对库装与全局 attach = 可读拒绝(MVP local-only)。
- **用户入口**:① CLI `ccteam role search/add`(agent;遇 skill 拒绝并指到 `skill add`)+ `ccteam skill search/add/ls/rm/update`(库);② web 插件市场浏览器(`MarketplaceView.tsx`):`GET /api/v1/marketplace`、`GET …/{id}/body` 预览、`GET /api/v1/projects/{slug}/marketplace`(installed_status)、`POST …/marketplace/install`,skill CTA = 「安装到库」;③ 会话引用:composer ＋ 菜单两段(项目自有 / 全局库 admin-only)→ turn 附件 `scope=global` 路径指针(§2.6 skills 行)。

**代码归属**:`crates/ccteam-im/src/hub.rs`(catalog load + cache + upstream-fetch + 双目标 install,主逻辑)+ `crates/ccteam-core/src/{hub.rs,admin_actions.rs,roles.rs,paths.rs}`(core 侧 raw-base / 库原语 `skills_dir`·`validate_skill_library_id`·`write_library_skill(_file)`·`list_library_skills` / 项目面 `.agents/skills` 优先扫描)+ `crates/ccteam-web/src/routes/{marketplace.rs,skills.rs}`(REST,挂进 `/api/v1` OpenApiRouter、web-token 门)+ `crates/ccteam-cli`(`Command::Skill` 组)。ccteam-hub 侧:`scripts/sync.py` + `sources.json` + `index.json`。**红线**:内容 verbatim(不改写)、content_sha 校验(完整性)、host 白名单(只已登记源 host)、ccteam repo 零提示词内容(§三,零例外)。

---

## 7. 推后:ccteam-flow 编排层(非当前运行态)

> 以下是**推后**的自动编排能力,住在独立 crate `ccteam-flow`,**当前未接进运行中的 gateway daemon**。这里只记其存在与红线,供编排层落地时不退基线;**不要**把它当成当前 daemon 的运行方式。当前运行态由用户在 IM / web / API 里**手动**驱动多个 session(§2)。

`ccteam-flow` 设计目标是一个文件系统驱动的 thin orchestrator(声明式 `workflow.yaml` 拓扑 + trigger:`manual` / `schedule` / `gate` / `watch:<path>`;bg-job 形态 Claude `claude --bg --agent`、Codex `codex exec --json`)。`workflow.yaml` 红线:**不许**出现 `prompt:` / `system_prompt:` / `messages:` 字段(R3);agent 行为住 `.claude/agents/<role>.md`。该层的**编排级 HITL 批准**(`workflow.yaml` mode-as-state-SoT)、self-healing fix-loop、squad 跨 session 路由、5 类编排模式均随它一并推后;`ApprovalIR` 是该编排层的类型占位。

> 注:**per-session 交互式批准已 v0.8.7 落地**(§6.5 的 `PermissionMode::Hitl` + `PermissionRequest` hook,走 IM 弹窗),与此处推后的「编排层 workflow.yaml 批准 state SoT」是两件事 —— 前者是 hitl session 单工具放行/拒绝,后者是声明式编排的审批节点。非 hitl session 仍 `--dangerously-skip-permissions`。

> **已退役**(v0.8.6 删除,非推后):flex(`TeamKind::Flex` + `SessionRecord` + `ProjectState.sessions` + `.ccteam/sessions/` + `session add/ls/attach/rm`)、`.ccteam/ready`、webhook 路由 + `webhook-token`、`.ccteam/spawn_requests/` 的 live 写入路径(模块留 flow crate,daemon 不创建)。

---

## 8. 关键风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| **`--agent <role> --name/--resume` 在 tmux send-keys 路径** | role-as-attribute 模型不成立 | smoke gate 已实证(交互 + resume + hooks 全触发;roleless 省略 `--agent` 同样跑通);见 §4.1 |
| **agent session 卡死 / turn 超时** | 用户等不到回复 | submit/turn 超时阈值 → 中文人话 + 下一步建议回 IM;stall 软告警;per-vendor cap 兜底 |
| **daemon 死了消息沉默** | 命令写到磁盘但永不消费 | action 工具在 daemon 不健康时直接返回 error,绝不假装成功;`ccteam status` 看 liveness |
| **成本失控** | 一夜烧光 | per-vendor `max_cost_usd_per_24h` + 全 ccteam $200 物理上限;不限 max_turns;`doctor --check-cost-orphan` 对账 |
| **`--dangerously-skip-permissions` 被滥用** | rm -rf 用户文件 | `allowed_chat_ids` allowlist + hook 拦危险 Bash + 项目隔离;只暴露给可信 chat |
| **Claude tmux pane 死 / daemon restart 丢 context** | bot 失能 / 上下文断 | deterministic 名 reattach;dead pane recreate 走 `--resume` lossless,失败 fresh spawn + `chat_session_reset{reason}` |
| **缺 cto.md 的旧项目** | `--agent cto` 启动失败 | pre-v1.0「清旧数据 + 重 init」;init 必种 cto.md |
| **state.json 损坏** | 启动崩溃 | `.tmp` + rename 原子写;启动校验 schema,损坏走 backup |
| **vendor 协议变更** | hook 字段 / CLI flag / RPC 失效 | vendor-seam forward-compat warn-once 降级(skip + warn,不 panic);capabilities PATH probe |
| **Channel 单点(IM bot 死)** | 通知不到用户 | outbound ledger at-least-once + 重放;daemon 重启续接 |

---

## 9. 本文档的位置

- `requirements.md` 回答 **为什么做** 与 **谁会用**(核心痛点)。
- 本文档 `tech-design.md` 回答 **怎么做** —— 架构论证、设计权衡、扩展点选择,描述**当前**架构;**协议确切长什么样以代码为准**,见 §10。
- `usage.md` 回答 **怎么用**(用户命令手册,纯命令)。

所有实现 PR 必须能映射回:① `requirements.md` 的某条痛点 ② 本文档某个组件 / 流程 ③ 改协议同步代码 + §10 指针表。无法映射的,先放进 backlog 而非合入主线。

---

## 10. 协议 → 代码位置(代码是唯一 SoT)

旧 `interfaces.md` 已退役。协议细节(CLI / JSON / event / 路由 / schema)全部以代码为准 —— 文档不再维护第二份会漂移的副本。下表是「想看 X → 去代码哪」的指针;有自检的优先跑自检。

| 协议 | 代码位置(SoT) | 自检 / 速查 |
|---|---|---|
| 文件系统布局 / 路径 | `crates/ccteam-core/src/paths.rs`(`CcteamPaths` + `canonical_home_dirs`) | `ccteam doctor`(home-layout drift) |
| 项目 state.json | `crates/ccteam-core/src/state.rs`(`ProjectState` serde) | — |
| role 库读取 | `crates/ccteam-core/src/roles.rs`(`list_roles` / `read_role` / `RoleSummary` / `RoleDetail`,frontmatter 解析) | `cargo test -p ccteam-core roles` |
| 默认 cto role 模板 | `crates/ccteam-core/src/templates/cto_role.md`(导出 `ccteam_core::CTO_ROLE_MD`)+ `commands.rs::DEFAULT_AGENT_SCAFFOLDS` | — |
| progress.jsonl 事件 schema | `crates/ccteam-harness/src/execution/progress_bridge.rs` + `enriched_event.rs`(`EventKind`) | schema 单一权威 |
| chat turns.jsonl(按 sid)| `crates/ccteam-harness/src/execution/turns_mirror.rs`(`append_turn`/`read_all_turns`,目录键 = sid)+ live writer = `ccteam-im/src/gateway.rs`(`spawn_event_pump` ANSWER 分支按 `session.id` append) | `cargo test -p ccteam-im event_pump_writes_turns` |
| CLI 命令 / flag(分组) | `crates/ccteam-cli/src/main.rs`(clap `Command` / `Project` / `Session` / `Internal`)+ `commands.rs` | `ccteam --help` |
| `config` setup hub | `crates/ccteam-cli/src/commands.rs`(`run_config_menu` + `config <key> <value>`/`get`/`show`/`mcp`;包 `ccteam_im::onboarding::telegram_setup`) | `ccteam config show`;`cargo test -p ccteam-cli config` |
| 删除/停止引擎 | `crates/ccteam-cli/src/commands.rs`(`run_remove`/`RemoveOptions`/`RemoveReport` + `run_project_stop` + `stop_project_chat_sessions`(注入 `&dyn ProcessBackend` → 默认 rmux backend 真停,去 tmux-only)+ `purge_project_managed_paths`) | `cargo test -p ccteam-cli --test remove_test` |
| settings.local.json hook merge/scrub | `crates/ccteam-core/src/tool_surface.rs`(`ensure_chat_hooks_installed` / `remove_chat_hooks`) | `cargo test -p ccteam-core tool_surface` |
| MCP 工具清单 / schema(15) | `crates/ccteam-cli/src/mcp_tool_groups.rs`(`STUB_TOOLS` / `ToolGroup`,含 `Session`;chat 4:删 DEAD `chat_send_input`/`chat_history`)+ `mcp_serve.rs::tool_definitions` + `mcp_{admin,chat,advise,session}_tools.rs` | `ccteam doctor --verify-mcp`(drift → exit 1) |
| cto 调度 `session_*`(spawn/dispatch/collect/list/stop)+ secret 门 + project 维度 | `crates/ccteam-cli/src/mcp_session_tools.rs`(5 工具 + forwarder 转发 `_caller_secret`)+ `main.rs`(`execute_session_tool` + role 预筛 `session_caller_authorized` + project 维度 `assert_caller_owns_session`,best-effort 非硬边界);secret = `ccteam-core/src/session_secret.rs`(`mint`/`ct_eq`);认证 + 解析 = `ccteam-im/src/gateway.rs`(`verify_session_caller` 校验 `(role,secret)` 对;`session_resolve` → project 维度 + tail 子 `turns.jsonl`) | `cargo test -p ccteam-cli session_tool_tests`;`cargo test -p ccteam-im verify_session_caller`;`ccteam doctor --verify-mcp`(session 5/0) |
| HITL 权限模式(per-session) | `crates/ccteam-harness/src/adapter.rs`(`PermissionMode { Skip, Hitl }` + `SpawnCtx.permission_mode`);spawn argv + hook 安装 = `execution/claude_tui.rs`(`permission_args` → skip 用 `--dangerously-skip-permissions`、hitl 用 `--permission-mode default` + 注入 `PermissionRequest` hook) | `cargo test -p ccteam-harness claude_tui` |
| HITL 批准回路 `permission/ask` | hook 端 = `crates/ccteam-hooks/src/permission_request.rs`(读 stdin → `permission/ask` JSON-RPC over mcp.sock,FAIL-SAFE deny,**无 `interrupt`**);daemon 端 = `crates/ccteam-cli/src/main.rs`(`execute_permission_ask` 建 Approve/Deny ChoicePrompt + `summarize_tool_input` + `gateway.session_sid_for` + 短 TTL `permission_prompt_timeout_secs` + `emit_permission_prompt_outstanding` parked-signal);回应两路同源 = IM `resolve_selection` / web `Gateway::resolve_web_selection`,都复用 `ccteam-im/src/pending.rs` `take_by_token` | `cargo test -p ccteam-cli permission`;`cargo test -p ccteam-im web_resolve`;`#[ignore]` `claude_agent_hitl_*` smoke(真 binary) |
| ccteam-hub 插件市场:catalog / install(`ccteam role`/`ccteam skill` + web)| hub catalog load + cache(`~/.ccteam/hub-cache/`)+ 双目标 install(sha256 校验;agent `write_role` → 项目 / skill `write_library_skill(_file)` → 全局库)= `crates/ccteam-im/src/hub.rs`(`load_catalog` / `hub_base` / `HubIndex` / `HubPlugin` / `install_plugin(project_dir, library_root, …)`);core 侧 = `crates/ccteam-core/src/hub.rs` + `CcteamPaths::{hub_cache_dir,skills_dir}` + 库原语(`admin_actions.rs`/`roles.rs`);CLI = `ccteam-cli/src/main.rs`(`Command::{Role,Skill}`)+ `commands.rs`(`run_role_*` + skill 组 add/ls/rm/update/source/ensure-project/migrate-project;`role add` 遇 skill 拒绝) | `cargo test -p ccteam-im hub`;`cargo test -p ccteam-core hub`;`cargo test -p ccteam-cli --test skill_command_test` |
| 标准 API:marketplace(REST)| `crates/ccteam-web/src/routes/marketplace.rs`(`GET /marketplace`、`GET /marketplace/{id}/body`、`GET /projects/{slug}/marketplace`(带 `installed_status`)、`POST /projects/{slug}/marketplace/install`(`force`);`?refresh=true` 强刷缓存;委托 `ccteam_im::hub`) | `cargo test -p ccteam-web marketplace`;`cargo test -p ccteam-web --test openapi_test`(op-count drift) |
| 标准 API:status(daemon-wide 快照)| `crates/ccteam-web/src/routes/status.rs`(`GET /status` → `StatusResponse{daemon_healthy, sessions_live, sessions_idle, cost_today_usd, budget_cap_usd, ...}`;喂 cost pill + Status view;无 gateway→503) | `cargo test -p ccteam-web status`;`cargo test -p ccteam-web --test openapi_test`(op-count drift) |
| per-session web 历史 / SSE / 审批 / 终端 pane(按 sid)| 历史 = `crates/ccteam-web/src/routes/sessions_api.rs`(`GET /sessions/{sid}` → `session_resolve` → `collect_session_turns` 读 `.ccteam/chat/<sid>/turns.jsonl`;按 sid 过滤 SSE `event_matches_sid`;审批 SSE frame 带 `token`+option `{label,id}`;`POST /sessions/{sid}/resolve` → `handle_session_resolve` → `Gateway::resolve_web_selection`;POST 空 role = roleless);终端 pane = `routes/session_pane.rs`(`resolve_session_pane(sid)` 共享 helper,`pty_ws.rs` + `pane_snapshot.rs` 共用,no-gateway→503/unknown→404);SPA = `web/src/pages/ChatConsole.tsx`(`/chat/s/:sid` keyed)+ `pages/chatDefaults.ts`(`ROLELESS`/`resolveRole`)+ `lib/sessionsApi.ts` + `hooks/useSessionEvents.ts` | `cargo test -p ccteam-web sessions_api`;`cargo test -p ccteam-web --test openapi_test`;vitest |
| OpenAPI spec / `/api/docs` | `crates/ccteam-web/src/routes/openapi.rs`(单聚合 `OpenApiRouter` `split_for_parts()` + `Scalar` UI + `openapi.json` serve);各 handler `#[utoipa::path]` 注解(`api_v1.rs`/`projects.rs`/`roles.rs`/`sessions_api.rs`/`capabilities.rs`/`teams_*`);spec.version = `env!(CARGO_PKG_VERSION)` | `cargo test -p ccteam-web --test openapi_test`(op-count drift 测试) |
| Hooks impl / settings | `crates/ccteam-hooks` + `ccteam internal hook` 子命令;`ccteam init` 落 `.claude/settings.local.json` | — |
| Web 路由总装 | `crates/ccteam-web/src/routes/mod.rs`(router 合并) | — |
| 标准 API:project | `crates/ccteam-web/src/routes/projects.rs`(POST/DELETE)+ `api_v1.rs`(GET list/show) | 真实 HTTP smoke(W5b) |
| 标准 API:role | `crates/ccteam-web/src/routes/roles.rs`(GET list / GET·PUT one) | — |
| 标准 API:session | `crates/ccteam-web/src/routes/sessions_api.rs`(GET/POST + `{sid}` + `/turn` + `/resolve` + `/events` SSE + `/stop`;POST 接受空 role = roleless,无 422) | `cargo test -p ccteam-web --test openapi_test`(op-count drift) |
| web 附件 + 技能附加(composer ＋ 菜单;vendor 通用)| 两步流:`POST /projects/{slug}/uploads`(raw body → `<project>/.ccteam/uploads/`,25 MiB 上限,远程 host 项目可读拒绝)+ `GET /projects/{slug}/skills`(`.claude/skills/*/SKILL.md` 清单)= `crates/ccteam-web/src/routes/uploads.rs`;turn 面 `TurnForm.attachments[]`(校验:file/image 钉死 uploads 目录、skill id 字符集 + 已装;空 text 有附件即合法;远程会话可读拒绝)+ 组文 `build_turn_text_with_attachments` = `sessions_api.rs` —— 复用 IM 同一行语法 `[attachment image_path|file_path="…"]`(共享助手 `attachment_line`/`attachment_path_key` = `ccteam-im/src/transport/mod.rs`,`wrap_inbound` 同源);skill 行按 vendor 渲染于**单点缝** `skill_attachment_line(vendor,…)`(wire 中立 `{kind:"skill",name,scope?}`;`scope=global`(admin-only)= 用户级全局库 `~/.ccteam/skills/<id>/SKILL.md` 路径指针,嵌套 id 过 `validate_skill_library_id`,远程 host 项目可读拒):claude → 点名原生 Skill 工具 `/name`、codex → 嵌 plaintext mention `$name`(其 `TOOL_MENTION_SIGIL='$'`)、grok/opencode/kimi(ACP 无原生 loader)→「read SKILL.md and follow」兜底;读侧项目面 `ccteam_core::list_skills`(`.agents/skills` 优先,遗留 `.claude/skills` 实体兼容)+ 全局库 `GET /api/v1/skills`(`routes/skills.rs`,admin-only);SPA = `components/ChatComposer.tsx`(＋菜单两段:项目 / 全局库 admin-only;chips/拖拽/粘贴/上传中阻发)+ `lib/attachmentsApi.ts` + `submitTurn(attachments)` | `cargo test -p ccteam-web --test sessions_api_test`(upload→turn 组文/skill/拒绝面 4 个 e2e);`cargo test -p ccteam-core --lib roles`;vitest `attachmentsApi`/`ChatComposer` |
| v0.8.21 历史会话恢复 + 外部收编(列 stopped / cold-resume / 发现+import)| per-session `meta.json`(spawn 原子写、`/stop` 不删、扛重启)= `crates/ccteam-harness/src/execution/session_meta.rs`(`list_session_metas` 按 `last_active` desc、`touch_last_active`、`discover_external_claude_sessions` 读 jsonl tail 的 `cwd` 内容发现 + `from_utf8_lossy` 抗 CJK 切断 + subagent 排除);编排 = `crates/ccteam-im/src/gateway.rs`(`list_history_sessions` 过滤 live、`resume_stopped_session`(绑 `expected_slug`)、`import_external_session`(uuid cwd 归属校验、先 spawn 后写 meta)、`find_meta_for_sid` 跨项目扫;create 后写 meta、turn 完成 `touch_last_active`;IM `/use` cold-resume + `owner_identity_visible` ACL);REST = `crates/ccteam-web/src/routes/sessions_api.rs`(`GET …/sessions/history`、`POST …/sessions/{sid}/resume`(绑 slug)、`GET …/external-sessions`、`POST …/sessions/import`,全过 `can_see_project`)+ SPA `web/src/pages/ChatConsole.tsx`(HistorySection + import dialog,admin-gated)+ `lib/sessionsApi.ts` | `cargo test -p ccteam-harness --test session_meta_test`;`cargo test -p ccteam-web --test sessions_api_test`(history/resume/import 端到端);`cargo test -p ccteam-im im_use_cold_resume`;`… resume_stopped_session_rejects_cross_project_slug_before_spawn` |
| v0.8.21 Wave-2:`meta.json` 成 session 唯一 SoT(`gateway-state.json` 退役)| 退役 `SavedGatewayState.sessions` vec + `SavedGatewaySession` + `gateway_state_path_in`;daemon 只持久**路由** `routing.json`(per-chat 焦点 `default_project`/`current_project`/`current_session` + `live_sids`)+ **单调计数** `next-sid`(独立文件,扛 routing/meta 清空不回退)= path helper `routing_state_path_in`/`next_sid_path_in` = `crates/ccteam-im/src/lib.rs`;`RoutingState`、`load_state`(sync 复原路由 + 把 `live_sids` 暂存 `restore_pending`)、`persist_routing`/`persist_next_sid`(9 旧 `persist_state` 点按职责拆)、`enable_persistence(ccteam_root)`、`rebuild_session_from_meta`(单一重建核 = `plan_session_rebuild`(锁内、同步)+ `spawn_for_plan`(锁外、慢)+ `apply_rebuilt_session`(锁内);`/use`·import·重启复用)、`resume_restored_sessions{,_shared}`(重启 cold-start 重 spawn;`_shared` 锁外 spawn 不阻塞 web `POST`)、`/role` 同步改写 meta、`chat_owner_visible` 改 identity-string 比(抗 `from_identity` round-trip)= `crates/ccteam-im/src/gateway.rs`;out-of-process 读 `tracked_chat_sessions`/`_names`(routing `live_sids` ⋈ `meta.json`,projects 从 config 枚举)+ 调用点 `status.rs`/`commands.rs`/`main.rs`。**breaking 无迁移**:重启所有 live session cold-start(stream-json 子进程随 daemon 死、terminal 丢窗格),`--resume` 恢复对话;secret 重 mint(不持久) | `cargo test -p ccteam-im --lib`(`gateway_persistence_restores_routes_and_sessions`/`sid_stable_*`/`next_sid_monotonic_survives_routing_and_meta_wipe`/`restored_session_resume_does_not_hold_gateway_lock_while_adapter_waits`/`hitl_mode_persists_across_reload`);`cargo test -p ccteam-cli --test status_view_test`;`cargo test -p ccteam-web --test status_test` |
| 标准 API:config/im | `crates/ccteam-web/src/routes/im_config.rs`(详上方「web config/im」行;`AppState.im_poll` 背景轮询态)| `cargo test -p ccteam-web --test im_config_test` |
| 标准 API:capabilities | `crates/ccteam-web/src/routes/capabilities.rs`(`HarnessCapability` + `probe_available` PATH probe) | — |
| gateway spine(标准 API drive) | `crates/ccteam-im/src/gateway.rs`(`SessionView` + `session_views` / `create_session_api` / `submit_to_sid` / `stop_session`;`GatewayEvent.sid` = SSE 过滤键) | `cargo test -p ccteam-im gateway` |
| tracked session reader(`session ls` / `status` 共用)| `crates/ccteam-im/src/gateway.rs`(`tracked_chat_sessions(state_path) → Vec<TrackedSessionRow{slug,sid,role,vendor,...}>`,读持久化 gateway state 文件,无新 RPC、不碰 cto-gate) | `cargo test -p ccteam-im tracked_chat_sessions` |
| `ccteam status` / `session ls`(F3/B4)| `status` = `crates/ccteam-cli/src/main.rs`(`run_status` 嵌套列 project+sessions + `first_lan_ipv4`/`is_lan_ipv4` getifaddrs FFI,删 `--tail`);`session ls` = `commands.rs`(`run_sessions` + `render_sessions_table`,列 SLUG/SID/ROLE/VENDOR/STATUS;tracked→live、orphan=live-pane∧¬tracked、daemon-down 降级)| `cargo test -p ccteam-cli --test status_view_test`;`cargo test -p ccteam-cli render_sessions_table` |
| web config/im(REST,非 MCP)| `crates/ccteam-web/src/routes/im_config.rs`(GET masked `handle_get_im_config` 响应类型不含 secret + `mask_last4` · PUT telegram/lark 先验后落盘 0600 · telegram chat-id 背景 long-poll task + GET 轮询 + `AppState.im_poll`;全写返 `restart_required`);onboarding 拆出 = `crates/ccteam-im/src/onboarding.rs`(`telegram_validate_token_with_base` + `telegram_poll_chat_id_with_base`);test seam = `AppState::with_creds_path` + `CCTEAM_{TELEGRAM,LARK}_API_BASE` | `cargo test -p ccteam-web --test im_config_test`;`cargo test -p ccteam-web --test openapi_test` |
| web 统一 chat-shell(插件市场 / Status / Settings)| shell + 顶栏 cost pill + 底部导航 = `web/src/App.tsx` + `components/CostPill.tsx`;插件市场 = `web/src/pages/MarketplaceView.tsx` + `lib/marketplaceFormat.ts`(浏览/预览/install,取代旧 Roles 只读页);Status = `web/src/pages/StatusView.tsx` + `lib/statusApi.ts`(读 `GET /api/v1/status`);Settings = `web/src/pages/SettingsPage.tsx` + `lib/configApi.ts`(IM config,token 永 password 不预填 + 内联确认 + chat_id 可取消轮询)| vitest(`MarketplaceView.test` / `StatusView.test` / `CostPill.test` / `configApi.test` / `SettingsPage.test`)|
| host 轴 + 多机 join/远程 spawn(v0.8.18 + v0.8.24 Track D;v0.9.0 反向连接)| **hosts API** = `crates/ccteam-web/src/routes/hosts.rs`(`GET /hosts` 含 local+satellites online/offline · `/{host}` · `POST …/register-mcp` · **`POST /hosts/join-token` mint · `POST /hosts/join`** · WS `GET /hosts/channel` + `GET /hosts/exec/{nonce}`(HTTP heartbeat 已删,report 走控制通道))+ registry = `ccteam_core::host_registry` + remote gate = `ccteam_im::remote_host` + gateway `create_session_api_on_host` · CLI `ccteam host *`。`probe_bin`/`PROBE_SPECS` 仍测 local agent。**status per-session 成本** = `routes/status.rs`(`StatusResponse.sessions: Vec<SessionCostRow>`,读 `chat_turn_completed` usage × `ccteam_cost::estimate_cost`)。**own-only ACL** = `ccteam-im/src/gateway.rs`(`chat_can_access = session.owner==*chat`;`/use`/`/stop`/`/screen`/`render_sessions` 全过它)+ `ProjectState.owner`(`core/src/state.rs`;`ChatKey::identity()`→`channel:chat_id`,`create_project` 时记)。**SPA** = `web/src/pages/HostsView.tsx` + `lib/hostsApi.ts`(主机页)· `StatusView.tsx` 成本列 · `components/AvatarMenu.tsx`(头像个人设置)+ `lib/i18n.ts` + `hooks/useWebSettings.ts`(界面语言中/英,默认中)| `cargo test -p ccteam-web --test openapi_test`;`cargo test -p ccteam-core mcp_register`;`cargo test -p ccteam-im own_only`;vitest(`hostsApi`/`HostsView`/`StatusView`/`AvatarMenu`/`i18n`)|
| Web chat WS (`ccteam-chat.v1`) | `crates/ccteam-web/src/chat_protocol.rs` + `routes/chat_ws.rs`;CLI bridge = `crates/ccteam-cli/src/web_chat_bridge.rs` | `cargo test -p ccteam-web chat_frame` |
| PTY WS (`ccteam-pty.v1`) | `crates/ccteam-web/src/routes/pty_ws.rs` + SPA `useTerminal` | `cargo test -p ccteam-web --test pty_ws_test`(env-gated) |
| byte-faithful rmux 终端(裸字节流)| `crates/ccteam-harness/src/rmux_backend.rs`(`subscribe` 用 `output_stream()`/`PaneOutputChunk::Bytes`、`capture` 排 `Oldest` 字节 backlog;rmux-sdk **0.5**,byte API 自 0.3.1 起就有);snapshot-on-connect = `ccteam-web/src/pty.rs`;ANSI snapshot = `routes/pane_snapshot.rs` | env-gated 真机 smoke(`ws_*` / `pane_snapshot`,沙箱不流)|
| IM transport / 凭证 | `crates/ccteam-im/src/transport/`(`Channel` trait + providers)+ `im/credentials.json` 解析 | — |
| IM 出站分片 | `Channel::max_message_len` + `sanitize::split_for_channel`(UTF-16 预算 / fence 平衡)+ `daemon::send_gateway_outbound` | `cargo test -p ccteam-im split` |
| IM 进度 status | `progress::ProgressFold` + `Channel::edit_message` + `gateway::spawn_event_pump` + `GatewayEventKind::Progress` | `cargo test -p ccteam-im --test im_progress_test` |
| IM 附件 I/O | 入站:`transport::{ChannelAttachment,AttachmentKind}` + telegram `getFile` / lark `im/v1/messages/{id}/resources/{key}`(staging+sanitize helper 共享在 `transport/mod.rs`);出站:`transport::{OutboundFile,OutboundFileKind}` + `chat_send_file` 走 `mcp.sock`,telegram `sendPhoto/sendDocument` / lark `im/v1/{images,files}` 上传后发 image/file 消息 | `cargo test -p ccteam-im` |
| HarnessAdapter / ProcessBackend / AgentVendor | `crates/ccteam-harness/src/adapter.rs`(trait + `AgentVendor` enum + `ExecutionMode`)+ `lib.rs` | `cargo test -p ccteam-harness adapter` |
| Claude spawn argv(role-as-attribute + roleless)+ pane/turns 按 sid | `crates/ccteam-harness/src/execution/claude_tui.rs`(`spec_for_new` / `spec_for_resume` / `spec_for_fresh` —— **非空 role 才 push `--agent`**,空=roleless;`chat_session_name(slug, sid)` → pane / `--name`;`chat_spawn_env_owned(role,slug,secret,sid)` 注入 `CCTEAM_CHAT_SID`/`CCTEAM_CHAT_SECRET`;`parse_chat_session_name → Option<(slug, sid)>`;`/role` death-probe) | `cargo test -p ccteam-harness --test claude_tui_resume_test`;`cargo test -p ccteam-harness --test claude_tui_env_test`(env-gated) |
| 命令面中立词汇 | `Directive` / `DirectiveOutcome`(5 值)/ `ChoicePrompt` / `ChoiceOption` / `ChoiceSelection` / `ContextUsage` / `ThreadStatus` + `handle_directive` / `thread_status`(无 default impl)= `crates/ccteam-harness/src/adapter.rs` | `cargo test -p ccteam-harness adapter` |
| gateway 纯路由 + 命令菜单 + `/role` + session 模型(sid/去 dedup/roleless)| slash→`Directive`→`handle_directive`→渲染 outcome + `GATEWAY_COMMANDS` 单表 + `resolve_selection`(token-global)+ `switch_current_role`(`/role` 原地换、保持同一 sid+pane)+ `render_sessions`;session 持久态 = `SessionResolve{sid,role,vendor,project,project_dir}` + `session_resolve`/`session_sid_for`/`reply_target_for`(全按 sid)+ `ensure_role_exists`(空 role 短路);**去 `(project,role)` dedup**(同 role 多 session)= `crates/ccteam-im/src/gateway.rs` | `cargo test -p ccteam-im gateway`;`cargo test -p ccteam-im session_sid_for` |
| pending interaction 注册表 | `PendingInteractions`(自己的锁 / 双 origin / 单飞 per(chat,session) / TTL / `take_by_token`)= `crates/ccteam-im/src/pending.rs` | `cargo test -p ccteam-im pending` |
| transport 选项/回填/菜单 | `MessageOption` / `ChoiceReply` / `SendMessage.options` / `ChannelMessage.selection` / `Channel::register_commands`(全 `#[serde(default)]`)= `crates/ccteam-im/src/transport/mod.rs`;TG inline keyboard + `callback_query` = `transport/providers/telegram.rs` | `cargo test -p ccteam-im` |
| Claude 命令面 gate | 四通道 `handle_directive`(prompt 透传 / BRIDGE_SAFE local / arg-popup `NeedsChoice` / panel-only `Rejected`)+ `/esc`→Escape + `ensure_chat_hooks_installed` 追加 `AskUserQuestion` matcher = `crates/ccteam-harness/src/execution/claude_tui.rs` | `cargo test -p ccteam-harness --test claude_tui_test` |
| **Claude stream-json adapter(v0.8.11 协议轴)** | `crates/ccteam-harness/src/execution/claude_stream_json/`:`spawn_spec.rs`(`build_argv` —— 不带 `-p`、`--input-format/--output-format stream-json`、`--session-id`\|`--resume` 互斥、`--agent` 仅 persona、**禁 `--append-system-prompt`**;`deterministic_session_uuid(slug,sid)` 无状态 resume key)· `transport.rs`(`StreamJsonTransport::{connect_stdio, spawn_from_io<R,W>, send_line, subscribe, request_control, wait_for_init, wait_closed, shutdown}` —— 消费端不持 `Child`)· `protocol.rs`(NDJSON wire 类型 + `user_text_line` / `can_use_tool_response_line`)· `translate.rs`(`StreamTranslator::{ingest, on_close}` → `ThreadEvent`;in-flight 关闭→人话 `TurnFailed`)· `bridge.rs`(`classify_slash` 三类 + `CanUseToolResolver`/`FnResolver`/`ApprovalDecision`)· `mod.rs`(`ClaudeStreamJsonAdapter` + `SessionIdentity{sid,vendor_uuid,host}` + HITL dispatcher) | `cargo test -p ccteam-harness --test claude_stream_json_test`(fake-vendor e2e:spawn→init→turn→slash→HITL→idle→resume→故障矩阵) |
| **共享 ACP core + OpenCode / Grok**(v0.8.24)| `execution/acp/`(transport + protocol + translate,`InboundPolicy::AutoAllowPermission`)· `execution/opencode_acp/`(`OpencodeAcpAdapter`,`opencode acp`,resume 优先 `session/resume`)· `execution/grok_acp/` re-export 共享 core· `AgentVendor::Opencode` + `OPENCODE_BIN_ENV`· `UnifiedTokenUsage.reported_cost_usd` + `resolve_turn_cost`(`Vendor::Opencode` 无价表)| `cargo test -p ccteam-harness --test opencode_acp_test`;`cargo test -p ccteam-harness --test grok_acp_test` |
| **curated MCP + evolution**(v0.8.24 C)| curated MCP = `execution/mcp_config.rs` + stream-json spawn 写 `chat/<sid>/mcp.json`· evolution = `routes/evolution.rs`(读 `experience.jsonl`)| `cargo test -p ccteam-web --test hosts_multihost_test`(hosts)+ openapi |
| **protocol 轴 + 创建面 + 工厂三路由** | `SessionProtocol{StreamJson(默认), Terminal, Acp}` = `crates/ccteam-harness/src/adapter.rs`;`default_adapter_factory(vendor, protocol)` vendor-first(Claude 按 protocol·Grok/OpenCode/Kimi→ACP·Codex→app-server)= `crates/ccteam-im/src/daemon.rs`;gateway `start_session`/`create_session_api_on_host` 带 `protocol`+`host`、`GatewaySession`/`SessionView` 持久化走 `meta.json`、`/new … terminal` 解析、`/screen` 对 stream-json 人话拒、pump 写 `chat_turn_completed`= `gateway.rs`;web `CreateSessionForm` = `sessions_api.rs`;SPA 4-way runtime + Workflow = `web/src/pages/{ChatConsole,WorkflowView}.tsx` | `cargo test -p ccteam-im daemon`;`cargo test -p ccteam-web` |
| Codex 命令面 + transport | 六类映射 `handle_directive` + 三层 resolution + 两段式 + `CodexThreadTracker`(消费 `thread/tokenUsage/updated`)+ `resolve_codex_transport()`(socket→UDS / 默认 stdio)= `crates/ccteam-harness/src/execution/codex_app_server.rs`;per-vendor 单例工厂 `default_adapter_factory` = `crates/ccteam-im/src/daemon.rs` | `cargo test -p ccteam-harness --test codex_app_server_test` |
| D6 反问 ingress | `intercept_ask` chat 变体(`AskUserQuestion`→`ChoicePrompt`→`mcp.sock`→`updatedInput.answers`)= `crates/ccteam-hooks/src/intercept_ask.rs`;`mcp.sock` 的 `interaction/ask` op = `crates/ccteam-cli/src/main.rs` | `cargo test -p ccteam-hooks intercept_ask` |
| `/sessions` 状态:model + 上下文 | Claude 倒读 transcript 尾(`read_status_tail`,`[1m]`→1M / 否则 200k 基线)= `crates/ccteam-harness/src/execution/transcript_tail.rs`;Codex 读 `CodexThreadTracker`;gateway 单点渲染 `188k / 1M (19%)` = `ContextUsage::render` / `ThreadStatus::status_suffix`(`adapter.rs`) | `cargo test -p ccteam-harness adapter` |
| workflow.yaml schema(推后) | `ccteam-flow` / `ccteam-core` 解析代码(推后的编排层,§7) | — |
| **(v0.9.0 W1)AgentPrincipal 调度门泛化 + 四 vendor 身份注入** | principal 认证 = `ccteam-im/src/gateway.rs`(`verify_session_principal(sid,secret)→CallerCtx{sid,slug,role}`,去 cto-only)· 门 = `mcp/dispatch.rs`(`execute_session_tool` 按 principal + `_caller_slug` 服务端覆写)· HTTP sid-bearer = `ccteam-web/src/routes/mcp.rs`(注入四件套;**`/mcp` 自鉴权、挂在 `auth_layer` 之外**——外层只认 web-token 家族,曾把 session bearer 先行 401 → 托管会话退化 admin、A2A spawn 丢父边,v0.9.2 修;spawn 响应 `caller` 标 `admin`/`ambient:<sid>`)· MCP 注入 = `execution/mcp_config.rs`(`acp_mcp_servers_http` 共享 opencode+grok · `codex_thread_mcp_config{,_at}` = `thread/start|resume.config.mcp_servers` HTTP per-thread)· Codex 全局 HTTP writer/旧 stdio 识别 = `ccteam-core/src/mcp_register.rs` + `ccteam-cli/src/mcp_serve.rs`· codex `vendor_uuid` 落盘 + resume-first = `execution/codex_app_server.rs`· `session_spawn` 扩参 = `mcp/protocol.rs` | `cargo test -p ccteam-web --test mcp_session_bearer_test`(含 fake 组合 + ignored 真机 Codex 0.144.x);`-p ccteam-harness --test codex_app_server_test`/`opencode_acp_test`/`grok_acp_test` |
| **(v0.9.3)四 vendor 全局 MCP 对称注册**(任何 vendor 主会话可当 orchestrator) | writer = `ccteam-core/src/mcp_register.rs`(`install_{grok,opencode}_mcp_into` + `resolve_*_config_path` + `*_registered`;Grok=`~/.grok/config.toml` `[mcp_servers.ccteam]{url,enabled,headers}`、OpenCode=`~/.config/opencode/opencode.json` 运行时 v1 形状 `mcp.ccteam{type:remote,url,headers,enabled}`,均 0600)· 装载 = `ccteam-cli/src/mcp_serve.rs`(`install_{grok,opencode}_mcp`)+ `commands.rs`(config mcp 四 vendor 报告)+ `ccteam-web/src/routes/hosts.rs`(register-mcp)。托管会话不受全局条目遮蔽:Claude strict / Codex deep-merge / OpenCode `MCP.add` 按名覆盖 / **Grok = spawn 注入 child-only env `GROK_CLAUDE_MCPS_ENABLED=false` 关 Claude-compat 扫描**(`grok_acp/spawn_spec.rs::build_envs`;2026-07-22 现场证伪早期「同名去重注入胜」探针——Grok `[compat.claude] mcps` 默认开、import `~/.claude.json` 的 stdio 条目,每 session 泄漏一个孤儿 `mcp-serve` 子进程 + 同名双注册且谁胜依 grok 版本,tools 可能落 admin/stdio 而非 session principal;从源头禁扫,Claude 全局 stdio 注册保留不动) | `cargo test -p ccteam-core --lib mcp_register`;`-p ccteam-harness --test grok_acp_test`(`spawn_disables_claude_mcp_compat_scan`);真机 `grok mcp doctor`(7 tools) |
| **(v0.9.8)外部 Agent MCP Phase 1(tenant WebUser principal)+ 异步可见性** | `/mcp` 三 principal 鉴权(admin/tenant web token 族 + session bearer)= `ccteam-web/src/routes/mcp.rs::require_mcp_auth`(经 `auth::resolve_identity`)· `McpCaller::User{user_id}` + 全工具 project ACL(现 7 工具) + 防枚举 = `ccteam-im/src/mcp/dispatch.rs`(`authorize_user_session_tool`/`user_can_see_project`/`strip_caller_args`;spawn owner 归 `user:<id>`)· 身份策略单源 = `ccteam-core/src/identity.rs`(web `Identity` 委托)· web SSE 重连权威回填 + visibility 复活 = `ccteam-web/web/src/hooks/useSessionEvents.ts` + `pages/SessionView.tsx` · root 会话异步收尾 IM 镜像(turn 起源标记 `TurnOrigin`,子会话不镜像)+ `/status` 子会话外显 + `/sessions` activity/按钮互补 = `ccteam-im/src/gateway.rs` | `cargo test -p ccteam-web --test mcp_tenant_bearer_test`;`-p ccteam-im session_tool_tests`(user_* 族)+ mirror/status 定向;vitest `useSessionEvents`/`SessionView` |
| **(v0.9.5)Kimi 第五 vendor(共享 ACP 薄壳)** | `execution/kimi_acp/`(`KimiAcpAdapter`,`kimi acp` 长驻 stdio;resume 阶梯 `session/resume`→`session/load`→`session/new`;skip=`AutoAllowPermission`、hitl=`DefaultDecline` fail-closed;remote=NotImplemented)· spawn = `kimi_acp/spawn_spec.rs`(`CCTEAM_KIMI_BIN`,argv 仅 `kimi acp`,零 model/persona/permission 旗,模型 post-handshake `session/set_model`,目录走 configOptions 同 opencode)· per-session MCP 复用 `acp_mcp_servers_http` · 全局注册 = `ccteam-core/src/mcp_register.rs`(`install_kimi_mcp_into` → `$KIMI_CODE_HOME/mcp.json` `mcpServers.ccteam{url,headers}`,**headers 为 map 非 ACP 数组**,0600 幂等)· probe = `hosts.rs` `PROBE_SPECS`(`default_bin:"kimi"`) | `cargo test -p ccteam-harness --test kimi_acp_test`(fake = `tests/fixtures/kimi_acp/fake_kimi_acp.py`);真机 smoke 留痕 `docs-local/versions/v0-9-5/` |
| **(v0.9.0 W2)委派语义 + 可靠性合同 + 引擎中立化** | 委派 meta 字段(`parent_sid`/`spawned_by_role`/`delegation_depth`)= `execution/session_meta.rs`· 落盘 watch = `execution/delegation.rs`(`DelegationWatch` armed/scan/reconcile,atomic-durable)· 通知/护栏/预算/reconcile/idem = `ccteam-im/src/{gateway.rs,delegation.rs}`(`create_delegated_session`/`run_delegation_notifier`/`reconcile_delegations`/`emit_delegation_progress` append→notify 顺序 · `IdemCache` · `fleet_cost_24h`/`budget_exceeded`;**v0.9.5 通知时机 = vendor turn 边界**:pump 聚合中途叙述、`TurnCompleted/TurnFailed/Error` 才发 boundary signal,`NotifyMode final/all/off` = `harness/execution/delegation.rs`,wait 等 `session_turn_in_flight` 清零,session_* 工具超时 = `mcp/dispatch.rs::execute_session_tool`)· dispatch wait/idem = `mcp/dispatch.rs`· 7 `delegation_*` = `execution/progress_bridge.rs`· `/status` delegations = `ccteam-web/src/routes/status.rs`· **引擎中立化**:删 `templates/cto_role.md`/`CTO_ROLE_MD`/`DEFAULT_AGENT_SCAFFOLDS`,`bootstrap_project_at_dir` 不种 role,roleless 默认(`chatDefaults.ts`/gateway `/new`)| `cargo test -p ccteam-im --lib`(delegation/notifier/reconcile/idem/guardrail 混沌);`-p ccteam-web --test status_test` |
| **(v0.9.0 W3 + 反向连接)跨机执行(host 轴)** | 反向传输:hub(通道注册 + exec rendezvous)= `execution/host_channel.rs`(`HostChannelHub`/`ExecBridge`/keepalive 常量)· `ccteam-exec.v1` wire + daemon 侧 `connect`(hub→`(AsyncRead,AsyncWrite)`)= `execution/remote_exec.rs` · 卫星 exec 引擎(协议盲字节泵:vendor 允许名单/relpath 限 `.ccteam/chat/<sid>/`/env 白名单 `CCTEAM_*`/`{{DAEMON_URL}}` 替换)= `execution/satellite_exec.rs` · 卫星客户端(出站控制通道 + 拨回 + 退避重连,`ccteam start` 内嵌)= `ccteam-web/src/satellite.rs` · daemon 侧 WS(`/api/v1/hosts/channel` + `/hosts/exec/{nonce}` + report 落 registry)= `ccteam-web/src/routes/hosts.rs` · registry `projects` + 0600 + slug gate = `ccteam-core/src/host_registry.rs`(`HostReport`/`apply_report`;HTTP heartbeat 已删)· 主侧 = `remote_host.rs`(`prepare_host_for_spawn`→`HostTarget{remote}` + `regate_remote_host` + `HubRemoteHostProxy`)+ `SpawnCtx.remote`(`adapter.rs`)+ claude 远程 spawn `execution/claude_stream_json/mod.rs`(相对 `--mcp-config` + files)· rebuild 三路径 re-gate = `gateway.rs`(offline 绝不本地重生);codex/opencode/grok/kimi 远程 = 显式 `NotImplemented` | `cargo test -p ccteam-harness --test claude_stream_json_remote_test --test satellite_exec_e2e`;`-p ccteam-web --test satellite_ws_test`(真 socket 反向 e2e)`--test hosts_multihost_test`;`-p ccteam-im`(`remote_session_never_respawns_locally_when_host_offline`) |
| **(v0.9.0 W4)团队可视化 + 全局 SSE** | `GatewayEventKind::Delegation` + `GatewayEvent.slug` = `ccteam-im/src/gateway.rs`(`emit_delegation_progress` 同点广播)· REST + 全局 SSE = `ccteam-web/src/routes/agents.rs`(`GET /api/v1/agents/{graph,events}`,tenant ACL `can_see_project` 过滤,无 slug 帧 tenant fail-closed)+ `ring.rs`(`GlobalEventRing` 256 帧 Last-Event-ID)· 删 legacy `routes/{sse,harness_sse}.rs` + `watcher.rs`(SPA 零消费)· SPA = `web/src/pages/AgentsView.tsx` + `lib/{agentsApi,agentsTree,agentsReducer}.ts` + `hooks/useAgentsEvents.ts`(v0.9.2 起 = 可折叠委派树,泳道 SVG/`agentsLayout` 已删;admin beta-gate)| `cargo test -p ccteam-web --test agents_test --test openapi_test`;vitest(`AgentsView`/`agentsTree`/`agentsReducer`);Playwright `v090-agents.spec.ts` |
| **(v0.9.2)项目↔主机绑定 + 容量挤停 + A2A 限幅 + 团队树** | project 绑定:`ProjectEntry{host,remote_slug,remote_path}` + `pick_unused_project_slug` + data-home = `ccteam-core/src/{config.rs,projects.rs}` · `project_init`/`project_init_result` op 对(nonce oneshot,15s)= `execution/host_channel.rs` + `ccteam-web/src/{routes/hosts.rs,satellite.rs}` · 远程建项/import/`host`+`host_online`/`cataloged` 反查 = `ccteam-web/src/routes/{projects.rs,hosts.rs}` + `views.rs` · spawn 继承绑定 + `HOST_SPAWN_PARAM_REMOVED` + `wire_slug` 走线 + rebuild `ensure_session_host_binding` = `ccteam-im/src/{gateway.rs,remote_host.rs,mcp/dispatch.rs}` · **容量**:`SessionsConfig.max_live`(50)= `ccteam-core/src/config.rs`,`ensure_live_capacity`/`select_live_capacity_eviction` + `session_evicted` progress + `SessionLifecycle` 广播 = `ccteam-im/src/gateway.rs` + `execution/progress_bridge.rs`(护栏默认 children 10 / delegated 50)· **A2A 限幅**:共享截断器 + collect `max_chars`(10k)/wait 10k/通知 4k = `ccteam-im/src/{delegation.rs,mcp/dispatch.rs,mcp/protocol.rs}` · SPA:新建项目选主机/项目 host 徽章/import = `web/src/lib/hostFilter.ts` + `pages/{HomeView,HostsView}.tsx`,vendor chips = `components/VendorChip.tsx` | `cargo test -p ccteam-{core,im} --lib`;`-p ccteam-web --lib` + `--test hosts_multihost_test --test tenant_acl_test`;vitest(`hostFilter`/`Sidebar`/`AgentsView`);Playwright |
| **status vendor 面板 + 共享 probe**(事实层)| probe 下沉(`AgentProbeSpec`/`probe_bin`/`AvailabilitySnapshot`,web/im/卫星 report 三方共用一份 spec)= `crates/ccteam-core` · MCP `status` daemon-aware dispatch(按 caller project 的 host 绑定出面板:installed / auth 诚实枚举(ready/not_ready/unknown + last_session_ok,PATH 成功不冒充已登录)/ budget(ok/disabled/unpriced/…)/ host_online + observed_at/stale;卫星行来自 25s report,离线给最后快照标 stale、绝不本机顶替;不改变状态、不产生 turn/sid/cost,工具仍 8 个)= `crates/ccteam-im/src/mcp/` | `ccteam doctor --verify-mcp`(8/0);`cargo test -p ccteam-im`(status/probe) |
| **目录三源 + routing notes 透传 + spawn 失败发现面**(目录/观点层 + spawn 面)| catalog 三源并列不抹平(runtime last-seen ⋈ hub `models.json` ⋈ 用户别名注释,各标来源;advisory,**永不当 spawn allowlist**,stale/缺失不阻断任何 spawn)= last-seen 缓存 `crates/ccteam-harness/src/model_catalog.rs`(`~/.ccteam/model-catalog.json`,adapter 握手 best-effort 落盘)+ `crates/ccteam-im/src/{mcp,hub.rs}` · routing notes(`<project>/.ccteam/routing.md` > `~/.ccteam/routing.md`,二者替换而非合并;global 由 `ensure_ccteam_home` 缺失时以 `create_new` 生成中立模板,绝不覆盖;dumb markdown,限幅 + source/sha/truncated 包裹后随 `status` 原文透传,只搬运不解释)= `crates/ccteam-core/src/paths.rs` + `crates/ccteam-im/src/mcp/vendor_panel.rs` · spawn 失败发现面(vendor 未装 → mint sid 前快速失败,错误附同 host 已装 vendor 集 + freshness;host offline 报 offline 绝不本地 fallback;auth unknown / model 不在 catalog 不提前阻断,vendor 原始错误保留)= `crates/ccteam-im/src/{gateway.rs,mcp/dispatch.rs}` | `cargo test -p ccteam-core routing_notes`;`cargo test -p ccteam-im routing_notes` |
| **(v0.9.7 W1/W2)daemon 生命周期核 + CLI + systemd 退场**(pid-detach 自管,借鉴 codex `app-server-daemon`,见 `LICENSES.md`)| 生命周期核 = `crates/ccteam-core/src/daemon.rs`(`PidRecord`{pid+process_start_time+version+started_at} JSON + PID 复用守卫 · `acquire_operation_lock`(`daemon.lock`)· 双判定 `probe_daemon`(MCP `initialize`→`serverInfo.version`)× `process_matches_record` · `start_managed`(setsid detach)· `stop_managed_with`(SIGTERM+40s,`--force` 才 SIGKILL 且仅 daemon 自身)· `daemon_status`;裸-pid `write_pidfile`/`pid_alive`/`check_daemon_health` 旧 API 已废)· CLI = `crates/ccteam-cli/src/daemon_cli.rs`(`ccteam daemon start/stop/restart/status/logs` 全 `--json`,单行契约 `started/alreadyRunning/stopped/notRunning/restarted/error`;`restart_managed` 单锁复用)· legacy 接管 = `crates/ccteam-cli/src/legacy_takeover.rs`(指纹白名单只认历代安装器 unit,手写永不代删;注入式 paths+runner)· 前台 `ccteam start` 不写 pidfile + probe 实例守卫 + trigger-file 全链退役 = `main.rs`· install.sh/Makefile 零 unit 生成(检测+调用 only)· doctor `check_legacy_service` | `cargo test -p ccteam-core daemon::`(17);`cargo test -p ccteam-cli --bins`(daemon_cli/legacy_takeover);`--test graceful_shutdown_test`(前台不写 pidfile) |
| **(v0.9.7 W3)InstallChannel + `ccteam update` + 版本检查 + fleet 偏差**(借鉴 codex `install-context`/`update_action`,见 `LICENSES.md`)| channel 检测 = `crates/ccteam-core/src/install_channel.rs`(`InstallChannel{Npm,Bun,Pnpm,Standalone,Source,Other}` + `detect`/`detect_with`(env `CCTEAM_MANAGED_BY_*`→marker `~/.ccteam/install-channel`→路径启发)+ `InstallMarker` 读写 + `suggested_update_command`;install.sh 装成功写 marker)· 版本检查 = `crates/ccteam-core/src/version_check.rs`(`state/version.json`{latest,last_checked,dismissed} + `maybe_refresh_latest`(≥20h gate,注入 fetch,静默降级)+ `update_available`(`normalize` 容 `v` 前缀 semver))· `check_for_update` = `preferences.rs`(default true)· `ccteam update` = `crates/ccteam-cli/src/update.rs`(channel→动作:standalone 重放 install.sh(`CCTEAM_POST_INSTALL=none`,无第二下载器)/ source 打印指引 / npm stub / other 报错;升级重启合同 `run_restart_contract`:probe→drain(5min/`--now`)→`restart_managed`→版本核对;`--no-restart`;`fetch_latest_version` = curl `-sI` redirect 解析)· doctor `check_updates` + `run_status` fleet 偏差 = `update::fleet_version_skew`(卫星 `ccteam_version`≠daemon → WARN,registry 空则静默)· 版本 bump = `Cargo.toml` `workspace.package.version` | `cargo test -p ccteam-core`(install_channel 6 + version_check 8 + preferences 2);`cargo test -p ccteam-cli --bins`(update/daemon_cli);真机 A7 = orchestrator live e2e |

改协议 = 改代码 +(若新增一类协议)补本表一行。**不**再维护独立的 interfaces.md。
