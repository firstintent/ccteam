# ccteam 开发计划

> 本文从 [tech-design.md](./tech-design.md) §7 拆出并扩展。
>
> - `requirements.md` 回答 **为什么做**(10 条痛点)
> - `tech-design.md` 回答 **怎么做**(架构、协议、扩展点)
> - **本文回答 何时做什么、做到什么标准、谁阻塞谁**——是单一权威的进度文档,PR 必须能映射回本文某条任务。

---

## 0. 总体节奏

| 里程碑 | 时长 | 主目标 | 关键解锁的痛点 |
|---|---|---|---|
| **M0** | 2–3 周 | 单项目 CLI MVP——一句话需求 → 自动跑出能用的代码 | 1, 2, 3, 4, 7, 8, 9 |
| **M1** | 2 周 | 多项目并发 + Telegram 入口 | 5, 9(强化) |
| **M2** | 2 周 | Seed Gate 否决无效想法 + Score 客观质量门 | 6, 3(强化) |
| **M3** | 3 周 | 跨项目记忆(差异化护城河) | 10 |
| **M4** | 3 周 | Critic Agent 闭环——超越"测试通过=完成" | 3(深化) |
| **M5** | 3–6 月 | 大型软件长跑能力(对标 Symphony) | 长期 |

**累计**:M0–M4 大约 12–13 周,基本覆盖 requirements.md 所有痛点。M5 进入开放探索期。

---

## 1. 痛点 → 里程碑反向映射

每条痛点应能指出"在哪个里程碑被解决到什么程度":

| 痛点 | M0 | M1 | M2 | M3 | M4 |
|---|---|---|---|---|---|
| 1. 想法死于"开始" | 跑通端到端 | — | — | — | — |
| 2. AI 仍要求当 PM | 跑通自治流水线 | — | Seed 不再问"做不做" | — | Critic 不再问"够不够好" |
| 3. 测试是黑洞 | tests-pass 即终态 | — | golden-rules + 6 维评分 | — | anti-leniency + WEAK BLOCK |
| 4. bug 修复无限循环 | fix-loop 上限 3 + escalate | — | — | — | — |
| 5. 想法多全部烂尾 | — | 多项目并发 + 排队 | — | — | — |
| 6. 不是每个想法都值得做 | — | — | Seed REJECT/CLARIFY | — | — |
| 7. 进度不透明 | tmux + progress.jsonl | — | — | — | — |
| 8. 每步都点允许 | `--dangerously-skip-permissions` | — | — | — | — |
| 9. AI 团队需要主持 | 守护进程 + tmux long session + CLI `--format json` | Telegram 入口替代 CLI + `ccteam-control` skill(用户自带 claude 当入口) | `ccteam-mcp` MCP server | — | — |
| 10. 每项目从零开始 | — | — | — | RAG 召回 + 反模式 | — |
| 11. 关键节点不把控 | L1 架构约束(hooks + required_outputs) | cross-cutting watcher 上线 | 单 critic + dev 隔离;L3 telegram fork 决策 | phase 内 audit 矩阵 + 投票共识 | anti-leniency + WEAK BLOCK |
| 12. 工作流编排 | phase 主干 + `sub_skills` 字段定义(空允许) | — | sub-skill 自动 trigger + 产物接力 | 新插件按 `skill_intent.yaml` 自动挂载 | — |
| 13. 并行规模 | phase 模板 `parallelism: solo` 字段(只此一档) | — | `parallelism: agent_team` 启用(implement phase 多角色并行) | `parallelism: multi_session` 启用(大项目子模块独立 session) | 自动并行规模识别 |

**门槛规则**:某个里程碑若未真正解决其声明的痛点,**不许跳到下一个里程碑**——这是质量门,不是日历推进。

---

## 2. M0 — 单项目 CLI MVP(active)

**唯一验收**:用 CLI 提一个需求 → 关掉所有终端 → 半小时后回来 → 看到一个能跑的项目 + 测试报告。

### 2.1 任务清单

