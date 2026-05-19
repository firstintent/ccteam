# V0.3.1 新 session 开发交接提示词

> **Session 边界文档**:把整段从 `# 你接手的工作` 起复制进新 Claude Code session
> 第一条消息(或写进 system prompt),新 session 即可在主仓 `~/workplace/ccteam`
> 续跑 V0.3.1 的 F48-F51,完事 ship V0.3.1.

---

# 你接手的工作

你接手 **ccteam V0.3.1 patch round 的剩余 4 个 finding(F48-F51)的开发工作**。
前 2 个 finding(F46 + F47)已经在另一个 Claude session 里 ship 完毕,你按
现成的 `docs/versions/v0-3-1/{prd.md,dev-plan.md}` 节奏跑剩余 4 个 PR,之后 ship V0.3.1。

## 1. 仓库 + 当前状态

- **本地仓库**:`git rev-parse --show-toplevel`(本会话 cwd 即是)
- **`origin`**:`git@github.com:firstintent/ccteam.git`
- **main HEAD**:看 `git rev-parse origin/main`(交接时 = `ac8b428`)
- **workspace.version**:`0.3.0`(F51 bump 到 `0.3.1`)
- **测试 baseline**:`cargo test --workspace` 应显示 **797 passed / 0 failed**
- **clippy baseline**:`cargo clippy --workspace --all-targets -- -D warnings` 9 errors
  (origin/main 同步基线;不能新增)
- **CLAUDE.md**:137 行(<250 cap),勿 sweep
- **rustfmt**:`rustfmt.toml` 已 pin(stable / max_width 100 / 4-space);新代码必 fmt-clean,不 sweep 旧 drift

## 2. V0.3.1 状态地图(交接时)

| F | 内容 | 状态 | PR | commit |
|---|---|---|---|---|
| **F46** | `HarnessAdapter` trait + `ClaudeCodeAdapter` + statusline wrapper + web SSE | ✅ merged | #46 | `e315696` |
| **F47** | `CodexAdapter` stub + `team.yaml::sessions[]` + `ccteam session` CLI parser stub | ✅ merged | #49 | `ac8b428` |
| **F48** | `kind: flex` team kind(orchestrator behavior gating + team factory `--kind=flex`)| ⏳ pending | — | — |
| **F49** | Adhoc multi-session primitives(`ccteam session {add,ls,attach,rm}` 全实现 + tmux `<slug>-<sid>` + 文件布局) | ⏳ pending | — | — |
| **F50** | Web 层更新(`kind` 列 / per-session cards / harness badge / SSE 加 sid 过滤 / 截图 `<slug>-<sid>.png`)| ⏳ pending | — | — |
| **F51** | chore + ship gate(version 0.3.0 → 0.3.1 + README + CLAUDE.md baseline + e2e + retro)| ⏳ pending | — | — |

依赖图(详 dev-plan §1.1):
```
F48 (kind: flex)         (独立)
F49 (multi-session)  ←── F48 (kind 必须存在)
F50 (web flex)       ←── F46 + F49
F51 (ship gate)      ←── F48 + F49 + F50
```

## 3. 读啥(优先级排序)

**起手 60 秒 onboarding**(每次 session 开机都跑):
```bash
cd "$(git rev-parse --show-toplevel)"
git fetch origin && git rev-parse origin/main
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6}END{print p" / "f}'
# 期望 797 / 0(F47 ship 后)
gh pr list --state open --json number,title  # 看有没有 in-flight PR
```

**必读文档**(按顺序):
1. `CLAUDE.md` — 整文件(137 行,5 分钟)。重点:
   - §三 红线(progress.jsonl SoT / 永不主动 kill 长 session / ccteam-core 不出现 team 名字面量 / `ccteam` 二进制名 — 不是 cct,F44 已反向回滚 F39)
   - §五 patch 流程 + worktree 约定
   - §六 实战累积(env-mutating tests 放 `crates/*/tests/*.rs` / `disable_tool_surface_bootstrap_for_tests` 必先调)
   - §七 Rust fmt 约定
2. `docs/versions/v0-3-1/prd.md`(1254 行,关键章节按 finding 跳读):
   - §1 战略 pivot(为啥 V0.3.1 加 flex team kind)
   - §5 F48 / §6 F49 / §7 F50 / §8 F51(对应 finding 全文)
   - §9 已知风险 / §10 V0.4 deferred / §11 PR sequencing / §13 CLAUDE.md baseline 更新 plan(F51 落)
