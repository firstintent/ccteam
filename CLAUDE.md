# CLAUDE.md — ccteam 实现导引

> 本文档面向**下一次接手 ccteam 实现的 Claude session**。项目已过初创期,
> 核心里程碑 M0–M4 全部 ship,代码已上规模。本文是 session 起手的第一份导引。

---

## 一、当前状态(2026-05-16)

| 项 | 值 |
|---|---|
| 仓库根 | 本会话 `cwd` 即是;显式查用 `git rev-parse --show-toplevel` |
| 主分支 main HEAD | **以 `git rev-parse origin/main` 为准**(V0.4.6 ship 后) |
| Workspace version | **`0.4.6`**(V0.4.5 + 11 个 finding F81-F91 — lifecycle / 用户痛点根治 / 运维收敛) |
| 测试 baseline | **`755/1`**(`cargo test --workspace --locked` 仅 ccteam-web 端口绑定 flake;V0.4.6 +30 vs 725 baseline) |
| Clippy | 0 errors + ~20 warnings(pre-existing doc-list drift,sweep 时确认相同基线) |
| 代码规模 | ~17 kLOC Rust + ~14 kLOC TypeScript |
| 已 ship 里程碑 | **V0.1-V0.3.2** 见各自 `docs/v0-X-Y/README.md`<br>**V0.4.0**:F60-F69(phase 全删 + workflow.yaml + artifact watcher + 17 MCP 工具 + thin orchestrator + WorkflowView SPA)<br>**V0.4.1**:UX 简化 patch(start 合并 web、send/spawn CLI、handle 干掉、daemon hot-reload、mcp 退出 deadlock fix)<br>**V0.4.2**:F72-F75 — `ccteam init` 三合一 + `~/.ccteam/config.yaml` 全局 SoT + `doctor --migrate-v041-to-v042` + `ccteam new` thin wrapper<br>**V0.4.3 hotfix**:F76 slug grammar validation + 优化 collision wording<br>**V0.4.4 hotfix**:F77 — `session_context_from_cwd` walk-up + `paths.project_dir(slug)` 走 config.yaml registry<br>**V0.4.5 hotfix**:F78 watcher 项目相对路径 + F80 phantom `agent_spawn` 清理<br>**V0.4.6**:F81-F91 — F81 `ccteam remove` lifecycle + F82 workflow.yaml enabled+热加载(cancel token) + F83 workflow.yaml 移到 `.ccteam/` + F84 budget cap(max_cost_usd_per_24h) + F85 `~/.claude/jobs/` GC + F86 daemon graceful shutdown + F87 clap allow_hyphen_values + F88 web bearer token clipboard + F89 CLI 瘦身(internal 分组) + F90 Web WorkflowView 4 新面板 + F91 cost SoT 收敛到 claude state.json |
| 当前 next | V0.4.7 候选:F92 真 cost 数据源(linkScanPath jsonl aggregation,state.json 没有 cost_usd 字段);Codex CLI argv 标准化(F62 推迟);Codex bg-job 形态;workflow.yaml 条件分支;`schedule` 真实 cron;WSL inotify flake 根治 |
| 永久 deferred | M2.2 agent_team enablement(spike A,Claude Code 无 first-class CLI surface — 见 `docs/v0-1/m2-agent-team-spike.md`)|

**ccteam 是 Claude Code 之上的元工具,不是独立 AI 系统**(V0.4.0 后):每个项目用 `workflow.yaml` 声明 agent 拓扑(**无 prompt,只有连线 + trigger 类型 + 并发上限**),`.claude/agents/<role>.md` 定义每个 agent 行为;Rust orchestrator(binary 名 `ccteam`)读 workflow.yaml,通过 `ArtifactWatcher`(inotify/fsevents)监听文件系统控制平面 → 按 parallelism spawn Claude Code(`claude --bg --agent <role>`)或 Codex(tmux+codex)session,`progress.jsonl` 记录 7 类业务 event(workflow_start / agent_spawn / agent_done / artifact_received / gate_triggered / budget_exceeded / workflow_done + escalation);用户通过 meta-agent(常驻 ccteam-managed claude session)+ `ccteam-control` skill / `ccteam` MCP server(**17 个 `mcp__ccteam__*` 工具**)用自然语言对话操作;web UI(`ccteam web`,`crates/ccteam-web`)提供 WorkflowView(agent cards + artifact counts + gate 解锁)+ SSE live updates。详见 `docs/v0-4-0/prd.md` 完整架构哲学 + `docs/tech-design.md` §2.1 三层架构。

