# ccteam interfaces — 精确协议参考

> 本文是 [tech-design.md](./tech-design.md) 的"接口卡"。
>
> - `tech-design.md` 回答 **怎么做**(架构论证、设计权衡)
> - **本文回答 接口确切长什么样**(YAML schema、JSON shape、文件路径、命令签名)
>
> **实现 PR 修改任何对外协议 → 必须同步本文**。架构论证不属于本文,在 tech-design;具体字段定义不属于 tech-design,在本文。

---

## 1. 文件系统布局

### 1.1 全局目录(`~/.ccteam/`)

```
~/.ccteam/
├── config.yml             # 全局配置(并发上限、API key、bot token、信任档位、模型单价表)
├── inbox/                 # 待 triage 的需求
├── queue/
│   ├── seeding/
│   ├── planning/
│   ├── coding/
│   ├── reviewing/
│   ├── done/
│   └── archive/
├── phases/                # phase 模板(启动时复制到项目)
│   ├── 00-seed.md
│   ├── 01-plan-ceo.md
│   ├── 02-plan-eng.md
│   ├── 03-implement.md
│   ├── 04-test-author.md
│   ├── 05-test-run.md
│   ├── 06-fix.md
│   ├── 07-review.md
│   ├── 08-score.md
│   └── 09-ship.md
├── control/               # 用户 → orchestrator 控制信号(详见 §3.3)
├── memory/                # 跨项目记忆(M3+)
│   ├── patterns/
│   ├── anti-patterns/
│   └── index.json         # RAG 索引元数据
├── progress/
│   └── <slug>.jsonl       # 每项目一个事件流(hooks 写,inotify 监听;详见 §4)
├── log/
│   └── <slug>/            # stream-json 归档(可选,调试用)
├── tmux/
│   └── <slug>.layout      # 项目 tmux pane 布局模板
└── state/
    └── orchestrator.json  # orchestrator 自身 in-memory 状态的快照
```

### 1.2 项目级目录(`~/projects/<slug>/`)

```
~/projects/<slug>/
├── src/                          # 实际代码
├── tests/
├── package.json / pyproject.toml
├── CLAUDE.md                     # 项目级运营手册(自动生成,详见 tech-design §6.5)
├── .ccteam/                      # ccteam 元数据(git 跟踪)
│   ├── spec.md                   # 用户原始需求 + Seed 后澄清
│   ├── plan-ceo.md
│   ├── plan-eng.md
│   ├── architecture.md
│   ├── implement-report.md
│   ├── test-report.md
│   ├── review-report.md
│   ├── scorecard.md              # M2+
│   ├── code-review.md            # sub-skill 产物示例(详见 §7)
│   ├── state.json                # 项目级状态机(详见 §2.1)
│   ├── escalation.md             # 触发用户介入时写这里
│   ├── fix-loop.state.md         # fix-cycle 内部状态(ralph-loop 风格)
│   └── ready                     # SessionStart hook 写出的就绪标记
├── .claude/
│   └── settings.json             # 详见 §6.1
└── .gitignore
```

### 1.3 Multi-session 项目子模块布局(`parallelism: multi_session`)

仅当 `parallelism: multi_session` 时启用(M3+)。在 §1.2 基础上扩展:

```
~/projects/<slug>/
├── .ccteam/
│   ├── state.json                # master state(项目级,详见 §2.2)
│   ├── interface-contracts.md    # 子模块间接口契约(fan-out 时定下,fan-in 时验证)
│   └── sub-modules/
│       ├── backend-api/
│       │   ├── state.json        # sub-module state(与单 session 一致;详见 §2.3)
│       │   └── progress.jsonl    # 本子模块独立事件流
│       ├── frontend-dashboard/
│       ├── mobile-app/
│       └── docs/
├── backend-api/                  # 子模块代码(独立目录)
├── frontend-dashboard/
├── mobile-app/
└── docs/
```

---

## 2. State 协议

### 2.1 项目级 `state.json`(单 session 项目)

