# V0.4.0 开发计划

> V0.4.0 架构重写计划。**核心方向已 lock**（2026-05-14）：
> 删除 phase 模板系统，引入 workflow.yaml agent 拓扑，Harness 极薄，
> Meta-agent 常驻 MCP 协调；按依赖顺序拆 **10 个 PR**（F60-F69，
> 最后 F69 是 ship gate chore）。worktree-per-PR；每 PR 一份 subagent briefing。
>
> 配套文档：
> - 需求决策：`docs/v0-4-0/prd.md`
> - 文档索引：`docs/v0-4-0/README.md`（待建）
>
> base = `origin/main` V0.3.2 ship 终点；测试 baseline `866/0`；
> workspace.version 起点 `0.3.2`。
>
> **模式**：F60/F61/F62 三路并行起步（互不冲突）；F60 完成后 spawn F63；
> F63 完成后 F64/F65 并行；F66 汇总三路后起；F67 与 F66 并行可开始；
> F68 汇总 F61+F67 后起；F69 全路 merge 后 ship gate。

---

## 1. PR 总览

| # | finding | branch | 工程量估 | 主要前置 |
|---|---|---|---|---|
| **PR #1** | **F60** Phase machinery removal | `v0-4-0-f60` | ~-1800 LOC 净删除 + ~30 LOC 新桩 | 无 |
| **PR #2** | **F61** ClaudeCodeAdapter thin refactor | `v0-4-0-f61` | ~-800 LOC + ~50 LOC 新 | 无 |
| **PR #3** | **F62** Real CodexAdapter | `v0-4-0-f62` | ~300 LOC + ~15 测试 | 无 |
| **PR #4** | **F63** workflow.yaml schema + parser | `v0-4-0-f63` | ~200 LOC + ~15 测试 | F60 |
| **PR #5** | **F64** Artifact watcher（inotify）| `v0-4-0-f64` | ~150 LOC + ~10 测试 | F63 |
| **PR #6** | **F65** Meta-agent MCP tools（7 新工具）| `v0-4-0-f65` | ~400 LOC + ~20 测试 | F63 + F64 |
| **PR #7** | **F66** Thin orchestrator（替换 2713 LOC 状态机）| `v0-4-0-f66` | ~400 LOC + ~20 测试 | F63 + F64 + F65 |
| **PR #8** | **F67** Progress tracking refactor | `v0-4-0-f67` | ~300 LOC 改 + ~10 测试 | F66 |
| **PR #9** | **F68** ccteam-web adaptation | `v0-4-0-f68` | ~500 LOC TS + ~8 测试 | F61 + F67 |
| **PR #10** | **F69** Example workflows + e2e + ship gate | `v0-4-0-f69` | 文档 + e2e + bump | F60-F68 全 |

**总计**：净增约 +1600 LOC Rust + 500 LOC TS；净删约 -2800 LOC Rust；
+98 测试；总体 orchestrator 从 2713 LOC 压缩到 ~400 LOC。
预估 4-6 周（单人；F60/F61/F62 三路并行 + F64/F65 并行可压缩至 ~3 周）。

### 1.1 依赖图

```
PR #1 (F60 删 phase 机制)   ────┐
PR #2 (F61 薄 CC adapter)   ────┤   三路并行（互不冲突）
PR #3 (F62 真 Codex)        ────┘
                                  ↓
PR #4 (F63 workflow YAML)  ←───── depends on F60
                                  ↓
PR #5 (F64 artifact watcher) ←─── depends on F63
PR #6 (F65 MCP tools)       ←─── depends on F63 + F64
                                  ↓
PR #7 (F66 thin orchestrator) ←── depends on F63 + F64 + F65
PR #8 (F67 progress refactor) ←── depends on F66（可与 F66 并行开始）
                                  ↓
PR #9 (F68 web adaptation)  ←──── depends on F61 + F67
                                  ↓
PR #10 (F69 ship gate)      ←──── depends on F60-F68 全
```

**并行机会**：
- **F60 / F61 / F62** 三路 worktree 起步（改动文件不重叠）
- **F64 vs F65** F63 merge 后并行起（F64 改 watcher 模块，F65 改 mcp_serve.rs）
- **F66 vs F67** F66 完主骨架后 F67 可同步开改 progress.rs，冲突仅 progress.rs 本身

---

## 2. PR #1 — F60 Phase machinery removal（完整 subagent briefing 模板）

> **目标**：彻底删除 phases.rs（934 行）、golden_rules.rs（555 行）、dag.rs（全）、
> orchestrator.rs 中所有 phase 状态机逻辑（~1200 行）、inject_directives / 
> escalate_grammar / decision_mode / golden_rules / PhaseDAG 等相关字段。
> 这是净删除 PR，不写任何新功能——只保留让编译通过的最小桩（~30 行）。
>
> **不在本 PR**：新 workflow 解析（F63）、新 orchestrator 逻辑（F66）。
> 本 PR 结束时 orchestrator.rs 应退化为空 struct + 几个 TODO 桩。

**关联 PRD**：`docs/v0-4-0/prd.md` §F60

**前置**：无。

### 2.1 任务

- [ ] **#1.1** 删除 `crates/ccteam-core/src/phases.rs`（全文 934 行）
  - 删除：`PhaseTemplate`、`PhaseHooks`、`DecisionMode`、`GoldenRule`、
    `GoldenRuleKind`、`SubSkillSpec`、`SubSkillTrigger`、`AgentTeamRole`
  - 删除：`inject_directives`、`escalate_grammar_ref`、`decision_mode`、
    `max_clarify_rounds`、`golden_rules` 所有逻辑
  - 删除：`phases.rs` 中所有 `load` / `parse` / `validate` / `effective_*` 函数

- [ ] **#1.2** 删除 `crates/ccteam-core/src/golden_rules.rs`（全文 555 行）
  - 删除：`GoldenRulesReport`、`enforce` 函数、所有文件系统检查逻辑

- [ ] **#1.3** 删除 `crates/ccteam-core/src/dag.rs`（全文）
  - 删除：`Dag` struct、`from_templates`、`next_on_done`、`next_on_escalate`、
    `is_terminal_phase`、`is_terminal_state`、`dev_dag`

- [ ] **#1.4** 清理 `crates/ccteam-core/src/orchestrator.rs`（~1200 行删除）
  - 删除：`TeamRuntime { spec, templates, dag }` 的 `templates` + `dag` 字段
  - 删除：`decide_tick`、`decide_tick_from_events` 函数（已依赖 Dag）
  - 删除：`dispatch_phase_with_state`、`attachments_for_next_phase` 函数
  - 删除：`handle_golden_rules_violation` 函数
  - 删除：`PhaseState::InFlight`、`PhaseState::DonePending`、`PhaseState::AutoLocked`
    的处理分支（state 机内 ~800 行）
  - **保留桩**：`pub struct Orchestrator`、`pub fn new(paths: CcteamPaths) -> Self`、
    `pub async fn run_project(&self, slug: &str) -> Result<()>` 返回 `todo!()`
  - 保留：`run_new`（启动新项目 tmux session，F66 实现 workflow 调度）
  - 保留：`run_meta_agent`（meta-agent 常驻，不变）

- [ ] **#1.5** 清理 `crates/ccteam-core/src/state.rs`
  - 删除 `PhaseState` enum 中 phase-specific 变体：`InFlight`、`DonePending`、
    `AutoLocked`（保留 `Idle` 和 `Done`）
  - 删除 `ProjectState` 中：`current_phase`、`phase_state`、`last_event_type`、
    `escalation_count`、`decision_candidates`（这些将由 workflow progress 替代）
  - **保留**：`ProjectState` 核心字段（`slug`、`team`、`kind`、`created_at`、
    `cost`、`status`）+ `FlexState`（F49/V0.3.1 已 ship，F66 将扩展）

- [ ] **#1.6** 清理 `crates/ccteam-core/src/templates.rs`
  - 删除该文件或清空为空模块（模板系统已无 consumer）
  - 如有 `TemplateResolver`、`load_team_templates` 等函数，全删

- [ ] **#1.7** 清理所有 use 引用
  - `crates/ccteam-core/src/lib.rs`：删 `pub mod phases`、`pub mod golden_rules`、
    `pub mod dag`（或降为私有并清 export）
  - `crates/ccteam-cli/src/commands.rs`：删 phase-dispatch 相关子命令
    （`phase advance`、`phase show`、`decide` 等，如存在）
  - `crates/ccteam-cli/src/mcp_serve.rs`：删 phase 相关 MCP 工具定义
    （`ccteam__inject_decision`、`ccteam__decide` 等依赖 PhaseTemplate 的工具）

- [ ] **#1.8** 修复编译
  - `cargo check --workspace` 全绿（允许警告，禁止 error）
  - 所有测试中引用 `PhaseTemplate`、`Dag`、`golden_rules` 的测试文件：
    - `crates/ccteam-core/tests/` 下 phase 相关测试 → 整文件删除
    - `crates/ccteam-cli/tests/` 下 phase 相关集成测试 → 删除
  - **保留** harness.rs、progress.rs、projects.rs、tmux.rs、paths.rs 不动

- [ ] **#1.9** 文档同步
  - `docs/dev-coupling-audit.md`：添加 F60 entry（scope、被删文件列表）
  - `docs/interfaces.md`：删除 §"Phase YAML schema"、§"escalate_grammar" 相关章节
  - 注释掉（不删）`docs/v0-1/` 下 phase 相关设计文档的引用（历史归档保留）

### 2.2 红线 grep

提交前自检：

```bash
# 红线 1：phases.rs 已删，无 use 残留
grep -rn "use crate::phases\|mod phases\|phases::" crates/ccteam-core/src/ crates/ccteam-cli/src/
# 期望: 0 hit

# 红线 2：golden_rules.rs 已删，无 use 残留
grep -rn "use crate::golden_rules\|mod golden_rules\|golden_rules::" crates/ccteam-core/src/ crates/ccteam-cli/src/
# 期望: 0 hit

# 红线 3：dag.rs 已删，无 use 残留
grep -rn "use crate::dag\|mod dag\|dag::\|PhaseDAG\|from_templates" crates/ccteam-core/src/
# 期望: 0 hit

# 红线 4：inject_directives / escalate_grammar 已删
grep -rn "inject_directives\|escalate_grammar\|decision_mode\|golden_rules\|PhaseTemplate\|DecisionMode" \
  crates/ccteam-core/src/ crates/ccteam-cli/src/
# 期望: 0 hit（文档引用 OK，源代码 0）

# 红线 5：progress.jsonl 仍是 SoT（progress.rs 未被删）
grep -rn "pub mod progress\|pub use.*progress" crates/ccteam-core/src/lib.rs
# 期望: 1 hit（progress 模块保留）

# 红线 6：不新增 backwards-compat shim
grep -rn "// V0.3 compat\|// legacy phase\|// deprecated" crates/ccteam-core/src/
# 期望: 0 hit

# 编译检查
cargo check --workspace 2>&1 | grep -c "^error" || true
# 期望: 0
```

