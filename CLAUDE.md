# CLAUDE.md — ccteam 实现导引

> 本文档面向**下一次接手 ccteam 实现的 Claude session**。每次起手必读。
> 历史里程碑 + 升级 migration 见 `docs/versions/v0-X-Y/README.md`,本文只描述**当前状态 + 红线 + 纪律**。

---

## 一、当前状态(2026-05-23)

| 项 | 值 |
|---|---|
| 主分支 main HEAD | 以 `git rev-parse origin/main` 为准 |
| Workspace version | **`0.6.3`** |
| 测试 baseline | **`1471/1`**(`cargo test --workspace --locked --no-fail-fast`,1 fail 是 ccteam-web `workflow_summary_reflects_agent_spawn_and_done_events` running_count flake)|
| Clippy | **0 errors + 0 warnings**(`-D warnings` clean)|
| 代码规模 | ~73 kLOC Rust(workspace,不含 references)|
| 当前最新版 | **V0.6.3**(F142 `Trigger::Schedule` 接真 cron + F143 webhook ingress + F144 vendor 接缝 forward-compat + F145 `squad:` 跨 session 运行时路由)— 详 `docs/versions/v0-6-3/README.md` |
| 上一版 | **V0.6.2**(F140 per-role 代码 `scope` — 大型代码库:`AgentSpec.scope` 把每次 spawn 的 `cwd` 钉到与 role 相关的子树,收窄爆炸半径;源自 Anthropic《How Claude Code works in large codebases》)— 详 `docs/versions/v0-6-2/README.md` |
| V0.6.x 延期候选 | 空(本版闭所有 retained risk)|
| V0.7 主线候选 | Epic C 国内 IM(WeChat / 飞书 / DingTalk / QQ)启用 + chat memory 跨设备同步 + monorepo-aware `.mcp.json` + migrate-from-claude + 6 号编排模式深化(HumanApproval × bg/chat 矩阵全开) |
| 历史版本 | V0.1 → V0.6.1 见各自 `docs/versions/v0-X-Y/README.md` |

