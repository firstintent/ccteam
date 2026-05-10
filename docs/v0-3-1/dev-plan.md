# V0.3.1 开发计划

> V0.3.1 patch round 实施计划。按依赖顺序拆 6 个 PR(F46-F51,每 finding 一
> PR,最后 F51 是 ship gate chore)。worktree-per-PR;每 PR 一份 subagent
> briefing。
>
> 配套文档:
> - 需求决策:`docs/v0-3-1/prd.md`(13 节)
> - 文档索引:`docs/v0-3-1/README.md`
>
> base = `origin/main` `f9baf3f`(V0.3 ship);测试 baseline `738/0`;
> workspace.version 起点 `0.3.0`。
>
> 跟 V0.3 dev-plan 不同点:V0.3.1 是 patch round 用 F-编号(V0.2.2 模式),
> 不 bump main version,只 patch(0.3.0 → 0.3.1)。本文档**只完整给出 F46
> 的 subagent briefing**(立 trait + statusline wrapper 是基础),后续 PR 的
> briefing 各自由派工 dispatcher 在 F46 模板基础上增量化(参考 PRD 对应章节
> + 本文档 §3-§7 任务清单)。

---

## 1. PR 总览

| # | finding | branch | 工程量估 | 主要前置依赖 |
|---|---|---|---|---|
| **PR #1** | **F46** HarnessAdapter trait + ClaudeCodeAdapter | `v0-3-1-harness-adapter` | ~400 LoC + ~12 测试,~3-4 天 | 无(立 trait,后续都基于) |
| **PR #2** | **F47** CodexAdapter stub + harness 字段 | `v0-3-1-codex-stub` | ~150 LoC + ~5 测试,~1-2 天 | PR #1(消费 trait)|
| **PR #3** | **F48** kind: flex team kind | `v0-3-1-flex-kind` | ~250 LoC + ~10 测试,~2-3 天 | 无(独立 schema 加;推荐 #2 后做 sessions[] 字段定下) |
| **PR #4** | **F49** Adhoc multi-session primitives | `v0-3-1-multi-session` | ~400 LoC + ~15 测试,~3-4 天 | PR #1(spawn_session)+ PR #3(kind: flex 项目才能 add) |
| **PR #5** | **F50** Web 层更新 | `v0-3-1-web-flex` | ~300 LoC + ~8 测试,~2-3 天 | PR #1(harness SSE)+ PR #4(per-session) |
| **PR #6** | **F51** chore + ship gate | `v0-3-1-ship-gate` | ~150 LoC + 文档,~1-2 天 | PR #1-#5 全部 merge |

**总计**:~1.65 kLOC + ~50 测试,~12-15 天(单人 5 天/周即 2.5-3 周;
PR #2/#3 + PR #4/#5 并行可压到 ~10 天)。

### 1.1 依赖图

```
PR #1 (F46 HarnessAdapter)  ── trait + ClaudeCodeAdapter,foundation
   ↓
PR #2 (F47 CodexAdapter stub)            (并行起点;只依 trait)
   ↓
PR #3 (F48 kind: flex)                   (不强依 #1/#2;推荐 #2 后定 sessions[])
   ↓
PR #4 (F49 multi-session) ───┐
   ↓                          │
PR #5 (F50 web flex) ────────┴──► PR #6 (F51 ship gate)
```

并行机会:**PR #2 vs #3** 同时起 worktree(touch 不同模块);**PR #4
backend vs PR #5 frontend** 同时起 worktree(冲突点只在 askama template +
SSE 路由)。

---

## 2. PR #1 — F46 HarnessAdapter trait + ClaudeCodeAdapter

> **目标**:立 `crates/ccteam-core/src/harness.rs` 模块,trait + 数据结构 +
> 错误类型 + `ClaudeCodeAdapter` 完整实现 + statusline wrapper 安装入口
> + web SSE harness endpoint。**Foundation PR — 必先 merge**。

**关联 PRD**:§3(F46 全文)+ §1.3 战略论证(thin-harness research doc)

**前置**:无。

### 2.1 任务

- [ ] **#1.1** 新模块 `crates/ccteam-core/src/harness.rs`
  - `HarnessAdapter` trait(`name` / `ingest_snapshot` / `subagent_states` /
    `spawn_session` / `shutdown_session`)
  - `HarnessSnapshot` / `SubagentState` / `SpawnOpts` / `SessionHandle`
    数据结构(详 PRD §3.2.2)
  - `HarnessError` enum(`thiserror::Error` 派生);`NotImplemented` 变体的
    `harness` 与 `reason` 都是 `&'static str`(让 `CodexAdapter` 用 const
    string 不必 alloc)
  - `lib.rs` re-export:`pub use harness::{HarnessAdapter, HarnessSnapshot,
    SubagentState, HarnessError, ClaudeCodeAdapter};`