### 2.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/phases.rs` | delete（934 行）| -934 |
| `crates/ccteam-core/src/golden_rules.rs` | delete（555 行）| -555 |
| `crates/ccteam-core/src/dag.rs` | delete | -~200 |
| `crates/ccteam-core/src/templates.rs` | delete 或清空 | -664 |
| `crates/ccteam-core/src/orchestrator.rs` | edit（大删）| -~1200 → 保留 ~100 |
| `crates/ccteam-core/src/state.rs` | edit（删 phase 字段）| -~80 |
| `crates/ccteam-core/src/lib.rs` | edit（删 mod 声明）| -8 |
| `crates/ccteam-cli/src/mcp_serve.rs` | edit（删 phase 工具）| -~50 |
| `crates/ccteam-cli/src/commands.rs` | edit（删 phase 子命令）| -~30 |
| `crates/ccteam-core/tests/<phase_tests>` | delete（相关测试文件）| -~300 |
| `docs/dev-coupling-audit.md` | edit（F60 entry）| +15 |
| `docs/interfaces.md` | edit（删 Phase schema 章节）| -~50 |

净删除约 **-1800 LOC** + 保留桩约 +30 LOC。

### 2.4 PR 描述模板

```
v0.4.0-f60: Phase machinery removal (Closes F60)

Maps to:
- requirements.md §痛点 5（自动化推进太死板）+ §痛点 6（上下文爆炸）
- tech-design.md §3.5 Phase Engine → 删除 + §6.9 Context 管理
- docs/v0-4-0/prd.md §F60
- dev-coupling-audit.md F60

Scope（净删除）:
- Delete phases.rs（934 LOC）+ golden_rules.rs（555 LOC）+ dag.rs
- Delete templates.rs（664 LOC）
- Gut orchestrator.rs phase state machine（~1200 LOC），保留空桩
- Trim state.rs PhaseState + ProjectState phase fields
- Trim mcp_serve.rs phase-dependent tools（inject_decision 暂保留 stub）

Not in this PR: workflow.yaml（F63）、thin orchestrator（F66）
Tests: baseline 866/0 → 866-X/0（删 phase 测试，总数降但 failed=0）
```

### 2.5 worktree 命令

```bash
git worktree add -b v0-4-0-f60 /tmp/ccteam-v040-f60 origin/main
cd /tmp/ccteam-v040-f60
# ... 干活（删文件、清 use、修编译）...
cargo check --workspace 2>&1 | grep -c "^error" || true
cargo test --workspace 2>&1 | grep -E "^test result"
cargo fmt -- $(git diff --name-only origin/main | grep '\.rs$')
git commit -am "v0.4.0-f60: Phase machinery removal (Closes F60)"
git push origin v0-4-0-f60
gh pr create --title "v0.4.0-f60: Phase machinery removal" --body "$(cat <<'EOF'
... PR 描述模板 ...
EOF
)"
# merge 后:
cd /home/ubuntu/rob/ccteam && git worktree remove /tmp/ccteam-v040-f60
```

---

## 3. PR #2 — F61 ClaudeCodeAdapter thin refactor

> **目标**：重构 `ClaudeCodeAdapter`，从"tmux spawn + statusline JSON 解析"
> 改为"claude --bg --agent + 读 `~/.claude/jobs/<id>/state.json` 观测"。
> 删除 CC session 的 tmux 依赖（ClaudeCodeAdapter 不再自己管 tmux），
> 删除 statusline JSON 解析管道，删除 `statusline-command.sh` 写入路径。
>
> **不在本 PR**：workflow 调度（F66）、web UI 适配（F68）。

**关联 PRD**：`docs/v0-4-0/prd.md` §F61

**前置**：无（与 F60/F62 并行）。

### 3.1 任务

- [ ] **#2.1** 重构 `spawn_session`（harness.rs）
  - **旧**：`tmux new-session -d -s <name>` → 用 statusline hook 管道注入指令
  - **新**：
    ```
    claude --bg --agent <role>
           --output-format stream-json
           --workdir <project_dir>
    ```
    调用方式：`std::process::Command::new("claude")` 带 args + env；
    捕获 stdout 第一行 JSON（包含 `job_id`）；存入 `SessionHandle.job_id`
  - `SpawnOpts` 新增字段：`role: String`（agent role 名，对应 `.claude/agents/<role>.md`）
  - 删除：`SpawnOpts.extra_env`（statusline env 注入）不再需要
  - 删除：tmux session name 生成逻辑（`tmux.rs::session_name`）从 ClaudeCodeAdapter 移走

- [ ] **#2.2** 重构 `ingest_snapshot`（harness.rs）
  - **旧**：解析 statusline stdin JSON（`HarnessSnapshot` 从 statusline JSON 重建）
  - **新**：读 `~/.claude/jobs/<job_id>/state.json`（CC native job state 文件）
    ```rust
    let path = state_json_path(job_id);   // ~/.claude/jobs/<id>/state.json
    let raw = fs::read_to_string(&path)?;
    let snapshot = parse_cc_state_json(&raw)?;
    ```
  - `parse_cc_state_json` 从 state.json 提取：`model`、`context_pct`、`cost`、
    `turn_count`、`status`（running / idle / stopped）
  - `HarnessSnapshot` 保留 struct shape（`model`、`ctx_pct`、`cost_usd`、`captured_at`），
    但来源从 statusline 改为 state.json

- [ ] **#2.3** 删除 statusline 管道依赖
  - 删除：`crates/ccteam-core/src/harness.rs` 中 `write_harness_snapshot` 函数
  - 删除：`derive_harness_path` 函数（已无 statusline 路径逻辑）
  - 删除：`crates/ccteam-hooks/` 中 statusline hook 相关代码
    （`statusline-command.sh` 生成 + 注入；F60 后 phase hooks 也已删）
  - 保留：`HarnessSnapshot` struct（形状不变，F68 web 侧读它）
  - 保留：`SessionHandle`（新增 `job_id: String` 字段）

- [ ] **#2.4** 新增 `state_json_path` helper
  ```rust
  pub fn state_json_path(job_id: &str) -> PathBuf {
      dirs::home_dir()
          .unwrap_or_default()
          .join(".claude/jobs")
          .join(job_id)
          .join("state.json")
  }
  ```
  位置：`harness.rs` 末尾 pub helpers 区

- [ ] **#2.5** 删除 CC session 的直接 tmux 依赖
  - `ClaudeCodeAdapter::shutdown_session`：
    - **旧**：`tmux send-keys -t <name> /exit Enter` + kill-window
    - **新**：向 job state 写 `{"action":"stop"}` 或发 `SIGTERM` 给 job 进程
      （`state.json` 中有 `pid` 字段）；若进程已停则 no-op
  - 保留 `tmux.rs` 模块（flex kind / multi-session 仍用 tmux；只是 CC adapter 不再依赖）

- [ ] **#2.6** 更新 `ccteam-hooks` crate
  - 删除：statusline JSON schema 解析（`parse_statusline_json`）
  - 删除：`install_statusline_hook`、`statusline_command_sh` 模板生成
  - 保留：`PreToolUse` hook（F58 write-action 路径仍用）

- [ ] **#2.7** 测试
  - `crates/ccteam-core/tests/harness_thin_test.rs`（新文件）：
    - **t01_spawn_returns_job_id**: mock `claude --bg` 返回 JSON → 提取 job_id
    - **t02_ingest_from_state_json**: 写 mock state.json → `ingest_snapshot` 返回
      正确 `HarnessSnapshot`
    - **t03_shutdown_sends_sigterm**: `shutdown_session` → 发 SIGTERM 到 pid
    - **t04_state_json_path_helper**: `state_json_path("abc123")` 返回正确路径
    - **t05_statusline_write_removed**: 确认 `write_harness_snapshot` 函数不存在
      （编译 level；如删了函数，测试 import 失败则测试本身不写此条，改为注释）
  - `cargo test --workspace` ≥ baseline `866/0`（本 PR 净 +5 测试）

### 3.2 红线 grep

```bash
# 红线 1：CC adapter 不再写 statusline
grep -rn "statusline\|statusline-command\|write_harness_snapshot" \
  crates/ccteam-core/src/harness.rs
# 期望: 0 hit

# 红线 2：CC adapter 不再直接 spawn tmux session
grep -rn "tmux.*new-session\|new_session\|TmuxSession::new" \
  crates/ccteam-core/src/harness.rs
# 期望: 0 hit（tmux.rs 的 tmux 操作归 flex/multi-session，不在 ClaudeCodeAdapter）

# 红线 3：不解析 tmux output（CC adapter 改读 state.json）
grep -rn "pipe-pane\|capture-pane\|from_utf8.*tmux" \
  crates/ccteam-core/src/harness.rs
# 期望: 0 hit

# 红线 4：SessionHandle 有 job_id 字段
grep -n "job_id" crates/ccteam-core/src/harness.rs
# 期望: ≥ 2 hit（struct 定义 + 使用）

# 红线 5：不破坏 progress.jsonl SoT（harness 不写 progress）
grep -rn "progress.jsonl\|append_progress" crates/ccteam-core/src/harness.rs
# 期望: 0 hit
```

### 3.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/harness.rs` | edit（大改）| -800 + 50 新 |
| `crates/ccteam-core/src/state.rs` | edit（SessionHandle.job_id）| +5 |
| `crates/ccteam-hooks/src/lib.rs` | edit（删 statusline hook）| -~200 |
| `crates/ccteam-core/tests/harness_thin_test.rs` | new | +150 |
| `docs/dev-coupling-audit.md` | edit（F61 entry）| +10 |

净删除约 **-800 LOC** + 约 +50 LOC 新实现 + 约 +150 LOC 测试。

### 3.4 PR 描述模板

```
v0.4.0-f61: ClaudeCodeAdapter thin refactor (Closes F61)

Maps to:
- requirements.md §痛点 3（agent 透明度）
- tech-design.md §2.1 三层架构 + §3.8 用户接口层
- docs/v0-4-0/prd.md §F61
- dev-coupling-audit.md F61

Scope:
- spawn_session: claude --bg --agent <role>（不再 tmux new-session）
- ingest_snapshot: 读 ~/.claude/jobs/<id>/state.json（不再 statusline JSON）
- shutdown_session: SIGTERM via state.json pid（不再 tmux send-keys /exit）
- 删除 write_harness_snapshot、derive_harness_path、statusline hook 生成

Tests: +5（harness_thin_test.rs）
Baseline: 866/0 → 871/0
```