3. `docs/versions/v0-3-1/dev-plan.md`(599 行):
   - §1.1 依赖图(上面贴了一份)
   - §4 PR #3 = F48 / §5 PR #4 = F49 / §6 PR #5 = F50 / §7 PR #6 = F51 任务清单
   - §8 worktree subagent briefing 模板(只完整给了 F46 的;F48-F51 你照模板增量化)
4. 历史归档(只在改架构红线 / 团队 yaml schema 时翻):
   - `docs/versions/v0-2-2/prd.md` — V0.2.2 patch 模板(本次 V0.3.1 follow 它)
   - `docs/versions/v0-3/prd.md` — V0.3 PRD(M5.0-M5.4 ship 实情)
   - `docs/research/v0-3-1-harness-adapter-plan.md` — 设计起点(已被 prd 吸收)
   - `docs/research/thin-harness-fat-skills-architecture-improvement.md` — 战略论证
   - `docs/research/ccteam-codex-integration.md` — Codex 适配 V0.3.2 输入
5. 协议参考(改 schema / CLI / hooks 时同步):
   - `docs/interfaces.md` §1.1 / §5.5 / §15(web routes)
   - `docs/tech-design.md` §3.7 / §3.8 / §6

## 4. 工作流(autonomous loop)

用户的偏好(从前一个 session 学到):
- **autonomous loop 默认开**:doc-first → dev subagent → review → fix → merge → 派下一个 finding,无需逐 PR 等用户确认
- **撞 design blocker / 红线 ambiguity / scope creep 才 ping** 用户
- 每个 finding 完事(merge 后)给一份**简报**到 Telegram,说清测试数 / 关键 judgment call / 红线状态
- 用户在 Telegram 同步 push 一些 research note / parallel PR 到 main(`docs/research/*.md`),你拉 main 时 fast-forward 吸收即可

每个 finding 的标准节奏:
1. **读 PRD §对应章节 + dev-plan §对应章节**,把任务列表心里过一遍
2. **派 dev subagent**(`Agent` 工具,`subagent_type: general-purpose`,`run_in_background: true`):
   - briefing 模板照 dev-plan §8 抄,加本 finding 的具体任务清单
   - 强调:(a) `git push origin <branch>` 在每个增量 commit 之后立刻执行 — 防 budget cliff 丢工作;(b) commit cadence 拆细(2-4 个增量,不一锅出);(c) 红线 grep 矩阵 commit 前必跑
3. **agent 完事 → 你 review**:
   - `gh pr view N --json mergeable,additions,deletions,changedFiles`
   - `cd <worktree> && cargo build --workspace && cargo test --workspace`
   - `cargo tree -p ccteam-web | grep ccteam-cli` = 0(红线)
   - `cargo clippy --workspace --all-targets -- -D warnings | grep -c "^error"` ≤ 9
   - 红线 grep:`git grep -nE '\bcct\b' -- 'crates/' 'README.md' 'CLAUDE.md' 'docs/tech-design.md' 'docs/interfaces.md' ':!docs/versions/v0-2-2/' ':!docs/versions/v0-1/' ':!docs/versions/v0-2/' ':!docs/dev-coupling-audit.md'` 应该返回 0(post-F44 后 forward-looking 不应有 `cct` 二进制名)
   - 撞 stray `cct` 引用:edit 修了再 commit 一份 fixup
4. **mergeable 状态**:`gh pr view N --json mergeable`
   - `MERGEABLE` → 直接 squash-merge
   - `CONFLICTING` → 用户在 main 上推了 parallel PR(常见,他爱 push research notes / pane snapshot fixes 等),进 worktree `git merge origin/main`、解冲突(通常是 `routes/mod.rs` 的 router 加 row、`interfaces.md` 路由表加 row,都是新加 row 没有语义冲突,保留两边)、push、再 `gh pr merge`
5. **squash + delete remote branch**:`gh pr merge N --squash --delete-branch`
   (本地 branch 因为 worktree 占用会删不掉;`git worktree remove` 后再 `git branch -D` 收尾)
6. **fast-forward main**:`git fetch origin && git pull --ff-only origin main`
7. **Telegram 简报**(用 reply 工具,chat_id 339498819):测试数 / 关键 judgment call / 下一个派啥
8. **派下一个 finding**

## 5. dev subagent briefing 模板(F48-F51 用)