| # | 任务 | 验收(可执行) | 依赖 | 对应 tech-design 章节 |
|---|---|---|---|---|
| **W1 — Foundation** |
| M0.1 | 仓库骨架 | `mkdir orchestrator/ phases/ hooks/ cli/ tmux/`;`pyproject.toml` 可 `pip install -e .` | — | §4 目录现状 |
| M0.2 | 5 个最小 phase 模板 | `phases/{02-plan-eng,03-implement,04-test-author,05-test-run,06-fix,09-ship}.md` 都带 YAML front matter(必含 `name` / `required_inputs` / `required_outputs` / `parallelism` / `agent_team` / `sub_skills` 字段——M0 `parallelism: solo` 写死,`sub_skills` 列表可空),可被 orchestrator 解析 | — | §3.3、§6.10、§6.11 |
| M0.3 | 3 个 hook 脚本 | `progress-append.sh` / `parse-phase-end.sh` / `cost-accumulate.sh`;手动喂 stdin JSON 验证输出 | — | §6.2 |
| M0.4 | 产出项目 settings.json 模板 | 模板渲染后挂到示例项目,`claude` 启动后 SessionStart hook 写出 ready 标记 | M0.3 | §6.2 |
| **W2 — Orchestrator** |
| M0.5 | state.json schema + 原子读写 | `.tmp` + rename;启动校验 schema;损坏走 backup | M0.1 | §5.2 |
| M0.6 | orchestrator 主循环 | `asyncio` 30s 轮询 + inotify 监听 progress.jsonl;`ccteam start --foreground` 跑得起;**解析 phase 模板的 `sub_skills` 字段时空列表 no-op 不报错**(M2 才真正调度);**`parallelism` 字段读到非 `solo` 报错并 fail-fast**(避免静默走错路径) | M0.5 | §3.2、§6.10、§6.11 |
| M0.7 | tmux session 启动 + 重连 | 首启用 `tmux new-session -d ... claude`;重启走 `tmux has-session` + `kill -0 pid` 双重校验 | M0.6 | §6.1 |
| M0.8 | idle-aware 注入 | tail progress.jsonl 末尾事件;`Stop`/`idle_prompt` → `send-keys`;否则 `/btw` | M0.7 | §6.9 |
| M0.9 | PHASE_DONE/ESCALATE 解析 + 状态机转移 | Stop hook 解析 claude 最后一行;orchestrator 收到事件后按 §3.2 状态机切换 | M0.8 | §3.2、§4.4 |
| M0.10 | context 60% 边界 reset | PostToolUse hook 累加 token;在下一个 phase 边界 `/exit` + 新 session + `.ccteam/CLAUDE.md` 桥接 | M0.9 | §6.9 |
| **W3 — CLI + Fix-loop + 兜底** |
| M0.11 | 核心 CLI 命令 | `ccteam new` / `ls` / `show` / `start` / `attach <slug>` / `resume <slug>` / `peek` / `progress` 全部可用;**所有查询命令支持 `--format json`**(`ls --format json` / `show --format json` 必出,schema 见 interfaces §10.3)——为"用户自带 claude 当入口"路径打底 | M0.6 | §3.8、§10.1、§10.3 |
| M0.12 | fix-loop 集成 ralph-loop Stop hook 模式 | 进入 fix phase 时 orchestrator 写 fix-loop 状态文件;Stop hook 检测到则按 ralph-loop 范式拦截退出 + 重喂同一段 fix prompt;3 次未过则清状态文件,orchestrator 接管 escalate | M0.9 | §3.5(改写) |
| M0.13 | stall 检测三档软告警 | 5/15/30 min 阈值触发不同等级日志(M0 暂用 stderr,M1 接 telegram) | M0.6 | §6.8 |
| M0.14 | 成本累计 + 软告警 + 物理上限 | hook 读 transcript_path,parse `usage.*` 累加 → state.json;阈值 $20/$50/$200 三档,$200 真 kill | M0.6 | §6.8 |
| M0.15 | 端到端打通 | 一次完整 happy path:CLI 提需求 → tmux session 自启 → 跑 plan-eng → implement → test → fix(若有)→ ship,全程不需要主对话 | M0.1–M0.14 | §4.1 |

