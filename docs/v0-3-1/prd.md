# PRD V0.3.1 — Strategic pivot: flex team + multi-session + HarnessAdapter

> 范围:V0.3.1 是 V0.3 主线 ship 后的首个 patch round(跟 V0.2.2 模式一致,
> F-numbered findings under one umbrella),也是 ccteam 的**战略支点版本**。
>
> base = `origin/main` `f9baf3f`(V0.3 ship 终点);测试 baseline `738/0`;
> workspace.version 起点 `0.3.0`,F51 PR 落地时 bump `0.3.1`。
>
> 跟 V0.2.2 / V0.3 不同:V0.3.1 既是命名习惯上的 patch round(F-finding,
> 不 bump main version),又是产品意义上的**战略支点** — 把 ccteam 的
> 自我定位从"phase-driven workflow orchestrator"扩展为"session-layer farm
> + observability + cross-project memory bridge",同时支持 Claude Code
> 与 OpenAI Codex CLI。phase 编排演进**暂停**(无新 phase / 无 Fat Skills
> 演进机制),改起一支新 team kind `flex`(空 phase,用户原生方式驱动多
> session)。

---

## 1. 背景 — 战略 pivot

### 1.1 用户对话源(2026-05-10)

V0.3 在 2026-05-10 ship 后的同一天,用户在 dispatcher session 的
Telegram dialogue(messages 311-318)里把 V0.3.1 的方向钉死。原话节选:

> "team 工厂创建出空的 phases 团队,空团队的能力是 tmux 管理多个 claude
> code 和 codex session,工作流采用用户原始用 claude code 方式来完成...
> 由用户做主动态调整,而不是一开始创建一套永远不变的 phases。"

这条诉求实质把 ccteam 重新定位:不只是一个**预定 phase 编排器**,而是**一个
session farm + observability layer**;用户用最原始的 Claude Code(或 Codex)
姿态干活,ccteam 在一旁观测、记录、提供控制面;recurring pattern 出现后,
**用户**才决定哪些 phase 值得"冻结",并做 promotion(V0.3.2/V0.4 deferred,
非本轮范围)。

### 1.2 用户已 confirm 的关键回答

| Q | Answer |
|---|---|
| team kind 名字 | **`flex`**(暗示"flexible / mutable",CLI 上短) |
| Codex 深度 | **trait stub only** — `CodexAdapter` 完整实现 deferred V0.3.2 |
| 多 session | **拉前 V0.3.1** — 单项目可托管 N session,Claude+Codex 混合,支持 cross-review 模式 |
| Orchestrator 在 flex 团队的行为 | auto_loop / phase prompt 注入 / golden_rules **off** by default;silence_classifier / cost watcher / hooks / progress.jsonl / 跨项目 memory **on** |
| Dashboard | 加 `kind` 列,phase 列 nullable;flex 项目用 per-session cards(N 卡片),每张挂 harness badge |

### 1.3 战略论证(指向 research docs)

V0.3.1 的方向直接落在 ccteam 已有的两份 research doc 论证里:

- **`docs/research/thin-harness-fat-skills-architecture-improvement.md`**
  §1 结论 / §2.1 Thin Harness / §6 改进项:V0.3.1 不让 harness 做厚,反而
  砍掉一支 phase machinery 的扩展(暂停 phase 编排演进),把复杂度全部
  让给 user + LLM 在 session 内自由发挥;**flex 团队是 fat-skill-的-fat-skill**
  演进起点(用户先在 session 里自己跑,recurring pattern 后再考虑 freeze
  到 phase) — 这恰好是 thin harness 论证的极限形态。
- **`docs/research/ccteam-codex-integration.md`** §1 一句话结论 / §6.3
  Codex 作为 ccteam 控制端 / §7 代码改造建议:Codex 集成不应从"替换 Claude
  Code"开始,而应从"作为独立 reviewer / 控制端 / skill 运行面"开始。
  V0.3.1 F47 落 trait stub 是该路线的 M0(文档与手动路径)+ M1(控制面安装)
  的前置 — 把 trait shape 立起来,让后续 V0.3.2 的 `CodexAdapter` 实现有
  地方接入,不必动 V0.3.1 已 ship 的代码。

V0.3.1 的支点性体现在两点:

1. **暂停 phase 编排演进**:没有新 phase,没有新 sub_skill 协议,没有 Fat
   Skills 第一等对象升级 — 这些都 V0.4 deferred。
2. **新增 session-layer 视角**:flex 团队 + adhoc multi-session +
   HarnessAdapter trait,共同把 ccteam 抽象成"多 session 元工具",Claude
   与 Codex 都可作为 first-class harness。

---

## 2. 范围

实现 6 finding(F46-F51)+ 配套 ship gate:

- **F46**:`HarnessAdapter` trait + `ClaudeCodeAdapter`(statusline dual-write
  到 `~/.ccteam/harness/<slug>-<sid>.json`,web SSE 消费 snapshot stream)—
  立 trait,后续 finding 都基于它
- **F47**:`CodexAdapter` trait stub(impl 签名完整,所有方法返
  `Err(NotImplemented)`);`team.yaml::sessions[].harness` 字段;CLI 接受
  `--harness=codex` 但 spawn 时返友好 error
- **F48**:`kind: flex` team kind — `team.yaml::kind` 字段(默认 `workflow`,
  V0.1/V0.2/V0.3 yaml 不动);team factory `ccteam team init <name> --kind=flex`;
  orchestrator behavior gating(`should_run_auto_loop` / `should_inject_phase`
  对 flex 返 false;silence/cost/hooks/progress/memory 不变)
- **F49**:adhoc multi-session primitives — `ccteam session {add,ls,attach,rm}`
  CLI;per-session 子目录布局 `~/projects/<team>-<slug>/sessions/<sid>/`;
  tmux `ccteam-<slug>-<sid>` 命名;混合 harness 共存;`progress.jsonl`
  scoping(`~/.ccteam/progress/<slug>/<sid>.jsonl` 子目录)
- **F50**:web 层更新 — dashboard `kind` 列;flex `/project/<slug>` per-session
  cards + harness badges;新 `/session/<slug>/<sid>` 详情页;SSE filter by
  sid;screenshot endpoint 扩 `<slug>-<sid>.png`
- **F51**:chore + ship gate — workspace.version `0.3.0` → `0.3.1`、
  CLAUDE.md baseline 回填、e2e for flex multi-session、`docs/v0-3-1/e2e-retro.md`、
  `docs/v0-2/README.md` 更新

总规模估:~1.5 kLOC 增量(+ ~30 测试)+ 端到端 ~2-3 周(单人,6 PR 大体串行,
F47 / F48 + F49 / F50 部分可并行)。

---

## 3. F46 — `HarnessAdapter` trait + `ClaudeCodeAdapter`

### 3.1 问题

V0.3 web UI 的 dashboard 数据源 = `progress.jsonl` 事件流 + tmux pane 截图
(F38)。但 Claude Code 的 statusline + subagent 面板里**有大量结构化数据**
(模型名 / context% / token 计数 / cost / rate-limit / subagent 列表 + 进度),
V0.3 dashboard 只能拿到截图视觉版本(像素文本),不可 query;F35 enriched
outbox 也只有 ASCII pane_tail。

进一步,V0.3.1 引入 Codex 后,session 不再只有一种 harness。需要一层 trait
抽象把"harness 元数据 + 控制面"统一,Claude Code 与 Codex 都可填,web /
CLI / orchestrator 不需 if-by-name。

### 3.2 设计

#### 3.2.1 模块位置

新模块 `crates/ccteam-core/src/harness.rs`(后续 sub-module 可拆 `harness/`
目录)。trait + 数据结构 + `ClaudeCodeAdapter` 实现 + `CodexAdapter` stub
都在这里(F47 在 F46 基础上加 stub)。

#### 3.2.2 trait 形态