- [ ] **#1.2** `ClaudeCodeAdapter` 实现
  - `name() -> "claude-code"`
  - `ingest_snapshot(raw)`:解析 statusline stdin JSON;解析失败返
    `IngestFailed(detail)`;成功返 `HarnessSnapshot { harness: "claude-code",
    model_display_name, context_used_pct, cost_usd_total, rate_limit_pct,
    cwd, raw, captured_at }`
  - `spawn_session(opts)`:tmux new-session 启动 claude `--dangerously-skip-permissions`
    + cwd 切到 `~/projects/<team>-<slug>/`(F49 落地后改 `<sid>` subdir);
    返 `SessionHandle { tmux_session, harness, sid, pid, started_at }`
  - `shutdown_session(handle)`:tmux kill-session(graceful — 先 send-keys
    `/exit\n` 等 5s,再 kill);只在用户 `ccteam session rm` 时调
- [ ] **#1.3** statusline wrapper 安装(doctor flag)
  - `crates/ccteam-cli/src/commands.rs::run_doctor` 加 `--install-statusline-adapter`
    flag(可选,doctor 默认调用时也跑一次幂等)
  - 写 `~/.claude/statusline-command.sh` wrapper:
    - 检测原文件;有 → backup 到 `.bak-<utc-ts>`,写入 wrapper(包含 marker
      section + 透传到 `/path/to/orig.bak-<ts>`)
    - 无 → 直接写 wrapper 不透传
  - marker section:`# ccteam-managed:statusline begin` / `... end`
  - wrapper body:`tee` stdin 到 `~/.ccteam/harness/<slug>-<sid>.json`(slug/sid
    从 cwd 推导;推不到则 `_meta-<handle>.json`)+ 透传 stdin 到 stdout
- [ ] **#1.4** harness JSON dual-write 路径推导
  - 新 helper `crates/ccteam-core/src/harness/path.rs::derive_harness_path(cwd, paths) -> PathBuf`
  - cwd 匹配 `~/projects/<team>-<slug>/sessions/<sid>/...` → `<harness-dir>/<slug>-<sid>.json`
  - cwd 匹配 `~/projects/<team>-<slug>/...`(无 sessions/ 子层)→
    `<harness-dir>/<slug>-claude-1.json`(单 session 项目默认)
  - cwd 匹配 `~/projects/<handle>-meta/...` → `<harness-dir>/_meta-<handle>.json`
  - 其他 → 丢弃(返 None,wrapper 检测 None 不写)
- [ ] **#1.5** web SSE harness endpoints
  - `crates/ccteam-web/src/routes/harness_sse.rs` 新建:
    - `GET /sse/harness/<slug>`:推该 slug 下所有 session harness_snapshot
    - `GET /sse/harness/<slug>/<sid>`:单 session
  - watcher 后台 task 监 `~/.ccteam/harness/`(notify recursive)→ broadcast
    channel(`tokio::sync::broadcast`,bound 1024,同 V0.3 progress watcher
    模式 — 可考虑独立 channel 也可复用 EventBus 加 `EventKind`)
  - SSE wire format:`event: harness_snapshot` + `data: <one-line-JSON>`(同
    V0.3 progress wire format,加 `slug` + `sid` server 字段)
- [ ] **#1.6** 测试
  - `harness.rs` 单元:`HarnessSnapshot` round-trip serialize / deserialize;
    `derive_harness_path` 5 种 cwd 形态(单 session / multi-session /
    meta / mismatch);`ClaudeCodeAdapter::ingest_snapshot` 解析 5 种 stdin
    JSON shape(官方 + 用户魔改)
  - `crates/ccteam-cli/tests/statusline_install_test.rs`:
    - 无原 statusline 文件 → wrapper 直接写;marker 包裹 dual-write
    - 有原 statusline 文件 → backup + 透传 callsite 写入
    - 重跑 doctor → marker section 覆盖,backup 不重复
  - `crates/ccteam-web/tests/harness_sse_test.rs`:reqwest stream
    `/sse/harness/<slug>`,另一 thread append `~/.ccteam/harness/<slug>-claude-1.json`
    after,断 ≥ 1 event 收到 + 字段 verify

### 2.2 验收(摘 PRD §3.4)

