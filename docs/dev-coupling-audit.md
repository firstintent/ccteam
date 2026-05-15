# dev-team 耦合点审计

> 本文是 [ccteam-as-domain-agnostic-orchestrator.md](./ccteam-as-domain-agnostic-orchestrator.md)
> §B 步骤的产出。审计当前代码,把"假设了 dev 团队"的位置逐条钉死,给
> M3(team abstraction 里程碑;2026-05-05 reorder 前曾标 M4.5)提供修复路线。
>
> **审计日期**:2026-05-05
> **审计基线**:strategic doc §1 责任分界表(domain-agnostic vs team fill 的判定)
> **审计范围**:`crates/ccteam-core/src/`(9 文件)+ `crates/ccteam-cli/src/`
> (2 文件)+ `crates/ccteam-hooks/src/`(5 文件)+
> `crates/ccteam-core/src/templates/settings.json` + `phases/`(6 文件)+
> 顶层 `CLAUDE.md` 与 `docs/`
>
> 每条发现固定四要素:
> - **文件:行号**
> - **现状描述**
> - **是否真 dev-specific**(论证,不是一刀切)
> - **解耦方案**(改名 / 提 trait / 加配置 / 不必改)
> - **优先级**(P0 阻塞泛化 / P1 该做但可后置 / P2 边角 / N/A 已是领域无关)

---

## 摘要

