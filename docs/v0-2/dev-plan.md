# V0.2 开发计划

> 版本实施计划。按依赖顺序拆分 8 个 sub-milestone(M0.16 - M0.23)。
> 每个 milestone 对应 1-2 PR。
>
> 配套文档:
> - 需求决策:`docs/v0-2/prd.md`
> - 设计依据:`docs/v0-2/alignment-review.md`
> - 子设计:`docs/v0-2/phase-prompt-architecture.md`
>
> CLAUDE.md 约定:**`docs/v0-1/development-plan.md` 只追当前 in-flight 任务**;V0.2 进入
> in-flight 时,`docs/v0-1/development-plan.md` reference 本文档,不复制。

---

## 1. Milestone 总览

| # | 名 | 关联 PRD | 主要前置 | 工程量估算 |
|---|---|---|---|---|
| **M0.16** | Anti-pattern foundation | §6.4 / §6.3 / §6.2 | (无) | ~500 LOC, ~3 天 |
| **M0.17** | Team layout + TEAM_SOURCES | §5.1 / §5.2 | M0.16 | ~600 LOC, ~4 天 |
| **M0.18** | Phase prompt architecture | §5.3 + §6.1+§6.5 (候选 1/8) | M0.17 | ~800 LOC, ~5 天 |
| **M0.19** | Self-loop + AskUserQuestion 拦截 | §2 + §2.4 | M0.18(协议关键字稳定) | ~400 LOC, ~3 天 |
| **M0.20** | Plugin pipeline cleanup | §6.5(候选 7) | M0.16(基础设施) | ~300 LOC, ~2 天 |
| **M0.21** | Watchdog | §3 | M0.19(needs_attention.outbox) | ~500 LOC, ~4 天 |
| **M0.22** | Team factory(plugin 格式) | §4 | M0.17 + M0.18 + M0.20 | ~1200 LOC, ~6 天 |
| **M0.23** | Side fixes | §7 V0.2 必做 | (各独立) | ~400 LOC, ~3 天 |

**总计**:~5 kLOC + ~150 测试,~6 周(单人 5 天/周)。

### 依赖图

```
M0.16 (foundation)
  ↓
  ├─→ M0.17 (team layout) ─┐
  ├─→ M0.20 (plugin pipe) ─┤
  └─→ M0.23 (side fixes) ──┤   (M0.20 / M0.23 跟 M0.17 独立)
                            ↓
                          M0.18 (phase prompt)
                            ↓
                          M0.19 (self-loop)
                            ↓
                          M0.21 (watchdog)
                            
                          M0.22 (factory) — 依赖 M0.17 + M0.18 + M0.20
```

并行机会:M0.17 / M0.20 / M0.23 可同时开三个 worktree(独立编辑面)。

---

## 2. M0.16 Anti-pattern foundation

> **目标**:清理 ccteam-core 中违反"无 team 字面量"红线的反模式,为后续 milestone 提供干净基础。

**关联 PRD**:§6.2(候选 2)+ §6.3(候选 3)+ §6.4(候选 5)

### 任务

- [ ] **M0.16.1** `TeamSpec` 加 declarative flag 替代 `if team == META_TEAM_NAME`
  - 加字段:`evergreen: bool` / `phase_dag: Option<DagSpec>` / `cost_policy: CostPolicy enum {None, Track, KillAt(f64)}`
  - 修 `orchestrator.rs:553, 612, 1239, 1303, 1378` 5+ 处分叉 → 读 spec flag
  - meta-agent team.yaml 写 `evergreen: true, cost_policy: None`
  - `meta_agent.rs:11` 注释删
  - 测试:任意 team 设 `evergreen: true` 都走同路径