### 3.5 worktree 命令

```bash
git worktree add -b v0-4-0-f61 /tmp/ccteam-v040-f61 origin/main
cd /tmp/ccteam-v040-f61
cargo test --workspace 2>&1 | grep -E "^test result"   # 确认 baseline 866/0
# ... 干活 ...
cargo test --workspace 2>&1 | grep -E "^test result"
cargo fmt -- $(git diff --name-only origin/main | grep '\.rs$')
git commit -am "v0.4.0-f61: ClaudeCodeAdapter thin refactor (Closes F61)"
git push origin v0-4-0-f61
# merge 后:
cd /home/ubuntu/rob/ccteam && git worktree remove /tmp/ccteam-v040-f61
```

---

## 4. PR #3 — F62 Real CodexAdapter

> **目标**：实现真正的 `CodexAdapter`，替换 V0.3.1 F47 的 `NotImplemented` 桩。
> 方案：tmux spawn + `codex` CLI；状态跟踪读 codex 的 state 输出。
> 这是 V0.3.1 PRD §10.3 + V0.3.1 README erratum 中 slip 到 V0.3.3 的工作，
> 随 V0.4.0 架构重写一并实现。

**关联 PRD**：`docs/v0-4-0/prd.md` §F62 + `docs/research/ccteam-codex-integration.md`

**前置**：无（与 F60/F61 并行；但与 F61 无代码冲突）。

### 4.1 任务

- [ ] **#3.1** 实现 `CodexAdapter::spawn_session`（harness.rs）
  - tmux spawn：`tmux new-session -d -s <name> codex --no-ask <agent_role_md>`
  - agent role md：从 `SpawnOpts.role` 拼 `.claude/agents/<role>.md`
    （codex CLI 接受 `--system <file>` 或首条 user message；确认 codex 参数签名
    见 `references/codex/codex-rs/README.md`）
  - `SessionHandle.tmux_session_name` 填入（codex 走 tmux，与 F61 CC 走 job_id 不同）
  - 初始 state.json 写 `{"status":"starting","pid":<tmux pid>}`（模拟 CC state.json 格式）

- [ ] **#3.2** 实现 `CodexAdapter::ingest_snapshot`（harness.rs）
  - codex 没有原生 statusline；读 tmux `capture-pane -p` 末 5 行识别"idle"/"running"
  - 解析逻辑：grep `CODEX_STATUS: <json>` 行（约定 codex agent 最后写这行）
  - fallback：无法解析 → 返回 `HarnessSnapshot { model: "codex", ctx_pct: 0, cost_usd: 0.0, ... }`
  - 不阻塞（`capture-pane` 快，100ms 超时）

- [ ] **#3.3** 实现 `CodexAdapter::shutdown_session`（harness.rs）
  - 发 `q\r`（codex 退出键序列）→ 等 500ms → `tmux kill-session -t <name>` 兜底
  - 不 leak：`kill-session` 只对 codex 管理的 session，不波及其他 session

- [ ] **#3.4** 更新 `CODEX_NOT_IMPLEMENTED_REASON` 常量
  - 将 `NotImplemented` 桩的 const 删除（或改为 `unreachable!`）
  - 所有三个方法改为真实实现，不再 return `Err(NotImplemented)`

- [ ] **#3.5** 测试（tmux 依赖，`#[serial]`）
  - `crates/ccteam-core/tests/codex_adapter_test.rs`（新文件）：
    - **t01_spawn_creates_tmux_session**: `spawn_session` → `tmux list-sessions` 见新 session
    - **t02_ingest_snapshot_fallback**: 无 CODEX_STATUS 行 → 返回默认 snapshot（不 panic）
    - **t03_ingest_snapshot_parse**: mock tmux output 含 `CODEX_STATUS:` → 正确解析
    - **t04_shutdown_kills_session**: `shutdown_session` → `tmux list-sessions` 不见该 session
    - **t05_codex_not_implemented_removed**: `CodexAdapter::new()` 的 3 方法
      不返回 `NotImplemented`（编译级验证：impl 中无 `NotImplemented` 路径）
  - `#[cfg(feature = "codex-tests")]` 门控（codex CLI 非所有 CI 环境都装）；
    default feature set 不含（避免 CI 红）

- [ ] **#3.6** feature flag
  - `Cargo.toml`（ccteam-core）新增：`[features] codex-tests = []`
  - 测试文件头部 `#[cfg(feature = "codex-tests")]`
  - README / user-manual 说明 `cargo test -F codex-tests` 需要 codex CLI 已装

### 4.2 红线 grep

```bash
# 红线 1：CodexAdapter 不再返回 NotImplemented
grep -rn "NotImplemented\|CODEX_NOT_IMPLEMENTED" crates/ccteam-core/src/harness.rs
# 期望: 0 hit

# 红线 2：codex session 用 tmux（不用 claude --bg）
grep -n "claude.*--bg\|job_id" crates/ccteam-core/src/harness.rs | grep -i "codex"
# 期望: 0 hit（codex 走 tmux spawn，CC 走 --bg）

# 红线 3：不 kill 其他 tmux session（只 kill 自己 spawn 的 codex session）
grep -rn "kill-server\|kill-session" crates/ccteam-core/src/harness.rs
# 期望: 只在 shutdown_session 内，且带 -t <specific_name>
```

### 4.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/harness.rs` | edit（替换 CodexAdapter stub）| +200 改（-50 桩 +250 实现）|
| `crates/ccteam-core/Cargo.toml` | edit（feature flag）| +3 |
| `crates/ccteam-core/tests/codex_adapter_test.rs` | new | +180 |
| `docs/dev-coupling-audit.md` | edit（F62 entry）| +10 |

净约 +300 LOC + 15 测试（codex-tests feature）。

### 4.4 PR 描述模板

```
v0.4.0-f62: Real CodexAdapter (Closes F62)

Maps to:
- requirements.md §痛点 7（多 agent 混用）
- tech-design.md §6 扩展点 + docs/research/ccteam-codex-integration.md
- docs/v0-4-0/prd.md §F62
- dev-coupling-audit.md F62（V0.3.1 F47 erratum slip resolved）

Scope:
- CodexAdapter::spawn_session: tmux + codex CLI（替换 NotImplemented 桩）
- CodexAdapter::ingest_snapshot: capture-pane + CODEX_STATUS 行解析
- CodexAdapter::shutdown_session: q\r + kill-session 兜底
- Feature flag codex-tests（需 codex CLI 安装）

Tests: +5 under codex-tests feature
Baseline: 866/0 → 866/0（新测试 feature-gated，默认不跑）
```

### 4.5 worktree 命令

```bash
git worktree add -b v0-4-0-f62 /tmp/ccteam-v040-f62 origin/main
cd /tmp/ccteam-v040-f62
# ... 干活 ...
cargo test --workspace 2>&1 | grep -E "^test result"
cargo test --workspace -F codex-tests 2>&1 | grep -E "^test result"  # codex 环境才跑
cargo fmt -- $(git diff --name-only origin/main | grep '\.rs$')
git commit -am "v0.4.0-f62: Real CodexAdapter (Closes F62)"
git push origin v0-4-0-f62
cd /home/ubuntu/rob/ccteam && git worktree remove /tmp/ccteam-v040-f62
```

---

## 5. PR #4 — F63 workflow.yaml schema + parser

> **目标**：新建 `crates/ccteam-core/src/workflow.rs`，定义 `WorkflowSpec`
> struct（serde + 验证），能解析 workflow.yaml 文件，支持四种 trigger 类型
>（`schedule`、`watch:<path>`、`gate`、`manual`）。
> **workflow.yaml 里没有一行 prompt**——prompt 在 `.claude/agents/<role>.md`。

**关联 PRD**：`docs/v0-4-0/prd.md` §F63

**前置**：F60 已 merge（确认 phases.rs / dag.rs 已删，无命名冲突）。

### 5.1 任务

- [ ] **#4.1** 新文件 `crates/ccteam-core/src/workflow.rs`
  ```rust
  pub struct WorkflowSpec {
      pub name: String,
      pub agents: IndexMap<String, AgentSpec>,  // key = role name
  }

  pub struct AgentSpec {
      pub executor: Executor,      // claude | codex
      pub trigger: Trigger,        // see below
      pub parallelism: Option<u32>, // default 1
      pub input: Option<PathBuf>,  // watch input dir
      pub output: Option<PathBuf>, // artifact output dir
  }

  pub enum Executor { Claude, Codex }

  pub enum Trigger {
      Schedule,               // cron-style（V0.4.1 扩展；V0.4.0 = 手动触发占位）
      Watch(PathBuf),         // watch:<path>
      Gate,                   // 等所有 input verdicts 满足
      Manual,                 // 手动（ccteam trigger <role>）
  }
  ```

- [ ] **#4.2** `WorkflowSpec::load(path: &Path) -> Result<Self>`
  - `serde_yaml::from_str`（依赖 `serde_yaml`，已在 workspace deps 或新增）
  - `WorkflowSpec::validate` 检查：
    - 每个 agent 的 `input` 路径如存在需是目录（不要求已存在，允许首次运行创建）
    - `watch:` trigger 的 path 不能为空
    - `gate` trigger 必须有 `input`
    - agent name 只含 `[a-z0-9_-]`（合法 role 名）

- [ ] **#4.3** `WorkflowSpec::load_for_project(project_dir: &Path) -> Result<Self>`
  - 按序查找：`<project_dir>/workflow.yaml` → `<project_dir>/.ccteam/workflow.yaml`
  - 两处都不存在 → `Err(WorkflowNotFound)`

- [ ] **#4.4** 新 error type `WorkflowError`
  ```rust
  pub enum WorkflowError {
      NotFound(PathBuf),
      ParseFailed(serde_yaml::Error),
      ValidationFailed(String),
  }
  ```

- [ ] **#4.5** 写出 `examples/workflow-ui-quality-loop.yaml`
  ```yaml
  name: ui-quality-loop
  agents:
    explorer:
      executor: claude
      trigger: manual
      output: .ccteam/issues/

    fixer:
      executor: claude
      trigger: watch:.ccteam/issues/
      parallelism: 10
      input: .ccteam/issues/
      output: .ccteam/fixes/

    reviewer:
      executor: codex
      trigger: watch:.ccteam/fixes/
      input: .ccteam/fixes/
      output: .ccteam/verdicts/

    shipper:
      executor: claude
      trigger: gate
      input: .ccteam/verdicts/
  ```
  放至 `crates/ccteam-core/tests/fixtures/workflow-ui-quality-loop.yaml`（测试用）