- [ ] `crates/ccteam-core/src/harness.rs` 模块落地,trait + 数据结构 + 错误
  类型 + `ClaudeCodeAdapter` 实现完整
- [ ] `ccteam doctor --install-statusline-adapter` 安装 wrapper(marker 保护
  + 原文件 backup)
- [ ] `~/.ccteam/harness/<slug>-<sid>.json` 文件 dual-write happy path
- [ ] `/sse/harness/<slug>` + `/sse/harness/<slug>/<sid>` SSE 推送
  harness_snapshot 事件
- [ ] 用户已有自定义 statusline 脚本时,wrapper tee + 透传不破坏原 footer
- [ ] 路径匹配不到 slug 时,fallback 不 panic
- [ ] `cargo test --workspace` baseline 738 不退步;新增 ≥ 12 测试

### 2.3 文档同步

- `docs/interfaces.md` §15(web routes,V0.3 已写)加 `/sse/harness/<slug>` /
  `/sse/harness/<slug>/<sid>` schema
- `docs/interfaces.md` §1.1 全局目录加 `~/.ccteam/harness/<slug>-<sid>.json`
- `docs/dev-coupling-audit.md` F46 加(详 §9):"V0.3.1 HarnessAdapter trait
  抽象 + ClaudeCodeAdapter 实现"
- `docs/tech-design.md` §6 扩展点表加 harness layer 行(简短 placeholder,
  F51 补全)

---

## 3. PR #2 — F47 CodexAdapter stub + harness 字段

> **目标**:`CodexAdapter` 全 stub(所有方法返 `NotImplemented`,reason
> 指向 PRD §F47);`team.yaml::sessions[]` schema(`harness: claude | codex`);
> `ccteam session add --harness codex` CLI 接受;doctor codex 检测。

**关联 PRD**:§4(F47 全文)+ §3.2.2 trait shape

**前置**:PR #1(消费 trait)。

### 3.1 任务

- [ ] **#2.1** `CodexAdapter` 实现 — `harness.rs` 加空 struct + impl trait
  全部方法返 `NotImplemented`(详 PRD §4.2.1)