- [ ] **M0.16.2** `TEAM_BUNDLES` const → seed-on-bootstrap
  - 删 `templates.rs:30-107` `const TEAM_BUNDLES`
  - 改 `bootstrap_project` 为:首次启动时拷 ship 内 `teams/dev/` `teams/product-research/` 到 `~/.ccteam/teams/`(若不存在)
  - 改 `memory_bridge.rs::TEAMS` const → 扫 `~/.ccteam/teams/*/team.yaml` 找 `retro_schema` 非空的
  - 加 `--reset-shipped-teams` flag 重新拷
  - 测试:删除 `~/.ccteam/teams/`,bootstrap 后再生
- [ ] **M0.16.3** `render_project_claude_md` 模板化
  - `team.yaml` 加字段 `claude_md_template: |` (multi-line string) 或文件引用 `claude_md_template_path: <relpath>`
  - 倾向后者:`teams/<name>/CLAUDE.md.tmpl` 单文件,可读性高
  - 删 `projects.rs:186-198` `match team` 分支
  - dev / product-research team.yaml 各自补 template 文件
  - 测试:加新 team 不改 ccteam-core,CLAUDE.md 内容据 team.yaml 渲染

### 验收

- [ ] `grep -rE "\"dev\"|\"product-research\"|\"meta-agent\"" crates/ccteam-core/src/` 命中 0 次(注释 / 测试除外)
- [ ] `if team == META_TEAM_NAME` grep 命中 0 次
- [ ] `const TEAM_BUNDLES` / `const TEAMS` (memory_bridge) 不存在
- [ ] e2e 测试:删 `~/.ccteam/teams/`,`ccteam new --team dev` 仍 pass
- [ ] e2e 测试:`ccteam new --team meta-agent`(已有 team)走 evergreen 路径
- [ ] 369 baseline 不退步

### 文档同步

- `docs/tech-design.md` §3 加 evergreen flag 描述
- `docs/interfaces.md` §5.5 `team.yaml` schema 加新字段
- `docs/dev-coupling-audit.md` 关闭候选 2/3/5

---

## 3. M0.17 Team layout + TEAM_SOURCES

> **目标**:仓内 team 整目录化 + 三层加载优先级。

**关联 PRD**:§5.1 / §5.2

### 任务

- [ ] **M0.17.1** 仓内 team 整目录迁移
  - `git mv phases-product-research/ teams/product-research/phases/`
  - `git mv phases/ teams/dev/phases/`
  - `git mv phases-research/ teams/research-academic/phases/`(backlog,但布局统一)
  - `git mv teams/dev.yaml teams/dev/team.yaml`
  - `git mv teams/product-research.yaml teams/product-research/team.yaml`
  - 保 git history(不重写)
- [ ] **M0.17.2** `team.yaml.phase_dir` 语义改"team 目录相对路径",默认 `phases`
  - serde alias 兼容旧值(`phases-product-research` 等);old → 新映射逻辑写在 `TeamSpec::load`
  - 删除所有 `phases-` 字面量(grep `crates/ccteam-core/src/` `crates/ccteam-cli/src/`)
- [ ] **M0.17.3** `TEAM_SOURCES` enum + 三层加载
  - 新文件 `crates/ccteam-core/src/team_resolver.rs`
  - `enum TeamSource { Project, User, Repo }`,`const TEAM_SOURCES: &[TeamSource]`
  - `resolve_team(name) -> Result<TeamSpec>` 按数组顺序 first-source-wins
  - 缺层 ENOENT fall-through;yaml 错读容错(warn + 下一层);写严格(yaml 错 reject 不覆盖)
  - per-source cache + 显式 invalidate
- [ ] **M0.17.4** 加载路径切换
  - `orchestrator.rs::load_team_runtimes` 改用 `team_resolver`
  - `bootstrap_project` 用 resolver 写到对应层(默认 user)

### 验收

