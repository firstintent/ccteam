# V0.2.2 开发计划

> Patch 版本实施计划。按依赖顺序拆 7 个 PR(每 PR 对一 finding 或一组同源 finding +
> 一个 chore PR 收尾)。配套 worktree 分支 + subagent briefing 模板;每 PR 一份
> briefing 让 worktree subagent 拿到模板 + 进 worktree 即可开干。
>
> 配套文档:
> - 需求决策:`docs/v0-2-2/prd.md`(13 节,1300 行)
> - 用户反馈源:`docs/v0-2-2/feedback.md`
> - 文档索引:`docs/v0-2-2/README.md`
>
> base = `origin/main` `170f5a8`(V0.2.1 ship);测试 baseline = **511/0**。
>
> 跟 V0.2 dev-plan 不同点:V0.2.2 是 patch,**PR-based 而非 milestone-based**(7 PR
> 各对一 finding,milestone 即 PR)。CLAUDE.md §五 已加 patch 开发流程小节(doc-first
> → worktree-per-fix → PR → main review → fix → merge → cargo bump)。

---

## 1. PR 总览

| # | finding | branch | 工程量估 | 主要前置依赖 |
|---|---|---|---|---|
| **PR #1** | F39 cct convention sweep | `v0-2-2-cct-rename` | ~250 LoC + ~50 测试,~2 天 | 无(机械 rename,先 merge 立约定) |
| **PR #2** | F34 slug 四层 + F37 meta-agent 决策树 | `v0-2-2-meta-agent-and-slug` | ~400 LoC + ~30 测试 + 1 新 skill,~3-4 天 | **PR #1**(消费 `skills/` 顶层目录 + cct 命名)|
| **PR #3** | F35 silence classifier + capture-pane 入 outbox | `v0-2-2-silence-classifier` | ~350 LoC + ~25 测试,~3 天 | 无(`tmux.rs` helper 抽离独立)|
| **PR #4** | F36 subagent guard(defer until SubagentStop)| `v0-2-2-subagent-guard` | ~200 LoC + ~15 测试,~2 天 | 软依赖 PR #3(共享 enriched outbox classification 字段)|
| **PR #5** | F38 终端截图 PNG(vt100 + imageproc DIY) | `v0-2-2-screenshot` | ~250 LoC + ~150 KB vendored TTF + ~20 测试,~3 天 | 软依赖 PR #3(outbox 加 `screenshot_path` 字段)|
| **PR #6** | F40 team alias(`product-research` → `research`) | `v0-2-2-team-alias` | ~150 LoC + ~10 测试,~1.5 天 | 无 |
| **PR #7** | chore:workspace.version + dev-flow + e2e + retro | `v0-2-2-chore` | ~50 LoC + 文档,~1 天 | 全部 PR merge 后,V0.2.2 ship gate |

**总计**:~1.65 kLOC + ~150 测试,~15 天(单人 5 天/周即 3 周)。

### 依赖图

```
PR #1 (F39 cct rename) — 必先 merge,立约定
   ↓
PR #2 (F34 + F37) — 在新 skills/ 目录 + cct 命名上 follow-up

PR #3 (F35 silence classifier) ─┐
   ↓ (outbox schema 共享)        │
PR #4 (F36 subagent guard) ──────┤  (#3 / #4 / #5 跟 #1 / #2 解耦,可平行)
PR #5 (F38 screenshot) ──────────┘   (#5 rebase 到 #3 后加 screenshot_path 字段)

PR #6 (F40 team alias) — 独立面,跟谁都不撞

PR #7 (chore ship gate) — 最后,workspace.version bump + retro
```

并行机会:**PR #3 / #4 / #5 / #6 同时起 4 个 worktree**(独立编辑面);#1 / #2
是串行 critical path。

---

## 2. PR #1 — F39 cct convention sweep

> **目标**:三对象同步重命名(binary `ccteam` → `cct`、自带 skill `ccteam-*` →
> `cct-*`、skill 迁顶层 `skills/` 目录)+ V0.1/V0.2 用户升级迁移。机械 rename,
> 先 merge 立约定,后续 PR follow-up。

**关联 PRD**:§8(F39 全文)+ §13(CLAUDE.md dev-flow 段)

### 任务

- [ ] **#1.1** binary rename
  - `crates/ccteam-cli/Cargo.toml::[[bin]] name` `"ccteam"` → `"cct"`
  - Rust 内 `current_ccteam_bin()` 改 `current_cct_bin()`(全 callsite,grep `git grep -nE 'current_ccteam_bin'`)
  - `cargo install --path crates/ccteam-cli --force` 后产出 `~/.cargo/bin/cct` 在 PATH 可调
- [ ] **#1.2** 顶层 `skills/` 目录建立 + 两 skill 迁移
  - `mkdir skills/`(repo 根)
  - `git mv crates/ccteam-core/src/templates/ccteam_control_skill.md skills/cct-control/SKILL.md`
  - `git mv crates/ccteam-core/src/templates/ccteam_team_author_skill.md skills/cct-team-author/SKILL.md`
  - skill body 内 frontmatter `name:` 字段同步改 cct-* 命名
  - **PR #2 留位**:`skills/cct-project-creator/` 不在本 PR 创建(F34 PR 加)
- [ ] **#1.3** Rust 常量 / 函数名同步 rename
  - `crates/ccteam-core/src/skill.rs::CCTEAM_CONTROL_SKILL_MD` → `CCT_CONTROL_SKILL_MD`
  - `CCTEAM_TEAM_AUTHOR_SKILL_MD` → `CCT_TEAM_AUTHOR_SKILL_MD`
  - 跨目录 `include_str!`:`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/cct-control/SKILL.md"))`
  - `install_ccteam_control_skill` → `install_cct_control_skill`(+ team-author 同)
- [ ] **#1.4** settings.json hook 模板用占位符 `{{CCT_BIN}}`
  - `crates/ccteam-core/src/templates/settings.json` 全 hook command 用占位符
  - doctor 安装时 `current_exe()` 替换为绝对路径(已支持,只是名字变 cct)
- [ ] **#1.5** shell-out callsite + 文档 sweep
  - `git grep -nE '"ccteam"' crates/`(rust 字面 → 改 `"cct"`)
  - `git grep -nE '\bccteam (new|ls|show|attach|hook|doctor|watchdog|team)\b' README.md docs/tech-design.md docs/interfaces.md docs/requirements.md CLAUDE.md`
  - 全 forward-looking 文档 `ccteam <cmd>` → `cct <cmd>`;**`docs/v0-1/` `docs/v0-2/` 不动**(历史归档)
- [ ] **#1.6** V0.1/V0.2 用户升级迁移(`cct doctor` 自动跑)
  - 检测 `~/.cargo/bin/ccteam` 存在 → warn "old binary detected; safe to rm"(不主动删)
  - `~/projects/<slug>/.claude/settings.json` hook command 含 `/ccteam ` → rewrite 为 `/cct ` 真实路径(原子写)
  - `~/.claude/skills/ccteam-{control,team-author}/` marker 校验(`<!-- ccteam-managed -->` 或 frontmatter `name:`)→ 匹配则 `rm -rf`,不匹配(用户手改)→ 保留 + warn
  - 跟 V0.2.0 `--migrate-recommended-agents` 同模式(install 时清旧 + 新装)