```rust
// crates/ccteam-core/src/harness.rs

pub trait HarnessAdapter: Send + Sync {
    /// Stable identifier, e.g. "claude-code", "codex".
    fn name(&self) -> &'static str;

    /// Ingest a fresh status snapshot from the harness's native channel
    /// (Claude Code statusline stdin JSON; codex's equivalent TBD).
    /// Returns a normalized HarnessSnapshot the web layer / orchestrator
    /// can consume without knowing which harness generated it.
    fn ingest_snapshot(&self, raw: &str) -> Result<HarnessSnapshot, HarnessError>;

    /// Best-effort subagent state. Returns empty Vec when the harness
    /// doesn't expose this surface (V0.3.1 Claude Code: empty until
    /// upstream API; Codex stub: empty).
    fn subagent_states(&self, snapshot: &HarnessSnapshot) -> Vec<SubagentState> {
        vec![]
    }

    /// Spawn a new session. Returns SessionHandle (tmux session name +
    /// pid + initial state). Codex stub: returns
    /// Err(HarnessError::NotImplemented { ... }).
    fn spawn_session(&self, opts: SpawnOpts) -> Result<SessionHandle, HarnessError>;

    /// Graceful shutdown. The ONLY context that calls this is the user's
    /// explicit `ccteam session rm` request — never silent kill (red line
    /// CLAUDE.md §三).
    fn shutdown_session(&self, handle: &SessionHandle) -> Result<(), HarnessError>;
}

pub struct HarnessSnapshot {
    pub harness: String,                      // "claude-code" / "codex"
    pub model_display_name: String,
    pub context_used_pct: u8,                 // 0-100
    pub cost_usd_total: f64,
    pub rate_limit_pct: Option<u8>,
    pub cwd: Option<PathBuf>,
    pub raw: serde_json::Value,               // full JSON for forward-compat
    pub captured_at: DateTime<Utc>,
}

pub struct SubagentState {
    pub kind: String,                         // "main", "general-purpose", "code-reviewer"
    pub label: Option<String>,                // human label
    pub running_for: Option<Duration>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
}

pub struct SpawnOpts {
    pub project_slug: String,
    pub sid: String,                          // "claude-1" / "codex-2" / ...
    pub cwd: PathBuf,
    pub bypass_permissions: bool,             // CC: --dangerously-skip-permissions
    pub extra_args: Vec<String>,              // harness-specific
}

pub struct SessionHandle {
    pub tmux_session: String,                 // "ccteam-<slug>-<sid>"
    pub harness: String,
    pub sid: String,
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("harness '{harness}' not implemented in V0.3.1: {reason}")]
    NotImplemented { harness: &'static str, reason: &'static str },
    #[error("snapshot ingest failed: {0}")]
    IngestFailed(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

#### 3.2.3 ClaudeCodeAdapter — 完整实现

`ClaudeCodeAdapter` 落 trait 全部方法,数据 flow:

```
Claude Code TUI
  ↓ stdin JSON
statusline-command.sh (wrapper, ccteam-managed)
  ↓ original render path                  ↓ NEW dual-write
TUI footer string                        ~/.ccteam/harness/<slug>-<sid>.json
                                          ↓ notify watch
                                         ccteam-web::routes::sse::handle_harness_sse
                                          ↓ SSE (event: harness_snapshot)
                                         dashboard / project / session detail
```

**slug + sid 推导**(关键):wrapper 从 stdin JSON 的 `.cwd` 字段匹配
`~/projects/<team>-<slug>/sessions/<sid>/` 路径前缀(F49 落地后)或 `~/projects/<team>-<slug>/`
(单 session 项目,V0.3 兼容,sid = `claude-1` 默认)。匹配不到的 session
(meta-agent / random claude session)写 `~/.ccteam/harness/_meta-<handle>.json`
或丢弃(无 panic)。

**wrapper 安装**(`ccteam doctor --install-statusline-adapter`):

- 检测用户原 `~/.claude/statusline-command.sh`;有 → wrapper `tee` stdin 进
  `~/.ccteam/harness/<slug>-<sid>.json` + 透传到原脚本 stdout(保留用户
  自定义 footer);无 → 直接写 dual-write 文件,stdout 输出最简化 footer
- marker 保护用户手改:`# ccteam-managed:statusline begin / end`(同 V0.2.2
  F39 / F44 marker pattern,F44 反向后实际 marker 文本是
  `<!-- ccteam-managed:skill -->` 同款,这里是 sh comment 形式)
- 幂等:`doctor` 重跑只覆盖 marker section,用户手改段保留

**红线 reminder**:不解析 statusline 渲染输出,只解析 stdin JSON。
`progress.jsonl` 仍是 orchestrator 的状态 SoT,harness snapshot 是
**presentation-only 信息**,**不参与状态判定**(CLAUDE.md §三 红线)。

#### 3.2.4 web SSE consumer

新 SSE endpoint:

| Path | 内容 |
|---|---|
| `GET /sse/harness/<slug>` | 推该 slug 下所有 session 的 harness_snapshot 事件 |
| `GET /sse/harness/<slug>/<sid>` | 单 session 过滤,详情页用 |

wire format 跟 V0.3 progress SSE 一致,event name `harness_snapshot`,data
是 `HarnessSnapshot` 序列化 JSON(单行)。watcher 监 `~/.ccteam/harness/`
目录,文件 modify → 读取 → 广播。

dashboard / project / session detail 的 askama 模板加 harness panel(model
进度条 / cost 累计 / rate-limit %)— 详 F50。

### 3.3 不做(V0.3.1 内,V0.4 deferred)

- **Claude Code subagent live progress**(token counts in flight)— 上游无 API
- **statusline 数据进入 orchestrator 决策** — 破 §三 SoT 红线,永久 deferred
- **harness snapshot 历史 archive**(retro 用)— V0.4
- **wrapper 自动检测多用户 statusline 脚本**(systemd / launchd 风险)— V0.4
- **CodexAdapter 实现** — 见 F47

### 3.4 验收

- [ ] `crates/ccteam-core/src/harness.rs` 模块落地,trait + 数据结构 + 错误类型
- [ ] `ClaudeCodeAdapter` 实现 trait 全部方法
- [ ] `ccteam doctor --install-statusline-adapter` 安装 wrapper(marker 保护)
- [ ] `~/.ccteam/harness/<slug>-<sid>.json` 文件 dual-write happy path 测试
- [ ] `/sse/harness/<slug>` SSE endpoint 推送 harness_snapshot 事件
- [ ] 用户已有自定义 statusline 脚本时,wrapper tee + 透传不破坏原 footer
  (回归测试 fixture)
- [ ] 路径匹配不到 slug 时,fallback 路径不 panic(可丢弃或写
  `~/.ccteam/harness/_meta-<handle>.json`)
- [ ] 单元测试:`HarnessSnapshot` round-trip serialize / deserialize;
  `ClaudeCodeAdapter::ingest_snapshot` 解析 5 种 statusline JSON shape
  (官方 + 用户魔改)
- [ ] `cargo test --workspace` baseline 738 不退步;新增 ≥ 8 测试

---

## 4. F47 — `CodexAdapter` trait stub + `harness` 字段

### 4.1 问题

V0.3.1 不实现 Codex 完整支持(`docs/research/ccteam-codex-integration.md`
M2-M5 路线,~月级工程量),但需要把 trait shape **现在就立**,让 V0.3.2
的 `CodexAdapter` 实现有地方接入,不必动 V0.3.1 已 ship 的代码;同时 CLI
和 team.yaml schema 都需要识别 `harness: codex` 让用户提前声明意图。

### 4.2 设计

#### 4.2.1 `CodexAdapter` stub

`crates/ccteam-core/src/harness.rs` 加 `CodexAdapter` 空 struct:

```rust
pub struct CodexAdapter;

impl HarnessAdapter for CodexAdapter {
    fn name(&self) -> &'static str { "codex" }

    fn ingest_snapshot(&self, _raw: &str) -> Result<HarnessSnapshot, HarnessError> {
        Err(HarnessError::NotImplemented {
            harness: "codex",
            reason: "Codex adapter is trait-stub in V0.3.1; full implementation tracked in docs/v0-3-1/prd.md §F47, deferred to V0.3.2+. Use --harness=claude or wait.",
        })
    }

    fn spawn_session(&self, _opts: SpawnOpts) -> Result<SessionHandle, HarnessError> {
        Err(HarnessError::NotImplemented {
            harness: "codex",
            reason: "Codex spawn is trait-stub in V0.3.1; full implementation tracked in docs/v0-3-1/prd.md §F47 + docs/research/ccteam-codex-integration.md M1, deferred to V0.3.2+. Use --harness=claude or wait.",
        })
    }

    fn shutdown_session(&self, _handle: &SessionHandle) -> Result<(), HarnessError> {
        Err(HarnessError::NotImplemented {
            harness: "codex",
            reason: "Codex shutdown is trait-stub in V0.3.1; deferred to V0.3.2+.",
        })
    }
}
```

#### 4.2.2 `team.yaml::sessions[].harness` 字段(flex 团队用)

flex 团队的 `team.yaml`(F48 引入 `kind: flex` 后)可声明默认会话:

```yaml
# teams/<flex-team>/team.yaml
name: my-flex
kind: flex                                   # F48 引入
description: "My exploratory workspace"
sessions:                                    # F47 新字段(仅 kind: flex 时有意义)
  - sid: claude-1
    harness: claude                          # claude | codex
  - sid: codex-1
    harness: codex
```

`sessions:` 是**默认初始 session 列表**(team factory bootstrap 时给项目预设;
用户可后续 `ccteam session add/rm` 改);非 flex 团队该字段忽略(deny_unknown_fields
不开,该字段直接是 `Option<Vec<DefaultSessionSpec>>` default `None`)。

#### 4.2.3 CLI 接受 `--harness=codex`

`ccteam session add <slug> --harness codex` 在 V0.3.1 接受 flag,但执行到
`CodexAdapter::spawn_session` 时返 `Err(HarnessError::NotImplemented)`,CLI
打印友好 error:

```
$ ccteam session add my-flex-foo --harness codex
Error: Codex spawn is trait-stub in V0.3.1; full implementation tracked in
docs/v0-3-1/prd.md §F47 + docs/research/ccteam-codex-integration.md M1,
deferred to V0.3.2+. Use --harness=claude or wait.
```

(用户体感清晰指向追踪 issue;不 panic / 不无声 fallback。)

#### 4.2.4 doctor 检查

`ccteam doctor` 加 codex 检测段(用 `which codex` 检测 binary 是否在 PATH;
不强 fail 任何条件,只输出 informational):

```
[ccteam] codex CLI: not found (V0.3.1 trait-stub only; install codex CLI for V0.3.2+).
```

(类比 V0.2.2 ship 的 `claude-mem` 可选检测模式。)

### 4.3 不做(V0.3.1 内,V0.4 deferred)

- **Codex statusline / hook 实际接入** — V0.3.2(`ccteam-codex-integration.md`
  M1)
- **`mcp__codex__codex` MCP peer 注册给 meta-agent** — V0.3.2 / V0.4
- **AGENTS.md 模板生成**(同 CLAUDE.md 双栈)— V0.4
- **Codex skill 双栈分发**(`.agents/skills/`)— V0.4(详见 codex-integration
  doc M4)
- **CodexExecRunner sub-skill runner** — V0.4
- **`codex mcp-server` 注册给 Claude meta-agent** — V0.4

### 4.4 验收

- [ ] `CodexAdapter` struct 实现 `HarnessAdapter` trait,所有方法返
  `Err(HarnessError::NotImplemented)` + reason 指向本 PRD 与 codex-integration
  research doc
- [ ] `team.yaml::sessions[]` 字段 schema 解析;`harness: claude | codex`
  枚举(serde 默认 `claude`)
- [ ] `ccteam session add <slug> --harness codex` CLI 接受 flag,执行返
  友好 error + exit code 1
- [ ] `ccteam doctor` 输出 codex CLI 检测信息(present / not-found 都不 fail)
- [ ] interfaces.md §5.5 `team.yaml` schema 加 `kind` + `sessions[]` 字段
- [ ] 单元测试:`CodexAdapter::spawn_session` 返 NotImplemented + 错误
  消息含 V0.3.2 引用;`team.yaml` parse 时 `harness: codex` 被接受不 fail
- [ ] `cargo test --workspace` 不退步;新增 ≥ 5 测试

---

## 5. F48 — `kind: flex` team kind

### 5.1 问题

V0.1/V0.2/V0.3 的 ccteam 团队都是 phase-driven(`workflow` 暗含),
`parallelism: multi_session` 是 phase 级字段(per-phase fan-out 拓扑),
不是 team 级 kind。

V0.3.1 需要一支**空 phase 的 team**:用户起项目后,ccteam **不**注入 phase
prompt,**不**跑 auto_loop,**不**做 golden_rules check;但 hooks /
progress.jsonl / silence_classifier / cost watcher / 跨项目 memory bridge
**仍跑**(observability 全保留)。

### 5.2 设计

#### 5.2.1 `team.yaml::kind` 新字段

`crates/ccteam-core/src/team.rs::TeamSpec` 加字段:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,

    /// V0.3.1 F48 — team kind. Defaults to `workflow` so V0.1 / V0.2 /
    /// V0.3 yamls (which omit this field) parse unchanged.
    ///
    /// - `workflow` — phase-driven, single tmux session (default; dev /
    ///   meta-agent / `ccteam-project-creator`)
    /// - `multi_workflow` — phase-driven, master + predefined sub-modules
    ///   (research; existing `parallelism: multi_session` semantics)
    /// - `flex` — empty phase, user drives sessions adhoc; ccteam observes
    ///   + records + provides controls. No auto_loop / phase prompt /
    ///   golden_rules. silence/cost/hooks/progress/memory still run.
    #[serde(default)]
    pub kind: TeamKind,

    /// V0.3.1 F47 — default session spec for `kind: flex` teams. Ignored
    /// for `workflow` / `multi_workflow`. Used by team factory to bootstrap
    /// initial sessions; user can `ccteam session add/rm` after.
    #[serde(default)]
    pub sessions: Vec<DefaultSessionSpec>,

    // ... existing fields (description / phase_dir / retro_schema / ...)
    // unchanged
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamKind {
    Workflow,
    MultiWorkflow,
    Flex,
}