### 2.2 M0 不做的事(scope guard)

明确排除,出现在 PR 描述里就要拒:

- 多项目并发(M1)
- Telegram bot(M1)
- Seed phase——M0 默认所有需求 PASS 直接进 plan(M2)
- 跨项目记忆 / RAG(M3)
- Score / Critic / 6 维评分(M2/M4)
- `--resume` 兜底崩溃恢复(M0 用 `/exit` + 新 session + CLAUDE.md 桥接;`--resume` 留作 M1+ 增强)
- web dashboard、score UI 之类的可视化(永远不在 ccteam 主线)

### 2.3 M0 主要风险

| 风险 | 触发场景 | 兜底 |
|---|---|---|
| Stop hook 未触发就卡死 | claude 进程 hung,既不 idle 也不工具调用 | M0.13 stall 检测兜底——30 min 升级 escalation |
| context reset 时 CLAUDE.md 桥接信息不全 | 写入"当前进度"节遗漏关键决策 | M0.10 在写桥接前要求 claude 自己输出"当前进度"摘要,orchestrator 拼接而非凭空写 |
| ralph-loop Stop hook 与 ccteam 自有 Stop hook 冲突 | 同时存在两个 Stop hook,顺序不确定 | M0.12 把 ralph-loop 风格逻辑合并到 ccteam 自己的 parse-phase-end.sh,**不**装两个 Stop hook |
| send-keys 注入与用户 attach 同时输入造成 race | 用户 ccteam attach 中键入,正好 orchestrator 也在 send-keys | tech-design §6.1:PreToolUse hook 检测输入源,orchestrator 检测 user_attach 立即暂停自动注入 |

---

## 3. M1 — 多项目 + Telegram(2 周)

**唯一验收**:周一在 Telegram 扔 5 个想法 → 周三早上看到 3 个交付 + 2 个还在跑。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M1.1 | Telegram bot 入口 | bot 收消息写 `~/.ccteam/inbox/`;接 notify 推送 escalation/shipped | M0.15 |
| M1.2 | 多项目并发调度 | `max_concurrent_projects=3` 准入;超出排队;每项目独立 tmux session | M1.1 |
| M1.3 | inbox/queue/control 协议落地 | 用户用 `echo` 写 `control/reject-<slug>` 真生效;`answer-<slug>.md` 接 CLARIFY 回答 | M1.2 |
| M1.4 | 项目级 CLAUDE.md 自动生成 | plan phase 后写;reset 时追加"当前进度"节 | M0.10 |
| M1.5 | 优雅停机 + 重启自恢复 | `ccteam stop` 不杀 session;`ccteam start` 自动 reattach 所有活跃 session | M1.2 |
| M1.6 | cross-cutting watcher agents(L2 起步) | `cost-watcher` + `scope-watcher` 实现;**Stop hook 触发**(每 phase 边界跑一次,不在 PostToolUse 跑——避免 300+ 次/phase 灌爆);输出 PASS/CONCERN/BLOCK,append `progress.jsonl`;BLOCK 写 `escalation.md` | M1.5 | §3.6 L2、§6.3 模式 B |
| M1.7 | L3 兜底:telegram fork 决策 | watcher BLOCK 或 fix-loop escalate 时,bot push ABC 选项;24h 默认通过;用户 reply A/B/C 注入下一 phase | M1.1、M1.6 | §3.6 L3 |
| M1.8 | `ccteam-control` skill 发行 | binary 内嵌 SKILL.md;`ccteam doctor --install-skill` 可写到 `~/.claude/skills/ccteam-control/`;字段约定 + body 必含章节按 interfaces §11 落地;装后用户在任意目录开 claude 能正确调用 `ccteam ls --format json` 等 | M0.11 | §3.8、§6.7、interfaces §11 |
| M1.9 | 多轮 CLARIFY 协议 | inbox 协议支持同一 slug 多次 `answer-<slug>-<n>.md` 追问;Phase 0 prompt 改成"可多轮澄清直到信息足够再 verdict";telegram bot 通道走通(用户连发多条 message 自动归并到当前 CLARIFY) | M1.1、M1.3 | §4.2 |