---

## 二、必读文档(按推荐顺序)

| # | 文档 | 何时读 |
|---|---|---|
| 0 | `docs/README.md` — 全局文档索引 + 维护规约 | 加 / 改 / 归档文档前 |
| 1 | `docs/requirements.md` — 13 痛点 | 验收基准;PR 描述映射用 |
| 2 | `docs/tech-design.md` — 架构 SoT | 改架构前必看;§3.7 Cross-project Memory / §3.8 用户接口层 / §6 扩展点 |
| 3 | `docs/interfaces.md` — 协议参考 | 改 schema / CLI / MCP / hooks 时同步 |
| 4 | `docs/v0-2/README.md` — V0.2 文档入口 + V0.3 deferred 列表 | 当前最新版,V0.2 ship 状态 / V0.3 候选 |
| 5 | `docs/v0-1/README.md` — V0.1 历史归档入口 | 看 V0.1 决策依据(M0-M4.4)|
| 6 | `docs/dev-coupling-audit.md` — F-finding 解耦审计 | 改 `ccteam-core` 之前;新发现加 F<N> |
| 7 | `docs/ccteam-as-domain-agnostic-orchestrator.md` — 团队泛化论证 | M5+ 加新 team / 改 `ccteam-core` 红线时 |
| 8 | `docs/claude-code-best-practices.md` | 改 phase prompt / hooks / context 管理时 |
| 9 | `docs/claude-code-tool-surface.md` | 改 phase YAML `tools_required` / sub-skill 时 |
| 10 | `references/research/claude-code-memory-research.md` §六 | M4 任何记忆相关改动前 |

> **session 起手 30 秒 onboarding**:`git rev-parse origin/main` 看 HEAD → `cargo test --workspace 2>&1 \| grep -E "^test result" \| awk '{p+=$4;f+=$6}END{print p,f}'` 看 baseline → 读 `docs/v0-2/README.md` 看当前版状态 + V0.3 候选 → 读用户的具体诉求 → 干。

> **对照参考**(本地 clone,**`/references/` 已 gitignore**,不入库):`references/claude-code/`(Anthropic Claude Code 源码,bun + TypeScript)+ `references/codex/codex-rs/`(OpenAI Codex CLI Rust workspace)。做 HarnessAdapter / 协议适配 / hook 兼容性 / statusline-stdin shape 验证 等接口工作时翻;**不要把这两份源码当 ccteam 自己的依赖**(永久 deferred)。

---

## 三、不可触碰的架构红线

来自 `docs/tech-design.md`,任何 PR 不得违反:

- **tmux 长 session,不用 `claude -p`** — cache 复用 + 随时 attach + detach 即守护(§2.2、§6.1;最佳实践 §7.2)
- **文件系统是控制平面** — 不接 Linear/GitHub Issues 作状态源(§2.2)
- **`progress.jsonl` 是 orchestrator 唯一状态事实来源** — 不解析 tmux 终端输出(§5.5、§6.8)
- **默认 1M context,超 60% 在 phase 边界 reset** — `/exit` + 新 session + CLAUDE.md 桥接,**不**用 `--resume`(§6.9)
- **idle-aware 注入**:`Stop`/`SubagentStop`/`idle_prompt` 后 send-keys;忙时用 `/btw`(§6.9)
- **永不主动 kill 长 session** — 只软告警(5/15/30 min);唯一例外:项目累计 cost > $200 物理上限(§6.8)
- **`--dangerously-skip-permissions` + 项目级容器** — 产出项目专用,**不**等同本仓的 `bypassPermissions`(§6.1)
- **fix-loop 撞 3 次顶必 escalate,绝不静默重置**(§3.5)
- **M4 跨项目记忆 → ccteam-core 零检索代码**:全部经 Claude session 内官方接口(`/memory` / `Edit ~/.claude/rules/...`)完成,不写程序读 memory 文件;主路径走官方 `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` + per-repo auto-memory(详见 tech-design §3.7)
- **claude-mem 是严格可选增强**:ccteam 不写检测/集成代码,phase prompt 写 conditional "如有 `mcp__*claude-mem*search` 工具则可调",**LLM 自看 tool surface 决定**;用户没装则 100% 走默认路径
- **retro 写 `~/.claude/rules/ccteam-lessons-<team>.md` 必须限 marked section**:`<!-- ccteam-managed:lessons begin/end -->` 包裹,不污染用户其他段;phase prompt 严格约束,doctor `--install-memory-bridge` 只重写自己段(幂等)
- **新建项目目录走 `~/projects/<team>-<slug>/` 约定**(F22 后):`pick_unused_slug` 强制 team 前缀;meta-agent 仍 `<handle>-meta` 后缀(独立路径)
- **`ccteam-core` 不出现 team 名字面量**:strategic doc §3 红线;团队特定行为靠 `team.yaml` 数据驱动

