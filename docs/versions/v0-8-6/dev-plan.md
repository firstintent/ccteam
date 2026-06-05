# v0.8.6 Dev-Plan

> 配 `prd.md`。**worktree-per-wave + subagent 派工**；每 wave 一个 PR + `wave-N-handoff.md`（Decided/Rejected/Risks/Files/Remaining）。verify-gated：每 wave baseline ≥ 上 wave，clippy 0，`cargo fmt --all` 干净。
> Scope = full v0.8.6（A 主线 + items 1/2/3/4/E/F + G 全量 API）。harness = **claude-code 先跑通**，codex best-effort，gemini-cli/grok-cli/pi/DeepSeek-Reasonix 后续 adapter（capabilities 动态加）。

## 0. 开发顺序 + 理由（给新 dev session）
**本文档供 dev session 执行；设计/meta session 不写代码。** 起手读 `prd.md` 全文 + 本文；每 wave = 独立 worktree（`git worktree add -b v086-wN /tmp/ccteam-wN origin/main`）+ 一个 PR + `wave-N-handoff.md`（Decided/Rejected/Risks/Files/Remaining）；verify-gated：baseline ≥ 上 wave、clippy 0、`cargo fmt --all` 干净，**不过测试不算完成**。

**顺序：W1 → W2 → W3 → W4 → W5 → W6。理由：**
1. **W1 session=role 先做（de-risk + 头部价值）**：`claude --agent <role>` 在我们 tmux send-keys + resume 路径能否跑通是**第一风险**（能力官方有，接法未实证）—— 先用 smoke gate 验掉；万一不成立整个模型要改，越早越好。且 W1 完即可 IM 里跟 cto 对话 = 最快出价值。（考虑过"清理先行"，但 de-risk 基石优先级更高。）
2. **W2 清理紧跟（省上下文 + 干净地基）**：item1/2/E/F + flex + 死文件全是删除/简化、低风险；**砍掉 skill/MCP/死目录后，后续 W4/W5 的 dev session 上下文更省**（用户明确诉求），且在干净 init/paths 上建。
3. **W3 删除/停止**：依赖 W1 的 session 模型（`project stop` 停全 session）。
4. **W4 CLI 分组 + config**：等 rm/stop/config 功能就位，再把命令面收成最终分组（行为不变、仅路由重组）。
5. **W5 标准 API + web 最后（最大件）**：依赖 W1（session/role）+ W3（rm/stop）+ W4（config/CLI）+ W2（瘦 MCP）；含鉴权/版本化/SSE/per-session web，单独最大 wave，可拆子 PR。

6. **W6 全文档重写最后（大改动）**：v0.8.6 是架构级改动，tier-1 文档**全量重写**到新模型（非增量 sync）；放最后，docs 反映已落地的真实代码。

**依赖图**：W1 基石；W2 独立（可与 W3 并行，各自 worktree）；W3→W4→W5 偏线性（W4 用 W3 命令，W5 用全部）；W6 收尾。
**⚠ 文档 SoT（过渡期）**：W1–W5 期间 **`prd.md` + 本 dev-plan 是 v0.8.6 架构 SoT**；`CLAUDE.md` / `tech-design.md` / `README.md` / `usage.md` 旧文档**仍描述旧架构、stale，别依赖**，统一由 W6 重写。

---

## Wave 1 — IM session = role（keystone）
**目标**：chat session 以 role 启动，默认 `cto`；IM 可换 role。
**改动**：
- `ccteam-harness/src/execution/claude_tui.rs`：`spec_for_new` / `spec_for_resume` argv 加 `--agent <role>`（role 来自 `spec.role`）。
- init（`ccteam-cli/src/commands.rs` + `ccteam-core` templates）：种 `.claude/agents/cto.md`（默认 cto persona，内容见 prd DA.2）替代 explorer.md scaffold。
- `ccteam-im/src/gateway.rs`：默认 role `"assistant"`→`"cto"`（`/new` 默认 + `ensure_current_session` 默认 spawn）；新增 `/role <role>` 命令（换当前 session role = 用新 `--agent` 重启）。
**验收（smoke gate）**：
- deterministic fake claude 断言 `--agent <role>` 在 new/resume argv（扩 `claude_tui_resume_test.rs`）。
- `/role` 切换：argv 带新 role，重启该 session。
- chat-progress hooks 在 `--agent` session 下仍触发（smoke）。
- 改 cto.md → 重启后行为变（change-persona 生效）。