**ccteam 是 Claude Code 之上的元工具**(V0.4.0+,V0.6.0 起转 product-ready 元 AI 团队):每个项目用 `workflow.yaml` 声明 agent 拓扑(**无 prompt,只有 trigger + 并发上限 + `mode: chat`/`vendor: claude\|codex` for V0.6 mode 3 + V0.6.1 F124 `mode: human-approval` 第 4 mode + V0.6.1 F98 `plan_approval:` block**),`.claude/agents/<role>.md` 定义 agent 行为;Rust orchestrator 通过 `ArtifactWatcher`(inotify)监听文件系统控制平面 → spawn `claude --bg --agent <role>`(mode 2)/ 进 tmux 长 session `claude` TUI(mode 3,V0.6 F108)/ `codex exec --json` / `codex app-server` UDS(V0.6 F112 + V0.6.1 F122 progress.jsonl bridge);`progress.jsonl` 记录 7 类业务 event + `chat_session_reset` / `turn_done` + V0.6.1 新增 `plan_pending` / `plan_decision` / `plan_timeout`(F98)+ `persona_changed` / `tool_added`(F128)为唯一状态 SoT(mode 3 对话原文走 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`);用户通过 meta-agent + `ccteam-control` skill + **26 个 `mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}*` 子前缀分组工具**(V0.6 F111 24 + V0.6.1 F128 `admin_change_persona` + `admin_add_tool`)操作 + V0.6.1 F129 `@ccteam <NL>` IM mention 路径(pause/resume/list/cost/stop everything 5 keyword admin);`ccteam-imd` supervisor(V0.6 F116;V0.6.1 F130 折入 `ccteam start` 作为单 tokio 任务,标准二进制已删,`--no-imd` 跳过)守 IM bridge,统一 `openhuman/channels` Rust crate 14+ IM 平台(V0.6 F109);web UI 提供 4 面板 + SSE。详 `docs/tech-design.md` §2.1。

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
| 9 | `docs/versions/v0-6-3/README.md` | 看当前版本(V0.6.3 patch:F142 cron + F143 webhook + F144 vendor-seam + F145 squad routing)|
| 10 | `docs/versions/v0-6-2/README.md` | 上一版(V0.6.2 patch:F140 per-role `scope`)|
| 11 | `docs/versions/v0-6-1/README.md` | 上上版(V0.6.1 Epic D/E/F + F98 + F119-F139)|

**起手 30 秒**:`git log -1` 看 HEAD → `cargo test --workspace 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` 校 1471/1 → 读用户诉求 → 干。

**对照参考**(`references/` gitignore 不入库):`references/claude-code/`(Anthropic Claude Code 源码)+ `references/codex/codex-rs/`。HarnessAdapter / 协议适配时翻;**不**当 ccteam 依赖。

**文档维护三类**:
1. **`docs/` 根目录(全局)**— 每 session 装入上下文 + 与代码并列 SoT + 每版本 ship 后必更新
2. **`docs/versions/v0-x-x/`(版本归档)**— ship 后冻结,按需加载,**不动老版本**
3. **`research/`(扩展研究)**— 不更新,按需加载

## 三、不可触碰的架构红线

源 `docs/tech-design.md`,V0.6.0 F106 起按 **"模式 × vendor"双轴 scope**(详 `docs/versions/v0-6-0/README.md §五`)。任何 PR 不得违反:

| 红线 | 模式 1 in-proc | 模式 2 bg(Claude / Codex)| 模式 3 chat(Claude / Codex)|
|---|---|---|---|
| **文件系统是控制平面** | — | 守(artifact 双 vendor)| 守 — Claude: tmux 长 session + transcript jsonl byte-offset 增量读;Codex: app-server UDS;**两 vendor 共写 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`** |
| **`progress.jsonl` 是唯一 state SoT** | — | 守(双 vendor)| 业务事件 SoT 守(7 类 + `chat_session_reset` / `turn_done`);对话原文走 `turns.jsonl`(R2 在 mode 3 站住,不依赖 Anthropic 内部 `~/.claude/projects/`)|
| **No prompt injection** | 守 | 守 | 守 — agent 行为住 `.claude/agents/<role>.md`,**不**向 tmux pane 注入 system prompt;`/compact /new /clear` 完全透传 |
| **每次 spawn = fresh 1M context** | — | Claude: `claude --bg`(无 `--resume`);Codex `codex exec resume <tid>` 由 trait 决定,用户不见 | **不适用** — chat 复用 context 是 feature |
| **永不主动 kill 长 session** | 守 | 守 — `budgets.{claude,codex}.max_cost_usd_per_24h` per-vendor 触顶 → F84 auto-disable workflow | 守 — `/compact /new` 是合法 turn,非 kill;tmux 长跑 24/7 |
| **不解析 tmux 终端输出** | — | 守 | 守 — 读 transcript jsonl + Claude Code 官方 hooks fast event 通道;**不 scrape pane**(`tmux capture-pane` 仅 dev-time 调试 + screenshot tool 只读) |
| **fix-loop 撞 3 次必 escalate** | 守 | 守(`fix_counts` map → escalation event)| 守 + **AgentPath depth limit**(借 Codex `agent_max_depth` 实现 hop_limit 替代平铺 fix_counts)|
| **`ccteam-core` 零 team 名字面量** | 守 | 守 | 守 |
| **跨项目记忆走官方接口** | 守 | Claude: `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`;Codex: `~/.codex/AGENTS.md`(`ccteam init` 落 AGENTS.md → CLAUDE.md POSIX symlink)| 同 |
| **新建项目走 `<projects_root>/<team>-<slug>/`** | — | 守(`pick_unused_slug` 强制 team 前缀)| 守(per-bot tmux session = `<project>/<bot>`,IM bot 落 `.ccteam/chat/<bot>/`)|
| **root README.md MUST be English** | 守 | 守 | 守 |
| **README.md 不含版本进展/状态信息** | 守 | 守 | 守 |
| **HITL approval state SoT**(V0.6.1 F124 narrow scope;`mode: human-approval` 第 4 mode 与 1/2/3 并列)| — | progress.jsonl::plan_decision(F98 IM round-trip 写;orchestrator 等到事件再 drain pending)| 同 |

**README / 版本进展红线**(V0.6.1 F126):
- root `README.md` = OSS 主入口,必须英文。`docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*` 持续中文(国内用户面);所有版本归档 `docs/versions/v0-X-Y/` 中英不限(开发过程语)。
- root `README.md` **不**含 `Status` / `V0.x.y in production` / `shipped 日期` / `baseline 数字` / `candidate finding` 列表等版本进展段。版本进展全部去 `docs/versions/v0-X-Y/README.md`(每版本独立 dir);F-finding 索引去 `docs/dev-coupling-audit.md`。README 是产品介绍,**始终反映当前可用状态**,不夹版本时间轴。

