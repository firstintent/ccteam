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
**2026-05-10 V0.3 doc-only kickoff**:加 F45 P1(write helper promote ccteam-cli → ccteam-core::actions,M5.0 关键解耦),实施在 V0.3 PR #1 / #4);**2026-05-10 V0.3 PR #1 ship**:F45 promote 部分修复(actions 模块 + mcp_serve wrapper 透传 + dep_graph 自检测试落地),仍待 M5.3 写动作 endpoint 消费才整体 close;**2026-05-10 V0.3 PR #4 ship**:F45 **整体 close**(M5.3 写动作 endpoint + token auth + URL-shim cookie + path-traversal 守卫全部 ship);**2026-05-10 V0.3.1 doc-only kickoff**:加 F46-F51 六条(战略 pivot:flex team kind + adhoc multi-session + HarnessAdapter trait + CodexAdapter stub + web flex 适配 + ship gate),待 V0.3.1 ship 后填 close 状态;分布:

| 优先级 | 数量 | 编号 |
|---|---|---|
| **P0 阻塞泛化(剩余)** | 0 | — |
| **P1 该做但可后置(剩余)** | 2 | F15(M1+ block-push 时做)、F23(conditional;待 spike 重跑) |
| **P2 边角(剩余)** | 1 | F17 |
| **V0.3.1 待 ship**(2026-05-10 加) | 6 | F46(P0)、F47(P1)、F48(P0)、F49(P0)、F50(P1)、F51(P0)|
| **N/A 已是领域无关** | 2 | F14, F19(M3 docs sweep 后)|
| **已修复** | 38 | F1 / F5 / F6 / F7 / F18(2026-05-07 rename PR;F1 触发逻辑实际早 M3.1 已切到 template.auto_loop,本 PR 完成命名层 sweep)、F2 / F3 / F4(M3.1 dag.rs)、F8(2026-05-07 directory scan)、F9 / F10 / F11(M3.4 team-aware bootstrap;F11 dev 仍裸 `phases/` 但非阻塞)、F12 / F13(M3.3 `--team` CLI + `state.team`)、F16(M3.4 phase 模板 team 化)、F20(M3.1+M3.4 retro_schema 数据形式 + product-research 填字段 + M4.1 phase 消费)、F21(@a5fb21d)、F22(PR #12)、**F24 / F25(2026-05-08 M0.23 PR)**、**F26 / F27 / F28 / F29 / F30 / F31 / F32 / F33(2026-05-08 V0.2.1 patch)**、**F34 / F35 / F36 / F37 / F38 / F39 / F40(2026-05-09 V0.2.2 patch — 7 finding 跨 7 PR)**、**F41 / F42 / F43(2026-05-09 V0.2.2 e2e retro patch)** |

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

### F46 — HarnessAdapter trait 缺位,Claude Code statusline 结构化数据丢失(2026-05-10 加;**待 V0.3.1 PR #1 ship**)

- **文件:行号**:`crates/ccteam-core/src/harness.rs`(新建);`crates/ccteam-web/src/routes/sse.rs`(扩 harness SSE);`~/.claude/statusline-command.sh` wrapper 落 `~/.ccteam/harness/<slug>-<sid>.json`。
- **现状**:V0.3 web UI dashboard 的数据源 = `progress.jsonl` + tmux pane 截图(F38);Claude Code statusline + subagent 面板里**有大量结构化数据**(模型名 / context% / token / cost / rate-limit / subagent),V0.3 dashboard 拿不到结构化版本(只截图视觉版),F35 enriched outbox 也只 ASCII pane_tail。V0.3.1 引入 Codex 后,session 也不再只有一种 harness — 需要 trait 抽象统一。
- **是否真 dev-specific**:**否——presentation 信息抽象层。** 跟 phase / team 无关,所有 harness 都需要(claude / codex / 未来 cline / aider / 等)。
- **解耦方案**:`crates/ccteam-core/src/harness.rs` 加 `HarnessAdapter` trait + `HarnessSnapshot` / `SubagentState` / `SpawnOpts` / `SessionHandle` / `HarnessError` 数据结构;`ClaudeCodeAdapter` 完整实现 trait 全部方法;`ccteam doctor --install-statusline-adapter` 安装 wrapper(marker `# ccteam-managed:statusline begin/end` 保护用户手改 + 原文件 backup .bak-<utc-ts>);`~/.ccteam/harness/<slug>-<sid>.json` 协议(stdin JSON 全覆盖,delta archive V0.4 deferred);web SSE `GET /sse/harness/<slug>` + `/sse/harness/<slug>/<sid>`。**红线**:harness snapshot 只是 presentation,**不参与 orchestrator 状态决策**(progress.jsonl 仍 SoT)。
- **优先级**:**P0**(V0.3.1 foundation;F47/F49/F50 都依赖 trait shape)。
- **来源**:`docs/v0-3-1/prd.md §3`;研究 doc `docs/research/v0-3-1-harness-adapter-plan.md`(V0.3 ship 前临时记录,本 finding 是其正式版);用户 Telegram 2026-05-10 message 311。

### F47 — Codex CLI 缺前向兼容 trait stub,V0.3.2 实现无地方接入(2026-05-10 加;**待 V0.3.1 PR #2 ship**)

- **文件:行号**:`crates/ccteam-core/src/harness.rs`(F46 后扩 `CodexAdapter`);`crates/ccteam-core/src/team.rs::TeamSpec`(加 `sessions: Vec<DefaultSessionSpec>` 字段);`crates/ccteam-cli/src/commands.rs::run_session_*`(`--harness=codex` flag);`run_doctor` 加 codex 检测段。
- **现状**:V0.3.1 不实现 Codex 完整支持(`docs/research/ccteam-codex-integration.md` M2-M5 路线 ~月级工程量),但 CLI 与 schema 需要现在就接受 `harness: codex` 让用户提前声明意图,V0.3.2 落 `CodexAdapter` 实现时不破已 ship 代码。
- **是否真 dev-specific**:**否——前向兼容 stub。**
- **解耦方案**:`CodexAdapter` 全 stub 实现 trait,`spawn_session` / `ingest_snapshot` / `shutdown_session` 全返 `Err(HarnessError::NotImplemented { harness: "codex", reason: "Codex adapter is trait-stub in V0.3.1; full implementation tracked in docs/v0-3-1/prd.md §F47, deferred to V0.3.2+. Use --harness=claude or wait." })`(reason 是 `&'static str`,无 alloc);`team.yaml::sessions[]` schema(`harness: claude | codex`,serde 默认 `claude`);`ccteam session add --harness codex` 接受 flag,执行返友好 error + exit 1;`ccteam doctor` 检测 `which codex` 输出 informational(不 fail)。
- **优先级**:**P1**(forward-compat 接口,本 PR 不阻塞用户跑 claude harness)。
- **来源**:`docs/v0-3-1/prd.md §4` + `docs/research/ccteam-codex-integration.md` M0-M1 路线 + 用户 Telegram 2026-05-10 message 313 决策"V0.3.1 落 stub,V0.3.2 落 impl"。

### F48 — `kind: flex` team kind 缺位,phase 编排是 ccteam 唯一工作姿态(2026-05-10 加;**待 V0.3.1 PR #3 ship**)

- **文件:行号**:`crates/ccteam-core/src/team.rs::TeamSpec`(加 `kind: TeamKind` 字段);`crates/ccteam-core/src/orchestrator.rs::TeamRuntime`(加 `should_run_auto_loop` / `should_inject_phase` / `should_check_golden_rules` helpers);`crates/ccteam-core/src/team_factory.rs::init_team_staging`(`--kind=flex` 跳过 phase scaffold)。
- **现状**:V0.1/V0.2/V0.3 ccteam 团队都是 phase-driven(workflow 暗含),没有"空 phase / 用户原生姿态驱动 session" 这种姿态。V0.3.1 战略 pivot 把 ccteam 扩展为 session farm,需要支持用户在 session 里自由跑(无 phase 注入 / 无 auto_loop / 无 golden_rules),ccteam 只观测 + 记录 + 提供控制面。
- **是否真 dev-specific**:**否——team kind 抽象。**
- **解耦方案**:`team.yaml::kind` 字段,`TeamKind { Workflow, MultiWorkflow, Flex }`,`#[serde(default)]` 保 V0.1/V0.2/V0.3 yaml parse 不变(默认 `Workflow`)。`kind: flex` 与 `parallelism`(phase 级字段)正交 — flex 团队无 phase 所以 `parallelism` 不适用。`TeamSpec::validate` 拒绝非法组合(flex + golden_rules / escalate_grammar_extensions / 非空 phase_dir)。orchestrator behavior gating 三个 helper(`should_run_auto_loop` 等)按 kind 返 bool;**flex 团队 silence_classifier / cost watcher / hooks / progress.jsonl / 跨项目 memory bridge 仍跑**(observability 全保留)。team factory `--kind=flex` scaffold 跳过 phase markdown。
- **优先级**:**P0**(V0.3.1 战略 pivot 核心;F49 multi-session 在此基础上)。
- **来源**:`docs/v0-3-1/prd.md §5`;用户 Telegram 2026-05-10 message 311 原话:"team 工厂创建出空的 phases 团队...用户原始用 claude code 方式来完成"。

### F49 — Adhoc multi-session 缺位,单项目无法托管 N 个不同 harness session(2026-05-10 加;**待 V0.3.1 PR #4 ship**)

- **文件:行号**:`crates/ccteam-cli/src/commands.rs::run_session_{add,ls,attach,rm}`(新建);`crates/ccteam-core/src/state.rs::ProjectState`(扩 `sessions: BTreeMap<sid, SessionRecord>` + `next_sid_seq: BTreeMap<harness, u64>` 字段);`crates/ccteam-hooks/src/progress.rs`(`progress.jsonl` 路径解析按 `state::team_kind` 分流到 `<slug>/<sid>.jsonl` 子目录 vs flat `<slug>.jsonl`)。
- **现状**:V0.3 ccteam 项目 = 单 tmux session(except `parallelism: multi_session` 的 phase 级 fan-out 拓扑)。flex 团队需要 adhoc:用户起项目后任意时刻 `ccteam session add` 起新 session,删 session,attach 任一 session,混合 Claude+Codex(支持 cross-review pattern)。multi_session 的 master + 预定义 sub-module 是 phase 级、代码维度,**不**适合 adhoc + 进程维度。
- **是否真 dev-specific**:**否——session 维度并行机制。**
- **解耦方案**:**新轻量 session 注册**(不复用 multi_session 拓扑)— `~/projects/<team>-<slug>/.ccteam/sessions/<sid>/` 子目录,master `state.json::sessions{sid: {harness, tmux_session, started_at, pid}}` + `next_sid_seq{harness: u64}` 字段(serde default 空 BTreeMap 保 V0.3 单 session 项目兼容);sid 格式 `<harness>-<n>`,n 单调递增**删后不复用**;tmux 命名 `ccteam-<slug>-<sid>`(workflow / multi_workflow 项目仍 `ccteam-<slug>` 不变);`progress.jsonl` flex 项目走 `~/.ccteam/progress/<slug>/<sid>.jsonl`,workflow 项目仍 `<slug>.jsonl` flat;`ccteam session {add,ls,attach,rm}` CLI;**`rm` 是唯一显式用户授权 kill 路径**(CLAUDE.md §三 红线)。master state.json atomic write + retry-on-conflict 应对并发 add race。
- **优先级**:**P0**(V0.3.1 战略 pivot 核心 — flex 团队的实际用法)。
- **来源**:`docs/v0-3-1/prd.md §6`;用户 Telegram 2026-05-10 message 312"多 session 拉前 V0.3.1"决策。

### F50 — Web 层假设单 session 项目,flex 团队无展示路径(2026-05-10 加;**待 V0.3.1 PR #5 ship**)

- **文件:行号**:`crates/ccteam-web/templates/dashboard.html`(加 `Kind` 列);`crates/ccteam-web/templates/project.html`(flex 项目走 N session cards 模板分支);新模板 `templates/session.html` + handler `routes/session.rs`;`crates/ccteam-web/src/routes/sse.rs`(扩 `/sse/project/<slug>/<sid>` server-side filter);`crates/ccteam-core/src/screenshot.rs::render_screenshot`(签名加 `sid: Option<&str>`)。
- **现状**:V0.3 web UI(M5.0-M5.4 ship)假设单 session 项目;V0.3.1 引入 flex + adhoc multi-session 后,dashboard 需要 `Kind` 列,project 详情页对 flex 走 N session cards,需要新页 `/session/<slug>/<sid>`,SSE 需要 sid filter,screenshot 需要扩 `<slug>-<sid>.png`。
- **是否真 dev-specific**:**否——展示层适配。**
- **解耦方案**:dashboard 加 `Kind` 列(workflow / multi_workflow / flex);`Phase` 列对 flex 项目渲染 `—`;flex 项目 detail 页走 N session cards 分支(harness badge 蓝/绿,缩略截图,Detail link);workflow / multi_workflow 项目 detail 完全不变(回归保证);新页 `/session/<slug>/<sid>` 渲染 header + per-session events / harness panel / write actions sidebar;`/sse/project/<slug>/<sid>` server-side `msg.sid == <sid>` 过滤(`EventMsg` 加 `sid: Option<String>`);`/sse/harness/<slug>/<sid>` 推 harness_snapshot;`/screenshot/<slug>-<sid>.png` 路由 — `render_screenshot(slug, Some(sid), opts)`;`/screenshot/<slug>.png` workflow 项目保留(V0.3 兼容)。
- **优先级**:**P1**(展示层 — 用户首屏看到的 V0.3.1 价值,但不阻塞 backend ship)。
- **来源**:`docs/v0-3-1/prd.md §7`;用户 Telegram 2026-05-10 message 315 dashboard 决策。

### F51 — V0.3.1 ship gate(2026-05-10 加;**待 V0.3.1 PR #6 ship**)

- **文件:行号**:`Cargo.toml`(workspace.package.version 0.3.0 → 0.3.1);`CLAUDE.md` §一 baseline 表格 + §六 易踩坑;`docs/v0-3-1/e2e-retro.md`(新建);`docs/v0-2/README.md`(V0.3 pointer 更新);`docs/dev-coupling-audit.md` F46-F51 close 标记;`crates/ccteam-web/tests/flex_e2e_test.rs`(新建)。
- **现状**:V0.3.1 patch round(F46-F50)ship 后需要正式 ship gate。V0.2.2 政策:每 minor / patch release 必须 bump workspace.version + commit subject `vX.Y.Z:` 前缀。
- **是否真 dev-specific**:**否——chore + ship gate。**
- **解耦方案**:flex_e2e_test.rs 跑端到端(reqwest server happy path + codex error path);`Cargo.toml::workspace.package.version` `"0.3.0"` → `"0.3.1"`;CLAUDE.md §一 baseline 回填(workspace.version + 实测测试数 + V0.3.1 milestone 行);CLAUDE.md §六 加 V0.3 → V0.3.1 升级注;`docs/v0-3-1/e2e-retro.md` 4-suite 跨 flex 多 session / harness adapter / web UI / codex stub;`docs/v0-2/README.md` 更新 V0.3 起始 pointer 为 "已 ship V0.3 + V0.3.1";dev-coupling-audit.md F46-F51 close 标记;tech-design.md / interfaces.md 增量段终稿。
- **优先级**:**P0**(ship gate)。
- **来源**:`docs/v0-3-1/prd.md §8` / §12 / §13。

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