**M1 不做**:Seed phase 完整(M2)、score、跨项目记忆、agent 投票/共识(M3)、ccteam-mcp MCP server(M2)。

---

## 4. M2 — Seed Gate + Score(2 周)

**唯一验收**:提"AI 菜谱生成器" → Seed 直接 REJECT,附"已有 N 个免费同类工具";一个测试全绿但实现糙的项目得分低于阈值,自动进 fix-cycle。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M2.1 | Seed phase 模板与 verdict 解析 | YAML front matter 输出 `verdict: PASS/REJECT/CLARIFY`;orchestrator 据此分流 | M1.5 |
| M2.2 | CLARIFY 单问单答 | 强制 prompt 约束"只问一个问题";走 control/answer 回路 | M2.1、M1.3 |
| M2.3 | Score phase + 6 维加权 | 输出 `scorecard.md` 含 Functionality/Quality/Tests/UX/Speed/Docs + bug penalty | M1.5 |
| M2.4 | golden-rules.py 集成 | 抄 ccteam-creator 5 项 + 项目特定补充;phase `after` hook 调用,失败阻断 ship | M2.3 |
| M2.5 | Critic 与 dev 进程隔离(M2 简化版) | Score 阶段单独起子进程读 implement 产物,**禁止** dev 自评 | M2.3 |
| M2.6 | sub-skill 自动调度 | phase front matter `sub_skills` 被 orchestrator 自动 trigger;两档 trigger(`phase_start` / `phase_done`);产物按 `output_to` 落文件,自动作为下 phase prompt 的 `@文件引用`;复用 `claude-plugins-official:pr-review-toolkit/agents/code-reviewer` 验证一次端到端 | M2.5 | §6.10、§3.3 |
| M2.7 | `parallelism: agent_team` 启用 | implement phase 模板设 `parallelism: agent_team`;orchestrator 启用 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`;phase prompt 注入 backend-dev / frontend-dev / reviewer 三角色;Lead 协调下产物落 `.ccteam/<role>-output/` | M2.5 | §3.3、§6.3、§6.11 |
| M2.8 | `ccteam-mcp` MCP server | binary 出 `ccteam mcp-serve` 子命令(stdio 协议);暴露 `ccteam__ls` / `__show` / `__new` / `__peek` / `__progress` / `__pause` / `__resume` 七个 tool;`ccteam doctor --install-mcp` 写 `~/.claude.json`;装后用户自带 claude 通过 MCP 调度 ccteam 比 Bash 工具更鲁棒;ccteam-control skill body 改为推荐"优先 MCP,fallback Bash" | M1.8 | §3.8、§6.4、interfaces §12 |

**M2 不做**:RAG 召回(M3)、anti-leniency 严格规则(M4)、phase 内 audit 矩阵 / 投票(M3)、`parallelism: multi_session`(M3)、ccteam-mcp 写权限扩展(暂只暴露上述七个,attach/start/stop/memory rebuild 不暴露,见 interfaces §12.3)。

---

## 5. M3 — 跨项目记忆(3 周)

**唯一验收**:第二次提相似项目 → Seed 阶段 prompt 里出现"上次做过 X,建议复用 Y"。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M3.1 | retro phase 自动产出 pattern.md | 项目终态(shipped/rejected/escalated)触发;输出固定字段(tech stack/坑/成功设计/不要再做) | M2.5 |
| M3.2 | 向量索引(sqlite + sentence-transformers) | `~/.ccteam/memory/index.json` + 向量 db;`ccteam memory rebuild` 全量重建 | M3.1 |
| M3.3 | Seed phase 接 RAG 召回 | top-3 patterns 注入 Seed prompt;命中相似失败项目时 verdict 倾向 REJECT/CLARIFY | M3.2、M2.1 |
| M3.4 | anti-patterns 库 | REJECT 案例独立 namespace;召回时显式标注"不建议" | M3.1 |
| M3.5 | claude-mem MCP 接入(可选) | 若稳定则替换自建索引;不稳就跳过 | M3.2 |
| M3.6 | phase 内 audit 矩阵(L2 升级) | `architect` / `critic` / `designer` / `security` / `scope-watcher` 按 phase 启用清单跑;复用 `claude-plugins-official` 现成 agent | M2.5 | §3.6 L2 |
| M3.7 | agent 投票与共识机制 | M3.6 audit 输出 PASS/CONCERN/BLOCK;按 `yolo`/`balanced`/`careful` 信任档位决定是否上推 L3;分裂时弹用户 | M3.6、M1.7 | §3.6 L2/L3 |
| M3.8 | 新插件自动挂载(扩展性) | 扫 `~/.claude/plugins/.../skill_intent.yaml`;按推荐 phase 自动加进 phase 模板 `sub_skills` | M2.6 | §6.10 |
| M3.9 | `parallelism: multi_session`(大项目加速) | 实现 fan-out / fan-in 协议:plan-eng 输出子模块清单 + interface-contracts.md;orchestrator 起 N 个 sub-session;子模块独立跑 phase;review/ship 在 master fan-in;`max_sessions_per_project=4` 兜底;一次端到端验证(SaaS demo:backend ∥ frontend ∥ docs) | M2.7、M1.5 | §6.11 |

**M3 不做**:跨项目接口契约管理(M5)、自动子模块切分(M5)、跨子模块 stop-the-world 重构(M5)。

**M3 风险**:9 条任务对 3 周窗口偏紧(尤其 M3.6/3.7/3.9 都是新机制)。落地时按以下顺序削:M3.5(claude-mem MCP,可选)→ M3.4(anti-patterns,可推 M4)→ M3.8(新插件自动挂载,可推 M4)。M3.9 不可削——它是痛点 13 的最终落地。

---

## 6. M4 — Critic Agent 闭环(3 周)

**唯一验收**:测试全绿但 critic 发现"接口不优雅" → 自动进 fix-cycle 而非直接 ship。

| # | 任务 | 验收 | 依赖 |
|---|---|---|---|
| M4.1 | Critic agent 独立子进程 | 复用 `pr-review-toolkit/agents/code-reviewer.md`;不与 implement 共享 session | M2.5 |
| M4.2 | Anti-leniency 规则 | Critic 必须至少一维指出问题;不允许全维度高分 | M4.1 |
| M4.3 | WEAK 维度强制 BLOCK | 任一维度 ≤ X 分立即进 fix-cycle,绕过"勉强通过" | M4.2 |
| M4.4 | 评审维度 per-project 自适应 | 前端项目偏重 UX、CLI 偏重 Docs;由 plan-eng 阶段提示 | M4.1 |
| M4.5 | Critic 与 dev 切换的 cache 优化 | dev session 跨 phase cache 复用,Critic 单独热;算总成本回报 | M4.1 |

**M4 不做**:多个 Critic 并行 / 投票(M5)。

---

## 7. M5 — 长期对标 Symphony(3–6 月,开放探索)

**目标**:能搭大型软件——多模块、多服务、长跑数日。

不展开为短任务,仅列待解问题:

- 任务自动分解:把"做一个交易系统"拆成 N 个独立子项目,自动建 DAG
- DAG 调度(blocks / produces / consumes,而非平铺队列)
- Milestone 概念(周级别检查点),与子项目级 phase 区分开
- 跨子项目接口契约管理(谁定 schema、谁验证)
- 长 token 预算精细化(按子项目独立预算,跨子项目转账)
- A2A bridge——本地 ccteam 与云端 ccteam 协作
- 外部 tracker adapter(Linear / GitHub Projects),作为可选
- 多用户共享单 ccteam 实例(权限 / 隔离 / 公平调度)

**M5 进入条件**:M0–M4 全部经过至少 5 个真实小项目验证,确认现有协议(progress.jsonl / state.json / phase 模板)能扛住非平凡场景。

---

## 8. 跨里程碑依赖(关键路径)

```
M0.6 (orchestrator 主循环)
  ├─→ M0.7 (tmux 管理) ─→ M0.8 (idle-aware) ─→ M0.9 (状态机) ─→ M0.10 (reset) ─→ M0.15
  │                                                         └─→ M0.12 (fix-loop)
  └─→ M0.13 / M0.14 (stall + cost)