impl Default for TeamKind {
    fn default() -> Self { Self::Workflow }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultSessionSpec {
    pub sid: String,                          // "claude-1" / "codex-1"
    #[serde(default)]
    pub harness: HarnessKind,                 // claude | codex
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Claude,
    Codex,
}

impl Default for HarnessKind {
    fn default() -> Self { Self::Claude }
}
```

**back-compat**:V0.1/V0.2/V0.3 已 ship 的 `team.yaml`(`dev` / `research` /
`meta-agent` / `ccteam-project-creator` / 用户自建)都不含 `kind`,`#[serde(default)]`
解析为 `TeamKind::Workflow` — 行为完全不变。

**校验**(`TeamSpec::validate`):
- `kind: flex` 团队**必须**有空或缺失 `phase_dir`(或 `phases/` 目录不存在)
  — 强制 flex 无 phase
- `kind: flex` 团队**不允许**填 `auto_loop` / `escalate_grammar_extensions` /
  `golden_rules`(parse 时 fail-loud — 用户明确知道 flex 团队 no phase
  machinery)
- `kind: flex` 团队**允许**填 `claude_md_template` / `aliases` / `description` /
  `evergreen` / `cost_policy` / `sessions`
- `kind: workflow / multi_workflow` 团队的 `sessions: []` 字段**忽略**(parse
  时不 fail,但 doctor 跑出 warning 提示该字段只对 flex 有意义)

#### 5.2.2 `kind` 与 `parallelism` 正交

`kind` 是**team 级**字段(在 team.yaml),`parallelism` 仍是**phase 级**
字段(在 phase markdown frontmatter)。两者关系:

| kind | parallelism(phase 级) | 例 |
|---|---|---|
| `workflow` | per-phase: `solo` / `agent_team` | `dev`(V0.1+);phase 通常 solo |
| `multi_workflow` | per-phase: `multi_session`(在 fan-out phase) | `research`(V0.2.2 alias 后 canonical 名) |
| `flex` | N/A(无 phase) | 新 V0.3.1 |

flex 团队**没有 phase**,所以 `parallelism` 字段不存在 — 不强制要求
flex team yaml 写 `parallelism: multi_session` 之类,避免概念污染。

#### 5.2.3 orchestrator behavior gating

`crates/ccteam-core/src/orchestrator.rs` 的 tick 循环加 kind-aware
helper:

```rust
impl TeamRuntime {
    fn should_run_auto_loop(&self) -> bool {
        match self.spec.kind {
            TeamKind::Workflow | TeamKind::MultiWorkflow => true,
            TeamKind::Flex => false,
        }
    }

    fn should_inject_phase(&self) -> bool {
        match self.spec.kind {
            TeamKind::Workflow | TeamKind::MultiWorkflow => true,
            TeamKind::Flex => false,
        }
    }

    fn should_check_golden_rules(&self) -> bool {
        match self.spec.kind {
            TeamKind::Workflow | TeamKind::MultiWorkflow => true,
            TeamKind::Flex => false,
        }
    }
}
```

**保留启用**(flex 团队仍跑):
- silence_classifier(F35,事件流监测,与 phase 无关)
- cost watcher(F35 / V0.1 阈值 — soft warn 5/15/30min,hard kill 仅 cost
  > $200)
- hooks(`progress_append` / `cost_accumulate` / `parse_phase_end` /
  `intercept_ask_decision` / `load_context`):全部跑;`parse_phase_end`
  实际不会触发(flex 没 phase prompt → 没 PHASE_DONE: signal)但代码路径
  存在不冲突
- progress.jsonl 写入(进 `<slug>/<sid>.jsonl` 子文件,F49 落)
- 跨项目 memory bridge(rules 文件读写)
- enriched outbox(V0.2.2 F35,silence 触发的 escalation;flex 团队也会卡)

#### 5.2.4 `claude_md_template` 在 flex 团队的语义

flex 团队的 `<project>/CLAUDE.md` 仍由 `bootstrap_project` 写,模板从
`team.yaml::claude_md_template`(已存在字段,V0.2 M0.16 ship)读取;flex
团队的默认模板(seed yaml 自带)应**显式**告诉 session 内的 Claude:

> "本项目是 flex 团队,无预定 phase。你按照原始 Claude Code 姿态自由
> 工作即可。ccteam 在监测 progress.jsonl 与 cost,silence 长时会 escalate
> 给 meta-agent / 用户 channel,不会主动 kill 你。需要并行 worker 时,
> 跟 meta-agent 说 `ccteam session add <slug>` 起新 session。"

模板里**不**写"完成后写 PHASE_DONE: ..."之类协议关键字。

#### 5.2.5 team factory `--kind=flex`

`ccteam team init <name> --kind=flex` 生成的 staging tree:

```
~/.config/ccteam/teams/<name>/
├── .claude-plugin/plugin.json     # plugin manifest(同 V0.2 M0.22)
├── team.yaml                      # kind: flex + 默认 sessions[claude-1]
├── README.md
└── (无 phases/ 目录 — flex 团队无 phase)
```

跟 V0.2 M0.22 已 ship 的 `ccteam team init` 工厂一致,只是 phase 模板不
scaffold(`init_team_staging` 检测 `kind: flex` 跳过 phase scaffold)。

`ccteam team publish <name>` 把 staging 拷到 `~/.ccteam/teams/<name>/`(同
V0.2 M0.22 行为);doctor 校验跳过 phase 引用(`kind: flex` 团队的
`tools_required` 不必校验 phase 一致性)。

### 5.3 不做(V0.3.1 内,V0.4 deferred)

- **flex workflow promotion / demotion UX** — 用户在 session 里跑出
  recurring pattern 后,把 N 条 progress 事件 promote 成一支冻结 phase;
  反向:把已冻结 phase 拆回 flex。**这是 Fat Skills evolution 路径**(详
  `thin-harness-fat-skills-architecture-improvement.md` §6.1),V0.3.1
  ship 只 ship 空 base,V0.3.2 / V0.4 落 promotion 机制
- **`flex_workflows.yaml` per-team 持久化 schema**(promotion 后冻结的 phase
  存哪里) — V0.3.2 / V0.4
- **flex 团队的 retro phase** — V0.3.2(retro_schema 字段虽然存在,但 retro
  phase 是 phase machinery,flex 不跑;V0.3.2 用户跑完 promote 后再评估)
- **flex 团队的 verdict_schema** — 同上
- **flex 团队 plugin 形态强校验** — V0.4 evolve plugin schema 时再做
- **flex 团队的并行预算 / max_concurrent_sessions 配置** — V0.4(用户先用,
  撞性能墙再加)

### 5.4 验收

- [ ] `crates/ccteam-core/src/team.rs::TeamSpec` 加 `kind: TeamKind` +
  `sessions: Vec<DefaultSessionSpec>` 字段,`#[serde(default)]` 保 V0.1/V0.2/V0.3
  yaml 解析不变
- [ ] `TeamSpec::validate` 拒绝 `kind: flex` + `golden_rules` / `escalate_grammar_extensions`
  / 非空 phase_dir 的非法组合
- [ ] orchestrator 三个 helper(`should_run_auto_loop` / `should_inject_phase` /
  `should_check_golden_rules`)对 flex 返 false;tick 循环引用 helper 不再
  字符串硬编码 `if state.team == ...`(strategic doc §3 红线)
- [ ] `ccteam team init <name> --kind=flex` scaffold staging tree(无 `phases/`
  目录)
- [ ] `ccteam team publish <name>` flex 团队成功 publish 到 `~/.ccteam/teams/<name>/`
- [ ] `doctor --validate-team <name>` 对 flex 团队跳过 phase 引用校验,只校
  team.yaml + plugin.json 一致性
- [ ] 已存在的 `dev` / `research` / `meta-agent` / `ccteam-project-creator` 团队
  在 V0.3.1 升级后**完全无变化**(回归测试 fixture)
- [ ] 单元测试:`TeamSpec` round-trip serialize / deserialize 含 `kind`;
  flex bootstrap 写出的 `<project>/state.json` 字段(`team` / `parallelism: solo`
  默认,无 phase 字段触发);interfaces.md §5.5 schema 同步
- [ ] `cargo test --workspace` 不退步;新增 ≥ 8 测试

---

## 6. F49 — Adhoc multi-session primitives

### 6.1 问题

V0.3 之前 ccteam 项目 = **单 tmux session**(except `parallelism: multi_session`
的 fan-out phase,但那是 phase 级**预定义** sub-module 拓扑)。flex 团队
要支持**adhoc**:用户起项目后任意时刻 `ccteam session add <slug>` 起新
session,删 session,attach 到任一 session,混合 Claude+Codex(V0.3.1 codex
spawn 仍 NotImplemented,但 schema 接得住)。

multi_session 的 master + 预定义 sub-module 拓扑**不**适合 adhoc:

- multi_session 是 phase 级配置,sub-module 列表来自 phase prompt(plan-eng
  写 interface-contracts);flex 没 phase
- multi_session sub-module 的 `state.json` 跟 master 同 schema,但子目录是
  *代码*目录(`backend-api/` / `frontend-dashboard/`);adhoc session 不
  对应代码目录(同一项目同一仓库,用户可能起 N session 都跑同一代码 — 例
  CC 实现 + Codex review)

### 6.2 设计

#### 6.2.1 文件系统布局

flex 项目目录:

```
~/projects/<team>-<slug>/                # project root,同 V0.3
├── .ccteam/
│   ├── state.json                       # master state(扩 sessions 字段,§6.2.2)
│   ├── inbox/ outbox/                   # 项目级 inbox/outbox(同 V0.3)
│   └── sessions/                        # NEW V0.3.1 F49
│       ├── claude-1/
│       │   ├── state.json               # per-session state(子集字段)
│       │   ├── inbox/ outbox/           # per-session inbox/outbox(可选;
│       │   │                            #  默认走项目级 — V0.3.2 评估细分)
│       │   └── (per-session 工件 .md)
│       ├── claude-2/
│       └── codex-1/
└── (项目代码 — flex 团队下,该目录可能空,可能用户手填或一开始就 git
   clone 进来,ccteam 不假设)
```

**对比 V0.3 multi_session**(`docs/interfaces.md §1.3`):multi_session 把
*代码*分到 `<slug>/backend-api/` / `<slug>/frontend-dashboard/` 子目录,
sub-module 是**代码维度**;flex 把 *session*分到 `<slug>/.ccteam/sessions/<sid>/`,
session 是**进程维度**,代码不分。两条独立机制,V0.3.1 F49 不复用 V0.3
multi_session 路径(避免概念污染)。

**progress.jsonl scoping**:

```
~/.ccteam/progress/
├── <slug>.jsonl                         # 单 session 项目(workflow /
│                                          multi_workflow,V0.3 兼容路径)
└── <slug>/
    ├── claude-1.jsonl                   # flex 项目 per-session(F49)
    ├── claude-2.jsonl
    └── codex-1.jsonl
```

flex 项目 progress 写入 `~/.ccteam/progress/<slug>/<sid>.jsonl`,文件名
`<sid>` 不含 `<slug>` 前缀(子目录已分);workflow / multi_workflow 项目
仍写 `<slug>.jsonl` flat(不动 V0.3 协议)。`hooks::progress_append` 解析
`state.json::team_kind`(从项目目录推 — bootstrap 写入)决定路径形态。

watcher(V0.3 M5.2 ship)默认 recursive 监 `~/.ccteam/progress/`,flex 子
目录天然被覆盖。

#### 6.2.2 master `state.json::sessions` 字段

flex 项目的 master state.json 在 V0.3 字段基础上加:

```json
{
  "slug": "my-flex-foo",
  "team": "my-flex",
  "team_kind": "flex",                              // F48 / F49 新字段
  "tmux_session": "ccteam-my-flex-foo-claude-1",    // 第一个 session(legacy 兼容)
  "sessions": {                                     // F49 新字段
    "claude-1": {
      "harness": "claude",
      "tmux_session": "ccteam-my-flex-foo-claude-1",
      "started_at": "2026-05-10T12:00:00Z",
      "pid": 12345
    },
    "codex-1": {
      "harness": "codex",
      "tmux_session": "ccteam-my-flex-foo-codex-1",
      "started_at": "2026-05-10T12:05:00Z",
      "pid": null
    }
  },
  "next_sid_seq": {                                 // sid 单调递增,删后不复用
    "claude": 2,
    "codex": 2
  },
  "...": "其他字段同 V0.3 §2.1"
}
```

`team_kind: workflow | multi_workflow` 项目的 state.json 不写 `sessions:` /
`next_sid_seq:`(serde 默认 `None` / `BTreeMap::new()`),完全 V0.3 兼容。

#### 6.2.3 sid 格式

`<harness>-<n>`,n 从 1 开始单调递增,**删后不复用**(避免 attach 错 session);
全项目内唯一(不同 harness 各自计数 — `claude-1` 与 `codex-1` 共存)。
`next_sid_seq[harness]` 字段记下一个可用 n。

#### 6.2.4 tmux session 命名

`ccteam-<slug>-<sid>`,如 `ccteam-my-flex-foo-claude-1` / `ccteam-my-flex-foo-codex-2`。
**workflow / multi_workflow** 项目仍是 `ccteam-<slug>` 不带 sid 后缀(V0.3
兼容);flex 项目第一个 session 也带 `-<sid>` 后缀(无歧义)。

#### 6.2.5 CLI 设计

```
ccteam session add <slug> [--harness claude|codex] [--sid <name>]
ccteam session ls <slug>
ccteam session attach <slug> <sid>
ccteam session rm <slug> <sid>
```

详细行为:

| 子命令 | 行为 | 失败 |
|---|---|---|
| `add` | 调 `<HarnessAdapter>::spawn_session`(F46);写 master state.json `sessions[]`;tmux new-session;hooks 启动 | codex stub → friendly error;tmux 失败 → fail-loud + state 不写 |
| `ls` | 读 master state.json `sessions{}` → 表格(sid / harness / tmux / started / pid / status from hooks last event) | 项目不存在 → 404 |
| `attach` | tmux attach `ccteam-<slug>-<sid>` | session 不在 → fail-loud 列出可用 sid |
| `rm` | 调 `shutdown_session`(graceful);tmux kill;清 master state.json sessions[sid];**这是唯一**显式 kill 路径 | 不在 → noop + warn |

**红线 reminder**:`rm` 是**唯一**用户显式授权的 kill 路径。silence /
cost / stale 不触发 kill(CLAUDE.md §三)。

#### 6.2.6 跟 V0.3 multi_session 的边界

`parallelism: multi_session` 仍**保留**给 workflow / multi_workflow 团队
的 phase 级 fan-out(research 团队的 plan → fan-out → impl-parallel → fan-in
拓扑)— **不动**,V0.3 ship 行为完全保持。

flex 的 adhoc multi-session 是**新机制**,正交;workflow 团队的项目无法
`ccteam session add`(因为该团队 `kind: workflow`,master state.json 没
`sessions{}` 字段架构;CLI 检测 → friendly error "session add only works
on flex teams")。

### 6.3 不做(V0.3.1 内,V0.4 deferred)

- **跨 session orchestration / 协调**(eg "claude-1 完成 implement 后自动
  发 codex-1 review")— V0.4 channel adapter / pipeline 评估;V0.3.1 用户
  自己 `/btw` 派
- **session 级 inbox/outbox 细分** — 可选(`<sid>/inbox/`),V0.3.1 默认走
  项目级 inbox/outbox;V0.3.2 if 用户撞 cross-talk 再加
- **session 间共享 context / lessons** — V0.3.1 不做,session 各自跑;
  cross-project memory bridge(`~/.claude/rules/`)正交不动
- **N session > 配置阈值时拒绝 add** — V0.4(用户撞性能再加 max_sessions_per_project)
- **session rename(`ccteam session rename ...`)** — V0.4(等同 slug rename
  V0.3+ deferred 一起评估)
- **flex 项目的 `interface-contracts.md`** — 没 fan-out / fan-in concept,
  不需要

### 6.4 验收

- [ ] `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/` 子目录在
  `ccteam session add` 时创建
- [ ] master `state.json::sessions{}` + `next_sid_seq{}` 字段写入并 round-trip
  解析;V0.3 单 session 项目 state.json 不含这俩字段(serde default)
- [ ] `~/.ccteam/progress/<slug>/<sid>.jsonl` 子目录路径在 hooks 写入时分流
  (flex 团队);workflow / multi_workflow 项目仍 `<slug>.jsonl` flat
- [ ] `ccteam session add <slug> --harness claude` happy path:tmux new-session +
  state 写入 + hooks 起来 + claude bypass-perms session 启动
- [ ] `ccteam session add <slug> --harness codex` 返友好 error(F47 stub)
- [ ] `ccteam session ls <slug>` 返表格 + per-session status(从
  per-session progress.jsonl 末事件推)
- [ ] `ccteam session attach <slug> <sid>` 调 tmux attach;不存在 sid
  → fail-loud 列可用
- [ ] `ccteam session rm <slug> <sid>` graceful shutdown + tmux kill +
  state 清理;**仅在显式调用时**触发 kill
- [ ] sid 格式 `<harness>-<n>` 单调递增 + 删后不复用(`next_sid_seq` 字段
  保护);单元测试覆盖 race(并发 add)
- [ ] CLI 在 workflow / multi_workflow 团队上调 `ccteam session add` 返
  friendly error("session subcommands only work on flex teams")
- [ ] interfaces.md §1.3 / §2.1 schema 同步加 flex layout + state.json
  sessions 字段
- [ ] `cargo test --workspace` 不退步;新增 ≥ 12 测试

---

## 7. F50 — Web 层更新

### 7.1 问题

V0.3 web UI(M5.0-M5.4 ship)假设单 session 项目;V0.3.1 引入 flex 团队 +
adhoc multi-session 后:
- dashboard 表格里 `phase` 列对 flex 项目无意义(没 phase)
- project 详情页的 events / outbox / screenshot panel 应分到 N session
- harness snapshot stream(F46)需要前端消费

### 7.2 设计

#### 7.2.1 dashboard `/`

V0.3 列:`Slug / Team / Phase / Last event / Status badge / Cost`

V0.3.1 加列:`Kind`(workflow / multi_workflow / flex)。`Phase` 列对 flex
项目渲染 `—` 或 `manual`(空 string 渲染 `—` 更清晰)。`Status badge` 对
flex 项目仍跑 silence_classifier 分类(主要 `Healthy` / `MidToolHung` /
`PostStopLimbo` 等仍适用 — 与 phase 无关)。

模板更新 `dashboard.html`:加一列;现 V0.3 测试 fixture 不破。

#### 7.2.2 project 详情页 `/project/<slug>`

**workflow / multi_workflow 项目(V0.3 行为)**:不变 — single session panel
+ events / outbox / screenshot 各自一份。

**flex 项目(V0.3.1 新)**:
- header:slug / team / kind=flex / cost / created_at;**无** current_phase
- session cards section:每张卡 = 一个 session(`<sid>`)
  - sid + harness badge(`claude` 蓝 / `codex` 绿)
  - status(silence_classifier 分类,基于 per-session progress.jsonl)
  - last event 时间
  - 缩略屏幕截图(F38 reuse,`/screenshot/<slug>-<sid>.png`)
  - "Attach" 按钮(实际是文档化 tmux command 复制按钮 — web 不能 `tmux
    attach`,只能展示命令)
  - "Detail" 链接 → `/session/<slug>/<sid>`
- master events stream:聚合所有 session 的 progress events(SSE `/sse/project/<slug>`,
  V0.3 已 ship — 自然消费 flex 项目的 sub-jsonl 文件,因为 watcher recursive
  监 `~/.ccteam/progress/`)

#### 7.2.3 新页 `/session/<slug>/<sid>`

完整 session detail:

- header:project slug + sid + harness + tmux session name + started_at
- events stream(per-session SSE `/sse/project/<slug>/<sid>`)
- per-session outbox(若 session 有自己 outbox 子目录;V0.3.1 默认共享项目
  级,detail 页展示项目级)
- screenshot:`<slug>-<sid>.png`(F38 `render_screenshot` 加 sid 参数)
- harness panel:F46 `harness_snapshot` SSE 消费,model / context% / cost /
  rate-limit
- write actions sidebar(V0.3 M5.3 形态):`/btw` / `inject_decision` /
  `pause` / `resume`,但**作用于 session 级 inbox**(`<slug>/.ccteam/sessions/<sid>/inbox/`)
  — V0.3.1 默认仍写到项目级 inbox(简化 V0.3.2 评估细分)

#### 7.2.4 SSE filter by sid

V0.3 ship 的 `GET /sse/project/<slug>` 推所有 progress events;V0.3.1 加:

| Path | 内容 |
|---|---|
| `GET /sse/project/<slug>/<sid>` | 单 session 过滤,server-side `msg.sid == <sid>`(EventMsg 加 `sid` 字段) |
| `GET /sse/harness/<slug>` | 该 slug 下所有 session 的 harness_snapshot(F46) |
| `GET /sse/harness/<slug>/<sid>` | 单 session harness_snapshot |

`progress.jsonl` 的 sid 来自 hooks 写入(从 cwd 推 `<slug>/<sid>` 子目录,
F49)。

#### 7.2.5 screenshot endpoint 扩展

V0.3:`GET /screenshot/<slug>.png` → `render_screenshot(slug, opts)`

V0.3.1:`GET /screenshot/<slug>-<sid>.png` → `render_screenshot(slug, sid, opts)`,
F38 `render_screenshot` 签名加可选 sid;old endpoint 保留(workflow 项目用)。

flex 项目 dashboard 卡片用 `<slug>-<sid>.png`(每张 card 自己的 screenshot);
workflow 项目仍 `<slug>.png`。

### 7.3 不做(V0.3.1 内,V0.4 deferred)

- **mobile-responsive layout for session cards** — V0.4
- **session 拖拽 reorder / 标记 favorite** — V0.4
- **跨 session 命令面板**(eg "在所有 sessions 同时 /btw <prompt>")— V0.4
  pipeline 评估
- **session 级 chart / 累计 token use 时序图** — V0.4
- **search across sessions / 按 harness 过滤 dashboard** — V0.4
- **flex workflow promotion UI**(选 N events → "promote to phase")— V0.4
  Fat Skills evolution

### 7.4 验收

- [ ] dashboard `/` 加 `Kind` 列;现有 V0.3 e2e 测试不破(fixture project
  默认 kind=workflow,新列 cell="workflow")
- [ ] flex 项目的 dashboard `Phase` 列渲染 `—`
- [ ] flex 项目的 `/project/<slug>` 渲染 N session cards + harness badges +
  per-card screenshot
- [ ] workflow / multi_workflow 项目的 `/project/<slug>` 渲染**完全不变**
  (回归测试)
- [ ] 新页 `/session/<slug>/<sid>` 200,header / events / harness panel /
  write actions sidebar 完整
- [ ] `GET /sse/project/<slug>/<sid>` server-side filter by sid 正确
- [ ] `GET /sse/harness/<slug>` / `GET /sse/harness/<slug>/<sid>` SSE 推送
  harness_snapshot
- [ ] `GET /screenshot/<slug>-<sid>.png` 200 + PNG bytes;不存在 sid → 404
- [ ] `GET /screenshot/<slug>.png` workflow 项目仍正常(V0.3 兼容)
- [ ] askama 模板编译期类型检查不破
- [ ] interfaces.md §15(web routes,V0.3 已写)加 V0.3.1 新 endpoint 段
- [ ] `cargo test --workspace` 不退步;新增 ≥ 6 测试(reqwest hit + HTML
  断言)

---

## 8. F51 — Chore + ship gate

### 8.1 问题

V0.3.1 ship 前需要:e2e 跨 flex 多 session + 截图 + harness snapshot + 写
动作的端到端验证;workspace.version bump;CLAUDE.md baseline 回填;v0-2
README pointer 更新;`docs/v0-3-1/e2e-retro.md` 落档。

### 8.2 设计

#### 8.2.1 e2e 测试

新建 `crates/ccteam-web/tests/flex_e2e_test.rs`:

- 起 ccteam-web server(rand port,127.0.0.1:0)
- fixture 含 1 flex 项目 + 2 session(claude-1 + claude-2;codex-1 stub
  返 NotImplemented 也覆盖 error 路径)
- happy path:
  1. `GET /` 200 → 表格含 kind=flex 行
  2. `GET /project/<slug>` 200 → N session cards 渲染
  3. `GET /session/<slug>/claude-1` 200 → events / harness panel
  4. `GET /sse/project/<slug>/claude-1` 收 ≥ 1 progress event
  5. `GET /sse/harness/<slug>/claude-1` 收 ≥ 1 harness_snapshot
  6. `POST /api/<slug>/btw` 200 → inbox 文件落地
  7. `GET /screenshot/<slug>-claude-1.png` 200 + PNG magic bytes(若 CI 无
     tmux,504 + plain-text;两条路径都覆盖)
- 不依赖真 tmux session;mock harness snapshot file 写入,触发 watcher
- F47 codex error 路径:`ccteam session add <slug> --harness codex` 返 exit 1
  + stderr 含 "trait-stub in V0.3.1"

#### 8.2.2 retro 文档

`docs/v0-3-1/e2e-retro.md` —— 模仿 V0.3 / V0.2.2 retro 模板:

- 4-suite 跨 flex 多 session / harness adapter / web UI / codex stub
- F46-F50 撞坑回顾 + dust patches(若有)
- 跨浏览器(Chrome / Firefox / Safari macOS)spot-check session detail page
- workflow / multi_workflow 项目的回归验证(V0.3 行为不破)

#### 8.2.3 workspace.version bump

`Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"`。F51 PR commit
subject `v0.3.1: ...` 前缀。

#### 8.2.4 CLAUDE.md baseline 回填

§一表格更新:

| 项 | V0.3.1 后 |
|---|---|
| Workspace version | `0.3.1` |
| 测试 baseline | 实测后填(估 738 + ~40 = ~780) |
| 已 ship 里程碑 | 加 V0.3.1 行(F46-F51) |

§六 易踩坑加一条:V0.3 → V0.3.1 升级注 — flex 团队 / `kind` 字段 / per-session
state.json schema 兼容(V0.1/V0.2/V0.3 yaml 不动)。

#### 8.2.5 docs sweep

- `docs/v0-2/README.md`:V0.3 起始 pointer 更新为"已 ship V0.3 + V0.3.1 起始"
- `docs/dev-coupling-audit.md`:F46-F51 close 标记
- `docs/tech-design.md` §3.3 / §6.3 / §6.11 加 flex / HarnessAdapter / 多
  session 段
- `docs/interfaces.md` §1.3 / §2.1 / §5.5 / §15 增量补 V0.3.1 schema

### 8.3 不做(V0.3.1 内,V0.4 deferred)

- 性能 benchmark(>10 session simul) — V0.4
- 跨平台 binary release — 现状 `cargo install` 已 ship
- i18n — 永久 deferred

### 8.4 验收

- [ ] `crates/ccteam-web/tests/flex_e2e_test.rs` 端到端 happy path + codex
  error path 通过
- [ ] `Cargo.toml::workspace.package.version = "0.3.1"`
- [ ] CLAUDE.md §一 baseline 表格更新(workspace.version + 实测测试数 +
  V0.3.1 milestone 行)
- [ ] CLAUDE.md §六 加 V0.3 → V0.3.1 升级注
- [ ] `docs/v0-3-1/e2e-retro.md` 落档
- [ ] `docs/v0-2/README.md` V0.3 pointer 更新
- [ ] `docs/dev-coupling-audit.md` F46-F51 close 标记
- [ ] `docs/tech-design.md` / `docs/interfaces.md` 增量段补完
- [ ] `cargo test --workspace` 全绿,baseline ≥ 738 + V0.3.1 累计新增
- [ ] clippy 不新增 warning(4 pre-existing 不算)

---

## 9. 已知风险 / 威胁模型

### 9.1 statusline wrapper 安装破坏用户原脚本

`ccteam doctor --install-statusline-adapter` 写
`~/.claude/statusline-command.sh`,wrapper marker 保护用户手改段;但用户
可能用 systemd / launchd / 其他自定义 source 路径替代该文件。

**缓解**:
- doctor `--dry-run` 模式输出 wrapper 内容供用户 review
- 检测到现有非 marker 内容 → 备份原文件到 `~/.claude/statusline-command.sh.bak-<ts>`
  然后写入 wrapper(保留逃生绳)
- doctor 输出明确路径 + revert 命令文档

### 9.2 flex 团队的 LAN-wide RCE 暴露面跟 V0.3 一致

V0.3.1 不引入新 RCE 面 — 写动作仍走 V0.3 M5.3 ship 的 token auth
middleware;flex 项目的 `/api/<slug>/btw` / `inject_decision` / `pause` /
`resume` 复用同一 auth 路径。session 级 endpoint(`/api/<slug>/<sid>/btw`,
若加 — V0.3.1 当前 default 走项目级)同一 token 校验。

V0.3 PRD §9 的 LAN RCE 评估完全继承,不需新 ack。

### 9.3 codex stub 错误消息可能被错误地解释为 fully implemented

NotImplemented error 文本明确指向 PRD §F47 + research doc;但用户 / sub
用户可能被 CLI flag 接受迷惑(以为 codex 至少能起来)。

**缓解**:
- `ccteam doctor` 输出 codex 状态 informational(present / not-found /
  not-supported V0.3.1)
- README.md / `docs/v0-3-1/README.md` § 关键设计决策 显式声明 codex 是
  V0.3.1 stub
- F47 验收 unit test 断言 error 文本含 "V0.3.1" 与 "deferred"

### 9.4 多 session race condition

并发 `ccteam session add <slug>` 两进程同时跑可能撞 sid 冲突(都拿 next_sid_seq
= n)。

**缓解**:
- master state.json 写 atomic(`.tmp` + rename)+ pre-write read 后**再读一次**
  最新 state(double-check + retry-on-conflict),retry 上限 3 次
- 单元测试覆盖并发 add(spawn 2 并发 thread,断 sid 不冲突)
- F49 ship 时**不**支持跨主机并发(单机内 race;跨主机 flex team 是 V0.4
  评估,几乎用不到)

### 9.5 flex 团队 workflow promotion 路径未设计可能后悔

V0.3.1 ship 空 flex base,V0.3.2/V0.4 落 promotion;若 promotion 设计需要
V0.3.1 已 ship 的字段不在,会要求 schema 演进。

**缓解**:
- `team.yaml::kind` 是 enum,promotion 后增加 `kind: hybrid` 等扩展不破现
  yaml
- `state.json::sessions{}` 是 BTreeMap,promotion 时加 `state.json::frozen_phases`
  等并行字段不破现 schema
- promotion 时若需要 source-of-truth 是 progress.jsonl 累积 events,这俩
  本就完整存档;无 retroactive 数据丢失风险

---

## 10. 不在范围 / V0.4 deferred

显式列出,future 贡献者不要 silently 拉回前移:

### 10.1 phase 编排演进(全暂停)

- **新 phase 定义 / 协议升级** — V0.3.1 不改 phase 任何机制
- **Fat Skills 第一等对象升级**(`thin-harness-fat-skills-architecture-improvement.md`
  §6.1)— V0.4
- **CEO Plan / 10x Gate phase / Plan-Eng-Review 强化**(同上 doc §6.2-6.3)
  — V0.4
- **Context Pack / Tokenmaxxing 工程化**(同上 doc §6.4)— V0.4
- **Review/QA Gate 矩阵**(critic_dimensions 数据驱动)— V0.4(M5)
- **Builder Throughput Metrics**(同上 doc §6.8)— V0.4

### 10.2 flex 团队 evolution 路径

- **Workflow promotion / demotion UX**:flex 项目跑出 recurring pattern 后
  promote N events 为冻结 phase;反向拆 phase 回 flex。涉及 `flex_workflows.yaml`
  per-team 持久化 schema,V0.3.2 / V0.4 评估
- **flex_workflows.yaml schema** — V0.4
- **flex 团队 retro_schema / verdict_schema 启用** — V0.3.2 评估
- **`max_concurrent_sessions_per_project` / 性能阈值** — V0.4
- **session rename** — V0.4
- **session 间共享 lessons / context** — V0.4

### 10.3 Codex 完整支持

- **`CodexAdapter::spawn_session` 实际 impl**(`codex` CLI 调用 / hook 写入)
  — V0.3.2(`docs/research/ccteam-codex-integration.md` M1)
- **codex statusline-equivalent ingestion** — V0.3.2
- **`mcp__codex__codex` 注册给 meta-agent** — V0.3.2 / V0.4(同 doc §6.4)
- **CodexExecRunner sub-skill runner** — V0.4(同 doc M2)
- **Review/QA gate blocking** — V0.4(同 doc M3)
- **Skill 双栈分发(`.agents/skills/`)+ AGENTS.md 模板** — V0.4(同 doc M4)
- **Codex worker phase 实验性 team** — 永久评估(同 doc M5)

### 10.4 自动化 pipeline

- **CC implement → codex review 自动派单**(implement phase_done 自动起
  codex sidecar)— V0.4 channel adapter 评估
- **session 级 outbox / inbox 细分**(per-session 不共享项目级)— V0.3.2
- **跨 session orchestration / coordinator** — V0.4

### 10.5 Web UI 增强

- **mobile-responsive layout** — V0.4
- **flex workflow promotion UI**("promote N events to phase")— V0.4
- **Per-session metric chart / token use 时序图** — V0.4
- **跨 session search / filter** — V0.4
- **session 拖拽 reorder / favorite mark** — V0.4

### 10.6 永久 deferred

- **Multi-user / team 共享 ccteam 实例** — ccteam 单用户工具
- **远程协作 / cloud session** — 同上
- **i18n** — 中英混用文档标准
- **statusline 数据进入 orchestrator 决策** — 破 SoT 红线

---

## 11. PR sequencing

6 finding 落 6 个 PR,推荐顺序:

| PR # | finding | branch | touchpoints | 依赖 |
|---|---|---|---|---|
| **PR #1** | **F46** HarnessAdapter trait + ClaudeCodeAdapter | `v0-3-1-harness-adapter` | 新 `crates/ccteam-core/src/harness.rs` / `ccteam doctor --install-statusline-adapter` / `~/.ccteam/harness/` 协议 / web SSE harness endpoint | 无 |
| **PR #2** | **F47** CodexAdapter stub + harness 字段 | `v0-3-1-codex-stub` | `harness.rs` 加 CodexAdapter / `team.yaml::sessions[]` schema / `ccteam session add --harness` flag(F49 PR 完成后真正调用)/ doctor codex 检测 | PR #1(trait) |
| **PR #3** | **F48** kind: flex team kind | `v0-3-1-flex-kind` | `team.rs::TeamSpec` kind/sessions 字段 / orchestrator behavior gating helpers / team factory `--kind=flex` / claude_md_template seed | 无(不强依赖 PR #1/#2 — 但**推荐**在 #2 后做以让 sessions[] 字段定下) |
| **PR #4** | **F49** Adhoc multi-session primitives | `v0-3-1-multi-session` | `ccteam session {add,ls,attach,rm}` CLI / per-session subdir / state.json sessions+next_sid_seq / progress.jsonl scoping / tmux 命名 | PR #1(spawn_session 调用)+ PR #3(kind:flex 项目才能 add) |
| **PR #5** | **F50** Web 层更新 | `v0-3-1-web-flex` | dashboard kind 列 / project per-session cards / new `/session/<slug>/<sid>` 详情 / SSE filter by sid / screenshot 扩展 | PR #1(harness SSE)+ PR #4(per-session events / sid) |
| **PR #6** | **F51** chore + ship gate | `v0-3-1-ship-gate` | flex_e2e_test.rs / workspace.version bump 0.3.0 → 0.3.1 / CLAUDE.md baseline / docs/v0-3-1/e2e-retro.md / docs sweep | PR #1-#5 全部 merge |

依赖图:

```
PR #1 (F46 HarnessAdapter)  ── trait + ClaudeCodeAdapter,foundation
   ↓
PR #2 (F47 CodexAdapter stub)            (并行起点;只依 trait;CLI 真调用要 #4)
   ↓
PR #3 (F48 kind: flex)                   (不强依 #1/#2,但推荐 #2 后 sessions[] schema 定下)
   ↓
PR #4 (F49 multi-session) ───┐
   ↓                          │
PR #5 (F50 web flex) ────────┴──► PR #6 (F51 ship gate)
```

并行机会:
- **PR #2 vs #3**:trait stub vs team kind,touch 不同模块,可同时 worktree
- **PR #5 frontend**:跟 #4 backend 同时 worktree(冲突点只在 askama
  template + web SSE 路由)

**总计**:~1.5 kLOC + ~40 测试,~12-15 天(单人 5 天/周即 2.5-3 周;
PR #2/#3 + PR #4/#5 并行可压到 ~10 天)。

worktree 用法(详 dev-plan §8 briefing 模板):

```
git worktree add -b v0-3-1-<topic> /tmp/ccteam-v031-<topic> origin/main
```

跟 V0.2.2 / V0.3 一致;subagent briefing 含 PRD 章节 + 验收条目 + 红线
grep 矩阵。

---

## 12. Workspace version bump

`Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"` 在 F51 PR 落地。

V0.2.2 起立的政策:每 minor / patch release 必须 bump + commit subject
`vX.Y.Z:` 前缀。

V0.3.1 是 patch round 而非主版本(V0.4 才是下个主版本),version 跳 patch
位(0.3.0 → 0.3.1)而非 minor 位。

---

## 13. CLAUDE.md baseline 更新 plan(F51 落)

§一 表格更新:

```markdown
| Workspace version | **`0.3.1`**(V0.3.1 patch round 起跟版同步) |
| 测试 baseline | **<实测>全绿** |
| 已 ship 里程碑 | ... + **V0.3.1**:F46-F51(6 finding 跨 6 PR:HarnessAdapter trait +
  CodexAdapter stub + flex team kind + adhoc multi-session + web flex 适配 +
  ship gate)— 详 `docs/v0-3-1/README.md` |
| 当前 next | V0.4 候选方向 — phase 编排演进恢复 + Codex 完整 + flex workflow
  promotion + Fat Skills 第一等;deferred 项见 `docs/v0-3-1/prd.md §10` |
```

§六 易踩坑加:

```
- **V0.3 → V0.3.1 升级一次性迁移**(F46-F49):`team.yaml::kind` 字段 default
  `workflow` 保 V0.1/V0.2/V0.3 yaml 解析不变;flex 团队 state.json `sessions{}`
  + `next_sid_seq{}` 字段 serde default 不破老 state;`~/.ccteam/progress/`
  flex 项目走 `<slug>/<sid>.jsonl` 子目录,workflow 项目仍 flat `<slug>.jsonl`
  — hooks 解析 `team_kind` 自动分流;`ccteam doctor --install-statusline-adapter`
  写 wrapper marker section 保护用户手改段
```

§三 红线 V0.3.1 不动:`progress.jsonl` SoT / 永不主动 kill / ccteam-core
无 team 名字面量 — 三条 V0.3.1 反而**强化**(flex 团队靠 `team.yaml::kind`
数据驱动,orchestrator 不写 `if team_name == "flex"`)。

---

## Changelog

- 2026-05-10:**初稿**。基于 V0.3 ship 后用户在 dispatcher session 的
  Telegram dialogue(messages 311-318)确认的战略 pivot;6 finding(F46-F51)
  分布参考 V0.2.2 patch 模式;架构决策(`kind: flex` 与 `parallelism` 正交 /
  adhoc session 不复用 multi_session 拓扑 / per-session subdir layout 在
  `<project>/.ccteam/sessions/<sid>/` / progress.jsonl flex 项目走 `<slug>/<sid>.jsonl`
  子目录 / sid 格式 `<harness>-<n>` 单调递增不复用 / tmux `ccteam-<slug>-<sid>`
  命名)全部从 advisor 反馈 + ccteam-core 现状 audit 综合定;Codex stub 错误
  消息文本指向本 PRD + research doc。base = `origin/main` `f9baf3f`(V0.3
  ship 终点;workspace.version `0.3.0`,测试 baseline 738/0)。
- 2026-05-10:research scratch 文件 `docs/research/v0-3-1-harness-adapter-plan.md`
  里 5 个 open question 在本 PRD 定稿:
  - Q1(install 入口):`ccteam doctor --install-statusline-adapter` 显式
    flag,doctor 默认调用时也跑(同 V0.2.2 F39/F44 doctor 模式)
  - Q2(用户原脚本策略):tee + 透传(保留用户自定义 footer);marker section
    保护用户手改;原脚本 backup 逃生绳
  - Q3(harness JSON lifecycle):**覆盖**(每条 stdin JSON 完整覆盖,简化;
    delta 历史是 V0.4 deferred)
  - Q4(meta-agent harness panel):是 — meta-agent session 也产 harness
    snapshot(`~/.ccteam/harness/_meta-<handle>.json`)
  - Q5(CodexAdapter V0.3.1 vs V0.3.2):V0.3.1 落 stub,V0.3.2 落 impl;
    F47 验收明确 stub error 消息文本指向 V0.3.2
