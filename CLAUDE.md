# CLAUDE.md — ccteam 实现导引

> 本文档面向**下一次接手 ccteam 实现的 Claude session**。每次起手必读。
> 历史里程碑 + 升级 migration 见 `docs/v0-X-Y/README.md`,本文只描述**当前状态 + 红线 + 纪律**。

---

## 一、当前状态(2026-05-17)

| 项 | 值 |
|---|---|
| 主分支 main HEAD | 以 `git rev-parse origin/main` 为准 |
| Workspace version | **`0.5.0`** |
| 测试 baseline | **`907/1`**(`cargo test --workspace --locked --no-fail-fast`,1 fail 是 ccteam-web `workflow_summary_reflects_agent_spawn_and_done_events` running_count flake)|
| Clippy | 0 errors + 18 warnings(pre-existing doc-list drift)|
| 代码规模 | ~19 kLOC Rust + ~15 kLOC TypeScript |
| 当前最新版 | **V0.5.0**(F92 + F93a + F93b + F94 + F95 + F96 + F100 + F101)— 详 `docs/v0-5-0/README.md` |
| V0.5.x 延期候选 | F97 lifecycle 完善;F98 plan-approval↔outbox 联动;F99 Claude Code 版本 gating;Routing/Evaluator-Optimizer sugar — 详 `docs/v0-5-0/prd.md` 末段 + `docs/orchestration-patterns.md §五` |
| 历史版本 | V0.1 → V0.4.5 见各自 `docs/v0-X-Y/README.md` |

**ccteam 是 Claude Code 之上的元工具**(V0.4.0+):每个项目用 `workflow.yaml` 声明 agent 拓扑(**无 prompt,只有 trigger + 并发上限**),`.claude/agents/<role>.md` 定义 agent 行为;Rust orchestrator 通过 `ArtifactWatcher`(inotify)监听文件系统控制平面 → spawn `claude --bg --agent <role>`(或 Codex tmux session);`progress.jsonl` 记录 7 类业务 event 为唯一状态 SoT;用户通过 meta-agent(V0.5.0 F101 重定位为**轻量 router + memory bridge + dashboard**,不再自起 phase pipeline)+ `ccteam-control` skill + 17 个 `mcp__ccteam__*` 工具操作;web UI 提供 4 面板 + SSE。详 `docs/tech-design.md` §2.1。

## 二、必读文档(按推荐顺序)

| # | 文档 | 何时读 |
|---|---|---|
| 0 | `docs/README.md` | 加 / 改文档前(三类文档维护规则)|
| 1 | `docs/requirements.md` | 验收基准(15 痛点 = 13 用户 + 2 V1.0.0)|
| 2 | `docs/orchestration-patterns.md` | 加 workflow 模板 / 设计新 finding / 拓新领域 team 前(5 模式 + 拆分哲学)|
| 3 | `docs/tech-design.md` | 改架构前 |
| 4 | `docs/interfaces.md` | 改 schema / CLI / hooks 时必同步 |
| 5 | `docs/dev-coupling-audit.md` | 改 `ccteam-core` 前;新发现加 F<N> |
| 6 | `docs/ccteam-as-domain-agnostic-orchestrator.md` | 加新 team / 改红线时 |
| 7 | `docs/claude-code-best-practices.md` | 改 agent prompt / hooks / context 管理时 |
| 8 | `docs/claude-code-tool-surface.md` | 改 workflow.yaml + agent .md 时 |
| 9 | `docs/v0-4-6/README.md` | 看当前版本状态 |
| 10 | `docs/v0-5-0/README.md` | 看立项中下个版本(agent-team mode + 真 cost)|

**起手 30 秒**:`git log -1` 看 HEAD → `cargo test --workspace 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` 校 755/1 → 读用户诉求 → 干。

**对照参考**(`references/` gitignore 不入库):`references/claude-code/`(Anthropic Claude Code 源码)+ `references/codex/codex-rs/`。HarnessAdapter / 协议适配时翻;**不**当 ccteam 依赖。

**文档维护三类**:
1. **`docs/` 根目录(全局)**— 每 session 装入上下文 + 与代码并列 SoT + 每版本 ship 后必更新
2. **`docs/v0-x-x/`(版本归档)**— ship 后冻结,按需加载,**不动老版本**
3. **`research/`(扩展研究)**— 不更新,按需加载

## 三、不可触碰的架构红线

源 `docs/tech-design.md`,任何 PR 不得违反:

- **文件系统是控制平面** — 不接 Linear/GitHub Issues 作状态源
- **`progress.jsonl` 是唯一 state SoT** — 7 类 workflow event(workflow_start / agent_spawn / agent_done / artifact_received / gate_triggered / budget_exceeded / workflow_done + escalation);不解析 tmux / output.log 输出
- **No prompt injection** — orchestrator 永不向 session 注入 system prompt;agent 行为住 `.claude/agents/<role>.md`
- **每次 spawn = fresh 1M context** — Claude 走 `claude --bg --agent <role>` 新 job,无 `--resume`
- **永不主动 kill 长 session** — 唯一例外:per-project `max_cost_usd_per_24h` 触顶 → F84 auto-disable workflow(`enabled: false` + cancel token graceful exit)
- **fix-loop 撞 3 次必 escalate**(`fix_counts` map → escalation event;绝不静默重置)
- **`ccteam-core` 零 team 名字面量** — 团队特定行为靠 `team.yaml` + `workflow.yaml` 数据驱动
- **跨项目记忆走官方接口** — `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` + per-repo auto-memory;ccteam 不写程序读 memory 文件
- **新建项目走 `<projects_root>/<team>-<slug>/`** — `pick_unused_slug` 强制 team 前缀;meta-agent `<handle>-meta` 后缀(独立路径)