25 条发现(2026-05-05 加 F21、升级 F20 P1→P0,共增 1 条;2026-05-06 修复 F21;
2026-05-06 M4.4 spike 加 F22 P0 + F23 P1 conditional;2026-05-06 修复 F22;
**2026-05-06 post-M3/M4 sweep**:F2/F3/F4/F9/F10/F11/F12/F13/F20 由 M3 团队
抽象 + M4 跨项目记忆批量关闭;**2026-05-07 fix_loop → auto_loop rename batch**:
F1/F5/F6/F7/F8/F18 由独立 PR 一波关闭;**2026-05-08 V0.2 M0.23**:加 F24 + F25
P0 + 同 PR 关闭;**2026-05-08 V0.2 e2e retro**:加 F26-F33 八条 V0.2.1 候选;
**2026-05-08 V0.2.1 patch**:F26-F33 全部修复;
**2026-05-09 V0.2.2 patch**:加 F34-F40 七条用户反馈 + 命名 sweep + UX 增强,跨 7 PR 全部修复;
**2026-05-09 V0.2.2 e2e retro patch**:4-suite 并行 e2e 验证,撞 F41 (P1) + F42 (P1) + F43 (P2),同 PR 一波修;
**2026-05-10 V0.2.2 F44 反向回滚**:`/usr/bin/cct` namespace 碰撞驱动整体反向 F39,F44 单 PR 覆盖;
**2026-05-10 V0.3 doc-only kickoff**:加 F45 P1(write helper promote ccteam-cli → ccteam-core::actions,M5.0 关键解耦),实施在 V0.3 PR #1 / #4);**2026-05-10 V0.3 PR #1 ship**:F45 promote 部分修复(actions 模块 + mcp_serve wrapper 透传 + dep_graph 自检测试落地),仍待 M5.3 写动作 endpoint 消费才整体 close;**2026-05-10 V0.3 PR #4 ship**:F45 **整体 close**(M5.3 写动作 endpoint + token auth + URL-shim cookie + path-traversal 守卫全部 ship);**2026-05-10 V0.3.1 doc-only kickoff**:加 F46-F51 六条(战略 pivot:flex team kind + adhoc multi-session + HarnessAdapter trait + CodexAdapter stub + web flex 适配 + ship gate);**2026-05-10 V0.3.1 ship**:F46-F51 全部 close,workspace.version 0.3.1,833/0 测试;分布:

| 优先级 | 数量 | 编号 |
|---|---|---|
| **P0 阻塞泛化(剩余)** | 0 | — |
| **P1 该做但可后置(剩余)** | 2 | F15(M1+ block-push 时做)、F23(conditional;待 spike 重跑) |
| **P2 边角(剩余)** | 1 | F17 |
| **V0.3.1 待 ship** | 0 | — |
| **N/A 已是领域无关** | 2 | F14, F19(M3 docs sweep 后)|
| **已修复** | 44 | F1 / F5 / F6 / F7 / F18(2026-05-07 rename PR;F1 触发逻辑实际早 M3.1 已切到 template.auto_loop,本 PR 完成命名层 sweep)、F2 / F3 / F4(M3.1 dag.rs)、F8(2026-05-07 directory scan)、F9 / F10 / F11(M3.4 team-aware bootstrap;F11 dev 仍裸 `phases/` 但非阻塞)、F12 / F13(M3.3 `--team` CLI + `state.team`)、F16(M3.4 phase 模板 team 化)、F20(M3.1+M3.4 retro_schema 数据形式 + product-research 填字段 + M4.1 phase 消费)、F21(@a5fb21d)、F22(PR #12)、**F24 / F25(2026-05-08 M0.23 PR)**、**F26 / F27 / F28 / F29 / F30 / F31 / F32 / F33(2026-05-08 V0.2.1 patch)**、**F34 / F35 / F36 / F37 / F38 / F39 / F40(2026-05-09 V0.2.2 patch — 7 finding 跨 7 PR)**、**F41 / F42 / F43(2026-05-09 V0.2.2 e2e retro patch)**、**F46 / F47 / F48 / F49 / F50 / F51(2026-05-10 V0.3.1 patch)** |

### V0.2 §6 反模式候选状态(docs/v0-2/prd.md)

PRD V0.2 §6 列了 8 条 ccteam-core 反模式候选清理任务,跟 F-finding
独立编号但同源(都是"领域字面量泄漏到 core"):

| 候选 | 描述 | 状态 |
|---|---|---|
| 1 + 8 | 协议关键字 `PHASE_DONE` / `ESCALATE` 三处镜像 → 单一 source | **2026-05-08 关闭(M0.18):inject prompt template + frontmatter `completion_signal` / `escalate_grammar_ref` 单一 source;phase markdown 正文清理协议关键词;`build_phase_prompt_for_template` 是唯一 protocol literal 拼装位置;详 `docs/v0-2/phase-prompt-architecture.md`** |
| 2 | `render_project_claude_md` `match team` 写死 | **2026-05-07 关闭(M0.16.3)** |
| 3 | `TEAM_BUNDLES` 编译时常量 → seed-only | **2026-05-07 关闭(M0.16.2)** |
| 5 | meta-agent `if team == META_TEAM_NAME` 5 处分叉 | **2026-05-07 关闭(M0.16.1)** |
| 7 | `RECOMMENDED_AGENTS` ln -sf 8 plugin agent | **2026-05-08 关闭(M0.20)** — 改 in-memory plugin pipeline,`bootstrap_project` 写 `enabledPlugins` 到 spawned session settings.json;`ccteam doctor --migrate-recommended-agents` 清理 V0.1 残留 ln -sf |
| 4 | `golden_rules` layered merge | V0.3 deferred |
| 6 | `pre_trust_project` 写 `~/.claude.json` | V0.3 deferred |

**剩余 P0 关键路径**:**只剩 F1**(`auto_loop` 字段已在 phase YAML 里加了
[M3.1],orchestrator 仍按 `FIX_PHASE_NAME` 字符串触发 `FixLoopState`——需
切到读 `template.auto_loop`)。完成后 ccteam-core 可彻底放弃 "fix" 这个名字。

**元发现(2026-05-05 写)**:`pub use ... M0_PHASE_DAG, FIRST_PHASE`(`crates/
ccteam-core/src/lib.rs:21`)把 dev 假设暴露到 lib 接口表面——**已在 M3.1 落地**
(dag.rs 替代 M0_PHASE_DAG / FIRST_PHASE,lib API breaking change 已发生)。

**对 §A 的反馈**:审计过程中没有发现需要修订 strategic doc §1 责任分界表
或 §2 团队扩展契约的位置——所有发现都能映射到现有分类。这是抽象切对的
好信号。M3 落地后的 post-sweep 同样没发现需要新分类。

---

## P0 — 阻塞泛化

### F1 — `FIX_PHASE_NAME` / `FIX_LOOP_MAX_ITERATIONS` 字符串耦合 fix-loop 触发(**已修复:M3.1 加 phase YAML 字段;触发逻辑早于 2026-05-06 已切到 `template.auto_loop`;2026-05-07 rename PR 完成命名层 sweep**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs:32-33` + `:481-491`
  ```rust
  const FIX_PHASE_NAME: &str = "fix";
  const FIX_LOOP_MAX_ITERATIONS: u32 = 3;
  // ...
  let target_state = if phase == FIX_PHASE_NAME {
      let fl = FixLoopState::new(slug.to_string(), prompt, FIX_LOOP_MAX_ITERATIONS);
      fix_loop::write(&fix_loop::path_in(&project_dir), &fl)?;
      PhaseState::FixLocked
  } else { PhaseState::InFlight };
  ```
- **现状**:phase 名为字符串 `"fix"` 时才进入 ralph 自循环;其它 phase 一律
  普通 InFlight。`max_iterations = 3` 写死。
- **是否真 dev-specific**:**是。** "fix"是 dev 流程语义;mechanism 应为
  "phase 模板 `auto_loop: true` 时进入自循环",和该 phase 叫什么名无关。
  research 团队的 `03-primary` 数据收集 phase / `04-synthesis` insight 提取
  phase 都需要相同的"反复重喂直到达到 completion_signal"机制。
- **解耦方案**:
  1. phase YAML schema 加 `auto_loop: bool`(默认 false)、`auto_loop_max_iterations:
     u32`(默认 3)、`completion_signal: String`(必填若 auto_loop=true)三个字段;
  2. orchestrator 不再按字符串 `"fix"` 判定,改为读取 `PhaseTemplate.auto_loop`;
  3. `FixLoopState::new` 的 `completion_signal` 参数从 phase 模板传入,删除
     `"TESTS_GREEN"` 默认值。
- **优先级**:**P0**——`auto_loop` 字段是 strategic doc §1.3 / §2.1 团队扩展
  契约的核心,不解耦无法跑非 dev 团队。
- **2026-05-06 部分修复**:phase YAML schema 已加 `auto_loop` /
  `auto_loop_max_iterations` / `completion_signal` 三字段(`crates/ccteam-core/src/phases.rs`
  M3.1),`teams/dev.yaml` + `phases/06-fix.md` 已声明 `auto_loop: true`;但
  `orchestrator.rs` 的触发分支仍是 `if phase == FIX_PHASE_NAME`,**未切到读
  `template.auto_loop`**——orchestrator 把 phase 名为 "fix" 的 phase 视作自循环,
  其它 team 想标 `auto_loop: true` 仍不生效。剩余工作:把 `if phase == FIX_PHASE_NAME`
  改为 `if dag.lookup(phase).map_or(false, |t| t.auto_loop)`,FixLoopState 的
  `completion_signal` 与 `max_iterations` 改从 PhaseTemplate 读。

### F2 — `M0_PHASE_DAG` 硬编码 dev 流程(**已修复:M3.1 dag.rs**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs:37-44` + 通过
  `pub use ... M0_PHASE_DAG` 暴露到 lib.rs:21
- **现状**:`pub const M0_PHASE_DAG: &[(&str, Option<&str>)] = &[ ("plan-eng",
  Some("implement")), ... ("ship", None) ];`
- **是否真 dev-specific**:**是。** 整个表是 dev 团队具体 6 phase。mechanism 是
  "邻接表 / 拓扑序",dev 表是该 mechanism 的一个实例。
- **解耦方案**:
  1. 删除 `M0_PHASE_DAG` 常量;
  2. `Orchestrator::new` 从加载的 `PhaseTemplate[]` 推断 DAG——按文件名前缀
     `NN-` 排序得 happy-path;非线性分叉由新增 front matter 字段 `next_on_done` /
     `next_on_escalate` 显式声明(M3.2);
  3. `next_phase()` 函数改成 `&self.dag` 上的查表方法。
- **优先级**:**P0**——和 F1 一并解耦,是 `--team` 参数能工作的前提。
- **2026-05-06 已修复**:M3.1 落 `crates/ccteam-core/src/dag.rs`(`PhaseDag::infer_from_templates`),
  从加载的 `PhaseTemplate[]` 按文件名 `NN-` 前缀排序推断 happy-path DAG;`M0_PHASE_DAG`
  常量删除,lib.rs `pub use` 已清理。orchestrator 改用 `team.dag.next_phase()` 查表。

### F3 — `FIRST_PHASE = "plan-eng"` 硬编码(**已修复:M3.1 dag.rs**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs:46`
- **现状**:`pub const FIRST_PHASE: &str = "plan-eng";`,`decide_tick_from_events`
  在 `current_phase.is_empty()` 时回落到这个常量。
- **是否真 dev-specific**:**是。** research 团队第一个 phase 是 `00-topic` /
  `00-topic-clarify` 之类的 entry phase,与 plan-eng 无关。
- **解耦方案**:从 F2 推出的 DAG 取 `dag.entry_node()`(排序后第一个 phase 的
  name)作为 `current_phase` 兜底。
- **优先级**:**P0**——同 F2 一并清。
- **2026-05-06 已修复**:`FIRST_PHASE` 常量删除;orchestrator 在 `current_phase.is_empty()`
  时取 `team.dag.entry_phase()`(DAG 排序后第一个节点),由 team.yaml 注入 phase 集
  的入口节点决定。M3.4 的 product-research 团队入口 `01-kickoff` 验证可行。

### F4 — `is_terminal()` 字符串匹配 `"ship"` 终态(**已修复:M3.1 dag.rs**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs:59-64`
  ```rust
  pub fn is_terminal(state: &ProjectState) -> bool {
      state.phase_history.iter().any(|h|
          (h.phase == "ship" && h.status == "passed") || h.status == "escalated")
  }
  ```
- **现状**:终态 = "history 里出现过 ship-passed" 或 "任意 escalated"。
- **是否真 dev-specific**:**部分。** "escalated 即终态"是 mechanism;"`ship`
  passed 即终态"是 dev fill。research 团队的终点 phase 叫 `06-report` 或
  `06-retro`。
- **解耦方案**:改为 "history 末尾 phase ∈ DAG 的终点节点(no `next_phase`)
  且 status=passed"。从 F2 解耦后的 DAG 直接读得到。
- **优先级**:**P0**——和 F2 / F3 同批清。
- **2026-05-06 已修复**:`PhaseDag::is_terminal_phase` / `is_terminal_state`(`dag.rs:103-127`)
  按"DAG 终点节点(无 next_phase)且 status=passed"或"任意 escalated"判定,
  字符串字面量 `"ship"` 不再出现在 orchestrator 决策路径。dev 团队 `09-ship.md` /
  product-research `06-verdict.md` 都被识别为各自 DAG 的终点节点。

### F12 — `ccteam new` CLI 缺 `--team` 参数(**已修复:M3.3**)

- **文件:行号**:`crates/ccteam-cli/src/main.rs:54-60` + `commands.rs:97-104`
- **现状**:`Command::New { request, file }` 只接受 request 文本;`run_new`
  无 team 参数。
- **是否真 dev-specific**:**间接是。** 不加 `--team` 就只能跑 dev 团队。
- **解耦方案**:
  1. `Command::New` 加 `#[arg(long, default_value = "dev")] team: String`;
  2. `run_new` 签名加 `team: &str`;
  3. `bootstrap_project` 受 team 参数影响选 phase 模板与 CLAUDE.md(F9/F10);
  4. project state.json 写入 `team: <name>`(F13)。
- **优先级**:**P0**——M3.3 直接产物;F2/F10/F11 都需要这个入口才能生效。
- **2026-05-06 已修复**:M3.3 落 `Command::New { team: String, ... }`(`crates/ccteam-cli/src/main.rs:70`)
  + `run_new(paths, request, team)` 签名(`commands.rs:135`),`--team` 默认 `dev`;
  `ensure_team_resolvable` 先 lookup team_bundle / `~/.ccteam/teams/<name>.yaml`,未知
  团队 fail-fast。M3.5 e2e 验证 `ccteam new --team=product-research` 跑通 happy path。

### F13 — `state.json` 缺 `team` 字段(**已修复:M3.1**)

- **文件:行号**:`crates/ccteam-core/src/state.rs:54-77` + `interfaces.md §2.1`
- **现状**:`ProjectState` 没有 team 标识。orchestrator 启动期不知道某个项目
  属于哪个 team。
- **是否真 dev-specific**:**否——是缺 mechanism 字段。**
- **解耦方案**:
  1. `ProjectState` 加 `pub team: String`(serde default 给 `"dev"` 兼容旧文件);
  2. `Orchestrator::new` 改为按项目 `team` 字段 lazy-load 对应 team 的
     phase 模板(从 `~/.ccteam/teams/<name>/phases/`);
  3. interfaces.md §2.1 同步加字段。
- **优先级**:**P0**——orchestrator dispatch 时必须能区分 team 才能选对 DAG /
  完成信号 / artifact 列表。
- **2026-05-06 已修复**:M3.1 加 `pub team: String`(`state.rs:75`,默认 `"dev"` 兼容
  M0 旧文件)+ `ProjectState::initial_for_team(slug, team)` 构造器;`Orchestrator`
  按项目 `state.team` lazy-load 对应 `TeamSpec`(`team_bundle(team)` → `crates/ccteam-core/src/templates.rs:112`),
  从 team.yaml 的 `phase_dir` 找 phase 模板。`interfaces.md §2.1` 同步加字段。

---

## P1 — 该做但可后置

### F5 — `fix_loop` 模块 / 类型名假设了"fix"语义(**已修复:2026-05-07 rename PR**)

- **文件:行号**:`crates/ccteam-core/src/fix_loop.rs`(整个文件)+
  `lib.rs:6,17`(`pub mod fix_loop` / `pub use fix_loop::{FixLoopDecision,
  FixLoopFrontMatter, FixLoopState}`)+ `parse_phase_end.rs:24`(`use
  ccteam_core::fix_loop::{self, FixLoopDecision}`)
- **现状**:模块名 `fix_loop`,公共类型 `FixLoopState` / `FixLoopFrontMatter` /
  `FixLoopDecision` / `FixLoopState::new`,文件名 `fix-loop.state.md`。
- **是否真 dev-specific**:**命名上是,机制不是。** ralph 范式是"同一段
  prompt 重喂到信号出现",mechanism 不挑领域。但每出现一次 "fix" 名字,读
  代码的人就会假设"它只是 dev fix-cycle"。
- **解耦方案**:
  1. `mod fix_loop` → `mod auto_loop`;
  2. `FixLoopState` → `AutoLoopState` 等;
  3. 状态文件名 `fix-loop.state.md` → `auto-loop.state.md`;
  4. 已存在的 `fix-loop.state.md` 文件由 `ProjectState::load` 做一次性迁移
     (M3.1 实现迁移逻辑)。
- **优先级**:**P1**——纯重命名,机制不变,但延后会让 P0 的 F1/F2 修复后
  代码里出现 "auto_loop_phase 写 fix_loop.state.md" 的命名内外冲突。建议跟
  F1 同 PR 做。

### F6 — `PhaseState::FixLocked` 枚举值名假设 fix(**已修复:2026-05-07 rename PR**)

- **文件:行号**:`crates/ccteam-core/src/state.rs:30-32`
  ```rust
  pub enum PhaseState {
      InFlight, Idle,
      /// Stop hook owns the loop (ralph-loop fix-cycle pattern, §3.5).
      FixLocked,
  }
  ```
- **现状**:枚举值 `FixLocked` 只是状态机标记,但语义是"orchestrator 已交棒
  给 Stop hook 自循环"——和具体哪个 phase 无关。
- **是否真 dev-specific**:**命名上是,语义不是。**
- **解耦方案**:`FixLocked` → `AutoLocked`;serde rename `fix_locked` →
  `auto_locked`;state.json 加载时容错读取旧值(为已存在的 dev 项目)。
- **优先级**:**P1**——同 F5 一并改。

### F7 — `state.fix_cycle_count` 字段名假设 fix(**已修复:2026-05-07 rename PR**)

- **文件:行号**:`crates/ccteam-core/src/state.rs:65`
- **现状**:`pub fix_cycle_count: u32`
- **是否真 dev-specific**:**命名上是。** 同 F6,字段语义是"通用 auto-loop
  计数"。
- **解耦方案**:重命名 `auto_loop_cycle_count`(serde rename 兼容旧文件);或
  挪进 `phase_state` 内嵌(只在 AutoLocked 时有效)更优,但变更面更大。
- **优先级**:**P1**——同 F5/F6 一并改。

### F8 — `collect_artifacts` 硬编码 dev artifact 列表(**已修复:2026-05-07 directory scan PR**)

- **文件:行号**:`crates/ccteam-cli/src/commands.rs:305-329`
  ```rust
  for known in [
      ("spec", "spec.md"),
      ("plan_eng", "plan-eng.md"),
      ("plan_ceo", "plan-ceo.md"),
      ("architecture", "architecture.md"),
      ("implement_report", "implement-report.md"),
      ("test_report", "test-report.md"),
      ("fix_report", "fix-report.md"),
      ("review_report", "review-report.md"),
      ("retro", "retro.md"),
      ("escalation", "escalation.md"),
  ] { ... }
  ```
- **现状**:`ccteam show <slug> --format json` 报告项目 artifacts 时只看这 10
  个文件名是否存在。
- **是否真 dev-specific**:**部分。** `escalation.md` 是通用的(所有团队共用);
  其余 9 个是 dev fill。
- **解耦方案**:两路:
  - **快**:从 team.yaml 的 `artifacts:` 字段(strategic doc §2.8)读清单;
    `escalation` 永远在;
  - **更通用**:扫 `<project>/.ccteam/*.md` 自动列出,key 用文件名 stem 去掉
    后缀。简单且不需要 team 配置。
- **优先级**:**P1**——`ls` / `show` JSON schema 是 LLM 消费的(tech-design
  §3.8 用户自带 claude),错列 artifact 不阻塞跑通,但在 research 团队上线前
  必须修(否则 research 项目的 `topic.md` / `primary/*.md` 不被 `show` 报告)。
- **2026-05-06 状态**:**仍未修复**——`commands.rs:993-1017` 的 `collect_artifacts`
  仍硬编码同样 10 项 dev artifacts。M3 product-research 落地后 `show <slug> --format
  json` 在 product-research 项目上**只会报告 `escalation.md` 一项存在**,kickoff /
  market-survey / verdict 等 markdown 都被遗漏。建议优先修:扫 `<project>/.ccteam/*.md`
  自动列出,key 用文件名 stem,无须 team 配置。

### F9 — `bootstrap_project` 写死的 CLAUDE.md 内容含 dev 措辞(**已修复:M3.4**)

- **文件:行号**:`crates/ccteam-core/src/projects.rs:120-125`
  ```rust
  let body = format!(
      "# CLAUDE.md (auto-managed by ccteam)\n\n## 项目上下文\n- slug: {slug}\n
      - 用户原始需求: 见 .ccteam/spec.md\n\n## 工作约定\n- 不要交互式询问。
      所有决策已在 .ccteam/plan-eng.md 中。\n- 测试不过不算完成。\n\n
      ## 不做的事\n- 不要 git push(被 hook 拦截)\n- 不要修改 .ccteam/ 之外
      的元数据\n",
  );
  ```
- **现状**:每个新项目的 CLAUDE.md 都包含 "测试不过不算完成 / 不要 git push /
  决策在 plan-eng.md"。
- **是否真 dev-specific**:**是。** research 团队不跑测试,不该提"测试";没
  有 git push 概念但有"未审就发邮件";`plan-eng.md` 可能叫 `plan.md` 或
  `topic.md`。
- **解耦方案**:
  1. 把模板挪到 `crates/ccteam-core/src/templates/claude_md/<team>.md`(类似
     settings.json 的处理);
  2. team.yaml 加 `claude_md_template: claude_md/research.md` 字段;
  3. bootstrap 时读对应 team 的模板,替换 `{slug}` 占位符。
- **优先级**:**P1**——bootstrap 的内容污染 phase prompt 上下文,research 项目
  上线前必须修。
- **2026-05-06 已修复**:M3.4 `render_project_claude_md(slug, team)`
  (`crates/ccteam-core/src/projects.rs:186`)按 team 分支:dev 保留历史 "no git push /
  tests must pass" 措辞;`product-research` 改写为 "不写代码,只产研究报告 /
  3 独立信息源 / 不要把 dev 测试假设套到本项目";unknown team 落到通用 shell。

### F10 — `PHASE_TEMPLATES` 编译期 include dev phase MD(**已修复:M3.4**)

- **文件:行号**:`crates/ccteam-core/src/templates.rs:24-31`
  ```rust
  pub const PHASE_TEMPLATES: &[(&str, &str)] = &[
      ("02-plan-eng.md", include_str!("../../../phases/02-plan-eng.md")),
      ("03-implement.md", include_str!("../../../phases/03-implement.md")),
      // ... 6 entries total
  ];
  ```
- **现状**:6 个 dev phase 模板编译进 binary。`ccteam init` / `ccteam new` 写
  入全局 `~/.ccteam/phases/` 与项目 `.ccteam/phases/`。
- **是否真 dev-specific**:**是,但部分合理。** 让 dev 团队 zero-config 跑得
  起来需要某个团队是 binary 内嵌的默认;但当前实现没有"切换到 research 团队"
  的入口。
- **解耦方案**:
  1. 把 `PHASE_TEMPLATES` 改名 `DEV_PHASE_TEMPLATES`;
  2. 加 `RESEARCH_PHASE_TEMPLATES` 等同款常量,从 `phases-research/` include;
  3. `bootstrap_project` 接收 `team: &TeamSpec` 参数,选对应数组写入;
  4. 将来插件式装新 team 时,不进 binary 而是从 `~/.ccteam/teams/<name>/phases/`
     读盘——核心 binary 永远内嵌至少一个团队(dev)作 zero-config 兜底。
- **优先级**:**P1**——M3.4 直接产物。F10 + F2 共同决定 `--team` 参数能怎么
  实现。
- **2026-05-06 已修复**:M3.4 落 `TeamTemplateBundle` 多 team registry
  (`crates/ccteam-core/src/templates.rs:96-104`)—— dev 团队 6 phase 与 product-research
  6 phase 各自 `include_str!` 入 binary,通过 `team_bundle(team)` 选用;`team.yaml`
  的 `phase_dir` 字段决定写到哪个目录。"切换到 research 团队"的入口已存在
  (`ccteam new --team=product-research`)。

### F11 — phase 目录与文件命名缺 team scope(**部分修复:M3.4 product-research 团队 / dev 仍用裸 `phases/`**)

- **文件:行号**:`/home/rob/workplace/agents/ccteam/phases/`(整个目录)+
  `crates/ccteam-core/src/templates.rs:25-30` + `interfaces.md §1.1` 的
  `~/.ccteam/phases/`
- **现状**:仓库根 `phases/`、binary 内嵌路径、用户家目录 `~/.ccteam/phases/`、
  项目内 `.ccteam/phases/`——四处全部用裸 `phases/` 目录,没团队 scope。
- **是否真 dev-specific**:**目录布局上是。** 多团队后必须 `phases-<team>/`
  隔离。
- **解耦方案**:
  1. 仓库根 `phases/` → `phases-dev/`(M3.1 一次性 git mv);
  2. binary 内嵌 `phases-<team>/<NN>-<phase>.md`;
  3. 用户家目录 `~/.ccteam/phases/<team>/...` 或 `~/.ccteam/teams/<team>/phases/`
     (推荐后者,把 team config 与 phase 模板放一起);
  4. 项目内 `.ccteam/phases/` 仍按 phase 名扁平存放(项目本身只跑一个 team,
     phase 不混);项目 state.json 增加 `team: <name>` 字段(同 F13)。
- **优先级**:**P1**——必做但可在 F2 解耦后批量做。
- **2026-05-06 部分修复**:product-research 团队已用 `phases-product-research/` scope;
  `team.yaml.phase_dir` 字段(M3.1)驱动写入位置;binary 内嵌 `<phase_dir>/<NN>-<phase>.md`
  路径;orchestrator `~/.ccteam/<team.phase_dir>/` lazy-load。**未做**:dev 团队仍用
  裸 `phases/` 目录(向后兼容期)而非 `phases-dev/`,也无 `~/.ccteam/teams/<team>/`
  的合一目录布局——这两条是命名层 nice-to-have,不阻塞功能。

### F15 — settings.json 模板未含危险命令拦截(M1+ 风险)

- **文件:行号**:`crates/ccteam-core/src/templates/settings.json`(整个文件)
  + `interfaces.md §6.1` 描述 M0 模板不含 `Bash:git push.*` 拦截
- **现状**:M0 模板没有 dev 特定的危险命令拦截 matcher——M1+ 的 `block-push`
  hook 还没实现。
- **是否真 dev-specific**:**M1+ 实现时会变成 dev-specific。** 当前 M0 版本是
  纯 mechanism。
- **解耦方案**:M1+ 实现 `block-push` 时:
  1. team.yaml 加 `danger_command_patterns: [{ pattern, reason }]`;
  2. `render_project_settings` 受 team 参数,按 patterns 注入 `PostToolUse`
     的 matcher entry;
  3. 不直接写 `Bash:git push.*` 字面量到模板里。
- **优先级**:**P1**——M1+ 引入 `block-push` 时一并做;不在 M3 关键路径上,
  但把它写进 strategic doc §3.5 拒绝清单可避免 M1 时悄悄硬编码。

### F19 — CLAUDE.md(顶层)与 docs/* 把 ccteam 描述为"开发团队的编排层"(**N/A:M3 后 strategic doc + interfaces 团队抽象就位 / 仓库 docs 维持 dev-first 视角合理**)

- **文件:行号**:顶层 `CLAUDE.md`(全文以 dev 视角写)、`docs/requirements.md`
  全文以"做软件"为痛点中心、`docs/tech-design.md` §1 设计原则表 / §2.1 进程
  拓扑以 dev 流程举例
- **现状**:文档体系把 ccteam 写成"AI 开发团队"的编排器。
- **是否真 dev-specific**:**部分。** requirements.md 是 dev 团队的产品定义,
  迁移到非 dev 团队时**应该有 team 自己的 requirements.md**——本仓库的 dev
  requirements 不应被 research 复用。tech-design.md 的 mechanism 论证是通用的,
  只是举例都来自 dev。
- **解耦方案**:
  1. `docs/requirements.md` 顶部加一句"本文档是 dev 团队的需求定义;其它团队
     有自己的 requirements 文件:`docs/teams/<team>/requirements.md`";
  2. `docs/tech-design.md` 顶部加一句类似免责;
  3. 顶层 `CLAUDE.md` §一 已说"ccteam 是 Claude Code 之上的元工具",但
     §三所有举例都是 dev——M3 启动时加一节"Team 抽象层(M3+)"作前置阅
     读;
  4. 长期看,`docs/` 应分 `docs/core/`(领域无关)+ `docs/teams/<team>/`(领
     域特定),M3.4 一并整理。
- **优先级**:**P1**——文档体系不解耦,新加入 ccteam 项目的人会以为"ccteam
  只服务 dev 团队"。M3 启动 PR 必须同步动文档。
- **2026-05-06 重新评估为 N/A**:M3 落地后,`docs/ccteam-as-domain-agnostic-orchestrator.md`
  作为团队抽象 charter 已就位,`interfaces.md` / `tech-design.md` 已加 team
  抽象章节(M3 ship commit `23449cb`);`requirements.md` / 顶层 `CLAUDE.md`
  保持 dev-centric 视角是**合理的**——ccteam 仓库本身的产品形态是 dev 团队
  (痛点 1-13 都是 dev 痛点),非 dev 团队的需求由对应 `teams/<team>.yaml`
  的描述字段 + `phases-<team>/` 文档承担。F19 不再需要单独追踪。

### F20 — 跨项目记忆 schema 假设 dev 字段(**已修复:M3.1 retro_schema 数据形式 + M3.4 product-research 填字段 + 09-ship.md / 06-verdict.md 消费 schema**)

- **文件:行号**:`docs/tech-design.md §3.7` "关键字段:tech stack、踩过的坑、
  成功的设计选择、不要再做的事";`docs/v0-1/development-plan.md §5 M3.1` "输出固
  定字段(tech stack/坑/成功设计/不要再做)"
- **现状**:M3 retro pattern 字段全是 dev 视角。
- **是否真 dev-specific**:**是。** research 项目的 retro 字段不同(方法学 /
  数据源 / 假设结果)。
- **解耦方案**:同 strategic doc §2.7——team.yaml 的 `retro_schema[]` 决定字
  段。**reorder 后顺序明确**:M3(团队抽象)交付 `retro_schema` 数据形式 + 解析,
  M4.1(retro phase 实现)直接读 schema,从 day 1 就能写出团队特定字段。
- **优先级**:**P0**(2026-05-05 升级,见下方注解)——本文档 §development-plan
  reorder 后,跨项目记忆从原 M3 移到 M4(团队抽象 M3 之后),F20 自动落在 M3
  关键路径上,不再是"M4 实现时再补"的可后置项。

> **2026-05-05 升级注解**:原标 P1。ABC session 完成后用户审视发现:跨项目记
> 忆(原 M3)在团队抽象(原 M4.5)之前实施时,retro 字段写死成 dev 字段会让
> 后续跨项目 lessons 字段重写。`docs/v0-1/development-plan.md` 已 reorder M3 ↔ M4(团队抽象前
> 置),F20 现在阻塞新 M4 启动 —— 升级为 P0。
>
> **2026-05-06 状态更新**:M3.1 / M3.2 已 ship `retro_schema` 数据形式 +
> 校验(`crates/ccteam-core/src/team.rs`);`teams/dev.yaml` 已填 4 字段
> (tech_stack / pitfalls / successful_designs / do_not_do_again),
> `teams/product-research.yaml` 仍空(注释"M4.1 may revise")。M4 改走
> 官方记忆机制后,`retro_schema` 字段不再驱动 RAG 索引,而是驱动 retro phase prompt
> 写入 `~/.claude/rules/ccteam-lessons-<team>.md` 时的字段段落布局。
>
> **2026-05-06 关闭**:M4.1 ship 后(`873aa0a feat(M4.1): team-aware retro phase
> prompts`),`phases/09-ship.md` 与 `phases-product-research/06-verdict.md` 都
> inline retro 段按各自 team.yaml 的 `retro_schema` 字段写入 marked section;
> product-research `retro_schema` 已填 5 字段(market_signals / 等)。F20 关闭。

### F21 — `stall_warn_minutes` phase YAML 字段已 spec 但 orchestrator 未读取(**已修复:2026-05-06 @ a5fb21d**)

- **文件:行号**:`docs/interfaces.md §5.1` 已声明 phase YAML 有
  `stall_warn_minutes` 字段;`crates/ccteam-core/src/stall.rs` 的 `STALL_WARN_SECONDS`
  / `STALL_SUSPICIOUS_SECONDS` / `STALL_ESCALATE_SECONDS` 是常量,**与 phase 模板
  无关**。
- **现状**:phase 模板写 `stall_warn_minutes: 60` 不生效——orchestrator 永远
  按 5 / 15 / 30 分钟三档来 warn。
- **是否真 dev-specific**:**部分是。** dev 团队的 plan-eng / implement / fix
  阶段 5 分钟 warn 还合理(LLM 应该已经在出 token);但 research 团队 04-primary
  data-collection phase **正常就要等用户回 inbox 数据**,可能持续小时级
  ——5 分钟 warn 完全错配语义。
- **解耦方案**:
  1. `stall.rs` 改为接受 `&PhaseTemplate`,从 `template.stall_warn_minutes`
     字段读阈值;`STALL_*` 常量退回为"phase 没声明时的默认值"
  2. `phases.rs` 解析时把字段填进 `PhaseTemplate` 结构体(可能已有,需 verify)
  3. 文档 `interfaces.md §5.1` 明确说"3 档阈值的具体倍数"——M0 是 1×/3×/6×
     `stall_warn_minutes`,即 phase 写 60 → 60/180/360 分钟三档
- **优先级**:**P1**——M0.5.3 顺手做最经济(已经在动 phases.rs 解析逻辑);
  否则 M1 cross-cutting watcher 上线就会撞上(per-phase 阈值是 watcher 调度
  的核心条件)
- **审计漏报原因**:本条是"已 spec 未实现"的半完成态——schema 写在 interfaces.md
  里、orchestrator 假装支持(没崩),但实际行为不一致。常规审计扫现状代码
  vs 文档断言,容易漏掉这种"文档说有、代码静默忽略"的隐性债

### F22 — 项目 slug 缺 team 前缀,导致 `~/.claude/rules/*.md` `paths:` scope 失效(**已修复:2026-05-06 follow-up PR**)

- **文件:行号**:`crates/ccteam-core/src/projects.rs::pick_unused_slug` 现接受
  `team: &str` 参数;`crates/ccteam-cli/src/commands.rs::run_new` + `mcp_serve.rs::handle_new`
  调用点更新
- **修复**:`pick_unused_slug` 产出 `<team>-<base>`(collision 时 `<team>-<base>-<suffix>`);
  meta-agent 走自己的 `meta_slug(handle)` → `<handle>-meta`,不动
- **何为 fix-the-fix**:`interfaces.md §1.2` 项目目录约定改为 `~/projects/<team>-<slug>/`;
  M4.4 spike report §3 改 closed,§4 deferred check 解锁
- **migration**:历史项目目录(F22 前创建)保留原名;orchestrator 通过 state.json
  `team` 字段识别身份,目录名只是路径方便。新建项目走新规则。
- **来源**:`docs/v0-1/m4-spike.md` §3

### F23 — 容器 bind-mount `~/.claude/rules/` 待 M4.4 spike §4 重跑后定夺(2026-05-06 加)

- **文件:行号**:N/A(未发现现状代码缺陷,**待 spike 验证**)
- **现状**:M4.4 §4 deferred 检查"`--dangerously-skip-permissions` 容器
  里 `~/.claude/rules/*.md` 是否被 Claude Code 当做 context 注入"——**F22 修复后
  spike 已解锁**,等谁跑一次就能定夺(F23 → P0 / 关闭)
- **是否真 dev-specific**:**否——是基础设施层。**
- **解耦方案**(若 spike 失败):doctor 加 `--bind-mount-claude-rules`
  子模式(往容器配置追加只读 mount),sketch 见
  `docs/v0-1/m4-spike.md` §4
- **优先级**:**P1 conditional**——F22 修完后跑 spike;失败才升 P0
- **来源**:`docs/v0-1/m4-spike.md` §4

### F24 — orchestrator daemon 死亡时 MCP 默认 ack 静默成功(**已修复:M0.23.1**)

- **文件:行号**:`crates/ccteam-cli/src/mcp_serve.rs::tool_send_to_session` /
  `tool_pause` / `tool_resume` / `tool_inject_decision`(M0.23.1 前)
- **现状**:这些 action 工具写完磁盘 / 改完 state.json 就返回成功,完全
  不检查 orchestrator 是否在跑——daemon 死了消息派不出去,用户以为成功。
- **是否真 dev-specific**:**否——是基础设施层(每个团队都需要)。**
- **解耦方案**(已落):daemon 每 30s touch
  `~/.ccteam/state/orchestrator.heartbeat`;MCP action 工具入口 stat 该文件
  mtime,>60s grace 视为死亡,直接返回 error。read-only 工具(`ls`/`show`/
  `peek`/`progress`)不阻塞,`ls` 响应里附 `orchestrator.daemon_health` 让
  meta-agent 自决定要不要提示用户。详见 tech-design §6.8。
- **优先级**:**P0**(用户报告"消息不送达")。
- **来源**:`docs/v0-2/dev-plan.md §9` M0.23.1 + M0.23.3。

### F25 — 1M context 未默认启用,新项目 claude session 跑标准上下文(**已修复:M0.23.2**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs::OrchestratorConfig::default`
  (M0.23.2 前 `claude_argv` 只含 `["claude", "--dangerously-skip-permissions"]`)
- **现状**:tech-design §6.1/§6.9 红线"默认开 1M 上下文",代码却没传 `--model`
  flag。新项目 claude session 用账户默认 model(标准 200K context),撞 60%
  reset 阈值会非常快;cache 重用空间也小。
- **是否真 dev-specific**:**否——所有 team 都长跑,都需要 1M。**
- **解耦方案**(已落):default `claude_argv` 加 `--model
  claude-sonnet-4-6[1m]`(`[1m]` 后缀是 Claude Code 文档语法标准用法)。
  常量名 `DEFAULT_CLAUDE_MODEL` exported,后续升 model 改这一处。tests
  用 `claude_argv: vec!["sh", "-c", ...]` stub 不受影响。
- **优先级**:**P0**(长跑直接撞 context 上限)。
- **来源**:`docs/v0-2/dev-plan.md §9` M0.23.2 / `docs/v0-2/prd.md §7`。
- **后续**:Claude 团队若推出更新 model 别名(eg `claude-sonnet-4-7[1m]`),
  改 `DEFAULT_CLAUDE_MODEL` 一处即可;CLI flag 形式保持稳定。

### F28 — Project-layer team override 是 dead code(2026-05-08 加;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-core/src/team_resolver.rs:48-52`
  (`TEAM_SOURCES = [Project, User, Repo]`)+ `:139`(`with_project()`
  builder);production callers 均不带 `with_project`
  (`orchestrator.rs:1717` `for_orchestrator(...)` /
  `commands.rs:909` `run_phase_show` 同)。
- **现状**:M0.17 三层 resolver 设计 + docs 描述 first-source-wins,但
  `<project>/.ccteam/team/team.yaml` 在 production path 永远不被探查;
  User layer 与 Repo layer fall-through 工作正常。
- **是否真 dev-specific**:**否——team resolution 基础设施。**
- **解耦方案**:`for_orchestrator(...)` callsite 改
  `with_project(state.project_dir)`;`run_phase_show` 同;补 e2e 验证
  project 层真 first-source-wins。
- **优先级**:**P1**(用户报"per-project override 不生效"时阻塞)。
- **来源**:V0.2 e2e Suite B B5 / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:加 `Orchestrator::team_runtime_for_state(state)` (返回 `Cow<TeamRuntime>`);dispatch / process_project / stall / cost 命中 callsite 切到 project-aware lookup;`run_phase_show` 在 cwd 有 `.ccteam/team/team.yaml` 时走 `with_project(cwd)`;e2e test `team_resolver_project_layer_e2e_test.rs` 验证 project layer 真 first-source-wins。`TEAM_SOURCES::Project` enum variant 保留(M0.17 设计未改)。

### F29 — 无 CLI/env stub-claude 注入路径(2026-05-08 加;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs:238`
  (`OrchestratorConfig.claude_argv` 仅 Rust struct);
  `ccteam-cli/src/main.rs` `Command::Start` 无 `--claude-argv` flag;
  `OrchestratorConfig::default()` 不读 `CCTEAM_CLAUDE_ARGV`。
- **现状**:Rust unit/integration test 通过
  `claude_argv: vec!["sh", "-c", ...]` stub claude;CLI e2e / CI smoke
  无注入路径,phase pipeline 必须真 spawn claude(烧 LLM cost)。
- **是否真 dev-specific**:**否——测试基础设施。**
- **解耦方案**:`OrchestratorConfig::default()` 读 `CCTEAM_CLAUDE_ARGV`
  (shell-split);`ccteam start --claude-argv "<line>"` flag。
- **优先级**:**P1**(testability;阻塞纯 CLI e2e 验证 phase loop)。
- **来源**:V0.2 e2e Suite B B1/B2 / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:`OrchestratorConfig::default()` 读 `CCTEAM_CLAUDE_ARGV`(whitespace-split);`ccteam start --claude-argv "<line>"` flag(优先级:flag > env > default);integration tests `orchestrator_claude_argv_env_test.rs` + `start_claude_argv_flag_test.rs`。

### F30 — `doctor --validate-team` `[FAIL]` 不影响 exit code(2026-05-08 加;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-cli/src/commands.rs::render_validate_team_report`
  (line 719 起);Summary 计数器只 sum 每 phase finding,plugin-section
  的 `[FAIL]` 行(eg `plugin.json parse: missing field 'author'`)不入
  Summary,exit code 始终 0。
- **现状**:`doctor --help` 文案 "Fails-loud on schema violations and
  IO-contract gaps" 与实际行为冲突;CI / 脚本无法
  `&& gh release ...` gate。
- **是否真 dev-specific**:**否——doctor 基础设施。**
- **解耦方案**:`render_validate_team_report` 累计 plugin-section
  `[FAIL]` 进 Summary;exit code 据 `failures > 0` 设非零。
- **优先级**:**P1**(team factory CI gating 缺位)。
- **来源**:V0.2 e2e Suite D D7 / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:`render_validate_team_report` 改返回 `(String, u32 fails)`,plugin-section + phase-section `[FAIL]` 全部累计进 fails;`run_doctor` 在 `fails > 0` 时 `bail!` → 非零 exit code;integration test `doctor_validate_team_fail_loud_test.rs` 验证 unknown team / corrupt plugin manifest 都触发非零退出。

### F31 — `TeamSpec` 缺 `#[serde(deny_unknown_fields)]`(2026-05-08 加;落实 M0.22 ⚠1;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-core/src/team.rs:107`(`TeamSpec` struct
  顶级)+ nested(`TeamGoldenRules` 等)。
- **现状**:typo(eg `cost_polciy:`)静默 fall-back 到默认值;团队作者
  无 feedback。M0.22 PR description ⚠1 已 flag,V0.2 e2e Suite D D8 复现。
- **是否真 dev-specific**:**否——团队作者 UX。**
- **解耦方案**:加 `#[serde(deny_unknown_fields)]` 到 `TeamSpec` +
  nested;补 1 个 negative 测试。注:**plugin manifest 顶级保留
  `team.yaml` unknown 字段(M0.22 ⚠1)是 zod-strip 设计,与 ccteam 自己
  的 `TeamSpec` 严格 schema 互不冲突。**
- **优先级**:**P1**(team factory ship 前最好修)。
- **来源**:V0.2 e2e Suite D D8 / M0.22 PR ⚠1 / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:`#[serde(deny_unknown_fields)]` 加到 `TeamSpec` 顶层 + 所有 ccteam 自己定义的子结构(`RetroFieldSpec` / `ProtocolRule` / `DomainRule` / `TeamGoldenRules.Structured` / `CriticDimensionSpec` / `EscalateGrammarExtension`);`team::tests::f31_top_level_unknown_field_fails_loud` + `f31_nested_unknown_field_fails_loud` 验证 typo 触发 fail-loud。

---

## P2 — 边角

### F16 — `PHASE_TEMPLATES` 中 plan-ceo / 07-review / 08-score 缺位但 fix/test/ship 在位(**已修复:M3.4 多 team registry**)

- **文件:行号**:`crates/ccteam-core/src/templates.rs:24-31` 仅 6 个;
  `interfaces.md §5.2` 列了 9 个完整 phase
- **现状**:M0 只交付 6 个 dev phase(plan-eng / implement / test-author /
  test-run / fix / ship),plan-ceo / review / score 留给 M1+/M2+/M4。
- **是否真 dev-specific**:**是**(在交付的 phase 都是 dev fill)。
- **解耦方案**:同 F10——团队配置决定该 binary 内嵌哪些 phase。
- **优先级**:**P2**(随 F10 一并做)。
- **2026-05-06 已修复**:M3.4 落 multi-team registry 后,每个 team 的 phase 集
  由 `team_bundle(team)` 的 `phases: &'static [(&'static str, &'static str)]`
  字段决定;dev 团队仍只 6 phase(plan-ceo / review / score 留给 M2+ / M5),
  product-research 团队是完整 6 phase pipeline。"哪些 phase 入 binary"已是 team
  配置驱动的数据,不再是 PHASE_TEMPLATES 单一常量。剩余 dev plan-ceo / review /
  score 的实现归于产品功能(M2+ / M5),与领域无关耦合无关。

### F17 — 测试用例硬编码 dev phase 名

- **文件:行号**:`crates/ccteam-core/tests/state_machine_test.rs:42`
  (`["plan-eng", "implement", "test-author", "test-run", "fix", "ship"]`);
  `tests/e2e_happy_path_test.rs:20`(`"plan-eng"`)等多处。
- **现状**:test fixture 硬编码 dev DAG。
- **是否真 dev-specific**:**测试本来就 dev-specific**——它们验证的是 dev
  pipeline 的具体语义。
- **解耦方案**:M3.1 把这些测试整体迁到 `tests/team-dev/` 命名空间;新增
  `tests/team-research/` 验证 research DAG 跑通。不强行改测试 fixture 用 team
  配置——单元测试该面向具体场景。
- **优先级**:**P2**——目录重命名,不阻塞功能。

### F18 — `fix_loop_writes_with_ccteam_dir_already_present` 等测试名假设 fix(**已修复:2026-05-07 rename PR — `fix_loop_test.rs` → `auto_loop_test.rs`**)

- **文件:行号**:`crates/ccteam-hooks/tests/fix_loop_test.rs`(整个文件)
- **现状**:测试模块、文件名、测试函数名都用 `fix_loop`。
- **是否真 dev-specific**:**命名上**——同 F5/F6/F7,机制是 auto-loop。
- **解耦方案**:F5 重命名时一并改,`fix_loop_test.rs` → `auto_loop_test.rs`。
- **优先级**:**P2**(随 F5 一并改)。

### F26 — `mcp_serve::install_mcp()` 不 honor `CLAUDE_CONFIG_HOME`(2026-05-08 加;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-cli/src/mcp_serve.rs:660-666`
  (用 `dirs::home_dir()` 写入 `~/.claude.json`,asymmetric with
  `--install-skill` / `--install-memory-bridge` 走 `user_claude_dir()`)。
- **现状**:e2e 隔离测试必须叠加 `HOME=$E2E_ROOT` 兜底,否则
  `--install-mcp` 写入用户真 `~/.claude.json`。
- **是否真 dev-specific**:**否——env 处理一致性。**
- **解耦方案**:走 `user_claude_dir()`(已 honor `CLAUDE_CONFIG_HOME`)。
- **优先级**:**P2**(e2e harness 痛点,production 用户透明)。
- **来源**:V0.2 e2e Suite A / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:`install_mcp()` 走 `ccteam_core::projects::resolve_claude_json_path()`(已 honor `CLAUDE_CONFIG_HOME`,与 trust-entry writer + sibling installers 对称);integration test `install_mcp_claude_config_home_test.rs` 验证 redirect。

### F27 — `ccteam ls` 无 daemon health 注解(2026-05-08 加;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-cli/src/commands.rs::render_ls_text`
  (1391-1409 行;无 daemon section);`render_ls_json`(1422-1456;
  `orchestrator.running: null` 硬编码);`crates/ccteam-core/src/daemon.rs:46-49`
  (无 public `heartbeat_alive(paths) -> bool` helper)。
- **现状**:M0.23.1 daemon health check 已加到 MCP write 路径(F24
  closed),`ls` read-only 不 block 但也不注解;`docs/v0-2/dev-plan.md §9`
  M0.23.1 文字未明确要求 `ls` 注解。
- **是否真 dev-specific**:**否——观测层 UX。**
- **解耦方案**:`daemon.rs` 加 public `heartbeat_alive(&Paths) -> bool`;
  `render_ls_text` 加 head 一行 `daemon: <up|down>`;`render_ls_json`
  填 `orchestrator.running` 字段。
- **优先级**:**P2**(meta-agent 已可通过 MCP `ls` JSON `orchestrator`
  字段读取 — 该字段需先填值)。
- **来源**:V0.2 e2e Suite A A5 / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:`daemon::heartbeat_alive(&Paths) -> bool` public helper;`render_ls_text` head 一行 `daemon: <up|down>`;`render_ls_json` `orchestrator.running` 改 bool;3 unit tests 覆盖 down / up / json shape。

### F32 — daemon path doc drift(2026-05-08 加,docs-only;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`docs/v0-2/dev-plan.md §9` M0.23.1 写
  `~/.ccteam/daemon.{pid,heartbeat}`;实际代码路径
  `~/.ccteam/state/orchestrator.{pid,heartbeat}`(`crates/ccteam-core/src/daemon.rs:30,47`)。
- **现状**:F24 PR description 已记录"沿用 M1.5 已建立的 `state/`
  子目录约定 + `orchestrator.pid` 现有命名,语义等价";`tech-design.md
  §6.8` 已对齐,`dev-plan.md §9` 待回填。
- **是否真 dev-specific**:**否——纯 docs。**
- **解耦方案**:`docs/v0-2/dev-plan.md §9` M0.23.1 路径段更新一行。
- **优先级**:**P2**(cosmetic;不影响代码)。
- **来源**:V0.2 e2e Suite A / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:`docs/v0-2/dev-plan.md §9` M0.23.1 路径段更新为 `~/.ccteam/state/orchestrator.{pid,heartbeat}`,与 tech-design §6.8 / 实际代码对齐。

### F34 — slug 命名失控(算法 + 接口缺位;2026-05-09 加;**已修复:2026-05-09(V0.2.2 PR #2)**)

- **文件:行号**:`crates/ccteam-core/src/projects.rs::pick_unused_slug` 字符级 slugify(40-char cap,无 token / 语义层裁剪);`crates/ccteam-cli/src/main.rs::Commands::New` 缺 `--slug` 接口
- **现状**:brief 整段 slugify 撞死冗长 slug(`dev-ccteam-ui-ccteam-1-2-session-subagent-3` ≠ 用户原话 `ccteam-ui`);meta-agent 派单前不确认项目名,用户后悔无路(slug 重命名不支持)。
- **是否真 dev-specific**:**否——通用 UX 缺陷**。
- **解耦方案**:四层调用栈 — Tier 1 `ccteam new --slug X`(B2 prefix 自动加 team-)/ Tier 2 `ccteam-project-creator` skill(meta-agent 调,带 AskUserQuestion 结构化选项)/ Tier 3 `claude -p haiku` 智能 fallback(15s timeout,Y/n 确认 / `--no-auto-slug` env 控)/ Tier 4 deterministic `slugify_brief()`(token-aware + stop-word + dedup + 取前 3 token)。
- **优先级**:**P1**(用户日常摩擦点)。
- **来源**:2026-05-08 用户实战反馈 issue #1+#2(`docs/v0-2-2/feedback.md` / `docs/v0-2-2/prd.md §3`)。

### F35 — auto-loop 过度依赖 Stop event(2026-05-09 加;**已修复:2026-05-09(V0.2.2 PR #3 silence classifier)**)

- **文件:行号**:`crates/ccteam-core/src/auto_loop.rs::decide` 输入仅 `last_assistant_text`(只在 Stop hook 触发时跑);`progress.jsonl` 末事件 ≠ Stop 时(API tool-call hang / send-keys 路由错)永不触发。
- **现状**:DeepSeek API 调用未返回 → mid-tool-call hang → 无 Stop → auto-loop 停在 iteration 1 永不重试;`/btw` 注入 `phase_inject` 后无任何后续 event(F36 case)同样卡死。
- **是否真 dev-specific**:**否——控制平面盲区**。
- **解耦方案**:`silence_classifier.rs` 7-class deterministic classifier(Healthy/Terminal/SubagentBusy/SubagentRunaway/MidToolHung/PostStopLimbo/InjectLimbo)按 progress.jsonl 末事件语义 × 静默时长分类;limbo 类 deterministic re-inject 1 次(`MAX_LIMBO_RETRY = 1`,`limbo-retry-count.json` 记账,phase 推进 reset);hung 类 enriched outbox(`needs_attention.outbox.json` 加 `ccteam_classification` / `ccteam_silent_seconds` / `ccteam_last_event` / `ccteam_pane_tail`);meta-agent 走 propose-confirm NL 翻译,**不 autonomous decide**(红线"smart layer 只 translation 不 decision")。`capture_pane_tail` 提到 `ccteam-core::tmux` 共享,F38 同帮手字段 `with_ansi: bool`。
- **优先级**:**P0** ship-blocker(实际 bug,V0.1/V0.2 用户已撞)。
- **来源**:2026-05-08 用户实战反馈 issue #3+#5(`docs/v0-2-2/feedback.md` / `docs/v0-2-2/prd.md §4`)。

### F36 — send-keys 注入到活跃 subagent(2026-05-09 加;**已修复:2026-05-09(V0.2.2 PR #4 subagent guard)**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs::dispatch_phase_with_state` 注入前不感知 subagent 状态(`PreToolUse(tool=Task)` 未配 `SubagentStop`),tmux send-keys 落到 subagent 上下文。
- **现状**:`/btw` 注入 `test-author` prompt 时 `code-reviewer` subagent 仍活跃,prompt 被 subagent 接收(无工具权限,无法执行);主 agent 永不收到,无 Stop → auto-loop 卡死。
- **是否真 dev-specific**:**否——控制平面盲区**。
- **解耦方案**:`progress::subagent_active(events)` pure deterministic helper(扫末尾事件序列,counting `PreToolUse(Task)` − `SubagentStop` 配对窗口);dispatch 前 guard:active → `<project>/.ccteam/pending-inject.json` 落盘 defer + 不发 send-keys;daemon tick 在 `SubagentStop` event 后 drain pending(再 guard 防 race);`max_defer_minutes`(默认 10)兜底,超时 → 走 F35 enriched outbox(classification = `inject_defer_timeout`)+ 删 pending。F35 InjectLimbo 是 race 漏接的兜底层(`attempt_limbo_reinject` 见 pending exists 则跳过,避免烧 retry quota)。
- **优先级**:**P0** ship-blocker(实际 bug,V0.1/V0.2 用户已撞)。
- **来源**:2026-05-08 用户实战反馈 issue #4(`docs/v0-2-2/feedback.md` / `docs/v0-2-2/prd.md §5`)。

### F37 — meta-agent 绕开 pipeline 自调研(2026-05-09 加;**已修复:2026-05-09(V0.2.2 PR #2 决策树加固 + ccteam-project-creator skill)**)

- **文件:行号**:`crates/ccteam-core/src/templates/meta_agent_role.md` §1 决策树边界不严("调研 X" 被错误归入"问答"分支)+ §3 克制规则缺"❌ 不起 Agent subagent 自调研"反例。
- **现状**:用户要求"调研 Multica",meta-agent 没派 product-research,而是自起 `Agent(subagent_type=general-purpose)` 做 Web 搜索 + 直出结论;product-research 6-phase pipeline(kickoff / research / verdict / next-steps)被绕过,无可审计调研记录。
- **是否真 dev-specific**:**否——meta-agent 决策树软约束被漂移**。
- **解耦方案**:`meta_agent_role.md` §1 加 "调研 X = 项目请求" 反例(项目请求段含"调研 / 评估 / 分析 / 看看 X 值不值得");§3 克制规则加 "❌ 不要自起 `Agent(subagent_type=general-purpose)` / 调用 web 搜索做调研" 反例;§2 派单段 inline rules 抽出,改"走 `ccteam-project-creator` skill"(skill body 接 Phase A/B/C/D — 需求澄清 / slug 推荐 / team 选择 / 派单);F34 + F37 同 PR 落地。
- **优先级**:**P1**(决策树漂移影响 UX 一致性)。
- **来源**:2026-05-08 用户实战反馈 issue #6(`docs/v0-2-2/feedback.md` / `docs/v0-2-2/prd.md §6`)。

### F38 — 终端截图 PNG(UX 增强;2026-05-09 加;**已修复:2026-05-09(V0.2.2 PR #5 vt100 + imageproc DIY)**)

- **文件:行号**:无(新功能)。F35 enriched outbox `pane_tail` 是文本维度,`channel adapter` / 用户人眼缺直观视觉维度。
- **现状**:meta-agent 通知用户项目状态时只能给文本 `pane_tail`(30 行 capture-pane);ANSI 颜色 / progress bar / 框线字符在文本里失真。
- **是否真 dev-specific**:**否——通用 UX 增强**。
- **解耦方案**:`tmux capture-pane -e` → `vt100::Parser` cell grid → `imageproc::drawing::{draw_filled_rect_mut, draw_text_mut}`(`ab_glyph` 字体)→ `image` PNG。**纯 Rust 全栈**(vendored JetBrainsMono-Regular.ttf OFL via `include_bytes!`,绕过 font-kit / fontconfig 系统依赖);`ANSI_256` 调色板常量(16 + 216 cube + 24 grayscale);`mcp__ccteam__screenshot(slug, lines?)` MCP 工具(namespace 保留 ccteam,V0.3 评估改名);`ccteam doctor --screenshot-smoke <slug>` flag;`std::panic::catch_unwind` 兜 vt100 / imageproc 边缘 panic;`CCTEAM_SCREENSHOT_FONT_TTF` env 覆盖字体。F35 enriched outbox 后续可加 `screenshot_path` 字段(本 PR ship MCP 工具 + smoke,outbox 集成留 follow-up)。
- **优先级**:**P2**(UX 增强,补 F35 视觉维度;非阻塞)。
- **来源**:用户 2026-05-09 追加(`docs/v0-2-2/prd.md §7`);载体经 5 轮迭代:Pillow → vte+tiny-skia → freeze(Linux 5.15 segfault) → ansee(font-kit deps)→ vt100+imageproc DIY(选定)。

### F39 — `cct` 短前缀约定 sweep(2026-05-09 加;**已修复→后被 F44 反向**:2026-05-09 PR #1 落地;2026-05-10 PR #8 反向回滚 — 详 F44)

- **文件:行号**:`crates/ccteam-cli/Cargo.toml::[[bin]] name`、`skills/cct-*/SKILL.md`、`crates/ccteam-core/src/templates/settings.json` 占位符。
- **现状(F39 实施时)**:V0.1/V0.2 ship 时命名前缀都是 `ccteam-`;F39 把 binary 改 `cct`、skill 改 `cct-*`、占位符改 `{{CCT_BIN}}`、Rust API 改 `current_cct_bin` / `install_cct_*` / `CCT_*_SKILL_NAME`。
- **后续**:**F44 把 F39 全部反向**。原因:`/usr/bin/cct` 已被 Ubuntu `proj-bin`(PROJ Coordinate Conversion / GIS)占用,`~/.local/bin/cct` 在标准 PATH 上会静默 shadow 系统工具。
- **优先级**:已实施 → 已反向。
- **来源**:用户 2026-05-09 追加;反向决策 2026-05-10。

### F44 — 反向 F39 cct convention sweep,恢复 ccteam 二进制(2026-05-10 加;**已修复:2026-05-10(V0.2.2 PR #8)**)

- **文件:行号**:全 F39 触及面(binary、skill、Rust API、placeholder、docs sweep、CLAUDE.md §三/§四/§六)反向回滚到 ccteam-* 命名。
- **现状**:F39 选 `cct` 二进制名时未检查 namespace;Ubuntu `proj-bin` 包提供 `/usr/bin/cct`(PROJ 工具),与 ccteam 同名碰撞,`~/.local/bin/cct` 在标准 PATH 上前置会静默 shadow 系统 GIS 工具。
- **是否真 dev-specific**:**否——命名碰撞修复**。
- **解耦方案**:逐一反向:binary `cct` → `ccteam`、skill `cct-*` → `ccteam-*`、Rust `current_cct_bin` → `current_ccteam_bin` / `install_cct_*_skill` → `install_ccteam_*_skill`、placeholder `{{CCT_BIN}}` → `__CCTEAM_BIN__`、docs sweep。`ccteam doctor` 加 F39 → F44 反向迁移(检测 `~/.claude/skills/cct-{control,team-author,project-creator}/` marker + frontmatter 校验,匹配则 rm -rf;rewrite `~/projects/<slug>/.claude/settings.json` 内 `cct hook` → `ccteam hook` 原子写)。**保留**(per F39 §8.3):MCP server name `ccteam`、`~/.ccteam/` 项目根、crate 名、git repo / workspace 名 — 这些 F39 本就没动。
- **优先级**:**P0**(silent-substitute footgun)。
- **来源**:用户 2026-05-10 反馈 + 实证 `dpkg -S /usr/bin/cct` 命中 `proj-bin`。

### F45 — write 动作 + 读端 helper 锁在 ccteam-cli,V0.3 web crate 无法复用(2026-05-10 加;**整体 close:V0.3 M5.3 PR #4 写动作 endpoint + auth gate 落地**)

- **文件:行号**:`crates/ccteam-cli/src/mcp_serve.rs::tool_send_to_session`(line 456,`fn` 非 `pub fn`)+ `tool_inject_decision`(line 500,同)+ pause / resume 在 mcp_serve 内 inline logic 无独立 fn。
- **现状**:V0.2.2 ship 时,写动作 helper 全部位于 `ccteam-cli::mcp_serve.rs` 私有 fn,只供 MCP tool dispatch 内部消费;V0.3 web UI(新 crate `crates/ccteam-web`)需要复用同一逻辑写 inbox / control,但 `ccteam-web` 不能 depend on `ccteam-cli`(binary-as-library 反模式 + dep 图倒挂 — `ccteam-cli` 是 binary entry,`ccteam-web` 是新 crate,sibling 关系应共下沉 `ccteam-core`)。读侧 helper 已 public(`collect_projects` / `collect_recent_events` / `run_resume` / `run_show`,以及 `ccteam_core::ProjectState` / `CcteamPaths` / `render_screenshot` / `tmux::capture_pane_*` / `check_daemon_health` / `SessionMailbox` / `inbox_filename` / `pick_unused_slug` / `bootstrap_project`),写侧未 promote。
- **是否真 dev-specific**:**否——dep 图整理 + 跨消费者复用**。
- **解耦方案**:V0.3 M5.0(PR #1)新建 `crates/ccteam-core/src/actions.rs`,提 4 个 pub fn:`send_to_session(paths, slug, text)` / `inject_decision(paths, slug, DecisionInput)` / `pause(paths, slug)` / `resume(paths, slug)`;`mcp_serve.rs::tool_*` 内部 logic 全提到 `actions::*`,wrapper 留 args 拆 + JSON encode;`ccteam-web::routes::actions::*` 直调 `ccteam_core::actions::*`,`ccteam-web` 只 depend on `ccteam-core`(`cargo tree -p ccteam-web` 验证不出现 `ccteam-cli`)。M5.3(PR #4)写动作 endpoint 落地后 close。
- **优先级**:**P1**(V0.3 M5.0 启动门槛;`ccteam-web` 没这个 promote 就走不通)。
- **来源**:V0.3 M5.0 audit(`docs/v0-3/prd.md §3`)。
- **2026-05-10 部分修复(V0.3 PR #1)**:`crates/ccteam-core/src/actions.rs` 落地;4 个 pub fn(`send_to_session` + `send_to_session_with` / `inject_decision` / `pause` / `resume`)外加 `next_inbox_seq` / `next_inbox_path` / `DecisionInput` / `SendOptions` / `SendResult`。`mcp_serve.rs::tool_send_to_session` / `tool_inject_decision` / `tool_pause` / `tool_resume` 全部改为薄 wrapper(args 拆 + JSON encode + 调 `actions::*`),18 个 mcp_serve 测试不变绿(回归保证 wrapper 透传);`commands::run_resume` body 提到 `actions::resume`,旧 fn 仅留 thin-wrap。`crates/ccteam-web/tests/dep_graph_test.rs` 自检 `cargo tree -p ccteam-web` 不命中 `ccteam-cli`。
- **2026-05-10 读端补强(V0.3 PR #2)**:`ProjectSummary` / `collect_projects` / `collect_recent_events` 同样 promote — 原存于 `ccteam-cli::commands` 公有 fn,但 `ccteam-web` 不能 depend on `ccteam-cli`(同 binary-as-library 反模式),不能 import 它们。新建 `crates/ccteam-core/src/queries.rs` 模块,move 三者过来 + 加 6 个单元测试;`ccteam-cli::commands` 留 `pub use ccteam_core::{collect_projects, collect_recent_events, ProjectSummary};` 让 `mcp_serve.rs` / `run_ls` / `run_progress` 现有 import 路径不变。M5.1 dashboard / project handler 直接调 `ccteam_core::queries::*`,dep_graph_test 仍绿。
- **仍 open**:web 层 POST endpoint(`/api/<slug>/{btw,inject_decision,pause,resume}`)在 M5.3 落地后才真正消费 actions::*,届时本 finding 整体 close。
- **2026-05-10 整体 close(V0.3 PR #4)**:`crates/ccteam-web/src/routes/actions.rs` 落地 — 四个 POST handler `handle_btw` / `handle_inject_decision` / `handle_pause` / `handle_resume` 全部 thin-call `ccteam_core::actions::*`,validation(text length 1..=4000 / decision body 1..=8000 / 路径 absolute + 不含 `..` + `starts_with(project_ccteam_dir(slug))`) 在 route boundary 完成。`crates/ccteam-web/src/auth.rs` 加 token-Bearer middleware(loopback 信任默认 / 非 loopback 自动开 + 文件 mode 0600 + URL shim cookie)+ `/health` 例外。`cargo tree -p ccteam-web | grep ccteam-cli` 仍 0 命中,dep_graph_test 守红线。47 新测试覆盖 actions 4 endpoint + auth 8 路径 + token 文件 5 场景;737 全绿(690 baseline → 737)。本 finding **整体 close**。

### F46 — HarnessAdapter trait 缺位,Claude Code statusline 结构化数据丢失(2026-05-10 加;**2026-05-10 V0.3.1 PR #1 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/harness.rs`(新建,~720 LOC + 16 unit tests);`crates/ccteam-web/src/routes/harness_sse.rs`(新建);`crates/ccteam-web/src/watcher.rs`(sibling 线程 + `HarnessSnapshotEvent` channel);`crates/ccteam-cli/src/commands.rs::run_hook_harness_snapshot` + `install_statusline_adapter`;`~/.claude/statusline-command.sh` wrapper 落 `~/.ccteam/harness/<slug>-<sid>.json`。
- **现状**:V0.3 web UI dashboard 的数据源 = `progress.jsonl` + tmux pane 截图(F38);Claude Code statusline + subagent 面板里**有大量结构化数据**(模型名 / context% / token / cost / rate-limit / subagent),V0.3 dashboard 拿不到结构化版本(只截图视觉版),F35 enriched outbox 也只 ASCII pane_tail。V0.3.1 引入 Codex 后,session 也不再只有一种 harness — 需要 trait 抽象统一。
- **是否真 dev-specific**:**否——presentation 信息抽象层。** 跟 phase / team 无关,所有 harness 都需要(claude / codex / 未来 cline / aider / 等)。
- **解耦方案(已 ship)**:`crates/ccteam-core/src/harness.rs` 加 `HarnessAdapter` trait + `HarnessSnapshot` / `SubagentState` / `SpawnOpts` / `SessionHandle` / `HarnessError` 数据结构;`ClaudeCodeAdapter` 完整实现 trait 全部方法;`ccteam doctor --install-statusline-adapter` 安装 wrapper(marker `# ccteam-managed:statusline begin (V0.3.1 F46 ...)` 保护用户手改 + 原文件 backup `.bak-<YYYYMMDDTHHMMSSZ>`);`ccteam hook harness-snapshot` 子命令做 raw-stdin 双写(失败永不 bubble,statusline render 路径神圣);`~/.ccteam/harness/<slug>-<sid>.json` 协议(stdin JSON 全覆盖,delta archive V0.4 deferred);web SSE `GET /sse/harness/<slug>` + `/sse/harness/<slug>/<sid>`(sibling broadcast channel,与 progress 通道独立)。**红线**:harness snapshot 只是 presentation,**不参与 orchestrator 状态决策**(progress.jsonl 仍 SoT)。
- **优先级**:**P0**(V0.3.1 foundation;F47/F49/F50 都依赖 trait shape)。
- **来源**:`docs/v0-3-1/prd.md §3`;研究 doc `docs/research/v0-3-1-harness-adapter-plan.md`(V0.3 ship 前临时记录,本 finding 是其正式版);用户 Telegram 2026-05-10 message 311。

### F47 — Codex CLI 缺前向兼容 trait stub,V0.3.2 实现无地方接入(2026-05-10 加;**2026-05-10 V0.3.1 PR #2 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/harness.rs`(扩 `CodexAdapter` 全 stub + 共享 `CODEX_NOT_IMPLEMENTED_REASON: &str` 常量;5 个新单元测试);`crates/ccteam-core/src/team.rs`(加 `HarnessKind { Claude, Codex }` + `DefaultSessionSpec { sid, harness }` + `TeamSpec::sessions: Vec<DefaultSessionSpec>` 字段;8 个新单元测试);`crates/ccteam-cli/src/main.rs::Commands::Session` + `SessionAction { Add, Ls, Attach, Rm }` + `HarnessKindCli` ValueEnum;`crates/ccteam-cli/src/commands.rs::run_session_{add,ls,attach,rm}`(stub handlers,`add --harness=codex` 走 `CodexAdapter::spawn_session` NotImplemented 错误);`run_doctor::render_codex_detection_line`(`which codex` 输出 informational,不 fail);`crates/ccteam-cli/tests/session_cli_test.rs`(5 个 CLI integration tests);`docs/interfaces.md §5.5`(schema 同步)。
- **现状**:V0.3.1 不实现 Codex 完整支持(`docs/research/ccteam-codex-integration.md` M2-M5 路线 ~月级工程量),但 CLI 与 schema 需要现在就接受 `harness: codex` 让用户提前声明意图,V0.3.2 落 `CodexAdapter` 实现时不破已 ship 代码。
- **是否真 dev-specific**:**否——前向兼容 stub。**
- **解耦方案(已 ship)**:`CodexAdapter` 全 stub 实现 trait,`spawn_session` / `ingest_snapshot` / `shutdown_session` 全返 `Err(HarnessError::NotImplemented { harness: "codex", reason: <static str citing V0.3.2 + docs/research/ccteam-codex-integration.md> })`(reason 是 `&'static str`,无 alloc;统一 `CODEX_NOT_IMPLEMENTED_REASON` 常量);`team.yaml::sessions[]` schema(`harness: claude | codex` 严格 enum,serde 默认 `claude`,`#[serde(deny_unknown_fields)]` 在 `DefaultSessionSpec`);`ccteam session add --harness=codex` 走 codex stub error path 返 exit 1 + stderr 含 V0.3.2 引用;`add --harness=claude` 与 `ls/attach/rm` 暂返"see F49 (V0.3.1 PR #4)" hint(F49 落 master state.json::sessions 后才能跑实际 spawn);`ccteam doctor` 任何 mode 跑完后追加 informational `[ccteam] codex CLI: present @ <path>` 或 `not found (V0.3.1 trait-stub only; install codex CLI for V0.3.2+ — see docs/research/ccteam-codex-integration.md)`(不 fail)。共 20 个新测试,777 → 797 baseline 全绿。
- **优先级**:**P1**(forward-compat 接口,本 PR 不阻塞用户跑 claude harness)。
- **来源**:`docs/v0-3-1/prd.md §4` + `docs/research/ccteam-codex-integration.md` M0-M1 路线 + 用户 Telegram 2026-05-10 message 313 决策"V0.3.1 落 stub,V0.3.2 落 impl"。

### F48 — `kind: flex` team kind 缺位,phase 编排是 ccteam 唯一工作姿态(2026-05-10 加;**2026-05-10 V0.3.1 PR #3 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/team.rs::TeamSpec`(加 `kind: TeamKind` 字段);`crates/ccteam-core/src/orchestrator.rs::TeamRuntime`(加 `should_run_auto_loop` / `should_inject_phase` / `should_check_golden_rules` helpers);`crates/ccteam-core/src/team_factory.rs::init_team_staging`(`--kind=flex` 跳过 phase scaffold)。
- **现状**:V0.1/V0.2/V0.3 ccteam 团队都是 phase-driven(workflow 暗含),没有"空 phase / 用户原生姿态驱动 session" 这种姿态。V0.3.1 战略 pivot 把 ccteam 扩展为 session farm,需要支持用户在 session 里自由跑(无 phase 注入 / 无 auto_loop / 无 golden_rules),ccteam 只观测 + 记录 + 提供控制面。
- **是否真 dev-specific**:**否——team kind 抽象。**
- **解耦方案(已 ship)**:`team.yaml::kind` 字段,`TeamKind { Workflow, MultiWorkflow, Flex }`,`#[serde(default)]` 保 V0.1/V0.2/V0.3 yaml parse 不变(默认 `Workflow`)。`kind: flex` 与 `parallelism`(phase 级字段)正交 — flex 团队无 phase 所以 `parallelism` 不适用。orchestrator behavior gating 三个 helper(`should_run_auto_loop` 等)按 kind 返 bool;**flex 团队 silence_classifier / cost watcher / hooks / progress.jsonl / 跨项目 memory bridge 仍跑**(observability 全保留)。team factory `--kind=flex` scaffold 跳过 phase markdown,默认 `sessions: [{sid: claude-1, harness: claude}]`。
- **优先级**:**P0**(V0.3.1 战略 pivot 核心;F49 multi-session 在此基础上)。
- **来源**:`docs/v0-3-1/prd.md §5`;用户 Telegram 2026-05-10 message 311 原话:"team 工厂创建出空的 phases 团队...用户原始用 claude code 方式来完成"。

### F49 — Adhoc multi-session 缺位,单项目无法托管 N 个不同 harness session(2026-05-10 加;**2026-05-10 V0.3.1 PR #4 ship,close**)

- **文件:行号**:`crates/ccteam-cli/src/commands.rs::run_session_{add,ls,attach,rm}`(新建);`crates/ccteam-core/src/state.rs::ProjectState`(扩 `sessions: BTreeMap<sid, SessionRecord>` + `next_sid_seq: BTreeMap<harness, u64>` 字段);`crates/ccteam-hooks/src/progress.rs`(`progress.jsonl` 路径解析按 `state::team_kind` 分流到 `<slug>/<sid>.jsonl` 子目录 vs flat `<slug>.jsonl`)。
- **现状**:V0.3 ccteam 项目 = 单 tmux session(except `parallelism: multi_session` 的 phase 级 fan-out 拓扑)。flex 团队需要 adhoc:用户起项目后任意时刻 `ccteam session add` 起新 session,删 session,attach 任一 session,混合 Claude+Codex(支持 cross-review pattern)。multi_session 的 master + 预定义 sub-module 是 phase 级、代码维度,**不**适合 adhoc + 进程维度。
- **是否真 dev-specific**:**否——session 维度并行机制。**
- **解耦方案(已 ship)**:**新轻量 session 注册**(不复用 multi_session 拓扑)— `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/` 子目录,master `state.json::sessions{sid: {harness, tmux_session, started_at, pid}}` + `next_sid_seq{harness: u64}` 字段(serde default 空 BTreeMap 保 V0.3 单 session 项目兼容);sid 格式 `<harness>-<n>`,n 单调递增**删后不复用**;tmux 命名 `ccteam-<slug>-<sid>`(workflow / multi_workflow 项目仍 `ccteam-<slug>` 不变);`progress.jsonl` flex 项目走 `~/.ccteam/progress/<slug>/<sid>.jsonl`,workflow 项目仍 `<slug>.jsonl` flat;`ccteam session {add,ls,attach,rm}` CLI;**`rm` 是唯一显式用户授权 kill 路径**(CLAUDE.md §三 红线)。Codex add 仍走 V0.3.2 stub error。
- **优先级**:**P0**(V0.3.1 战略 pivot 核心 — flex 团队的实际用法)。
- **来源**:`docs/v0-3-1/prd.md §6`;用户 Telegram 2026-05-10 message 312"多 session 拉前 V0.3.1"决策。

### F50 — Web 层假设单 session 项目,flex 团队无展示路径(2026-05-10 加;**2026-05-10 V0.3.1 PR #5 ship,close**)

- **文件:行号**:`crates/ccteam-web/templates/dashboard.html`(加 `Kind` 列);`crates/ccteam-web/templates/project.html`(flex 项目走 N session cards 模板分支);新模板 `templates/session.html` + handler `routes/session.rs`;`crates/ccteam-web/src/routes/sse.rs`(扩 `/sse/project/<slug>/<sid>` server-side filter);`crates/ccteam-core/src/screenshot.rs::render_screenshot`(签名加 `sid: Option<&str>`)。
- **现状**:V0.3 web UI(M5.0-M5.4 ship)假设单 session 项目;V0.3.1 引入 flex + adhoc multi-session 后,dashboard 需要 `Kind` 列,project 详情页对 flex 走 N session cards,需要新页 `/session/<slug>/<sid>`,SSE 需要 sid filter,screenshot 需要扩 `<slug>-<sid>.png`。
- **是否真 dev-specific**:**否——展示层适配。**
- **解耦方案(已 ship)**:dashboard 加 `Kind` 列(workflow / multi_workflow / flex);`Phase` 列对 flex 项目渲染 `—`;flex 项目 detail 页走 N session cards 分支(harness badge 蓝/绿,缩略截图,Detail link);workflow / multi_workflow 项目 detail 完全不变(回归保证);新页 `/session/<slug>/<sid>` 渲染 header + per-session events / harness panel / write actions sidebar;`/sse/project/<slug>/<sid>` server-side `msg.sid == <sid>` 过滤(`ProgressUpdate` 加 `sid: Option<String>`);`/sse/harness/<slug>/<sid>` 推 harness_snapshot;`/screenshot/<slug>-<sid>.png` 路由 — `render_screenshot(slug, Some(sid), opts)`;`/screenshot/<slug>.png` workflow 项目保留(V0.3 兼容)。
- **优先级**:**P1**(展示层 — 用户首屏看到的 V0.3.1 价值,但不阻塞 backend ship)。
- **来源**:`docs/v0-3-1/prd.md §7`;用户 Telegram 2026-05-10 message 315 dashboard 决策。

### F51 — V0.3.1 ship gate(2026-05-10 加;**2026-05-10 V0.3.1 PR #6 ship,close**)

- **文件:行号**:`Cargo.toml`(workspace.package.version 0.3.0 → 0.3.1);`CLAUDE.md` §一 baseline 表格 + §六 易踩坑;`docs/v0-3-1/e2e-retro.md`(新建);`docs/v0-2/README.md`(V0.3 pointer 更新);`docs/dev-coupling-audit.md` F46-F51 close 标记;`crates/ccteam-web/tests/flex_e2e_test.rs`(新建)。
- **现状**:V0.3.1 patch round(F46-F50)ship 后需要正式 ship gate。V0.2.2 政策:每 minor / patch release 必须 bump workspace.version + commit subject `vX.Y.Z:` 前缀。
- **是否真 dev-specific**:**否——chore + ship gate。**
- **解耦方案(已 ship)**:flex_e2e_test.rs 跑端到端(reqwest server happy path + codex error path);`Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"`;CLAUDE.md §一 baseline 回填(workspace.version + 833/0 + V0.3.1 milestone 行);CLAUDE.md §六 加 V0.3 → V0.3.1 升级注;`docs/v0-3-1/e2e-retro.md` 4-suite 跨 flex 多 session / harness adapter / web UI / codex stub;`docs/v0-2/README.md` 更新 V0.3 pointer 为 "已 ship V0.3 + V0.3.1";dev-coupling-audit.md F46-F51 close 标记;tech-design.md / interfaces.md 增量段终稿。
- **优先级**:**P0**(ship gate)。
- **来源**:`docs/v0-3-1/prd.md §8` / §12 / §13。

### F63 — workflow.yaml schema + parser 缺位,phase 编排是 ccteam 唯一姿态(2026-05-14 加;**2026-05-14 V0.4.0 PR #4 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/workflow.rs`(新文件,`WorkflowSpec` / `AgentSpec` / `Trigger` / `Executor` / `OnTimeout` / `WorkflowError`);`crates/ccteam-core/src/lib.rs`(新增 `pub mod workflow` + `pub use`);`crates/ccteam-core/Cargo.toml`(`indexmap = { version = "2", features = ["serde"] }` 直接依赖,原为 serde_yaml 的 transitive);`crates/ccteam-core/tests/workflow_test.rs`(新 18 测试);`crates/ccteam-core/tests/fixtures/workflow-{ui-quality-loop,research-loop}.yaml`(新 2 fixture);`docs/interfaces.md §17`(workflow.yaml schema 参考)。
- **现状**:V0.3.x 之前 ccteam 唯一调度姿态是 phase 模板(`PHASE_TEMPLATES` + DAG + `auto_loop`)+ flex adhoc session;workflow.yaml(用户定义 agent 拓扑 + trigger graph)缺位。V0.4.0 大重构(F60 删 phase 系统 / F61 CC adapter 极薄 / F62 Codex adapter 实做 / F66 thin orchestrator)需要先有一个**纯数据 + 校验**的 schema 层作底子,否则 F64 watcher / F65 MCP / F66 orchestrator 无处接入。
- **是否真 dev-specific**:**否——meta-orchestrator 数据模型基础设施缺口。** workflow.yaml 的 agent role 名是**用户定义数据**,WorkflowSpec 自身不出现任何 team 名字面量;orchestrator(F66 落地)通过 role 名驱动调度,与 dev / qa / chainup / 任意领域 team 无关。
- **解耦方案(已 ship)**:`WorkflowSpec` 顶层 `name: String` + `description: Option<String>` + `agents: IndexMap<String, AgentSpec>`(IndexMap 保留 YAML 声明顺序,trigger graph 构建 / 日志 / fixture round-trip 确定性);`AgentSpec` 字段 `executor`(Claude / Codex,默认 Claude) / `trigger` / `parallelism: Option<u32>` / `input: Option<PathBuf>` / `output: Option<PathBuf>` / `interval: Option<String>`(V0.4.0 占位,V0.4.1 接 cron) / `timeout: Option<String>` / `on_timeout: Option<OnTimeout>`(Escalate / Retry / Skip);`Trigger` 自定义 `Deserialize` impl,YAML 标量字符串 `manual` / `schedule` / `gate` / `watch:<path>` → 对应变体,匹配 Serialize 反过来写回(F66 / F68 web 都依赖此 round-trip);`WorkflowSpec::load(&Path)` = read + serde_yaml::from_str + validate;`load_for_project(&Path)` 按序探 `<dir>/workflow.yaml` → `<dir>/.ccteam/workflow.yaml`;`validate()` 5 条规则:(1) agents 非空 (2) role 名 `[a-z0-9_-]` (3) `watch:` path 非空 (4) `gate` 必须有 input (5) `parallelism > 1` 只允许 watch;`WorkflowError`(thiserror)四变体:NotFound / ReadFailed(`#[from] io::Error`) / ParseFailed(`#[from] serde_yaml::Error`) / ValidationFailed(String)。**红线**:workflow.yaml 内严禁 `prompt:` / `system_prompt:` / `messages:` 字段(prompt 住 `.claude/agents/<role>.md`);workflow.rs 纯数据,不写 progress.jsonl / 不动 tmux / 不接 MCP。Fixture 双示例(ui-quality-loop = explorer manual / fixer watch+parallel=10 / reviewer watch codex / shipper gate;research-loop = claw manual / evaluator watch+parallel=5)。测试 18 项:t01-t02 fixture 加载 + 字段断言 / t03-t05 + t16-t17 校验 5 规则全否定路径 / t06 / t06b / t07 项目级 discovery 双路径 + NotFound / t08-t09 default 行为 / t10-t12 trigger 正向路径 / t13 重复 key last-wins(serde_yaml 行为) / t14 round-trip / t15 unknown executor → ParseFailed;workflow.rs 内置 8 单元测试覆盖 `parse_trigger` 边界 + `validate_role_name`。
- **优先级**:**P0**(V0.4.0 重构基石,F64 / F65 / F66 全部依赖)。
- **来源**:`docs/v0-4-0/prd.md §6.1` + `docs/v0-4-0/prd.md §F63` + `docs/v0-4-0/dev-plan.md §5`。

### F65 — Meta-agent MCP workflow tools 缺位,无法用 NL 驱动 workflow(2026-05-14 加;**2026-05-14 V0.4.0 PR #6 ship,close**)

- **文件:行号**:`crates/ccteam-cli/src/mcp_workflow_tools.rs`(新建,7 工具 handler + schema + dispatch + 17 单元测试);`crates/ccteam-cli/src/mcp_serve.rs`(`tool_definitions()` extend、`call_tool()` fall-through dispatch、tool 计数测试 10 → 17);`crates/ccteam-cli/src/main.rs`(`mod mcp_workflow_tools`);`crates/ccteam-cli/tests/mcp_e2e_test.rs`(tool 列表断言 + 6 新 e2e 测试);`docs/interfaces.md §12.2`(7 工具表格行 + 红线说明)。
- **现状**:V0.3.x 的 ccteam-mcp 10 工具集面向 phase 模型(`ls`/`show`/`new`/`pause`/`resume`/`send_to_session`/`inject_decision` 等);V0.4.0 引入 workflow.yaml + agent role 拓扑后,meta-agent 没有任何工具能(a) 立即派一个 agent session(b) 软停 / signal 一个 session(c) 一次性观察当前所有 agent(d) 热调 parallelism(e) 解锁 Gate(f) 看 artifact 桶。没这层 meta-agent 无法用 NL 驱动 workflow,只能 shell 出 `ccteam` CLI 走老路,V0.4.0 三层架构(meta-agent + orchestrator + harness)的中层失语。
- **是否真 dev-specific**:**否——meta-orchestrator 控制平面缺口。** 所有 workflow.yaml-based team(ui-quality-loop / research-loop / 任意用户定义拓扑)都需要这层。
- **解耦方案(已 ship)**:`mcp_workflow_tools.rs` 7 工具全是**文件系统控制平面**——每个 mutating tool 写 marker 文件到 `<project>/.ccteam/<bucket>/`(`spawn_requests/` / `stop_signal/` / `signal/` / `gate_override/` / `workflow_overrides.json`),F66 thin orchestrator 每 tick 扫桶 → 执行 → 删 marker;`observe_agents` 一次性读 `state.json::sessions`(V0.3.1 F49 registry,F66 会扩 record);`get_artifact_summary` stat-only 遍历 `workflow.yaml` 各 agent 的 `input`/`output` 目录(O(n) on inode,不读 file 内容)。`spawn_agent` 校验 role 在 workflow.yaml 中存在 → 写 spawn_requests/<role>-<ts>.json + 返回 session_id(`<role>-<rfc3339-ms>`)。`set_parallelism` 范围校验 1-50,原子 `tmp + rename` 写 workflow_overrides.json,合并已有 role(并发不互踩)。`signal` 四值 `pause`/`resume`/`interrupt` 写 marker、`btw` 走 `actions::send_to_session_with`(source="ccteam-mcp",source_user="meta-agent:signal:<role>")格式化 META-AGENT BTW payload 落 inbox。`dispatch()` 返 `Option<String>`(`None` = 不属本模块工具,let `mcp_serve.rs` 走 fall-through "unknown tool" 错误);`requires_daemon()` 区分 mutating(5 个)vs 只读(2 个)。`mcp_serve.rs::call_tool` 把 F65 工具收口在 `other => { gate + dispatch }` 一处,M2.5 dispatch 表不动。**红线**:本 PR 不动 `crates/ccteam-core/src/orchestrator.rs`(F66 在改);不动 `crates/ccteam-core/src/harness.rs`(F61 在改);ccteam-core 零 team 名字面量保留。**F66 集成 hook**:每个 marker 桶都用 inline `// F66:` 注释标注消费路径,F66 实现时 grep `// F66:` 即可定位所有 hand-off 点。测试:6 e2e(`crates/ccteam-cli/tests/mcp_e2e_test.rs::t_spawn_agent_returns_session_id` / `t_observe_agents_empty` / `t_set_parallelism_writes_override_file` / `t_get_artifact_summary_empty_dirs` / `t_trigger_gate_writes_marker` / `t_signal_btw_writes_inbox`),tool list 断言 10 → 17,所有 17 工具 inputSchema.type=object 断言;17 单元测试覆盖 dispatch / requires_daemon / 各工具 happy + sad path(unknown role / unknown signal / parallelism 越界 / btw 无 message / observe 空 / artifact 空 / artifact 计数)。
- **优先级**:**P0**(V0.4.0 meta-agent 控制平面;F66 thin orchestrator 实现依赖本 PR marker 桶设计)。
- **来源**:`docs/v0-4-0/prd.md §6.3` + `docs/v0-4-0/prd.md §F65` + `docs/v0-4-0/dev-plan.md §7`。

### F66 — Phase 状态机 → thin orchestrator(2713 LOC → ~820 LOC 含 doc;V0.4.0 PR #7;2026-05-14 加;**2026-05-14 V0.4.0 PR #7 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs`(F60 ~140 LOC 桩 → F66 ~820 LOC 新调度 shell;含 7 类 progress event 写入 + Gate 状态机 + budget guard + fix-loop 3-strike escalate + 测试 surface);`crates/ccteam-core/src/artifact_watcher.rs`(F66 内联最小 stub:F64 真实 inotify-backed 实现 merge 时取代;pre-scan 已有工件補触发 + tokio `mpsc` 信道契约稳定);`crates/ccteam-core/src/lib.rs`(`pub mod artifact_watcher` + `pub use ArtifactEvent / ArtifactWatcher`);`crates/ccteam-core/Cargo.toml`(`[features] test-util` 加入 default + 文档化:暴露 `test_*` + `set_adapter` 方法给集成测试 crate);`crates/ccteam-core/tests/orchestrator_thin_test.rs`(新 20 测试;`MockAdapter` 实现 `HarnessAdapter`,`serial_test::serial` 串行化 env-mutating 用例)。
- **现状**:V0.3.x phase 状态机(`PHASE_TEMPLATES` + DAG + auto_loop + decide_tick + dispatch_phase + golden_rules)2713 LOC 已在 F60 删尽;`orchestrator.rs` 留下 ~140 LOC 桩 `Orchestrator { paths, config }` + `run_project` / `run` 返回 `todo!()`。F66 在桩上重建新 workflow-driven dispatch:**orchestrator 不再注入任何 prompt,只做生命周期管理**(行为由 `.claude/agents/<role>.md` 自带,见 PRD §6.6 红线)。
- **是否真 dev-specific**:**否——meta-orchestrator 编排层重构。** 新 orchestrator 完全数据驱动:从 `workflow.yaml::agents` 读 role / trigger / parallelism,从 `ArtifactWatcher` 读 artifact 事件,从 `HarnessAdapter` spawn 真实 session;ccteam-core 内零 team 名字面量(红线 5 grep 验证);新 7 类 progress event(workflow_start / agent_spawn / agent_done / artifact_received / gate_triggered / budget_exceeded / workflow_done)+ escalation event 形成新业务 SoT 写入。
- **解耦方案(已 ship)**:`Orchestrator { paths, config, adapters: HashMap<&'static str, Arc<dyn HarnessAdapter>>, running, pending, fail_counts, gate_states, cost_accum }` 七字段全 `Arc<Mutex<...>>` 抗 tokio 并发;`Orchestrator::new` eager 注册 claude + codex 两 adapter(F62 真实 CodexAdapter);`run_project` 流程 = `WorkflowSpec::load_for_project`(F63)→ 写 `workflow_start` → `ArtifactWatcher::new`(F64 / 本 PR 桩)→ `dispatch_initial_triggers`(manual / schedule log "waiting",gate 落 GateState::Waiting,watch 由 watcher pre-scan 補触发)→ `event_loop`(`tokio::select!` 接 mpsc 信道 + 5s `tokio::time::interval` 跑 `poll_completions` + `check_workflow_done`)。`handle_artifact_event`:写 `artifact_received` → 检查 `agent.parallelism.unwrap_or(1)` ceiling → 满则 push pending FIFO,否则 `try_spawn`;`try_spawn`:budget guard 走 `progress.jsonl::agent_done.cost_usd` 累加(progress 是 SoT,跨重启正确)→ 超 `team.yaml::cost.hard_kill_threshold_usd` 或 `CCTEAM_BUDGET_LIMIT_USD` env 默认 `$200` → 写 `budget_exceeded` event + `send_btw_escalation`(永不 kill 已运行 session,红线 3);通过则 `adapter.spawn_session(SpawnOpts)` → 成功写 `agent_spawn`,失败 `bump_fail_count` → `escalation` event(`consecutive_failures`)+ 到 `MAX_CONSECUTIVE_SPAWN_FAILURES=3` 写 meta-agent inbox `<utc>-escalation-<slug>.txt`(红线 4,CLAUDE.md §三 fix-loop 3-strike)。`poll_completions`:扫每个 running handle 的 `state.json::status`(CC = `~/.claude/jobs/<sid>/state.json`,Codex = `~/.ccteam/codex/<sid>/state.json`,`CCTEAM_SESSION_STATE_DIR` env 测试覆盖)→ `stopped/completed/error` 视为完成 → 写 `agent_done` + dequeue pending → reset `fail_count` if success;`check_gates` 每 tick 跑:`Trigger::Gate` agent input 目录有 file 或 `.ccteam/gate_override/<role>` 存在 → `gate_triggered` event + try_spawn + 状态 → `Fired`(override file 强制路径 → remove file 后再 spawn);`check_workflow_done` 在所有 `Trigger::Gate` agent 都到 `Fired` + 无 running 时写 `workflow_done`,sentinel key `__workflow_done__` 防双写。`ArtifactWatcher` stub(F64 未 merge):构造时按 `Trigger::Watch(path)` 扫 project_dir 下已有文件 seed 进 `pending_seed`;`start()` spawn tokio task drain seed → `pending::<()>` parked;F64 merge 时整文件替换为 notify-backed event loop,**caller surface(`new` / `start` + `ArtifactEvent { role, artifact_path }`)不变**。`OrchestratorConfig` 保留(`tick_interval` / `claude_argv` / `ready_timeout` / `post_ready_warmup` / `skip_tool_check`)— F66 dispatch 不读但 CLI flag 仍 set,ABI 兼容。测试 surface 通过 `#[cfg(any(test, feature = "test-util"))]` 暴露 `test_handle_artifact_event` / `test_running_count` / `test_pending_count` / `test_fail_count` / `test_poll_completions` / `test_gate_override` / `test_register_running` / `test_adapter_keys` + `set_adapter`;`test-util` 加入 `default` features 让 `cargo test --workspace` 即默认 pick up 该 suite。**红线全过**:0 prompt-inject grep / 7 类 event 各 ≥1 写入 / 0 kill-running-session(`shutdown_session` 仅出现在红线文档注释)/ 24 hit fail_count / escalation 体现 3-strike rule / 0 team-name 字面量(`"ccteam"` / `"chainup"` / `"dev"`)。测试 20 项覆盖 happy-path(t01-t04 load+dispatch)、parallelism + queue(t05/t09/t17)、gate 流程(t06/t08/t10)、budget(t07/t12)、escalation(t18/t19)、independence(t13/t14/t15/t16)、never-kill-meta-agent(t20)。
- **优先级**:**P0**(V0.4.0 编排层重写核心)。
- **来源**:`docs/v0-4-0/prd.md §6.6` + `docs/v0-4-0/prd.md §F66` + `docs/v0-4-0/dev-plan.md §8`。

### F69 — V0.4.0 ship gate(2026-05-14 加;**2026-05-14 V0.4.0 PR #10 ship,close**)

- **文件:行号**:`Cargo.toml`(workspace.package.version `0.3.2` → `0.4.0`);`CLAUDE.md §一`(workspace version + 测试 baseline + 代码规模 + 已 ship 里程碑 + 当前 next 全部回填);`CLAUDE.md §六`(V0.3.2 → V0.4.0 migration 注 + Claude Code `--bg` CLI 漂移容错);`docs/v0-4-0/README.md`(状态 planning → SHIPPED + Findings 表全部 ✅);`docs/v0-4-0/e2e-retro.md`(完整 ship 验收记录 + 4 个 retro 教训 + 10 个 V0.4.1 候选);`crates/ccteam-web/src/routes/assets.rs`(legacy `/assets/{file}` 路由 + `handle_legacy_asset` + 5 个 `include_bytes!` 全删);`crates/ccteam-web/assets/{htmx.min.js,htmx-ext-sse.js,xterm.js,xterm.css,style.css}`(5 个 legacy 静态资源文件全删);`crates/ccteam-web/tests/assets_test.rs`(legacy 静态 byte assets 测试整文件删);`crates/ccteam-web/src/routes/sse.rs`(`htmx-ext-sse` 注释 → SPA EventSource 注释更新)。
- **现状**:F60-F68 全部 merged 后,需要 ship gate chore PR 收口:bump version、回填 CLAUDE.md baseline、完成 V0.3.3 deferred 的 legacy htmx static asset 清理(V0.3.2 F59 retired htmx routes 留下的 `include_bytes!` 块 + `/assets/{file}` 路由)、写实际 e2e retro。
- **是否真 dev-specific**:**否——ship gate chore + V0.3.3 deferred 项收口。**
- **解耦方案(已 ship)**:版本 bump 单点(workspace `Cargo.toml`);CLAUDE.md `§一` 表格 7 行回填(HEAD / version / baseline 770~ / clippy 9 pre-existing / 代码规模 15+12 / 里程碑 V0.4.0 + V0.4.1 candidates / 永久 deferred 不变),`§六` append 两个新坑(V0.3.2 → V0.4.0 migration 一次性迁移 + `claude --bg --agent` CLI 形态漂移容错);`docs/v0-4-0/README.md` 状态行 + Findings 表全部 ✅;`docs/v0-4-0/e2e-retro.md` 整文件重写(§1 ship gate 命令实际输出 + §2 4 个 retro 教训(F61 base 太老 redo / F61 SpawnOpts callsite 漏 / F66 ArtifactWatcher stub 与 F64 冲突 / F69 subagent hang)+ §3 smoke test 决策 + §4 10 个 V0.4.1 candidates 分 P0/P1/P2 + §5 ship 总结 + §6 后续 patch round 起点);legacy htmx 清理(F69 deferred 项)走 `crates/ccteam-web/src/routes/assets.rs` 顶部 module doc 重写、`handle_legacy_asset` 函数 + 5 个 `const HTMX_JS / HTMX_EXT_SSE_JS / XTERM_JS / XTERM_CSS / STYLE_CSS` + `.route("/assets/{file}", get(handle_legacy_asset))` 全删,只留 SPA `/app/*` + `/assets/spa/*` 路由;`crates/ccteam-web/assets/` 下 5 个 byte 文件物理删除(共 ~570 KB binary 资产);`crates/ccteam-web/tests/assets_test.rs`(4 个 legacy 测试用例)整文件删除;`crates/ccteam-web/src/routes/sse.rs::reconnect_hint` 注释 `htmx-ext-sse` → "SPA EventSource listener" 同义改述。**红线全过**(详 e2e-retro §1.5):phase 系统已 0 hit、progress.jsonl SoT 保留、CC adapter 不解析 tmux output、orchestrator 不主动 kill 长 session、ccteam-core 零 team 名字面量(`"ccteam"` 在 `actions.rs::DEFAULT_SOURCE_USER` + `tool_surface.rs` 测试 fixture 是 MCP server 名 identity 不是 team 名)、workflow.yaml 无 prompt 字段、SPA 不重建 Agent View、fix-loop 3-strike escalation 在 orchestrator 存在、MCP 工具 17 个(10 legacy + 7 F65 新)、不写 backwards-compat shim。已知 environmental flake:WSL inotify 资源受限导致 F64 `artifact_watcher_test` t02/t03/t05/t09 + F66 `orchestrator_thin_test` t01/t15 hang,V0.4.1 P0 根治。
- **优先级**:**P0**(round ship gate)。
- **来源**:`docs/v0-4-0/prd.md §F69` + `docs/v0-4-0/dev-plan.md §11 + §14`。

### V0.4.0 Round 总结

F60-F69 + F69-docs = **10 PR** ship,完成 ccteam 架构级重构:
- **删除**:phases.rs(934)+ golden_rules.rs(555)+ dag.rs(217)+ subskill.rs(522)+ orchestrator.rs phase 状态机 ~1200 + statusline 管道 ~800 + legacy htmx assets 5 文件 ~570KB(F69);
- **新建**:workflow.rs(411)+ artifact_watcher.rs(419)+ orchestrator.rs thin shell(~820)+ mcp_workflow_tools.rs(1014)+ harness.rs 新 ClaudeCodeAdapter(60 net)+ harness.rs 新 CodexAdapter(300)+ WorkflowView.tsx(~200)+ progress.rs/queries.rs workflow query layer(~400)+ examples/workflows/(2 yaml + 4 agent.md + README)+ docs/v0-4-0/(user-manual 497 + migration-guide 424 + e2e-retro 完整);
- **MCP 工具**:10 → 17(7 新 workflow tools);**Orchestrator**:2713 LOC phase 状态机 → ~820 LOC workflow-driven thin shell;**架构契约**:`workflow.yaml` 无 prompt + agent 行为完全由 `.claude/agents/<role>.md` 决定 + artifact 目录为唯一 inter-agent 通信媒介 + meta-agent 通过 17 MCP 工具用 NL 协调 + `progress.jsonl` 仍业务 SoT(8 类 workflow event)+ Agent View 不重建 + 永不 kill 长 session(budget 只软告警 + 阻新 spawn)+ fix-loop 3-strike escalate 不静默重置。
- **测试**:866 → ~770(phase 测试 -200,新 workflow + harness + progress + orchestrator + MCP + watcher 测试 +100)+ vitest 20。Workspace `version 0.3.2` → `0.4.0`。
- **后续**:V0.4.1 P0 三项(inotify flake 根治、Codex CLI argv 标准化、Codex bg-job 形态)+ P1 三项(workflow.yaml 条件分支、schedule trigger cron、跨项目 artifact 共享)+ P2 三项(`ccteam doctor --migrate-phase-to-workflow` 真实实现、fmt drift 清理 chore、真 binary smoke fixture)。详 `docs/v0-4-0/e2e-retro.md` §4。

### F67 — Progress tracking refactor:phase 查询函数 → workflow 聚合 + WorkflowSummary API(V0.4.0 PR #8;2026-05-14 加;**2026-05-14 V0.4.0 PR #8 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/progress.rs`(+~160 LOC:`AgentSessionStatus` / `AgentSessionSummary` 类型,`workflow_cost_total` / `current_agent_sessions` / `escalation_count` 三 pure-function 聚合,F60 phase 查询残留全部审计干净);`crates/ccteam-core/src/queries.rs`(+~180 LOC:`AgentStatus` / `WorkflowSummary` 类型,`workflow_summary(slug, &paths) -> Result<WorkflowSummary>` 入口、合并 workflow.yaml + progress.jsonl;`accumulate_session` / `count_files_in_dir` helper);`crates/ccteam-core/src/lib.rs`(+8 `pub use`:`AgentSessionStatus` / `AgentSessionSummary` / `AgentStatus` / `WorkflowSummary` / `workflow_summary` / `workflow_cost_total` / `current_agent_sessions` / `escalation_count`);`crates/ccteam-web/src/views.rs`(`DashboardRow.current_phase: String` 删除);`crates/ccteam-web/src/routes/api_v1.rs`(`ProjectSummary.current_phase` 删 + `decision_candidates` 删 + 新 `workflow_summary: Option<WorkflowSummary>`;`SessionDetail.decision_candidates` 删;`scan_candidates` 调用清除;`handle_project` 注入 `ccteam_core::workflow_summary(slug, paths)`);`crates/ccteam-web/tests/api_v1_test.rs`(断言改:`decision_candidates` / `current_phase` 必无,`workflow_summary` 必有);`crates/ccteam-core/tests/progress_refactor_test.rs`(新 12 测试 — `t01-t12`,详 §F67 scope 第 4 条);`docs/interfaces.md` §4(progress.jsonl 事件列表整段重写:8 类 workflow event + hook domain 表,字段语义表,写入责任表,消费方四档:orchestrator / `progress.rs` 聚合 / `queries::workflow_summary` / MCP `observe_agents`)。**预先修复**:F61 ship 时 `SpawnOpts` 加 `role` 字段未同步 `orchestrator.rs::try_spawn`,`SessionHandle` 加 `job_id` 字段未同步 `tests/orchestrator_thin_test.rs::MockAdapter` + `t20_meta_agent_not_killed`;本 PR 起手 3 行补齐(否则 `cargo check` 红)。
- **现状**:F60 删 phase 状态机后 progress.rs / queries.rs 内残留的 phase-specific 查询函数(`latest_terminal_event_for_phase` / `phase_transition_events` / `phase_history` 等)F60 已全数删尽,本 PR 接力做新业务聚合层 + 把 web 层(F68 即将做)与 meta-agent(F65 `observe_agents` 已 ship)需要的 `WorkflowSummary` API 落地。F66 写 8 类 workflow event(workflow_start / agent_spawn / agent_done / artifact_received / gate_triggered / budget_exceeded / workflow_done / escalation)到 progress.jsonl,本 PR 提供 pure-function 聚合(`workflow_cost_total` / `current_agent_sessions` / `escalation_count`)与 `workflow_summary(slug, paths)` 整合入口 — 后者读 workflow.yaml + progress.jsonl 后产出 `WorkflowSummary { workflow_name, agents[], artifact_counts, total_cost_usd, escalation_count, gate_states }`(`agents` 排序按 role ASCII,`gate_states` 默认 `waiting`,首个 `gate_triggered` event 翻 `fired`,`artifact_counts` 走每 agent `input` + `output` 目录的 `read_dir` 文件计数)。flex 项目通过 `team_kind == TeamKind::Flex` 切到 `collect_recent_flex_events` 读 per-sid 流 — workflow / multi_workflow 走 `progress::read_all_events` 读 `<slug>.jsonl`。`WorkflowSummary::default()` + `workflow_summary` 在缺 workflow.yaml 时返回 `workflow_name=""` 空摘要(不 500),让 F68 SPA 对 legacy 项目 graceful 降级。
- **是否真 dev-specific**:**否——business-state 查询层重构。** 本 PR 不写新事件、不引入新数据源;只把 F66 已写出的 progress 事件用 pure-function + 单 entry-point 暴露给 web / MCP。零 team 名字面量,零 tmux 解析,progress.jsonl 仍是 orchestrator 唯一 SoT。
- **解耦方案(已 ship)**:`AgentSessionStatus` 三变体 `Running` / `Done { cost_usd }` / `Errored { cost_usd }`(serde tagged `status`,snake_case),`AgentSessionSummary { role, session_id, started_at: DateTime<Utc>, status }` 字段稳定供 SPA cards;`current_agent_sessions` 走 `BTreeMap<session_id, summary>` 单一入口:`agent_spawn` insert if absent,`agent_done` 用 `status` ∈ {`completed`, `stopped`} → `Done`,其它 → `Errored`;输出按 `(started_at, session_id)` 排序确保 fixture 稳定。`workflow_cost_total` 只汇总 `agent_done.cost_usd`(F66 在 `agent_done` 写,`agent_spawn` 不带 cost — 因 harness 只在 session 结束时知道 cost);`escalation_count` 直接 count `event=="escalation"`。`workflow_summary`:1) `WorkflowSpec::load_for_project` 若返 `WorkflowError::NotFound` 则 spec=None(对 legacy 项目 graceful);2) flex / workflow 分流读 events;3) `gate_states` 初始化:每个 `Trigger::Gate` role 注入 `"waiting"`,然后 `gate_triggered` events 翻 `"fired"`;4) `artifact_counts` 走每 agent `input` + `output` 目录的 `read_dir` 数文件(`.is_file()`);5) `agents`:per-role `AgentStatus { role, running_count, queued_count: 0 (V0.4.0 占位), total_cost_usd, last_session_status }`,从 `current_agent_sessions` 逐条 accumulate,最后挂 `last_session_status`(按 `started_at` 排序的最后非 Running);若 sessions 出现 spec 外的 role(orphan)也保留一条 synthetic row,F68 UI 可显式高亮。`views.rs::DashboardRow` 删 `current_phase: String` — 该字段 F60 后已 decay 为空串,SPA dashboard 列改读 `workflow_summary` 形;`api_v1.rs::ProjectSummary` 删 `current_phase: String` + `decision_candidates: Vec<String>`,加 `workflow_summary: Option<WorkflowSummary>`(legacy 项目 `None`,新项目 `Some` — F68 渲染开始日;`scan_candidates`+`crate::decisions` 在 api_v1.rs 不再用,后续 PR / F68 可清模块);`SessionDetail.decision_candidates` 同步删。`api_v1_test.rs::get_api_v1_project_detail_returns_summary_shape` + `get_api_v1_session_detail_returns_harness_snapshot` 改断言:`decision_candidates` 必无、`current_phase` 必无、`workflow_summary` 字段必有(legacy 测试 fixture 用 `bootstrap_project` 不带 workflow.yaml → `workflow_summary` 字段 `null` 仍 PASS — 因 `Option::None` 序列化为 JSON null)。`docs/interfaces.md §4` 整段重写:`workflow_start` / `agent_spawn` / `agent_done` / `artifact_received` / `gate_triggered` / `budget_exceeded` / `workflow_done` / `escalation` 各列必填+选填字段+写入时机表,hook domain(PreToolUse / PostToolUse / SubagentStop / Stop / SessionEnd / notification / user_attach)保留为 V0.4.0 兼容性表。消费方四档(orchestrator 自身 / progress.rs 聚合 / queries::workflow_summary / MCP observe_agents)明示数据流向。**与 F65 `observe_agents` 对照**:F65 实现已留 `role` 字段读 placeholder + status="unknown" → F66 写 `state.json::sessions[<sid>].role` 后立刻 fill in;本 PR 不动该路径,只在 `workflow_summary` 中提供 session 维度的 cost / running_count 派生数据,F68 可叠加。**与 F66 orchestrator 对照**:本 PR 零修改 orchestrator.rs(除 F61 漏修补丁 1 行加 `role`);F66 写事件,本 PR 读事件,通过 `&[Value]` 切片消费 — 任何 F66 后续扩字段都不破本 PR API(仅添加而非更名)。测试 12 项覆盖 cost 汇总(t01)、session 状态机(t02-t03)、escalation 计数(t04)、空 + 默认路径(t05、t10)、phase symbol 残留审计(t06 — 编译级 + 运行级双断言)、SoT 基础函数 roundtrip(t07)、artifact 计数 + agent row(t08)、多 role cost 汇总(t09)、gate state(t11)、flex per-sid 流(t12)。**红线全过**:progress.rs / queries.rs 内零 phase 字段引用(grep `current_phase|phase_state|phase_history|golden_rules_check|latest_terminal_event_for_phase|phase_transition` 0 hit)、progress.jsonl 仍 SoT(`append_event` + `read_all_events` ≥2 hit)、零 tmux pane 解析、零 team 名字面量。
- **优先级**:**P0**(V0.4.0 web 适配前置 — F68 直接消费 `WorkflowSummary` API)。
- **来源**:`docs/v0-4-0/prd.md §F67` + `docs/v0-4-0/dev-plan.md §9`。