- [ ] **#4.6** 测试（`tests/workflow_test.rs`，新文件）
  - **t01_load_valid_yaml**: 加载 `ui-quality-loop.yaml` → 4 agents，正确 executor/trigger
  - **t02_load_research_loop**: 加载第二个 fixture `workflow-research-loop.yaml`
  - **t03_validate_empty_watch_path**: trigger `watch:` path 为空 → `ValidationFailed`
  - **t04_validate_gate_without_input**: gate trigger 无 input → `ValidationFailed`
  - **t05_invalid_agent_name**: agent name 含空格 → `ValidationFailed`
  - **t06_load_for_project_finds_workflow_yaml**: 写 tmpdir workflow.yaml → 能找到
  - **t07_load_for_project_not_found**: 空 tmpdir → `WorkflowNotFound`
  - **t08_executor_default_claude**: 无 executor 字段时默认 claude
  - **t09_parallelism_default_one**: 无 parallelism 字段时默认 1
  - **t10_manual_trigger_no_input_required**: manual trigger 无 input 不报错
  - **t11_watch_trigger_path_relative**: watch 相对路径解析正确
  - **t12_gate_trigger_needs_input_dir**: gate 有 input → 验证通过
  - **t13_duplicate_agent_name**: 重复 agent key（YAML level 覆盖）→ 最后一个胜出（serde 行为，文档说明）
  - **t14_serialization_roundtrip**: load → serialize → compare field by field
  - **t15_unknown_executor_fails**: executor 值为 `"unknown"` → `ParseFailed`

- [ ] **#4.7** 文档同步
  - `docs/interfaces.md`：新增 §"workflow.yaml schema"（每个字段注释）
  - `docs/dev-coupling-audit.md`：F63 entry

### 5.2 红线 grep

```bash
# 红线 1：workflow.yaml 里没有 prompt 字段
grep -rn "prompt:\|system_prompt:\|messages:" \
  crates/ccteam-core/tests/fixtures/workflow*.yaml
# 期望: 0 hit

# 红线 2：WorkflowSpec 不含 team 名字面量
grep -rn '"ccteam"\|"chainup"\|"dev"\|"qa"' crates/ccteam-core/src/workflow.rs \
  | grep -v "test\|fixture"
# 期望: 0 hit（workflow.rs 本身不硬编码 team 名）

# 红线 3：parser 不写 progress.jsonl
grep -rn "progress\|append_progress\|jsonl" crates/ccteam-core/src/workflow.rs
# 期望: 0 hit（parser 纯数据，无 IO 副作用）

# 红线 4：serde_yaml 版本不引入冲突
grep -n "serde_yaml" Cargo.toml crates/ccteam-core/Cargo.toml 2>/dev/null
# 期望: 有且只有一处 version pin
```

### 5.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/workflow.rs` | new | +200 |
| `crates/ccteam-core/src/lib.rs` | edit（pub mod workflow）| +2 |
| `crates/ccteam-core/Cargo.toml` | edit（serde_yaml dep，若未有）| +2 |
| `crates/ccteam-core/tests/workflow_test.rs` | new | +220 |
| `crates/ccteam-core/tests/fixtures/workflow-ui-quality-loop.yaml` | new | +18 |
| `crates/ccteam-core/tests/fixtures/workflow-research-loop.yaml` | new | +14 |
| `docs/interfaces.md` | edit（§workflow.yaml schema）| +50 |
| `docs/dev-coupling-audit.md` | edit（F63 entry）| +10 |

总约 +200 LOC Rust + 220 LOC 测试 + 15 测试用例。

### 5.4 PR 描述模板

```
v0.4.0-f63: workflow.yaml schema + parser (Closes F63)

Maps to:
- requirements.md §痛点 5（灵活工作流）+ §痛点 2（易用性）
- tech-design.md §6 扩展点（workflow 作为数据驱动扩展机制）
- docs/v0-4-0/prd.md §F63
- dev-coupling-audit.md F63

Scope:
- WorkflowSpec: name + agents（executor/trigger/parallelism/input/output）
- Trigger: schedule | watch:<path> | gate | manual
- WorkflowSpec::load + load_for_project + validate（4 条验证规则）
- Fixture: ui-quality-loop.yaml + research-loop.yaml

Tests: +15（workflow_test.rs）
Baseline: 866/0 → 881/0
```

---

## 6. PR #5 — F64 Artifact watcher

> **目标**：实现 `crates/ccteam-core/src/artifact_watcher.rs`，用 inotify
>（Linux）/ fsevents（macOS）监听 artifact 目录变化，debounce 后触发
> 对应 agent。是 workflow 调度的事件源。

**关联 PRD**：`docs/v0-4-0/prd.md` §F64

**前置**：F63 已 merge（`WorkflowSpec`、`Trigger::Watch` 已定义）。

### 6.1 任务

- [ ] **#5.1** 新文件 `crates/ccteam-core/src/artifact_watcher.rs`
  ```rust
  pub struct ArtifactWatcher {
      tx: mpsc::Sender<ArtifactEvent>,
  }

  pub struct ArtifactEvent {
      pub role: String,        // 哪个 agent 应被触发
      pub artifact_path: PathBuf,  // 触发变化的文件路径
      pub event_kind: WatchKind,
  }

  pub enum WatchKind { Created, Modified, Deleted }
  ```

- [ ] **#5.2** `ArtifactWatcher::new(spec: &WorkflowSpec) -> Result<(Self, mpsc::Receiver<ArtifactEvent>)>`
  - 遍历 `spec.agents`：找所有 `Trigger::Watch(path)` 的 agent
  - 对每个 watch path 注册 inotify/fsevents 监听（`notify` crate，跨平台）
  - debounce：同一目录 500ms 内的多次变化合并为一次事件

- [ ] **#5.3** `ArtifactWatcher::start(self) -> tokio::task::JoinHandle<()>`
  - spawn tokio 后台任务；收到 fs 事件 → debounce → 发 `ArtifactEvent` 到 tx
  - 错误处理：单次 inotify 错误不终止 watcher（log + continue）
  - 退出：tx dropped 时后台任务退出

- [ ] **#5.4** 目录自动创建
  - watch path 不存在时：自动 `fs::create_dir_all(path)`（workflow 首次运行）
  - 并记录 progress.jsonl event `{"event":"artifact_dir_created","path":"..."}`

- [ ] **#5.5** Cargo dep
  - 新增：`notify = "6"` 到 `crates/ccteam-core/Cargo.toml`
  - `tokio` 已有；确认 `mpsc` feature 已启用

- [ ] **#5.6** 测试（`tests/artifact_watcher_test.rs`，新文件）
  - **t01_watch_creates_missing_dir**: 指定不存在的 watch path → 目录被创建
  - **t02_new_file_triggers_event**: 写文件到 watch dir → 收到 `Created` event
  - **t03_modified_file_triggers_event**: 改现有文件 → 收到 `Modified` event
  - **t04_debounce_merges_rapid_writes**: 100ms 内写 10 个文件 → 收到 1 个事件
  - **t05_role_name_in_event**: event.role 匹配 workflow spec 中的 agent 名
  - **t06_watcher_drops_cleanly**: drop watcher → 后台任务退出（JoinHandle 完成）
  - **t07_nonexistent_root_dir**: watch root 不存在但父目录存在 → 自动创建
  - **t08_multiple_watch_paths**: 两个 agent 各有不同 watch path → 各自收到事件
  - **t09_deleted_file_event**: 删文件 → 可选：收到 `Deleted` 或无 event（文档化行为）
  - **t10_large_batch_debounce**: 1000ms 内写 100 文件 → 事件数 < 10（debounce 有效）

### 6.2 红线 grep

```bash
# 红线 1：watcher 不解析文件内容
grep -rn "serde_json\|from_str\|parse\|read_to_string" \
  crates/ccteam-core/src/artifact_watcher.rs
# 期望: 0 hit（watcher 只感知文件系统事件，不读内容）

# 红线 2：不直接 spawn agent（只发事件；spawn 由 orchestrator 做）
grep -rn "spawn_session\|HarnessAdapter\|ClaudeCodeAdapter" \
  crates/ccteam-core/src/artifact_watcher.rs
# 期望: 0 hit

# 红线 3：progress.jsonl 写入只在 dir_created 路径
grep -rn "append_progress\|progress.jsonl" \
  crates/ccteam-core/src/artifact_watcher.rs
# 期望: ≤ 1 hit（只有 dir_created 事件写）
```

### 6.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/artifact_watcher.rs` | new | +150 |
| `crates/ccteam-core/src/lib.rs` | edit（pub mod artifact_watcher）| +2 |
| `crates/ccteam-core/Cargo.toml` | edit（notify dep）| +2 |
| `crates/ccteam-core/tests/artifact_watcher_test.rs` | new | +200 |
| `docs/dev-coupling-audit.md` | edit（F64 entry）| +8 |

总约 +150 LOC Rust + 200 LOC 测试 + 10 测试用例。

### 6.4 PR 描述模板

```
v0.4.0-f64: Artifact watcher (inotify/fsevents) (Closes F64)

Maps to:
- requirements.md §痛点 5（自动化推进）
- tech-design.md §2.2 文件系统是控制平面 + §6 扩展点
- docs/v0-4-0/prd.md §F64
- dev-coupling-audit.md F64

Scope:
- ArtifactWatcher: notify crate + debounce（500ms）
- Trigger::Watch path 自动创建，写 progress.jsonl dir_created event
- ArtifactEvent：role + artifact_path + WatchKind

Tests: +10（artifact_watcher_test.rs）
Baseline: 881/0 → 891/0
```

---

## 7. PR #6 — F65 Meta-agent MCP tools

> **目标**：在 `crates/ccteam-cli/src/mcp_serve.rs` 新增 7 个 MCP 工具，
> 让 meta-agent（常驻 CC session）能通过自然语言操作 ccteam orchestrator：
> spawn/stop/observe agent、发 signal、设 parallelism、触发 gate、
> 获取 artifact 摘要。

**关联 PRD**：`docs/v0-4-0/prd.md` §F65

**前置**：F63（WorkflowSpec）+ F64（ArtifactWatcher）已 merge。

### 7.1 任务

#### 7 个新 MCP 工具签名