- [ ] **#1.7** CLAUDE.md §三 / §四 / §五 已加(本 PR 把 §13 PRD 草案的 "patch 开发流程"段也落)
- [ ] **#1.8** 测试
  - 黄金 e2e:`cct doctor --install-skill` 后 `~/.claude/skills/cct-{control,team-author,project-creator}/` 三个都存在(注意 PR #2 才加 project-creator,本 PR 只校 2 个)
  - migration 测试:fixture 模拟 V0.2.1 用户(`~/.claude/skills/ccteam-control/` + 老 settings.json)→ `cct doctor` 跑后清旧 + 新装

### 验收(摘 PRD §10 验收 F39 段)

- [ ] `crates/ccteam-cli/Cargo.toml::[[bin]] name = "cct"`;`cargo build --release` 产物 = `cct`
- [ ] `skills/cct-{control,team-author}/SKILL.md` ship,旧 `crates/ccteam-core/src/templates/ccteam_*_skill.md` 删
- [ ] `skill.rs` 三常量 + 三函数 rename(三 = control + team-author + project-creator,但 project-creator 在 PR #2 加,本 PR 只两个)
- [ ] `settings.json` 模板用 `{{CCT_BIN}}`,doctor 替换为 `current_exe()` 真路径
- [ ] `cct doctor` 跑迁移路径无回归;markered ccteam-* skill 自动清,unmarkered 保留 + warn
- [ ] forward-looking 文档全 `ccteam <cmd>` → `cct <cmd>`;`docs/v0-1/` `docs/v0-2/` 不动
- [ ] `git grep -nE 'ccteam-(control|team-author)'` 在 `crates/` `skills/` `README.md` `CLAUDE.md` 命中 0 次(historical archive 除外)
- [ ] `cargo test --workspace` 全绿,baseline 511 → ≥ 511

### 文档同步

- `CLAUDE.md` §三红线 / §四 Skills 行 / §五 patch 开发流程小节(本 PR 落)
- `docs/README.md` patch 目录约定(本 PR 落,与 PRD §13 配套)
- `docs/interfaces.md` §10 命令清单 `ccteam` → `cct`
- `docs/dev-coupling-audit.md` F39 close 标记

---

## 3. PR #2 — F34 slug 四层 + F37 meta-agent 决策树

> **目标**:slug 四层调用栈(显式 / meta-agent NL / `claude -p` 智能 / deterministic
> 兜底)+ 新 `cct-project-creator` skill body + meta-agent role prompt §1 自检反例
> 加固。F34 + F37 两 finding 同 PR(都改 `meta_agent_role.md`,且 F34 派单流程
> §3.2.2 重写为指 cct-project-creator skill,与 F37 §6.3 协同)。

**关联 PRD**:§3(F34 全文)+ §6(F37 全文)

**前置**:**PR #1 必须先 merge**(消费 `skills/` 目录 + cct-* 命名 + skill.rs install fn 模板)。

### 任务

- [ ] **#2.1** Tier 1 — `--slug` CLI flag
  - `crates/ccteam-cli/src/main.rs:71` `Commands::New` 加字段 `slug: Option<String>` / `auto_slug_model: Option<String>` / `no_auto_slug: bool`
  - `crates/ccteam-cli/src/commands.rs::run_new` 接 `slug: Option<&str>`
  - B2 前缀语义:`if slug.starts_with(&format!("{team}-"))` verbatim;否则 prepend `<team>-`(详 PRD §3.2.1)
  - 撞名仍 `pick_unused_slug` 的 `-{4hex}` retry
- [ ] **#2.2** Tier 4 — `slugify_brief()` deterministic 兜底
  - `crates/ccteam-core/src/projects.rs` 新函数 `slugify_brief(input: &str) -> String`(~30 LoC,详 PRD §3.2.4)
  - **不动 `slugify()`**(meta-agent path `<handle>-meta` 用)
  - `pick_unused_slug` 内部从 `slugify(base)` 切到 `slugify_brief(base)`
  - 单元测试覆盖 PRD §3.2.4 表的 7 输入预期
- [ ] **#2.3** Tier 3 — `claude -p` 智能 fallback
  - `commands.rs::run_new`:无 `--slug` + `which claude` 在 PATH + stdin 是 tty + 非 `--no-auto-slug` → shell out
  - prompt template 详 PRD §3.2.3(haiku-4-5,15s 超时硬截,sanitize stdout `[a-z0-9-]+` ≤ 60 char)
  - 失败 fallback 全表(PRD §3.2.3 fallback 链)→ 降级 Tier 4 + log warn 含 reason
  - flag:`--no-auto-slug` / `--auto-slug-model haiku/sonnet`(默认 haiku)/ env `CCTEAM_AUTO_SLUG=off`
  - **非 tty(脚本 / e2e)→ auto-accept** 建议(15s 超时硬截兜)
- [ ] **#2.4** 新 skill `cct-project-creator`
  - `skills/cct-project-creator/SKILL.md` 新建(skill body 详 PRD §3.2.2;Phase A/B/C/D 流程,AskUserQuestion 结构化选项)
  - frontmatter `name: cct-project-creator` + 触发词("新项目" / "建一个 X" / "调研 X")
  - **AskUserQuestion 显式说明**:本 skill 仅在 meta-agent context 调用,project session 的 PreToolUse hook 拦不到 meta-agent(V0.2 §2.4 已 spec)
- [ ] **#2.5** Rust 端新 install fn
  - `crates/ccteam-core/src/skill.rs` 加 `CCT_PROJECT_CREATOR_SKILL_MD` 常量 + `install_cct_project_creator_skill` fn
  - `cct doctor --install-skill` extend 装 3 skills(cct-control / cct-team-author / cct-project-creator)
- [ ] **#2.6** F37 — `meta_agent_role.md` 决策树加固
  - §1 自检段加显式反例 + 项目请求边界(详 PRD §6.2.1):「调研 X / 看 X 值不值得做」= 项目请求,**绝不**自起 `Agent(subagent_type=...)` 调研
  - §2 派单段简化为:走 `cct-project-creator` skill(已自动装好);流程细节走 skill body
  - §3 克制规则加显式反例(详 PRD §6.2.2):❌ `Agent(subagent_type=general-purpose)` / web 搜索 — 这是 product-research(PR #6 后改 `research`)团队的活
  - 边界不清场景:问一句 "事实询问还是产研请求?" 不默选(详 PRD §6.2.1)
- [ ] **#2.7** 测试
  - `slugify_brief` unit:PRD §3.2.4 表 7 输入断言
  - `--slug` flag e2e:`cct new --slug ccteam-ui --team dev` → `~/projects/dev-ccteam-ui/`;`--slug dev-x` verbatim;非法字符 fail-loud
  - Tier 3 fallback e2e:mock claude 返回非法 stdout → 降级 Tier 4
  - `cct doctor --install-skill` e2e:三 skill 都装 + frontmatter `name:` 校验
  - meta-agent role prompt e2e:doctor `--install-meta-agent <h>` 后 CLAUDE.md 含新 §1 反例段

### 验收(摘 PRD §10 F34 + F37)

- [ ] `cct new --slug <name> --team <team>` 创建用 `<team>-<name>` 或 verbatim
- [ ] 撞名 retry 仍工作;非法 slug fail-loud
- [ ] `skills/cct-project-creator/SKILL.md` ship + `install_cct_project_creator_skill` 落 `~/.claude/skills/`
- [ ] skill body 用 `AskUserQuestion` 做 slug / team / 关键澄清结构化选择
- [ ] Tier 3:`cct new` 无 `--slug` + tty,shell out `claude -p --model claude-haiku-4-5-20251001` + Y/n;15s 超时;失败降级 Tier 4
- [ ] Tier 4:`slugify_brief()` token-aware + stop-word + dedupe + 取前 3 token;`slugify()` 不动
- [ ] `pick_unused_slug` 切到 `slugify_brief(base)`
- [ ] `meta_agent_role.md` §1 反例段 + §2 派单段指 skill + §3 克制规则反例
- [ ] `cct doctor --install-meta-agent <handle>` 重写 CLAUDE.md 含新 §

### 文档同步

- `docs/interfaces.md` §10 `cct new` 加 `--slug` / `--no-auto-slug` / `--auto-slug-model` flag schema
- `docs/tech-design.md` §3.8 用户接口层 / §6 加 cct-project-creator skill 说明
- `docs/dev-coupling-audit.md` F34 / F37 close 标记

---

## 4. PR #3 — F35 silence classifier + enriched outbox

> **目标**:orchestrator daemon 主循环加事件感知 silence classifier(读 progress.jsonl
> 末事件 + 静默时长 → 4 类响应),deterministic re-inject + enriched escalate 写
> outbox(含 `pane_tail` + classification + last_event 字段);capture-pane helper
> 提到 ccteam-core。meta-agent role prompt 加 propose-confirm UX 模板。

**关联 PRD**:§4(F35 全文)

### 任务

- [ ] **#3.1** capture-pane helper 提到 ccteam-core
  - `crates/ccteam-core/src/tmux.rs` 加 `pub fn capture_pane_tail(slug: &str, lines: usize) -> Result<Option<String>>`
  - `crates/ccteam-hooks/src/parse_phase_end.rs:313`(L3 fail-safe)改引用 ccteam-core helper(回归测试 Stop hook L3 行为不变)
  - 红线 reminder:**只入 outbox 给 meta-agent / 用户读,不进 orchestrator 状态机**(§3.7 红线"永不解析终端输出")
- [ ] **#3.2** silence_classifier 模块 + 4 类 enum
  - `crates/ccteam-core/src/silence_classifier.rs`(新)或 `stall.rs` 升级;权衡:新模块独立单测面更干净,推荐新模块
  - `enum SilenceClass { Healthy, Terminal, SubagentBusy, SubagentRunaway, MidToolHung, PostStopLimbo, InjectLimbo }`(7 类细分,详 PRD §4.2.1)
  - `pub fn classify(events: &[ProgressEvent], silent_seconds: u64, thresholds: &StallThresholds) -> SilenceClass`
  - 复用 `stall.rs::StallThresholds::from_phase`(warn / suspicious / escalate 三阈值不另定义)
  - 单元测试 7 类全覆盖(eg `PreToolUse(tool=Task)` 后 < escalate 阈值 = `SubagentBusy` / ≥ 阈值 = `SubagentRunaway`)
- [ ] **#3.3** enriched outbox schema 字段
  - `<project>/.ccteam/needs_attention.outbox.json` 加新字段(详 PRD §4.2.2):
    - `ccteam_classification: string`(SilenceClass discriminant)
    - `ccteam_silent_seconds: u64`
    - `ccteam_last_event: { ts, event, tool? }`
    - `ccteam_pane_tail: string`(已有协议,新 case 复用)
  - `interfaces.md` §6.x 同步 schema
  - 跟 Stop hook L3 fail-safe 同 schema 共存(L3 不写新字段,只 fail-safe 兜底)
- [ ] **#3.4** orchestrator daemon tick 集成
  - `crates/ccteam-core/src/orchestrator.rs::process_project` per-project tick 末加 classifier 调用
  - 决策表(详 PRD §4.2.1):
    - `SubagentBusy` / `Healthy` / `Terminal` → 不动
    - `SubagentRunaway` / `MidToolHung` → enriched escalate(写 outbox + log)
    - `PostStopLimbo` / `InjectLimbo` → deterministic re-inject 1 次,再失败 → enriched escalate
  - re-inject 限流:1 次 retry cap(state 字段 `auto_loop.last_reinject_at` 记录)
  - **红线 reminder**(详 PRD §4.2.3):classifier 是 deterministic;**meta-agent 只 surface 选项**,不发 Ctrl+C / 改 phase / kill;只有 `PostStopLimbo` / `InjectLimbo` deterministic re-inject 由 orchestrator 直接做(已是 deterministic 路径,不经 LLM)
- [ ] **#3.5** meta-agent role prompt — propose-confirm UX 模板
  - `meta_agent_role.md` §7 watchdog 段补充 enriched escalate NL 翻译模板(详 PRD §4.2.3)
  - 模板示例:「项目 X 在 implement 第 12 分钟卡在 Read(file.md) 后无 PostToolUse,看起来 tool hang 而非 subagent 慢工。要不要 (a) `cct attach dev-x` 自看 (b) 让我等再 5 min 重看 (c) 这条不管了?」
  - **不 autonomous decide** — 用户回弹纠错
- [ ] **#3.6** 单元 + e2e 测试
  - `silence_classifier` 7 类 unit
  - `MidToolHung` / `SubagentRunaway` 触发 enriched outbox 字段完整性断言
  - `PostStopLimbo` / `InjectLimbo` deterministic re-inject 1 次 + 再失败 → escalate(测试)
  - capture-pane helper 共用 + Stop hook L3 行为不变 regression
  - `tick` 频率:每 project 每 5-10s,无新 event 时跑 classify(廉价 — 读 progress.jsonl 末几行 + match)

### 验收(摘 PRD §10 F35)

- [ ] silence_classifier 4+ 类分类 unit 测试覆盖
- [ ] `MidToolHung` / `SubagentRunaway` 触发 enriched outbox,字段完整(classification / silent_seconds / last_event / pane_tail)
- [ ] `PostStopLimbo` / `InjectLimbo` deterministic re-inject 1 次,再触发 → enriched escalate
- [ ] capture-pane helper 提到 ccteam-core,parse_phase_end.rs 改引用,Stop hook L3 行为不变
- [ ] meta-agent role prompt 加 enriched outbox NL 翻译模板

### 文档同步

- `docs/tech-design.md` §3.5 fix-loop / §6.9 idle injection 加 silence_classifier 说明
- `docs/interfaces.md` §6.x outbox schema 加新字段(`ccteam_classification` / `ccteam_silent_seconds` / `ccteam_last_event` / `ccteam_pane_tail` 已有)
- `docs/dev-coupling-audit.md` F35 close

---

## 5. PR #4 — F36 send-keys subagent guard

> **目标**:`dispatch_phase_with_state` 注入路径前加 active-subagent 检测,defer
> until SubagentStop;pending-inject.json 单文件协议;max-defer-minutes 兜底。
> 与 F35 协同:F36 主路径主动 defer,F35 `InjectLimbo` 类兜底。

**关联 PRD**:§5(F36 全文)

**前置**:软依赖 PR #3(共享 enriched outbox schema 字段 — `ccteam_classification`
新增 `InjectDeferTimeout` 一类)。

### 任务

- [ ] **#4.1** `subagent_active(events)` helper
  - `crates/ccteam-core/src/progress.rs` 加 `pub fn subagent_active(events: &[Value]) -> bool`
  - 扫末事件序列,counting `PreToolUse(tool=Task)` 和 `SubagentStop`,开多于关 → true
  - 单测:开 / 关 / 嵌套(PreToolUse(Task) 嵌 PreToolUse(Task) → SubagentStop → 仍有 1 个 active)
- [ ] **#4.2** `dispatch_phase_with_state` 注入前 guard
  - `orchestrator.rs::dispatch_phase_with_state` 等所有注入路径前加(详 PRD §5.2):
    ```rust
    if subagent_active(&recent_events) {
        self.queue_pending_inject(slug, phase, attachment_refs);
        return Ok(());  // 或返 PendingInject 状态
    }
    ```
  - **不发 send-keys**;落 `<project>/.ccteam/pending-inject.json`(单文件,最新覆盖旧的)
- [ ] **#4.3** pending-inject.json schema
  - 字段:`{ phase, attachment_refs, enqueued_at, max_defer_minutes }`
  - `interfaces.md` §6.x 同步加(跟 outbox 同段)
- [ ] **#4.4** daemon tick 真发 + 兜底
  - `orchestrator.rs` daemon 主循环 per-project tick:`pending-inject.json` 存在 + subagent 不再 active + 距 `enqueued_at` < `max_defer_minutes`(默认 10) → 真发 send-keys + 删文件
  - 超时 → fail-loud escalate:`<project>/.ccteam/needs_attention.outbox.json` 加 `ccteam_classification: "InjectDeferTimeout"`(F35 schema 复用)
- [ ] **#4.5** 测试
  - `subagent_active` unit:开 / 关 / 嵌套场景
  - dispatch e2e:active subagent 时 dispatch → 不发 send-keys,pending-inject.json 存在
  - daemon tick e2e:SubagentStop event 后,pending-inject 真发 + 删文件
  - 超时 e2e:max-defer-minutes 超 → enriched escalate(InjectDeferTimeout)

### 验收(摘 PRD §10 F36)

- [ ] `subagent_active(events)` unit 测试(开 / 关 / 嵌套)
- [ ] `dispatch_phase_with_state` 检测 active subagent → pending-inject.json 落盘 + 不发 send-keys
- [ ] daemon tick 在 SubagentStop event 后真发 pending-inject + 删文件
- [ ] max-defer-minutes 兜底:超时 → enriched escalate,不无限 defer

### 文档同步

- `docs/tech-design.md` §6.9 idle injection 加 subagent guard 说明
- `docs/interfaces.md` §6.x pending-inject.json schema
- `docs/dev-coupling-audit.md` F36 close

---

## 6. PR #5 — F38 终端截图 PNG(vt100 + imageproc DIY)

> **目标**:tmux capture-pane → vt100 状态机 → imageproc 渲染 → PNG,纯 Rust 全栈,
> vendored JetBrains Mono(OFL),无 system C deps。`mcp__<ns>__screenshot` MCP 工具 +
> outbox `screenshot_path` 字段(F35 schema rebase 加)。

**关联 PRD**:§7(F38 全文,5 轮迭代敲定 vt100 + imageproc DIY)

**前置**:软依赖 PR #3(F35 outbox 加 `screenshot_path` 字段时 rebase)。

### 任务

- [ ] **#5.1** Cargo deps + vendored TTF
  - `crates/ccteam-core/Cargo.toml` 加:`vt100 = "0.15"` + `image = "0.25"` + `imageproc = "0.25"` + `ab_glyph = "0.2"`
  - `crates/ccteam-core/assets/fonts/JetBrainsMono-Regular.ttf` vendor(OFL,~150 KB)
  - `LICENSES.md` 加 third-party-fonts 注(OFL 文本 + 来源 URL)
  - `include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")` 编译时打包
- [ ] **#5.2** ANSI_256 调色板常量
  - `crates/ccteam-core/src/screenshot/ansi_palette.rs`(子模块)
  - `pub const ANSI_256: [Rgb<u8>; 256]`(16 标准 + 216 立方 6×6×6 + 24 灰阶,标准映射)
  - 黄金值测试:每段抽 3 个 idx 断言 RGB 正确
- [ ] **#5.3** screenshot.rs 主模块(~250 LoC)
  - `crates/ccteam-core/src/screenshot.rs::render_screenshot(paths, slug, lines) -> Result<Option<PathBuf>>`
  - 流程(详 PRD §7.2.3):
    1. `capture_pane_with_ansi(slug, lines)` — `tmux capture-pane -e -p -t ccteam-<slug> -S -<lines>`
    2. `query_pane_dims(slug)` — `tmux display-message -p '#{pane_height} #{pane_width}'`
    3. `vt100::Parser::new(rows, cols, 0).process(&bytes)`
    4. 字体加载:env `CCTEAM_SCREENSHOT_FONT_TTF` 优先,缺则 vendored
    5. cell 度量 `text_size(scale, &font, "M")` → 单 cell w/h
    6. 遍历 grid:`draw_filled_rect_mut(bg)` + `draw_text_mut(fg + cell.contents())`
    7. `img.save("<project>/.ccteam/screenshots/<utc>.png")`
  - `vt100_color_to_rgb(c, default)`:match Default / Idx(i) → ANSI_256[i] / Rgb(r,g,b)
- [ ] **#5.4** graceful degrade — `std::panic::catch_unwind` 兜底
  - 失败场景全表(详 PRD §7.2.5):tmux 失败 / vt100 panic / ttf 解析失败 / imageproc panic / IO 失败
  - 全部 → `Ok(None)` + log warn 含 reason + outbox 不写 `screenshot_path`(主路径不挂)
  - 红线:**screenshot 永不阻塞 enriched escalate / outbox 写入**;F35 文本 `pane_tail` 是 ASCII 维度兜底
- [ ] **#5.5** MCP 工具 `mcp__<ns>__screenshot`
  - `crates/ccteam-cli/src/mcp_serve.rs` 加 tool(跟现 9 工具并列)
  - input: `{ slug: string, lines?: number }`(default 50)
  - output: `{ ok: bool, path?: string, reason?: string }`
  - **MCP namespace 决议**:V0.2.2 默认 `mcp__ccteam__screenshot` 不动 namespace(F39 §8.3 明示 MCP namespace 改名 V0.3 评估)
- [ ] **#5.6** doctor `--screenshot-smoke <slug>` flag
  - `crates/ccteam-cli/src/commands.rs` 加 `cct doctor --screenshot-smoke <slug>` 跑端到端渲染
  - print 命中的 ttf path / 失败 reason(字体 / tmux / IO 哪一层)
- [ ] **#5.7** F35 outbox schema rebase
  - 加 `ccteam_screenshot_path: Option<String>` 字段(best-effort,失败 silent)
  - meta-agent NL 翻译时:有路径就附 "(屏幕截图:file:///<path>)",没有就跳过
- [ ] **#5.8** 测试
  - `ANSI_256` 黄金值 unit
  - `vt100_color_to_rgb` unit(Default / Idx / Rgb 三 case)
  - `render_screenshot` smoke e2e:固定 ANSI byte 输入 → PNG 生成,断言尺寸 / 非空
  - graceful degrade e2e:tmux 不存在 → `Ok(None)`(主路径 outbox 仍写)
  - `cct doctor --screenshot-smoke <slug>` e2e:fixture project + smoke run

### 验收(摘 PRD §10 F38)

- [ ] `Cargo.toml` 加 4 deps(vt100 / image / imageproc / ab_glyph)
- [ ] vendored JetBrains Mono OFL ttf;`LICENSES.md` 加注
- [ ] `screenshot.rs::render_screenshot(slug, lines)` API 上线
- [ ] `ANSI_256: [Rgb<u8>; 256]` 调色板常量
- [ ] `mcp__<ns>__screenshot(slug, lines?)` MCP 工具(namespace 默认 ccteam)
- [ ] `CCTEAM_SCREENSHOT_FONT_TTF` env 覆盖字体
- [ ] graceful degrade 全覆盖(`catch_unwind` 兜潜在 panic)
- [ ] F35 enriched outbox rebase 加 `screenshot_path` 字段
- [ ] `cct doctor --screenshot-smoke <slug>` 端到端 verify
- [ ] `interfaces.md` §12 加 MCP 工具 schema
- [ ] 颜色映射黄金值测试:输入预设 ANSI escape,断言 cell 颜色映射 ANSI_256 表

### 文档同步

- `docs/interfaces.md` §12 加 `mcp__ccteam__screenshot` MCP 工具 schema
- `docs/tech-design.md` §3.8 / §6.x screenshot 段
- `docs/dev-coupling-audit.md` F38 close
- `LICENSES.md` 加 OFL 字体注(若文件不存在则建)

---

## 7. PR #6 — F40 team 名缩短 + alias 软迁移

> **目标**:`teams/product-research/` → `teams/research/`(`git mv` 保 history),
> `team.yaml::name = research` + `aliases: [product-research]` + `description` 字段
> 载全称;`team_resolver` 按 alias 匹配老项目;新项目目录 `~/projects/research-<slug>/`,
> 老项目仍 `~/projects/product-research-<slug>/` 不动。**不动用户数据**。

**关联 PRD**:§9(F40 全文)

### 任务

- [ ] **#6.1** 仓内 team 重命名(git mv 保 history)
  - `git mv teams/product-research/ teams/research/`
  - `teams/research/team.yaml`:
    - `name: research`(改)
    - `aliases: [product-research]`(新字段)
    - `description: "Product research team — kickoff → research → verdict → next-steps;用于'判断 idea 值不值得做'场景"`(新字段载全称)
  - phase markdown 内容不动(不引用自己 team 名)
- [ ] **#6.2** `TeamSpec::aliases` 字段 + resolver
  - `crates/ccteam-core/src/team.rs::TeamSpec` 加 `#[serde(default)] pub aliases: Vec<String>`
  - `team_resolver::resolve_team(query)` 按 name + aliases 匹配(详 PRD §9.2.2)
  - 老项目 `state.json::team: "product-research"` → resolver 命中 alias → 加载 `teams/research/team.yaml`(canonical = "research")
- [ ] **#6.3** `cct new --team` UI
  - 接受 `research` 或 `product-research`;后者 stderr warn `"deprecated alias, use 'research'"`(不 fail-loud,过渡期友好)
  - 实际派发 canonical name `research`(新项目 `state.json::team = "research"`)
  - `cct-project-creator` skill body Phase C team 选择 option label `"research"` + description 用 `team.yaml::description` 全文
- [ ] **#6.4** 老项目 rules 文件并存
  - `~/.claude/rules/ccteam-lessons-product-research.md` 不删(已积累跨项目记忆有价值,paths frontmatter 仍匹配 `~/projects/product-research-*`)
  - 新项目 `~/.claude/rules/ccteam-lessons-research.md`(新生成,paths 匹配 `~/projects/research-*`)
  - doctor 写一份新的(从 `teams/research/team.yaml::retro_schema` 渲染);老的留着不动
- [ ] **#6.5** 测试
  - `crates/ccteam-cli/tests/m3_product_research_e2e_test.rs` 改用 `research` 名
  - 增 alias resolution 测试:`team: product-research` 也通(state.json 里写 product-research → 加载 research team)
  - 增 deprecation warn 测试:`cct new --team product-research` stderr 含 "deprecated alias"

### 验收(摘 PRD §10 配套 + §9.2.5)

- [ ] `teams/research/team.yaml::name = research` + `aliases: [product-research]` + `description` 字段
- [ ] `git log --follow teams/research/team.yaml` 能追到 product-research 历史
- [ ] `team_resolver` 按 alias 匹配老项目 e2e 通过
- [ ] `cct new --team product-research` stderr warn + 实际派发 canonical `research`
- [ ] 老 rules 文件 `ccteam-lessons-product-research.md` 不动;新文件生成
- [ ] `interfaces.md` §5.5 加 `aliases` 字段
- [ ] `tech-design.md` §3 团队抽象表更新("领域命名"目标从 V0.3 deferred 改 V0.2.2 ship)
- [ ] `dev-coupling-audit.md` F40 close

### 文档同步

- `docs/interfaces.md` §5.5 team.yaml schema 加 `aliases` / `description` 字段
- `docs/tech-design.md` §3 团队抽象表
- `docs/dev-coupling-audit.md` F40 close

---

## 8. PR #7 — chore:workspace.version + dev-flow + e2e + retro

> **目标**:V0.2.2 ship gate。Cargo workspace.version `"0.0.1"` → `"0.2.2"`(retroactive
> 修正);CLAUDE.md §一表格 baseline 回填新数;e2e 联跑 + retro。

**关联 PRD**:§12(version bump)+ §13(CLAUDE.md dev-flow,PR #1 已落,本 PR 校验)

**前置**:PR #1-#6 全部 merge。

### 任务

- [ ] **#7.1** Cargo workspace.version bump
  - `Cargo.toml::workspace.package.version` `"0.0.1"` → `"0.2.2"`
  - commit subject `v0.2.2: ...` 前缀
  - 新政策注:每个 minor / patch release **必须 bump** + commit subject 一致(已写 PRD §12)
- [ ] **#7.2** CLAUDE.md baseline 回填
  - §一表格 测试 baseline 数从 511 更新到 V0.2.2 实际(预估 511 + 各 PR 新增 ≈ 600+)
  - "已 ship 里程碑"行加 V0.2.2 patch 段 — 详 docs/v0-2-2/README.md
- [ ] **#7.3** docs/README.md patch 目录约定
  - 已 PR #1 落档(2026-05-09);本 PR 校验存在
- [ ] **#7.4** docs/v0-2-2/e2e-retro.md
  - 落档 V0.2.2 e2e 联跑结果(7 PR merge 后跑一遍 dev / research 两 team smoke + 看是否新 finding)
  - 新发现 finding 入 V0.3 候选(不本轮做)
- [ ] **#7.5** dev-coupling-audit.md
  - F34-F40 全部 close 标记(各 PR merge 时已加,本 PR 校验)
- [ ] **#7.6** 红线 grep 矩阵跑一遍(详 §10)
- [ ] **#7.7** 测试
  - `cargo test --workspace` 全绿
  - `cargo clippy --workspace --all-targets` 不新增 warning(4 pre-existing 不算)
  - dev / research 两 team smoke e2e 各跑一遍

### 验收

- [ ] `Cargo.toml::workspace.package.version = "0.2.2"`
- [ ] CLAUDE.md §一表格 baseline 数更新
- [ ] `docs/v0-2-2/e2e-retro.md` 落档
- [ ] `dev-coupling-audit.md` F34-F40 全 close
- [ ] `git grep -nE "ccteam-(control|team-author)"` 在 `crates/` `skills/` `README.md` `CLAUDE.md` 0 命中
- [ ] `git grep -nE 'product-research'` 在 `crates/` `teams/` 命中只在 alias 字段 / 老 rules path / 注释(grep 矩阵详 §10)
- [ ] cargo test 全绿,clippy 不新增 warning

### 文档同步

- `CLAUDE.md` §一表格(baseline + 已 ship 段)
- `docs/v0-2-2/e2e-retro.md`(新落)
- `docs/dev-coupling-audit.md` F34-F40 全 close

---

## 9. Worktree subagent briefing 模板

每个 PR 一份 briefing。subagent 是 **新 session,无对话历史**,模板必须
self-contained — 含目标 / 必读文件 / 实施步骤 / 红线 grep / 验收 / PR 命令。
模板设计原则:**briefing 让 subagent 拿到模板 + 进 worktree 即可开干**。

### 9.1 通用前置(每个 PR 都跑一遍)

```bash
# 1. 进 worktree
cd /tmp/ccteam-<branch>

# 2. baseline 验证
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6}END{print "baseline:", p, "passed,", f, "failed"}'
# 期望:511 passed, 0 failed(V0.2.2 base = 170f5a8)

# 3. 读必读文档
# - PRD 对应章节(每 briefing 列出)
# - CLAUDE.md §三红线 + §五 PR 纪律
# - 关联代码文件(每 briefing 列出)
```

### 9.2 PR #1 briefing(F39 cct rename)

```markdown
## 任务

V0.2.2 PR #1 — `cct` 短前缀约定 sweep:binary `ccteam` → `cct`、自带 skill
`ccteam-{control,team-author}` → `cct-{control,team-author}`、迁顶层 `skills/`
目录、V0.1/V0.2 用户升级迁移。机械 rename,先 merge 立约定。

## 必读

1. `docs/v0-2-2/prd.md` §8(F39 全文,~150 行)
2. `docs/v0-2-2/dev-plan.md` §2(本 PR 步骤)
3. `CLAUDE.md` §三红线("cct 短前缀约定" / "skill 顶层 skills/ 目录")+ §五 PR 纪律
4. 触点代码:
   - `crates/ccteam-cli/Cargo.toml`
   - `crates/ccteam-core/src/skill.rs`
   - `crates/ccteam-core/src/templates/{ccteam_control,ccteam_team_author}_skill.md`
   - `crates/ccteam-core/src/templates/settings.json`
   - `README.md` / `docs/tech-design.md` / `docs/interfaces.md` / `docs/requirements.md`

## 实施步骤(详 dev-plan §2)

1. binary rename(#1.1)
2. `skills/` 目录建立 + 两 skill 迁移(#1.2)
3. Rust 常量 / 函数 rename(#1.3)
4. settings.json 占位符(#1.4)
5. 文档 sweep(#1.5)
6. doctor 升级迁移(#1.6)
7. CLAUDE.md §五 dev-flow 段(#1.7)
8. 测试(#1.8)

## 红线 grep(commit 前必跑)

```bash
git grep -nE 'ccteam-(control|team-author)' crates/ skills/ README.md CLAUDE.md
# 期望:0 命中(historical archive `docs/v0-1/` `docs/v0-2/` 不算)

git grep -nE '"ccteam"' crates/ccteam-cli/src/ crates/ccteam-core/src/
# 期望:全部是注释 / 测试 fixture / 历史归档,无 forward shell-out

git grep -nE '\bccteam (new|ls|show|attach|hook|doctor|watchdog|team)\b' README.md docs/tech-design.md docs/interfaces.md docs/requirements.md CLAUDE.md
# 期望:0 命中
```

## 验收 checklist

详 dev-plan §2 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-cct-rename --title "v0.2.2 PR #1: F39 cct convention sweep" --body "$(cat <<'EOF'
## Closes
- F39(`docs/v0-2-2/prd.md` §8)
- `docs/dev-coupling-audit.md` F39 close

## 改动
- binary rename `ccteam` → `cct`(`crates/ccteam-cli/Cargo.toml`)
- 顶层 `skills/` 目录建立,`cct-{control,team-author}` 迁入
- Rust 常量 / 函数 / settings.json 占位符 sync
- forward-looking 文档 sweep,V0.1/V0.2 历史不动
- `cct doctor` 自动迁移老用户(skill 目录 + settings.json hook 路径)
- `CLAUDE.md` §五 patch 开发流程小节(详 prd.md §13)

## 测试
- baseline 511 → ?(预期不退步)
- 新增迁移 e2e 测试

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

### 9.3 PR #2 briefing(F34 + F37)

```markdown
## 任务

V0.2.2 PR #2 — slug 四层调用栈(显式 / meta-agent NL / claude -p 智能 / deterministic
兜底)+ 新 `cct-project-creator` skill body + meta-agent role prompt §1 决策树
加固。F34 + F37 同 PR(都改 `meta_agent_role.md`,F34 派单流程指 cct-project-creator
skill 与 F37 协同)。

## 前置

**PR #1 必须先 merge** — 消费 `skills/` 顶层目录 + `cct-*` 命名 + skill.rs install fn
模板。worktree base 已是 PR #1 merge 后的 main。

## 必读

1. `docs/v0-2-2/prd.md` §3(F34 全文,~280 行)+ §6(F37 全文,~60 行)
2. `docs/v0-2-2/dev-plan.md` §3(本 PR 步骤)
3. `CLAUDE.md` §三红线("控制平面无 LLM" 不破:slug 生成是数据转换 utility,不是控制)+ §五 PR 纪律
4. 触点代码:
   - `crates/ccteam-cli/src/main.rs:71`(Commands::New 加字段)
   - `crates/ccteam-cli/src/commands.rs::run_new`
   - `crates/ccteam-core/src/projects.rs`(新 `slugify_brief`)
   - `crates/ccteam-core/src/skill.rs`(新 install_cct_project_creator_skill)
   - `crates/ccteam-core/src/templates/meta_agent_role.md`(§1 / §2 / §3 改)
   - `skills/cct-project-creator/SKILL.md`(新建)

## 实施步骤(详 dev-plan §3)

1. Tier 1 `--slug` flag(#2.1)
2. Tier 4 `slugify_brief()`(#2.2)
3. Tier 3 `claude -p` 智能 fallback(#2.3)
4. 新 skill `cct-project-creator`(#2.4)
5. Rust 端 install fn(#2.5)
6. F37 meta_agent_role.md §1/§2/§3 改(#2.6)
7. 测试(#2.7)

## 红线 grep

```bash
# claude -p 是 < 5s utility,不是控制路径,但仍 grep 验证不污染 orchestrator daemon
git grep -nE 'Command::new\("claude"\)' crates/ccteam-core/src/orchestrator.rs crates/ccteam-core/src/auto_loop.rs
# 期望:0 命中(slug 生成不在这两文件)

# meta_agent_role.md 决策树反例段必含
git grep -nE '调研.*X|Agent\(subagent_type' crates/ccteam-core/src/templates/meta_agent_role.md
# 期望:命中 ≥ 2(§1 反例 + §3 克制反例)

# AskUserQuestion 拦截只 scope project session
git grep -nE 'AskUserQuestion' crates/ccteam-core/src/templates/
# 期望:project settings.json template 命中(拦截);meta-agent 路径不命中(允许)
```

## 验收 checklist

详 dev-plan §3 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-meta-agent-and-slug --title "v0.2.2 PR #2: F34 slug 四层 + F37 meta-agent 决策树" --body "$(cat <<'EOF'
## Closes
- F34(`docs/v0-2-2/prd.md` §3)
- F37(`docs/v0-2-2/prd.md` §6)
- `docs/dev-coupling-audit.md` F34 / F37 close

## 改动
- `cct new --slug` flag + B2 前缀语义(自动加 team 前缀)
- 四层调用栈:Tier 1 显式 / Tier 2 meta-agent NL / Tier 3 claude -p haiku / Tier 4 slugify_brief deterministic
- 新 skill `cct-project-creator`(meta-agent 派单工作流,AskUserQuestion 结构化选择)
- meta_agent_role.md §1 决策树反例(调研 X = 项目请求)+ §3 克制规则(❌ 自起 Agent 调研)

## 测试
- 新增 ~30 测试(slugify_brief / Tier 3 fallback / skill install / role prompt)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

### 9.4 PR #3 briefing(F35 silence classifier)

```markdown
## 任务

V0.2.2 PR #3 — orchestrator 事件感知 silence classifier(读 progress.jsonl 末事件
+ 静默时长 → 7 类响应),enriched outbox + capture-pane helper 提到 ccteam-core,
meta-agent role prompt 加 propose-confirm UX 模板。

## 必读

1. `docs/v0-2-2/prd.md` §4(F35 全文,~90 行)
2. `docs/v0-2-2/dev-plan.md` §4(本 PR 步骤)
3. `CLAUDE.md` §三红线:**永不解析终端输出**(pane_tail 只入 outbox 给人读,不进状态机) / **永不主动 kill** / **控制平面无 LLM**(classifier 是 deterministic;meta-agent 只 propose,不 decide)
4. `docs/tech-design.md` §3.5(fix-loop)+ §6.9(idle injection)
5. 触点代码:
   - `crates/ccteam-core/src/tmux.rs`(加 `capture_pane_tail` helper)
   - `crates/ccteam-core/src/silence_classifier.rs`(新模块)
   - `crates/ccteam-core/src/orchestrator.rs::process_project`(tick 末加 classifier)
   - `crates/ccteam-core/src/stall.rs`(StallThresholds 复用)
   - `crates/ccteam-hooks/src/parse_phase_end.rs:313`(改引用 ccteam-core helper)
   - `crates/ccteam-core/src/templates/meta_agent_role.md` §7

## 实施步骤(详 dev-plan §4)

1. capture-pane helper 提到 ccteam-core(#3.1)
2. silence_classifier 模块 + enum(#3.2)
3. enriched outbox schema 字段(#3.3)
4. orchestrator daemon tick 集成(#3.4)
5. meta-agent role prompt propose-confirm 模板(#3.5)
6. 单元 + e2e 测试(#3.6)

## 红线 grep

```bash
# pane_tail 只入 outbox,不解析进状态机
git grep -nE 'capture_pane_tail|capture-pane' crates/ccteam-core/src/orchestrator.rs
# 期望:命中只在 escalate / outbox 写入路径,不在 phase_done / state mutation 路径

# classifier 不发 Ctrl+C / kill
git grep -nE 'kill|Ctrl-C|C-c' crates/ccteam-core/src/silence_classifier.rs crates/ccteam-core/src/orchestrator.rs
# 期望:silence_classifier 0 命中;orchestrator 命中只在 daemon shutdown / cost > $200 已有路径

# meta-agent autonomous decide 不破红线 — role prompt 必须是 propose,不是 act
git grep -nE 'Bash.*tmux send-keys|Bash.*Ctrl-C' crates/ccteam-core/src/templates/meta_agent_role.md
# 期望:0 命中(meta-agent 只 surface,不发命令)
```

## 验收 checklist

详 dev-plan §4 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-silence-classifier --title "v0.2.2 PR #3: F35 silence classifier + enriched outbox" --body "$(cat <<'EOF'
## Closes
- F35(`docs/v0-2-2/prd.md` §4)
- `docs/dev-coupling-audit.md` F35 close

## 改动
- 新 `silence_classifier` 模块,7 类 enum(Healthy / Terminal / SubagentBusy / SubagentRunaway / MidToolHung / PostStopLimbo / InjectLimbo)
- enriched outbox schema 加 `ccteam_classification` / `ccteam_silent_seconds` / `ccteam_last_event` / `ccteam_pane_tail` 字段
- capture-pane helper 提到 `ccteam-core::tmux`,Stop hook L3 复用
- orchestrator daemon tick 集成 classifier;Limbo 类 deterministic re-inject 1 次,失败 → enriched escalate
- meta-agent role prompt §7 加 propose-confirm UX 模板

## 测试
- 新增 ~25 测试(classifier 7 类 / outbox 字段完整性 / re-inject 限流 / capture-pane 提取后 L3 行为不变 regression)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

### 9.5 PR #4 briefing(F36 subagent guard)

```markdown
## 任务

V0.2.2 PR #4 — `dispatch_phase_with_state` 注入路径前加 active-subagent 检测,
defer until SubagentStop;pending-inject.json 单文件协议;max-defer-minutes 兜底。

## 前置

**软依赖 PR #3** — 复用 enriched outbox classification 字段(`InjectDeferTimeout`
新增一类)。worktree base 推荐 PR #3 merge 后的 main;如先于 #3 起,rebase 时
追加 `InjectDeferTimeout` 字段。

## 必读

1. `docs/v0-2-2/prd.md` §5(F36 全文,~50 行)
2. `docs/v0-2-2/dev-plan.md` §5(本 PR 步骤)
3. `CLAUDE.md` §三红线 — **idle-aware 注入**(已 spec,F36 强化 subagent 维度)
4. 触点代码:
   - `crates/ccteam-core/src/progress.rs`(加 `subagent_active` helper)
   - `crates/ccteam-core/src/orchestrator.rs::dispatch_phase_with_state` + daemon tick
   - `crates/ccteam-cli/src/main.rs`(若加 `cct dev pending-inject` 调试 CLI 可选)

## 实施步骤(详 dev-plan §5)

1. `subagent_active(events)` helper(#4.1)
2. dispatch 路径前 guard(#4.2)
3. pending-inject.json schema(#4.3)
4. daemon tick 真发 + 兜底(#4.4)
5. 测试(#4.5)

## 红线 grep

```bash
# guard 不发 send-keys
git grep -nE 'send_keys|tmux send-keys' crates/ccteam-core/src/orchestrator.rs
# 期望:命中只在 daemon tick 真发路径,不在 dispatch 直接路径(active subagent 时)

# pending-inject 只单文件,不积累队列
git grep -nE 'pending-inject' crates/ccteam-core/src/
# 期望:文件名 `pending-inject.json` 单数,不带 timestamp 后缀
```

## 验收 checklist

详 dev-plan §5 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-subagent-guard --title "v0.2.2 PR #4: F36 send-keys subagent guard" --body "$(cat <<'EOF'
## Closes
- F36(`docs/v0-2-2/prd.md` §5)
- `docs/dev-coupling-audit.md` F36 close

## 改动
- 新 `progress::subagent_active(events)` helper(扫 PreToolUse(Task) / SubagentStop 计数)
- `dispatch_phase_with_state` active subagent 时 → pending-inject.json 落盘,不发 send-keys
- daemon tick 在 SubagentStop event 后真发 pending-inject + 删文件
- max-defer-minutes 兜底(默认 10):超时 → enriched escalate(InjectDeferTimeout class,F35 schema 复用)

## 测试
- 新增 ~15 测试(subagent_active unit / dispatch e2e / tick 真发 / 超时兜底)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

### 9.6 PR #5 briefing(F38 screenshot)

```markdown
## 任务

V0.2.2 PR #5 — tmux capture-pane → vt100 状态机 → imageproc 渲染 → PNG,纯 Rust
全栈,vendored JetBrains Mono(OFL),无 system C deps;`mcp__<ns>__screenshot`
MCP 工具 + outbox `screenshot_path` 字段(F35 schema rebase)。

## 前置

**软依赖 PR #3** — F35 outbox 加 `screenshot_path` 字段时 rebase。

## 必读

1. `docs/v0-2-2/prd.md` §7(F38 全文,~250 行,含 5 轮迭代敲定 vt100 + imageproc DIY)
2. `docs/v0-2-2/dev-plan.md` §6(本 PR 步骤)
3. `CLAUDE.md` §三红线:**graceful degrade**(screenshot 永不阻塞 outbox 主路径)
4. 触点代码:
   - `crates/ccteam-core/Cargo.toml`(加 4 deps)
   - `crates/ccteam-core/assets/fonts/JetBrainsMono-Regular.ttf`(vendor)
   - `crates/ccteam-core/src/screenshot.rs`(新模块)
   - `crates/ccteam-core/src/screenshot/ansi_palette.rs`(子模块)
   - `crates/ccteam-cli/src/mcp_serve.rs`(加 screenshot tool)
   - `crates/ccteam-cli/src/commands.rs`(加 `--screenshot-smoke` flag)
   - `crates/ccteam-core/src/tmux.rs`(共享 `capture_pane_with_ansi`)
   - `LICENSES.md`(若不存在则建,加 OFL 注)

## 实施步骤(详 dev-plan §6)

1. Cargo deps + vendored TTF(#5.1)
2. ANSI_256 调色板(#5.2)
3. screenshot.rs 主模块(#5.3)
4. graceful degrade(#5.4)
5. MCP 工具(#5.5)
6. doctor `--screenshot-smoke <slug>`(#5.6)
7. F35 outbox rebase(#5.7)
8. 测试(#5.8)

## 红线 grep

```bash
# 无 system C deps
git grep -nE 'font-kit|fontconfig|freetype' crates/ccteam-core/
# 期望:0 命中(纯 ab_glyph + vendored ttf)

# graceful degrade 全 catch_unwind
git grep -nE 'panic::catch_unwind' crates/ccteam-core/src/screenshot
# 期望:vt100 process / imageproc draw 关键路径 ≥ 2 命中

# screenshot 永不阻塞主路径
git grep -nE 'render_screenshot' crates/ccteam-core/src/orchestrator.rs crates/ccteam-core/src/silence_classifier.rs
# 期望:命中只在 best-effort 写 outbox 字段,不在主 escalate 路径(失败 silent,不 propagate)
```

## 验收 checklist

详 dev-plan §6 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-screenshot --title "v0.2.2 PR #5: F38 终端截图 PNG(vt100 + imageproc DIY)" --body "$(cat <<'EOF'
## Closes
- F38(`docs/v0-2-2/prd.md` §7)
- `docs/dev-coupling-audit.md` F38 close

## 改动
- 新 `crates/ccteam-core/src/screenshot.rs`(~250 LoC):tmux capture-pane → vt100 → imageproc → PNG
- 4 cargo deps:vt100 / image / imageproc / ab_glyph(全纯 Rust,无 system C deps)
- vendored JetBrains Mono(OFL,~150 KB),`include_bytes!` 编译期打包
- ANSI_256 调色板常量(16 + 216 cube + 24 grayscale)
- `mcp__ccteam__screenshot(slug, lines?)` MCP 工具
- F35 enriched outbox 加 `screenshot_path` 字段(best-effort,失败 silent)
- `cct doctor --screenshot-smoke <slug>` 端到端 verify
- graceful degrade `catch_unwind` 兜潜在 panic;`CCTEAM_SCREENSHOT_FONT_TTF` env 覆盖字体

## 测试
- 新增 ~20 测试(ANSI_256 黄金值 / vt100_color_to_rgb / render smoke / graceful degrade / doctor smoke)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

### 9.7 PR #6 briefing(F40 team alias)

```markdown
## 任务

V0.2.2 PR #6 — `teams/product-research/` → `teams/research/`(`git mv` 保 history),
`team.yaml::name = research` + `aliases: [product-research]` + `description` 全称;
`team_resolver` 按 alias 匹配老项目;**不动用户数据**。

## 必读

1. `docs/v0-2-2/prd.md` §9(F40 全文,~110 行)
2. `docs/v0-2-2/dev-plan.md` §7(本 PR 步骤)
3. `CLAUDE.md` §三红线 — **`ccteam-core` 不出现 team 名字面量**(已 spec;本 PR alias 字段是 yaml 数据驱动,不破)
4. 触点代码:
   - `teams/research/team.yaml`(git mv 后改 name + aliases + description)
   - `crates/ccteam-core/src/team.rs::TeamSpec`(加 aliases 字段)
   - `crates/ccteam-core/src/team_resolver.rs::resolve_team`
   - `crates/ccteam-cli/src/commands.rs::run_new`(deprecation warn)
   - `crates/ccteam-cli/tests/m3_product_research_e2e_test.rs`(改名 + alias resolution 测试)
   - `crates/ccteam-core/src/templates/cct_project_creator_skill.md`(若 PR #2 已落,Phase C team 选择 option label `research`)

## 实施步骤(详 dev-plan §7)

1. git mv + team.yaml 改(#6.1)
2. TeamSpec aliases + resolver(#6.2)
3. cct new --team UI deprecation warn(#6.3)
4. 老项目 rules 文件并存(#6.4)
5. 测试(#6.5)

## 红线 grep

```bash
# ccteam-core 不出现 team 名字面量
git grep -nE '"product-research"|"research"' crates/ccteam-core/src/
# 期望:命中只在 注释 / 测试 / yaml 字符串字面量;不在 if team == ... 分叉

# 老 rules 文件并存,不删
git grep -nE 'ccteam-lessons-product-research' crates/ccteam-core/src/
# 期望:命中只在 doctor 兜底引用 / 注释,不在 unlink / remove 路径
```

## 验收 checklist

详 dev-plan §7 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-team-alias --title "v0.2.2 PR #6: F40 team alias(product-research → research)" --body "$(cat <<'EOF'
## Closes
- F40(`docs/v0-2-2/prd.md` §9)
- `docs/dev-coupling-audit.md` F40 close

## 改动
- `git mv teams/product-research/ teams/research/`(保 history)
- `team.yaml`:`name: research` + `aliases: [product-research]` + `description` 全称
- `TeamSpec::aliases` 字段 + resolver alias 匹配
- `cct new --team product-research` stderr deprecation warn,实际派发 canonical `research`
- 老项目 `~/projects/product-research-*/` + 老 rules 文件不动(用户数据零迁移)

## 测试
- alias resolution e2e:state.json team=product-research 仍能加载 research team
- deprecation warn e2e:stderr 含 "deprecated alias"

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

### 9.8 PR #7 briefing(chore ship gate)

```markdown
## 任务

V0.2.2 PR #7 — ship gate。Cargo workspace.version `"0.0.1"` → `"0.2.2"`(retroactive
修正);CLAUDE.md baseline 回填;e2e retro;dev-coupling-audit 全部 close。

## 前置

PR #1-#6 全部 merge。

## 必读

1. `docs/v0-2-2/prd.md` §12(version bump)+ §13(CLAUDE.md dev-flow)
2. `docs/v0-2-2/dev-plan.md` §8(本 PR 步骤)+ §10(红线 grep 矩阵)+ §11(文档同步矩阵)
3. `CLAUDE.md` §一(baseline 回填)+ §五(patch 流程,PR #1 已落)

## 实施步骤(详 dev-plan §8)

1. workspace.version bump(#7.1)
2. CLAUDE.md baseline(#7.2)
3. docs/README.md(#7.3,PR #1 已落,本 PR 校验)
4. e2e-retro.md 落档(#7.4)
5. dev-coupling-audit F34-F40 close(#7.5)
6. 红线 grep 矩阵(#7.6)
7. 测试 + clippy(#7.7)

## 红线 grep 矩阵(详 §10)

详 dev-plan §10 全 PR 跨维度 grep 矩阵。本 PR 是最后 gate,**全部跑一遍**。

## 验收 checklist

详 dev-plan §8 验收段。

## PR 命令

```bash
gh pr create --base main --head v0-2-2-chore --title "v0.2.2: workspace.version bump + ship gate" --body "$(cat <<'EOF'
## Closes
- V0.2.2 ship gate
- `docs/dev-coupling-audit.md` F34-F40 全 close

## 改动
- `Cargo.toml::workspace.package.version` `"0.0.1"` → `"0.2.2"`
- `CLAUDE.md` §一 baseline 回填到 V0.2.2 实际(预期 ~600+)
- `docs/v0-2-2/e2e-retro.md` 落档(7 PR merge 后 dev / research smoke 联跑结果)
- 红线 grep 矩阵全跑过(详 dev-plan §10)

## 测试
- baseline ≥ 511 + V0.2.2 各 PR 新增,全绿
- clippy 不新增 warning(4 pre-existing 不算)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```
```

---

## 10. 红线 grep 矩阵

每 PR commit 前必查;PR #7 ship gate 全跑一遍。

| 红线维度 | grep | 期望 | 跨 PR |
|---|---|---|---|
| **tmux 长 session 不用 `claude -p`** 针对 session lifecycle | `git grep -nE 'Command::new\("claude"\)' crates/ccteam-core/src/orchestrator.rs crates/ccteam-core/src/auto_loop.rs` | 0 命中 | PR #2(slug 用 `claude -p` 但是 utility,不在 orchestrator/auto_loop)|
| **控制平面无 LLM** 针对控制决策 | `git grep -nE 'Bash.*tmux send-keys\|Ctrl-C\|kill' crates/ccteam-core/src/templates/meta_agent_role.md` | 0 命中(meta-agent 只 propose) | PR #3(F35 propose-confirm)/ PR #2(F37 决策树)|
| **永不主动 kill** | `git grep -nE 'kill\b' crates/ccteam-core/src/silence_classifier.rs` | 0 命中(Limbo 类只 re-inject 不 kill) | PR #3 |
| **永不解析终端输出** | `git grep -nE 'capture_pane_tail\|capture-pane' crates/ccteam-core/src/orchestrator.rs` | 命中只在 outbox 写入路径,不在 phase_done / state mutation | PR #3 |
| **PHASE_DONE / ESCALATE 字面量** 单 source | `git grep -nE 'PHASE_DONE\|ESCALATE:' crates/ccteam-core/src/` | ≤ 5 命中(全在 inject prompt 拼装位置,V0.2 M0.18 已收敛) | F39 sweep 后仍守住基线 |
| **cct 命名约定** | `git grep -nE 'ccteam-(control\|team-author)' crates/ skills/ README.md CLAUDE.md` | 0 命中(historical archive `docs/v0-1/` `docs/v0-2/` 不算) | PR #1 / 全 PR ship gate |
| **`ccteam-core` 无 team 字面量** | `git grep -nE '"dev"\|"product-research"\|"research"\|"meta-agent"' crates/ccteam-core/src/` | 命中只在 注释 / 测试 fixture / yaml 字符串字面量(非分叉) | PR #6(F40 alias 是 yaml 数据,不破)|
| **graceful degrade** | `git grep -nE 'panic::catch_unwind' crates/ccteam-core/src/screenshot` | ≥ 2 命中(vt100 process / imageproc draw) | PR #5 |
| **screenshot 永不阻塞主路径** | `git grep -nE 'render_screenshot' crates/ccteam-core/src/orchestrator.rs crates/ccteam-core/src/silence_classifier.rs` | 命中只在 best-effort 写 outbox,不 propagate 错误 | PR #5 |
| **font-kit / system C deps** 不引入 | `git grep -nE 'font-kit\|fontconfig\|freetype' crates/ccteam-core/` | 0 命中 | PR #5 |
| **AskUserQuestion 拦截 scope project session** | `git grep -nE 'AskUserQuestion' crates/ccteam-core/src/templates/` | project settings.json 命中(拦截);meta-agent 路径不拦(允许) | PR #2 |

---

## 11. 文档同步矩阵

每 PR merge 时同步;PR #7 ship gate 校验完成。

| PR | docs/tech-design.md | docs/interfaces.md | docs/dev-coupling-audit.md | CLAUDE.md / 其他 |
|---|---|---|---|---|
| **#1 F39** | — | §10 命令清单 | F39 close | CLAUDE.md §三/§四/§五;docs/README.md patch 约定 |
| **#2 F34+F37** | §3.8 用户接口层 / §6 skill | §10 `cct new` flag schema | F34 / F37 close | — |
| **#3 F35** | §3.5 fix-loop / §6.9 idle | §6.x outbox schema(`ccteam_classification` 等)| F35 close | — |
| **#4 F36** | §6.9 idle | §6.x pending-inject.json | F36 close | — |
| **#5 F38** | §3.8 / §6.x screenshot | §12 MCP screenshot 工具 | F38 close | LICENSES.md(OFL 注)|
| **#6 F40** | §3 团队抽象表 | §5.5 team.yaml(aliases / description)| F40 close | — |
| **#7 chore** | — | — | F34-F40 全 close 校验 | CLAUDE.md §一 baseline 回填;docs/v0-2-2/e2e-retro.md 落档 |

跨版本 SoT 不动:`docs/v0-1/` `docs/v0-2/` 历史归档,反映当时 ship 实情(PR #1 sweep 显式排除)。

---

## 12. 测试 baseline

| PR | base | 增量 | 累计 |
|---|---|---|---|
| #1 F39 | 511 | +~50(skill rename + migration e2e) | ~561 |
| #2 F34+F37 | 561 | +~30(slugify_brief / Tier 3 / skill install / role prompt) | ~591 |
| #3 F35 | 591 | +~25(classifier 7 类 / outbox 字段 / re-inject 限流)| ~616 |
| #4 F36 | 616 | +~15(subagent_active / dispatch / tick 真发 / 超时)| ~631 |
| #5 F38 | 631 | +~20(ANSI_256 / vt100_color / render smoke / degrade)| ~651 |
| #6 F40 | 651 | +~10(alias resolution / deprecation warn)| ~661 |
| #7 chore | 661 | 0(配套不加测试,跑全集)| **~661** |
| #8 F44 | 628(post-retro) | +2(反向迁移测试;F39 13 个 forward 测试 in-place flip 不计净增)| **631** |

**V0.2.2 ship 测试 baseline 实测**:628(F41-F43 dust patch 后)→ F44 ship 631。
clippy 不新增 warning(4 pre-existing 不算)。

---

## 13. PR #8 — F44 revert F39 cct convention sweep

> **目标**:整体反向 PR #1 (F39) 的所有改动 — binary 名 `cct` → `ccteam`、
> skill `cct-*` → `ccteam-*`、Rust API、placeholder、docs sweep + V0.2.2(F39'd)
> → V0.2.2(F44'd) 反向迁移逻辑。**不 bump workspace.version**(继续 `0.2.2`,
> 跟 F41-F43 dust patch 同 umbrella)。

**关联 PRD**:§11(F44 全文)+ §8(F39 全文,不修改作为历史记录)

### 任务

- [x] **#8.1** binary rename 反向
  - `crates/ccteam-cli/Cargo.toml::[[bin]] name` `"cct"` → `"ccteam"`
  - Rust 内 `current_cct_bin()` 改回 `current_ccteam_bin()`(全 callsite)
  - `Makefile` 早已是 `BIN_NAME := ccteam`(F39 漏改),F44 不动
- [x] **#8.2** 顶层 `skills/` 目录反向 rename
  - `git mv skills/cct-{control,team-author,project-creator}/ skills/ccteam-{control,team-author,project-creator}/`
  - SKILL.md frontmatter `name:` 字段 + body 内字面 `cct` 命令例子全 sweep
- [x] **#8.3** Rust 常量 / 函数名反向
  - `CCT_*_SKILL_{NAME,MD}` → `CCTEAM_*_SKILL_{NAME,MD}`
  - `install_cct_*_skill` → `install_ccteam_*_skill`
  - `LEGACY_SKILL_NAMES` 内容反向(`ccteam-*` → `cct-*`,变成 F39 era 的反向迁移目标)
- [x] **#8.4** placeholder 反向
  - `crates/ccteam-core/src/templates/settings.json` `{{CCT_BIN}}` → `__CCTEAM_BIN__`
  - `crates/ccteam-core/src/templates.rs::render_project_settings` 替换串同步
- [x] **#8.5** F39 → F44 反向迁移逻辑(`ccteam doctor`)
  - 检测 `~/.claude/skills/cct-{control,team-author,project-creator}/` marker / frontmatter,匹配则 `rm -rf`,不匹配保留 + warn
  - `~/projects/<slug>/.claude/settings.json` hook command `/cct hook ` → `/ccteam hook `(原子写,`json.ccteam-migrate.tmp` 临时文件)
  - 检测 `~/.local/bin/cct` / `~/.cargo/bin/cct` 旧 symlink — warn,不主动删
- [x] **#8.6** 测试反向
  - `tool_surface.rs` 内 13 个 F39 migration 测试 fixtures `ccteam-*` → `cct-*` flip(in-place,test 数量不变)
  - 加 2 反向专属测试:`migrate_legacy_skill_dirs_handles_project_creator`、`migrate_legacy_skill_dirs_idempotent_after_first_run`
  - `commands.rs` 内 doctor F39 测试改名 → F44(逻辑相同,fixtures 反向)
- [x] **#8.7** docs sweep 反向
  - `README.md` / `docs/{tech-design,interfaces,requirements,dev-coupling-audit}.md`:`cct <cmd>` → `ccteam <cmd>`(forward-looking 段)
  - `crates/ccteam-core/src/templates/{meta_agent_role,memory_bridge_*}.md`:全 `cct` → `ccteam`
  - `CLAUDE.md` 三处:§三 删除 cct 红线、§四 Skills 行回到 `ccteam-*`、§六 V0.2 → V0.2.2 entry 改为 F39 → F44 反向迁移
  - `docs/v0-2-2/{prd.md,dev-plan.md,README.md}`:加 F44 章节(本节);F39 §8 / PR #1 §2 全文不动作为历史
  - `docs/v0-1/` / `docs/v0-2/` 历史归档不动
  - `docs/v0-2-2/e2e-retro.md` 不动(F39 ship 时的快照,反映当时实情)

### 验收

- [x] `crates/ccteam-cli/Cargo.toml::[[bin]] name = "ccteam"`,`cargo build --release` 产物 = `ccteam`
- [x] `skills/ccteam-{control,team-author,project-creator}/SKILL.md` 三个 ship,frontmatter `name:` = `ccteam-*`,body 命令字面量 `ccteam <cmd>`
- [x] `skill.rs` 三常量 + 三 install 函数全反向 rename
- [x] `settings.json` 模板用 `__CCTEAM_BIN__`,`render_project_settings` 替换正确
- [x] `git grep -nE '\bcct\b' -- ':!docs/v0-2-2/prd.md' ':!docs/v0-2-2/dev-plan.md' ':!docs/v0-2-2/README.md' ':!docs/v0-2-2/e2e-retro.md' ':!docs/dev-coupling-audit.md' ':!docs/v0-1' ':!docs/v0-2/'` 在 forward-looking 源码 / 文档命中 0(F44 PRD / dev-plan / README + dev-coupling-audit F39 历史 + tool_surface.rs 反向迁移逻辑内 `cct-*` 检测字符串除外)
- [x] `cargo test --workspace` 全绿,628 → 631(F39 in-place flip 不计净增,纯加 2 测试)
- [x] clippy 不新增 warning

### 文档同步

- `CLAUDE.md` §一 baseline 表格 `628 → 631`、§六 反向迁移条目
- `docs/v0-2-2/{README,prd,dev-plan}.md` 三个加 F44
- `docs/dev-coupling-audit.md` F39 标 "已 F44 反向" + F44 新增条目
- `docs/v0-2-2/e2e-retro.md` 不动(历史 e2e 快照)

---

## Changelog

- 2026-05-10:**F44 PR #8 追加** — 用户 2026-05-10 反馈 `/usr/bin/cct` 已被
  Ubuntu `proj-bin`(PROJ GIS)占用 → F39 整体反向回滚为 PR #8。本 dev-plan
  加 §13 PR #8 任务清单 + 测试 baseline 表加一行。F39 PR #1 (§2) 全文不动。
- 2026-05-09:初稿。基于 `docs/v0-2-2/prd.md` + V0.2 dev-plan 风格参考拆 7 PR
  (F39 / F34+F37 / F35 / F36 / F38 / F40 / chore),~1.65 kLOC / ~3 周。每 PR
  worktree subagent briefing 模板就位,subagent 拿到模板 + 进 worktree 即可开干。
