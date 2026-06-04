# CLAUDE.md — ccteam 实现导引

> 本文档面向**下一次接手 ccteam 实现的 Claude session**。每次起手必读。
> 历史里程碑 + 升级 migration 见 `docs/versions/v0-X-Y/README.md`,本文描述**当前状态 + 红线 + 纪律**。

---

## 〇、v0.8.3 / v8.3 当前架构红线

本仓已落地 **v8.3「云 CC/Codex + IM + Web」真实路径收敛**(版本号 `0.8.3`):架构 SoT 是 `docs/tech-design.md` 与本文(**协议细节一律以代码为准**,见 tech-design 末尾「协议 → 代码位置」指针表);UX/概念原型见 `docs/im.html`(crate 拓扑以本节 + tech-design §3.1 的 `core→harness→cost` 为准)。**下文若仍有 V0.6.x / orchestrator-era 术语残留,以本节为准:**

- **改名/重构已收敛**:`ccteam-mux`→`ccteam-harness`、`MuxBackend`→`ProcessBackend`、`ccteam-imd`→`ccteam-im`;orchestrator 从 `ccteam-core` 抽到 `ccteam-flow`(core 瘦成 primitives leaf)。拓扑保持 `core -> harness -> cost`,不要翻成 `harness -> core`。
- **执行层两轴**:`HarnessAdapter`(vendor 怎么驱动:Claude=tmux+send-keys+transcript+hook;Codex=app-server JSON-RPC)× `ProcessBackend`(进程跑哪:tmux/inproc/remote);tmux pane 操作只属于 `PaneBackend` 子 trait。两 vendor 归一成中立 `CanonicalEvent` + `ApprovalIR`(**不**抄 alleycat 的 codex-emulation)。
- **daemon = IM/web⇄session 路由网关**(一个进程含 IM gateway + web chat WS + MCP Unix socket + web server,**不 tick、无 orchestrator 循环**);编排 `ccteam-flow` 推后,过渡期借 cc/codex 内部编排。
- **`ccteam init` 当前布局**:项目内写 `.ccteam/{agents,skills,state.json}` + `.claude/agents`;`.ccteam/skills/.gitkeep` 预留项目自有 skill 扩展。
- **`ccteam start` 当前职责**:无 slug 时只启动 resident gateway daemon(web + web chat WS + IM gateway + MCP socket + 可选 hook sink),不构造 `ccteam-flow::Orchestrator`,legacy `--tick-seconds` / `--claude-argv` 仅兼容解析。
- **会话 = resume-by-id**(spawn-on-demand + 按 id resume + 空闲释放,**非**常驻吊着):IM session 属 chat 类,红线「每次 spawn = fresh 1M context」对它**不适用**(chat 复用 context 本就是 feature);autonomous bg 路径仍 fresh-spawn。
- **v8.1 不做手机批准** → agent 走 `--dangerously-skip-permissions`(无批准门);`ApprovalIR` 留类型占位,HITL 批准推后。
- **核心概念 `chat ⇄ project ⇄ session`**:一个 chat = 你的终端(IM chat 或 web chat),跨多 project、随时 `/new` 多 session、随时切(`@bot` / `/use` / `/cd`);命令 Claude 走 send-keys、Codex 走 app-server RPC(/compact=`thread/compact/start`、/review=`review/start`)。
- **vendor 选型**:Claude→tmux(全 TUI + 耐久 + 已有);Codex→app-server(原生、文档化)。per-adapter best-fit,不强行统一。
- **progress 写入权威**:`harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export。

> 验证优先用确定性 fake(`CCTEAM_{CLAUDE,CODEX}_BIN`)和真实 WS smoke;不退 baseline。起手/恢复先读本文 §一 + `docs/tech-design.md`(架构 SoT);`docs/im.html` 是 UX/概念原型,按需查。

---

## 一、当前状态

| 项 | 值 |
|---|---|
| Workspace version | `0.8.5` |
| 测试 baseline | `1903/0`(`cargo test --workspace --locked --no-fail-fast --exclude ccteam-web`;`ccteam-web` ws_* 测试留 CI/专机)|
| Clippy | 0 errors + 0 warnings(`cargo clippy --workspace --all-targets -- -D warnings`)|
| 当前在做 | **v0.8.5 已落地「IM 命令面全量覆盖 + /sessions 状态 + 菜单 + skill 双装」**(D1 中立命令面 `handle_directive`/`ChoicePrompt` + D2 Codex 全量映射 + D2.4 `CodexThreadTracker` + D3/D4 弹窗两段式 + D5 Claude 四通道 gate + D6 AskUserQuestion hook + F10 codex stdio transport 单轴 + P1 菜单 + P3 `/sessions` 状态 + P4 skill 双装;PRD/handoff 见 `docs/versions/v0-8-5/`) |

> 主分支 HEAD 以 `git rev-parse origin/main` 为准;历史版本里程碑见 `docs/versions/v0-X-Y/README.md`(冻结归档)。

**ccteam 是 Claude Code(+ Codex)之上的元工具** —— 云端常驻的元 AI 团队,从 IM 和 web 驱动。架构 5 块:

- **配置**:每项目 `workflow.yaml` 声明 agent 拓扑(**无 prompt**,只有 trigger + 并发上限 + `vendor: claude|codex`);`.claude/agents/<role>.md`(Codex `AGENTS.md`)定义 agent 行为
- **执行**:resident daemon = IM/web⇄session 路由网关(**不 tick、无 orchestrator 循环**)→ 按需 spawn / resume session:Claude 走 tmux 长 session(send-keys + transcript + hook),Codex 走 `codex app-server` JSON-RPC;两 vendor 归一成中立 `CanonicalEvent`
- **状态 SoT**:`progress.jsonl` 业务事件(`harness/progress_bridge` 单一权威);chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`(不依赖 Anthropic 内部 `~/.claude/projects/`)
- **接口**:27 个 MCP 工具 `mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}*`(代码 `STUB_TOOLS` + `ccteam doctor --verify-mcp` 自检)+ `@ccteam <NL>` IM admin(pause/resume/list/cost/stop)+ web UI SSE 面板 + `/app/chat` web chat 控制台
- **安装**:`curl install.sh | sh`(prebuilt binary,linux + macOS,Windows 走 WSL2)→ Claude `/plugin install ccteam` OR Codex `codex plugin marketplace add firstintent/ccteam`;`cargo install --git https://github.com/firstintent/ccteam ccteam-cli` 是 fallback

