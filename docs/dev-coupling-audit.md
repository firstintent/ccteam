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

21 条发现(2026-05-05 加 F21、升级 F20 P1→P0,共增 1 条),分布:

| 优先级 | 数量 | 编号 |
|---|---|---|
| **P0 阻塞泛化** | 7 | F1, F2, F3, F4, F12, F13, F20(2026-05-05 升级) |
| **P1 该做但可后置** | 10 | F5, F6, F7, F8, F9, F10, F11, F15, F19, F21 |
| **P2 边角** | 3 | F16, F17, F18 |
| **N/A 已是领域无关** | 1 | F14 |

**P0 关键路径**:F1(`auto_loop` 字段)+ F2(DAG 由 phase 模板推断)+ F3
(`FIRST_PHASE` 改 DAG entry node)+ F4(`is_terminal` 改 DAG 终点判断)+
F12(CLI `--team`)+ F13(`state.json.team` 字段)。这 6 条解耦后 ccteam-
core 才能跑非 dev 团队。

**元发现**:`pub use ... M0_PHASE_DAG, FIRST_PHASE`(`crates/ccteam-core/src/
lib.rs:21`)把 dev 假设暴露到 lib 接口表面——M3.1 是一次 lib API breaking
change。按 CLAUDE.md §五.3 "不写 backwards-compat shim",直接换。

**对 §A 的反馈**:审计过程中没有发现需要修订 strategic doc §1 责任分界表
或 §2 团队扩展契约的位置——所有发现都能映射到现有分类。这是抽象切对的
好信号。

---

## P0 — 阻塞泛化

### F1 — `FIX_PHASE_NAME` / `FIX_LOOP_MAX_ITERATIONS` 字符串耦合 fix-loop 触发

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

### F2 — `M0_PHASE_DAG` 硬编码 dev 流程

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

### F3 — `FIRST_PHASE = "plan-eng"` 硬编码

- **文件:行号**:`crates/ccteam-core/src/orchestrator.rs:46`
- **现状**:`pub const FIRST_PHASE: &str = "plan-eng";`,`decide_tick_from_events`
  在 `current_phase.is_empty()` 时回落到这个常量。
- **是否真 dev-specific**:**是。** research 团队第一个 phase 是 `00-topic` /
  `00-topic-clarify` 之类的 entry phase,与 plan-eng 无关。
- **解耦方案**:从 F2 推出的 DAG 取 `dag.entry_node()`(排序后第一个 phase 的
  name)作为 `current_phase` 兜底。
- **优先级**:**P0**——同 F2 一并清。

### F4 — `is_terminal()` 字符串匹配 `"ship"` 终态

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

### F12 — `ccteam new` CLI 缺 `--team` 参数

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

### F13 — `state.json` 缺 `team` 字段

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

---

## P1 — 该做但可后置

### F5 — `fix_loop` 模块 / 类型名假设了"fix"语义

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

### F6 — `PhaseState::FixLocked` 枚举值名假设 fix

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

### F7 — `state.fix_cycle_count` 字段名假设 fix

- **文件:行号**:`crates/ccteam-core/src/state.rs:65`
- **现状**:`pub fix_cycle_count: u32`
- **是否真 dev-specific**:**命名上是。** 同 F6,字段语义是"通用 auto-loop
  计数"。
- **解耦方案**:重命名 `auto_loop_cycle_count`(serde rename 兼容旧文件);或
  挪进 `phase_state` 内嵌(只在 AutoLocked 时有效)更优,但变更面更大。
- **优先级**:**P1**——同 F5/F6 一并改。

### F8 — `collect_artifacts` 硬编码 dev artifact 列表

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

### F9 — `bootstrap_project` 写死的 CLAUDE.md 内容含 dev 措辞

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

### F10 — `PHASE_TEMPLATES` 编译期 include dev phase MD

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

### F11 — phase 目录与文件命名缺 team scope

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

### F19 — CLAUDE.md(顶层)与 docs/* 把 ccteam 描述为"开发团队的编排层"

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

### F20 — 跨项目记忆 schema 假设 dev 字段