---

## 四、扩展机制速查(详见 tech-design §6)

| 机制 | 用途 | 文档 |
|---|---|---|
| **CLAUDE.md** | 项目级 / 用户级持久指令 | best-practices §4.1 |
| **Skills** | `ccteam-control`(M1.8 ✅)/ `ccteam-team-author`(M0.22 ✅)/ `ccteam-project-creator`(V0.2.2 F34 ✅)/ `ccteam-creator`(V0.4.4 ✅,workflow + agent + skill 创建 dialogue 指引,引用官方 spec 不复制)/ `ccgram-messaging`(M3+ 多 orchestrator);自带 skill SoT 在 repo 根 `skills/` | tech-design §6.7 |
| **MCP** | `ccteam-mcp`(M2 ✅,9 tools)/ `claude-mem`(M4 可选) | tech-design §6.4 |
| **Subagents** | phase 内 `Task(subagent_type=...)` 节流;`code-reviewer` 等 8 个 plugin agent 已 ln -sf | best-practices §6.3 |
| **Hooks** | `progress.jsonl append` / `parse-phase-end` / `cost-accumulate` 已 ship;`Stop`/`SubagentStop`/`SessionEnd` 都进 idle 列表(F1+F2 修复) | tech-design §6.2 |
| **Plugins / Marketplaces** | `~/.claude/plugins/marketplaces/claude-plugins-official/`;按需 ln -sf,**不 vendor**(§3.7 检查清单) | tech-design §3.7 / §6.10 |

---

## 五、PR / 实现纪律

1. **每个 PR 描述必须映射**:
   - `requirements.md` §二某条痛点(例:`痛点 4`)
   - `tech-design.md` 某章节(例:`tech-design §3.5`)
   - `docs/v0-1/development-plan.md` 某条任务(例:`Closes M4.1`)/ `dev-coupling-audit.md` 某条 F-finding
   - 改协议(YAML 字段、JSON shape、文件路径、CLI 签名)→ **必须同步 `interfaces.md`**
2. **commit message 用英语**;文档与 phase prompt 用中文
3. **不写 backwards-compat shim**;`ccteam-core` 不写废弃代码 stub;CLAUDE.md §五.3
4. **优先编辑现有文件,不轻易新建**;phase 模板优先 `@~/.claude/plugins/.../<file>` 引用而非复制
5. **测试不过不算完成**;`cargo test --workspace` 退步 = block;clippy 不能新增 warning
6. **大需求时让 Claude 反向面试自己**(plan-eng phase template 已实现机制)

### 多 session 并行编辑同一仓库