**vendor 红线补充**(V0.6 F107 / F112):
- ccteam **不 vendor** Claude / Codex 二进制(`references/{claude-code,codex/codex-rs}/` git-ignore 不入库,仅协议参考;实际 spawn 走 `$PATH` 内 `claude` / `codex` binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override)
- `vendor: AgentVendor::{Claude, Codex}` enum 是 trait 一等公民,无 default — workflow.yaml 必须 explicit 或由 `ccteam-creator` skill auto-推断写入

## 四、扩展机制速查

详 `docs/tech-design.md` §6:

| 机制 | 用途 |
|---|---|
| **CLAUDE.md** | 项目级 / 用户级持久指令 |
| **Skills**(repo 根 `skills/`,V0.6.0 F113 总入口 + 6 sub-skill)| `/ccteam` 总入口 NL dispatcher / `ccteam-control` / `ccteam-creator` / `ccteam-team` / `ccteam-im-setup`(F117 一次性 IM token onboarding)/ `ccteam-advise`(F112 Codex parallel vote)/ `ccteam-scan`(V0.6.2 F141 大型代码库导航性体检 — 只读 audit)|
| **MCP** | `ccteam` 26 工具(V0.6 F111 24 + V0.6.1 F128 `admin_change_persona` + `admin_add_tool`),5 group 子前缀:`mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}*`;`CCTEAM_DISABLE_TOOLS` group enum(非 glob);可选 `claude-mem`(LLM 自看 surface 决定是否调)|
| **Subagents** | agent 内 `Task(subagent_type=...)` ad-hoc 节流;8 个 plugin agent 已 ln -sf |
| **Hooks** | `ccteam internal hook progress-append / load-context / intercept-ask`(F89 隐藏)|
| **Plugins** | `~/.claude/plugins/marketplaces/claude-plugins-official/`;按需 ln -sf,**不 vendor** |

## 五、PR / 实现纪律

1. **每个 PR 描述映射**:`requirements.md` 某条痛点 + `tech-design.md` 某节 + `dev-coupling-audit.md` 某 F-finding;改协议必同步 `interfaces.md`
2. **commit 用英语**;文档与 agent prompt 用中文
3. **Pre-v1.0 = 开发阶段,不留技术债**:无真实用户群,**允许大胆做更好的抽象**。**不做历史迁移** — 当新版本与旧版本状态数据不兼容时,**不写迁移步骤、也不写兼容代码分支**,直接采取「清除旧版数据(`~/.ccteam/` + 各项目 `.ccteam/`)→ 重新 `ccteam init` 安装」的重装策略;deprecated 直接删,breaking rename 不留 alias。tier-1 文档**只描述当前架构**,EOL 内容去版本 dir
4. **不写 backwards-compat shim**
5. **优先编辑现有文件,不轻易新建**
6. **测试不过不算完成** — `cargo test --workspace` 退步 = block;clippy 不能新增 warning
7. **版本发布同步全局文档(ship gate,不是 post-ship polish)** — 每次 `vX.Y.Z` ship(minor / patch 都算)**必须**同步 3 类文档:
   - **tier-1 内部 docs**:CLAUDE.md §一 baseline + `tech-design.md` / `interfaces.md` / `dev-coupling-audit.md` / `claude-code-tool-surface.md` + workspace `Cargo.toml` version bump ── Wave 4a doc-syncer 守
   - **用户面 docs**:`README.md` + `docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*.md` ── 必须把本版新能力(新 MCP tool / 新 skill mode / 新 vendor 支持 / 新 ops 行为)融入**用户视角**;不能留旧版本描述;**不**写"V0.X.Y 新增"措辞(README §三红线"不含版本进展")─ 直接以当前能力描述呈现
   - **版本归档**:`docs/versions/v0-X-Y/README.md` + handoff doc 落地;F-finding 索引去 `docs/dev-coupling-audit.md`
   - Ship gate 第 12 项(写入 V0.X.Y `README.md §5`):用户面 docs 必须 grep clean + 覆盖本版新能力 + 不夹历史版本号

### 多 session 并行编辑同一仓库