- [ ] **#6.1** `ccteam__spawn_agent`
  ```json
  {
    "name": "ccteam__spawn_agent",
    "description": "Spawn a named agent role for a project workflow",
    "inputSchema": {
      "slug": "string (project slug)",
      "role": "string (agent role name, must match workflow.yaml agents key)",
      "overrides": "object? (optional SpawnOpts overrides: parallelism, timeout_secs)"
    }
  }
  ```
  实现：调 `Orchestrator::spawn_agent(slug, role)` → 返回 `{"ok":true,"session_id":"..."}`

- [ ] **#6.2** `ccteam__stop_agent`
  ```json
  {
    "name": "ccteam__stop_agent",
    "description": "Stop a running agent session",
    "inputSchema": {
      "slug": "string",
      "role": "string",
      "session_id": "string? (如空则停该 role 所有 session)"
    }
  }
  ```
  实现：`Orchestrator::stop_agent(slug, role, session_id)` → shutdown_session

- [ ] **#6.3** `ccteam__observe_agents`
  ```json
  {
    "name": "ccteam__observe_agents",
    "description": "List all running agent sessions for a project with status",
    "inputSchema": {
      "slug": "string"
    }
  }
  ```
  返回：`{"agents": [{"role":"fixer","session_id":"...","status":"running","cost_usd":0.12}]}`
  实现：读各 `~/.claude/jobs/<id>/state.json`（CC）或 `tmux list-sessions`（Codex）

- [ ] **#6.4** `ccteam__signal`
  ```json
  {
    "name": "ccteam__signal",
    "description": "Send a signal or message to a running agent session",
    "inputSchema": {
      "slug": "string",
      "role": "string",
      "session_id": "string?",
      "signal": "string (pause | resume | btw | interrupt)",
      "message": "string? (仅 btw 时使用)"
    }
  }
  ```
  实现：`pause`/`resume` → SIGSTOP/SIGCONT；`btw` → inbox 写入；`interrupt` → SIGINT

- [ ] **#6.5** `ccteam__set_parallelism`
  ```json
  {
    "name": "ccteam__set_parallelism",
    "description": "Dynamically change the parallelism limit for a workflow agent role",
    "inputSchema": {
      "slug": "string",
      "role": "string",
      "parallelism": "number (1-50)"
    }
  }
  ```
  实现：写 `.ccteam/workflow_overrides.json`（orchestrator 热读）→ 返回 `{"ok":true}`

- [ ] **#6.6** `ccteam__trigger_gate`
  ```json
  {
    "name": "ccteam__trigger_gate",
    "description": "Manually trigger a gate agent (bypass automatic input satisfaction check)",
    "inputSchema": {
      "slug": "string",
      "role": "string",
      "force": "boolean? (default false; true = skip gate condition check)"
    }
  }
  ```
  实现：写 `.ccteam/gate_override/<role>` 文件 → orchestrator 在 next tick 触发

- [ ] **#6.7** `ccteam__get_artifact_summary`
  ```json
  {
    "name": "ccteam__get_artifact_summary",
    "description": "Get counts and latest filenames for each artifact directory",
    "inputSchema": {
      "slug": "string"
    }
  }
  ```
  返回：`{"artifacts": {"issues": {"count":5,"latest":"bug-42.md"}, "fixes": {...}}}`
  实现：遍历 workflow.yaml 中各 agent 的 input/output 目录，统计文件数 + 最新文件名

- [ ] **#6.8** 测试更新（`crates/ccteam-cli/tests/mcp_e2e_test.rs`）
  - 更新 `tool_definitions_have_unique_names_and_object_schemas` 中的 assert 计数
    （原 `assert_eq!(names.len(), 10, ...)` → 改为 `17`，新增 7 工具）
  - **t_spawn_agent_returns_session_id**: mock orchestrator → `ccteam__spawn_agent` 返回 session_id
  - **t_observe_agents_empty**: 无运行 session → `{"agents": []}`
  - **t_set_parallelism_writes_override_file**: 调 set_parallelism → `.ccteam/workflow_overrides.json` 存在
  - **t_get_artifact_summary_empty_dirs**: workflow 目录为空 → `{"artifacts": {}}`
  - **t_trigger_gate_writes_marker**: 调 trigger_gate → `.ccteam/gate_override/<role>` 文件存在
  - **t_signal_btw_writes_inbox**: `signal=btw, message="hello"` → inbox 文件写入

### 7.2 红线 grep

```bash
# 红线 1：新工具仍用 ccteam__ 前缀
grep -n '"name": "ccteam__' crates/ccteam-cli/src/mcp_serve.rs \
  | grep -E "spawn_agent|stop_agent|observe_agents|signal|set_parallelism|trigger_gate|get_artifact_summary"
# 期望: 7 hit（7 个新工具都有名字定义）

# 红线 2：不重建 Agent View（observe_agents 读 state.json，不起新监控服务）
grep -rn "watch.*agent\|monitor.*session\|poll.*tmux" \
  crates/ccteam-cli/src/mcp_serve.rs | grep "observe"
# 期望: 0 hit（observe 是一次性读取，不是持续监控）

# 红线 3：工具总数更新
grep -n "assert_eq.*names.len\|tool.*count\|len.*17" \
  crates/ccteam-cli/tests/mcp_e2e_test.rs
# 期望: 17（或新正确计数）hit

# 红线 4：ccteam-core 零 team 名字面量
grep -rn '"ccteam"\|"chainup"' crates/ccteam-core/src/ | grep -v "test\|fixture"
# 期望: 0 hit
```

### 7.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-cli/src/mcp_serve.rs` | edit（新增 7 工具定义 + handler）| +350 |
| `crates/ccteam-cli/tests/mcp_e2e_test.rs` | edit（计数更新 + 6 新测试）| +150 |
| `docs/interfaces.md` | edit（§MCP tools 新增 7 条）| +80 |
| `docs/dev-coupling-audit.md` | edit（F65 entry）| +10 |

总约 +400 LOC + 20 测试（含更新计数测试）。

### 7.4 PR 描述模板

```
v0.4.0-f65: Meta-agent MCP tools (7 new tools) (Closes F65)

Maps to:
- requirements.md §痛点 9（远程操控）+ §痛点 1（可视化）
- tech-design.md §6.4 MCP + §3.8 用户接口层
- docs/v0-4-0/prd.md §F65
- dev-coupling-audit.md F65

New tools:
  ccteam__spawn_agent, ccteam__stop_agent, ccteam__observe_agents,
  ccteam__signal, ccteam__set_parallelism, ccteam__trigger_gate,
  ccteam__get_artifact_summary

Total MCP tools: 10 → 17
Tests: +6（mcp_e2e_test.rs）
Baseline: 891/0 → 897/0
```

---

## 8. PR #7 — F66 Thin orchestrator

> **目标**：替换 `orchestrator.rs` 中 2713 LOC 的 phase 状态机，写入新的
> workflow 调度逻辑（~400 LOC）：解析 workflow → 根据 trigger 类型 spawn agent
> → 通过 ArtifactWatcher 接 watch 事件 → 检查 gate 条件 → 执行 budget 检查。
> orchestrator 不注入任何 prompt，只做生命周期管理。

**关联 PRD**：`docs/v0-4-0/prd.md` §F66

**前置**：F63（WorkflowSpec）+ F64（ArtifactWatcher）+ F65（MCP tools）已 merge。

### 8.1 任务

- [ ] **#7.1** 新 orchestrator 骨架（替换 `orchestrator.rs` 中 todo!() 桩）
  ```rust
  pub struct Orchestrator {
      paths: CcteamPaths,
      adapters: HashMap<String, Arc<dyn HarnessAdapter>>,  // "claude" | "codex"
  }

  impl Orchestrator {
      pub fn new(paths: CcteamPaths) -> Self { ... }

      pub async fn run_project(&self, slug: &str) -> Result<()> {
          let spec = WorkflowSpec::load_for_project(&project_dir)?;
          let (watcher, rx) = ArtifactWatcher::new(&spec)?;
          let _handle = watcher.start();
          self.dispatch_initial_triggers(slug, &spec).await?;
          self.event_loop(slug, &spec, rx).await
      }
  }
  ```

- [ ] **#7.2** `dispatch_initial_triggers`
  - 遍历 `spec.agents`：`Trigger::Schedule` + `Trigger::Manual` 类型
    不自动触发（log "waiting for trigger"）
  - `Trigger::Watch(path)` → ArtifactWatcher 已注册，等事件
  - 注：首次 run 时如 input 目录非空，立即触发（"已有工件则补触发"）

- [ ] **#7.3** `event_loop`（核心调度循环）
  ```rust
  loop {
      select! {
          Some(evt) = rx.recv() => {
              self.handle_artifact_event(slug, &spec, evt).await?;
          }
          _ = shutdown_signal => break,
      }
  }
  ```

- [ ] **#7.4** `handle_artifact_event`
  - 找 `spec.agents` 中 trigger 匹配 evt.artifact_path 的 agent
  - 检查 parallelism：当前该 role 运行中的 session 数 < agent.parallelism → spawn
  - spawn：`self.adapters[&agent.executor.to_str()].spawn_session(opts)` →
    记 `session_id` 到 progress.jsonl
  - 超 parallelism → 放入 pending queue，有 session 完成时 dequeue

- [ ] **#7.5** Gate 检查（`Trigger::Gate`）
  - 每次 artifact event 或 `trigger_gate` MCP 调用后检查：
    input 目录中 verdict 文件数 ≥ 阈值（默认：所有 fix 文件都有对应 verdict）
  - 条件满足 → 触发 gate agent（同 spawn 逻辑）
  - `.ccteam/gate_override/<role>` 文件存在 → 强制触发并删该文件

- [ ] **#7.6** Budget 检查
  - 每次 spawn 前检查 `ProjectState.cost_usd < budget_limit`
    （budget_limit 来自 `team.yaml` 或默认 $200）
  - 超限 → 不 spawn + 写 progress event `{"event":"budget_exceeded","..."}` +
    发 meta-agent signal（`btw` 消息告警）
  - **永不主动 kill** 已运行 session（只停止新 spawn）

- [ ] **#7.7** Session 完成检测
  - 轮询（or inotify watch）`~/.claude/jobs/<id>/state.json` 的 `status` 字段
  - CC session：state.json `status == "stopped"` → 视为完成
  - Codex session：tmux window 退出 → 视为完成
  - 完成后：dequeue pending（7.4）；写 progress event `{"event":"agent_done","role":"...","session_id":"..."}`

