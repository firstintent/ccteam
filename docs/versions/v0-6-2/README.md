# V0.6.2 — per-role `scope` 切口(大型代码库)

> **Status**:**SHIPPED 2026-05-21**(branch `claude/claude-code-large-codebase-ZC2zh`)。
> Baseline 1412/1 · clippy `-D warnings` clean · 2 finding(F140 + F141)· single-branch patch。
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

**刻意不吸收**:CLAUDE.md 内容生成 / self-improving hook / LSP / `permissions.deny` / 结构化搜索 MCP —— 都是 inner harness / 项目仓库职责,ccteam 顶多**提示**,不**拥有**。

ship 过程中用户追加一个请求:做一个"大型代码库检查"的 skill。它正好把上面"半条"操作化 —— `ccteam-scan` 就是 §1.5 模板里 `explorer` role 的一次性交互版,且严守"只读 advisory / 只提示不拥有"的筛子。落为 **F141**。

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

## 三、F141 — `ccteam-scan` skill(大型代码库导航性体检)

**形态决策**(用户拍板):独立**只读** skill,可随时重跑(对应文章"每 3-6 月 re-audit"),不折进 `ccteam-creator`、不做 doctor 子命令。

**`/ccteam-scan` 做什么**(一次性、只读):

1. 规模与形状 —— 文件数 / 顶层目录 / 语言分布 / 最大子树
2. monorepo 结构探测 —— Cargo workspace / pnpm / npm / Nx / Lerna / Go work / Bazel / Gradle / Maven marker → 枚举 member
3. **`scope:` 建议** —— 为每个 member / 子系统给一行可直接粘进 `workflow.yaml` 的 F140 `scope:` 值(核心产出)
4. navigability gap 体检(advisory)—— 分层 CLAUDE.md / `permissions.deny` 噪声排除 / LSP / codebase map 四项,只报告不修复
5. 产出报告 `<repo>/.ccteam/codebase-scan.md` —— 即 §1.5 explorer→artifact→editor 模板里的 explorer artifact,`/ccteam-creator` 可直接消费

**哲学守线**:只读 advisory。唯一写动作是 `.ccteam/codebase-scan.md` 报告(ccteam 控制平面,非源码 / 非配置);绝不代写 `CLAUDE.md` 内容、不装 LSP —— 那会撞"no prompt injection / 不拥有 inner harness"红线。所有 gap 以"建议"呈现。

**改动**:`skills/ccteam-scan/SKILL.md` 新建;`skill.rs` 接线(`CCTEAM_SCAN_SKILL_NAME` + `CCTEAM_SCAN_SKILL_MD` include_str! + `install_ccteam_scan_skill`);`ccteam doctor --install-skill` 成为第 4 个 shipped skill(control / creator / team / scan)。

> **遗留观察(不在本版修)**:`ccteam doctor --install-skill` 此前只装 3 个 skill;V0.6.0 的 `ccteam`(dispatcher)/ `ccteam-im-setup` / `ccteam-advise` 三个 skill body 在 `skills/` 里但从未接进安装器。本版只把新增的 `ccteam-scan` 接好,未顺带修这 3 个 —— 留作独立 follow-up。

---

## 四、Files

| 文件 | 改动 |
|---|---|
| `crates/ccteam-core/src/workflow.rs` | F140:`AgentSpec.scope` 字段 + `impl AgentSpec::cwd` + `validate_scope` fn + validate 循环调用 + 7 单元测试 |
| `crates/ccteam-core/src/orchestrator.rs` | F140:`SpawnCtx.cwd` → `agent.cwd(project_dir)` |
| `crates/ccteam-core/tests/{orchestrator_thin,artifact_watcher,budget,graceful_shutdown}_test.rs` | F140:10 处 `AgentSpec` 字面量补 `scope: None` |
| `skills/ccteam-scan/SKILL.md` | F141:新建 skill body |
| `crates/ccteam-core/src/skill.rs` | F141:`CCTEAM_SCAN_SKILL_NAME` / `CCTEAM_SCAN_SKILL_MD` / `install_ccteam_scan_skill` + 2 单元测试 |
| `crates/ccteam-core/src/lib.rs` | F141:re-export scan skill 符号 |
| `crates/ccteam-cli/src/commands.rs` | F141:`--install-skill` 接 `ccteam-scan`(selector match + install-all + doc 修正)|
| `docs/interfaces.md` | F140:§17.2 `AgentSpec` 表加 `scope` 行 |
| `docs/orchestration-patterns.md` | F140:新 §1.5 大型代码库模板(scope + explorer→artifact→editor)|
| `docs/dev-coupling-audit.md` | F140 / F141 索引行 |
| `Cargo.toml` / `CLAUDE.md` | version 0.6.1→0.6.2 + §一 baseline + §四 skill 家族 |

---

## 五、验收

- `cargo test --workspace --locked --no-fail-fast` —— **1412 pass / 1 fail**(已知 `ccteam-web::workflow_summary_reflects_agent_spawn_and_done_events` running_count flake;baseline 1403/1 + 7 F140 测试 + 2 F141 测试)。
- `cargo clippy --workspace --all-targets -- -D warnings` —— 0 warning。
- F140 新增 7 单元测试:`cwd_none_scope_resolves_to_project_root` / `cwd_some_scope_joins_under_project_root` / `validate_accepts_relative_scope` / `validate_rejects_scope_with_parent_dir` / `validate_rejects_absolute_scope` / `validate_rejects_empty_scope` / `scope_round_trips_through_yaml`。
- F141 新增 2 单元测试:`ccteam_scan_skill_installs_under_canonical_dir` / `ccteam_scan_skill_install_is_idempotent`。
