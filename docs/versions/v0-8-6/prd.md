# v0.8.6 PRD —— IM 通用模式（session = subagent role）+ 目录/CLI 收敛

> **Doc-first PRD**。用户（TG）review 后才动代码。讨论来源：2026-06-05 TG（rounds 1–4，多次收敛）。协议细节以代码为 SoT，本文记「为什么 + 决策 + 验收」。file:line 为讨论期 grep 结论，落地前 Wave 内复核。

---

## 0. Scope & 核心模型

**本版主线**：把 **IM 通用模式**开发好（`先把 im 方式走通`）。**多模式（agent-team / 自治 bg）现在不管** —— 不删、`.claude/agents/` 反而成为核心（role 库），但本版不投入自治编排。

### 0.1 核心模型：session = subagent role（两层）

经源码验证（`references/claude-code/src`），`claude --agent <role>` 是**顶层、交互式、可 resume** 的 CLI flag（main.tsx:1377；tipRegistry.ts:427「use --agent to directly start a conversation with a subagent」；sessionRestore.ts:194/208 跨 resume 保留）。据此 IM session 模型定为两层：

- **① 项目知识层（vendor 原生，ccteam 不碰）**：Claude session 自动读项目 `CLAUDE.md`；Codex 自动读 `AGENTS.md`。ccteam **不生成、不修改、不抑制**这些文件 —— 项目自己的事（老项目用自己的，新项目有啥读啥）。
- **② role / persona 层（ccteam 控制面）**：IM session 启动走 `claude --agent <role> --name <sid>`，session 即 become 该 role。role 定义携带 persona + tools + disallowedTools + skills + mcpServers + model + effort + permissionMode + initialPrompt + memory（loadAgentsDir.ts:106-132）。**role 库放项目级 `.claude/agents/<role>.md`**，`ccteam init` 种默认 role（可按项目改）。ccteam 的通用 bot 行为（IM 回复风格 / MCP 工具意识等）写进 role 定义本身，自洽。

### 0.2 相对前几轮的两个反转（已与用户确认）

- **不再抑制 CLAUDE.md**：放弃 `omitClaudeMd` / `CLAUDE_CODE_DISABLE_CLAUDE_MDS`。CLAUDE.md 照读，作项目知识层。（round-3「不参考 claude.md」字面需求被用户校正：vendor 本性会读，不跟它较劲。）
- **「AGENTS.md 替代 CLAUDE.md」作废**：不再统一指令文件。各 vendor 读各自原生文件（Claude=CLAUDE.md / Codex=AGENTS.md），ccteam 不接管；ccteam 的自定义只在 **role** 层。（round-1/2 的 AGENTS.md 生成 + 桥接规则全部撤销。）

### 0.3 红线遵守
no prompt injection（role 走 vendor 原生 `--agent`，ccteam 不注入 system prompt）/ `progress.jsonl` 是 state SoT / 不解析终端输出 / 永不**主动** kill 长 session / 会话 = resume-by-id / pre-v1.0 不留迁移与 compat shim。

### 0.4 用户启动全流程（两命令面：宿主 CLI + IM）
**宿主 bootstrap（终端，一次性）**：① `curl install.sh | sh`（前置 claude 已装+登录）② `ccteam init`（建 ~/.ccteam 骨架 + cwd 项目：`.ccteam/{state.json,workflow.yaml}` + `.claude/agents/cto.md`）③ `ccteam config`（交互菜单：装 MCP + IM token + 未来 config）④ `ccteam start`（常驻 daemon = IM gateway + web + MCP socket）。
**IM 驱动（Telegram）**：`/pair <code>` → `/cd <项目>`（或 `/newproject <slug> <path>` 现建）→ daemon spawn `cto` session（`claude --agent cto --name`）→ 直接聊（cto 荐 role）→ `/role <role>` 换角色干活 → `/new`·`/use`·`@handle` 多会话 → 透传 `/compact /review` → `@ccteam status/pause/remove`。
**删除（init 逆）**：`ccteam project rm <slug> [--purge]` / IM `@ccteam remove`（见 Item 3 D3.5）。

---

## 1. Item A（主线）—— IM session 绑定 subagent role