```json
{
  "slug": "bookmark-mgr-a3f9",
  "created_at": "2026-05-04T10:23:00Z",
  "tmux_session": "ccteam-bookmark-mgr-a3f9",
  "claude_session_id": "abc123-def-456",
  "claude_pid": 12345,
  "phase_state": "in_flight",
  "current_phase": "implement",
  "parallelism": "solo",
  "phase_history": [
    {"phase": "seed",     "status": "passed", "duration_s": 90, "cost_usd": 0.12},
    {"phase": "plan-ceo", "status": "passed", "duration_s": 45, "cost_usd": 0.08},
    {"phase": "plan-eng", "status": "passed", "duration_s": 60, "cost_usd": 0.15}
  ],
  "fix_cycle_count": 0,
  "cost_used_usd": 1.23,
  "soft_warn_threshold_usd": 20.0,
  "hard_kill_threshold_usd": 200.0,
  "context_tokens_used": 142000,
  "context_reset_threshold_tokens": 600000,
  "context_reset_count": 0,
  "last_progress_event_at": "2026-05-04T11:23:45Z",
  "last_event_type": "Stop",
  "last_user_interaction_at": "2026-05-04T10:23:00Z",
  "user_attached": false,
  "user_pause_pending": false
}
```

**`phase_state` 枚举**:`in_flight` / `idle` / `fix_locked`(详见 tech-design §3.2)。

**`parallelism` 枚举**:`solo` / `agent_team` / `multi_session`(详见 §5.1 phase schema)。

**原子写入**:`.tmp` + `rename`;启动校验 schema,损坏走 backup。

### 2.2 Master `state.json`(`parallelism: multi_session`)

在 §2.1 基础上扩展子模块状态摘要:

```json
{
  "slug": "saas-platform-x9f2",
  "parallelism": "multi_session",
  "current_phase": "fan-out",
  "phase_state": "in_flight",
  "sub_modules": {
    "backend-api":          {"current_phase": "implement", "phase_state": "in_flight"},
    "frontend-dashboard":   {"current_phase": "implement", "phase_state": "idle"},
    "mobile-app":           {"current_phase": "test-run",  "phase_state": "in_flight"},
    "docs":                 {"current_phase": "ship",      "phase_state": "idle"}
  },
  "max_sessions_per_project": 4,
  "...": "上述 §2.1 字段全部保留"
}
```

**项目级 phase 序列**:`plan` → `fan-out` → `implement-parallel` → `fan-in` → `review` → `ship`。

### 2.3 Sub-module `state.json`(multi-session 内)

字段与 §2.1 完全相同;只是粒度是子模块而非项目。

---

## 3. Inbox / Queue / Control 文件协议

### 3.1 Inbox

文件名:`<ISO-timestamp>-<random>.md`,原子写入(先写 `.tmp` 再 `mv`)。

```markdown
---
source: telegram          # telegram | cli | echo
user: rob
created_at: 2026-05-04T10:23:00Z
---

# 想法

做一个本地书签管理器,离线可用,按域名归类,支持搜索。
最好是 PWA,能装到手机。
```

### 3.2 Queue 状态分桶

按 §1.1 所示 `~/.ccteam/queue/<state>/<slug>.md`。状态值与 `state.json.current_phase` 大致对齐(详见 tech-design §3.2 状态机)。

### 3.3 Control(用户 → orchestrator)

```
~/.ccteam/control/
├── reject-<slug>           # 创建文件 = 命令"否决项目 <slug>"
├── pause-all               # 创建文件 = 暂停所有调度
├── pause-<slug>            # 暂停单项目
├── resume-<slug>           # 恢复单项目
├── answer-<slug>.md        # 内容 = 用户对 clarify 问题的回答
├── boost-<slug>            # 提升优先级
└── fork-reply-<slug>.md    # L3 fork 决策回复(M1+;详见 §9.3)
```

orchestrator 每轮(30s)扫描 `control/`,处理后**删除文件**(确保幂等)。

---

## 4. Progress.jsonl 事件流

每个项目一个 `~/.ccteam/progress/<slug>.jsonl`。**这是 orchestrator 唯一的状态事实来源**——tmux 终端输出只给人看,不解析。

### 4.1 事件类型(完整清单)