主仓工作树绑定一个 session,并行用 `git worktree add -b <branch> /tmp/ccteam-<name> origin/main` 起独立工作树,完事 `git worktree remove`。**主仓 main 不变 dirty**。

跨 session 见主仓 dirty:`git stash push -m "<owner> WIP"` 再切;**别盲目 `git checkout -- .`**。

### Patch 版本(V0.x.y)开发流程

1. **doc-first**:PRD + dev-plan 落 `docs/versions/v0-x-y/`,用户 review 后才动代码
2. **worktree-per-finding** + subagent 派工(briefing 含 PRD section + 验收条目)
3. **PR review/fix/merge** + `workspace.package.version` bump,commit 用 `vX.Y.Z:` 前缀
4. **CLAUDE.md baseline 回填**:`cargo test --workspace` 通过新数后改本文 §一表格

### Minor 版本(V0.x.0)4-wave 范式(V0.6.0 锁定)

V0.6.0 走通的 4-wave 流程,V0.6.x patch + V0.7 minor 起点直接复用:

1. **doc-first kick-off**:Epic + PRD + dev-plan 同落 `docs/versions/v0-x-0/`(Epic 替代上版 F-finding 顶层结构);多 agent review 收敛(architect / cc-expert / pm / researcher / codex-expert)→ `README.md` synthesize
2. **wave-by-wave worktree 并行**:每 wave 一份 `wave-N-handoff.md`(Decided / Rejected / Risks / Files / Remaining 五段固定)+ 一个 PR;subagent 派工 briefing 必含 wave acceptance gate + 上 wave handoff link
3. **per-wave PR review**:CR(architect)+ implementation(worktree)+ doc-syncer(本 wave)三角分工;CR 不动 docs,doc-syncer 不动 host probe,worktree 不跨 wave 修代码
4. **final wave = doc-syncer + host-probe 双线收尾**:Tier-1 docs sync(本文件 §一 baseline + tech-design / interfaces / dev-coupling-audit / claude-code-tool-surface)+ MCP tool name / config schema sweep + clippy 0-warning gate + version bump + tag

**红线**:每 wave 必须 baseline ≥ 上 wave 数字(test pass count + clippy 0 warnings),否则不发 PR。

## 六、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱;本仓用 Claude Code 默认行为开发,只产出项目挂 ccteam hook
- **`.claude/settings.json` 的 `bypassPermissions` 是开发态便利** — 产品形态走 `--dangerously-skip-permissions`,语义不同
- **`claude-plugins-official` 是参考实现,不是依赖** — 别 vendor;按 §3.7 三粒度选(@引用 / 拷贝改 / 整 plugin install)
- **测试 `bootstrap_project` / `bootstrap_meta_project` 前必先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` 写垃圾,会破坏 claude 登录
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),**不**放 lib `#[cfg(test)] mod tests`
- **改了 `ccteam-core` 公共 API**(如 `pick_unused_slug` 签名)→ grep 全 caller(tests / mcp_serve.rs / commands.rs)
- **`claude --bg --agent` CLI 形态可能漂移** — `CCTEAM_CLAUDE_BIN` + `CCTEAM_CLAUDE_JOBS_DIR` env override 让测试不依赖真实 binary;生产改 `state_json_path` + `spawn_session` argv 即可,无需重构上层
- **本文件 ≤200 行** — 越长 cache 越贵,Claude 越忽略

历史版本升级 migration(V0.1 → V0.4.5)详各 `docs/versions/v0-X-Y/README.md`,不在此重复。

## 七、Rust 代码格式化约定

`rustfmt.toml` pin stable rustfmt(`max_width = 100` / `tab_spaces = 4` / `use_field_init_shorthand`):

- **不用 `cargo fmt`**(含 `cargo fmt -- <files>` 形式)── cargo fmt **silently 忽略文件参数**,会全 workspace 跑,把存量 ~4-5 kLOC fmt drift 拉进你的 PR diff(2026-05-24 V0.6.5 W1-T3 实战踩坑)
- **新文件 / 大改文件:`rustfmt --edition 2021 <files>` 直调**(commit 前;原地写回)
- **dry-run probe:**`rustfmt --check --edition 2021 <files>`
- **小改 drifted 文件:不 fmt-sweep**(让 drift 维持现状,别动)
- **不上 workspace-wide CI fmt gate**(drift 清零后才指望)
- **drift 清理走独立 chore PR**,按模块拆,一次一个 crate