- [ ] `grep -rE "phases-(product-research|research)?" crates/` 命中 0 次
- [ ] 旧 state.json(phase_dir 写 `phases-product-research`)serde alias 兼容加载
- [ ] e2e 测试:同名 team 在 project / user / repo 三层都存在,加载到 project 那个
- [ ] e2e 测试:user 层 team.yaml 故意写坏,fall-back 到 repo,warn 一条
- [ ] 369 baseline 不退步

### 文档同步

- `docs/interfaces.md` §1.1 / §5.5 路径布局更新
- `docs/tech-design.md` §3 加 TEAM_SOURCES 设计

---

## 4. M0.18 Phase prompt architecture(§5.3 D + 候选 1/8)

> **目标**:协议外移到 orchestrator inject prompt,phase markdown 正文 100% 领域。

**关联 PRD**:§5.3 + §6.1(候选 1)+ §6.5(候选 8)

详见 `docs/v0-2/phase-prompt-architecture.md` §11 详细步骤。

### 任务

- [ ] **M0.18.1** frontmatter 字段补齐
  - `phases.rs::PhaseTemplate` 加:`completion_signal: Option<String>` / `escalate_grammar_ref: Option<String>` / `outbox_question_protocol: Option<String>` / `inject_directives: Option<Vec<String>>`
  - 默认值常量;serde alias 兼容旧 yaml
- [ ] **M0.18.2** inject prompt 模板化
  - `progress.rs::build_phase_prompt_with_attachments` 升级签名接 `&PhaseTemplate`
  - 据字段差异化拼装(详见 phase-prompt-architecture.md §5)
  - 最终 short prompt ≤ 1 KB(property test)
  - 旧调用点同步迁移
- [ ] **M0.18.3** team.yaml `golden_rules` 拆 `protocol` / `domain`
  - `team.rs::TeamSpec` 加嵌套结构
  - 旧扁平 list serde alias 当 `protocol` 加载
  - inject prompt 据 `protocol.*.enforce: prompt_directive` 注入
- [ ] **M0.18.4** 12 个 phase markdown 正文清理
  - 删 `PHASE_DONE: <name>` / `ESCALATE: <prefix>` 关键词
  - 删 `完成后写 .ccteam/<...>-report.md` 协议级文件路径声明(已被 frontmatter `required_outputs` 表达)
  - 保留纯领域(任务叙述、约束、报告内容要点)
- [ ] **M0.18.5** doctor 校验增量
  - `crates/ccteam-cli/src/doctor.rs` 加:frontmatter schema 完整 / phase IO 契约一致 / 正文非空 / 正文协议关键词残留 warn(不 fail)
  - 新命令 `ccteam doctor --validate-team <name>`
- [ ] **M0.18.6** `ccteam phase show <team> <phase>` 命令
  - 渲染最终 inject prompt + `@` 引用拉取的 phase markdown 正文
  - 调试 / 用户改 phase 时验证用

### 验收

- [ ] `grep -rE "PHASE_DONE|ESCALATE:" phases/ phases-product-research/` 0 命中(改造后路径)
- [ ] `grep -rE "PHASE_DONE|ESCALATE:" crates/ccteam-core/src/` 命中 ≤ 5 次(全在 inject prompt template 拼装位置 / phase YAML 字段读取)
- [ ] property test:任意 frontmatter 组合 inject prompt ≤ 1 KB
- [ ] property test:inject prompt 永远含 `@.ccteam/phases/<name>.md`
- [ ] 故意改 phase markdown 正文加废话 — phase 行为不变
- [ ] 故意删 frontmatter `completion_signal` — doctor fail-loud
- [ ] dev / product-research 两 team smoke e2e pass(各跑完一遍)
- [ ] 369 baseline + ~25 新测试

### 文档同步

- `docs/interfaces.md` §5.1 加 marker / 协议字段全集更新
- `docs/v0-2/phase-prompt-architecture.md` 已是 SoT,无需更新
- `docs/dev-coupling-audit.md` 关闭候选 1/8

---

## 5. M0.19 Self-loop + AskUserQuestion 拦截