## Wave 2 — 目录/模板/工具清理
**目标**：init 瘦身 + `~/.ccteam` manifest + skill/MCP/死文件清除。
**改动**：
- **item1**：`ccteam-core/src/paths.rs` canonical-layout manifest；init 停建 phases/queue/memory/control/templates；`doctor` home-drift 检查；`state/orchestrator.pid`→`daemon.pid`、heartbeat 删。
- **item2**：删 `render_project_claude_md` 等 CLAUDE.md/AGENTS.md 生成；`.ccteam/` 只写 state.json+workflow.yaml（停 spec.md / `.ccteam/agents/` / skills / gitkeep）；hook → `.claude/settings.local.json`；slug 数字累加（`pick_unused_under_team_prefix` 后缀 → demo2/demo3）；模板集中到单一 `templates/`。
- **itemE**：删 skill 文件 `ccteam` / `ccteam-advise` / `ccteam-team`（`ccteam-scan`→work-role）；`skill.rs` install 路径同步。
- **itemF**：删 `mcp_workflow_tools.rs`（F65 7 工具）+ dispatch；删 `chat_reset`；工具计数测试 + `doctor --verify-mcp` 同步。
- **§6 死文件**：停写 `.ccteam/ready`（load_context.rs）；spawn_requests/pending-inject/webhooks live 路径不创建（webhook 路由删，lib.rs:123）；flex EOL（kind:flex + session add/ls/attach/rm + `.ccteam/sessions/`）。
**验收**：全新 init 后 `~/.ccteam` + 项目 `.ccteam/` 干净；skill/MCP 计数降；baseline 不退；grep 确认死文件 writer/reader 全清。

## Wave 3 — 删除/停止
**目标**：`project rm` + `project stop`。
**改动**：
- `run_remove` 核心抽可复用函数；`project rm`（= 现 remove 分组形，`--purge` = init 逆：cto.md + settings.local hook 段 + `.ccteam/` + config 注册 + `~/.ccteam` state；**保留**用户 role/CLAUDE.md/.env/settings.json）。
- 新增 `project stop <slug>`：停项目所有 role-session（kill tmux，resumable）。
**验收**：rm/stop 走可复用函数；purge 范围正确（保留用户 role 实证）；dry-run 列清单；`remove_test` 扩展覆盖 rm/stop/purge。

## Wave 4 — CLI 分组 + config + skill 转化
**目标**：CLI 收成 ~9 顶层 + `config` + skill→MCP/CLI。
**改动**：
- clap 路由重组：flat（init/start/stop/status/config/doctor）+ project/session 组 + 隐藏 internal；删 6 废弃别名；old→new（prd §5.2）。
- `config` 交互菜单（上下键：装 MCP / IM token / prefs；非交互 `config <key> <val>`）；吸收 `doctor --install-mcp` + im-setup（Rust `telegram_setup()`）+ prefs。
- itemE 转化：control→MCP（已在）；creator 折进 cto + init。
- tier-1 docs sync。
**验收**：`ccteam --help` 顶层 ~9；废弃别名删净；config 交互 + 非交互都跑；usage.md/tech-design/skills 同步。

## Wave 5 — 标准 API 全量（最大）
**目标**：project/role/session 资源 API + capabilities + SSE + 鉴权/版本化；per-session web 页；harness=claude-code 跑通。
**改动**：
- `ccteam-web`：重构 api_v1 → 标准资源（`/projects`、`/roles`、`/projects/{slug}/sessions{,/turn,/events SSE,/stop}`、`/capabilities`）；鉴权（web-token + 版本前缀）；item3 删除并入 DELETE。
- harness=claude-code adapter 接 session 资源；`capabilities` 动态列（claude-code now；codex best-effort；余 future）。
- web UI：每 session 独立视图/页 + 切换器（消费 per-session SSE + 历史；不再一个 WS 页混所有 session）。
- **itemF 深砍**：API 落地后退掉 workflow 查看/控制类 MCP（show/peek/progress/new/pause/resume/send/inject）→ MCP ~10。
**验收**：API 端点全通（集成测试）；per-session web 页独立切换；capabilities 准；鉴权；MCP 深砍后计数；`ws_*` 测试留 CI/专机。

---