详 `docs/tech-design.md`。

## 二、必读文档(全局文档已收敛到 3 份)

> **代码是唯一 SoT**。文档只留代码里没有的「为什么 / 架构论证 / 怎么用」;协议细节(CLI / JSON / event / 路由)一律以代码为准 —— 见 `tech-design.md` 末尾「协议 → 代码位置」指针表。

| 文档 | 角色 | 何时读 |
|---|---|---|
| `docs/tech-design.md` | 架构 SoT(gateway daemon + HarnessAdapter×ProcessBackend + chat⇄project⇄session)+ 协议→代码指针表 | 改架构前 / 找协议在哪 |
| `docs/requirements.md` | 原始需求(核心痛点 = 验收基准) | 验收基准 / PR 痛点映射 |
| `docs/usage.md` | 用户命令手册(install→start→use→运维,纯命令) | 看怎么用 |

历史版本归档 `docs/versions/v0-X-Y/README.md`(冻结、按需);探索研究 `docs/research/`(不更新、按需);UX 概念原型 `docs/im.html`、v0.8.3 原型 `docs/versions/v0-8-3/prd.html`。这些都**不**自动进上下文。

**起手 30 秒**:`git log -1` 看 HEAD → `cargo test --workspace --exclude ccteam-web 2>&1 | awk '/^test result/{p+=$4;f+=$6}END{print p,f}'` 记基线 → 读用户诉求 → 干。

**对照参考**(`references/` gitignore 不入库):`references/claude-code/` + `references/codex/codex-rs/`。HarnessAdapter / 协议适配时翻;**不**当 ccteam 依赖。

## 三、不可触碰的架构红线