> **目标**:phase auto_loop default-on + Stop hook 兜底 + PreToolUse 拦截 AskUserQuestion。

**关联 PRD**:§2 / §2.4

### 任务

- [ ] **M0.19.1** auto_loop default-on
  - `phases.rs::PhaseTemplate.auto_loop` 默认 false → true(或 team.yaml 级 default 字段)
  - 现有 fix-loop 路径仍正常
- [ ] **M0.19.2** Stop hook 兜底改造
  - `crates/ccteam-hooks/src/parse_phase_end.rs` 增逻辑:Stop 触发时扫 `.ccteam/` 找 phase_done / escalate / outbox 任一新文件
  - 三种都没且 `stop_hook_active != true` → exit 2 + stderr 写"phase 未正常收尾,请输出 PHASE_DONE / ESCALATE / 写 outbox 之一"
  - `stop_hook_active == true`(第二次)→ append `.ccteam/needs_attention.outbox.json`(payload 含 `last_assistant_message` payload 字段 + `tmux capture-pane` 末 30 行)
  - 不再依赖 ccteam orchestrator 主动 send-keys
- [ ] **M0.19.3** PreToolUse 拦截 AskUserQuestion
  - `bootstrap_project` 写 `.claude/settings.json` 时加 hook:
    ```json
    "PreToolUse": [{
      "matcher": "AskUserQuestion",
      "hooks": [{ "type": "command", "command": "ccteam-hooks intercept-ask" }]
    }]
    ```
  - 实现 `ccteam-hooks intercept-ask` 子命令:返回 `{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "本 phase 应自决,改写 outbox"}}`
- [ ] **M0.19.4** Golden_rules.protocol 加禁用 AskUserQuestion 红线
  - dev / product-research / meta-agent team.yaml 都加 `forbid_ask_user_question: true`(或 protocol golden_rule)
  - inject prompt 据此注入"询问用户唯一合法出口是 outbox"段(已在 §5.3 规划)

### 验收

- [ ] e2e:phase 内 LLM 输出纯文本问句 + Stop → 第一次 Stop hook exit 2,LLM 续聊;第二次撞 `stop_hook_active`,写 `needs_attention.outbox`
- [ ] e2e:phase 内 LLM 调用 AskUserQuestion → PreToolUse hook deny,LLM 收到 reason 改写 outbox
- [ ] e2e:用户离线 4 小时,phase 不停滞在"等用户输入"(除非真写 outbox)
- [ ] auto_loop cycle ≥ 3 → escalate(已有,不退步)
- [ ] 369 baseline + ~12 新测试

### 文档同步

- `docs/tech-design.md` §3.5 / §6.9 更新自循环行为
- `docs/interfaces.md` §6 hooks 配置 schema 加 PreToolUse intercept

---

## 6. M0.20 Plugin pipeline cleanup(候选 7)

> **目标**:删 `RECOMMENDED_AGENTS` ln -sf 路径,改用 spawned session `enabledPlugins`。

**关联 PRD**:§6.5(候选 7)+ review §2.2

### 任务

- [ ] **M0.20.1** `RECOMMENDED_AGENTS` const 删除
  - `crates/ccteam-core/src/tool_surface.rs:65-106` 整段删
  - `link_recommended_agents_for_phases_into` ln -sf 函数删
  - `ccteam doctor --install-recommended-agents` 命令 deprecate
- [ ] **M0.20.2** spawned project session 写 `enabledPlugins`
  - `bootstrap_project` 写 `.claude/settings.json` 时加 `enabledPlugins` 字段
  - 字段值据 phase YAML `tools_required.subagents` 推断(eg `code-reviewer` → `pr-review-toolkit@claude-plugins-official`)
  - 推断映射表写在 `crates/ccteam-core/src/plugin_resolution.rs`(新)