直接套用,把 `<F##>` / `<MILESTONE_NAME>` / `<dev-plan §X 路径>` / `<具体任务清单>` 替换:

```
You are implementing **V0.3.1 PR #<N> — F<##> <MILESTONE_NAME>** for ccteam.
Previous V0.3.1 PRs (F46 #46, F47 #49, ...) merged on main. Your scope is
strictly the §F<##> finding documented in `docs/versions/v0-3-1/prd.md` §<N>.

## 0. Repo + worktree

main HEAD = `<commit>`. Baseline = <N> tests / 0 failed.

```
cd "$(git rev-parse --show-toplevel)"
git fetch origin
git worktree add -b v0-3-1-<branch> /tmp/ccteam-v031-f<##> origin/main
cd /tmp/ccteam-v031-f<##>
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6}END{print "baseline:", p" / ", f}'
```

## 1. Required reading
- `CLAUDE.md` §三 + §五 + §六 + §七
- `docs/versions/v0-3-1/prd.md` §<N>(F<##> 全文)
- `docs/versions/v0-3-1/dev-plan.md` §<N>(任务列表 + 验收)
- `docs/versions/v0-3-1/dev-plan.md` §8.1(通用前置 — 已知)
- 已 ship 的相关 finding:F46 → `crates/ccteam-core/src/harness.rs`,F47 → `crates/ccteam-core/src/team.rs::TeamSpec::sessions`

## 2. Scope(F<##> only — do not creep)
<具体任务清单,从 dev-plan §<N>.1 复制粘贴>

## 3. Architectural red lines
- progress.jsonl SoT — orchestrator 不读 HarnessSnapshot
- ccteam-web 不依赖 ccteam-cli — `cargo tree -p ccteam-web | grep ccteam-cli` 必须 0
- ccteam-core 不出现 team 名字面量
- 不动 CLAUDE.md / README / Makefile / workspace.version(F51 才动)
- 二进制名一律 `ccteam`(post-F44),不要写 `cct`
- 新文件 fmt clean(`cargo fmt -- <new files>`);不 sweep drifted 文件

## 4. Discipline
- `cargo test --workspace` >= baseline + 新增
- `cargo clippy --workspace --all-targets -- -D warnings` <= 9 errors
- 增量 commit + push:每完成一个子任务就 commit + push,不要一锅
- Commit message English / HEREDOC / 含 `Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>`
- NEVER `--no-verify` / `--amend`

## 5. Final deliverable
1. PR URL
2. 最终测试数(formatted as `N passed / 0 failed`)
3. `cargo tree -p ccteam-web | grep ccteam-cli` 输出(must be empty)
4. Worktree 路径(`/tmp/ccteam-v031-f<##>`)
5. SHORT(<150 词)judgment calls 摘要

撞 blocker 立即 push 已 commit 的 progress + 报告问题,不要硬撑。
```

## 6. F48 / F49 / F50 / F51 关键 task list 速查

(详 `docs/versions/v0-3-1/dev-plan.md` §4-§7;以下只列高密度要点)

### F48 — `kind: flex` team kind
- `crates/ccteam-core/src/team.rs::TeamSpec` 加 `pub kind: TeamKind` field,`#[serde(default)]` → `Workflow` 保 V0.1-V0.3 yaml 兼容
- `pub enum TeamKind { Workflow, MultiWorkflow, Flex }`(注意:V0.3 已有 `parallelism: multi_session` 走 multi_workflow 旧路径,F48 不动它,只是把它和新加的 flex 在 kind 这一新轴上对齐)
- `crates/ccteam-core/src/orchestrator.rs` 行为 gating:
  - flex team:auto-loop / golden_rules / phase prompt injection **off**
  - silence_classifier / cost watcher / progress.jsonl / hooks **on**
- 团队 factory `cct team create <name> --kind=flex` → 生成空 `phases: []` + `sessions: [{sid: "claude-1", harness: claude}]` 的 team.yaml(后者作为默认入口 session,F49 落地后用户可 add/rm)
- 测试:5+ 新测(yaml round-trip kind 各值 / orchestrator decide_tick 在 flex 不进 phase / factory `--kind=flex` 输出形态)
- **不做**:F49 的 session add/ls/rm 逻辑;F50 的 web 层 `kind` 列(那是 F50 scope)