### F62 — V0.3.1 F47 CodexAdapter `NotImplemented` 桩(V0.3.3 → V0.4.0 slip;2026-05-14 加;**2026-05-14 V0.4.0 PR #3 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/harness.rs::CodexAdapter`(V0.3.1 F47 ship 时三个 fallible 方法全返 `HarnessError::NotImplemented`,`CODEX_NOT_IMPLEMENTED_REASON` const 指向 V0.3.2 / `docs/research/ccteam-codex-integration.md`);`crates/ccteam-cli/src/commands.rs::run_session_add` codex 分支(对 stub 做 `NotImplemented` 解包并打印 deferral 错误);`crates/ccteam-web/tests/flex_e2e_test.rs::v0_3_1_codex_adapter_remains_trait_stub`;`crates/ccteam-cli/tests/session_cli_test.rs::session_add_codex_exits_with_v0_3_2_pointer`。
- **现状**:V0.3.1 PRD §10.3 + V0.3.1 README erratum 把 CodexAdapter 真实实现 slip 到 V0.3.3;V0.4.0 架构重写(F60-F69)随手吸收。stub 阻止 flex team 真混用 claude + codex(`ccteam session add --harness=codex` 永远 exit 1)。
- **是否真 dev-specific**:**否——multi-harness 编排基础设施缺口。** 所有用 codex 做 review/QA gate 的 team(F65 ui-quality-loop sample 即依赖此)都受影响。
- **解耦方案(已 ship)**:`CodexAdapter::spawn_session` 走 `tmux new-session -d -s ccteam-<slug>-<sid> codex [extra_args...]`,挂自己 spawn 的 session(精确 `-t <name>` 不波及他人);`ingest_snapshot` 接收 tmux `capture-pane -p` 文本,grep `CODEX_STATUS: <json>` marker 行解析,无 marker → permissive fallback snapshot(`model="codex"`,`ctx_pct=0`,`cost=0`,不报 error;snapshot 流是 presentation-only,见 CLAUDE.md §三);`shutdown_session` 发 `q\r` send-keys → 500ms grace → `tmux kill-session -t <name>` 兜底,幂等于"已死"。初始 `~/.ccteam/codex/<sid>/state.json` 写 `{status:"starting", pid, model:"codex", context_pct:0, cost_usd:0}`(镜像 CC statusline dual-write 形,F66 watcher 统一消费)。`CODEX_STATUS_MARKER` + `CODEX_STATUS_TAIL_LINES` 通过 `pub use` 公开给 watcher / 测试。`crates/ccteam-cli/src/commands.rs::run_session_add` codex 分支与 claude 分支合并,共享 sid 分配 / session_dir 创建 / state.json 写入路径。`crates/ccteam-core/Cargo.toml` 加 `[features] codex-tests = []` + `serial_test` dev-dep;`crates/ccteam-core/tests/codex_adapter_test.rs` 新文件 6 测试(t01 spawn 创建 tmux session、t02 ingest fallback、t03 ingest parse、t04 shutdown 幂等清理、t05 三方法均不返 NotImplemented、e2e 走 fake codex shell 一次性 round-trip)`#[serial]` 串行化、`tmux_available()` 软跳过;`flex_e2e_test.rs` / `session_cli_test.rs` 老 stub 断言改为反向 regression guard。
- **优先级**:**P0**(multi-harness gating)。
- **来源**:`docs/v0-3-1/prd.md §10.3` erratum + `docs/v0-4-0/prd.md §F62` + `docs/v0-4-0/dev-plan.md §4`。