**本节是架构红线的唯一权威清单**(`docs/tech-design.md` §0 只放 R-code 速查 + 就地论证,引用本节)。两条用户进入层(IM + web)+ autonomous bg 都守;orchestrator tick 已退役,旧的「模式 1/2/3」分栏作废,统一如下。任何 PR 不得违反:

| 红线 | 怎么守 |
|---|---|
| **No prompt injection** | agent 行为住 `.claude/agents/<role>.md`(Codex `AGENTS.md`),**不**向 tmux pane / app-server 注入 system prompt;`/compact /new /clear` 完全透传 |
| **`progress.jsonl` 是 state SoT** | `harness/progress_bridge` 是 schema 单一权威,`core` 只 re-export;chat 对话原文走 ccteam-owned `<project>/.ccteam/chat/<bot>/turns.jsonl`(不依赖 Anthropic 内部 `~/.claude/projects/`)|
| **不解析终端输出** | 读 transcript jsonl + 官方 hooks fast event;**不 scrape pane**(`tmux capture-pane` 仅 dev 调试 + screenshot tool 只读)|
| **永不主动 kill 长 session** | `budgets.{claude,codex}.max_cost_usd_per_24h` 触顶 auto-disable 是唯一例外;`/compact /new` 是合法 turn,非 kill |
| **会话 = resume-by-id** | spawn-on-demand + 按 id resume + 空闲释放 + 扛 daemon 重启,**非**常驻吊着;chat 复用 context 是 feature(仅 autonomous bg 仍 fresh-spawn)|
| **`ccteam-core` 零 team 名字面量** | core = primitives leaf,team 名不入 core |
| **跨项目记忆走官方接口** | Claude `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md`;Codex `~/.codex/AGENTS.md`(`ccteam init` 落 AGENTS.md → CLAUDE.md symlink)|
| **新建项目走 `<projects_root>/<team>-<slug>/`** | `pick_unused_slug` 强制 team 前缀;IM/web bot 落 `.ccteam/chat/<bot>/` |
| **文件系统是状态面**(非 chat 命令面)| state.json / progress.jsonl / turns.jsonl on disk;chat 命令走 IM/WS(`ccteam-chat.v1`) → gateway `handle_text`,**不**再 file-watch(orchestrator tick 已退役;仅 autonomous bg 用 artifact 控制面)|
| **root README.md 必须英文 + 不含版本进展/状态** | README 始终反映当前能力,不夹版本时间轴 / baseline / shipped 日期 |

> **已退役的旧红线**(新架构打破,勿再引用):「每次 spawn = fresh 1M context」(chat 复用 context 是 feature,仅 bg 适用)、「fix-loop 撞 3 次 escalate / AgentPath depth」(属推后的 `ccteam-flow` 引擎)、「HITL approval state SoT / `mode: human-approval` 第 4 mode」(批准全推后,`ApprovalIR` 仅类型占位,agent 走 `--dangerously-skip-permissions`)。

**README / 版本进展红线**(V0.6.1 F126):
- root `README.md` = OSS 主入口,必须英文。`docs/usage.md`(用户命令手册)中文(国内用户面);所有版本归档 `docs/versions/v0-X-Y/` 中英不限(开发过程语)。
- root `README.md` **不**含 `Status` / `V0.x.y in production` / `shipped 日期` / `baseline 数字` / `candidate finding` 列表等版本进展段。版本进展全部去 `docs/versions/v0-X-Y/README.md`(每版本独立 dir)。README 是产品介绍,**始终反映当前可用状态**,不夹版本时间轴。

**vendor 红线补充**(V0.6 F107 / F112):
- ccteam **不 vendor** Claude / Codex 二进制(`references/{claude-code,codex/codex-rs}/` git-ignore 不入库,仅协议参考;实际 spawn 走 `$PATH` 内 `claude` / `codex` binary + `CCTEAM_{CLAUDE,CODEX}_BIN` env override)
- `vendor: AgentVendor::{Claude, Codex}` enum 是 trait 一等公民,无 default — workflow.yaml 必须 explicit 或由 `ccteam-creator` skill auto-推断写入

