# CLAUDE.md — ccteam 实现导引

> 本文档面向**下一次接手 ccteam 实现的 Claude session**。项目已过初创期,
> 核心里程碑 M0–M4 全部 ship,代码已上规模。本文是 session 起手的第一份导引。

---

## 一、当前状态(2026-05-09)

| 项 | 值 |
|---|---|
| 仓库根 | `~/workplace/agents/ccteam` |
| 主分支 main HEAD | **以 `git rev-parse origin/main` 为准**(V0.2.2 ship 后) |
| Workspace version | **`0.2.2`**(V0.2.2 起跟版同步;V0.1/V0.2 ship 时 `0.0.1` 未跟,V0.2.2 retroactive 修正)|
| 测试 baseline | **628 全绿**(`cargo test --workspace`)|
| Clippy | 4 pre-existing errors(非本仓引入,sweep 时确认相同基线) |
| 代码规模 | ~12 kLOC across `ccteam-core` / `ccteam-cli` / `ccteam-hooks` |
| 已 ship 里程碑 | **V0.1**:M0 / M0.5 / M1 / M2 / M2.3 / M3 / M4.1-M4.4 — 详 `docs/v0-1/README.md`<br>**V0.2**:M0.16-M0.23(8 个 milestone:反模式清理 / 团队布局 + plugin 模型 / phase prompt 协议外移 / 自循环 + AskUserQuestion 拦截 / plugin pipeline / daemon supervision / watchdog / team factory)— 详 `docs/v0-2/README.md`<br>**V0.2.2**:F34-F40(7 finding 跨 7 PR:cct convention sweep / slug 4-tier + 决策树加固 / silence classifier / subagent guard / 截图 PNG / team alias)— 详 `docs/v0-2-2/README.md` |
| 当前 next | V0.3 候选方向待定,deferred 项见 `docs/v0-2/README.md` 末尾 + `docs/v0-2-2/prd.md §11` |
| 永久 deferred | M2.2 agent_team enablement(spike A,Claude Code 无 first-class CLI surface — 见 `docs/v0-1/m2-agent-team-spike.md`)|

**ccteam 是 Claude Code 之上的元工具,不是独立 AI 系统**:每个项目一个 Claude Code 长 session(tmux 守护,hooks 上报,MCP 接外部);Rust orchestrator 编排(binary 名 `cct`,F39 起);用户通过 meta-agent(常驻 ccteam-managed claude session)+ `cct-control` skill / `ccteam` MCP server(`mcp__ccteam__*` 命名空间)用自然语言对话操作。详见 `docs/tech-design.md` §2.1 三层架构。

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
- **`cct` 短前缀约定**(V0.2.2 F39):binary 名 `cct`,自带 skill `cct-control` / `cct-team-author`,顶层 `skills/` 目录是 SoT;**项目名仍叫 ccteam**(`crates/ccteam-{cli,core,hooks}` / `~/.ccteam/` / git repo / MCP 命名空间 `mcp__ccteam__*` 不动 — 详 PRD §8.3)。`cct doctor` 自带 V0.1/V0.2 → V0.2.2 迁移逻辑;新代码 / phase prompt / docs 一律用 `cct`

---

## 四、扩展机制速查(详见 tech-design §6)

| 机制 | 用途 | 文档 |
|---|---|---|
| **CLAUDE.md** | 项目级 / 用户级持久指令 | best-practices §4.1 |
| **Skills** | `cct-control`(M1.8 ✅,V0.2.2 F39 改名)/ `cct-team-author`(M0.22 ✅)/ `ccgram-messaging`(M3+ 多 orchestrator);自带 skill SoT 在 repo 根 `skills/` | tech-design §6.7 |
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

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱。本仓库用 Claude Code 默认行为开发,只产出物(`~/projects/<team>-<slug>/`)挂 cct 自己的 hook
- **`.claude/settings.json` 的 `bypassPermissions` 是开发态便利** — 产品形态是 `--dangerously-skip-permissions` + 容器隔离,语义不同(best-practices §4.2 三选一)
- **phase prompt 别写太长** — 单条 send-keys 装得下;复杂内容用 `@文件引用`(best-practices §3)
- **`claude-plugins-official` 是参考实现,不是依赖** — 别 vendor 一份;按 §3.7 三粒度选(@引用 / 拷贝改 / 整 plugin install)
- **测试调 `bootstrap_project` / `bootstrap_meta_project` 之前必须先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` + `~/.claude/agents/` 写垃圾,长期撑大 `.claude.json` 会破坏 claude 登录(2026-05-06 实测)
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),不放 lib `#[cfg(test)] mod tests`;同 binary 内其他测试读 env 会 race
- **多 session 并行编辑同一仓库** → 用 `git worktree`,主仓不动
- **跨 session 协作时见到主仓 dirty 状态** → `git stash push -m "<owner-session> WIP"` 再切,别盲目 `git checkout -- .`
- **改了 `ccteam-core` 公共 API**(如 `pick_unused_slug` 签名)→ grep 全 caller(包括 tests / mcp_serve.rs / commands.rs)
- **F22 后 slug 带 team 前缀**:`run_new` test 期望 `dev-<base>` 而非 `<base>`;改新 slug 路径时验证 rules `paths:` 还匹配
- **V0.1 → V0.2 升级一次性迁移**:M0.20 后 plugin agent 通过 spawned session `enabledPlugins` 启用,不再 ln -sf 进 `~/.claude/agents/`。V0.1 用户首次升级 V0.2 时跑 `cct doctor --migrate-recommended-agents` 清理旧 ln -sf(只删 ccteam 自己创建的 marketplace symlink,用户手写 agent 不动)
- **V0.2 → V0.2.2 升级一次性迁移**(F39):binary `ccteam` → `cct`,skill `ccteam-{control,team-author}` → `cct-{control,team-author}`。`cct doctor --install-skill` / `--install-meta-agent` 自动检测 + 清旧 skill dir(marker 校验,只清 ccteam-managed 的;用户手改保留 + warn) + rewrite `~/projects/<slug>/.claude/settings.json` 老 hook command 路径(原子写)
- **本文件不超过 250 行** — CLAUDE.md 越长 cache 越贵,Claude 越忽略(best-practices §4.1 + §8)