### F61 — ClaudeCodeAdapter thin refactor(V0.4.0 PR #2;2026-05-14 加;**2026-05-14 V0.4.0 PR #2 ship,close**)

- **文件:行号**:`crates/ccteam-core/src/harness.rs::ClaudeCodeAdapter`(V0.3.1 F46 ship 时绕 tmux + statusline-command.sh dual-write 管道,`spawn_session` 自己写 `~/.ccteam/harness/<slug>-<sid>.json`,`shutdown_session` 走 tmux `send-keys /exit` → 5s grace → `tmux kill-session`);`crates/ccteam-cli/src/main.rs::HookCommand::HarnessSnapshot`(statusline wrapper sink);`crates/ccteam-cli/src/commands.rs::run_hook_harness_snapshot` / `install_statusline_adapter` / `render_statusline_wrapper` / `find_latest_statusline_backup` / `STATUSLINE_MARKER_BEGIN`(install_statusline_adapter 200+ LOC 全 V0.3.1 F46 wrapper 管理);`crates/ccteam-cli/tests/statusline_install_test.rs`(217 LOC,V0.3.1 F46 doctor wrapper round-trip);`crates/ccteam-core/src/harness.rs::write_harness_snapshot` / `derive_harness_path`(F46 dual-write 实现)。
- **现状**:V0.3.1 设计假设 Claude Code 没有 first-class 后台任务接口,只能开 tmux session + 用 statusline JSON 旁路抽 status;V0.4.0 起 Claude Code 上线 `--bg --agent <role>` 后台 job + `~/.claude/jobs/<job_id>/state.json` 原生状态文件,V0.3.1 dual-write 管道全冗余。继续抱着 wrapper 增加 `claude statusline-command.sh` user-file 协调复杂度(F46 backup + marker + idempotent re-install),且 ccteam 错失 `state.json` 上的 `turn_count` / `status` 等结构化字段(F66 / web 仪表盘需要)。
- **是否真 dev-specific**:**否——harness 接口选型架构红线。** 任何 workflow.yaml-based team 都受影响:F66 thin orchestrator 读 state.json 决定下一步、F68 web 仪表盘读同源数据展示。
- **解耦方案(已 ship)**:`ClaudeCodeAdapter::spawn_session` 改 `Command::new("claude")` + `["--bg","--agent",<role>,"--output-format","stream-json","--workdir",<cwd>]`,捕获 stdout 第一行 JSON 解析 `job_id` → 写入 `SessionHandle.job_id`(新字段);`ingest_snapshot` 改读 `state_json_path(job_id) = $CCTEAM_CLAUDE_JOBS_DIR/<job_id>/state.json`(或 `~/.claude/jobs/...`)并 `parse_cc_state_json` 提取 `status`/`model`/`context_pct`/`cost_usd`/`turn_count`/`pid`,`HarnessSnapshot` 形状不变 (F68 web 读 it) — 来源从 statusline 改为 state.json,`raw` 保留 turn_count + status 供 F66 直读;`shutdown_session` 改读 state.json 提 pid → `libc::kill(pid, SIGTERM)`,ESRCH(已死)视为成功(幂等);`job_id` 为 `None` 的 legacy / codex handle 走 no-op warn 路径,不破 `ccteam session rm`。`SpawnOpts` 加 `role: String` 字段(claude 必填,codex 忽略 — F62 共用 schema);`SessionRecord` 加 `Option<String> job_id` serde-default 字段(老 state.json 兼容);`SessionHandle.job_id: Option<String>` 同样 serde-default + skip-serializing。删除 200+ LOC statusline 管道:`HookCommand::HarnessSnapshot` clap 变体 + `run_hook_harness_snapshot`、`--install-statusline-adapter` doctor 子命令 + `install_statusline_adapter`、`render_statusline_wrapper`、`find_latest_statusline_backup`、`STATUSLINE_MARKER_BEGIN` const;`statusline_install_test.rs` 整文件删。`crates/ccteam-core/Cargo.toml` 加 `libc = "0.2"` for SIGTERM。新 helper `state_json_path(job_id) -> PathBuf`(认 `CCTEAM_CLAUDE_JOBS_DIR` env override 测试 hermetic);新 `parse_cc_state_json(raw) -> Result<HarnessSnapshot>`(独立函数,挂在 `lib.rs::pub use harness::*`);新 env consts `CLAUDE_JOBS_DIR_ENV` / `CLAUDE_BIN_ENV`。**CodexAdapter 不动**:F62 已 ship 真实 tmux+codex 实现,本 PR 仅改 CC adapter,F62 测试零退步。**红线**:`ccteam-core/src/harness.rs::ClaudeCodeAdapter` 不再调 `tmux new-session` / `pipe-pane` / `capture-pane`(grep clean 在 CC adapter 部分);`progress.jsonl` 仍是 orchestrator 状态唯一 SoT,harness.rs 零 append 调用;`HarnessSnapshot` struct 形状不变,web 层 F68 才接新源。测试:`crates/ccteam-core/tests/harness_thin_test.rs` 新建 10 测试(t01 spawn_returns_job_id 通过 `$CCTEAM_CLAUDE_BIN` 指向 fake shell、t02 ingest_from_state_json 通过 `$CCTEAM_CLAUDE_JOBS_DIR` 指向 tempdir、t03 shutdown_sends_sigterm fork 子进程 sleep + 写假 state.json + 验证 SIGTERM + 幂等二次调用、t04 state_json_path_env_override、t05 thin_api_surface_present 编译级断言新公开 API、t06 spawn_includes_role 通过 `CCTEAM_TEST_ARGV_SINK` 抓 argv 验证 `--bg --agent <role>` 等 6 个 flag、t07 spawn_rejects_empty_role、t08 spawn_missing_job_id_fails_loud、t09 shutdown_without_job_id_is_noop、t10 harness_snapshot_shape_round_trip);`session_cli_test.rs::session_add_claude_spawns_bg_job_and_records_state` 改造(fake claude 印 `{"job_id":"job-fixture-1"}` 后 exit 0,断言 record.job_id == Some("job-fixture-1"))。
- **优先级**:**P0**(V0.4.0 harness 接口选型;F66 thin orchestrator + F68 web 适配都依赖本 PR 落地 state.json 源)。
- **来源**:`docs/v0-4-0/prd.md §F61 + §6.4` + `docs/v0-4-0/dev-plan.md §3`。