### 1.1 现状
- chat 路径 spawn argv（claude_tui.rs:288-291 / 263-266）：`claude --dangerously-skip-permissions --name|--resume <sid>` —— **无 `--agent`**。`CCTEAM_CHAT_ROLE` env 仅供 hook 给 progress 打标签（chat_spawn_env_owned :233-237），**不加载 persona**。
- 结论（见 `docs` 内 role 加载分析）：chat 模式里 role = 纯寻址；bot 脑子 = 项目根 CLAUDE.md（vendor auto-load）。`.claude/agents/<role>.md` 在 chat 模式**从不被读**。
- 副作用 gap：`admin change-persona` 写 `.claude/agents/<role>.md` 对 chat bot **无效**。

### 1.2 决策
- **DA.1 spawn 改造**：chat session 启动 argv 从 `claude --name <sid>` 改为 `claude --agent <role> --name <sid>`（resume 路径同理带 `--agent`）。role 取自 session 的 (slug, role) 寻址键 —— 寻址键不变，新增「以该 role 启动」语义。
- **DA.2 role 库 + 默认 role `cto`**：role 库 = 项目级 `.claude/agents/<role>.md`。`ccteam init` 种 1 个默认 role **`cto`**（`.claude/agents/cto.md`，替代 explorer.md scaffold）。cto = chat-first「CTO 管家」persona，三职责：① **懂 ccteam**（知识写进 cto.md 正文 + MCP 工具自描述，**不靠 skill**）；② 为用户**推荐**合适 work-role；③ 调度 role-session —— **v0.8.6 取 A 档：只推荐，用户自己切 role**（DA.4 换 `--agent`）。cto 主动 spawn/派活 role-session（B 档）与开 Claude Task 子 agent（多模式）均**推后**。session 默认 `claude --agent cto --name <sid>`。
- **DA.2b work-role 来源**：用户自建 / 从 **agency-agents**（Claude 原生 .md，209 个，MIT）选，落同一 `.claude/agents/`。v0.8.6 选 role = **手动丢 .md**；picker/import UI **推后下版**。
- **DA.3 change-persona 修复**：因 session 现在真的 `--agent <role>` 加载 role.md，`change-persona`（→ `session persona`，见 Item D）天然生效（下次启动/rebind 生效）。
- **DA.4 切换 role**：「改 session 的 role」= 用新 `--agent` 重启该 session（`--agent` 是启动期绑定）。**IM 入口 = 新增 gateway 命令 `/role <role>`**（换当前 session 角色，底层 = 带新 `--agent` 重启；今天 IM 无此命令，是 gap）；CLI 入口 = `session role <slug> <sid> <role>`。运行时切换（若 Claude 有 in-session `/agent`）Wave-1 复核。
- **DA.5 Codex（本版 best-effort / 推后）**：Codex session 读 `AGENTS.md`（原生项目知识层）。Codex 的 role/subagent 绑定机制与 Claude `--agent` 不同（Codex subagent = 独立机制），本版**不强求 Codex role 对齐**，留 Claude 为主；Codex 只保证项目知识层（AGENTS.md）可读。

### 1.3 验收
- IM 新建/恢复 session 的 argv 含 `--agent <role>`（deterministic fake claude 断言 argv，参考 claude_tui_resume_test.rs 既有模式）。
- 改 `.claude/agents/<role>.md` 后，重启该 session 行为随之变（smoke）。
- chat-progress hooks 在 `--agent` session 下仍触发（hooks.ts:318 表明 hook 区分 main-thread vs subagent in `--agent` session，需 smoke 实证）。
- `--agent <role> --name/--resume` 在 tmux send-keys 路径的交互 + resume 组合走通（Wave-1 smoke gate）。

---

## 2. Item 1 —— `~/.ccteam` 全局目录梳理

### 2.1 现状
生成时机：`ccteam init`（commands.rs:108-133 建骨架空目录）/ `ccteam start`（main.rs:2155 建 run/）/ 注册 bot（imd/registry/）。
- **KEEP**：config.yaml、hooks/hook.sh、run/、progress/、imd/、im/credentials.json、web-token、teams-progress.jsonl、state/{pid}。
- **死（停建 + 删）**：phases/ queue/ memory/ control/ templates/（helper 已空）、watchdog.yaml、state/orchestrator.heartbeat、config.yaml.bak/.tmp（保留原子写产物）。inbox/ 核实（internal send 边角）。