- [ ] **#7.8** Progress.jsonl 写入（7 个 event 类型）
  - `workflow_start`：run_project 开始时
  - `agent_spawn`：每次 spawn_session 成功
  - `agent_done`：每次 session 完成
  - `artifact_received`：每次 ArtifactWatcher 触发
  - `gate_triggered`：每次 gate 条件满足
  - `budget_exceeded`：每次 budget 超限
  - `workflow_done`：所有 gate agent 完成时

- [ ] **#7.9** 测试（`tests/orchestrator_thin_test.rs`，新文件）
  - **t01_run_project_loads_workflow**: 有 workflow.yaml → run_project 不报错启动
  - **t02_no_workflow_returns_error**: 无 workflow.yaml → `WorkflowNotFound`
  - **t03_dispatch_watch_trigger_on_existing_artifact**: watch dir 已有文件 →
    启动时 spawn agent
  - **t04_artifact_event_spawns_agent**: 模拟 ArtifactEvent → `spawn_session` 被调
  - **t05_parallelism_limit_respected**: parallelism=2，已有 2 个 session →
    第 3 个 artifact 不立即 spawn
  - **t06_gate_trigger_fires_on_input_satisfied**: gate 条件满足 → gate agent spawn
  - **t07_budget_exceeded_blocks_spawn**: cost > budget → spawn 被拒，progress event 写入
  - **t08_gate_override_file_force_triggers**: `.ccteam/gate_override/<role>` 存在 → 强制 spawn
  - **t09_completed_session_dequeues_pending**: session 完成 → pending queue 中的任务被 spawn
  - **t10_workflow_done_event_written**: 所有 gate agents 完成 → `workflow_done` event 写入
  - **t11_progress_jsonl_has_correct_events**: run + artifact → progress.jsonl 包含 workflow_start + artifact_received + agent_spawn
  - **t12_budget_preserved_across_restarts**: progress.jsonl 中已有 cost → 重启 Orchestrator 后 budget 检查正确
  - **t13_orchestrator_new_registers_adapters**: 新建 Orchestrator → claude + codex 两个 adapter 已注册
  - **t14_slow_session_completion_no_leak**: session 超时未完成 → orchestrator 不 leak handle
  - **t15_manual_trigger_not_auto_spawned**: manual trigger agent → 启动时不自动 spawn
  - **t16_multiple_watch_agents_independent**: 两个 watch agent 各自 watch 不同目录 → 互不干扰
  - **t17_pending_queue_fifo**: 3 个 artifact 在 parallelism=1 下按序处理
  - **t18_session_error_writes_escalation_event**: spawn_session 失败 → 写 escalation event（不 panic）
  - **t19_escalation_on_3_consecutive_failures**: 同一 role 连续 3 次 spawn 失败 → escalation to meta-agent
  - **t20_meta_agent_not_killed**: meta-agent session → orchestrator 不主动 kill

### 8.2 红线 grep

```bash
# 红线 1：orchestrator 不注入 prompt
grep -rn "send_prompt\|inject_phase\|send-keys.*prompt\|phase_prompt" \
  crates/ccteam-core/src/orchestrator.rs
# 期望: 0 hit

# 红线 2：progress.jsonl 仍是 SoT（7 类 event 都有写）
grep -rn "workflow_start\|agent_spawn\|agent_done\|artifact_received\|gate_triggered\|budget_exceeded\|workflow_done" \
  crates/ccteam-core/src/orchestrator.rs
# 期望: ≥ 7 hit（每个 event 类型各至少 1 处写入）

# 红线 3：不主动 kill 长 session（只 block 新 spawn）
grep -rn "shutdown_session\|kill.*session" crates/ccteam-core/src/orchestrator.rs \
  | grep -v "budget\|override\|force"
# 期望: 0 hit（budget path 不 kill，只 log + block）

# 红线 4：fix-loop 3 次顶必 escalate
grep -rn "escalation\|escalate\|consecutive_failures\|retry_count" \
  crates/ccteam-core/src/orchestrator.rs
# 期望: ≥ 1 hit（escalation 计数逻辑存在）

# 红线 5：ccteam-core 零 team 名字面量
grep -rn '"ccteam"\|"chainup"' crates/ccteam-core/src/orchestrator.rs
# 期望: 0 hit
```

### 8.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/orchestrator.rs` | 大改（保留桩 → 新实现）| +400（从 ~100 桩扩）|
| `crates/ccteam-core/src/lib.rs` | edit（pub mod artifact_watcher 已有）| +0 |
| `crates/ccteam-core/tests/orchestrator_thin_test.rs` | new | +350 |
| `docs/dev-coupling-audit.md` | edit（F66 entry）| +10 |

总约 +400 LOC + 20 测试。

### 8.4 PR 描述模板

```
v0.4.0-f66: Thin orchestrator (workflow-driven dispatch) (Closes F66)

Maps to:
- requirements.md §痛点 5（自动推进）+ §痛点 6（上下文管理）
- tech-design.md §3.5 Orchestrator + §5.5 progress.jsonl SoT
- docs/v0-4-0/prd.md §F66
- dev-coupling-audit.md F66

Scope:
- run_project: load workflow → ArtifactWatcher → dispatch_initial_triggers → event_loop
- handle_artifact_event: parallelism check → spawn_session
- Gate 检查 + gate_override 强制触发
- Budget 检查（永不 kill 已运行 session）
- 7 类 progress.jsonl events
- fix-loop 3 次顶 escalation（meta-agent btw 告警）

Tests: +20（orchestrator_thin_test.rs）
Baseline: 897/0 → 917/0
```

---

## 9. PR #8 — F67 Progress tracking refactor

> **目标**：重构 `progress.rs`，使 progress.jsonl 聚焦业务状态（task 完成度、
> 依赖满足、cost、escalation），删除 session 生命周期跟踪（已由 F66 orchestrator
> 直接写 agent_spawn/agent_done events 承接）。精简 `queries.rs` 查询函数。

**关联 PRD**：`docs/v0-4-0/prd.md` §F67

**前置**：F66 已 merge（新 event 类型已确立）。

### 9.1 任务

- [ ] **#8.1** 重构 `crates/ccteam-core/src/progress.rs`
  - **删除**：`latest_terminal_event_for_phase`（phase 已删）
  - **删除**：`phase_transition_events`、`phase_history` 等 phase-specific 查询函数
  - **新增**：`workflow_cost_total(events: &[Value]) -> f64`（汇总所有 agent_spawn 的 cost）
  - **新增**：`current_agent_sessions(events: &[Value]) -> Vec<AgentSessionSummary>`
    （从 agent_spawn + agent_done 推导当前运行中的 session）
  - **新增**：`escalation_count(events: &[Value]) -> u32`（escalation event 计数）
  - **保留**：`append_progress_event`、`read_all_events`、`latest_event` 等基础函数

- [ ] **#8.2** 重构 `crates/ccteam-core/src/queries.rs`
  - **删除**：所有 `current_phase`、`phase_state`、`decision_candidates` 相关查询
  - **保留**：`project_status`、`session_list`、`event_tail`
  - **新增**：`workflow_summary(slug: &str) -> Result<WorkflowSummary>`
    ```rust
    pub struct WorkflowSummary {
        pub workflow_name: String,
        pub agents: Vec<AgentStatus>,
        pub artifact_counts: HashMap<String, u64>,
        pub total_cost_usd: f64,
        pub escalation_count: u32,
    }
    ```

- [ ] **#8.3** 清理 `crates/ccteam-web/src/routes/` 对 phase 字段的引用
  - `views.rs`：删 `current_phase`、`phase_state`、`decision_candidates` 字段
  - 改为 `workflow_summary: Option<WorkflowSummary>`（如 F68 未完成则 Optional + None）

- [ ] **#8.4** 更新 `docs/interfaces.md`
  - §"progress.jsonl event schema"：删 phase-specific event 类型（`phase_start`、
    `phase_done`、`golden_rules_check` 等）
  - 新增 7 类 workflow event schema（workflow_start、agent_spawn 等，来自 F66 §7.8）

- [ ] **#8.5** 测试（`tests/progress_refactor_test.rs`，新文件）
  - **t01_workflow_cost_total**: 多个 agent_spawn events（各有 cost）→ 正确汇总
  - **t02_current_agent_sessions_open**: spawn 无对应 done → 出现在 current
  - **t03_current_agent_sessions_closed**: spawn + done → 不出现在 current
  - **t04_escalation_count**: 2 escalation events → count=2
  - **t05_workflow_summary_empty**: 无 events → 空 summary（不 panic）
  - **t06_latest_terminal_event_removed**: 确认 `latest_terminal_event_for_phase`
    已删（编译 level：测试文件不引用该函数，确认不 export）
  - **t07_append_and_read_roundtrip**: append + read_all → 内容一致
  - **t08_workflow_summary_with_artifacts**: 含 artifact_received events →
    artifact_counts 正确
  - **t09_cost_accumulation_from_multiple_agents**: 3 个 agent role 各有 cost events
    → total_cost 为三者之和
  - **t10_empty_progress_file_returns_defaults**: 空 progress.jsonl → WorkflowSummary 默认值

### 9.2 红线 grep

```bash
# 红线 1：progress.rs 无 phase 字段引用
grep -rn "current_phase\|phase_state\|phase_history\|golden_rules_check" \
  crates/ccteam-core/src/progress.rs crates/ccteam-core/src/queries.rs
# 期望: 0 hit

# 红线 2：progress.jsonl 仍是 SoT（append_progress_event 保留）
grep -n "pub fn append_progress_event\|pub fn read_all_events" \
  crates/ccteam-core/src/progress.rs
# 期望: ≥ 2 hit

# 红线 3：不解析 tmux output
grep -rn "capture-pane\|pipe-pane\|from_utf8.*tmux" \
  crates/ccteam-core/src/progress.rs crates/ccteam-core/src/queries.rs
# 期望: 0 hit
```

### 9.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-core/src/progress.rs` | edit（删 phase 函数 + 新增 workflow 查询）| -100 + 80 新 |
| `crates/ccteam-core/src/queries.rs` | edit（删 phase 查询 + 新 workflow_summary）| -80 + 60 新 |
| `crates/ccteam-web/src/views.rs` | edit（删 phase 字段）| -30 + 10 新 |
| `crates/ccteam-core/tests/progress_refactor_test.rs` | new | +200 |
| `docs/interfaces.md` | edit（§progress.jsonl event schema 更新）| -20 + 40 新 |
| `docs/dev-coupling-audit.md` | edit（F67 entry）| +8 |

总约 +300 LOC 改 + 10 测试。

### 9.4 PR 描述模板