### F60 — Phase machinery removal(V0.4.0 PR #1;2026-05-14 加)

- **scope(净删除)**:
  - **删整个文件**:`crates/ccteam-core/src/phases.rs`(934 LOC)、
    `crates/ccteam-core/src/golden_rules.rs`(555 LOC)、
    `crates/ccteam-core/src/dag.rs`(217 LOC)、
    `crates/ccteam-core/src/subskill.rs`(522 LOC)。
  - **掏空保留桩**:`crates/ccteam-core/src/orchestrator.rs`
    (2713 → ~140 LOC):删 `decide_tick` / `decide_tick_from_events`、
    `TeamRuntime { templates, dag }`、`dispatch_phase_with_state`、
    `attachments_for_next_phase`、`handle_golden_rules_violation`、
    `process_project` / `process_meta_project` / `process_session_inbox`、
    全部 `PhaseState::{InFlight, DonePending, AutoLocked}` 处理分支;
    保留 `Orchestrator { paths, config }` + `new` + `paths` +
    `run_project`(`todo!("F66 thin orchestrator")`)+ `run`(同 stub)。
  - **trim**:
    - `crates/ccteam-core/src/state.rs`:`PhaseState` 仅留 `Idle` / `Done`
      (旧变体 deserialize alias 到 `Idle`);`current_phase` /
      `phase_history` / `last_event_type` / `auto_loop_cycle_count` 保留
      为 serde-compat 字段(`skip_serializing_if`),新写不带、老 state.json
      仍可读。
    - `crates/ccteam-core/src/templates.rs`:删 `TEAM_BUNDLES` /
      `PHASE_TEMPLATES` / `team_bundle` / `write_*_phase_templates*` /
      `write_all_global_team_templates`;保留 `PROJECT_SETTINGS_JSON` /
      `HELPER_TEMPLATES` / `render_project_settings` /
      `write_project_settings` / `write_global_helper_templates` /
      `EnabledPluginsSetting` / `SettingsEnv`(non-phase 项目 bootstrap)。
    - `crates/ccteam-core/src/progress.rs`:删 `build_phase_prompt` /
      `build_phase_prompt_for_template` /
      `build_phase_prompt_for_template_with_team` /
      `build_phase_prompt_with_attachments` /
      `synthesize_minimal_template` / `latest_terminal_event_for_phase`;
      保留 `append_event` / `last_event` / `read_all_events` /
      `is_idle` / `subagent_active` / `idle_aware_message`(channel 层 SoT)。
    - `crates/ccteam-core/src/team.rs`:删 `use crate::phases::GoldenRule`,
      内联 cmd-check 验证;`as_cmd_check_rules` 因无消费者一并删;
      legacy yaml shape 的 `Vec<GoldenRule>` 用 private `LegacyGoldenRule`
      shim 替换(deserialize-only,保 backwards-compat)。
    - `crates/ccteam-core/src/projects.rs`:删 `compute_enabled_plugins` /
      `load_phase_templates_for_bootstrap` / `setup_tool_surface` /
      `parse_subskill_subagent_name`;`bootstrap_project` 不再写 phase
      模板,`enabled_plugins` 一律 default(F66 重做)。
    - `crates/ccteam-core/src/team_factory.rs`:删 phase template parse
      smoke 测试 `init_phase_frontmatter_parses_back_into_phase_template`。
    - `crates/ccteam-core/src/memory_bridge.rs`:`tests::seed` 不再调
      `write_all_global_team_templates`,inline 写最小 team.yaml 三件套。
    - `crates/ccteam-core/src/actions.rs`:`resume` 删 `phase_history.push`
      paired "resumed" 标记逻辑;`PhaseHistoryEntry` import 一并删。
    - `crates/ccteam-cli/src/commands.rs`:`render_validate_team_report`
      / `run_phase_show` / `render_reset_shipped_teams_report` /
      `render_install_memory_bridge_report` / `render_tool_surface_report`
      五个 doctor 子命令 stub 化(返回 V0.4.0 F60 not-implemented message);
      `run_new` 删 shipped-seed 自愈;`stamp_project_team_kind` 给 dev /
      meta-agent 默认 `TeamKind::Workflow`(不再依赖 disk team.yaml)。
    - `crates/ccteam-cli/src/main.rs`:`ccteam start` 删 shipped-team
      self-heal。
    - `crates/ccteam-cli/src/mcp_serve.rs`:`tool_ls` 的 active_count 改
      `0`(F66 重算)、PhaseState 字符串 match arm 收敛到 idle / done。
  - **删测试**:phase 机制相关 integration test 整文件删除
    (`crates/ccteam-core/tests/`:`context_reset_test.rs` /
    `dispatch_test.rs` / `e2e_happy_path_test.rs` /
    `m1_dispatch_e2e_test.rs` / `m1_meta_dispatch_test.rs` /
    `m2_subskill_test.rs` / `m3_phase_done_pending_test.rs` /
    `m3_team_runtime_test.rs` / `orchestrator_test.rs` /
    `pending_inject_e2e_test.rs` / `phases_test.rs` /
    `silence_classifier_e2e_test.rs` / `state_machine_test.rs` /
    `team_resolver_project_layer_e2e_test.rs` / `templates_test.rs` /
    `tool_surface_e2e_test.rs`,`crates/ccteam-cli/tests/`:
    `m3_product_research_e2e_test.rs`)。