```jsonl
{"ts":"2026-05-04T11:23:00Z","event":"session_start","tmux_session":"ccteam-bookmark-mgr-a3f9"}
{"ts":"...","event":"phase_inject","phase":"implement"}
{"ts":"...","event":"PreToolUse","tool":"Edit","path":"src/db.ts"}
{"ts":"...","event":"PostToolUse","tool":"Bash","cmd":"pnpm test","exit_code":0,"duration_ms":4521}
{"ts":"...","event":"phase_milestone","phase":"implement","note":"完成 schema + migration"}
{"ts":"...","event":"phase_done","phase":"implement","duration_s":4521,"cost_usd":2.13}
{"ts":"...","event":"escalate","reason":"db migration 不可调和","cycle":3}
{"ts":"...","event":"user_attach","detected_by":"PreToolUse-input-source"}
{"ts":"...","event":"watcher_concern","watcher":"scope-watcher","note":"添加了云同步,超出 spec"}
{"ts":"...","event":"watcher_block","watcher":"cost-watcher","note":"项目累计 $200 触发硬上限"}
{"ts":"...","event":"SessionEnd","reason":"context_reset"}
```

### 4.2 写入责任

| 事件 | 写入方 |
|---|---|
| `session_start` / `phase_inject` | orchestrator(send-keys 前后直接 append) |
| `PreToolUse` / `PostToolUse` / `phase_milestone` | Claude Code hooks(详见 §6.1) |
| `phase_done` / `escalate` | Stop hook 解析 claude 最后一行(`PHASE_DONE: <phase>` / `ESCALATE: <reason>`) |
| `user_attach` | PreToolUse hook 检测输入源 |
| `watcher_concern` / `watcher_block` | Cross-cutting watcher 异步子进程(详见 tech-design §6.3 模式 B) |
| `SessionEnd` | SessionEnd hook |

### 4.3 消费方

- **orchestrator**:`inotify` 监听末尾,做状态转移与 stall 检测
- **用户 dashboard pane**:`tail -f progress/<slug>.jsonl | jq -c '.event + ":" + (.tool // .note // "")'`
- **retro phase**(M3):作为项目历史输入

### 4.4 Stream-json 归档(可选)

用 hook 把 `--output-format stream-json` 内容旁路归档到 `~/.ccteam/log/<slug>/`,仅供事后调试,**不参与状态判定**。

---

## 5. Phase 模板 schema

### 5.1 YAML front matter 完整字段

```yaml
---
name: implement                   # phase 名,必须与文件名 (03-implement.md) 一致
required_inputs:                  # 必读上游产物;orchestrator 验证存在性
  - .ccteam/plan-eng.md
  - .ccteam/architecture.md
required_outputs:                 # 必产出物;Stop 前 hook 验证;缺则不视为 phase_done
  - src/**/*
  - .ccteam/implement-report.md
soft_cost_warn_usd: 5.0           # 仅告警,不打断
stall_warn_minutes: 5             # 5 分钟无 hook event 第一次软告警
parallelism: solo                 # solo | agent_team | multi_session(详见 tech-design §3.3、§6.3、§6.11)
                                  # M0 仅支持 solo;M2 支持 agent_team;M3 支持 multi_session
                                  # subagent 不在此声明——任何 agent 都可 ad-hoc 通过 Task 工具启动
agent_team:                       # 仅当 parallelism: agent_team 时生效
  - role: backend-dev
  - role: frontend-dev
  - role: reviewer
sub_skills:                       # 替人编排的 sub-skill(详见 §7)
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done           # phase_start | phase_done(M0/M2 仅这两档)
    output_to: .ccteam/code-review.md
hooks:                            # phase 级 hook(项目级 hook 在 settings.json,详见 §6)
  before: scripts/snapshot-git.sh
  after: scripts/run-golden-rules.sh
---

# 任务

(prompt body...)
```

### 5.2 9 个 phase 列表