```
v0.4.0-f67: Progress tracking refactor (Closes F67)

Maps to:
- requirements.md §痛点 1（可视化）
- tech-design.md §5.5 progress.jsonl SoT
- docs/v0-4-0/prd.md §F67
- dev-coupling-audit.md F67

Scope:
- progress.rs: 删 phase 查询函数，新增 workflow_cost_total/current_agent_sessions/
  escalation_count
- queries.rs: 新增 workflow_summary（WorkflowSummary struct）
- views.rs: 删 phase 字段，加 workflow_summary optional
- interfaces.md: 更新 progress.jsonl event schema（workflow event 7 类）

Tests: +10（progress_refactor_test.rs）
Baseline: 917/0 → 927/0
```

---

## 10. PR #9 — F68 ccteam-web adaptation

> **目标**：更新 web UI（`crates/ccteam-web/`）：从 API 侧读 F67 新增的
> `WorkflowSummary`；新增 workflow 视图（agent cards + artifact counts）替代
> phase 视图；更新 F61 新 CC state.json 读路径的 harness panel；
> 删除 SPA 中 phase-specific UI。

**关联 PRD**：`docs/v0-4-0/prd.md` §F68

**前置**：F61（薄 CC adapter）+ F67（WorkflowSummary API）已 merge。

### 10.1 任务

- [ ] **#9.1** 更新 JSON API（`routes/api_v1.rs`）
  - `GET /api/v1/projects/{slug}` 响应 body 新增 `workflow_summary: WorkflowSummary`
    （来自 F67 `queries::workflow_summary`）
  - 删除：`decision_candidates`、`current_phase`、`phase_state` 字段

- [ ] **#9.2** 新增 SPA 页面 `src/pages/WorkflowView.tsx`
  - 渲染 `WorkflowSummary.agents`（agent cards：role / status / cost / session count）
  - 渲染 `WorkflowSummary.artifact_counts`（每个目录 count + 进度条）
  - SSE 更新：订阅 `/sse/project/{slug}` → `artifact_received` / `agent_spawn` /
    `agent_done` events → 局部更新对应 card

- [ ] **#9.3** 替换 phase 视图
  - `ProjectDetail.tsx`：删 phase tab（current_phase badge / PhaseTimeline）
  - 替换为 `WorkflowView` 组件（嵌入 tab 或主视图）

- [ ] **#9.4** 更新 harness panel（F61 新路径）
  - `SessionDetail.tsx`：harness snapshot 来源改为读 `~/.claude/jobs/<id>/state.json`
  - SSE `/sse/harness/{slug}/{sid}` 端点侧：改为 poll state.json（周期 5s）
    而非 statusline JSON watch

- [ ] **#9.5** 新 MCP 工具的 web 控制（可选，V0.4.0 目标，V0.4.1 可延）
  - Dashboard 增加 "Spawn Agent" 按钮 → 调 `POST /api/v1/projects/{slug}/spawn_agent`
  - 对应 Rust handler 透传到 F65 `ccteam__spawn_agent` 逻辑

- [ ] **#9.6** 测试
  - `tests/api_v1_workflow_test.rs`（新文件）：
    - `GET /api/v1/projects/{slug}` 含 `workflow_summary` 字段
    - 不含 `current_phase` / `decision_candidates` 字段（regression guard）
  - playwright `workflow.spec.ts`：
    - WorkflowView 渲染 agent cards
    - SSE agent_spawn event → card status 更新
    - artifact count 显示正确

### 10.2 红线 grep

```bash
# 红线 1：SPA 不重建 Agent View（不实现 live session 监控循环）
grep -rn "setInterval.*agent\|poll.*session\|watch.*session" \
  crates/ccteam-web/web/src/pages/WorkflowView.tsx
# 期望: 0 hit（只订阅 SSE；SSE 是推送，不是 polling）

# 红线 2：SPA 删除 phase 字段引用
grep -rn "current_phase\|phase_state\|decision_candidates\|PhaseTimeline" \
  crates/ccteam-web/web/src/pages/
# 期望: 0 hit

# 红线 3：harness panel 改读 state.json（不读 statusline）
grep -rn "statusline\|derive_harness_path" \
  crates/ccteam-web/src/routes/
# 期望: 0 hit
```

### 10.3 文件 touch 矩阵

| 文件 | 操作 | 行数估 |
|---|---|---|
| `crates/ccteam-web/src/routes/api_v1.rs` | edit（新 workflow_summary + 删 phase fields）| -20 + 40 |
| `crates/ccteam-web/src/views.rs` | edit（WorkflowSummary serialization）| +30 |
| `crates/ccteam-web/web/src/pages/WorkflowView.tsx` | new | +200 |
| `crates/ccteam-web/web/src/pages/ProjectDetail.tsx` | edit（删 phase tab，加 WorkflowView）| -50 + 30 |
| `crates/ccteam-web/web/src/pages/SessionDetail.tsx` | edit（harness panel 新路径）| -20 + 20 |
| `crates/ccteam-web/tests/api_v1_workflow_test.rs` | new | +120 |
| `crates/ccteam-web/web/src/tests/workflow.spec.ts` | new | +150 |
| `docs/dev-coupling-audit.md` | edit（F68 entry）| +8 |

总约 +500 LOC TS + Rust + 8 测试。

### 10.4 PR 描述模板

```
v0.4.0-f68: ccteam-web adaptation (workflow view + thin harness panel) (Closes F68)

Maps to:
- requirements.md §痛点 1（可视化）+ §痛点 3（agent 透明度）
- tech-design.md §3.8 用户接口层
- docs/v0-4-0/prd.md §F68
- dev-coupling-audit.md F68

Scope:
- WorkflowView.tsx：agent cards + artifact counts + SSE live updates
- ProjectDetail：删 phase tab，加 WorkflowView
- harness panel：改读 state.json（CC），codex 改读 capture-pane summary
- API：新增 workflow_summary，删 decision_candidates/current_phase

Tests: +8（api_v1_workflow + workflow.spec.ts）
Baseline: 927/0 → 935/0
```

---

## 11. PR #10 — F69 示例 workflows + e2e + ship gate

> **目标**：补齐示例 workflow 文件（ui-quality-loop + research-loop），
> 跑 e2e 测试套件，更新文档，bump version 到 0.4.0，完成 ship gate。

**关联 PRD**：`docs/v0-4-0/prd.md` §F69

**前置**：F60-F68 全部 merge。

### 11.1 任务

- [ ] **#10.1** 正式 workflow 示例文件（放到 `examples/` 而非 tests fixture）
  - `examples/workflows/ui-quality-loop.yaml`（来自 F63 fixture 确认版）
  - `examples/workflows/research-loop.yaml`
    ```yaml
    name: research-loop
    agents:
      searcher:
        executor: claude
        trigger: manual
        output: .ccteam/research/

      synthesizer:
        executor: claude
        trigger: watch:.ccteam/research/
        parallelism: 3
        input: .ccteam/research/
        output: .ccteam/synthesis/

      reviewer:
        executor: codex
        trigger: watch:.ccteam/synthesis/
        input: .ccteam/synthesis/
        output: .ccteam/reviews/

      reporter:
        executor: claude
        trigger: gate
        input: .ccteam/reviews/
    ```
  - `examples/workflows/README.md`（如用户已明确需要的话新建）：
    说明每个示例的用途 + 如何复制到项目目录

- [ ] **#10.2** e2e 测试套件（`crates/ccteam-core/tests/e2e/`）
  - **e2e_01_workflow_bootstrap**: 新建项目 + 写 workflow.yaml → `ccteam run <slug>` 
    不报错启动（smoke test）
  - **e2e_02_manual_trigger_spawn**: 启动后调 `ccteam__spawn_agent` MCP 工具 →
    对应 agent session 出现在 `ccteam__observe_agents` 结果
  - **e2e_03_artifact_trigger**: 向 watch 目录写文件 → 对应 agent 被自动 spawn
    （end-to-end：watcher → orchestrator → spawn_session）
  - **e2e_04_gate_fires**: fixer agent 产出 verdict 文件满足 gate 条件 →
    shipper agent 被 spawn
  - **e2e_05_budget_guard**: 设 budget_limit=$0.001 → spawn 后立即报 budget_exceeded，
    不再 spawn 新 session
  - **e2e_06_web_dashboard_shows_workflow**: `ccteam web` 启动 → `GET /api/v1/projects/<slug>`
    含 `workflow_summary` 字段
  - **e2e_07_progress_jsonl_integrity**: 完整 e2e run → progress.jsonl 包含
    workflow_start + ≥1 agent_spawn + ≥1 agent_done + workflow_done 全部 7 类

- [ ] **#10.3** CLAUDE.md 更新
  - `§一` 表格：`Workspace version → 0.4.0`；`测试 baseline → <新 baseline>/0`
  - 更新"当前 next"行（V0.3.3 deferred 清理完，V0.4.0 ship 后）
  - `§六` 易踩的坑：添加 V0.3.2 → V0.4.0 升级迁移注意事项

- [ ] **#10.4** 文档更新
  - 新文件 `docs/v0-4-0/README.md`（F60-F69 ship 状态）
  - 新文件 `docs/v0-4-0/user-manual.md`（workflow.yaml 用法 + agent role 配置 + MCP 工具列表）
  - 更新 `docs/dev-coupling-audit.md`：F69 entry + 全 round 总结
  - 更新 `docs/interfaces.md`：确认 workflow.yaml schema 完整性

- [ ] **#10.5** ship gate
  - bump `Cargo.toml` `workspace.package.version` → `0.4.0`
  - `cargo test --workspace` 全绿；记录新 baseline
  - 写 `docs/v0-4-0/e2e-retro.md`（e2e 运行结果 + 发现的问题 + 修复记录）

### 11.2 红线 grep（最终验收）

```bash
# 完整红线 grep 矩阵（见 §12）
```

### 11.3 worktree 命令

```bash
git worktree add -b v0-4-0-f69 /tmp/ccteam-v040-f69 origin/main
cd /tmp/ccteam-v040-f69
cargo test --workspace 2>&1 | grep -E "^test result"
# ... 干活（examples + e2e + docs + bump）...
cargo test --workspace 2>&1 | grep -E "^test result"
sed -i 's/^version = "0.3.2"/version = "0.4.0"/' Cargo.toml
cargo test --workspace 2>&1 | grep -E "^test result"   # 确认 bump 不破测试
git commit -am "v0.4.0: ship gate + version bump"
git push origin v0-4-0-f69
cd /home/ubuntu/rob/ccteam && git worktree remove /tmp/ccteam-v040-f69
```

---

## 12. 红线 grep 矩阵（全 round 汇总）