### 2.2 决策
- **D1.1** init 停建 orchestrator-era 骨架目录；只建当前架构真用的（hooks/）。其余按需 mkdir。
- **D1.2** `~/.ccteam` 规范布局收敛到 `ccteam-core/src/paths.rs` 单一 manifest，所有 path accessor 从此派生。
- **D1.3** `ccteam doctor` 加 home-layout drift 检查（+ 可选 `--gc-home`）。
- **D1.4（低优）** `state/orchestrator.pid` → `state/daemon.pid`；heartbeat 删（无 alias）。

### 2.3 验收
全新 init 后 `~/.ccteam` 无死目录；paths.rs 单一布局来源；doctor 报告/清理 drift；baseline 不退。

---

## 3. Item 2（重定义）—— init 模板清理（不再管 CLAUDE.md/AGENTS.md）

### 3.1 现状
init 写：`.ccteam/{spec.md, state.json, workflow.yaml, agents/, skills/.gitkeep[, inbox/.gitkeep]}`、`.claude/{settings.json, agents/}`、根 `CLAUDE.md`（仅当 !exists）。模板一半集中（include_str! 自 templates/）、一半内联 const。

### 3.2 决策（按 §0 模型重定义）
- **D2.1 ccteam 不再生成/桥接/抑制 CLAUDE.md 或 AGENTS.md**（项目知识层归 vendor + 项目）。删除 init 中所有 CLAUDE.md 生成逻辑（projects.rs:313-318 render_project_claude_md 等）。
- **D2.2 `.ccteam/` 只写 state.json + workflow.yaml**。停写：spec.md、`.ccteam/agents/`（中立拷贝，0 reader）、`.ccteam/skills/`、各 `.gitkeep`。
- **D2.3 `.claude/agents/` = 核心保留（role 库）**：init 种默认 role（见 DA.2）。这是 ccteam 唯一管理的"指令面"。
- **D2.4 模板集中**：所有 init 模板（settings.local.json / workflow.yaml / 默认 cto role.md）抽到单一源码 `templates/` 目录，内联多行字符串常量清零，全部 include_str!。可读性优先（用户原始诉求）。
- **D2.5 hook 写 `.claude/settings.local.json`**（不再 `settings.json`）：ccteam 的 hook 注入本地层（gitignored，Claude settings 层级照读、与用户 settings.json 合并），**不碰用户的 `.claude/settings.json`** → 零冲突、不脏用户 git。ccteam 只 merge/清自己的 hook 段。〔落地复核 hook 在 settings.local 的 merge 语义〕
- **D2.6 slug 撞名 = 数字累加**：默认 slug = 目录名（slugify）。registry 以 slug 为寻址键须唯一。撞名（如 /workspace/demo vs /workspace2/demo）→ **数字累加**：`demo` / `demo2` / `demo3` …（弃现 `-{4hex}` 后缀，可读）。非交互可 `--slug` 显式；同一 path 重复 init = re-init 刷新，非撞名。

### 3.3 验收
- 新项目 init：不再生成根 CLAUDE.md / AGENTS.md；`.ccteam/` 只有 state.json + workflow.yaml；`.claude/agents/` 有默认 role。
- 老项目 init：CLAUDE.md / AGENTS.md 原样不动（本就不碰）。
- `rg "render_project_claude_md|DEFAULT_WORKFLOW_YAML（内联）" -g '*.rs'`：CLAUDE.md 生成逻辑删净；模板内联清零。

---

## 4. Item 3 —— 项目删除/停止（`project rm` / `project stop`，一个命令）

### 4.1 现状
CLI `ccteam remove <slug> [--purge --dry-run --force]` 成熟（commands.rs:5281；refuse 活 session 非 --force；永不删 .env）。MCP/IM/Web **零删除**。