## Wave 6 — 全文档重写（大改动，全量非增量）
**目标**：v0.8.6 是架构级大改动（session=role / IM-universal / 无多模式 / cto / 新 CLI / 标准 API），tier-1 文档**全量重写**到新模型，不是增量 sync。
**改动（逐份重写）**：
- `CLAUDE.md`（起手必读，≤200 行）：§0 架构红线、§一 状态/baseline、§三 红线、§四 扩展（skill→~0、MCP 瘦身）、§五 —— 按 session=role / cto / harness×provider / 标准 API 重写；删 orchestrator-era / 多模式 / flex 残留术语。
- `docs/tech-design.md`（架构 SoT）：重写 —— session=role via `--agent`、harness×provider、3 资源 API（project/role/session）+ capabilities、config、删除/停止模型；移除 orchestrator/flow-era 描述；刷新「协议→代码」指针表。
- `README.md`（英文产品入口）：重写到新能力（IM-universal + role 库 + agency-agents + 标准 API），**不含**版本进展/baseline。
- `docs/usage.md`（命令手册）：重写 —— 新 CLI（init/start/stop/status/config/doctor + project/session 分组、project rm/stop）、IM 命令（/pair /cd /use /new /role @handle @ccteam）、启动全流程（prd §0.4）。
- `skills/` + `.claude-plugin`/`.codex-plugin`/`.mcp.json`：删掉的 skill（item E）从 marketplace/manifest 摘除；MCP 工具列表按 item F 同步。
- `docs/requirements.md`：核对痛点（基本稳定，补新增 IM-universal/标准 API）。
- 版本归档 `docs/versions/v0-8-6/README.md`（里程碑）+ 各 `wave-N-handoff.md` 收尾。
**验收**：tier-1 文档全反映新架构、零 orchestrator-era/多模式/flex/AGENTS.md-替代 残留；`doctor --verify-mcp` 工具名与文档一致；README 英文无版本进展段；CLAUDE.md ≤200 行；skill ship-gate grep 0 命中。

**CLAUDE.md 现有红线冲突清单（W6 必改 —— 2026-06-05 审计）**：
- §三「跨项目记忆…`ccteam init` 落 AGENTS.md → CLAUDE.md symlink」→ **删**：item2 ccteam 不生成/接管项目 CLAUDE.md/AGENTS.md（vendor 原生，项目自有）。
- §〇「`ccteam init` 布局 `.ccteam/{agents,skills,state.json}`」→ **改**：item2 `.ccteam/` 只 state.json+workflow.yaml；agents 拷贝/skills/gitkeep 停建。
- §三「新建项目走 `<projects_root>/<team>-<slug>/`、`pick_unused_slug` 强制 team 前缀」→ **改**：D2.6 slug=目录名+数字累加（demo2/demo3）；`ccteam init` 可在任意现有目录就地。
- §四 Skills 表（7 个）→ **改**：itemE skill→~0（功能落 MCP/cto/work-role/薄 CLI）。
- §四 + §一「27 个 MCP 工具」→ **改**：itemF 砍到 ~20（深砍随 API ~10）。
- §〇/§一「核心概念 chat⇄project⇄session」→ **扩**：+role（session=role）。
- §三「永不主动 kill」→ **补例外**：`project stop` / `rm --force` = 用户显式命令（非主动 kill）。
- §六「hook 在 `.claude/settings.json`」→ **改**：D2.5 hook 写 `.claude/settings.local.json`。
- §〇 标题「v0.8.3/v8.3 当前架构」+ §一 baseline/「当前在做 v0.8.5」→ **改**：更新到 v0.8.6。
**未冲突（W6 保留/强化）**：No-prompt-injection（`--agent` 让 vendor 自读 role.md，正是这条的兑现）、`progress.jsonl` SoT、不 scrape pane、resume-by-id、core→harness→cost、README 英文无版本进展、skill 自洽红线、不 vendor binaries。

---

## Ship gate（prd §9）
`cargo test --workspace --exclude ccteam-web` ≥ 当前 main baseline（写本文时 1912/0，不退）；clippy 0；`cargo fmt --all -- --check`；`doctor --verify-mcp` drift 0；**tier-1 docs 全量重写（W6，非增量 sync）**；workspace version → 0.8.6 + tag。

## Risks（详 prd §8）
- W1 `--agent <role> --name/--resume` 在 tmux send-keys 路径未实证 → W1 smoke gate 优先（能力官方有，验我们的接法 + hook 触发）。
- W4 CLI 重组回归面大 → 「行为不变、仅路由层重组」+ 全量 test。
- W5 API 是最大件（鉴权/版本化/SSE/per-session web）→ 单独最大 wave，可能需拆子 PR。
- 删 CLAUDE.md 生成 / F65 工具 / flex / webhook → 落地前 grep 全 caller 确认无 live 依赖。
