# v0.8.6 — IM 通用模式(session = role)+ 目录/CLI 收敛 + 标准资源 API

> 冻结版本归档。本目录 = v0.8.6 的完整开发记录:`prd.md`(为什么 + 决策 + 验收)、`dev-plan.md`(wave 顺序 + 理由)、`wave-1..5-handoff.md`(各 wave 的 Decided/Rejected/Risks/Files/Remaining)。本 README 是里程碑索引 —— 一版交付了什么、新模型是什么、baseline、推后项。

## 一句话

v0.8.6 是**架构级**改动:把 ccteam 从 orchestrator-era 的「多模式编排」收敛成 **IM 通用模式** —— 一个 chat/IM session 就是一个**绑定 role 的 agent 会话**(`claude --agent <role>`),daemon 是纯路由网关(不 tick、无 orchestrator 循环),并一次到位落地一套**标准资源 API**(project / role / session + capabilities)。同时大幅瘦身:CLI 命令面、`~/.ccteam` 目录、bundled skill、MCP 工具全部收敛。

## 新模型(替代旧 v8.3 orchestrator-era 描述)

核心概念从 `chat ⇄ project ⇄ session` 扩成 **`chat ⇄ project ⇄ session ⇄ role`**。

- **session = role**:chat/IM session 以 `claude --agent <role> --name|--resume <sid>`(tmux send-keys 路径)启动,session 即 become 该 role;hooks 照常触发(含 `Stop` → `chat_turn_completed`)。role 库 = 项目级 `.claude/agents/<role>.md`。`ccteam init` 种默认 role **`cto`**(chat-first 管家:懂 ccteam、**推荐** work-role;本版只推荐,用户用 `/role` 自己切)。
- **daemon = 纯路由网关**:IM gateway + web + MCP unix socket,**无 tick、无 orchestrator 循环**;session = spawn-on-demand + resume-by-id + 空闲释放。
- **harness × provider facet**:harness = agentic CLI 适配(本版 **claude-code** 跑通;codex best-effort;gemini-cli / grok-cli 等后续 adapter,`AgentVendor` 可扩展枚举),provider = 子 facet(model)。经 `GET /capabilities`(PATH 探测)动态暴露。
- **标准资源 API(`/api/v1`,web-token 鉴权)**:project(`GET/POST /projects`、`GET/DELETE /projects/{slug}`,DELETE = 注销 + 停 session,file-purge 留 CLI)、role(`GET /projects/{slug}/roles`、`GET/PUT /projects/{slug}/roles/{role}`)、session(`GET/POST /projects/{slug}/sessions`、`GET /sessions/{sid}`、`POST /sessions/{sid}/turn`、`GET /sessions/{sid}/events` SSE、`POST /sessions/{sid}/stop`)、`GET /capabilities`。session-id = gateway `s{n}`。
- **项目知识层归 vendor**:ccteam **不生成、不修改、不抑制** `CLAUDE.md` / `AGENTS.md`(Claude 读 CLAUDE.md、Codex 读 AGENTS.md,都归项目自己)。ccteam 唯一管理的「指令面」= role 层(`.claude/agents/`)。

## 各 wave 交付(W1–W6)

| Wave | 主题 | 交付 |
|---|---|---|
| **W1** | IM session = role(keystone) | spawn argv 加 `--agent <role>`(new/resume);init 种默认 `cto.md`(`ccteam_core::CTO_ROLE_MD` 单一源);gateway 默认 role `cto` + 新增 IM `/role <role>`(原地换角色 = 带新 `--agent` 重启,保持同一 sid)。**头号风险**(`--agent` 在 tmux send-keys + resume 路径)已用真实 claude smoke 实证:交互可答、`--resume` 恢复同一 session、hooks 全触发。|
| **W2** | 目录 / 模板 / 工具清理(deletion wave) | `~/.ccteam` 停建 orchestrator-era 死目录(queue/memory/log…)+ `canonical_home_dirs()` 单一布局 manifest + doctor home-drift;init 停生成项目 CLAUDE.md/AGENTS.md(删 `render_project_claude_md`),`.ccteam/` 只留 state.json+workflow.yaml,hook 写 `.claude/settings.local.json`(不碰用户 settings.json),slug 撞名数字累加(demo/demo2/demo3);删 7 个 F65 `workflow_*` MCP 工具 + `chat_reset`(28→20);删 skill `ccteam`/`ccteam-advise`/`ccteam-team`/`ccteam-scan`;停写 `.ccteam/ready`、删悬空 webhook 路由。flex **CLI** 删除(类型 EOL 推 W5)。|
| **W3** | 删除 / 停止 | `ccteam project rm <slug> [--purge --dry-run --force]`(复用 `run_remove` 引擎;`--purge` = init 逆:`.ccteam/` + 种入 cto.md + settings.local hook 段 + config 注册 + per-slug 状态,**保留**用户 role/CLAUDE.md/AGENTS.md/.env/settings.json)+ `ccteam project stop <slug>`(停项目全部 chat session,dash-aware tmux 枚举,resumable)。附 purge-preservation 审计(实跑 CLI 证明用户文件不被删)。|
| **W4** | CLI 分组重构 + config + skill → ~0 | clap 路由重组:扁平 `init/start/stop/status/config/doctor` + `project` 组 + `session` 组 + 隐藏 `internal`;**删 6 个废弃顶层别名**(hook/peek/progress/send/spawn/mcp-serve);新 `config` setup hub(交互菜单 + `config mcp`/`config <key> <value>`/`get`/`show`,吸收 `doctor --install-mcp` + IM token onboarding + prefs);删 doctor 的 `--install-*` 系列;删最后 3 个 bundled skill(control/creator/im-setup)→ **0 bundled skill**(留 `skills/.gitkeep`)。|
| **W5** | 标准资源 API(最大 wave)+ flex EOL + MCP 深砍 | flex 类型全删(core + web:`TeamKind::Flex`/`SessionRecord`/`ProjectState.sessions`…);MCP 深砍 20→**12**;`/api/v1` 标准资源 API(project/role/session + capabilities + SSE + 鉴权 + 版本化)+ gateway spine(`SessionView`/`session_views`/`create_session_api`/`submit_to_sid`/`stop_session`,`Arc<Mutex<Gateway>>` 注入 web AppState,acyclic web→im);真实 HTTP smoke 全过;SPA flex 清理。|
| **W6** | 全文档重写(本 wave) | tier-1 文档全量重写到新模型(非增量 sync):`CLAUDE.md`(≤200 行,§0/§一/§三/§四/§五)、`docs/tech-design.md`(架构 SoT + 协议→代码指针)、`README.md`(英文产品入口,无版本进展)、`docs/usage.md`(新 CLI + IM 命令 + 启动全流程 + `/api/v1`)、manifests(skill 摘除 + MCP 12 同步)、本版本归档。workspace version → 0.8.6。|

