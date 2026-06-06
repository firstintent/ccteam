# ccteam 技术实现方案

> 本文档基于 [requirements.md](./requirements.md)(已确认的用户痛点)给出 ccteam 当前的技术架构、组件分解、数据协议、扩展点映射。
>
> **产品定位**:「云 Claude Code(+ Codex)+ IM + Web」—— 把一个常驻 gateway daemon 架在你机器上的真实 agentic CLI 之上,让你在 IM(Telegram 等)或 web 控制台里像用一台终端一样,跨多个项目、多个 session、多个 **role** 操作真实 agent。

---

## 0. 架构红线

**权威红线清单 = [CLAUDE.md §三](../CLAUDE.md)**(always-loaded 的开发宪法:红线表 + vendor / README / skill 红线;任何 PR 不得违反)。本文档**不重复**该清单 —— 只在下面各组件章节就地给红线的**架构论证**(为什么这么定),并用下表的 R-code 简写引用 §三 的条目。

**R-code 速查**(简写 ↔ CLAUDE.md §三):

- `R1` 文件系统是状态面(非 chat 命令面)· `R2` `progress.jsonl` 是 state SoT(+ `turns.jsonl` 对话原文)· `R3` No prompt injection(`--agent` 让 vendor **自读** role.md = 这条的**兑现**,不是违反)· `R4` 会话 = resume-by-id · `R5` 永不**主动** kill 长 session(例外:`project stop` / `rm --force` 是用户**显式**命令)· `R6` 不解析终端输出(不 scrape pane)· `R7` `ccteam-core` 零 team 名字面量 · `R8` 跨项目记忆走 vendor 原生接口 · `R9` crate 拓扑 `core → harness → cost` · `R10` 新建项目走 `<projects_root>/<team>-<slug>/`(slug 撞名数字累加)· `R11` root README.md 英文且不含版本进展

> **已退役的旧红线**(v0.8.6 架构打破,勿再引用):「每次 spawn = fresh 1M context」(chat 复用 context 是 feature;仅 autonomous bg 适用)、「fix-loop 撞 3 次 escalate / AgentPath depth」(属推后的 `ccteam-flow` 引擎)、「HITL approval state SoT / 第 4 mode」(**编排级**批准仍推后,`ApprovalIR` 仅类型占位;**per-session** HITL 已 v0.8.7 落地,走 `PermissionMode::Hitl` + `PermissionRequest` hook,见 §6.5;非 hitl session 仍 `--dangerously-skip-permissions`)、「`ccteam init` 落 AGENTS.md → CLAUDE.md symlink」(v0.8.6 起 ccteam **不**生成/接管项目 CLAUDE.md/AGENTS.md)。

---

## 1. 设计原则

每条原则都直接挂钩 `requirements.md` 中的某条痛点。

| 原则 | 对应痛点 | 落地约束 |
|---|---|---|
| **守护进程化** | 痛点 9:AI 团队需要人来主持 | gateway daemon(`ccteam start`)独立于任何 Claude Code 主对话,systemd / 后台长跑;SIGTERM / `ccteam stop` → daemon 优雅 drain(web / IM gateway / MCP socket,每任务 ≤ 5s)。**不 tick、无 orchestrator 循环**。 |
| **文件即状态机** | 痛点 7:进度永远不透明 | 一切状态可从文件系统恢复:`progress.jsonl` 业务事件 SoT + chat 原文 `turns.jsonl` + Claude session jsonl + Codex thread;daemon 重启按持久化 session id 续接,不丢上下文 |
| **session = role** | 痛点 12 + 痛点 13 | 一个 chat/IM session 以某个 **role** 启动(`claude --agent <role> --name|--resume <sid>`),session **即 become** 该 role;role 库住项目级 `.claude/agents/<role>.md`(Claude Code first-class spec)。ccteam **不注入** system prompt —— 只决定「消息路由到哪个 session」+「以哪个 role 启动」(详 §2) |
| **vendor 原生项目知识** | 痛点 10 + 痛点 13 | 项目知识层归 vendor:Claude session 自动读项目 `CLAUDE.md`,Codex 自动读 `AGENTS.md`。ccteam **不生成、不修改、不抑制**这些文件(R8);ccteam 的自定义只在 **role** 层 |
| **resume-by-id 会话** | 痛点 8/9 | session 按需 spawn(首条消息触发)+ 按 session id resume + 空闲释放;不常驻吊着(state 落盘,重启后 reconnect),避免影子 SoT(R4) |
| **零交互沙盒** | 痛点 8:每一步都点允许 | IM 路径的 Claude session 走 `--dangerously-skip-permissions`;只把 bot 暴露给可信 chat(`allowed_chat_ids` allowlist) |
| **失败必须可见** | 痛点 7 + 痛点 11 | gateway 的原则是失败不能静默挂住:submit/turn 超时、Claude tmux pane 死、Codex app-server 断,都翻成 `gateway error: ...` 回到 IM / web |
| **预算硬上限** | 痛点 5 + 自激励 loop 防失控 | 成本 per-vendor 记账(`ccteam-cost`);`budgets.{claude,codex}.max_cost_usd_per_24h` per-vendor cap;CLAUDE.md $200 全 ccteam 物理上限兜底 |
| **headless 状态引擎** | 通知层不能改状态 | ccteam 核心是 headless 引擎,IM / web / 标准 API 都是可插拔前端,共用同一 gateway dispatch;任何前端**不得**引入新 LLM 层 —— LLM 推理只发生在 agent session(Claude/Codex)内部 |

---

## 2. 总体架构

### 2.1 核心模型:chat ⇄ project ⇄ session ⇄ role

ccteam gateway 把 IM / web chat 消息路由到真实 Claude/Codex session。核心对象有四个:

| 对象 | 含义 |
|---|---|
| **chat** | 一个 IM 私聊、群聊或 web chat。每个 chat 有自己的当前项目、当前 session 和 session 列表。 |
| **project** | 本地一个已 `ccteam init` 的项目目录,用 slug 标识。 |
| **session** | 一个可继续上下文的 agent 会话,属于某个 chat × project × vendor × **role**。它是一个常驻的 `ThreadHandle`,有自己独立的 context(`/compact` `/clear` 各自独立),用 gateway 命名空间的 `s{n}` 标识。 |
| **role** | 一个可复用的 persona/定义 = 项目级 `.claude/agents/<role>.md`(frontmatter:persona + tools + disallowedTools + skills + mcpServers + model + effort + permissionMode + initialPrompt + memory)。session 启动时 `claude --agent <role>` 让 vendor **自读**该文件 → session 即 become 该 role。 |