- [ ] **M0.20.3** `tools_required.subagents` 校验改写
  - doctor 不再校 `~/.claude/agents/<name>.md` 存在性
  - 改校:`enabledPlugins` 启用列表能解析出该 agent
  - subagent 名要求:plugin agent 走 `pluginName:agentName` 命名空间(Claude Code namespace 自动)
- [ ] **M0.20.4** 已有用户迁移
  - 加 `ccteam doctor --migrate-recommended-agents` 一次性命令:删 `~/.claude/agents/` 下 ccteam ln -sf 的 8 个 symlink
  - CLAUDE.md 加 release note 提醒

### 验收

- [ ] `grep -rE "RECOMMENDED_AGENTS|link_recommended_agents" crates/` 0 命中
- [ ] e2e:新 ccteam 项目,`Task(subagent_type="code-reviewer")` 仍能 spawn(走 plugin pipeline)
- [ ] e2e:`~/.claude/agents/` 里没有 ccteam ln -sf 的 symlink
- [ ] `ccteam doctor` 不再有 "ln -sf" 警告路径
- [ ] 369 baseline

### 文档同步

- `docs/tech-design.md` §6.4 plugin pipeline 章节加描述
- `docs/claude-code-tool-surface.md` 更新 — 删"必须 ln -sf"段
- `docs/dev-coupling-audit.md` 关闭候选 7

---

## 7. M0.21 Watchdog

> **目标**:meta-agent 升级为 watchdog(translation only,不 decide)。

**关联 PRD**:§3

### 任务

- [x] **M0.21.1** meta-agent role prompt 升级
  - `crates/ccteam-core/src/templates/meta_agent_role.md` 加 watchdog 部分(§7)
  - 描述 watchdog 角色边界(translation only,不做技术决策)
  - 列周期任务:`ccteam watchdog scan` + 优先级处理顺序
- [x] **M0.21.2** Watchdog 数据源
  - 信号 1:`needs_attention.outbox.json`(M0.19 Stop hook L3 兜底)
  - 信号 2:`<project>/.ccteam/auto-loop.state.md::iteration`(直接读 state 文件,
    dev-plan 原写"`progress.jsonl` 里 `auto_loop_cycle` 事件",代码里没这事件;
    `iteration` 字段是同语义的更直接来源)
  - 信号 3:phase 当前 cost / 时长超阈值(state.json 字段)
  - 信号 4:orchestrator daemon heartbeat stale(M0.23.1 已 ship)
- [x] **M0.21.3** 通知阈值配置
  - `~/.ccteam/watchdog.yaml`(新)— 用户级配置
  - 字段:`notify_on_cycle_count` / `notify_on_phase_cost_usd` / `notify_on_phase_duration_min` / `notify_mode`
- [x] **M0.21.4** Watchdog 通知机制
  - meta-agent 通过自己的 outbox(`~/projects/<handle>-meta/.ccteam/outbox/`)
    收 alert(`escalation` priority=high / `progress` priority=normal)
  - V0.2:**手动**触发(meta-agent 跑 `ccteam watchdog scan --push --user <handle>`);
    M2+ channel layer 上线后才有 cron-style 自动 timer

### 验收

- [x] e2e:auto-loop iteration ≥ 2 时 surface NL 通知 + 写 outbox
  (`crates/ccteam-core/tests/watchdog_e2e_test.rs::auto_loop_iteration_2_surfaces_alert_then_pushes_to_meta_outbox`)
- [x] e2e:`notify_mode: quiet` 静默 cycle 但 `daemon_down` / `cost_overrun` 仍 surface
  (`crates/ccteam-core/tests/watchdog_e2e_test.rs::quiet_mode_drops_cycle_alert_but_pushes_daemon_down_when_heartbeat_missing`)
- [x] watchdog 不改任何 orchestrator 行为(grep `crates/ccteam-core/src/orchestrator.rs` `watchdog` = 0)
  (额外:`watchdog_does_not_mutate_state_or_progress_jsonl` 验 state.json mtime / progress 计数不变)