- [ ] **#2.2** `team.yaml::sessions[]` schema —
  `crates/ccteam-core/src/team.rs::TeamSpec` 加 `pub sessions:
  Vec<DefaultSessionSpec>` + `DefaultSessionSpec { sid, harness:
  HarnessKind }` + `HarnessKind { Claude, Codex }`(详 PRD §5.2.1
  Rust 字段定义)。**注**:F48 PR #3 才加 `kind` 字段;F47 PR 只加
  `sessions[]` 与 `HarnessKind`(独立可并行,不撞 #3)
- [ ] **#2.3** `ccteam session` CLI 子命令 stub
  - **F49 PR 才完整实现** `add/ls/attach/rm`,本 PR(F47)只立 CLI parser
    + `--harness` flag schema(`clap::ValueEnum` 派生)+ `add` 调
    `CodexAdapter::spawn_session` 时返 friendly error(实测 stub error path)
  - 注意:F47 PR ship 时 `ccteam session add --harness claude` 会走
    `ClaudeCodeAdapter::spawn_session` — F46 已实现,但因 `state.json::sessions{}`
    schema 在 F49 才落,本 PR 不真正写 master state(只能 dry-run / smoke
    test on `--harness=codex` error path)。建议 F47 PR 实测 codex error
    branch 即可,claude branch 在 F49 PR 完整 happy path
- [ ] **#2.4** `ccteam doctor` codex 检测段
  - `which codex` → 输出 `[ccteam] codex CLI: present @ <path>` /
    `[ccteam] codex CLI: not found (V0.3.1 trait-stub only; install codex
    CLI for V0.3.2+ — see docs/research/ccteam-codex-integration.md)`
  - 不 fail 任何条件
- [ ] **#2.5** 测试
  - `harness.rs::CodexAdapter::spawn_session` 返 `NotImplemented` + 错误
    消息含 "V0.3.1" / "deferred to V0.3.2" / "docs/v0-3-1/prd.md §F47"
  - `team.yaml` parse `sessions: [{sid: claude-1, harness: claude}]` 成功
    + round-trip
  - `team.yaml` parse `sessions: [{sid: codex-1, harness: codex}]` 成功
    (schema 接受 codex 不 fail)
  - `ccteam session add <slug> --harness codex` exit 1 + stderr 含 stub
    错误消息

### 3.2 验收(摘 PRD §4.4)

- [ ] `CodexAdapter` 全 stub,error 消息含 V0.3.2 引用
- [ ] `team.yaml::sessions[]` schema 解析 ≥ 5 fixture
- [ ] `ccteam session add --harness codex` 友好 error path
- [ ] doctor codex 检测段输出 informational
- [ ] interfaces.md §5.5 schema 同步加 `sessions[]` 字段
- [ ] 新增 ≥ 5 测试

### 3.3 文档同步

- `docs/interfaces.md` §5.5 加 `sessions[]` 字段
- `docs/dev-coupling-audit.md` F47 加

---

## 4. PR #3 — F48 kind: flex team kind

> **目标**:`team.yaml::kind` 字段(默认 `workflow`,V0.1/V0.2/V0.3 yaml
> 不动);orchestrator behavior gating helpers;team factory `--kind=flex`
> scaffold(无 phases/);claude_md_template seed for flex 团队。

**关联 PRD**:§5(F48 全文)

**前置**:无强依赖(独立 schema 加);**推荐**在 PR #2 之后 — 让
`sessions[]` 字段在 PR #2 已定,这里只加 `kind` + 验证组合。

### 4.1 任务

- [ ] **#3.1** `team.rs::TeamSpec` 加 `kind: TeamKind` 字段(详 PRD §5.2.1);
  `TeamKind` enum(`Workflow` / `MultiWorkflow` / `Flex`);`#[serde(default)]`
  保 V0.1/V0.2/V0.3 yaml parse 不变
- [ ] **#3.2** `TeamSpec::validate` 加 flex 团队约束:
  - `kind: flex` + 非空 `golden_rules` → fail-loud + msg 指向 PRD §5.2.1
  - `kind: flex` + 非空 `escalate_grammar_extensions` → 同上
  - `kind: flex` + 存在 `phase_dir` 对应非空目录 → 同上(team factory
    publish 时校验)
  - `kind: workflow / multi_workflow` + 非空 `sessions[]` → warn(不 fail;
    用户该字段只对 flex 有意义)
- [ ] **#3.3** orchestrator behavior gating(详 PRD §5.2.3)
  - `crates/ccteam-core/src/orchestrator.rs::TeamRuntime` 加三个 helper
    fn:`should_run_auto_loop` / `should_inject_phase` /
    `should_check_golden_rules`,按 `self.spec.kind` 返 bool
  - tick 循环里所有 `if state.team == ...` 字面字符串(若仍存在)替换
    为 helper 调用(grep 验证 — strategic doc §3 红线)
- [ ] **#3.4** team factory `--kind=flex`
  - `ccteam team init <name> --kind <kind>` clap 加 `--kind` flag(默认 workflow)
  - `team_factory.rs::init_team_staging` 检测 kind=flex 跳过 phase scaffold;
    生成 staging tree(plugin.json + team.yaml + README.md,**无** phases/)
  - team.yaml 默认 `kind: flex` + `description` 占位 + `sessions: [{sid: claude-1, harness: claude}]` 默认 +
    `claude_md_template` 写 flex 团队默认指令(详 PRD §5.2.4 模板要求)
- [ ] **#3.5** `ccteam team publish <name>` flex 团队成功 publish 到
  `~/.ccteam/teams/<name>/`;`doctor --validate-team <name>` 跳过 phase 引用
  校验(对 flex)
- [ ] **#3.6** seed flex team(可选 — 提一支 demo)
  - `teams/flex/`(repo 内 seed)— 不强求,但能让 e2e fixture 有可用 spec;
    若不 ship seed,F51 e2e 用 staging dir fixture 起也行
- [ ] **#3.7** 测试
  - `TeamSpec` round-trip serialize / deserialize 含 `kind`
  - `validate` 拒绝 flex + golden_rules / escalate_grammar_extensions / 非空
    phase_dir 的非法组合
  - V0.3 dev / research / meta-agent yaml 在 V0.3.1 升级后 parse 不变(回归
    fixture,5 个 spec round-trip 不破)
  - team factory `--kind=flex` 生成 staging tree(snapshot 测试)
  - orchestrator helpers 返值 unit:workflow → true/true/true,flex →
    false/false/false

### 4.2 验收(摘 PRD §5.4)

- [ ] `team.yaml::kind` 字段 default `workflow` parse 不变
- [ ] flex 团队 validate 拒绝非法组合
- [ ] orchestrator helpers 三个,kind-aware
- [ ] team factory `--kind=flex` scaffold 无 phases/
- [ ] V0.3 已 ship 团队完全无变化
- [ ] 新增 ≥ 8 测试

### 4.3 文档同步

- `docs/interfaces.md` §5.5 加 `kind` 字段
- `docs/dev-coupling-audit.md` F48 加

---

## 5. PR #4 — F49 Adhoc multi-session primitives

> **目标**:`ccteam session {add,ls,attach,rm}` CLI;per-session subdir;
> master state.json `sessions{}` + `next_sid_seq{}` 字段;tmux `<slug>-<sid>`
> 命名;`progress.jsonl` flex 项目子目录 scoping。

**关联 PRD**:§6(F49 全文)

**前置**:PR #1(spawn_session);PR #3(kind: flex 项目才能 add)。

### 5.1 任务摘要

- [ ] `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/` 子目录创建(F49 §6.2.1)
- [ ] master `state.json::sessions{}` + `next_sid_seq{}` 字段(F49 §6.2.2;
  serde default 空 BTreeMap 保 V0.3 单 session 项目兼容)
- [ ] sid 单调递增 + 删后不复用(`next_sid_seq` 计数器)
- [ ] tmux `ccteam-<slug>-<sid>` 命名;flex 项目第一个 session 也带 `-<sid>`
  后缀(workflow / multi_workflow 项目仍 `ccteam-<slug>` 不变,V0.3 兼容)
- [ ] `progress.jsonl` flex 项目走 `~/.ccteam/progress/<slug>/<sid>.jsonl`
  子目录(`hooks::progress_append` 解析 `state.json::team_kind` 自动分流;
  workflow 项目 flat path 不变)
- [ ] `ccteam session add <slug> [--harness X] [--sid Y]`:
  - 检测 team kind != flex → friendly error
  - 调 `<HarnessAdapter>::spawn_session`(F46/F47);写 master state.json
    `sessions[]`;tmux new-session;hooks 启动
  - sid 自动分配(`next_sid_seq[harness]++`)or 用户指定(校验唯一)
  - master state.json atomic write + retry-on-conflict(F49 §9.4 缓解)
- [ ] `ccteam session ls <slug>`:读 state.json `sessions{}` → 表格(sid /
  harness / tmux / started / pid / status from per-session progress.jsonl 末事件)
- [ ] `ccteam session attach <slug> <sid>`:tmux attach `ccteam-<slug>-<sid>`;
  不存在 → fail-loud + 列可用
- [ ] `ccteam session rm <slug> <sid>`:`shutdown_session`(graceful);tmux
  kill;清 state.json `sessions[sid]`;**唯一**显式 kill 路径(CLAUDE.md
  §三 红线)
- [ ] CLI 在 workflow / multi_workflow 团队上调 session 子命令 → friendly
  error("session subcommands only work on flex teams; this project is
  team `<name>` (kind=<kind>)")

### 5.2 测试(详 PRD §6.4)

- per-session subdir 创建
- state.json round-trip 含 sessions{} + next_sid_seq{};V0.3 单 session
  项目 state.json 不含俩字段(serde default)
- progress.jsonl flex 子目录路径 vs workflow flat 路径分流(team_kind 驱动)
- `add` happy path(claude harness)
- `add --harness codex` friendly error(F47 stub)
- `ls` 表格输出 + per-session status
- `attach` 命中 + miss 各自路径
- `rm` graceful shutdown + state 清理
- sid 单调递增 + 删后不复用(并发 add 单元测试,2 thread spawn)
- workflow 团队 `session add` → friendly error

### 5.3 文档同步

- `docs/interfaces.md` §1.3 / §2.1 schema 同步 flex layout + state.json
  sessions / next_sid_seq 字段
- `docs/dev-coupling-audit.md` F49 加

---

## 6. PR #5 — F50 Web 层更新

> **目标**:dashboard `kind` 列;flex 项目详情页 per-session cards + harness
> badges;新页 `/session/<slug>/<sid>`;SSE filter by sid;screenshot 扩展
> `<slug>-<sid>.png`。

**关联 PRD**:§7(F50 全文)

**前置**:PR #1(harness SSE);PR #4(per-session events / sid)。

### 6.1 任务摘要

- [ ] dashboard `dashboard.html`:加 `Kind` 列;flex 项目 `Phase` 列渲 `—`
- [ ] project 详情 `project.html`:flex 项目走新模板分支(N session cards
  + harness badge + 缩略截图 + Detail link);workflow / multi_workflow
  项目仍走 V0.3 单 panel 模板(回归保证)
- [ ] 新模板 `templates/session.html` + handler `routes/session.rs`:
  `GET /session/<slug>/<sid>` 渲染 header / events / harness panel /
  write actions sidebar
- [ ] SSE handler 加 sid filter:
  - `GET /sse/project/<slug>/<sid>` server-side `msg.sid == <sid>` 过滤
  - `EventMsg` 加 `sid: Option<String>` 字段(workflow 项目 None)
  - watcher tail 解析路径 `~/.ccteam/progress/<slug>/<sid>.jsonl` 时注入 sid
- [ ] screenshot endpoint 扩展:
  - `GET /screenshot/<slug>-<sid>.png` 路由 — `render_screenshot(slug, Some(sid), opts)`
  - F38 `render_screenshot` 签名加 `sid: Option<&str>` 参数;`<sid>` 决定
    tmux session name 与输出文件名;非 None 时实际 tmux session
    `ccteam-<slug>-<sid>`
  - `GET /screenshot/<slug>.png`(workflow 项目)路由保留 — 调
    `render_screenshot(slug, None, opts)`,V0.3 行为完全保持
- [ ] askama 模板编译期类型检查不破

### 6.2 测试(详 PRD §7.4)

- dashboard `Kind` 列 ≥ 6 fixture 项目(workflow / multi_workflow / flex
  各 2)
- flex 项目 `/project/<slug>` 渲 N session cards + harness badge
- workflow / multi_workflow 项目 `/project/<slug>` 完全不变(V0.3 e2e 测试
  不破)
- `/session/<slug>/<sid>` 200 + content
- SSE filter by sid:启 server,append progress 不同 sid 文件,断接收顺序
- screenshot endpoint `<slug>-<sid>.png` 200 / 404 / 504(F38 失败)
- screenshot endpoint `<slug>.png` workflow 项目 200(回归)

### 6.3 文档同步

- `docs/interfaces.md` §15(web routes)加 `/session/<slug>/<sid>` /
  `/sse/project/<slug>/<sid>` / `/sse/harness/<slug>` 等 endpoint schema
- `docs/dev-coupling-audit.md` F50 加

---

## 7. PR #6 — F51 chore + ship gate

> **目标**:V0.3.1 ship gate。flex_e2e_test.rs + retro 文档 +
> workspace.version bump 0.3.0 → 0.3.1 + CLAUDE.md baseline + docs sweep。

**关联 PRD**:§8(F51 全文)+ §12 / §13

**前置**:PR #1-#5 全部 merge。

### 7.1 任务摘要

- [ ] `crates/ccteam-web/tests/flex_e2e_test.rs`:tempdir CCTEAM_HOME,
  fixture 1 flex project + 2 sessions + mock harness JSON,reqwest 跑 happy
  path(详 PRD §8.2.1)
- [ ] `Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"`;commit
  subject `v0.3.1:` 前缀
- [ ] CLAUDE.md §一 baseline 表格更新(workspace.version + 实测测试数 +
  V0.3.1 milestone 行;详 PRD §13)
- [ ] CLAUDE.md §六 加 V0.3 → V0.3.1 升级注
- [ ] `docs/v0-3-1/e2e-retro.md` 落档(模仿 V0.3 / V0.2.2 retro 模板;
  4-suite 跨 flex 多 session / harness adapter / web UI / codex stub)
- [ ] `docs/v0-2/README.md` V0.3 起始 pointer 更新为"已 ship V0.3 + V0.3.1"
- [ ] `docs/dev-coupling-audit.md` F46-F51 close 标记(改 `2026-05-10 加;
  待 V0.3.1 ship` 为 `... 2026-05-10 加;V0.3.1 PR #N ship 已修复`)
- [ ] `docs/tech-design.md` §3.3 / §6.3 / §6.11 加 flex / HarnessAdapter /
  多 session 段终稿
- [ ] `docs/interfaces.md` §1.3 / §2.1 / §5.5 / §15 增量段终稿
- [ ] 红线 grep 矩阵全跑过(详 §9 矩阵)
- [ ] `cargo test --workspace` 全绿;`cargo clippy --workspace --no-deps`
  不新增 warning(4 pre-existing 不算)

### 7.2 验收(摘 PRD §8.4)

- [ ] flex_e2e_test.rs 端到端 happy path + codex error path 通过
- [ ] `Cargo.toml::workspace.package.version = "0.3.1"`
- [ ] CLAUDE.md §一 / §六 更新
- [ ] `docs/v0-3-1/e2e-retro.md` 落档
- [ ] `cargo test --workspace` ≥ 738 + V0.3.1 累计新增
- [ ] clippy 无新 warning

---

## 8. Worktree subagent briefing 模板

每 PR 起 worktree 时,主 session 用 Agent 工具派,briefing 套以下模板。

### 8.1 通用前置(每 PR briefing 都加)

```markdown
## 起始

```
git fetch origin
git worktree add -b <branch> /tmp/ccteam-v031-<topic> origin/main
cd /tmp/ccteam-v031-<topic>
cargo test --workspace 2>&1 | tail -3   # confirm 738 baseline
```

## 必读(全 PR 通用)

- `CLAUDE.md` §一(状态)+ §三 红线 + §五 PR 纪律
- `docs/v0-3-1/prd.md` §<N>(本 PR 对应 finding 全文)+ §1 战略背景 +
  §10 不在范围(避免 silently 拉前 V0.4 deferred)
- `docs/v0-3-1/dev-plan.md` §<N>(本 PR 任务 / 验收)
- `docs/tech-design.md` §6 扩展点 + §3.7 跨项目记忆 + §5.5 progress.jsonl SoT
- `docs/interfaces.md` §1 文件系统布局 + §2 state 协议 + §5.5 team.yaml +
  §15 web routes
- `docs/research/v0-3-1-harness-adapter-plan.md`(临时计划记录,V0.3 ship
  前 dispatcher 写,本 PRD §3 是正式版)
- (PR #2):`docs/research/ccteam-codex-integration.md`(F47 stub 路线 — M0
  只读路径)

## 全 PR 红线(CLAUDE.md §三)

- progress.jsonl 是 SoT,**不解析 tmux 终端输出**;harness snapshot 是
  presentation-only,**不参与状态判定**
- **永不主动 kill 长 session** — `session rm` 是唯一显式用户授权 kill 路径
- ccteam-core **无 team 名字面量** — kind / harness / sid 全数据驱动
- 测试不退步(baseline 738);clippy 不新增 warning
- 文档同步(对应 dev-plan §文档同步段)

## PR 命令(完整 HEREDOC,见下)
```

### 8.2 PR #1 briefing(F46 HarnessAdapter — 完整)

```markdown
## 任务

V0.3.1 PR #1 — `crates/ccteam-core/src/harness.rs` 新模块,`HarnessAdapter`
trait + 数据结构 + 错误类型 + `ClaudeCodeAdapter` 完整实现 + statusline
wrapper 安装入口 + web SSE harness endpoint。Foundation PR — 后续 5 PR 都
基于本 PR 的 trait shape。

## 触点代码

- `crates/ccteam-core/src/harness.rs`(新建)
- `crates/ccteam-core/src/lib.rs`(re-export)
- `crates/ccteam-cli/src/commands.rs`(`run_doctor` 加 `--install-statusline-adapter`)
- `crates/ccteam-cli/src/main.rs`(`Commands::Doctor` 加 `install_statusline_adapter` flag)
- `crates/ccteam-web/src/routes/harness_sse.rs`(新建)
- `crates/ccteam-web/src/lib.rs`(router 注册)
- `crates/ccteam-web/src/watcher.rs`(扩 EventBus 加 harness path 监 / 复用)

## 实施步骤(详 dev-plan §2)

1. trait + 数据结构 + 错误类型(#1.1)
2. `ClaudeCodeAdapter` 实现(#1.2)
3. statusline wrapper 安装(#1.3)
4. harness JSON dual-write 路径推导(#1.4)
5. web SSE harness endpoints(#1.5)
6. 测试(#1.6)

## 红线 grep

```bash
# harness module 不解析 tmux 终端输出
git grep -nE 'capture_pane|tmux capture' crates/ccteam-core/src/harness.rs
# 期望:0 命中(只读 statusline JSON,不解析终端)

# orchestrator 不消费 harness snapshot 做状态决策
git grep -nE 'HarnessSnapshot' crates/ccteam-core/src/orchestrator.rs
# 期望:0 命中(orchestrator SoT 是 progress.jsonl,harness 只 presentation)

# trait 命名规范
git grep -nE 'pub trait HarnessAdapter' crates/ccteam-core/src/harness.rs
# 期望:命中 1

# CodexAdapter 不在 PR #1 出现(留 PR #2 独立 ship)
git grep -nE 'CodexAdapter' crates/
# 期望:0 命中
```

## PR 命令

```bash
gh pr create --base main --head v0-3-1-harness-adapter --title "v0.3.1 PR #1: F46 HarnessAdapter trait + ClaudeCodeAdapter" --body "$(cat <<'EOF'
## Closes
- F46 部分(`docs/dev-coupling-audit.md`):trait + ClaudeCodeAdapter 落地;
  CodexAdapter stub 在 PR #2 follow-up

## 关联
- 战略 pivot(`docs/v0-3-1/prd.md §1`):V0.3 ship 后,V0.3.1 把 ccteam 从
  "phase orchestrator" 扩展为"session farm + observability layer";本 PR
  立 HarnessAdapter trait 作 foundation
- 痛点 7 进度透明(`docs/requirements.md`)— harness snapshot 增加结构化
  数据维度
- tech-design §6 扩展点 — harness 是新扩展点

## 改动
- 新模块 `crates/ccteam-core/src/harness.rs`:trait + 数据结构 + 错误类型 +
  ClaudeCodeAdapter 实现
- `ccteam doctor --install-statusline-adapter` 安装 wrapper(marker 保护用户
  手改 + 原文件 backup .bak-<utc-ts>)
- `~/.ccteam/harness/<slug>-<sid>.json` 协议落地(stdin JSON 全覆盖,无
  delta archive — V0.4 deferred)
- web SSE `GET /sse/harness/<slug>` + `/sse/harness/<slug>/<sid>` 推
  harness_snapshot 事件

## 测试
- 新增 ~12(harness 单元 / wrapper 安装 / SSE wire / dual-write fallback)
- V0.3 现有测试不破(738 → 750)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```
```

### 8.3 后续 PR briefing(增量化基)

PR #2-#6 的 briefing 在 dispatcher 派工时基于 §8.2 模板**增量化**生成,
关键修改点:

- **任务段**:替换为对应 finding 的核心范围(详本 dev-plan §3-§7);引用
  PRD 对应章节(§4 / §5 / §6 / §7 / §8)
- **触点代码**:列具体改动文件(本 dev-plan §3-§7 任务摘要列出)
- **红线 grep**:每 PR 自己的 grep 矩阵 — 见 §9
- **PR 命令 body**:Closes 段对应 finding;改动段对应任务;测试段对应新增
  测试数

完整模板复用 §8.1 通用前置 + §8.2 形态;dispatcher 派工时 inline 替换。

---

## 9. 红线 grep 矩阵(每 PR ship 前跑过)

| 红线 | grep 命令 | 期望 |
|---|---|---|
| harness 不解析 tmux 输出 | `git grep -nE 'capture_pane' crates/ccteam-core/src/harness.rs` | 0 命中 |
| orchestrator 不消费 harness snapshot 做状态决策 | `git grep -nE 'HarnessSnapshot' crates/ccteam-core/src/orchestrator.rs` | 0 命中 |
| ccteam-core 不出现 team 名字面量 | `git grep -nE '"flex"\|"workflow"\|"multi_workflow"' crates/ccteam-core/src/{orchestrator,team_resolver}.rs` | 仅在 enum derive / 测试 fixture 出现,业务路径 0 命中 |
| ccteam-web 不依赖 ccteam-cli | `cargo tree -p ccteam-web \| grep ccteam-cli` | 0 命中(V0.3 PR #1 已建测试,F50 PR 不破) |
| `session rm` 是唯一显式 kill 路径 | `git grep -nE 'shutdown_session\|tmux kill-session\|kill_session' crates/ccteam-core/src/` | 仅在 `harness.rs::shutdown_session` 实现 + `commands.rs::run_session_rm` 调用,其他路径 0 命中 |
| flex 团队解析仍走 TeamSpec 数据驱动 | `git grep -nE 'if state.team' crates/ccteam-core/src/orchestrator.rs` | 0 命中(strategic doc §3 红线;`should_run_auto_loop` 等 helper 替代) |
| codex stub error 消息含追踪指针 | `git grep -nE 'docs/v0-3-1/prd.md §F47\|deferred to V0.3.2' crates/ccteam-core/src/harness.rs` | ≥ 3 命中(spawn / ingest / shutdown 都指向) |

---

## Changelog

- 2026-05-10:**初稿**。dev-plan 跟 V0.2.2 / V0.3 模式,但**只完整给出 PR
  #1(F46 HarnessAdapter)的 subagent briefing**(立 trait + statusline
  wrapper 是 foundation,后续 PR 在其基础上增量),PR #2-#6 briefing 由
  dispatcher 派工时基于 §8.2 模板增量化生成。base = `origin/main` `f9baf3f`
  (V0.3 ship);测试 baseline 738/0;workspace.version 0.3.0 → 0.3.1(F51 PR)。