### F49 — Adhoc multi-session primitives
- `ccteam session add <slug> --harness=claude` 完整实现:tmux `ccteam-<slug>-<sid>` / per-session subdir `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/` / per-session progress `~/.ccteam/progress/<slug>/<sid>.jsonl`(flex 用子目录,非-flex 仍 flat) / master state.json::sessions[] 写入 / hooks dispatch by `state.json::team_kind`
- `ccteam session ls <slug>` / `attach <slug> <sid>` / `rm <slug> <sid>`(rm 走 F46 已实现的 `ClaudeCodeAdapter::shutdown_session`,这是 V0.3.1 唯一的 auto-kill 路径,**只允许用户显式触发**)
- `--harness=codex` 已经 F47 stub error path,本 PR 不动
- sid 格式 `<harness>-<n>`,monotonic + 不复用,`next_sid_seq` BTreeMap on master state
- progress.jsonl 重新拼 reader:flex 项目下读 `<slug>/<sid>.jsonl` glob;非-flex 兼容老路径
- 测试:10+ 新测(session add 落 tmux + state / ls 输出 / attach 是 thin wrapper 易测 / rm graceful + state 清 / sid 不复用 / mixed harness 失败 codex 仍 stub)
- 文档:`docs/interfaces.md` §1.3 加 flex 多 session 文件布局

### F50 — Web 层更新
- dashboard `/`:加 `kind` 列;flex 行 `current_phase` 显示 `manual` 或 `—`
- project 详情 `/project/<slug>`:flex 项目下显示 N 个 session card(harness badge / sid / 状态 / cost),每个 click 进新 `/session/<slug>/<sid>` 子页
- `/session/<slug>/<sid>` 详情页:per-session 事件流 / harness snapshot 卡片 / pane 截图 / 写动作 form(`/btw` / `pause` / `resume` 路由扩 `<sid>`)
- `/sse/project/<slug>` 加 `?sid=` 过滤(or 新加 `/sse/session/<slug>/<sid>`,选简洁的)
- `/screenshot/<slug>-<sid>.png` 走 F38 截图 + 多 session 路径
- 模板:扩 `project.html`(branch on `state.kind`)+ 新建 `session.html`
- 测试:8+ 新测(flex 项目 dashboard 渲 kind 列 / session card 渲 / session 详情 200 / SSE sid 过滤)

### F51 — chore + ship gate
- `Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"`
- `cargo build --workspace` 刷 Cargo.lock
- `CLAUDE.md` §一 baseline 表格回填:测试数(F46+F47+F48+F49+F50 累计;最后实测 paste)/ workspace version 0.3.1 / V0.3.1 milestone 行加(F46-F51 五 finding 简述);**注意 250 行 cap**
- `README.md` §Web dashboard(M5.4 已加)再加一段说明 flex team + multi-session 入口示例
- `docs/versions/v0-3-1/e2e-retro.md` 落档(模仿 `docs/versions/v0-2-2/e2e-retro.md` 模板 / 4-suite 验证)
- `docs/versions/v0-2/README.md` "V0.3.1 起始 (drafting)" → "V0.3.1 ship"
- e2e 测试:flex 项目 + 多 session + harness snapshot 完整链路
- 红线 grep 矩阵跑过(详 PRD §13 + 红线 grep 命令上面贴过)
- commit subject `v0.3.1: F51 e2e + retro + ship gate (V0.3.1 ship)`,body 列 closes F46-F51 + #PR-list

## 7. 常见血泪经验