**skill 自洽红线**:`skills/*/SKILL.md` + `skills/*/` 其他 body 文件随 `ccteam init` 装到用户机器,**必须**自洽 ── 禁版本号(`V0.X.Y`)/ 禁 `docs/versions/v0-X-Y/` 引用 / 禁 Wave-N / 禁 F-tag / 禁 ship-status creep(shipped / "已 ship" / "ship gate" 等)/ 禁 PR # / 禁 commit ref。允许:sibling skill 引用、MCP tool 名、CLI 命令、稳定 user-facing docs (`docs/usage.md`)。Dev-side 历史去 `docs/versions/v0-X-Y/README.md`。Ship gate:`grep -rnE "V\d+\.\d+\|docs/versions\|Wave [0-9]\|F[0-9]+[a-z]?\b\|F-Bug\|ship gate\|shipped" skills/*/SKILL.md` 必须 0 命中。

## 四、扩展机制速查

详 `docs/tech-design.md` §6:

| 机制 | 用途 |
|---|---|
| **CLAUDE.md** | 项目级 / 用户级持久指令 |
| **Skills**(repo 根 `skills/`)| `/ccteam` 总入口 NL dispatcher / `ccteam-control` / `ccteam-creator`(`ccteam probe-project --json` 探测 → workflow.yaml scope + role.md sensible defaults)/ `ccteam-team` / `ccteam-im-setup`(一次性 IM token onboarding)/ `ccteam-advise`(MCP advise_vote + advise_parallel)/ `ccteam-scan`(大型代码库只读 audit,`--quick` mode)|
| **MCP** | `ccteam` 27 工具,0 STUB(`STUB_TOOLS` const + `ccteam doctor --verify-mcp` 自检,drift exit 1);5 group 子前缀 `mcp__ccteam__{workflow_,chat_,advise_,admin_,screenshot}*`;`CCTEAM_DISABLE_TOOLS` group enum;wire 纪律:`mcp-serve` stdout 纯 JSON-RPC、tracing→stderr |
| **Subagents** | agent 内 `Task(subagent_type=...)` ad-hoc 节流;8 个 plugin agent 已 ln -sf |
| **Hooks** | `ccteam internal hook progress-append / load-context / intercept-ask`(F89 隐藏)|
| **Plugins** | 参考实现 `~/.claude/plugins/marketplaces/claude-plugins-official/`(按需 ln -sf,**不 vendor**);**ccteam 自身分发**:repo 同时是 Claude + Codex plugin marketplace ── `.claude-plugin/{plugin,marketplace}.json` + `.codex-plugin/plugin.json` + 根 `.mcp.json`(`command: "ccteam"` 走 PATH binary);user 装路径 = install.sh(binary)+ `/plugin install ccteam` OR `codex plugin marketplace add firstintent/ccteam` |

## 五、PR / 实现纪律

1. **每个 PR 描述映射**:`requirements.md` 某条痛点 + `tech-design.md` 某节;改协议**以代码为 SoT**(同步 tech-design 末尾「协议→代码」指针表,不再维护 interfaces.md)
2. **commit 用英语**;文档与 agent prompt 用中文
3. **Pre-v1.0 = 开发阶段,不留技术债**:无真实用户群,**允许大胆做更好的抽象**。**不做历史迁移** — 当新版本与旧版本状态数据不兼容时,**不写迁移步骤、也不写兼容代码分支**,直接采取「清除旧版数据(`~/.ccteam/` + 各项目 `.ccteam/`)→ 重新 `ccteam init` 安装」的重装策略;deprecated 直接删,breaking rename 不留 alias。tier-1 文档**只描述当前架构**,EOL 内容去版本 dir
4. **不写 backwards-compat shim**
5. **优先编辑现有文件,不轻易新建**
6. **测试不过不算完成** — `cargo test --workspace` 退步 = block;clippy 不能新增 warning
7. **版本发布同步文档(ship gate)** — 每次 `vX.Y.Z` ship 必须同步:
   - **内部 SoT**:CLAUDE.md §一 baseline + `docs/tech-design.md`(架构 + 协议→代码指针表)+ workspace `Cargo.toml` version bump
   - **用户面**:root `README.md`(英文,不含版本进展)+ `docs/usage.md`(命令手册)── 把本版新能力融入**当前能力描述**,不写"V0.X.Y 新增"措辞
   - **版本归档**:`docs/versions/v0-X-Y/README.md` + handoff doc 落地

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
4. **final wave = doc-syncer + host-probe 双线收尾**:Tier-1 docs sync(本文件 §一 baseline + `tech-design.md` 架构 + 协议→代码指针表)+ MCP tool name / config schema sweep + clippy 0-warning gate + version bump + tag

