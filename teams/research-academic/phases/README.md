# phases-research/ — research 团队 phase 集

> 本目录是 ccteam 上**第一个非 dev 团队**的 phase 集草案。论证、契约、与 dev
> 团队的差异点都在 [`docs/ccteam-as-domain-agnostic-orchestrator.md`](../docs/ccteam-as-domain-agnostic-orchestrator.md)。
>
> **状态**:草案。orchestrator 的 `--team` 参数与 `auto_loop` / `completion_signal`
> 字段在 M4.5.1 落地之前,本目录**不能直接被 ccteam 调度**——属于"先把契约
> 写清楚,等 mechanism 就绪后挂上"。详见 `docs/dev-coupling-audit.md` 修复
> 路线 PR A → PR D。

## 八 phase 概览

| # | phase | 完成信号 | 等价 dev phase | 与 dev 的关键差异 |
|---|---|---|---|---|
| 00 | `topic` | `TOPIC_SCOPED` | plan-ceo | 输出"决策问题",不是"产品规划" |
| 01 | `desk` | `DESK_COMPLETE` | code-explorer 探索 | 信息源是已有资料,不是 codebase |
| 02 | `hypothesis` | `HYPOTHESES_SET` | plan-eng | 假设必须可证伪 |
| 03 | `method` | `METHOD_DESIGNED` | (无对应) | 决定怎么收集一手数据;research 独有 |
| 04 | `primary` | `PRIMARY_GATHERED` | implement | **必然有外部等待**(用户 / 受访者) |
| 05 | `synthesis` | `INSIGHTS_TRIANGULATED` | test-author + test-run | "多源交叉验证" 替代 "tests pass" |
| 06 | `report` | `REPORT_READY` | ship | 交付物是 markdown,不是代码 |
| 07 | `retro` | `RETRO_RECORDED` | retro | 字段不同:方法学反思 vs 技术决策 |

**没有 `fix` phase**——research 的"假设被反驳"不是 fix,而是回到 02-hypothesis
重设(走 `ESCALATE: HYPOTHESIS_REJECTED — REVERT_TO_PHASE 02-hypothesis`),
或接受负面结果直接写报告(走 PHASE_DONE: synthesis 携带"假设被反驳"insight)。

**auto_loop phase**:`04-primary` 与 `05-synthesis` 标记 `auto_loop: true`
(M4.5.1 字段就绪后)——前者循环到 ≥3 来源拿到,后者循环到所有 insight 都
≥2 来源支撑。dev 的 fix 走 ralph 范式,research 这两个 phase 同范式不同
触发条件。

## 完成信号定义(对应 strategic doc §2.5)

| signal | 何时输出 | orchestrator 应做的 |
|---|---|---|
| `PHASE_DONE: <phase>` | phase 任务全部满足 done criteria | 转下一 phase |
| `TOPIC_SCOPED` / `DESK_COMPLETE` / ... | (M4.5.1+)phase 内 auto_loop 满足时输出 | Stop hook 放行退出 |
| `ESCALATE: <prefix> — <reason>` | 见下表 ESCALATE grammar | 按 prefix 路由 |

## ESCALATE grammar 扩展(对应 strategic doc §2.4)

```yaml
escalate_prefixes:
  - prefix: HYPOTHESIS_REJECTED
    route: REVERT_TO_PHASE
    target_phase: 02-hypothesis
    reason: "一手数据反驳了某条假设——回到 hypothesis 重设"
  - prefix: SOURCE_UNAVAILABLE
    route: NEED_USER_INPUT
    reason: "关键一手数据来源拿不到——用户决定替代来源 / 缩范围 / 放弃"
  - prefix: METHOD_INSUFFICIENT
    route: REVERT_TO_PHASE
    target_phase: 03-method
    reason: "选定方法不能证伪假设——回到 method 重设"
  - prefix: ETHICAL_CONCERN
    route: NEED_USER_INPUT
    reason: "拟定方法触发研究伦理担忧(例:暗访、未经同意录音)——必须用户拍板"
  - prefix: DATA_AMBIGUOUS
    route: NEED_USER_INPUT
    reason: "synthesis 阶段多源数据矛盾,议事拍不了板——用户取舍方向"
  - prefix: SCOPE_DRIFT
    route: NEED_USER_INPUT
    reason: "phase 中产生的发现超出 topic.md 声明范围——用户决定扩 scope 还是切下个项目"
```

兜底(无前缀)= `NEED_USER_INPUT`,与 dev 团队一致。

## 推荐 plugin / agent 清单(对应 strategic doc §2.6)

dev 团队推荐的 8 个 plugin agent 中,**只有 `code-explorer` 在 research 团队
有用**(改用作"已有资料探索"角色——见 01-desk.md)。其它 7 个
(`code-reviewer` / `code-architect` / `code-simplifier` /
`silent-failure-hunter` / `pr-test-analyzer` / `type-design-analyzer` /
`comment-analyzer`)与代码强耦合,research 不装。

research 真正需要的 agent(`method-critic` / `source-quality-critic` /
`triangulation-checker` / `bias-watcher`)在 claude-plugins-official 没有
对应。按 strategic doc §2.6 与 [`claude-code-tool-surface.md`](../docs/claude-code-tool-surface.md)
§1.1.3 方案 B 兜底:在 phase body 里显式 `Task(subagent_type="general-purpose",
prompt="<内联 critic role 描述>")`,文档先行,M4.6+ 再考虑独立 plugin。