- **budget cliff 接力**:agent 没 commit 就 budget out → 你 cd worktree、检查 `git status` + `git log`,把 uncommitted edits stage + commit + push 自己保留,然后派 continuation subagent 续做剩余 task;agent commit 了但没 push → 直接 `git push origin <branch>` 帮它推
- **冲突解决**:用户在 main 上 push parallel PR 是常见(他在 push research notes / pane-snapshot fixes / meta-agent 改进等),触发 `MERGEABLE: CONFLICTING`。常见冲突点是 `crates/ccteam-web/src/routes/mod.rs` 的 router 加 row、`docs/interfaces.md` §15 路由表加 row,都是新增 row 没有真正语义冲突,保留两边即可。`git merge origin/main` → 解冲突 → push → `gh pr merge` 即可
- **`disable_tool_surface_bootstrap_for_tests()` 必先调**:任何调 `bootstrap_project` / `bootstrap_meta_project` 的测试,前面没这行就会向真实 `~/.claude.json` + `~/.claude/agents/` 写垃圾,长期撑大 `.claude.json` 会破坏 claude 登录(2026-05-06 实测)
- **env-mutating 测试**(`set_var/remove_var CLAUDE_CONFIG_HOME` / `HOME` 等):放 `crates/*/tests/*.rs` integration,各独立进程,**不**放 lib `#[cfg(test)] mod tests`(同 binary 内其他测试读 env 会 race)
- **`cct` 二进制名是 F39 旧约定,F44 反向回滚,二进制名一律 `ccteam`**:新 / 改的代码 / 文档 / 模板里出现 `cct ` / `cct-<某 skill 名>` 都要替成 `ccteam ` / `ccteam-<...>`(`ccteam-control` / `ccteam-team-author` / `ccteam-project-creator` 三个 skill 名是 ccteam-prefix);唯一例外是 `LEGACY_SKILL_NAMES` const + 历史 doc(`docs/versions/v0-2-2/` / F39 / F44 段落)留 `cct-*` 描述历史
- **CLAUDE.md ≤ 250 行**,每加一行内容(F51 baseline 回填会加)就考虑要不要压老 V0.2.2 / V0.3 行的描述,超 250 cache 不命中

## 8. 用户偏好(从前一个 session 学到)

- **handle 是 `cto`**:`make setup HANDLE=cto` / `tmux attach -t ccteam-meta-cto`
- **autonomous review-fix-merge loop**:不需要逐 PR 等用户确认;撞 design ambiguity 才 ping
- **doc 用中文,commit message + PR title/body 用英文**(CLAUDE.md §五.2)
- **`worktree-per-PR`**:每个 finding 单独 `git worktree add`,主仓 main 始终 clean
- **Telegram chat_id `339498819`** — 用 `mcp__plugin_telegram_telegram__reply` 工具 reply,format=text,chat_id 必带
- **每个 finding ship 后简报**:测试数 / 净增 / 关键 judgment call / 下一步派啥;**不要**长报告
- 用户偶尔 push parallel research / fix PR 到 main(常见 in-flight)— 拉 main 时 fast-forward 吸收

## 9. Telegram 输出格式范本

合 PR 后简报:
```
F<##> merged ✅(`<commit>`)— <一句话总结>

- Tests <baseline> → **<final>**(+<delta>:<分类细分>)
- 关键 judgment call:<1-2 条精简>
- 红线状态:<dep graph / cct sweep / clippy>

派 F<下一个>(<scope 一句话总结>)。
```

撞 blocker:
```
F<##> 撞 <类型>:<具体问题>

<选项 a / b / c 简述,推荐哪个 + why>

回字母,我继续。
```

## 10. ship checkpoint

V0.3.1 ship 当且仅当 F51 PR merge 后:
- `Cargo.toml::workspace.package.version = "0.3.1"`
- CLAUDE.md §一 baseline 表 V0.3.1 milestone 行存在
- `docs/versions/v0-3-1/e2e-retro.md` 存在
- `docs/versions/v0-2/README.md` "V0.3.1 ship" 字样
- `cargo test --workspace` 全绿(F46+F47 净增 51 + F48 ~10 + F49 ~15 + F50 ~10 + F51 e2e ~3-5 = 870-890 左右)
- `cargo tree -p ccteam-web | grep ccteam-cli` = 0
- 一份 V0.3.1 ship 报告发 Telegram

如果 F48-F51 全 ship 完事,V0.3.1 ship 完了,你可以再写一份**新 session 交接提示词
针对 V0.3.2**(用户已经设计 codex 适配的真实实现走 V0.3.2;详 `docs/research/ccteam-codex-integration.md`),格式同本文,放到 `docs/research/v0-3-2-handoff-prompt.md`。

---

# 立即开始

```bash
# 1. 起手 onboarding
cd "$(git rev-parse --show-toplevel)" && git fetch origin && git pull --ff-only origin main
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6}END{print p" / "f}'
# 期望 797 / 0(F47 ship 后)— 若不是,先排查

# 2. 派 F48
# 用 Agent tool / general-purpose / run_in_background=true / 套上面 §5 模板
# 任务清单从 docs/versions/v0-3-1/dev-plan.md §4 复制(F48 部分)
# 完事按 §4 工作流 review/merge,然后派 F49 → F50 → F51

# 3. 全部 merge 后,Telegram 发 V0.3.1 ship 报告
```

加油,V0.3.1 距 ship 只剩 4 个 PR + 1 个 ship gate。