M0.15 ─→ M1.1 (telegram) ─→ M1.2 (多项目) ─→ M1.5 (重启恢复) ─┬─→ M1.6 (cross-cutting watcher) ─→ M1.7 (L3 fork 决策)
                                                              └─→ M2.1 (Seed) ─→ M2.5 (Critic 隔离) ─┬─→ M2.6 (sub-skill 调度)
                                                                                                    └─→ M2.7 (agent_team 启用)
                                                                                                         ├─→ M3.1 (retro)
                                                                                                         ├─→ M3.6 (audit 矩阵) ─→ M3.7 (投票)
                                                                                                         ├─→ M3.8 (新插件自动挂载)
                                                                                                         └─→ M3.9 (multi_session)

M3.1 ─→ M3.2 (向量索引) ─→ M3.3 (Seed 接 RAG)
```

不在关键路径上的(可与主线并行):M1.4(项目 CLAUDE.md)、M1.8(ccteam-control skill)、M1.9(多轮 CLARIFY)、M2.4(golden-rules)、M2.8(ccteam-mcp MCP server)、M4.4(评审自适应)、M3.5(claude-mem MCP)、M3.8(新插件挂载)。

**关键路径变化**(因 M3.9):M2.7 → M3.9 成为新关键路径终点之一(痛点 13 最终落地);M2.7 卡住整条 multi-agent 速度并行链。

---

## 9. 进度风险登记

| 风险 | 概率 | 影响 | 应对 |
|---|---|---|---|
| Claude Code 协议变更(hook 字段、CLI flag) | 中 | M0 整体阻塞 | 锁定测试过的 `claude --version`;CI 新版本 smoke test |
| ralph-loop Stop hook 范式与 ccteam 自有 Stop hook 互相冲突 | 中 | M0.12 卡住 | 不挂两个 hook,把 ralph 逻辑合到 parse-phase-end.sh(见 §2.3) |
| context reset 后行为不一致(新 session 把 CLAUDE.md 桥接信息当独立任务做) | 中 | M0.10 不可用 | M0.10 必须包含至少 3 个真实项目的 reset 验证用例 |
| Telegram bot 平台变更或封号 | 低 | M1 入口断 | 双通道 fallback:邮件 + 文件 inbox(已在 §3.8) |
| 向量索引方案选错(自建 vs claude-mem) | 中 | M3 阻塞 | M3.2 与 M3.5 并行 spike 1 周,择优 |
| 估算偏差累积 | 高 | 整体延期 | 每个里程碑结束做 retro,把实际工时回填本文 |

---

## 10. 计划维护纪律

1. **每个 PR 描述必须含**:
   - 对应任务编号(`Closes M0.X`)
   - 痛点编号(`requirements.md 痛点 N`)
   - tech-design 章节(`tech-design §X.Y`)
2. **本文档优先于 tech-design.md §7**——§7 已退化为指针,任何里程碑变化只改本文。
3. **任务粒度**:M0 详到子任务级(因为活跃);M1–M4 详到任务级;M5 仅高层方向。M0 完成后,把 M1 推进到子任务级。
4. **里程碑推进准则**:声明该里程碑解决的痛点必须能在 §1 反向映射表里被一个真实场景验证,**不**靠 checkbox 凑数。
5. **新工作流入**:不能映射到现有任务的需求 → backlog,不进主线(对应 tech-design §11)。