### 4.2 决策（简化：一个命令，不铺多端）
- **D3.1 一个命令**：删除 = **`ccteam project rm <slug> [--purge --dry-run --force]`**（现 `ccteam remove` 的分组形，逻辑抽成可复用函数）。**不再** IM `@ccteam remove` / MCP / Web 各起名 —— IM 要删 = 让 **cto 跑这个命令**（cto 有工具），不另立名。
- **D3.2 停 vs 删**：`ccteam project stop <slug>` = 停该项目**所有 role-session**（kill tmux，可后续 resume；非删、非 pause）。`project rm` = 删项目（先停活 session 再删）。单 session 删（`session rm`）非本版必需，推后。
- **D3.3 活 session 语义（C）**：`stop` 本就是停（用户显式命令，不违「永不**主动** kill」）；`rm` 默认先 stop 再删，`--force` 跳过确认；`dry-run` 先列将删清单。
- **D3.4 purge 范围 = init 的逆（删 ccteam 建的，留用户的，已确认）**：`--purge` 删 → config.yaml 注册项 + `~/.ccteam/{progress,imd/registry}/<slug>` + 项目 `.ccteam/` + ccteam 在 `.claude/` 的痕迹（种的 `cto.md` + **`settings.local.json` 的 hook 段**）。**保留不碰**：用户自选 work-role（`.claude/agents/` 非 cto）、项目 CLAUDE.md/AGENTS.md、`.env`、业务代码、用户 `.claude/settings.json`。（不做"彻底清场"flag，除非你要。）

### 4.3 验收
`project rm` / `project stop` 走可复用函数；`stop` 即停、`rm` 先停再删（--force 跳确认）；--purge 只删 ccteam 痕迹（cto + settings.local hook 段），保留用户 role / CLAUDE.md / .env / settings.json；dry-run 列清单；remove_test 覆盖 rm/stop/purge 范围。

---

## 5. Item 4 —— CLI 分组重构

### 5.1 决策（ergonomic + bot 并入 session，已锁）
- **顶层扁平**：`init` · `start` · `stop` · `status` · **`config`** · `doctor`
  - **`config`（新，setup 总入口）**：交互式上下键菜单，项 = toggle(装 MCP) / 输入(IM token) / 未来更多；**吸收** 原 `doctor --install-mcp` + IM token onboarding（itemE 的 im-setup，底层 `ccteam_im::onboarding::telegram_setup()`）+ `prefs`（preferences.toml）。保留非交互 `ccteam config <key> <值>`（或 flag）给 headless/CI。`doctor` 仅留诊断/自检/修复。
- **`project`**：`ls` · `show <slug>` · `new <slug>` · `stop <slug>`（停项目全部 session）· `rm <slug> [--purge]`
- **`session`**（含 role + 原 bot config）：`ls` · `attach <slug> [role]` · `pause` · `resume` · `rm <slug> [role]` · `register …` · `unregister <slug> <role>` · `persona <slug> <role> <md>`（改 role 库的 role.md）· `add-tool <slug> <role> <tool>` · `role <slug> <sid> <role>`（绑/换 session 的 role，= 重启）
- **`internal`**（隐藏，机器/skill）：mcp-serve · hook<子> · hook-emit · probe-project · peek · progress · send · spawn · web（`prefs` 已并入 `config`）
- **直接删（pre-1.0 不留 shim）**：6 个废弃顶层别名 hook/peek/progress/send/spawn/mcp-serve。

### 5.2 old → new（关键）
`ls/show/new/remove`→`project *`；`sessions`→`session ls`；`attach/pause/resume`→`session *`；`admin register-bot/unregister-bot/list-bots/change-persona/add-tool`→`session register/unregister/ls/persona/add-tool`；`session add/ls/attach/rm`(flex)→**删**（flex EOL，§6）；`internal*/mux/web/probe-project`→`internal *`（隐藏）；`prefs`→并入 `config`；`doctor --install-mcp`→`config` 菜单项。

结果：6 扁平（init/start/stop/status/config/doctor）+ project + session + 隐藏 internal ≈ 顶层 9 个名字（现 25）。

### 5.3 验收
`ccteam --help` 顶层 8 个；废弃别名删净；旧 handler 逻辑保留仅 clap 路由重组；usage.md / tech-design 协议指针 / skills 引用同步。

---

## 5b. Item E —— skill 最小化（ccteam 自带 skill → ~0）

用户倾向：MCP + 薄代码 > skill 的不确定触发。审计现 7 个 ccteam skill，配合 session=role 新模型，**ccteam 自带 must-have skill 目标 ~0**：