```
00-seed.md             # 可行性评估(PASS/REJECT/CLARIFY;输出格式详见 §5.3)
01-plan-ceo.md         # 产品规划
02-plan-eng.md         # 技术规划(multi_session 项目在此输出子模块清单 + interface-contracts.md)
03-implement.md        # 代码实现
04-test-author.md      # 测试编写
05-test-run.md         # 跑测试,输出报告
06-fix.md              # 修 bug(在 fix-cycle 中循环;详见 tech-design §3.5)
07-review.md           # 代码审查
08-score.md            # 评分(M2+;6 维 + bug penalty)
09-ship.md             # 提交、产文档、收尾
```

### 5.3 Phase 0 Seed verdict 输出格式

Seed phase 末尾必须输出固定 markdown,orchestrator 解析 YAML front matter 决定走向:

```markdown
---
verdict: PASS | REJECT | CLARIFY
confidence: 0.0-1.0
---

## 市场分析
(已有竞品 / 用户量 / 替代方案)

## 技术可行性
(核心难点 / 依赖 / 估算工作量)

## 商业可行性(按需)
...

## 决策
- PASS 时:建议技术栈、项目骨架
- REJECT 时:列举具体理由(已有 X、成本不可持续、用户量级 < N)
- CLARIFY 时:**只**提一个问题(prompt 显式约束;`24h` 无回答 → 自动归档)
```

---

## 6. Hooks 配置 schema

### 6.1 项目 `.claude/settings.json` 完整模板

```json
{
  "permissions": {
    "allow": ["*"],
    "deny": ["WebFetch(url:https://*.bank.com/*)"]
  },
  "env": {
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "1"
  },
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/load-context.sh", "timeout": 5},
          {"type": "command", "command": "scripts/progress-append.sh session_start", "async": true}
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/parse-phase-end.sh", "timeout": 10},
          {"type": "command", "command": "scripts/progress-append.sh Stop", "async": true}
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "idle_prompt|permission_prompt",
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh notification", "async": true}
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh PreToolUse", "async": true}
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh PostToolUse", "async": true},
          {"type": "command", "command": "scripts/cost-accumulate.sh", "async": true}
        ]
      },
      {
        "matcher": "Bash:git push.*",
        "hooks": [
          {"type": "command", "command": "scripts/block-push.sh"}
        ]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh SubagentStop", "async": true}
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {"type": "command", "command": "scripts/progress-append.sh SessionEnd", "async": true}
        ]
      }
    ]
  }
}
```

### 6.2 Hook 事件用途

| Hook | 作用 |
|---|---|
| `SessionStart` | 写 ready 标记;append `session_start` 事件 |
| `Stop` | 解析最后一行 `PHASE_DONE` / `ESCALATE`;append `Stop` 事件(idle 信号);若 fix-loop.state.md 存在则按 ralph-loop 范式拦截重喂 |
| `Notification:idle_prompt` | claude 显式等待用户输入 → idle 信号 |
| `Notification:permission_prompt` | 不应出现(`--dangerously-skip-permissions` 兜底);出现说明配置失效 |
| `PreToolUse` | append 工具调用事件;活跃信号(stall 检测反向判断) |
| `PostToolUse`(通用) | append 事件;`cost-accumulate.sh` 累加 token / cost 到 `state.json` |
| `PostToolUse(Bash matcher)` | 拦截危险命令(`git push` / `rm -rf /` / deploy 脚本) |
| `SubagentStop` | 子 agent 退出(仅 Agent Teams phase 内相关) |
| `SessionEnd` | claude 进程退出 → orchestrator 知道 reset 完成 vs crash |

### 6.3 `cost-accumulate.sh` 工作原理

Claude Code **不**在 hook 输入直接给 `cost_usd`,必须自算:

1. hook stdin 的 JSON 含 `transcript_path`(Claude Code 的 session JSONL 路径)
2. 脚本 tail 该 JSONL 最后一条 `role:assistant` 记录
3. 解析 `message.usage.{input_tokens, cache_read_input_tokens, cache_creation_input_tokens, output_tokens}`
4. 按 `~/.ccteam/config.yml` 的 `model_rates` 表算成本增量
5. 原子地累加到 `state.json.cost_used_usd` 与 `state.json.context_tokens_used`
6. 后者驱动 tech-design §6.9 的 60% reset 阈值

字段名参考 `claude-plugins-official/session-report/skills/session-report/analyze-sessions.mjs`。