- **不在本 PR**:`workflow.yaml` schema + parser(F63)、artifact
  watcher(F64)、新 MCP 工具(F65)、薄 orchestrator 重建(F66)。
- **是否真 dev-specific**:**否——架构 pivot 净删除。** Claude Code 已具
  备 phase / plan 内置能力,phase 模板系统跟原生功能竞争,根本错误。
- **解耦方案(已 ship)**:净删除 ~3500 LOC,workspace 仍 cargo
  check + cargo test 全绿(测试数从 866 → 663,失败 0,删的是 phase
  机制专属测试)。F66 在新 `workflow.yaml` 拓扑上重建调度,**不** 把
  phase 模板再带回来。
- **优先级**:**P0**(架构 pivot 必经)。
- **来源**:`docs/v0-4-0/prd.md §F60` + §5 / `docs/v0-4-0/dev-plan.md §2`。

### F40 — `product-research` team 名冗长 + 领域名缺位(2026-05-09 加;**已修复:2026-05-09(V0.2.2 PR #6 alias 软迁移)**)

- **文件:行号**:`teams/product-research/team.yaml::name`(M3.4 起的字面值)
  + 全栈 callsite(`crates/ccteam-core/src/team.rs::TeamSpec` /
  `team_resolver.rs::resolve_team` / `templates.rs::TEAM_BUNDLES` /
  `memory_bridge.rs::lookup_bridge_template` /
  `crates/ccteam-cli/src/commands.rs::ensure_team_resolvable` 等)。