## Baseline(ship gate)

- `cargo test --workspace --exclude ccteam-web` = **1861/0**(`ccteam-web` ws_* env-gated 测试留 CI/专机)。
- `ccteam-web` = 230 pass / 5 env-gated `ws_*`(sandbox 不能流 PTY,非回归)。
- `cargo clippy --workspace --all-targets -- -D warnings` = **0**(含 web)。
- `cargo fmt --all -- --check` 干净。
- `ccteam doctor --verify-mcp` = **12 工具**,drift 0。
- workspace version → **0.8.6**。

> baseline 轨迹(deletion-heavy 版本):W1 1913 → W2 1865 → W3 1881 → W4 1864 → W5 1848 → 1861/0(降幅逐 wave 对账;删 legacy 测试 + 加新功能测试的净值)。

## 红线(保留 + 兑现)

保留:`progress.jsonl` 是 state SoT(`harness/progress_bridge` 单一权威)、不 scrape pane(读 transcript jsonl + hooks)、resume-by-id、永不**主动** kill 长 session(**例外**:`project stop` / `rm --force` 是用户显式命令)、`ccteam-core` 零 team 名字面量、crate 拓扑 `core→harness→cost`、root README 英文且无版本进展、skill 自洽红线、不 vendor claude/codex 二进制。

**兑现**:no prompt injection —— `--agent` 让 vendor **自读** role.md,这条红线现在是**被满足**(而非被违反):ccteam 不向 pane / app-server 注入 system prompt,role persona 经 vendor 原生 `--agent` 机制加载。

## 推后 / Deferred(本版未做,记录在案)

- **ccteam-flow orchestrator**:仍存在但 daemon **不运行**;多模式(agent-team / 自治 bg)编排推后。
- **Codex role 对齐**:本版 Codex 只保证读项目原生 `AGENTS.md`;role 绑定(`--agent` 等价机制)推后。
- **per-session web UI 改造**:`/api/v1` 端点已 live + smoke 过;每 session 独立视图/页 + 切换器(消费 per-session SSE)是已记录的**前端后续项**(旧 SPA 会话页仍走旧 project-sid 路径)。
- **work-role import/picker UI**:本版选 role = 手动丢 `.md` + IM `/role`;picker/import 推后下版。
- **单 session 粒度删**(`session rm <slug> <role>`):非本版必需,推后。
- **MCP 再深砍到 ~10**:本版到 12(admin 3 + chat 6 + advise 2 + screenshot 1);workflow 查看/控制类已随 API 落地退役。
- 低优 chore:`state/orchestrator.pid` → `daemon.pid` + heartbeat 删(D1.4);init 模板 inline const → include_str! 集中(D2.4);doctor `--gc-home`;`SessionDetail.tsx` 的 `isFlex` 死分支(无害)。

## 已删除(GONE)

flex(`kind:flex` + `session add/ls/attach/rm` + `.ccteam/sessions/` + `SessionRecord`)、7 个 F65 `workflow_*` MCP 工具 + `chat_reset`、4+3 bundled skill、项目 CLAUDE.md/AGENTS.md 生成、webhook 路由、`.ccteam/ready`、6 个废弃顶层 CLI 别名、`doctor --install-mcp/--install-skill/--install-meta-agent/--install-all`、`ccteam prefs` 顶层命令。