`async: true` 必须设——同步阻塞会拖慢 PostToolUse 路径。

---

## 7. Sub-skill 调度 schema

### 7.1 phase front matter 的 `sub_skills` 字段

```yaml
sub_skills:
  - skill: "claude-plugins-official:pr-review-toolkit/agents/code-reviewer"
    trigger: phase_done
    output_to: .ccteam/code-review.md
  - skill: "claude-plugins-official:security-guidance/hooks/security_reminder_hook.py"
    trigger: phase_start
    output_to: .ccteam/security-precheck.md
```

### 7.2 Trigger 时机(M0/M2 仅两档)

| Trigger | 何时跑 | 实现 |
|---|---|---|
| `phase_start` | phase prompt 注入前 | orchestrator 把 skill 内容前置到 prompt(或异步先跑产出文件供 phase 引用) |
| `phase_done` | claude 输出 `PHASE_DONE` 后 | orchestrator 在状态机转移前调用 skill,产物落到 `output_to` |

**M0/M2 不引入 `before_done` 之类需 Stop hook 拦截的 trigger**(详见 tech-design §6.10)。

### 7.3 复用粒度三档与 `skill:` 路径前缀

| 路径前缀 | 粒度 | 含义 |
|---|---|---|
| `claude-plugins-official:<plugin>/<path>` | 直接 `@文件引用` | 零安装,phase 模板里 inline 引用 |
| `local:<path>` | 拷贝到项目 | 冻结版本,改不影响原仓库 |
| `installed:<plugin>/<command>` | 整 plugin 安装 | M2/M3 才考虑;`/plugin install <name>` 后调用 |

orchestrator 解析时按前缀分发实现路径。

### 7.4 产物自动接力

orchestrator 在调度下一 phase 时:
1. 扫上一 phase 全部 `sub_skills.output_to` 路径
2. 把这些路径作为下一 phase prompt 的 `@文件引用` 自动追加
3. 下一 phase claude 自动读到上游 audit / review 产物

### 7.5 `skill_intent.yaml`(M3+ 新插件挂载)

社区新插件提供:

```yaml
# ~/.claude/plugins/<plugin>/skill_intent.yaml
suggested_phases:
  - phase: ship
    trigger: phase_done
    rationale: "OWASP/STRIDE 安全审计应在 ship 前必跑"
  - phase: review
    trigger: phase_done
    rationale: "深度代码 review 与浅 review 互补"
default_output_to: .ccteam/{plugin}-output.md
```

ccteam Seed phase 后扫一次 `~/.claude/plugins/.../skill_intent.yaml`,按 `suggested_phases` 自动加进对应 phase 模板的 `sub_skills` 列表。

---

## 8. Multi-session per project 协议(M3)

### 8.1 Tmux session 命名

```
ccteam-<slug>                        # master session(编排)
ccteam-<slug>--backend-api           # 子模块 session
ccteam-<slug>--frontend-dashboard
ccteam-<slug>--mobile-app
ccteam-<slug>--docs
```

双连字符 `--` 分隔 slug 与 sub-module,避免与 slug 内部连字符歧义。

### 8.2 Fan-out / Fan-in 状态转移

```
master phase: plan-eng
  └─→ 输出 interface-contracts.md + sub-modules 清单 → master phase: fan-out
fan-out:
  └─→ orchestrator 起 N 个 sub-session(每 session 跑独立 9-phase 流)
  └─→ master phase: implement-parallel
implement-parallel:
  └─→ master inotify 监听所有 sub-module progress.jsonl
  └─→ 所有 sub-module 都到达 review phase 时 → master phase: fan-in
fan-in:
  └─→ master session 起来跑 review(读所有子产物 + 验证 contracts 满足)
  └─→ master phase: ship
```

### 8.3 资源约束

```yaml
# ~/.ccteam/config.yml
max_sessions_per_project: 4         # 默认;可项目级覆盖
max_total_sessions: 12              # 全局上限(跨所有项目)
max_subagents_per_phase: 5
hard_cost_kill_per_project_usd: 200 # multi-session 总和 = master + sum(sub-modules)
```

---