**红线**:每 wave 必须 baseline ≥ 上 wave 数字(test pass count + clippy 0 warnings),否则不发 PR。

## 六、易踩的坑(实战累积)

- **不要给 ccteam 自己加 ccteam 风格的 hook/orchestrator** — 循环引用排错地狱;本仓用 Claude Code 默认行为开发,只产出项目挂 ccteam hook
- **`.claude/settings.json` 的 `bypassPermissions` 是开发态便利** — 产品形态走 `--dangerously-skip-permissions`,语义不同
- **`claude-plugins-official` 是参考实现,不是依赖** — 别 vendor;按 §3.7 三粒度选(@引用 / 拷贝改 / 整 plugin install)
- **测试 `bootstrap_project` / `bootstrap_meta_project` 前必先调 `disable_tool_surface_bootstrap_for_tests()`** — 否则向真实 `~/.claude.json` 写垃圾,会破坏 claude 登录
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` 等)放 `crates/*/tests/*.rs` integration(各独立进程),**不**放 lib `#[cfg(test)] mod tests`
- **改了 `ccteam-core` 公共 API**(如 `pick_unused_slug` 签名)→ grep 全 caller(tests / mcp_serve.rs / commands.rs)
- **`claude --bg --agent` CLI 形态可能漂移** — `CCTEAM_CLAUDE_BIN` + `CCTEAM_CLAUDE_JOBS_DIR` env override 让测试不依赖真实 binary;生产改 `state_json_path` + `spawn_session` argv 即可,无需重构上层
- **WSL2 / inotify-busy 宿主** `fs.inotify.max_user_instances=128` 易触顶,本机跑 cargo test 见大批 watcher/SSE/web e2e 502 → **环境层**,non-WSL / 大 limit 机 OR CI 复测;不计入 baseline
- **`gh auth token` 没 `workflow` scope** ── 改 `.github/workflows/*` 文件的 PR HTTPS 推会被 GitHub 403 拒绝(`refusing to allow an OAuth App to create or update workflow`)。改用 SSH 推 `git push -u git@github.com:firstintent/ccteam.git <branch>:<branch>`
- **`cargo fmt --all` 已是 required**(V0.6.6 post-ship drift-zero,`.github/workflows/check.yml` CI gate 守) ── 不再有"小改 drifted 文件不 fmt-sweep"特例,commit 前一律跑 + CI `cargo fmt --all -- --check` 不过 PR 不能 merge
- **本文件 ≤200 行** — 越长 cache 越贵,Claude 越忽略

历史版本升级 migration(V0.1 → V0.4.5)详各 `docs/versions/v0-X-Y/README.md`,不在此重复。

## 七、Rust 代码格式化约定

`rustfmt.toml` pin stable rustfmt(`max_width = 100` / `tab_spaces = 4` / `use_field_init_shorthand`)。**Workspace drift 已一次清零**(`cargo fmt --all` chore PR + CI gate 守),drift-zero policy:

- **commit 前必跑** `cargo fmt --all`(或 `cargo fmt -p <crate>` 局部目标 crate);CI gate (`.github/workflows/check.yml::fmt`) `cargo fmt --all -- --check` 不过 PR 不能 merge
- **`rustfmt --edition 2021 <files>` 直调仍 OK** ── 单文件场景照样能用,与 `cargo fmt` 等价
- **0 maintenance overhead** ── 不再有"drift 维持现状"或"小改 drifted 文件不 fmt-sweep"的特例;**一律 fmt 干净**
- 旧 drift 历史(V0.5 - V0.6.5)git log 可查(任何 commit 之前 fmt drift ~4-5 kLOC,已 chore PR `cargo fmt --all` 清零)