- [x] 451 baseline + 21 新测试 = 472(原估 ~10,实际拆得更细;含 lib + e2e + cli)

### 文档同步

- [x] `docs/tech-design.md` §3.9 watchdog 章节
- [x] `docs/tech-design.md` §1 设计原则表加 "smart layer 只做 translation 不做 decision" 红线
- [x] `docs/interfaces.md` §12.5 `watchdog.yaml` schema + alert 输出契约
- [x] `docs/interfaces.md` §10.5 + §13 文件路径表 加 `ccteam watchdog scan` / `~/.ccteam/watchdog.yaml`

---

## 8. M0.22 Team factory(plugin 格式)

> **目标**:meta-agent 内嵌 `ccteam-team-author` skill,对话式产出 team-plugin。

**关联 PRD**:§4

### 任务

- [ ] **M0.22.1** `ccteam-team-author` skill 设计
  - 新 skill `crates/ccteam-core/src/skills/ccteam-team-author/SKILL.md`
  - 跟 `ccteam-control` 并列
  - 引导 meta-agent 跟用户对话:phase 列表 / tools / golden_rules / retro_schema / verdict_schema / plugin metadata(name, description, author)
- [ ] **M0.22.2** 工厂产物结构
  - 落 staging: `~/.config/ccteam/teams/<name>/`
  - 内容:`.claude-plugin/plugin.json` + `team.yaml` + `phases/` + 必要的 `agents/` `commands/` `hooks/hooks.json` `.mcp.json`
  - phase markdown 模板:frontmatter 字段填好 + 正文模板(用户后续可改)
- [ ] **M0.22.3** `ccteam team publish <name>` 命令
  - staging → plugin repo 转换路径
  - 选项 1:`--target local`:链接到 `~/.claude/plugins/marketplaces/ccteam-local/plugins/<name>/`(directory source)
  - 选项 2:`--target github`:`gh repo create` + push,产出 `source: github` 引用
  - 用户拿到的 share link 是 GitHub URL 或 directory path
- [ ] **M0.22.4** doctor `--validate-team` 加强
  - 校 plugin manifest schema(reuse Claude Code zod schema)
  - 校 `team.yaml` 是合法顶级 unknown 字段(不会被 plugin pipeline 报错)
  - 校 `enabledPlugins` 引用的 plugin 真存在

### 验收

- [ ] e2e:meta-agent 走完工厂对话,产出 `~/.config/ccteam/teams/<test-team>/`
- [ ] e2e:`ccteam doctor --validate-team <test-team>` pass
- [ ] e2e:`ccteam team publish <test-team> --target local` → `ccteam new --team <test-team>` 跑通
- [ ] e2e:产物的 `plugin.json` 用 Claude Code 自己的 plugin install 命令也能加载(不报错)
- [ ] 369 baseline + ~15 新测试

### 文档同步

- `docs/tech-design.md` 加 §X team factory 章节
- `docs/interfaces.md` 加 plugin manifest 兼容字段说明
- 新建 `docs/v0-2/team-factory-userguide.md` 用户指南(简短,~100 行)

---

## 9. M0.23 Side fixes(已知未决项)

> **目标**:V0.2 必做的独立 bug / hardening。

**关联 PRD**:§7 V0.2 必做行

### 任务

- [ ] **M0.23.1** orchestrator daemon health supervision
  - daemon 写 `~/.ccteam/state/orchestrator.pid` + `~/.ccteam/state/orchestrator.heartbeat`(每 30s;沿用 M1.5 `state/` 子目录约定 + `orchestrator.*` 命名)
  - MCP 任意命令入口 + meta-agent skill 启动时 health check
  - daemon 死亡时:meta-agent 立即 surface "daemon down",MCP 命令 fail-loud