## 9. Defense in Depth 输出协议(L2 / L3)

详见 tech-design §3.6 三层防御协议。本节仅定义对外输出格式。

### 9.1 audit agent 三档 verdict

每个 audit agent(architect / critic / designer / security / scope-watcher)输出:

```markdown
---
verdict: PASS | CONCERN | BLOCK
confidence: 0.0-1.0
audit_role: architect
---

## Findings
- (具体发现)

## Suggestion
- (修改建议;BLOCK 时必须给出 actionable diff 描述)
```

orchestrator 综合所有 audit 的 verdict:
- 全 PASS → 自动通过
- 任意 BLOCK → 进入 fix-cycle(详见 tech-design §3.5)或转 L3
- 有 CONCERN 但无 BLOCK → 按信任档位决定(yolo/balanced 通过;careful 上推 L3)

### 9.2 Cross-cutting watcher 输出格式(`progress.jsonl` 事件)

```jsonl
{"ts":"...","event":"watcher_pass","watcher":"cost-watcher","cost_usd":3.21}
{"ts":"...","event":"watcher_concern","watcher":"scope-watcher","note":"添加了 plan-eng 未声明的云同步特性"}
{"ts":"...","event":"watcher_block","watcher":"cost-watcher","note":"项目累计 $200 触发硬上限","action":"kill_and_escalate"}
```

### 9.3 L3 telegram fork 决策消息格式

```
📋 项目 <slug>:<phase> agent 议事拍不了板
方案摘要: ...
各 agent 立场:
  - architect: PASS
  - scope-watcher: BLOCK("加了云同步,超 spec")
  - critic: CONCERN("接口设计可改进")
  - designer: PASS

[A] approve: 接受当前实现,继续
[B] tweak: <reply 一句话调整>(例:"去掉云同步")
[C] reject: 退回 plan-eng 重做

(24h 不响应自动 A approve;careful 模式不超时)
```

用户回复(走 telegram bot 或 control 文件):
- `A` → 写 `~/.ccteam/control/fork-reply-<slug>.md` 内容 `A`
- `B <内容>` → 文件内容为 `B\n<内容>`
- `C` → 文件内容为 `C`

orchestrator 检测后注入下一 phase prompt 或回退到上一 phase。

### 9.4 信任档位(`config.yml`)

```yaml
trust_mode: balanced              # yolo | balanced | careful
                                  # yolo:    L3 永不弹(仅 L1 BLOCK 时 escalate)
                                  # balanced(默认): L3 仅在 L2 投票分裂时弹
                                  # careful: 任何 CONCERN 都弹
fork_timeout_hours: 24            # L3 默认通过超时(careful 模式忽略)
```

---

## 10. CLI 命令签名

### 10.1 启动 / 停止

```bash
ccteam start                           # 启动 orchestrator(前台)
ccteam start --daemon                  # 启动并 daemonize
ccteam stop                            # 优雅停机(保留 tmux session)
ccteam stop --kill-sessions            # 同时关闭所有 tmux session(慎用)
```

### 10.2 提交需求

```bash
ccteam new "需求文本"
ccteam new -f spec.md                  # 从文件提
ccteam new --mode yolo "需求"          # 覆盖默认 trust_mode
```

### 10.3 查询状态

```bash
ccteam ls                              # 所有项目状态(human 表格)
ccteam ls --format json                # JSON 输出(给 LLM / 脚本用)
ccteam show <slug>                     # 单项目详情(含 session 状态、cost、最近 progress)
ccteam show <slug> --format json       # JSON 输出
ccteam progress <slug> --tail          # 实时 tail progress.jsonl
ccteam progress <slug> --phase implement  # 看特定 phase 事件
```