## team.yaml 占位

(M4.5.2 落地 `team.yaml` schema 后,本节内容迁到 `~/.ccteam/teams/research.yaml`。)

```yaml
name: research
phase_dir: phases-research
entry_phase: 00-topic
critic_dimensions:
  - name: source_diversity
    weight: 0.25
    weak_threshold: 0.4
    rubric: "0.0 = 单一来源;1.0 = ≥3 独立一手来源经过交叉验证"
  - name: hypothesis_falsifiability
    weight: 0.20
    weak_threshold: 0.5
    rubric: "0.0 = 不可证伪 / 同义反复;1.0 = 给定一组观测能明确证伪"
  - name: method_appropriateness
    weight: 0.20
    weak_threshold: 0.5
    rubric: "0.0 = 方法与问题脱钩;1.0 = 样本 + 工具能采到证伪所需数据"
  - name: triangulation_strength
    weight: 0.15
    weak_threshold: 0.4
    rubric: "0.0 = 单源支撑结论;1.0 = 每条 insight ≥2 独立来源"
  - name: insight_actionability
    weight: 0.20
    weak_threshold: 0.5
    rubric: "0.0 = '有趣但用不上';1.0 = 直接驱动决策"
escalate_prefixes: [...]                # 见上表
recommended_agents: []                  # 见上节;暂无适配 plugin
recommended_skills: [ccteam-control]    # 与 dev 共用
recommended_mcp: [Telegram]             # 一手数据回收通道
memory_namespace: team_only
on_loop_exhaust: escalate               # 一手数据收集失败时升级,不自动 abort
retro_schema:
  - { field: research_question, type: text }
  - { field: methods_used, type: text }
  - { field: source_quality, type: rubric, scale: 0..1 }
  - { field: hypothesis_outcomes, type: text }
  - { field: would_redo_method, type: bool }
  - { field: insights_per_dollar, type: number }
artifacts:
  topic: topic.md
  desk: desk.md
  hypothesis: hypotheses.md
  method: method.md
  primary_dir: primary/
  primary_index: primary/index.md
  synthesis: synthesis.md
  report: report.md
  retro: retro.md
danger_command_patterns:
  - pattern: "curl .*api/send.*"
    reason: "research 不该直接发受访者邮件——经 ccteam fork-reply 走人审"
  - pattern: 'rm -rf .*/primary/'
    reason: "保护一手数据不可逆删除"
```

## happy path 心跑一遍(strategic doc §C 纪律 5)

研究员开 ccteam → `ccteam new --team=research "为什么我们的 PWA 离线
缓存 30 天后用户流失率突然抬头?"`:

1. **00-topic**:claude 把 brief 收敛成"决策问题":这个研究将驱动什么
   决策?(例:决定是否重构离线缓存策略)。**escalate 风险**:brief 太
   模糊——`ESCALATE: NEED_USER_INPUT — 这个研究输出会驱动什么具体决策?
   重构、灰度、还是放弃 PWA 离线?`
2. **01-desk**:已有 web vitals / Sentry log / 用户调研历史扫一遍。**自然
   产物**:`desk.md` + 4 条已知信息缺口。
3. **02-hypothesis**:基于 desk 缺口,提 ≥3 条可证伪假设。**escalate
   风险**:某条假设无法证伪——`ESCALATE: METHOD_INSUFFICIENT — 假设 H2
   '用户不知道有离线模式' 不可证伪,因为我们没有 in-app 调研通道`。
4. **03-method**:为每条假设设计采集方法 + 样本框。`Task(subagent_type=
   "general-purpose")` 启 method-critic 子 agent 审"方法能否证伪假设"。
5. **04-primary**:发起一手数据收集——这一步**必然外部等待**(用户访谈
   排期、调研问卷回收周期)。phase 模板把"等用户回 inbox 数据"作为合
   法 NEED_USER_INPUT 状态;orchestrator 收到 `answer-<slug>.md` 后续推。
   **关键 escalate**:某来源拿不到——`ESCALATE: SOURCE_UNAVAILABLE —
   3/5 受访者拒绝访谈,要不要扩样本框 / 改用问卷 / 放弃这条假设?`
6. **05-synthesis**:跨假设 / 跨来源 triangulate。`Task(...)` 启
   triangulation-critic 审"每条 insight 是否有 ≥2 独立来源支撑"。
   **escalate 风险**:数据矛盾——`ESCALATE: DATA_AMBIGUOUS — H1 与 H3
   的证据互相打架,一组指向缓存策略问题、另一组指向通知策略问题。请
   选 [A] 收窄到缓存 [B] 收窄到通知 [C] 拆两个项目分别深挖。`
7. **06-report**:把 synthesis 改写成决策友好格式;ship。
8. **07-retro**:写 `retro.md` 按 retro_schema;研究员的方法学反思进
   memory,下个 research 项目自动召回。

跑得通——8 phase 顺序在 happy path 上没有死锁,ESCALATE 路由都有出口。

## 后续

详见 [strategic doc §6.1 M4.5 任务清单](../docs/ccteam-as-domain-agnostic-orchestrator.md#61-m45--team-abstractionm本文档对应里程碑约-2-周)
M4.5.4 任务"phases-research/ 起草纳入仓库"——本目录草案合入主线即关闭该任务。