| skill | 去处 |
|---|---|
| `ccteam`（NL dispatcher）| **删** —— IM 里 cto 即 NL 入口 |
| `ccteam-control` | **→ MCP**（workflow_*/admin_* 已在；change-persona = `session persona` 薄代码写 role.md）|
| `ccteam-advise` | **删 / 直接用 MCP**（advise_vote/advise_parallel 已在）|
| `ccteam-im-setup` | **→ MCP / 薄 CLI**（包 Rust `ccteam_im::onboarding::telegram_setup()`）|
| `ccteam-scan` | **→ work-role**（agency-agents 有 audit/security 角色），非 ccteam skill |
| `ccteam-team` | **推后**（属"不管"的多模式）|
| `ccteam-creator` | **折进 cto + 薄 init**（cto 荐角色 + init 种 cto/选 role + MCP I/O）|

**验收**：`skills/` 下 ccteam 自带 skill 删净/转化；`ccteam doctor --install-skill` 安装路径同步移除；原功能逐条可经 MCP 工具 / cto / work-role / 薄 CLI 达成。注：work-role（`.claude/agents` 定义，用户选的）可自带 skill —— 用户的事，不在本项。

---

## 5c. Item F —— MCP 工具最小化（省 context）

每个 MCP 工具 schema 都吃 session context；审计 28 个工具，砍掉服务于推后编排/legacy 的：
- **删 `workflow_*` 编排套件 7 个**：spawn_agent / stop_agent / observe_agents / signal / set_parallelism / trigger_gate / get_artifact_summary —— 全是推后 ccteam-flow orchestrator 的 marker 工具，v0.8.6 无 consumer（mcp_workflow_tools.rs:516-620 + dispatch；整模块可删）。session 生命周期改走 CLI（`session *`）/ IM `/role`。
- **删 `chat_reset`**：v0.8.2 gateway 不再跑 supervisor tick，信号无人消费。
- **保留**：chat 生命周期（register/unregister/list/send_input/send_file/history）+ admin（ls/change_persona/add_tool）+ workflow 查看/控制（show/peek/progress/new/pause/resume/send_to_session/inject_decision）+ screenshot + advise（vote/parallel）。
- **深砍（随 Item G 标准 API）**：API 的 project/session 资源落地后，workflow 查看/控制类可再退，MCP 降到 ~10（chat + advise + persona + screenshot）。

**验收**：删 ~8 个工具（F65×7 + chat_reset）；`STUB_TOOLS` / `doctor --verify-mcp` / 工具计数测试同步；精确计数落地按代码点准。

---

## 5d. Item G —— 标准资源 API（一次到位：全量）

把 web 现用接口抽成一套**标准资源 API**（web 现用 → 将来 app/独立端复用）。核心概念（review 后收敛，≤3 资源 + facet）：
- **project**（注册的 workspace：dir + slug + 元数据）
- **role**（可复用 persona/定义 = `.claude/agents` 库，agency-agents 填充；可列、picker 选）
- **session**（运行实例 = project × role × harness(+provider)，resume-by-id；= 用户原说的 "agent"，发 turn / 收 event 的对象）
- facet：**harness = agentic CLI 驱动**（claude-code、codex、gemini-cli、grok-cli、pi、DeepSeek-Reasonix —— **可扩展枚举**，每个一套 adapter）；**provider = 子 facet**（仅某 harness 支持多模型时有意义，主要 claude-code；多数 CLI 自带固定模型）。都是 session 属性、非顶层资源；只读 `GET /capabilities` 动态列当前可用 harness(×provider)。
- 不引入单独 "agent"（= session 重叠 + 与 Claude `--agent`=role 定义撞名）。

资源草图：`/projects`（GET/POST、GET/DELETE {slug}）· `/roles`（GET、GET/PUT {slug}/{role}）· `/projects/{slug}/sessions`（GET/POST、`/turn` POST、`/events` SSE、`/stop` POST）· `/capabilities`。现 ccteam-web api_v1 + item3 删除并入此。
- **web 侧（并入此 API）**：每 session 一个**独立视图/页**（自己的历史 + 干净切换，**不再一个 WS 页混所有 session**），消费 per-session `/sessions/{sid}/events`(SSE) + `/sessions/{sid}` 历史；UI 出 session 列表/切换器，选中只渲染该 session。与 IM 的 `/use`、`@handle` "当前 session" 语义对齐。
**一次到位（用户定）**：整套 API（资源模型 + 全端点 + SSE + 鉴权 + 版本化）进 v0.8.6，做成 app/独立端可直接集成。**v0.8.6 实现 harness=claude-code**（codex best-effort）；gemini-cli/grok-cli/pi/DeepSeek-Reasonix 逐个 adapter 后续接入、capabilities 动态加。〔A 待确认：本版跑通 claude-code 一个 harness，非 6 个全实现〕