每个 PR 提交前跑，ship gate PR 再跑一遍全量：

```bash
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# 核心架构红线
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

# 1. phase 系统已彻底删除（F60 完成后）
grep -rn "PhaseTemplate\|inject_directives\|escalate_grammar\|decision_mode\|golden_rules\|PhaseDAG\|Dag::" \
  crates/ccteam-core/src/ crates/ccteam-cli/src/ | grep -v "test\|fixture\|docs"
# 期望: 0 hit

# 2. progress.jsonl 仍是 SoT
grep -n "pub fn append_progress_event\|pub fn read_all_events" \
  crates/ccteam-core/src/progress.rs
# 期望: ≥ 2 hit（基础函数保留）

grep -rn "tail.*progress\|parse.*jsonl\|read.*progress.jsonl" \
  crates/ccteam-web/src/routes/ | grep -v "queries.rs"
# 期望: 0 hit

# 3. 不解析 tmux output（CC adapter）
grep -rn "pipe-pane\|capture-pane\|from_utf8.*tmux" \
  crates/ccteam-core/src/harness.rs
# 期望: 0 hit（CC adapter 改 state.json；codex 用 capture-pane 可有 1 hit）

# 4. 不主动 kill 长 session
grep -rn "kill-session\|kill-server" \
  crates/ccteam-core/src/orchestrator.rs
# 期望: 0 hit

grep -rn "shutdown_session" \
  crates/ccteam-core/src/orchestrator.rs | grep -v "budget\|force\|override"
# 期望: 0 hit（orchestrator 不主动 kill，只 budget block）

# 5. ccteam-core 零 team 名字面量
grep -rn '"ccteam"\|"chainup"' crates/ccteam-core/src/ | grep -v "test\|fixture"
# 期望: 0 hit

# 6. workflow.yaml 无 prompt
grep -rn "prompt:\|system_prompt:\|messages:" \
  examples/workflows/ crates/ccteam-core/tests/fixtures/ | grep ".yaml"
# 期望: 0 hit

# 7. Agent View 不重建（SPA 不实现持续 session 监控循环）
grep -rn "setInterval.*agent\|poll.*session\|ws.*agent.*monitor" \
  crates/ccteam-web/web/src/
# 期望: 0 hit

# 8. fix-loop escalation 存在
grep -rn "escalation\|consecutive_failure\|retry_count" \
  crates/ccteam-core/src/orchestrator.rs
# 期望: ≥ 1 hit

# 9. MCP 工具 ccteam__ 前缀一致
grep -n '"name": "ccteam__' crates/ccteam-cli/src/mcp_serve.rs | wc -l
# 期望: 17（原 10 + 新 7）

# 10. 不写 backwards-compat shim
grep -rn "// V0.3 compat\|// legacy\|deprecated" \
  crates/ccteam-core/src/ crates/ccteam-cli/src/ crates/ccteam-web/src/
# 期望: 0 hit

# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
# 编译 + 测试健康度
# ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

# 11. Rust 编译无 error
cargo check --workspace 2>&1 | grep -c "^error" || true
# 期望: 0

# 12. Clippy 不增（pre-existing 9 条不动）
cargo clippy --workspace --all-targets 2>&1 | grep -c "^error" || true
# 期望: ≤ 9

# 13. Fmt（changed files）
cargo fmt -- --check $(git diff --name-only origin/main..HEAD | grep '\.rs$')
# 期望: clean

# 14. 测试 baseline
cargo test --workspace 2>&1 | grep -E "^test result" \
  | awk '{p+=$4;f+=$6}END{print "passed:",p,"failed:",f}'
# F69 完成后期望: passed ≥ 964, failed = 0
# （866 base + ~98 新测试；F60 删 phase 测试后 net 可能略低，失败数=0 是硬性要求）
```

---

## 13. Subagent 派工模板（通用 briefing skeleton）

每 PR 派 subagent 用以下 briefing skeleton：

```markdown
你是 V0.4.0 PR #<N>（F<NN>）的 implementer agent。

## 任务来源
- `docs/v0-4-0/prd.md` §F<NN> 全文
- `docs/v0-4-0/dev-plan.md` §<对应章节>（本文档）

## 已 lock 的架构方向
- 删除 phase 模板系统（phases.rs / golden_rules.rs / dag.rs 已在 F60 删完）
- workflow.yaml 无 prompt（prompt 在 .claude/agents/<role>.md）
- ClaudeCodeAdapter: claude --bg --agent + state.json（F61 后）
- CodexAdapter: tmux + codex CLI（F62 后）
- Orchestrator: ~400 LOC，不注入 prompt，只做生命周期管理（F66 后）
- Agent View 不重建（ccteam 只读 CC 原生 state.json，不重造监控）

## worktree
```bash
git worktree add -b v0-4-0-f<NN> /tmp/ccteam-v040-f<NN> origin/main
cd /tmp/ccteam-v040-f<NN>
cargo test --workspace 2>&1 | grep -E "^test result"   # 确认 baseline
```

## 任务清单
（从 dev-plan §<N>.1 逐条拷贝；每完成一条打 [x]）

## 红线 grep（提交前必跑）
（从 dev-plan §<N>.2 拷贝；每条贴实际输出）

## 验收
（从 prd.md §F<NN> Acceptance 拷贝）

## PR 提交
```bash
cargo test --workspace 2>&1 | grep -E "^test result"   # 确认通过
cargo fmt -- $(git diff --name-only origin/main | grep '\.rs$')
git add <specific-files>   # 不用 git add -A
git commit -m "v0.4.0-f<NN>: <短描述> (Closes F<NN>)"
git push origin v0-4-0-f<NN>
gh pr create --title "v0.4.0-f<NN>: <短描述>" --body "$(cat <<'EOF'
<dev-plan §<N>.4 PR 描述模板>
EOF
)"
```

## 注意（所有 PR 通用）
- 不动 `references/`（只读参考，不引入为依赖）
- 不动 V0.3.2 已 ship 的 F52-F59 web 层代码，除非 F68 显式清理
- 修改前先 `cargo test --workspace` 看 baseline；完工后 ≥ baseline，failed = 0
- 严禁 `--no-verify`；hook 失败查日志修根因
- 单条问题 ≤ 5 次 fix-loop；第 6 次开始 escalate 回主 session
- commit message 用英语；PR body 可中英混用
- 不写 backwards-compat shim；不写 deprecated 注释
- 改了 `ccteam-core` 公共 API → grep 全 caller（mcp_serve.rs / commands.rs / tests）
- `docs/interfaces.md` 改协议时同步更新（YAML 字段 / JSON shape / CLI 签名）

## F60 实现者额外注意
- 这是净删除 PR；删完后 orchestrator.rs 应退化为约 100 行空桩（保留 struct + new + todo!()）
- 删文件前确认没有其他 crate 在 use 它（grep 全 workspace）
- state.rs 改动：保留 `Idle` + `Done` PhaseState 变体；其余删（F66 会扩展 state）
- 测试数量会下降（删 phase 测试）；baseline 数字降是预期行为，但 failed 必须 = 0

## F61 实现者额外注意
- 先读 `references/` 下 Claude Code SDK 文档确认 `claude --bg --agent` 参数签名
- `state.json` 路径约定：`~/.claude/jobs/<job_id>/state.json`（与 CC 约定一致）
- tmux.rs 模块保留（flex kind 仍用 tmux）；只是 ClaudeCodeAdapter 本身不再调 tmux
- 测试中 mock `claude --bg` 调用：用 `CLAUDE_PATH` env var 覆盖 claude binary 路径

## F62 实现者额外注意
- 先读 `references/codex/codex-rs/README.md` 确认 `codex` CLI 参数签名
- `CODEX_STATUS:` 行格式是约定，需在 `examples/workflows/` 说明文档里写清
- feature flag `codex-tests` 门控：默认 CI 不跑；本地有 codex 时用 `-F codex-tests`

## F65 实现者额外注意
- 原有 10 个 MCP 工具；新增 7 个；test 中 `assert_eq!(names.len(), 17)` 必须更新
- `ccteam__inject_decision` 工具：F60 删了 PhaseTemplate，但该工具在 workflow
  context 下改为"向 inbox 写 JSON decision"；保留工具名，改 handler 逻辑
- 新工具的 inputSchema 必须是 JSON Schema object 类型（参现有工具格式）
```

---

## 14. Ship gate 验收命令

F69 merge 前主 session 跑一遍：

```bash
# ① Rust 测试（硬性：failed = 0）
cargo test --workspace 2>&1 | grep -E "^test result" \
  | awk '{p+=$4;f+=$6}END{print "passed:",p,"failed:",f}'
# 期望: passed ≥ 866（+新测试），failed = 0

# ② Clippy（不增 pre-existing error 数）
cargo clippy --workspace --all-targets 2>&1 \
  | grep "^error" | wc -l
# 期望: ≤ 9（pre-existing baseline）

# ③ Fmt（changed files only）
cargo fmt -- --check $(git diff --name-only origin/main..HEAD | grep '\.rs$')
# 期望: clean（无 diff）

# ④ 全量红线 grep（§12 完整版）
# （贴 §12 所有命令的实际输出到 e2e-retro.md）

# ⑤ 编译检查
cargo build --workspace --release 2>&1 | grep -c "^error" || true
# 期望: 0

# ⑥ 手动 smoke test — workflow 启动
cat > /tmp/test-workflow.yaml <<'EOF'
name: smoke-test
agents:
  worker:
    executor: claude
    trigger: manual
    output: .ccteam/smoke/
EOF
mkdir -p /tmp/smoke-project
cp /tmp/test-workflow.yaml /tmp/smoke-project/workflow.yaml
ccteam run smoke-test 2>&1 | head -20
# 期望: "workflow_start" event，不 panic

# ⑦ 手动 smoke test — web UI
ccteam web --bind 127.0.0.1:7331 &
sleep 2
curl -s http://127.0.0.1:7331/health | python3 -m json.tool
curl -s http://127.0.0.1:7331/api/v1/projects | python3 -m json.tool | head -20
# 期望: health OK；projects 返回 JSON 列表（含 workflow_summary）
kill %1

# ⑧ Version bump 确认
grep '^version' Cargo.toml
# 期望: version = "0.4.0"

# ⑨ playwright（前端 e2e，如 node 环境可用）
cd crates/ccteam-web/web && npm run test 2>&1 | tail -10
# 期望: 全绿（或 skip 说明原因）
```

写完 `docs/v0-4-0/e2e-retro.md`（包含上述命令实际输出截图或文字）后才发 ship gate PR。