- **文件:行号**:`docs/tech-design.md §3.7` "关键字段:tech stack、踩过的坑、
  成功的设计选择、不要再做的事";`docs/development-plan.md §5 M3.1` "输出固
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
> 后续 RAG 索引重建。`development-plan.md` 已 reorder M3 ↔ M4(团队抽象前
> 置),F20 现在阻塞新 M4 启动 —— 升级为 P0。

### F21 — `stall_warn_minutes` phase YAML 字段已 spec 但 orchestrator 未读取

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

---

## P2 — 边角

### F16 — `PHASE_TEMPLATES` 中 plan-ceo / 07-review / 08-score 缺位但 fix/test/ship 在位

- **文件:行号**:`crates/ccteam-core/src/templates.rs:24-31` 仅 6 个;
  `interfaces.md §5.2` 列了 9 个完整 phase
- **现状**:M0 只交付 6 个 dev phase(plan-eng / implement / test-author /
  test-run / fix / ship),plan-ceo / review / score 留给 M1+/M2+/M4。
- **是否真 dev-specific**:**是**(在交付的 phase 都是 dev fill)。
- **解耦方案**:同 F10——团队配置决定该 binary 内嵌哪些 phase。
- **优先级**:**P2**(随 F10 一并做)。

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

### F18 — `fix_loop_writes_with_ccteam_dir_already_present` 等测试名假设 fix

- **文件:行号**:`crates/ccteam-hooks/tests/fix_loop_test.rs`(整个文件)
- **现状**:测试模块、文件名、测试函数名都用 `fix_loop`。
- **是否真 dev-specific**:**命名上**——同 F5/F6/F7,机制是 auto-loop。
- **解耦方案**:F5 重命名时一并改,`fix_loop_test.rs` → `auto_loop_test.rs`。
- **优先级**:**P2**(随 F5 一并改)。

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

按依赖关系排:

1. **PR A — phase 模板 schema 扩展(F1 字段层)**
   - phase YAML 加 `auto_loop` / `auto_loop_max_iterations` /
     `completion_signal` 字段
   - phase 模板都更新(`fix.md` 加 `auto_loop: true` /
     `completion_signal: TESTS_GREEN`)
   - 测试覆盖
2. **PR B — orchestrator DAG 提取(F2 + F3 + F4 + F1 触发逻辑)**
   - 删 `M0_PHASE_DAG` / `FIRST_PHASE`
   - 从 phase 模板推断 DAG
   - `is_terminal` 改 DAG 终点判断
   - `if phase == FIX_PHASE_NAME` 改 `if template.auto_loop`
   - lib.rs `pub use` 清理(breaking change 已说明)
3. **PR C — fix_loop → auto_loop 重命名(F5 + F6 + F7 + F18)**
   - 一次性重命名,保留 serde 兼容
4. **PR D — `--team` CLI 入口(F12 + F13)**
   - `Command::New` 加 `--team`
   - `ProjectState.team` 字段
   - `bootstrap_project` 受 team 参数
5. **PR E — bootstrap 模板 team 化(F9 + F10 + F11 + F16 + F17)**
   - CLAUDE.md 模板按 team
   - `PHASE_TEMPLATES` 按 team
   - `phases/` → `phases-dev/`
   - 测试目录 `tests/team-dev/`
6. **PR F — `collect_artifacts` 自动扫(F8)**
   - 改成扫 `.ccteam/*.md`
7. **PR G — 文档解耦(F19)**
   - `docs/` 顶部加免责
   - 长期 `docs/core/` + `docs/teams/<team>/` 整理(M3.4)
8. **延后**:
   - F15(M1+ 引入 `block-push` 时一并做)
   - F21(`stall_warn_minutes` 已 spec 未实现,M0.5.3 顺手或 M1 cross-cutting watcher 上线前)
   - F20(原 P1 已升 P0,M3 完成 retro_schema 数据形式后,M4.1 retro phase 实现一并处理)

每个 PR 必须有 dev pipeline happy-path 回归测试通过。

---

## 维护

本文档与 strategic doc 同步:发现新的耦合点 → 加 F<N>;已修复的 → 不删,
标 `**已修复:<日期> @ <commit>**`。审计是历史记录,不是 todo list。