## 四、扩展机制速查

详 `docs/tech-design.md` §6:

| 机制 | 用途 |
|---|---|
| **CLAUDE.md** | 项目级 / 用户级持久指令 |
| **Skills**(repo 根 `skills/`,V0.5.0 F100 5→3)| `ccteam-control`(CLI / MCP wrap)/ `ccteam-creator`(new project + workflow + agent + skill 对话向导)/ `ccteam-team`(`/ccteam:team` 当前 session 起 Anthropic Agent Team)|
| **MCP** | `ccteam` 17 工具(`mcp__ccteam__*`);可选 `claude-mem`(LLM 自看 surface 决定是否调)|
| **Subagents** | agent 内 `Task(subagent_type=...)` ad-hoc 节流;8 个 plugin agent 已 ln -sf |
| **Hooks** | `ccteam internal hook progress-append / load-context / intercept-ask`(F89 隐藏)|
| **Plugins** | `~/.claude/plugins/marketplaces/claude-plugins-official/`;按需 ln -sf,**不 vendor** |

## 五、PR / 实现纪律

1. **每个 PR 描述映射**:`requirements.md` 某条痛点 + `tech-design.md` 某节 + `dev-coupling-audit.md` 某 F-finding;改协议必同步 `interfaces.md`
2. **commit 用英语**;文档与 agent prompt 用中文
3. **Pre-v1.0 = 开发阶段,不留技术债**:无真实用户群,**允许大胆做更好的抽象**。**不做历史迁移**(deprecated 直接删 / breaking rename 不留 alias / `#[serde(default)]` compat 仅在迁移成本 > 重启成本 时用)。tier-1 文档**只描述当前架构**,EOL 内容去版本 dir
4. **不写 backwards-compat shim**
5. **优先编辑现有文件,不轻易新建**
6. **测试不过不算完成** — `cargo test --workspace` 退步 = block;clippy 不能新增 warning

### 多 session 并行编辑同一仓库

主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/main` 起独立工作树,完事 `git worktree remove`。**主仓 main 不变 dirty**。

跨 session 见主仓 dirty:`git stash push -m "<owner> WIP"` 再切;**别盲目 `git checkout -- .`**。

### Patch 版本(V0.x.y)开发流程

1. **doc-first**:PRD + dev-plan 落 `docs/v0-x-y/`,用户 review 后才动代码
2. **worktree-per-finding** + subagent 派工(briefing 含 PRD section + 验收条目)
3. **PR review/fix/merge** + `workspace.package.version` bump,commit 用 `vX.Y.Z:` 前缀
4. **CLAUDE.md baseline 回填**:`cargo test --workspace` 通过新数后改本文 §一表格

## 六、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱;本仓用 Claude Code 默认行为开发,只产出项目挂 ccteam hook
- **`.claude/settings.json` 的 `bypassPermissions` 是开发态便利** — 产品形态走 `--dangerously-skip-permissions`,语义不同
- **`claude-plugins-official` 是参考实现,不是依赖** — 别 vendor;按 §3.7 三粒度选(@引用 / 拷贝改 / 整 plugin install)
- **测试 `bootstrap_project` / `bootstrap_meta_project` 前必先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` 写垃圾,会破坏 claude 登录
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),**不**放 lib `#[cfg(test)] mod tests`
- **改了 `ccteam-core` 公共 API**(如 `pick_unused_slug` 签名)→ grep 全 caller(tests / mcp_serve.rs / commands.rs)
- **`claude --bg --agent` CLI 形态可能漂移** — `CCTEAM_CLAUDE_BIN` + `CCTEAM_CLAUDE_JOBS_DIR` env override 让测试不依赖真实 binary;生产改 `state_json_path` + `spawn_session` argv 即可,无需重构上层
- **本文件 ≤200 行** — 越长 cache 越贵,Claude 越忽略

历史版本升级 migration(V0.1 → V0.4.5)详各 `docs/v0-X-Y/README.md`,不在此重复。

## 七、Rust 代码格式化约定

`rustfmt.toml` pin stable rustfmt(`max_width = 100` / `tab_spaces = 4` / `use_field_init_shorthand`):

- **新文件 / 大改文件:`cargo fmt -- <files>` 必跑**(commit 前)
- **小改 drifted 文件:不 fmt-sweep**(本仓存量 fmt drift ~4-5 kLOC;全仓 `cargo fmt --all` 会爆 PR diff)
- **不上 workspace-wide CI fmt gate**(drift 清零后才指望)
- **drift 清理走独立 chore PR**,按模块拆,一次一个 crate