- **现状**:`product-research` 又长又冗(命令行 `ccteam new --team
  product-research "<brief>"`、`~/projects/product-research-<slug>/`
  目录前缀、`~/.claude/rules/ccteam-lessons-product-research.md` rules
  文件名),跟简短的 `dev` 对比读 / 写都繁;领域名 vs team 名混淆
  (`product-research` 既是技术 team 名又是领域描述,两者绑死)。
- **是否真 dev-specific**:**否——team 命名问题。**
- **解耦方案**:V0.2 PRD §5.4 的 `team.yaml::aliases` 方案拉回 V0.2.2 做。
  仓内 `teams/product-research/` → `teams/research/`(`git mv` 保 history);
  `team.yaml::name = research` + `aliases: [product-research]` + 多行
  `description` 字段载全称;`TeamSpec` 加 `pub aliases: Vec<String>` +
  validate;`resolve_team` / `team_bundle` / `Orchestrator::team_runtime` /
  `ensure_team_resolvable` 全 alias-aware(name miss → walk
  `aliases:`);老项目 `state.json::team = "product-research"` 字面 +
  `~/projects/product-research-*` 目录 + 老 rules 文件全不动;新项目走
  `state.team = "research"` + `~/projects/research-*`;`ccteam new --team
  product-research` stderr warn deprecated 但仍工作。