- [ ] **M0.23.2** 1M context 默认启用
  - `bootstrap_project` 启动 claude session 时加 `--model claude-sonnet-4-6[1m]` 或等价 flag
  - 加 F<N> 进 dev-coupling-audit
- [ ] **M0.23.3** inbox 文件派发可靠性
  - `send_to_session` MCP tool 实现里:写 inbox 后立即检查 orchestrator dispatcher 是否 ack
  - 若未派发(eg orchestrator 未运行),返回 error 而非默认成功
- [ ] **M0.23.4** F22 收尾(slug 团队前缀的 e2e 测试覆盖)— 如有遗漏

### 验收

- [ ] e2e:kill daemon → meta-agent / MCP 立即 surface "daemon down"
- [ ] 新项目 claude session `claude --version` / `/status` 显示 1M context
- [ ] `send_to_session` 在 daemon 死时 fail-loud 而非默认 ack
- [ ] 369 baseline + ~8 新测试

### 文档同步

- `docs/dev-coupling-audit.md` 加 / 关闭 F<N> 条目
- `docs/tech-design.md` §6.8 daemon supervision 段

---

## 10. PR 实施顺序建议

按依赖 + 并行机会:

| 周 | PR | Milestone |
|---|---|---|
| 1 | PR-A: M0.16 | foundation(必先做) |
| 2 | PR-B: M0.17 + PR-C: M0.20 + PR-D: M0.23 | 三 worktree 并行 |
| 3 | PR-E: M0.18 | phase prompt 架构 |
| 4 | PR-F: M0.19 | 自循环 |
| 5 | PR-G: M0.21 + PR-H: M0.22(starts) | watchdog 完成 + factory 启动 |
| 6 | PR-H: M0.22(完成) | factory ship + V0.2 收尾 |

每 PR 必含:
- PR 描述映射 PRD 章节(eg "Closes V0.2 §6.4 / 候选 5")
- 369 baseline + 新测试列表
- 文档同步清单(checkbox)
- `dev-coupling-audit.md` F-finding 关闭(若适用)

---

## 11. 红线检查 / 测试策略

每 PR commit 前必查:

- [ ] `grep -rE "\bdev\b|\bproduct-research\b|\bmeta-agent\b" crates/ccteam-core/src/`
  命中只在注释 / 测试 / 字符串字面量做日志
- [ ] `grep -rE "PHASE_DONE|ESCALATE:" crates/ccteam-core/src/`(M0.18 后)命中 ≤ 5 次
- [ ] `grep -rE "phases-(product-research|research)" crates/`(M0.17 后)命中 0 次
- [ ] `grep -rE "RECOMMENDED_AGENTS" crates/`(M0.20 后)命中 0 次
- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace --all-targets` 不新增 warning(4 pre-existing 不算)

---

## 12. 文档同步矩阵

每 milestone ship 时必须同步:

| Milestone | docs/tech-design.md | docs/interfaces.md | docs/dev-coupling-audit.md | 其他 |
|---|---|---|---|---|
| M0.16 | §3 evergreen flag | §5.5 team.yaml | 候选 2/3/5 关 | — |
| M0.17 | §3 TEAM_SOURCES | §1.1, §5.5 | — | — |
| M0.18 | — | §5.1 frontmatter | 候选 1/8 关 | phase-prompt-architecture 已 SoT |
| M0.19 | §3.5 / §6.9 | §6 hooks | — | — |
| M0.20 | §6.4 plugin pipeline | — | 候选 7 关 | claude-code-tool-surface |
| M0.21 | §X watchdog | watchdog.yaml | — | 加 smart-layer 红线 |
| M0.22 | §X team factory | plugin manifest | — | 新建 team-factory-userguide |
| M0.23 | §6.8 supervision | — | F<N> 关 | — |

---

## Changelog

- 2026-05-07:初稿。基于 docs/v0-2/prd.md + alignment review 拆 8 个 milestone
  (M0.16-M0.23),~5 kLOC / ~6 周。
