# V0.6.2 — per-role `scope` 切口(大型代码库)

> **Status**:**SHIPPED 2026-05-21**(branch `claude/claude-code-large-codebase-ZC2zh`)。
> Baseline 1410/1 · clippy `-D warnings` clean · 1 finding(F140)· single-PR patch。
>
> **立项来源**:Anthropic 博客《How Claude Code works in large codebases: Best practices and where to start》(2026-05-14)。用户要求"从 ccteam 设计哲学出发吸收建议,不追求全,只追求最高价值"。

---

## 一、设计判断:harness 套 harness,只吸收"拓扑"这一条

文章讲的 harness 是 **inner harness** —— Claude Code / Codex 围绕单 model、单 session 的那一圈(CLAUDE.md / hooks / skills / plugins / LSP / within-session subagent)。ccteam 是 **outer harness**:它不进 session 内部,只决定何时、spawn 哪个 role、几个并发、artifact 怎么流。

筛子:**文章里凡是"单 session 内部就能做、或属于项目仓库配置"的,都是 inner harness 的活,ccteam 碰它就是重复造 harness + 撞自己红线(no prompt injection / progress.jsonl 是 SoT)。ccteam 只该吸收 inner harness 结构上看不见的东西 —— 拓扑。**

按此筛子,文章七八条建议浓缩成**一条半**:

| | 内容 | 形态 |
|---|---|---|
| 一条 | per-role `scope` —— 每次 spawn 的 cwd 钉到子树 | 代码(F140)|
| 半条 | explorer→artifact→editor 认作 ccteam 早已内建的、文章 subagent 建议的多-agent 泛化版,写进大仓 workflow 模板 | 文档(`orchestration-patterns.md §1.5`)|

**刻意不吸收**:CLAUDE.md 内容生成 / self-improving hook / LSP / `permissions.deny` / 结构化搜索 MCP —— 都是 inner harness / 项目仓库职责,ccteam 顶多在 `ccteam doctor` 里**提示**,不**拥有**。

---

## 二、F140 — per-role 代码 `scope`

**问题**:`orchestrator.rs::try_spawn_with_prompt` 把 `SpawnCtx.cwd` 硬编码为 `project_dir`(仓库根)。大代码库下 N 个 agent 全部在根目录起步,各自盲目导航、烧 context 找路。红线 R3"每次 spawn = fresh 1M context"给的是**干净**窗口 —— 但 fresh ≠ scoped:干净的 1M 窗口对着百万行仓库根,光"找路"就烧穿预算。

**改动**:

- `AgentSpec` 新增 `scope: Option<PathBuf>`(`#[serde(default, skip_serializing_if)]`,V0.6.1 workflow.yaml 零改动兼容)。
- `AgentSpec::cwd(project_dir)` —— `Some(scope)` → `project_dir.join(scope)`;`None` → `project_dir`(V0.6.1 默认)。
- `validate_scope` —— 拒绝空路径、绝对路径、含 `..` 的路径(path-traversal guard:workflow.yaml 永远无法把 spawn 指到项目树外)。`WorkflowSpec::validate` 在 per-role 循环里调用。
- orchestrator `SpawnCtx.cwd` 由 `project_dir.to_path_buf()` 改为 `agent.cwd(project_dir)`。
- 目录不存在是运行期问题 —— 走普通 spawn 失败 → `fail_counts` 3-strike escalate(与既有 spawn 失败路径一致,不加 fallback shim)。

`scope` 是纯拓扑决策:inner harness 只看见自己一个 session,结构上做不到;只有 spawn 多 agent 的 outer harness 能给每个 agent 定切口。Claude Code 仍自动向上 walk 目录树、加载沿途 `CLAUDE.md`,root context 不丢。

---

## 三、Files

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs` | `AgentSpec.scope` 字段 + `impl AgentSpec::cwd` + `validate_scope` fn + validate 循环调用 + 7 单元测试 |
| `crates/ccteam-core/src/orchestrator.rs` | `SpawnCtx.cwd` → `agent.cwd(project_dir)` |
| `crates/ccteam-core/tests/{orchestrator_thin,artifact_watcher,budget,graceful_shutdown}_test.rs` | 10 处 `AgentSpec` 字面量补 `scope: None` |
| `docs/interfaces.md` | §17.2 `AgentSpec` 表加 `scope` 行 |
| `docs/orchestration-patterns.md` | 新 §1.5 大型代码库模板(scope + explorer→artifact→editor)|
| `docs/dev-coupling-audit.md` | F140 索引行 |
| `Cargo.toml` / `CLAUDE.md` | version 0.6.1→0.6.2 + §一 baseline 回填 |

---

## 四、验收

- `cargo test --workspace --locked --no-fail-fast` —— 1410 pass / 1 fail(已知 `ccteam-web::workflow_summary_reflects_agent_spawn_and_done_events` running_count flake;baseline 1403/1 + 7 新 scope 测试)。
- `cargo clippy --workspace --all-targets -- -D warnings` —— 0 warning。
- 新增 7 单元测试:`cwd_none_scope_resolves_to_project_root` / `cwd_some_scope_joins_under_project_root` / `validate_accepts_relative_scope` / `validate_rejects_scope_with_parent_dir` / `validate_rejects_absolute_scope` / `validate_rejects_empty_scope` / `scope_round_trips_through_yaml`。