- **优先级**:**P2**(命名;不阻塞功能,但持续摩擦用户体验)。
- **来源**:`docs/v0-2-2/prd.md §9`(F40 全文,~110 行)。
- **2026-05-09 已修复(V0.2.2 PR #6)**:仓内 team 重命名 + alias 软迁移
  ship。530 tests 全绿(524 baseline + 6 新 alias resolution / round-trip
  / cli 测试);clippy 无新 warning。详 PR `v0-2-2-team-alias`。

### F33 — team factory phase scaffold body 含 bare protocol tokens(2026-05-08 加;**已修复:2026-05-08(V0.2.1 PR)**)

- **文件:行号**:`crates/ccteam-core/src/team_factory.rs` phase
  scaffold render(`init_phase_body_excludes_protocol_literals` test
  enforces colon-grammar `PHASE_DONE: <name>` / `ESCALATE:` 不出现);
  生成的 `01-intake.md` 末尾注释含 bare `PHASE_DONE` / `ESCALATE`
  tokens(无 colon)说明"正文不写协议关键字"。
- **现状**:validator 正确跳过(M0.18.5 residue check 抓的是
  `PHASE_DONE: <name>` colon-grammar);但 LLM 读到这段注释时可能误解
  bare token 为 signal。
- **是否真 dev-specific**:**否——team factory 模板措辞。**
- **解耦方案**:scaffold 注释改用 backticks 引号或别的指代
  (eg `"协议关键字(由 orchestrator inject prompt 注入)"`);或彻底
  从 body 删,只在用户指南文档讲。
- **优先级**:**P2**(cosmetic;validator 行为正确)。
- **来源**:V0.2 e2e Suite D D5 / `docs/v0-2/e2e-retro.md`。
- **2026-05-08 已修复(V0.2.1 PR)**:scaffold body 注释改为意图描述("阶段切换 / 升级信号由 orchestrator 在每次 phase prompt 注入时附带,不要在正文里复述"),不再出现协议关键字本身;`init_phase_body_excludes_protocol_literals` 测试加严:bare-token 形式(无 colon)也被检测。

### F80 — Phantom `agent_spawn` rows after daemon SIGKILL + cost-aggregator divergence(2026-05-16 加;**2026-05-16 V0.4.5 hotfix ship,close**)

- **文件:行号**:`crates/ccteam-core/src/claude_job.rs`(新建,~220 LOC,`JobLiveness` + `probe_job` + `probe_state_json` + `classify` 4 entry points + 8 单元测试);`crates/ccteam-core/src/lib.rs`(+1 `pub mod claude_job` + 4 `pub use`);`crates/ccteam-core/src/progress.rs`(`current_agent_sessions` 重构为 `current_agent_sessions_inner` 内核 + `current_agent_sessions_with_liveness<F>` 接受 closure 的新 sibling + `open_agent_spawns` triple 提取 helper;新写 agent_spawn 记 `job_id` 字段;文档说明 pure vs liveness-aware 双 API);`crates/ccteam-core/src/queries.rs`(`workflow_summary` 用 `current_agent_sessions_with_liveness(events, probe_job)` — 读侧 phantom 自动 demote);`crates/ccteam-core/src/orchestrator.rs`(`try_spawn_with_prompt` 写 `job_id` 进 `agent_spawn` event;`poll_completions` 加 stale-spawn cleanup pass:扫 `open_agent_spawns` 不在 in-memory `running` 内的 sid 全部 probe → terminal 合成 `agent_done {status: "killed"|"error"|...}`;新 `bump_project_state_cost` 把 `state.cost_used_usd` 与 `agent_done.cost_usd` 拉齐);`crates/ccteam-core/tests/progress_refactor_test.rs`(+6 测试:t13-t18,phantom drop / live keep / 全链 state.json E2E / legacy 无 job_id row / 显式 done 不重 probe / open_agent_spawns 三元组);`crates/ccteam-core/tests/orchestrator_thin_test.rs`(+4 测试:t33 phantom 清理写 `agent_done` / t34 in-memory-owned 跳过清理 / t35 `agent_spawn` 携带 job_id round-trip / t36 cleanup 同时 bump `state.cost_used_usd`);`crates/ccteam-web/web/src/index.css`(新 `.agent-active-dot` + `@keyframes agent-active-pulse` + `prefers-reduced-motion` opt-out);`crates/ccteam-web/web/src/pages/WorkflowView.tsx`(AgentCard 头加 pulsing dot `isActive = agent.running_count > 0`);`docs/interfaces.md §4`(`agent_spawn` 表格行加 `job_id` 选填字段;`agent_done` 表格行加 V0.4.5 F80 phantom cleanup 写入时机)。
- **现状(live host bug,2026-05-15 实测)**:`~/.ccteam/progress/dex-ui.jsonl` 含 4 个 explorer `agent_spawn` 但只 1 个真活;web UI 显 `running=4` `cost=$0.00`(成本字段从未写,因 `cost_usd` 只在 `agent_done` 里 emit,SIGKILL 死的 session 永不写)。pre-F80 `current_agent_sessions` 把任何 spawn 无 done 的 row 都算 `Running`,SIGKILL 死的 phantom session 永远计数。同时 `ccteam show <slug>` 走 `state.cost_used_usd`(hooks 累加),web 走 `agent_done.cost_usd` 汇总,两套数源永久 divergence。
- **是否真 dev-specific**:**否——orchestrator 业务状态层 bug + cost 数据源 divergence。** 所有 workflow team 都受影响。
- **解耦方案(已 ship)**:**(a) plumbing — job_id 上 progress event**:`orchestrator::try_spawn_with_prompt` 把 `HarnessAdapter::spawn_session` 返回的 `SessionHandle::job_id` 写进 `agent_spawn` 事件(Codex 行为 `null`,Claude 写 `"9432490e"` 形短 hash)。**(b) read-side liveness probe**:`claude_job::probe_job(Option<&str>)` 读 `harness::state_json_path(id)` → `JobLiveness::{Running, Terminal { status, cost_usd }}`。Terminal 触发条件:`job_id=None`(legacy 行)/ `state.json` 不存在 / 文件 unparseable / `firstTerminalAt` 非空(Claude Code 自家 terminal 时间戳)/ `state ∈ {done, completed, failed, crashed, stopped, error}`。`progress::current_agent_sessions_with_liveness<F>` 接 closure,默认 `current_agent_sessions` 仍 pure(老测试无副作用);`queries::workflow_summary` 调 with-liveness 版本注入 `probe_job` — 读侧 phantom 立即 demote(`Running` → `Done`/`Errored`),web UI 无需等 orchestrator 下一 tick。**(c) write-side cleanup**:`orchestrator::poll_completions` 加 stale-spawn pass:`progress::open_agent_spawns(events)` 提取所有未关 spawn 的 `(sid, job_id, role)` triples → 排除 in-memory `running` 已 own 的 sid(避免与真正在 own 的 session race)→ 剩余 probe → Terminal verdict 合成一条 `agent_done` event(`status: "killed"` 用于 missing state.json,`status: "error"` 用于 failed/crashed,`status: "completed"` 用于 done)。SoT 还是 progress.jsonl;daemon 重启后第一个 tick 自动清理上轮 SIGKILL 留下的 phantom。**(d) cost-aggregator alignment**:`orchestrator::bump_project_state_cost(slug, delta)` 在每条 `agent_done` 写后跑(包括清理路径合成的 done),原子 `load → += delta → save` ProjectState;`ccteam show <slug>` 与 web UI 同源(progress.jsonl 是 SoT,state.cost_used_usd 是其镜像)。**(e) UI 活动指示**:WorkflowView AgentCard 顶 `agent.running_count > 0` 时显 7px 脉动绿点(slow 1.8s ease-in-out infinite + box-shadow 6px,prefers-reduced-motion 静态 0.85 opacity),操作员一眼区分 "真在跑" vs "stale count"。**红线**:`claude_job.rs` 零 mutation,只读;清理只写 `agent_done`(progress.jsonl SoT 第 8 类事件,不新增 event 种类);零 `--kill` 行为(只写记账);`queries.rs` 仍 read-only(liveness probe 只读 state.json,从不写)。测试 18 + 36 = 54 项过(progress_refactor_test 18 / orchestrator_thin_test 36 / claude_job 单元 8 / vitest 20);workspace baseline 707/0 → 717/0(+10 新测试 — t13-t18 + t33-t36 + 8 unit)。
- **优先级**:**P0**(live ship blocker — web UI 显错数据)。
- **来源**:live host operator-reported bug, 2026-05-15。

---

## N/A — 已是领域无关(显式排除,避免误判)

### F14 — `build_phase_prompt` 路径前缀写死 `.ccteam/phases/`

- **文件:行号**:`crates/ccteam-core/src/progress.rs:134-138`
  ```rust
  pub fn build_phase_prompt(phase: &str) -> String {
      format!("请按 @.ccteam/phases/{phase}.md 完成本阶段。完成后写 .ccteam/{phase}-report.md...")
  }
  ```
- **现状**:phase prompt 引用路径写死 `.ccteam/phases/<phase>.md` 与
  `.ccteam/<phase>-report.md`。
- **是否真 dev-specific**:**否——纯 mechanism。** prompt 中文措辞"完成本
  阶段"对所有团队都中性,无问题;路径布局是 ccteam 共用约定。
- **解耦方案**:不需要改。
- **优先级**:**N/A**(已是领域无关)。
- **注**:列入审计是为了显式排除——读 strategic doc §1.5 时容易误判这条该
  改。

---

## 修复顺序建议(M3.1 PR 拆分)

> **2026-05-06 状态更新**:M3 已 ship,以下大部分 PR 已落地。保留历史记录。

按依赖关系排:

1. **PR A — phase 模板 schema 扩展(F1 字段层)** ✅ M3.1 ship
   - phase YAML 加 `auto_loop` / `auto_loop_max_iterations` /
     `completion_signal` 字段
   - phase 模板都更新(`fix.md` 加 `auto_loop: true` /
     `completion_signal: TESTS_GREEN`)
   - 测试覆盖
2. **PR B — orchestrator DAG 提取(F2 + F3 + F4 + F1 触发逻辑)** ✅ M3.1 ship(F2/F3/F4 部分 — F1 触发逻辑仍未切)
   - 删 `M0_PHASE_DAG` / `FIRST_PHASE` ✅
   - 从 phase 模板推断 DAG ✅(`crates/ccteam-core/src/dag.rs`)
   - `is_terminal` 改 DAG 终点判断 ✅
   - `if phase == FIX_PHASE_NAME` 改 `if template.auto_loop` ❌ 仍未做(F1 剩余 P0)
   - lib.rs `pub use` 清理 ✅
3. **PR C — fix_loop → auto_loop 重命名(F5 + F6 + F7 + F18)** ❌ 未做
   - 一次性重命名,保留 serde 兼容
4. **PR D — `--team` CLI 入口(F12 + F13)** ✅ M3.3 ship
5. **PR E — bootstrap 模板 team 化(F9 + F10 + F11 + F16 + F17)** ✅ M3.4 ship(F11 dev 仍裸 `phases/`,F17 测试 fixture 仍 dev,均不阻塞)
6. **PR F — `collect_artifacts` 自动扫(F8)** ❌ 仍未做(P1)
   - 改成扫 `.ccteam/*.md`
7. ~~**PR G — 文档解耦(F19)**~~ N/A — 重新评估,M3 后维持仓库 dev-first 视角合理(见 F19 状态注解)
8. **延后**:
   - F15(M1+ 引入 `block-push` 时一并做)
   - ~~F21~~(已修复 @a5fb21d)
   - ~~F20~~(已修复:M3.1 schema + M4.1 phase 消费)
   - F23(M4.4 spike §4 deferred,F22 修复后已解锁,等谁跑一次)

每个 PR 必须有 dev pipeline happy-path 回归测试通过。

---

## 维护

本文档与 strategic doc 同步:发现新的耦合点 → 加 F<N>;已修复的 → 不删,
标 `**已修复:<日期> @ <commit>**`。审计是历史记录,不是 todo list。