**`--format json` 是 M0 强制项**——所有查询命令必须支持,以让"用户自带 claude"路径(详见 [tech-design.md §3.8](./tech-design.md#38-用户接口层))通过 Bash 工具调时无需解析表格。

#### `ccteam ls --format json` schema

```json
{
  "projects": [
    {
      "slug": "bookmark-mgr-a3f9",
      "current_phase": "implement",
      "phase_state": "in_flight",
      "cost_used_usd": 1.23,
      "context_tokens_used": 412000,
      "tmux_session": "ccteam-bookmark-mgr-a3f9",
      "user_attached": false,
      "age_seconds": 13500,
      "last_event_ts": "2026-05-04T15:32:00Z",
      "stall_level": "ok"
    }
  ],
  "orchestrator": {
    "running": true,
    "active_count": 1,
    "max_concurrent": 3
  }
}
```

#### `ccteam show <slug> --format json` schema

是 §2.1 项目级 state.json 的全量 + 派生字段:

```json
{
  "state": { /* §2.1 state.json 全量 */ },
  "phase_history": [
    {"phase": "00-seed", "verdict": "PASS", "duration_s": 90, "cost_usd": 0.12},
    {"phase": "01-plan-ceo", "completed_at": "...", "cost_usd": 0.31}
  ],
  "recent_events": [ /* progress.jsonl 末尾 50 条 */ ],
  "artifacts": {
    "spec": ".ccteam/spec.md",
    "plan_eng": ".ccteam/plan-eng.md",
    "implement_report": ".ccteam/implement-report.md"
  },
  "stall": {"level": "ok", "silent_seconds": 23},
  "recommendations": [
    "若 cost > $50,考虑 attach 检查"
  ]
}
```

### 10.4 进入项目

```bash
ccteam attach <slug>                   # tmux attach 到项目 master session
ccteam attach <slug> --sub backend-api # multi-session 项目 attach 到子模块 session(M3+)
ccteam peek <slug>                     # tmux capture-pane 一次性看当前屏,不 attach
```

### 10.5 控制

```bash
ccteam reject <slug>                   # 否决
ccteam pause <slug>                    # 暂停(不杀 session)
ccteam resume <slug>                   # 恢复自动调度(接管 attach 后的暂停)
ccteam answer <slug> "回答内容"          # 响应 clarify
ccteam fork-reply <slug> A             # L3 fork 决策(M1+;A/B/C)
ccteam fork-reply <slug> B "去掉云同步"  # tweak
ccteam kick <slug>                     # 软重启项目 session(claude --resume)
```

### 10.6 维护

```bash
ccteam memory ls                       # 看跨项目记忆(M3+)
ccteam memory rebuild                  # 重建索引
ccteam config edit                     # 改全局配置
ccteam doctor                          # 体检:tmux server / claude 可用性 / 死 session 检测
```

---

## 11. `ccteam-control` skill(M1+)

让用户在自己的 Claude Code session 里调度 ccteam。架构论证见 [tech-design.md §3.8 / §6.7](./tech-design.md#38-用户接口层)。

### 11.1 安装位置

```
~/.claude/skills/ccteam-control/
└── SKILL.md
```

由 ccteam M1 release 通过 `ccteam doctor --install-skill` 写入,或手动 `cp` from binary unpack。装一次,所有 claude session 自动可见。

### 11.2 SKILL.md 字段约定

```yaml
---
name: ccteam-control
description: |
  Manage ccteam projects from any Claude Code session.
  Use when the user asks about ccteam status, wants to start a new ccteam project,
  needs to inspect / pause / resume an active ccteam project, or asks for advice on
  how to intervene when a project is stuck.
allowed-tools: [Bash]
---

(SKILL body)
```

`description` 字段必须明确"何时激活"——Claude Code 用 description 做 skill 选择决策。

### 11.3 SKILL body 必含章节

| 章节 | 内容 |
|---|---|
| **能力清单** | 所有可调 CLI 命令(从 §10 摘录,标注 `--format json` 是默认偏好) |
| **典型工作流** | 跨项目汇报 / 立项前多轮澄清 / 卡住诊断三类场景的 step-by-step |
| **决策原则** | 何时建议 `attach`(用户想介入)vs `peek`(只看不动)vs `pause`(暂停后再决定) |
| **不能做什么** | 不能替用户 attach(tty 交互);不能直接编辑 `~/projects/<slug>/.ccteam/` 元数据(走 control 文件协议) |

### 11.4 与 ccteam-mcp(M2+)的关系

M1 时 skill 让 claude 用 Bash 工具 + `--format json` 调 CLI。M2 ccteam-mcp 上线后:

- skill 仍保留——是 claude 发现"原来可以管 ccteam"的引导层
- skill body 改为推荐"优先用 mcp__ccteam__* tools,fallback 到 Bash"
- 老的 Bash 调用方式仍兼容(--format json 永不下线)

---

## 12. `ccteam-mcp` MCP server(M2+)

把 ccteam 状态查询暴露为 MCP structured tool。架构论证见 [tech-design.md §6.4](./tech-design.md#64-mcp-servers)。

### 12.1 注册方式

```json
// ~/.claude.json 或 ~/.claude/mcp_servers.json
{
  "mcpServers": {
    "ccteam": {
      "command": "ccteam",
      "args": ["mcp-serve"],
      "env": {}
    }
  }
}
```

由 `ccteam doctor --install-mcp` 写入(M2 release)。`ccteam mcp-serve` 是 binary 子命令,stdio 协议。

### 12.2 暴露的 tool 清单(M2 起步集)

| Tool 名 | 对应 CLI | 入参 | 返回 |
|---|---|---|---|
| `ccteam__ls` | `ccteam ls --format json` | `{}` | §10.3 ls JSON schema |
| `ccteam__show` | `ccteam show <slug> --format json` | `{slug: string}` | §10.3 show JSON schema |
| `ccteam__new` | `ccteam new "..."` | `{prompt: string, priority?: "low"\|"normal"\|"high", mode?: "yolo"\|"balanced"\|"careful"}` | `{slug: string, workspace: string}` |
| `ccteam__peek` | `ccteam peek <slug>` | `{slug: string, lines?: number}` | `{capture: string, ts: string}` |
| `ccteam__progress` | `ccteam progress <slug>` | `{slug: string, phase?: string, last_n?: number}` | `{events: [...]}` |
| `ccteam__pause` | `ccteam pause <slug>` | `{slug: string}` | `{ok: bool}` |
| `ccteam__resume` | `ccteam resume <slug>` | `{slug: string}` | `{ok: bool}` |

### 12.3 不暴露的(M2 显式排除)

- `ccteam attach` — tty 交互,MCP 协议不适合
- `ccteam start / stop` — orchestrator 生命周期管理是 ops 决策,不让 LLM 误调
- `ccteam memory rebuild` — 重操作,走 CLI

### 12.4 双消费者

| 消费者 | 用途 | 配置位置 |
|---|---|---|
| 用户自带 claude session(主) | 用户在任意目录开 claude → 通过 MCP 管 ccteam(详见 tech-design §3.8) | `~/.claude.json`(全局) |
| 项目级 claude(次) | phase 内自查"我在哪个 phase / 累计多少 cost" | `~/projects/<slug>/.mcp.json`(项目级) |

---

## 13. 关键文件路径速查

| 路径 | 用途 |
|---|---|
| `~/.ccteam/config.yml` | 全局配置(并发、阈值、信任档位、模型单价) |
| `~/.ccteam/inbox/` | 用户提需求 |
| `~/.ccteam/queue/<state>/` | 项目状态分桶 |
| `~/.ccteam/control/` | 用户 → orchestrator 控制信号(详见 §3.3) |
| `~/.ccteam/phases/` | phase 模板(详见 §5) |
| `~/.ccteam/memory/` | 跨项目记忆(M3+) |
| `~/.ccteam/progress/<slug>.jsonl` | 结构化事件流(详见 §4) |
| `~/.ccteam/log/<slug>/` | stream-json 归档(可选,调试用) |
| `~/.ccteam/tmux/<slug>.layout` | 项目 tmux pane 布局模板 |
| `~/.ccteam/state/orchestrator.json` | orchestrator 自身快照 |
| `~/projects/<slug>/.ccteam/` | 项目元数据(详见 §1.2) |
| `~/projects/<slug>/CLAUDE.md` | 自动生成的项目运营手册 |
| `~/projects/<slug>/.claude/settings.json` | 项目级 Claude Code 配置(详见 §6.1) |
| `~/projects/<slug>/.ccteam/sub-modules/<name>/` | multi-session 子模块元数据(M3+;详见 §1.3) |