一个 chat 可以同时活多个项目、多个 session;另一个 chat 状态独立,不会串。

**两层 session 模型**(v0.8.6 keystone):

- **① 项目知识层(vendor 原生,ccteam 不碰)**:Claude session 自动读项目 `CLAUDE.md`;Codex 自动读 `AGENTS.md`。老项目用自己的,新项目有啥读啥 —— ccteam 既不生成也不抑制(R8)。
- **② role / persona 层(ccteam 控制面)**:session 启动走 `claude --agent <role> --name <sid>`。`ccteam init` 种一个默认 role **`cto`**(chat-first「CTO 管家」persona);用户用 `/role <role>` 换角色干活。

**默认 role `cto`** 三职责(行为全写进 `.claude/agents/cto.md` 正文,**不靠 skill**):① 懂 ccteam(知识 + MCP 工具自描述);② 为用户**推荐**合适的 work-role;③ 调度 role-session —— **v0.8.6 只推荐,用户自己切 role**(主动 spawn/派活的 B 档与开 Claude Task 子 agent 均推后)。work-role 来源 = 用户自建 .md 或从 [agency-agents](https://github.com)(Claude 原生 .md 库)选,落同一 `.claude/agents/`;v0.8.6 选 role = 手动丢 .md,picker/import UI 推后下版。

**chat-local 路由命令**(由 gateway 路由表 `is_gateway_command` 拦截,源自单表 `GATEWAY_COMMANDS`):

| 命令 | 作用 |
|---|---|
| `/pair <code>` | 将当前 chat 建立为可用入口,并确保默认 session 存在 |
| `/cd <project>` | 当前 chat 切到项目(只切当前 chat) |
| `/newproject <slug> <path>` | 现场注册并建一个新项目 |
| `/new [vendor] [role]` | 在当前项目创建新 session(默认 vendor=claude、role=cto) |
| `/use <session-id>` | 当前 chat 切到已有 session |
| `/role <role>` | **原地换当前 session 的 role**(底层 = 带新 `--agent` 重启,**保持同一 sid**) |
| `/sessions` / `/projects` | 列当前 chat 的 session / daemon 已知项目 |
| `@handle <text>` | 路由到指定 session 并设为当前;不带 `@handle` 则发给当前 session |
| `@ccteam <NL>` | IM admin(status / pause / resume / list / cost / stop) |

`/compact` `/review` `/clear` 这类**不是** gateway 命令,会作为一个普通 turn / directive 透传给当前 session 的 adapter,由 adapter 翻译成 vendor-native 操作(详 §2.5)。

> **`/role` 的实现**(`switch_current_role`,gateway.rs):abort 旧 event pump + `close_thread` 旧 pane,以新 `--agent <role>` `start_thread`,**保持同一 gateway sid**(`/use <sid>` 不失效);无活动 session → 报错;同 role → no-op(不白扔 live context);保留原 vendor。`--agent` 是**启动期绑定**,换 role = fresh-spawn 该 session。

### 2.2 daemon = IM/web⇄session 路由网关

无 slug 的 `ccteam start` 是一个常驻 gateway daemon,**不是** tick loop / orchestrator 循环。它在同一个 tokio runtime 内、共享一条 shutdown 信号(Ctrl-C / SIGTERM / `ccteam stop` trigger 文件),启动以下任务:

| 组件 | 位置 / 说明 |
|---|---|
| IM gateway | `ccteam-im::run_daemon_with_shutdown`;Telegram(等)long-poll 入站 + 出站发送;chat⇄project⇄session⇄role 路由表 |
| Web chat WS | `GET /ws/chat`(`ccteam-chat.v1`);CLI 层 mpsc bridge 把 browser frame 翻成 `ChannelMessage{channel:"web"}` 后接入同一个 Gateway |
| 标准资源 API | `/api/v1/*`(web-token auth);project / role / session 三资源 + `/capabilities`(详 §2.6) |
| MCP socket | `~/.ccteam/run/mcp.sock` —— daemon-local line-delimited JSON-RPC handler,供 Claude/Codex plugin 调 ccteam 工具 |
| Web server | axum + SSE,默认 `http://127.0.0.1:7331`,服务 SPA bundle |

**关键约束**:此路径**不构造** `ccteam-flow::Orchestrator`,**不跑** supervisor tick(`ccteam-flow` 是推后的编排层,当前未接进运行中的 daemon,详 §7)。daemon 退出时**不 kill** tmux session(R5):下次 `ccteam start` 按持久化 session id 重新接管(Claude 按 deterministic session 名 reattach;dead pane recreate 走 `--resume` lossless);未发送 / 失败的 IM 出站回复保存在 `~/.ccteam/imd/outbound.jsonl`,启动后重放。

### 2.3 执行层两轴:HarnessAdapter × ProcessBackend

执行层正交两轴,组合是 N+M 不是 N×M:

- **`HarnessAdapter`(vendor 怎么驱动)** —— Claude = tmux TUI + `tmux send-keys -l` + transcript-tail + 官方 PreToolUse / Stop 等 hook;Codex = `codex app-server` JSON-RPC。
- **`ProcessBackend`(进程跑哪)** —— tmux / inproc / remote 等承载位置。tmux pane 操作(capture / resize / pane_pid)**只**住在 `PaneBackend` 子 trait,不在 base trait。

一个 session = `HarnessAdapter(vendor)` × `ProcessBackend(host)`。两个 vendor 都归一成中立的 `CanonicalEvent` + `ApprovalIR`(**不**抄 codex-emulation —— 每个 vendor 用自家原生通道:Claude 走 hook + transcript,Codex 走 JSON-RPC)。

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

- **`AgentSpecBrief`** 携带 `role`(spawn argv 用它拼 `--agent <role>`)+ slug + cwd + session-id-name。session=role 模型的落点就在这里:`spec.role` → claude argv 的 `--agent` 槽位。
- **`CanonicalEvent`** 是 `ThreadEvent` 的中立别名(`ThreadStarted` / `TurnStarted` / `TurnCompleted{usage}` / `TurnFailed` / `Item{Started,Updated,Completed}` / `Error`);schema 对齐 Codex `ThreadEvent`,两 vendor 的 emitter 1:1 映射进 gateway。`events()` 守 **final-only 契约**(rustdoc 钉死):一个 turn 的最终 agent 文本**恰好一次**经 `ItemCompleted(AgentMessage)` 吐出,`ItemUpdated` 仅 delta/presentation、consumer 可丢;token usage / plan / rate-limit 等非终态遥测**不**进此流(旁路镜像 `progress.jsonl`,经 `thread_status` 查询)。gateway 的取文本逻辑依赖此契约,故契约住 adapter trait。
- **`ExecutionMode`** 三态(`ThreadHandle::mode`):`InProc`(in-process subagent,占位)/ `Bg`(`claude --bg` / `codex exec --json` 单轮 fresh-context)/ `Chat`(tmux + claude TUI / `codex app-server` UDS,多轮 context 复用 —— IM/web 路径全走这个)。
- **resume 语义**:`Chat` 形态 resume-by-id —— Claude 用 deterministic session 名 lossless 续接,Codex 用 thread id app-server resume;`Bg` 形态每次 spawn fresh,`resume_thread` 返回 `NotImplemented`。
- **`close_thread`** 是唯一杀长 session 的路径:正常只由用户主动触发(`project stop` / `project rm`、`/role` 换角色),绝不静默(R5)。

**Vendor-seam forward-compat**:ccteam 读 sub-harness 吐出的 `state.json` / `codex app-server` JSON-RPC 通知 —— 这些是 vendor 自有 schema,按自家节奏新增字段 / enum 值,ccteam 清不掉也管不了(与「不做历史迁移」红线只管 ccteam 自有 state 不冲突)。`ccteam-harness::warn_unknown_vendor_token(seam, token, detail)` 是 process-wide warn-once helper(`(seam, token)` 作 dedup key);未知 token → skip + warn,不中断 event stream。

### 2.4 harness × provider facet + 可扩展 AgentVendor

执行层的「vendor 选型」在 v0.8.6 抽象为两个 facet,都是 **session 属性、非顶层资源**:

- **harness = agentic CLI 驱动器**:每个一套 `HarnessAdapter`。当前实现 **claude-code**(主路,smoke 实证)与 **codex**(best-effort);gemini-cli / grok-cli / 其它逐个 adapter 后续接入。
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

**Codex 命令面**(`crates/ccteam-harness/src/execution/codex_app_server.rs`):`handle_directive` 六类映射(RPC 直映射 / 查询合成→`Done` / per-session override / 语义 `Redirect` / TUI-only `Rejected` / server 错误原文传播)+ 三层 resolution(内建表 → `skills/list` 缓存动态匹配 → `Rejected` 附候选);弹窗两段式(bare→list RPC/静态枚举→`NeedsChoice`→重入 apply)。`CodexThreadTracker` 是 harness 级 dispatcher,消费 `thread/tokenUsage/updated` 维护 per-thread token 缓存,喂 Codex `/status` 与 `/sessions` 的 `thread_status`。transport **单轴** `resolve_codex_transport()`:有 `CCTEAM_CODEX_APP_SERVER_SOCKET` 走 UDS、否则默认 stdio。`default_adapter_factory` 是 **per-vendor 单例**(`crates/ccteam-im/src/daemon.rs`)→ 每 daemon 恰好一个常驻 codex app-server 子进程跨 resume/turn 复用。

**D6 agent 反问**(`AskUserQuestion`):机制是「hook IS the interaction」—— `intercept_ask` 的 chat 变体(`crates/ccteam-hooks/src/intercept_ask.rs`)在 `mode: chat` session 把 `AskUserQuestion` 转成同一中立 `ChoicePrompt`,经 daemon `mcp.sock` 的 `interaction/ask` op 发 IM、阻塞等答案,返回 `allow + updatedInput.answers` 让 picker 跳过、模型直拿答案;超时/无 chat → 降级 bg deny-with-reason。`ensure_chat_hooks_installed` 追加一条 matcher `AskUserQuestion` 的 PreToolUse 条目(写 `.claude/settings.local.json`,见 §3.2)。

> 协议细节(命令→RPC 映射表、Claude local-jsx 名单、Codex 命令 drift 快照、各 channel 渲染)一律以代码为准,见 §10 指针表。

### 2.6 标准资源 API(`/api/v1`)

把 web 现用接口抽成一套**标准资源 API**(web 现用 → 将来 app / 独立端可直接集成)。核心是 **3 资源 + facet**,全走既有 web-token auth、版本前缀 `/api/v1`:

| 资源 | 端点 | 说明 |
|---|---|---|
| **project** | `GET /projects`、`GET /projects/{slug}`(`api_v1.rs`,只读)· `POST /projects`、`DELETE /projects/{slug}`(`routes/projects.rs`) | `DELETE` = **注销 + 停 session(deregister),不 file-purge**;破坏性 purge 留 CLI `project rm --purge`(§4) |
| **role** | `GET /projects/{slug}/roles`、`GET/PUT /projects/{slug}/roles/{role}`(`routes/roles.rs`) | 读 `.claude/agents` 库;core `ccteam_core::roles::{list_roles,read_role}` 解析 frontmatter |
| **session** | `GET/POST /projects/{slug}/sessions`、`GET /sessions/{sid}`、`POST /sessions/{sid}/turn`、`POST /sessions/{sid}/resolve`(HITL token-resolve)、`GET /sessions/{sid}/events`(SSE)、`POST /sessions/{sid}/stop`(`routes/sessions_api.rs`) | session = project × role × harness(+provider),resume-by-id;`{sid}` = gateway `s{n}`;`/resolve {token,selection}` 走 `Gateway::resolve_web_selection`(= IM 点击同路,**非** turn) |
| **facet** | `GET /capabilities`(`routes/capabilities.rs`) | 动态列当前可用 harness(×provider):`HarnessCapability{id,vendor,available,providers}`;`available` = PATH probe(`<bin> --version` exit 0,进程级缓存) |

**gateway spine**(`crates/ccteam-im/src/gateway.rs`)—— 标准 API 的 drive 路径不动 `HarnessAdapter` trait,而是在 gateway 上加薄方法:

- `pub struct SessionView{ sid, ... }` + `session_views()` —— 列 daemon 内所有 tracked session;
- `create_session_api(...)` —— API 建 session(等价于 IM `/new`);
- `submit_to_sid(sid, text)` —— 向指定 sid 发 turn(等价于 `@handle`);
- `stop_session(sid)` —— 停指定 session;
- `GatewayEvent.sid: Option<String>` —— **SSE 过滤键**:per-session SSE handler 只保留 `sid` 匹配自己的事件。`None` = 不绑 session 的事件(如 `chat_send_file` MCP 路径、D6 `interaction/ask` hook prompt);IM 投递路径**忽略** `sid`(按 channel + chat_id 路由),故这是 additive。

**依赖方向(acyclic)**:共享 `Arc<Mutex<Gateway>>` 从 daemon 注入 ccteam-web `AppState`(**web → im 单向依赖**,无环);standalone `internal web`(无 daemon)→ gateway None → session 端点优雅 **503**(`gateway_unavailable`),只读 project/role/capabilities 仍可用。

> **前端 follow-up(已记录)**:per-session SPA UI 改造推后 —— 既有 SPA 会话页走旧 project-sid 路径(`/ws/chat` 混流),新 API 用 gateway-sid(`s{n}`),两 sid 命名空间不兼容,改造 = 重设计。**API 已 live + 真实 HTTP smoke 过**,留前端 handoff。

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

**progress 写入权威**:`ccteam-harness::progress_bridge` 是 `progress.jsonl` 业务事件 schema 的单一权威,`ccteam-core` 只 re-export(R2)。chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`,**不**依赖 Anthropic 内部 `~/.claude/projects/`。

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
   ▼  Gateway::handle_text:解析 @reviewer → 定位/创建 chat 当前 project 的 reviewer session
   │     (首条消息触发 HarnessAdapter::start_thread,以 `claude --agent reviewer --name <sid>` 启动;
   │      已存在则复用 ThreadHandle)
   │
   ▼  HarnessAdapter::submit_turn(handle, TurnInput::UserText("看一下..."))
   │     Claude: tmux send-keys -l "<text>" Enter
   │
   ▼  gateway 立刻回入口:"submitted <session> turn <id>"
   │
   ▼  HarnessAdapter::events 流(Claude: hooks fast event + transcript tail;Codex: JSON-RPC)
   │     → CanonicalEvent::ItemCompleted{AgentMessage} / TurnCompleted{usage}
   │     → 镜像写 turns.jsonl(R2)+ progress.jsonl 业务事件 + cost ledger(per-vendor)
   │     → GatewayEvent{ sid: Some("s3"), ... } 兼供 IM 投递 + per-session SSE
   │
   ▼  outbound ledger 把 assistant 文本发回 IM / web(at-least-once;失败重放)
```

> **session=role 实证细节**(W1 smoke):`--agent <role>` 顶层 turn 触发 `Stop`(→ `chat_turn_completed`,turn 完成检测命根)+ `SessionStart`/`UserPromptSubmit`/`Pre+PostToolUse`;有时**也**触发 `SubagentStop`(顶层被建模为 implicit-main 的 subagent)—— 但**不会双发 IM 回复**:回复只走 transcript-content track(`ItemCompleted{AgentMessage}`),hook track(stop/subagent-stop)只写 `progress.jsonl`,两轨解耦。

### 4.2 失败与恢复

gateway 的原则:失败必须可见,不能静默挂住。

| 故障 | 用户看到 | 恢复方式 |
|---|---|---|
| agent 启动失败 | `gateway error: ...` | 看 daemon log,确认 `claude` / `codex` 在 `PATH` 且已登录 |
| 缺 `cto.md` 的旧项目 | `--agent cto` 启动失败 | pre-v1.0「清旧数据 + 重 `ccteam init`」策略;init 必种 cto.md |
| Claude tmux pane 死 | `gateway error: ...` | `ccteam stop && ccteam start` 再发同一 `@handle`(deterministic 名 reattach;dead pane recreate + `--resume` lossless,失败回退 fresh spawn + emit `chat_session_reset{reason}`) |
| Codex app-server socket 断 | `gateway error: ...` | 确认 `codex app-server` 可用;重启 daemon(app-server resume 接回 thread) |
| turn 超时 | `gateway error: turn timed out ...` | 稍后重试;反复出现则 `/compact` 或 `/new` |
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
| turns.jsonl | chat 对话原文 SoT,ccteam-owned `<project>/.ccteam/chat/<bot>/` | `ccteam-im`(turns mirror) |
| role 定义 | `.claude/agents/<role>.md` frontmatter | `ccteam-core::roles`(`list_roles` / `read_role`) |
| outbound ledger | `~/.ccteam/imd/outbound.jsonl`;at-least-once IM 投递,daemon 启动后重放 | — |

**关键论证**:「progress.jsonl 唯一事实来源」是架构红线(R2)。曾考虑解析 tmux `capture-pane` 输出做状态判断 —— 拒,因为终端文本格式不稳定、ANSI 转义难、对 prompt cache 敏感(R6)。所有状态转移走 hook + transcript jsonl + app-server event,deterministic 且可重放。

---

## 6. Claude Code / Codex 扩展点映射

### 6.1 Tmux 长 session(Claude chat session = role)

chat session = per-bot 一个 tmux session + `claude --agent <role>` TUI 长跑;dual-track 观测,**不 scrape pane**(R6)。

```bash
SESSION="ccteam-chat-<slug>-<role>"      # tmux session deterministic 命名
# fresh spawn:--agent <role> 让 vendor 自读 .claude/agents/<role>.md(R3 兑现);
#             --name 让 Anthropic 把 session jsonl 落在 deterministic 名下,
#             使日后 recreate 能 --resume(argv 顺序以 claude_tui.rs spec_for_new 为准)
tmux new-session -d -s "${SESSION}" -c "${PROJECT_DIR}" \
  "claude --agent <role> --dangerously-skip-permissions --name <session-id-name>"
# 输入面:tmux send-keys -l 直送 user content + Enter(literal 模式,0 escape 雷区);
#         /compact /new /clear 经 handle_directive 透传(§2.5)
# 输出面 Track A:官方 hooks(SessionStart/UserPromptSubmit/Stop/SubagentStop/PostToolUse)
#                 作 fast event 通道(低延迟 turn boundary)
# 输出面 Track B:byte-offset 增量读 transcript jsonl → 抽 full message content →
#                 镜像写 ccteam-owned turns.jsonl(R2 SoT)
```

**关键约束**:✅ 用 `--dangerously-skip-permissions`(消灭弹窗,痛点 8);✅ `--agent <role>`(session=role,R3 兑现);❌ **不**用 `claude -p`(失去 attach + 每 turn 冷启 cache 失效);❌ **不**设 `--max-turns`(长跑,由超时 + 成本上限兜底)。

**daemon-restart 上下文恢复**:dead-pane recreate 走 `claude --agent <role> --dangerously-skip-permissions --resume <session-id-name>` lossless lookup(`spec_for_resume`,resume 路径**也带** `--agent`)—— Anthropic 自己 reload session jsonl,cache + 推理链续接;失败才 fallback fresh spawn(`spec_for_fresh` 委托 `spec_for_new`,继承 `--agent`)+ emit `chat_session_reset{bot, reason}`(user-visible degraded,不冒充 resume)。tmux 命令包在 `tokio::process::Command` 异步 spawn —— 单 binary 零额外运行时依赖。

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

`ccteam init` 种默认 `cto.md`(管家 role,3 职责见 §2.1);work-role 由用户自建 / 从 agency-agents 选,落同一目录。**这是 ccteam 唯一管理的"指令面"** —— 与「No prompt injection」红线不冲突:ccteam 不向 pane / app-server 注入 system prompt,而是让 vendor 用原生 `--agent` 机制自读 role.md(R3 兑现)。`session persona` / API `PUT /roles/{role}` 改写 role.md body(保留 frontmatter),下次该 session fresh start 生效。

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

#### 提供的 MCP:`ccteam`(**17 工具**,0 STUB)

v0.8.6 把 MCP 工具瘦身到 12 个(退役推后编排的 `workflow_*` 套件 + `chat_reset`);v0.8.7 W1 为 cto 调度新增 `session_` group 5 工具 → 共 17。所有工具加 group 子前缀,**server name 不变**(`ccteam`):

| Group(子前缀) | 工具数 | 工具 |
|---|---|---|
| `admin_` | 3 | ls / change_persona / add_tool |
| `chat_` | 6 | register_bot / unregister_bot / list_bots / send_input / history / send_file |
| `advise_` | 2 | vote(Claude + Codex 并行 advisor + 第三次 Claude verdict synthesis)/ parallel(N-of-N 原文返回) |
| `session_` | 5 | cto 调度:spawn / dispatch / collect / list / stop(work-role session 的 spawn→dispatch→polled collect;**daemon `role==cto` 硬门** + 只用 gateway session map,不碰 deprecated registry/supervisor) |
| `screenshot` | 1 | 只读终端截图(单成员 group,无子前缀:`ccteam__screenshot`) |

`STUB_TOOLS: &[&str] = &[]`(`crates/ccteam-cli/src/mcp_tool_groups.rs`)是 invariant 守门员;`ccteam doctor --verify-mcp` 自检 stub-counter parity + 总数(17),drift → exit code 1。`CCTEAM_DISABLE_TOOLS` 用 group enum(非 glob,防 typo):`CCTEAM_DISABLE_TOOLS=advise,chat`。完整 tool schema = `tool_definitions()`(`mcp_serve.rs`)+ 各 group 模块(`mcp_{admin,chat,advise,session}_tools.rs`)。

**`session_` 权限门(双层)**:① cto role.md `tools:` 行授予 5 个 `mcp__ccteam__session_*`(work-role 模板不列 → Claude allow-list 第一道);② **硬门**:daemon `execute_session_tool` 据 ambient `_caller_role`(spawn env 注入,**绝不**取 caller args 防伪)经纯函数 `session_caller_authorized` 查 `SESSION_TOOL_PRIVILEGED_ROLES=["cto"]`,**门先于 gateway 执行**(gateway down 也拒非 cto)。collect = polled MVP(tail 子 session `turns.jsonl`,`since` 游标 + `n` 上限)。

> **曾经的 `workflow_*`(15→8→0)与 4+3 bundled skill 均已退役**:推后编排的 marker 工具 v0.8.6 无 consumer,session 生命周期改走 CLI(`project`/`session` 组)/ IM(`/new` `/role` `@ccteam`)/ 标准 API(§2.6)。原 skill 功能落 MCP 工具 + cto role + work-role + config CLI。

**Wire 协议纪律**:`ccteam internal mcp-serve` stdout 是 line-delimited JSON-RPC frame channel,**所有 tracing / 日志走 stderr**,否则污染 frame parse。两条 transport(stdio + daemon 的 `~/.ccteam/run/mcp.sock`)共用同一 handler。

#### admin actions:change-persona + add-tool

- daemon-side **只做文件 mutation**:`change_persona` 读 `.claude/agents/<bot>.md` 替换 body(保留 frontmatter)写回 + emit `persona_changed`;`add_tool` 读 `workflow.yaml` parse `agents[bot].tools:` 去重 append + emit `tool_added`。**不调 LLM**(R3)。
- 生效路径:bot 下次 fresh start 读新 `.claude/agents/<bot>.md`(因 session 真的 `--agent <role>` 加载它,W1 起天然生效)。

### 6.5 安全

- **per-session 权限模式(v0.8.7 W2)**:每个 session 有 `PermissionMode { Skip(默认), Hitl }`(`ccteam-harness/adapter.rs`,挂 `SpawnCtx.permission_mode`,穿所有创建路径:IM `/new claude cto hitl` 尾 token、web `POST /sessions` `permission_mode`、cto `session_spawn` param;`/role` 切换 + 重启 resume 保留)。
  - **`Skip`** → spawn 走 `--dangerously-skip-permissions`(YOLO,无手机批准门;agent 按本机 Claude Code 权限直接执行)。
  - **`Hitl`** → spawn 走 `--permission-mode default`(**绝不** skip,否则 ask-path 被白嫖)+ per-session 注入 vendor 原生 `PermissionRequest` hook(无 matcher = 全工具、无 timeout 让长审批不被杀)→ 非 allowlist 工具触发 hook → `ccteam-hooks/permission_request.rs` 经 `permission/ask` JSON-RPC 走 mcp.sock → daemon `execute_permission_ask` 建 Approve/Deny ChoicePrompt(token-keyed External pending)弹到绑定 chat → 用户点同意才放行。**回应路两条同源**:IM 点击 = `resolve_selection`,web 点击 = `POST /sessions/{sid}/resolve` → `resolve_web_selection`,两者都 `take_by_token`→`apply_pending` 同一 pending(非 turn)。**FAIL-SAFE = deny**;permission prompt 用比 600s interaction/ask **更短的 TTL**(`PERMISSION_PROMPT_TIMEOUT_SECS_DEFAULT`,env `CCTEAM_PERMISSION_PROMPT_TTL_SECS`)+ outstanding 时写一行 `chat_permission_prompt_outstanding` progress(operator 知道 parked);deny **只挡该次工具、不带 `interrupt`、不 kill turn**(守 R5)。**不注入 system prompt**(R3:批准门是 vendor 原生 hook,非注入)。Codex 忽略该 lever(自有 sandbox)。
- **Telegram allowlist**:`~/.ccteam/im/credentials.json` 的 `allowed_chat_ids` 是第一层边界,生产不留空;bot token 不进 git;daemon 只跑在你控制的机器上;Web UI 只绑 `127.0.0.1` 除非明确配反代 + 鉴权。Lark/Feishu allowlist `allowed_user_ids`(open_id)**fail-closed**(空=拒绝所有人,与 telegram 相反)。

### 6.6 Web 仪表盘 + 标准 API

`crates/ccteam-web/` 是 Vite + React SPA(`build.rs` 在 `cargo build` 时跑 `npm run build` → rust-embed,`CCTEAM_SKIP_WEB_BUILD=1` 跳过);backend axum + SSE,服务 SPA bundle + 标准资源 API(§2.6)+ SSE + WS。

**Authentication**:loopback 免 token;非 loopback 自动生成 `~/.ccteam/web-token`(mode 0600)+ LAN-RCE 倒计时;URL shim `?token=ccteam:<hex>` → HttpOnly cookie + 303 干净 URL。标准 API `/api/v1/*` 全走同一 web-token auth。

**Chat 控制台**:`/app/chat` 通过 `GET /ws/chat` + 子协议 `ccteam-chat.v1` 进入 Gateway。`ccteam-web` 持有 web-local JSON 帧和 mpsc 端点;`ccteam-cli::web_chat_bridge` 在 `run_start` 装配处把它翻译成 `ccteam-im::transport::ChannelMessage` / `SendMessage`,所以 web 与 IM **共用同一个 `Gateway::handle_text`、同一批 session、同一个 outbound ledger**。

**per-session web 视图(v0.8.7 W4)**:`/chat/s/:sid`(`ChatConsole.tsx` 按 `s{n}` keyed)= 每个 gateway session 独立页 + 历史 + 干净切换(不混流):历史走 `GET /api/v1/sessions/{sid}`(`session_resolve(sid)` 作 404 闸 → 读 `<project_dir>/.ccteam/chat/<role>/turns.jsonl`,**非**按 `session_id==s{n}` 过滤 progress.jsonl);事件走 `GET /api/v1/sessions/{sid}/events`(按 `ev.sid==Some(s{n})` 过滤的 gateway broadcast SSE);per-sid localStorage(`ccteam.chat.rows.v2.${sid}`),sid 变即 reseed + 重订阅。HITL 审批 ChoicePrompt 经 SSE 带 `sid`+`token`+每 option `{label,id}` 渲染成 per-session 琥珀色行;点击 `[Approve]`/`[Deny]` → `POST /sessions/{sid}/resolve {token,selection=id}` → `Gateway::resolve_web_selection`(`take_by_token`→`apply_pending`,= IM 点击同路,**非** turn)→ 阻塞的 `permission/ask` hook 即收 allow/deny(工具真跑 / 即时拒,无 600s 超时)。

**OpenAPI 自动文档(v0.8.7 W5)**:`GET /api/docs`(`utoipa-scalar` 交互式 UI)+ `GET /api/v1/openapi.json`(OpenAPI 3.1 spec)。spec 由**同一套路由注册**生成(单聚合 `OpenApiRouter`,`routes/openapi.rs` `split_for_parts()` 既出 live `Router` 又出 spec → 单源、anti-drift);每 `/api/v1` handler 挂 `#[utoipa::path]`,加/删路由不注解会改 op 数、drift 测试红(强制 dual-edit)。两者 mount 在同一 `auth::auth_layer` web-token 门后(无公开未鉴权 spec)。

**架构红线**:web 守 R2(SSE watcher 仅读 progress.jsonl)/ R5(不 kill 长 session)/ R6(不解析 tmux 终端;裸终端字节只走 `ccteam-pty.v1`)/ R8(不写跨项目记忆)。写控制走跟 IM channel **完全相同**的 gateway dispatch 路径。ccteam 核心是 **headless 状态引擎** —— 任何前端**不得**引入新 LLM 层:web/API 输入 = 经 gateway 写控制,不经任何 ccteam 中介 LLM;LLM 推理只发生在 agent session 内部。

### 6.7 透明度与可观测性

| 你要看 | 命令 / 文件 |
|---|---|
| daemon 是否活着 | `ccteam status` |
| 安装和依赖 | `ccteam doctor` / `ccteam doctor --verify-mcp` |
| 最近 daemon 日志 | `tail -120 /tmp/ccteam.log` |
| outbound ledger | `~/.ccteam/imd/outbound.jsonl` |
| 项目状态 / 业务进度 | `~/.ccteam/progress/<slug>.jsonl` / `<project>/.ccteam/state.json` |
| 当前可用 harness | `GET /api/v1/capabilities` |
| 一屏看全 | web SPA(SSE 实时) |

**Stall 检测(软告警,不强制 kill)**:`agent_spawn` 后无对应完成事件超阈值 → 软告警。**永远不主动 kill**(R5)—— 除非命中物理上限(per-vendor cap 或全 ccteam $200),或用户显式 `project stop` / `rm --force`。相信长跑、相信用户能介入。

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
| **`--agent <role> --name/--resume` 在 tmux send-keys 路径** | session=role 模型不成立 | W1 smoke gate 已实证(交互 + resume + hooks 全触发);见 §4.1 |
| **agent session 卡死 / turn 超时** | 用户等不到回复 | submit/turn 超时阈值 → `gateway error` 回 IM;stall 软告警;per-vendor cap 兜底 |
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
| chat turns.jsonl | `crates/ccteam-im`(turns mirror) | — |
| CLI 命令 / flag(分组) | `crates/ccteam-cli/src/main.rs`(clap `Command` / `Project` / `Session` / `Internal`)+ `commands.rs` | `ccteam --help` |
| `config` setup hub | `crates/ccteam-cli/src/commands.rs`(`run_config_menu` + `config <key> <value>`/`get`/`show`/`mcp`;包 `ccteam_im::onboarding::telegram_setup`) | `ccteam config show`;`cargo test -p ccteam-cli config` |
| 删除/停止引擎 | `crates/ccteam-cli/src/commands.rs`(`run_remove`/`RemoveOptions`/`RemoveReport` + `run_project_stop` + `stop_project_chat_sessions` + `purge_project_managed_paths`) | `cargo test -p ccteam-cli --test remove_test` |
| settings.local.json hook merge/scrub | `crates/ccteam-core/src/tool_surface.rs`(`ensure_chat_hooks_installed` / `remove_chat_hooks`) | `cargo test -p ccteam-core tool_surface` |
| MCP 工具清单 / schema(17) | `crates/ccteam-cli/src/mcp_tool_groups.rs`(`STUB_TOOLS` / `ToolGroup`,含 `Session`)+ `mcp_serve.rs::tool_definitions` + `mcp_{admin,chat,advise,session}_tools.rs` | `ccteam doctor --verify-mcp`(drift → exit 1) |
| cto 调度 `session_*`(spawn/dispatch/collect/list/stop)+ role 门 | `crates/ccteam-cli/src/mcp_session_tools.rs`(5 工具)+ `main.rs`(`execute_session_tool` + `session_caller_authorized` + `SESSION_TOOL_PRIVILEGED_ROLES`,daemon-side 硬门);gateway 解析 = `ccteam-im/src/gateway.rs`(`session_resolve` → tail 子 `turns.jsonl`) | `cargo test -p ccteam-cli mcp_session`;`ccteam doctor --verify-mcp`(session 5/0) |
| HITL 权限模式(per-session) | `crates/ccteam-harness/src/adapter.rs`(`PermissionMode { Skip, Hitl }` + `SpawnCtx.permission_mode`);spawn argv + hook 安装 = `execution/claude_tui.rs`(`permission_args` → skip 用 `--dangerously-skip-permissions`、hitl 用 `--permission-mode default` + 注入 `PermissionRequest` hook) | `cargo test -p ccteam-harness claude_tui` |
| HITL 批准回路 `permission/ask` | hook 端 = `crates/ccteam-hooks/src/permission_request.rs`(读 stdin → `permission/ask` JSON-RPC over mcp.sock,FAIL-SAFE deny,**无 `interrupt`**);daemon 端 = `crates/ccteam-cli/src/main.rs`(`execute_permission_ask` 建 Approve/Deny ChoicePrompt + `summarize_tool_input` + `gateway.session_sid_for` + 短 TTL `permission_prompt_timeout_secs` + `emit_permission_prompt_outstanding` parked-signal);回应两路同源 = IM `resolve_selection` / web `Gateway::resolve_web_selection`,都复用 `ccteam-im/src/pending.rs` `take_by_token` | `cargo test -p ccteam-cli permission`;`cargo test -p ccteam-im web_resolve`;`#[ignore]` `claude_agent_hitl_*` smoke(真 binary) |
| role catalog / import(`ccteam role`) | catalog(离线 parse/search/find)= `crates/ccteam-core/src/role_catalog.rs` + `templates/agency_agents_catalog.json`(192 entries)+ `sanitize_role_stem`;网络 import = `crates/ccteam-im/src/role_import.rs`(`import_role_from_catalog{,_with_base}`,`AGENCY_RAW_BASE` HEAD,verbatim `write_role`);CLI = `ccteam-cli/src/main.rs`(`Command::Role`)+ `commands.rs`(`run_role_{search,add,list}`) | `cargo test -p ccteam-core role_catalog`;`cargo test -p ccteam-im role_import` |
| per-session web 历史 / SSE / 审批渲染 / 批准 | 历史 = `crates/ccteam-web/src/routes/sessions_api.rs`(`GET /sessions/{sid}` → `session_resolve` → `collect_session_turns` 读 `turns.jsonl`;按 sid 过滤 SSE `event_matches_sid`;审批 SSE frame 带 `token`+option `{label,id}` via `session_event_payload`/`approval_token`;`POST /sessions/{sid}/resolve` → `handle_session_resolve` → `Gateway::resolve_web_selection`);SPA = `web/src/pages/ChatConsole.tsx`(`/chat/s/:sid` keyed,`resolveApproval` 调 `/resolve`)+ `lib/sessionsApi.ts`(`resolveApproval`)+ `hooks/useSessionEvents.ts` + `pages/chatTranscript.ts` | `cargo test -p ccteam-web sessions_api`;`cargo test -p ccteam-web --test openapi_test`;vitest |
| OpenAPI spec / `/api/docs` | `crates/ccteam-web/src/routes/openapi.rs`(单聚合 `OpenApiRouter` `split_for_parts()` + `Scalar` UI + `openapi.json` serve);各 handler `#[utoipa::path]` 注解(`api_v1.rs`/`projects.rs`/`roles.rs`/`sessions_api.rs`/`capabilities.rs`/`teams_*`);spec.version = `env!(CARGO_PKG_VERSION)` | `cargo test -p ccteam-web --test openapi_test`(op-count drift 测试) |
| Hooks impl / settings | `crates/ccteam-hooks` + `ccteam internal hook` 子命令;`ccteam init` 落 `.claude/settings.local.json` | — |
| Web 路由总装 | `crates/ccteam-web/src/routes/mod.rs`(router 合并) | — |
| 标准 API:project | `crates/ccteam-web/src/routes/projects.rs`(POST/DELETE)+ `api_v1.rs`(GET list/show) | 真实 HTTP smoke(W5b) |
| 标准 API:role | `crates/ccteam-web/src/routes/roles.rs`(GET list / GET·PUT one) | — |
| 标准 API:session | `crates/ccteam-web/src/routes/sessions_api.rs`(GET/POST + `{sid}` + `/turn` + `/resolve` + `/events` SSE + `/stop`) | `cargo test -p ccteam-web --test openapi_test`(op-count drift) |
| 标准 API:capabilities | `crates/ccteam-web/src/routes/capabilities.rs`(`HarnessCapability` + `probe_available` PATH probe) | — |
| gateway spine(标准 API drive) | `crates/ccteam-im/src/gateway.rs`(`SessionView` + `session_views` / `create_session_api` / `submit_to_sid` / `stop_session`;`GatewayEvent.sid` = SSE 过滤键) | `cargo test -p ccteam-im gateway` |
| Web chat WS (`ccteam-chat.v1`) | `crates/ccteam-web/src/chat_protocol.rs` + `routes/chat_ws.rs`;CLI bridge = `crates/ccteam-cli/src/web_chat_bridge.rs` | `cargo test -p ccteam-web chat_frame` |
| PTY WS (`ccteam-pty.v1`) | `crates/ccteam-web/src/routes/pty_ws.rs` + SPA `useTerminal` | `cargo test -p ccteam-web --test pty_ws_test`(env-gated) |
| IM transport / 凭证 | `crates/ccteam-im/src/transport/`(`Channel` trait + providers)+ `im/credentials.json` 解析 | — |
| IM 出站分片 | `Channel::max_message_len` + `sanitize::split_for_channel`(UTF-16 预算 / fence 平衡)+ `daemon::send_gateway_outbound` | `cargo test -p ccteam-im split` |
| IM 进度 status | `progress::ProgressFold` + `Channel::edit_message` + `gateway::spawn_event_pump` + `GatewayEventKind::Progress` | `cargo test -p ccteam-im --test im_progress_test` |
| IM 附件 I/O | 入站:`transport::{ChannelAttachment,AttachmentKind}` + telegram `getFile`;出站:`transport::{OutboundFile,OutboundFileKind}` + `chat_send_file` 走 `mcp.sock` | `cargo test -p ccteam-im` |
| HarnessAdapter / ProcessBackend / AgentVendor | `crates/ccteam-harness/src/adapter.rs`(trait + `AgentVendor` enum + `ExecutionMode`)+ `lib.rs` | `cargo test -p ccteam-harness adapter` |
| Claude spawn argv(session=role) | `crates/ccteam-harness/src/execution/claude_tui.rs`(`spec_for_new` / `spec_for_resume` / `spec_for_fresh`,均含 `--agent <role>`) | `cargo test -p ccteam-harness --test claude_tui_resume_test` |
| 命令面中立词汇 | `Directive` / `DirectiveOutcome`(5 值)/ `ChoicePrompt` / `ChoiceOption` / `ChoiceSelection` / `ContextUsage` / `ThreadStatus` + `handle_directive` / `thread_status`(无 default impl)= `crates/ccteam-harness/src/adapter.rs` | `cargo test -p ccteam-harness adapter` |
| gateway 纯路由 + 命令菜单 + `/role` | slash→`Directive`→`handle_directive`→渲染 outcome + `GATEWAY_COMMANDS` 单表 + `resolve_selection`(token-global)+ `switch_current_role`(`/role` 原地换、保持同一 sid)+ `render_sessions` = `crates/ccteam-im/src/gateway.rs` | `cargo test -p ccteam-im gateway` |
| pending interaction 注册表 | `PendingInteractions`(自己的锁 / 双 origin / 单飞 per(chat,session) / TTL / `take_by_token`)= `crates/ccteam-im/src/pending.rs` | `cargo test -p ccteam-im pending` |
| transport 选项/回填/菜单 | `MessageOption` / `ChoiceReply` / `SendMessage.options` / `ChannelMessage.selection` / `Channel::register_commands`(全 `#[serde(default)]`)= `crates/ccteam-im/src/transport/mod.rs`;TG inline keyboard + `callback_query` = `transport/providers/telegram.rs` | `cargo test -p ccteam-im` |
| Claude 命令面 gate | 四通道 `handle_directive`(prompt 透传 / BRIDGE_SAFE local / arg-popup `NeedsChoice` / panel-only `Rejected`)+ `/esc`→Escape + `ensure_chat_hooks_installed` 追加 `AskUserQuestion` matcher = `crates/ccteam-harness/src/execution/claude_tui.rs` | `cargo test -p ccteam-harness --test claude_tui_test` |
| Codex 命令面 + transport | 六类映射 `handle_directive` + 三层 resolution + 两段式 + `CodexThreadTracker`(消费 `thread/tokenUsage/updated`)+ `resolve_codex_transport()`(socket→UDS / 默认 stdio)= `crates/ccteam-harness/src/execution/codex_app_server.rs`;per-vendor 单例工厂 `default_adapter_factory` = `crates/ccteam-im/src/daemon.rs` | `cargo test -p ccteam-harness --test codex_app_server_test` |
| D6 反问 ingress | `intercept_ask` chat 变体(`AskUserQuestion`→`ChoicePrompt`→`mcp.sock`→`updatedInput.answers`)= `crates/ccteam-hooks/src/intercept_ask.rs`;`mcp.sock` 的 `interaction/ask` op = `crates/ccteam-cli/src/main.rs` | `cargo test -p ccteam-hooks intercept_ask` |
| `/sessions` 状态:model + 上下文 | Claude 倒读 transcript 尾(`read_status_tail`,`[1m]`→1M / 否则 200k 基线)= `crates/ccteam-harness/src/execution/transcript_tail.rs`;Codex 读 `CodexThreadTracker`;gateway 单点渲染 `188k / 1M (19%)` = `ContextUsage::render` / `ThreadStatus::status_suffix`(`adapter.rs`) | `cargo test -p ccteam-harness adapter` |
| workflow.yaml schema(推后) | `ccteam-flow` / `ccteam-core` 解析代码(推后的编排层,§7) | — |

改协议 = 改代码 +(若新增一类协议)补本表一行。**不**再维护独立的 interfaces.md。