主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/main` 起独立工作树,完事 `git worktree remove`。**主仓 main 不变 dirty**。

跨 session 见主仓 dirty 状态:先 `git stash push -m "<owner-session> WIP"` 再切;别盲目 `git checkout -- .`。

### Patch 版本(V0.x.y)开发流程

1. **doc-first**:PRD + dev-plan 落 `docs/v0-x-y/`;用户 review 后才动代码
2. **worktree-per-PR**:每个 finding 单独 `git worktree add -b <branch> /tmp/ccteam-<name> origin/main`
3. **subagent 派工**:主 session 用 Agent 工具派每个 worktree(briefing 含 PRD section + 验收条目)
4. **PR review/fix/merge**:主 session 拉 PR diff review → 退回 fix 或本地补 → merge
5. **cargo bump**:`workspace.package.version` 同步 bump,commit subject 用 `vX.Y.Z:` 前缀
6. **CLAUDE.md baseline 更新**:`cargo test --workspace` 通过新数后回填 §一表格

(主仓 main 不变 dirty;worktree 工具流详上节"多 session 并行编辑同一仓库")

---

## 六、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱。本仓库用 Claude Code 默认行为开发,只产出物(`~/projects/<team>-<slug>/`)挂 ccteam 自己的 hook
- **`.claude/settings.json` 的 `bypassPermissions` 是开发态便利** — 产品形态是 `--dangerously-skip-permissions` + 容器隔离,语义不同(best-practices §4.2 三选一)
- **phase prompt 别写太长** — 单条 send-keys 装得下;复杂内容用 `@文件引用`(best-practices §3)
- **`claude-plugins-official` 是参考实现,不是依赖** — 别 vendor 一份;按 §3.7 三粒度选(@引用 / 拷贝改 / 整 plugin install)
- **测试调 `bootstrap_project` / `bootstrap_meta_project` 之前必须先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` + `~/.claude/agents/` 写垃圾,长期撑大 `.claude.json` 会破坏 claude 登录(2026-05-06 实测)
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),不放 lib `#[cfg(test)] mod tests`;同 binary 内其他测试读 env 会 race
- **多 session 并行编辑同一仓库** → 用 `git worktree`,主仓不动
- **跨 session 协作时见到主仓 dirty 状态** → `git stash push -m "<owner-session> WIP"` 再切,别盲目 `git checkout -- .`
- **改了 `ccteam-core` 公共 API**(如 `pick_unused_slug` 签名)→ grep 全 caller(包括 tests / mcp_serve.rs / commands.rs)
- **F22 后 slug 带 team 前缀**:`run_new` test 期望 `dev-<base>` 而非 `<base>`;改新 slug 路径时验证 rules `paths:` 还匹配
- **V0.1 → V0.2 升级一次性迁移**:M0.20 后 plugin agent 通过 spawned session `enabledPlugins` 启用,不再 ln -sf 进 `~/.claude/agents/`。V0.1 用户首次升级 V0.2 时跑 `ccteam doctor --migrate-recommended-agents` 清理旧 ln -sf(只删 ccteam 自己创建的 marketplace symlink,用户手写 agent 不动)
- **V0.2.2 (F39) → V0.2.2 (F44) 反向迁移**:用户从 F39'd V0.2.2 升级到 F44'd V0.2.2 时,`ccteam doctor` 自动检测 `~/.local/bin/cct` 旧 symlink、`~/.claude/skills/cct-{control,team-author,project-creator}/` 旧 skill dir(marker 校验,只清 ccteam-managed 的;用户手改保留 + warn)、`~/projects/<slug>/.claude/settings.json` 老 hook command 路径(`cct` → `ccteam`,原子写)。原因:F39 选 `cct` 二进制名时未检查 namespace,Ubuntu `proj-bin` 已占 `/usr/bin/cct`(PROJ 工具),`~/.local/bin/cct` 在标准 PATH 上会静默 shadow GIS 工具
- **V0.3 → V0.3.1 升级一次性迁移**:`team.yaml::kind` default `workflow` 保老 yaml 不破;flex state.json 新增 `sessions{}` / `next_sid_seq{}` serde default;flex progress 走 `~/.ccteam/progress/<slug>/<sid>.jsonl`,workflow 仍 flat `<slug>.jsonl`;statusline adapter 用 marker section 保护用户脚本
- **V0.3.1 → V0.3.2 升级一次性迁移**:legacy HTML routes(`/`、`/project/<slug>`、`/session/<slug>/<sid>`)F59 起 301 → `/app/...`;旧 bookmark 第一次访问会跳一次,可以保留(不需要用户操作)。`templates/{dashboard,project,session}.html` 已删;`templates/base.html` 留作 askama SSR fallback;`assets/htmx.min.js` 等 legacy 静态资源 V0.4.0 F69 已彻底删除。`crates/ccteam-web/web/` SPA bundle 通过 `build.rs` 自动 `npm run build`;本机开发可 `CCTEAM_SKIP_WEB_BUILD=1 cargo build` 或 `--no-default-features` 跳过(详 `docs/v0-3-2/user-manual.md §4`)
- **V0.3.2 → V0.4.0 升级一次性迁移**:架构级重构。**`team.yaml::kind: workflow`(phase 驱动)EOL** — 跑 `ccteam doctor --migrate-phase-to-workflow` 把旧 `phases:` 列表迁出生成 `workflow.yaml` 骨架 + 各 `.claude/agents/<role>.md` 模板,用户 review prompt 内容后删旧 `phases` 字段(自动迁移只生成结构,语义需手动调:phase 顺序 vs artifact-trigger 事件驱动)。新建项目改写 `workflow.yaml` + `.claude/agents/<role>.md` 替代 `team.yaml::phases`。ccteam-mcp 工具从 10 → **17**(`spawn_agent` / `stop_agent` / `observe_agents` / `signal` / `set_parallelism` / `trigger_gate` / `get_artifact_summary` 新增);meta-agent 自己的 CLAUDE.md 工具表需同步更新,跑 `ccteam doctor --update-meta-agent` 自动改。ClaudeCodeAdapter 从 tmux + statusline 改为 `claude --bg --agent <role>` + `~/.claude/jobs/<job_id>/state.json`;`SpawnOpts` 新增 `role: String` 字段(向后兼容:旧 callsite 传 `role: String::new()`),`SessionHandle` 新增 `job_id: String` 字段。`statusline-command.sh` 不再生成;F46 statusline install 路径(`ccteam doctor --install-statusline-adapter`)+ `HookCommand::HarnessSnapshot` 子命令删除。`progress.jsonl` event schema 重写:phase event(`phase_start` / `phase_done` / `golden_rules_check` 等)→ workflow event(`workflow_start` / `agent_spawn` / `agent_done` / `artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done` + `escalation`)。`current_phase` / `phase_history` / `decision_candidates` 等 ProjectState 字段保留为 `skip_serializing_if = Option::is_none` serde-compat 字段:**新写**不带,**老 state.json** 仍可读;F66 thin orchestrator 完全不消费它们(只读 progress.jsonl 业务 SoT)。`assets/htmx.min.js`、`xterm.js`、`style.css` 等 legacy 静态资源全部删除(F69),`crates/ccteam-web/src/routes/assets.rs` 现仅服务 SPA bundle(`/app/*` + `/assets/spa/*`)。本机器 WSL inotify 资源受限导致 F64 `artifact_watcher_test` t02/t03/t05/t09 + F66 `orchestrator_thin_test` t01/t15 在 cargo test --workspace 时 hang;pre-existing 环境问题,跑测试时 `--skip artifact_watcher_test --skip t01_run_project_loads_workflow --skip t15_manual_trigger_not_auto_spawned` 可绕过,V0.4.1 候选根治
- **`claude --bg --agent` CLI 形态可能漂移** — F61 实现的是 dev-plan §3 抽象契约(`claude --bg --agent <role> --workdir <dir>` 写 `~/.claude/jobs/<job_id>/state.json`),与上游 Claude Code 当前 CLI(`claude daemon bg` + `~/.claude/sessions/<pid>.json`)不完全一致。`CCTEAM_CLAUDE_BIN` + `CCTEAM_CLAUDE_JOBS_DIR` env override 让测试不依赖真实 binary;生产部署如 Claude Code stabilize 后 CLI 形态变化,本仓改 `state_json_path` + `spawn_session` argv 即可,无需重构上层
- **本文件不超过 250 行** — CLAUDE.md 越长 cache 越贵,Claude 越忽略(best-practices §4.1 + §8)

---

## 七、Rust 代码格式化约定

`rustfmt.toml` 在仓库根 pin 房子样式(stable rustfmt;`max_width = 100`、`tab_spaces = 4`、`use_field_init_shorthand`)。约定:

- **新文件 / 大改的文件:`cargo fmt -- <files>` 必跑**(commit 前)。提交前每个 PR 跑一遍 `cargo fmt --check -- <changed_files>` 自检。
- **小改 drifted 文件:不做 fmt-sweep**。本仓存量 fmt drift ~4-5 kLOC(历史遗留);全仓 `cargo fmt --all` 会爆 PR diff,review 不动。规则:动几行格式化几行,周边老代码不顺手 reformat。
- **不上 workspace-wide CI fmt gate**(直到 drift 清零):全仓 `cargo fmt --check` 会红;改用 changed-files-only 自检 + `make fmt` 局部跑。
- **`make fmt` = `cargo fmt --all`**(开发态便利,清干净自己的工作树用);**`make fmt-check` = `cargo fmt --all -- --check`**(只在 drift 清零后才指望它绿)。
- **drift 清理走独立 chore PR**:不混进 finding / feature PR;按模块拆,一次一个 crate / 子目录,方便 review。