---

## 6. 横切清理（grep-verified）

| 项 | 结论 | 处置 |
|---|---|---|
| flex（kind:flex + session add/ls/attach/rm + `.ccteam/sessions/`）| V0.3.1 遗留，无多模式后正交废弃 | **EOL 删除** |
| `.ccteam/ready` | load_context.rs:23 写，**无 reader** | **删写入 + 路径** |
| `.ccteam/spawn_requests/` + `pending-inject.json` | 仅 deferred ccteam-flow orchestrator 消费 | **live 路径不创建**；模块留 flow crate |
| `.ccteam/webhooks/` + `webhook-token` | web 路由活（lib.rs:123）但**无 consumer**（flow 未跑）| **默认删悬空路由**（flow 回来再加）—— 若你想留占位告诉我 |

---

## 7. Dev-plan（wave / worktree）
- **Wave 1 — IM session=role（主线）**：claude_tui spawn 改 `--agent <role>`、init 种默认 `cto` role、**新增 IM `/role <role>` 命令**、smoke（--agent + --name/--resume 交互+resume + hooks 触发 + change-persona 生效）。
- **Wave 2 — 目录/模板/工具清理**：item 1（~/.ccteam manifest + 停建死目录 + doctor drift）+ item 2（停生成 CLAUDE.md/AGENTS.md、`.ccteam` 只留 state.json+workflow.yaml、模板集中 + hook→`settings.local.json` + slug 数字累加）+ **Item F**（删 F65 workflow 工具 7 + chat_reset）+ §6 死文件清理 + flex EOL。
- **Wave 3 — 删除引擎 + 三端**：item 3（remove engine + session 粒度 + MCP/IM/Web）。
- **Wave 4 — CLI 分组重构 + config + skill 转化 + 文档**：item 4（clap 路由重组 + 删别名 + **新增 `config` 交互菜单**：吸收 MCP 装 / IM token / prefs）+ Item E 转化部分（im-setup→config 项、control→MCP、creator 折进 cto+init）+ tier-1 docs sync。
- **Item E（skill 最小化）跨 wave**：删除部分（`ccteam`/`ccteam-advise`/`ccteam-team` skill 文件 + `ccteam-scan`→work-role）随 W2 清理；转化部分随 W4。
- **Wave 5 — Item G 标准 API（全量，一次到位）**：project/role/session 资源模型 + `/capabilities` + 全端点（turn / events-SSE / stop / delete）+ 鉴权 + 版本化（app/独立端可集成）；重构/并入 ccteam-web api_v1。harness=claude-code 跑通，其余 adapter 后续。**本版最大 wave。**

每 wave：wave-N-handoff.md（Decided/Rejected/Risks/Files/Remaining）+ 一个 PR；baseline ≥ 上 wave；clippy 0；cargo fmt --all。

## 8. Risks
- `--agent <role> --name` 交互+resume 在 ccteam tmux send-keys 路径未实证 → Wave-1 smoke gate 优先（能力官方有，只验证我们的接法 + hook 触发）。
- CLI 重组回归面大 → 「行为不变、仅路由层重组」+ 全量 test 守。
- 删根 CLAUDE.md 生成逻辑：确认无其他路径依赖它（grep render_project_claude_md 全 caller）。
- Codex role 对齐推后 → 本版 Codex 只保证读 AGENTS.md，不强求 role 绑定；文档需写清「本版 role=Claude」。

## 9. 验收 gate（ship）
- `cargo test --workspace --exclude ccteam-web` ≥ 当前 main baseline（写本文时 1912/0，不退）。
- `cargo clippy --workspace --all-targets -- -D warnings` 0；`cargo fmt --all -- --check` 通过。
- `ccteam doctor --verify-mcp` drift 0。
- tier-1 docs 同步（CLAUDE.md §一 baseline + tech-design 协议→代码指针 + README/usage.md）。
