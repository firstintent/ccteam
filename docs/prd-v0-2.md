# PRD V0.2 — 项目 session 自循环 + meta-agent watchdog

> 版本需求文档。范围只覆盖 V0.2 增量,不复述 V0.1 已 ship 内容。
> 实施纳入里程碑时再开 development-plan 子项。

---

## 1. 背景

V0.1 实测(`docs/user-quickstart-v0.1.md` 跑下来)暴露的核心痛点:**项目 session
"卡住"时编排层无法感知 / 无法兜底**。具体场景:

- 项目 session 输出一段问题,等用户决策 → 用户没在线 → 永远挂起
- AskUserQuestion 工具同步阻塞 → detach 即停
- 普通文本提问完全绕开异步协议 → meta-agent 也无感知
- send_to_session inbox 文件未及时派发 → meta-agent 以为踢了一脚实际没生效
- orchestrator 守护进程停止后无告警 → 项目无声停摆 5min+

合并起来一句话:**项目 session 一旦不主动走结构化协议出口,整个 ccteam control
plane 就瞎了**。用户与 meta-agent 都被迫 polling tmux 屏幕。

## 2. 解决方向 — 项目 session 自循环

V0.2 不靠 meta-agent 代答(meta-agent 没有项目业务上下文,不该做技术决策),
而是让**项目 session 自己持续 loop**,直到主动走三种合法出口之一收尾。

### 2.1 三个合法收尾出口(协议唯一化)

phase 末态只允许:
1. `phase_done.json` — phase 正常完成
2. `escalate.json` — phase 异常需用户介入
3. `*.outbox.json` — 主动询问用户(走现有 outbox 通道,channel-friendly)

任何其他形式的"停下"(纯文本提问、AskUserQuestion 阻塞、单纯 idle)都被
定义为"未收尾",由 auto_loop 自动推进继续。

### 2.2 auto_loop default-on

现有机制(M0.12,`crates/ccteam-core/src/auto_loop.rs`):phase template
显式 `auto_loop: true` 才启用,用于 fix-loop 场景。

V0.2 翻面:**auto_loop 成为 phase 默认行为**。Stop hook 触发 = LLM idle,
若本轮 phase 未产出三种合法出口任一 → orchestrator 自动 send-keys "继续"
让 LLM 自己看 phase prompt + `.ccteam/` 状态决定下一步。

**Stop hook 实施细节**(基于 Claude Code hooks lifecycle 研究,详见
`docs/v0-2-claude-code-alignment-review.md` §3.1):

- Stop hook **exit 2 + stderr** 路径:hook 检查 `.ccteam/` 这一轮 phase 没
  产出三种文件之一 → exit 2,stderr 写"phase 未正常收尾,请输出 PHASE_DONE
  / ESCALATE / 写 outbox 之一",Claude Code 自动把 stderr 注入对话强制模型
  继续(`hooks.ts:2784-2805`)。**不需要 ccteam orchestrator 主动 send-keys**,
  Claude Code 自己接管 loop
- **防递归**:Stop hook 二次进入时 payload 含 `stop_hook_active: true`
  (`query.ts:1567`)。hook 自身检测此字段,第二次跳过 block,append
  `needs_attention.outbox` 让 watchdog(§3)接力 surface 给用户
- **拿 transcript 不读盘**:payload `last_assistant_message` 字段直接是最后
  一条 assistant 文本,hook 判定逻辑零额外 IO

### 2.3 防死循环

- **L1 prompt 约束**:phase template + team `golden_rules` 写明"询问用户唯一合法出口是写 outbox。任何纯文本问句不会被人看见,只会触发 auto_loop"。LLM 自律走 outbox。
- **L2 cycle cap**:现有 `auto_loop_cycle_count` 3 次顶硬 escalate(`docs/tech-design.md` §3.5)。auto_loop 通用化后这条 cap 自动复用,LLM 反复输出同一问题陷死循环也会被强制 escalate。
- **L3 防递归 fail-safe**:`stop_hook_active: true` 检测到第二次进入即不再 block,直接 append `needs_attention.outbox`(详见 §2.2 实施细节)

### 2.4 AskUserQuestion 处置 — 升级到 V0.2 必做

这工具是 LLM 内部同步阻塞,**Stop hook 不会触发**(进程仍在 wait input,不算 idle)。auto_loop 救不了它。**两道刹车都做**:

- **L1 prompt 约束**:`golden_rules` 写明"禁用 AskUserQuestion,改写 outbox"
- **L2 PreToolUse hook 拦截**:配 `matcher: "AskUserQuestion"`,返回
  `permissionDecision: "deny"` + reason "本 phase 应自决,改为写 outbox"。
  机制确定可行(详见 `v0-2-claude-code-alignment-review.md` §3.2),不必等
  漂移数据 — V0.2 直接做

之前 PRD 草案把 hook 拦截标为 "V0.3 视漂移决定"。Fork 4 研究确认机制零成本
+ 行为 deterministic,**升级到 V0.2 必做** — prompt 层是软约束(LLM 偶尔
漂移),hook 层是硬保险。

## 3. 双保险 — meta-agent 升级为 watchdog(translation,不 decide)

### 3.1 红线先确认

`tech-design.md §6.8` 红线:**控制平面不放 LLM**(symphony 反模式禁令)。
具体禁止:
- orchestrator 自己调 LLM 决定 "auto_loop 要不要再来一次"
- per-project supervisor claude session(每项目一倍成本 / 责任分裂)
- observer-only LLM 仅监控不行动(纯烧钱无 actionable)

V0.2 不破上述任何一条。

### 3.2 watchdog 真正承担的角色:**translation,不是 decision**

智能加在"把状态翻译给用户"那一面,不在做技术决策那一面。载体是 meta-agent
(本身是 LLM session,基础设施免费,不在控制路径)。

升级后 meta-agent 周期读所有项目 `progress.jsonl` 摘要,做的是 UX 判断:
- "项目 A auto_loop cycle ≥ 2 — 不用我决定,但要不要现在告诉用户?"
- "项目 B 当前 phase 跑了 90 分钟 + cost $30,符合往常这 phase 常态吗?"
- "5 个项目 idle,哪个最值得用户先看?"

watchdog **不改任何 orchestrator 行为** — auto_loop / cycle cap / escalate
该怎么干还怎么干。watchdog 只决定"什么时候、用什么措辞、按什么优先级"通知用户。

### 3.3 与 V0.2 自循环的关系(双保险)

| 阶段 | 保险层 |
|---|---|
| LLM 走主路径(写三种出口之一)| 不需要 watchdog |
| LLM 漂移不写 outbox 进入 auto_loop | cycle cap 硬截(deterministic) |
| LLM 缓慢但合理推进 cycle ≥ 2 | watchdog 主动告知用户"在反复 loop,要不要看一眼"(可选打扰) |
| LLM 完全死掉(进程级) | 现有 stall 5/15/30 三级软告警 + (V0.3?) liveness probe |

watchdog 不是兜底决策层,是**预警层**。

### 3.4 信号源:外部 timer + Stop hook,不用 SessionEnd

基于 Claude Code hooks lifecycle 研究(详见
`docs/v0-2-claude-code-alignment-review.md` §3.3):**`SessionEnd` 不适合
做 watchdog 触发源**。`exit_reason` 枚举只有 6 个全是用户主动事件
(`clear` / `resume` / `logout` / ...),stall 不会触发它。

watchdog 数据源:

- **ccteam Rust orchestrator 外部 timer**(已有 `stall.rs`)— 探"压根没 Stop
  也没新进 progress event"的死亡 session
- **Stop hook 兜底**(§2.2)— 探"phase 完结但没写完成事件",写
  `needs_attention.outbox`,watchdog 读取

watchdog 自己不做 hook;它只读这些 deterministic 信号 + meta-agent LLM 翻译。

### 3.5 通知打扰阈值

watchdog 主要风险是误报警 → 用户嫌烦。需用户可配置:
- 默认保守(仅 cycle ≥ cap-1 / phase 时长超 P95 / cost 超 phase 配额)
- 用户可调"安静"或"敏感"

## 4. 用户自定义工作流(team 工厂)

### 4.1 场景

用户有一个**持续迭代的老项目**,想用 ccteam 管理标准研发循环:

```
新需求提出 → 需求合理性评估 → 架构设计 → 开发 → 测试 → 发布
                                                        ↓
                                              (回到"新需求提出")
```

现有 dev team 是 greenfield 工作流(plan-eng → implement → ship),不匹配
"老项目持续迭代"语义。用户希望能**自己定义 phase 序列**,而不是 fork 一份
phase markdown 自己改。

### 4.2 抽象映射

在现有抽象里这就是**用户自定义 team**:
- team.yaml 已数据驱动(`phase_dir` 字段,M3.2)
- phase markdown 目录已可任意指定
- 缺的是**让普通用户能产出 team.yaml + phase markdown 套件**(目前要手写
  yaml + 6+ 个 markdown + 给每个 phase 配 `tools_required` / `sub_skills` /
  `golden_rules`,门槛太高)

V0.2 不引入新概念,只是把 team 创建做成**对话式工厂**。

### 4.3 工厂产物形态 — Claude Code plugin 格式

> 重大调整(2026-05-07):基于 Fork 3 plugin/marketplace 机制研究
> (详见 `docs/v0-2-claude-code-alignment-review.md` §2),工厂产物**不另起
> ccteam 私有协议,直接走 Claude Code plugin 格式**。`plugin.json` schema
> 已 partial-merge 了 commands / agents / skills / hooks / mcpServers /
> userConfig / dependencies 全部扩展点(§1.4 哲学),覆盖 ccteam team 95%
> 需要的字段。

team-plugin 目录形态:

```
~/.claude/plugins/marketplaces/ccteam/plugins/<team-name>/
├── .claude-plugin/plugin.json      # Claude Code 标准 manifest
├── team.yaml                        # ccteam 私有字段(plugin 顶级 unknown,zod strip)
├── phases/                          # ccteam 私有,plugin pipeline 不读
│   ├── 00-kickoff.md
│   └── ...
├── agents/                          # 标准 plugin 目录,Claude Code 自动 namespace
│   └── code-reviewer.md
├── commands/                        # 同上
├── hooks/hooks.json                 # 同上
└── .mcp.json                        # 同上
```

**share team = `gh repo create` + 引用为 marketplace `github` source**
(`source: 'github', repo: 'user/my-ccteam-team'`),零 ccteam 自营注册中心。
本地开发时 `~/.config/ccteam/teams/<name>/` 退化为 staging 目录(尚未发布
的 team),`ccteam team publish` 命令负责把它转成 plugin repo(或推到
marketplace `directory` source)。

### 4.4 工厂产出步骤

入口走 meta-agent。用户自然语言描述工作流,meta-agent 通过 skill / sub-skill
跟用户对话:

1. 收集 phase 列表 + 每 phase 用途
2. 推断每 phase 的 `tools_required` / `sub_skills`(可选)
3. 推断 team 级 `golden_rules` / `retro_schema` / `verdict_schema`
4. 收集 team-plugin 元数据(name / description / author / version)
5. 生成 plugin manifest + team.yaml + phases/ + 必要的 agents/ commands/
6. 工厂产物落 staging:`~/.config/ccteam/teams/<name>/`(或 `--target plugin`
   直接落 `~/.claude/plugins/marketplaces/local/plugins/<name>/`)
7. 调用 `ccteam doctor --validate-team <name>` 校验
8. 给用户一个 try-run 命令(eg `ccteam new --team <name> "<scenario>"`)

工厂**不实施任何项目业务**,只产出 plugin 兼容文件 + 触发 doctor 校验。

### 4.5 复用 Claude Code plugin 标准能力

| ccteam 需求 | 复用的 plugin 字段 | 出处(详见 review §2)|
|---|---|---|
| 用户填表选项(team prompt 变量、cost 上限、phase 顺序)| `userConfig`(type/title/sensitive/min/max),`${user_config.KEY}` 注入 | §2.4 |
| Team 间复用(eg `product-research` 引用 `core-rules`)| `dependencies: ["core-rules@ccteam"]`,自动 cycle 检测 | §2.6 |
| Sub-skill / plugin agent 装载 | plugin in-memory pipeline,**不需 ln -sf**;给 spawned session 写 `enabledPlugins` | §2.2 |
| Hooks(stall watcher 等)| `hooks/hooks.json` | §2.1 |
| MCP servers(eg ccteam-mcp 自身)| `.mcp.json` | §2.1 |
| 安装 / 卸载 / 更新 | plugin install / uninstall 标准命令 | §2.5 |

### 4.6 待讨论的关键架构问题

V0.2 实施前必须先讨论敲定:

1. **DAG 循环表达** — 老项目场景"测试 → 发布 → 新需求"是循环。两条候选:
   - (a) 仍用单次 ccteam 项目跑一圈,循环靠用户每次新需求 `ccteam new`
   - (b) phase YAML `next_on_done` 字段允许指回上游,DAG 变 graph
   倾向 (a) — 简单、已有机制承载
2. **三层优先级实施**(详见 §5.2):整团 first-source-wins(project > user > repo)
3. **phase 模板 vs prompt 边界** — 已在 §5.3 D 方案敲定:协议在 frontmatter +
   orchestrator inject prompt template,正文 100% 领域。工厂生成时只填 frontmatter
   + 领域正文,从不在正文产协议关键词
4. **工厂自身载体** — meta-agent 内嵌 skill(`ccteam-team-author` skill 跟
   `ccteam-control` 并列),倾向这个
5. **工厂输出校验** — `ccteam doctor --validate-team <name>` 复用 §5.3 已规划
   的命令。工厂结束立即调
6. **工厂的 escape hatch** — 用户改 phase markdown 改不到协议(§5.3 D 已保证),
   只改正文领域;`team.yaml` 改坏 doctor fail-loud。简化版的"用户改完 commit
   fork"(走 plugin repo 自然 fork 即可),不需 marker 双段

### 4.7 与 V0.2 §2 / §3 / §5 的关系

工厂跟自循环 / watchdog 正交:
- 自循环 / watchdog 是 **runtime 行为**,作用在 phase 执行时
- 工厂是 **authoring 工具**,作用在 phase 定义时

工厂产出的新 team 自动继承:
- §2 自循环行为(runtime 默认)
- §3 watchdog 监督(runtime 默认)
- §5.3 D phase prompt 架构(用户自然不会写出协议关键词,因为工厂模板里就没有)
- §5.1/§5.2 团队布局(plugin 内的 phases/ 跟仓内 `teams/<name>/phases/` 同结构)

## 5. 团队布局重构(§4 工厂的前置基础)

### 5.1 现状

仓根铺平 + 命名约定硬编码:

```
ccteam/
├── teams/
│   ├── dev.yaml
│   └── product-research.yaml
├── phases/                  ← dev team
├── phases-product-research/ ← product-research team
└── phases-research/         ← research backlog
```

问题:

1. **`phases-<team>` 命名约定写死在多处**(grep `ccteam-core` / `ccteam-cli` /
   doctor install path / e2e tests),即使 `team.yaml.phase_dir` 已有字段也只是
   半数据驱动 — 改了 yaml,文件系统约定仍隐含
2. **不对称**:`teams/*.yaml` 在子目录,phase markdown 在仓根 — 一个 team 的
   配置散在两个地方
3. **跟 user-level team 布局不兼容**:V0.2 §4 工厂要落到 `~/.config/ccteam/teams/<name>/`
   是单目录形态,跟仓内"配置和 phases 分离"对不上
4. **领域命名缺位**:`phases-product-research` 是技术名,不是领域名;用户视角
   应该是"我要一个产品调研 team"而不是"phases-product-research/"

### 5.2 目标布局

每个 team 单一目录,team.yaml 在目录内,phase markdown 在子目录:

```
ccteam/
└── teams/
    ├── software-development/      ← 领域命名(`dev` 重命名)
    │   ├── team.yaml
    │   └── phases/
    │       ├── 00-kickoff.md
    │       └── ...
    ├── product-research/
    │   ├── team.yaml
    │   └── phases/
    └── research-academic/         ← phases-research/ backlog 重命名
        ├── team.yaml
        └── phases/
```

User-level team 实际落在 Claude Code plugin pipeline(§4.3 决议),staging
路径同结构:

```
# 已发布的 user-level team(plugin 模式)
~/.claude/plugins/marketplaces/<mkt>/plugins/<team-name>/
├── .claude-plugin/plugin.json
├── team.yaml
├── phases/
└── (其他 plugin 标准目录)

# 本地开发态 staging(尚未 publish)
~/.config/ccteam/teams/<name>/
├── team.yaml
└── phases/
```

#### 5.2.1 三层加载优先级(借鉴 layered settings 设计模式)

orchestrator 加载 team 时按 project > user > repo 优先级搜:**整团维度,
first-source-wins**(撞名 project 完全覆盖 user / repo,不字段级合并)。

设计依据见 `docs/v0-2-claude-code-alignment-review.md` §4。借鉴的是 Claude
Code `SETTING_SOURCES` enum 模式,**不照搬**字段维度合并(ccteam team 是
整体替换,合并语义会让"哪个 phase 在哪一层"不可溯源)。

```rust
// ccteam-core/src/team_resolver.rs (新)
const TEAM_SOURCES: &[TeamSource] = &[
    TeamSource::Project,  // ~/projects/<slug>/.ccteam/team/
    TeamSource::User,     // ~/.claude/plugins/.../<team-plugin>/  + staging
    TeamSource::Repo,     // teams/<name>/ (仓内 ship)
];

fn resolve_team(name: &str) -> Result<TeamSpec> {
    for source in TEAM_SOURCES {
        match source.try_load(name) {
            Ok(Some(spec)) => return Ok(spec),
            Ok(None) => continue,  // 该层缺,fall-through(读容错)
            Err(e) if e.is_yaml_error() => {
                tracing::warn!("team {name} at {source:?} unreadable: {e}");
                continue;  // 读容错
            }
            Err(e) => return Err(e),  // IO 严重错 fail-loud
        }
    }
    Err(anyhow!("team {name} not found"))
}
```

写路径(`ccteam team save` / 工厂 publish)严格:目标层 yaml 坏直接 reject,
不覆盖。per-source cache + 显式 invalidate(配合 `ccteam doctor` 命令)。

orchestrator 加载完全数据驱动 — `ccteam-core` 不再 hardcode 任何 team 名 /
目录约定字面量(继续遵守 strategic doc §3 红线 + §6 反模式重构候选 2/3)。

### 5.3 phase markdown 用户可改 — 领域层 / 编排层分离

让用户改 phase 是 §4 工厂的隐含必然。但 phase markdown 当前**协议和领域
混在一起**(正文里同时有 `PHASE_DONE` 关键词、`required_outputs` 路径声明、
ESCALATE grammar 例子,跟业务约束 / 风格偏好 / 角色 framing 混杂),
用户随手改可能破协议。

> **完整设计在子文档:`docs/phase-prompt-architecture.md`**(三层架构、
> frontmatter 字段全集、orchestrator inject prompt 模板、改造前后示例、
> 实施步骤、测试策略)。本节只列摘要 + 决议。

#### 5.3.1 决议 — 方案 D(协议外移到 orchestrator inject prompt)

观察 Claude Code 自身设计哲学(skills / agents / output-styles 全部统一
"frontmatter 协议 + 正文自由 prompt",**正文从不承载协议关键词**),候选
保护方案 A-E 中只有 D 跟 Claude Code 哲学完全对齐:

- **A 双文件**:跟 skill / agent 单文件惯例冲突
- **B Marker 双段**:引入 ccteam 自创 marker convention,Claude Code 自己从不用
- **C Doctor + Lint**:校验正文 contract phrase 存在性,违反"正文 = 自由 prompt"
- **D Inject prompt 外移**:phase markdown 正文 100% 领域,frontmatter 全部协议,
  orchestrator 据 frontmatter 字段差异化拼装 inject prompt;**用户改正文永远破不了协议**
- **E Fork**:跟 §4 工厂不兼容

D 不是颠覆性改造 — orchestrator 现有 `progress::build_phase_prompt_with_attachments`
**已经做事实上的协议注入**(short prompt 里写了 `PHASE_DONE` / `ESCALATE` /
report 路径)。V0.2 §5.3 是把这条已有路径做得 declarative-richer + phase markdown
正文里**重复**的协议关键词清出去。

#### 5.3.2 三层架构摘要

```
Layer 1 — frontmatter (协议层,declarative)
    ↓ orchestrator 加载时读
Layer 3 — orchestrator inject prompt (运行时拼装,short prompt)
    ↓ short prompt 里 `@` 引用
Layer 2 — phase markdown 正文 (领域层,用户全权改)
```

LLM 看到的最终 prompt = inject short prompt + `@` 引用拉取的 phase markdown 正文。
frontmatter 不直接进 LLM 视野,只通过 inject prompt 转译进。

#### 5.3.3 V0.2 落地步骤(详见子文档 §11)

1. frontmatter 字段补齐:加 `completion_signal` / `escalate_grammar_ref` /
   `outbox_question_protocol` / `inject_directives`
2. inject prompt 模板化:`build_phase_prompt_with_attachments` 升级接受
   `&PhaseTemplate`,据字段拼装
3. team.yaml.golden_rules 拆 `protocol` / `domain` 两段
4. 12 个 shipped phase markdown 正文清理:删协议级片段
5. doctor 校验增量:frontmatter schema / IO 契约 / 正文不含协议关键词(warn)
6. 加 `ccteam phase show <team> <phase>` 命令渲染最终 inject prompt

#### 5.3.4 不在 V0.2 范围

- 方案 A 双文件、方案 B marker、方案 C 正文 lint — 哲学不对齐,不留作 backup
- inject prompt 模板完全外部化(用户可改协议层) — 破红线
- LLM-aware phase markdown 校验 — 破"控制平面无 LLM"红线
- 自动 phase dry-run 测试 — V0.3

### 5.4 重命名 vs 不重命名

`dev` → `software-development` 是 breaking change(state.json `team` 字段
+ user-level rules `ccteam-lessons-dev.md` + 测试期望 + slug 前缀 F22
全部受影响)。两条路:

- **(a) 只重构布局,不重命名**:`teams/dev/team.yaml` + `teams/dev/phases/`,
  team 名仍是 `dev`。零 breaking change,但"领域命名"目标只完成一半
- **(b) 同步重命名**:加 `team.yaml.aliases: [dev]` 字段,旧名仍可识别;
  state.json 用 alias 加载,save 时写新名。一次性 migration

倾向 (a) 先 ship,(b) 留给 V0.3。理由:重命名牵扯太广(F22 slug 前缀 +
auto-memory 文件名 + e2e 测试假设),布局重构本身已经够大;领域命名先
应用在**新建** team(§4 工厂产出),老 team 名维持不动。

### 5.5 实施 surface 估算

- `ccteam-core/src/teams.rs`(或同等 module):team 加载逻辑改成扫
  `<root>/teams/<name>/team.yaml`,phase_dir 默认 `<team_dir>/phases/`,
  不再去仓根找 `phases-<name>/`
- `team.yaml`:`phase_dir` 字段语义从"仓根相对路径"变"team 目录相对路径",
  默认 `phases`;旧 yaml(`phase_dir: phases-product-research`)serde alias 兼容
- doctor install path 校验 + e2e tests 改路径
- migration:repo 内 `phases-product-research/` → `teams/product-research/phases/`
  用 git mv 一次完成(保 history),`phases-research/` 同理;`phases/` →
  `teams/dev/phases/`
- 更新 `interfaces.md §5.5 team.yaml schema` + `tech-design.md §3` 把
  布局红线写明
- `ccteam doctor --install-recommended-agents` 重新 verify 仍 pass

### 5.6 与 §4 工厂的关系

§5 是 §4 的**前置**:

- §5 ship 后,工厂产出物的目录结构跟仓内 shipped team 一致 — 这是 §4 验收
  "工厂生成的 team 能 doctor pass"的隐含前提
- §5 §4 可同 milestone 落地;§5 先做(纯技术重构,无 LLM),§4 后做(对话流)

## 6. ccteam-core 反模式重构(V0.2 必做)

> 基于 Fork 2 audit + Claude Code 哲学对照,详见
> `docs/v0-2-claude-code-alignment-review.md` §5(完整 8 个候选清单)。
> 本节列 V0.2 必做 6 条(高优先级);V0.3 deferred 2 条见 §7。

V0.2 §2 / §3 / §4 / §5 是新功能 + 协议升级;§6 是**清理 ccteam-core 既有
反模式**,让 V0.2 之后的 codebase 真正贴合 Claude Code 哲学。这些反模式同时
也是 §4 工厂、§5.2 三层优先级落地的前置障碍。

### 6.1 候选 1+8 — 协议关键字三处镜像 → 单一 source of truth

**位置**:
- `crates/ccteam-core/src/progress.rs:158-160`(Rust 拼 `PHASE_DONE: {phase}` 字面量)
- `crates/ccteam-hooks/src/parse_phase_end.rs:114-149`(`strip_prefix("PHASE_DONE:")`)
- `crates/ccteam-core/src/templates/meta_agent_role.md` + 12 个 phase markdown 正文

**现状**:`PHASE_DONE` / `ESCALATE` 字面量在 Rust 字符串、phase markdown
正文、注入 prompt 三处镜像;phase YAML frontmatter 反而**没**声明它们。

**重构**:跟 §5.3 D 方案合并实施 —
- frontmatter 加 `completion_signal` / `escalate_grammar_ref` 字段(已规划)
- `build_phase_prompt` / `parse_phase_end` 都从 `&PhaseTemplate` 读字段拼
- phase markdown 正文清理(已规划)

**违反哲学**:§1.3(frontmatter+body)+ §1.6(composable primitives)。

### 6.2 候选 2 — `render_project_claude_md` `match team` 写死团队语义

**位置**:`crates/ccteam-core/src/projects.rs:186-198`

**现状**:`match team { "dev" => ..., "product-research" => ..., _ => generic }`
每条分支硬塞一段中文 CLAUDE.md(含 dev "测试不过不算完成"、research
"不写代码")。**直接违反 ccteam-core 红线"core 不出现 team 名字面量"**。

**重构**:`team.yaml` 加 `claude_md_body: |` 字段,或 `teams/<name>/CLAUDE.md.tmpl`
单文件;`bootstrap_project` 只做 `{slug} {team}` 模板插值。

**违反哲学**:§1.2 file-system 发现 + 红线。

### 6.3 候选 3 — `TEAM_BUNDLES` 编译时常量(team 注册中心反模式)

**位置**:
- `crates/ccteam-core/src/templates.rs:30-107`(`include_str!` 12 个 phase + 2 个 team.yaml,串成 `const TEAM_BUNDLES`)
- `crates/ccteam-core/src/memory_bridge.rs:39-42`(`const TEAMS` 同样写死)

**现状**:加一支 team = 改 ccteam-core Rust 源 + 重 build。`load_team_runtimes`
已经会扫 `~/.ccteam/teams/<name>/team.yaml`,但安装路径仍走 `TEAM_BUNDLES`
注册。

**重构**:把 dev / product-research 的 team.yaml + phases 当 **seed examples**
拷到 `~/.ccteam/teams/<n>/`;runtime 只信任磁盘扫描;`memory_bridge::TEAMS`
改成"扫磁盘看哪个 team 声明 `retro_schema` 就给谁建 bridge"。

**违反哲学**:§1.2 file-system + §1.4 plugin pipeline(plugin 模型也是磁盘扫描)。

### 6.4 候选 5 — meta-agent `if team == META_TEAM_NAME` 5 处分叉

**位置**:`crates/ccteam-core/src/orchestrator.rs:553, 612, 1239, 1303, 1378`
+ `templates.rs:247` + `meta_agent.rs:11`(注释自承是 hardcoded branch)

**现状**:meta-agent 是事件循环 / 永不 terminal / 不走 phase DAG / 不计 cost /
不算 stall;实现方式是 5+ 处独立 `if state.team == META_TEAM_NAME` 分叉。

**重构**:在 `TeamSpec` 加 `evergreen: bool` / `phase_dag: Option<...>` /
`cost_policy: CostPolicy` 字段;orchestrator 5 处 if 改成读 spec flag;
meta-agent 变成自带 team.yaml 的特殊 team。**任何用户自建 evergreen team
(eg V0.3 看门狗 reviewer-agent)都走同路径**。

**违反哲学**:§1.6 composable primitives(把"模式"切成 ifs 而非 declarative flag)。

**额外受益**:为 V0.3 加新 evergreen agent(reviewer / supervisor / channel-adapter)
开门 — 不必再每加一个就加 5 处 if。

### 6.5 候选 7 — `RECOMMENDED_AGENTS` 8 plugin agent 写死 ln -sf

**位置**:`crates/ccteam-core/src/tool_surface.rs:65-106`(8 个 `(filename,
plugin, relpath)` 三元组硬编码 + `link_recommended_agents_for_phases_into`
ln -sf 进 `~/.claude/agents/`)

**现状**:bootstrap 时强制 ln -sf 8 个 plugin agent。**这是 ccteam 自挖坑** —
Claude Code 有 in-memory plugin pipeline,namespace 自动加 `pluginName:` 前缀,
不需要 symlink。ccteam 之所以 ln -sf 是因 spawned project session 没启用
plugin pipeline。

**重构**(详见 review §2.2):
- 删 `RECOMMENDED_AGENTS` const 和 ln -sf 路径
- 给 spawned project session 的 `.claude/settings.json` 写
  `enabledPlugins: {"<plugin>@<mkt>": true}` 启用 plugin pipeline
- phase YAML `tools_required.subagents` 仍声明依赖,但 doctor 校验从"是否
  ln -sf 在 `~/.claude/agents/`"改为"是否在 enabledPlugins 启用列表"

**违反哲学**:§1.4 plugin pipeline + §1.2 convention。

**额外受益**:跟候选 6(`pre_trust_project` 写 `~/.claude.json`,V0.3)同向 —
都是减少 ccteam 对全局 user state 的侵入式写入,改用 project-level settings。

### 6.6 实施顺序

按依赖关系排:

1. **候选 5(meta-agent flag 化)**:`TeamSpec` 加 evergreen 字段 — 是后面候选
   3 落地的前置(去掉 if 分叉之后,team 加载完全 declarative)
2. **候选 3(TEAM_BUNDLES → seed)**:dev/product-research 改成 seed-on-bootstrap
   而非 const-include
3. **候选 2(`render_project_claude_md` 模板化)**:跟候选 3 同 PR 一起做
4. **候选 1+8(协议关键字外移)**:跟 §5.3 D 方案合并 PR
5. **候选 7(plugin pipeline)**:独立 PR,涉及 settings.json 写入逻辑

## 7. 已知未决项 + V0.3 deferred

> §4 工厂相关的未决问已在 §4.6 内嵌列出。§5 重构相关的命名 breaking change
> 决议见 §5.4。

V0.2 内必做(虽列在未决但属于 §2/§3 隐含前置):

| 项 | 状态 |
|---|---|
| orchestrator daemon health supervision(MCP entry health check / heartbeat)| V0.2 必做(daemon 死则上述所有方案失效) |
| 1M context 默认启用(bootstrap 配置)| 独立 bug,V0.2 内修(F<N>,加进 dev-coupling-audit.md) |
| inbox 文件派发可靠性(send_to_session 后立即派发)| 独立 bug,V0.2 内修 |
| `PreToolUse` hook 拦截 AskUserQuestion | **升级 V0.2 必做**(机制确定,§2.4 已包含) |

V0.3 deferred:

| 项 | 状态 |
|---|---|
| 反模式候选 4(golden_rules layered merge:team default + phase override) | V0.3 — 不阻塞 V0.2,V0.3+ 多 team 模板生态会用上 |
| 反模式候选 6(`pre_trust_project` 写 `~/.claude.json` → 项目级 settings.json)| V0.3 — 已有 `CCTEAM_DISABLE_TOOL_SURFACE_BOOTSTRAP` env 旁路兜底 |
| watchdog 升级到 Critic agent(M5)整合 | V0.3 / M5 一起评估 |
| Conditional / lazy phase activation via `paths:` glob(借鉴 Claude Code skills `paths:` 字段)| V0.3 — 等多 team 实际场景驱动 |
| Team 重命名(`dev` → `software-development` 等领域命名)| V0.3 — 牵扯 state.json / slug 前缀 / 测试假设过广 |

## 8. 验收

§2 / §3 自循环 + watchdog:
- [ ] dev / product-research 两个 team 在用户离线 4 小时场景下,phase 推进不停滞
  在"等用户输入"状态(除非真的写了 outbox)
- [ ] auto_loop cycle 达到 cap=3 时,产出 escalate 而非 silent reset
- [ ] 任何 phase 收尾时,`.ccteam/` 必有 phase_done / escalate / outbox 至少一个
- [ ] meta-agent watchdog 在 auto_loop cycle ≥ 2 时,主动 surface 一条用户可读
  通知;通知阈值用户可调
- [ ] orchestrator daemon 死亡时,meta-agent / MCP 任意命令入口立即 surface
  "daemon down",不是静默挂起
- [ ] tech-design 加一条新红线:**smart layer 只做 translation 不做 decision**

§4 团队工厂(plugin 模型):
- [ ] 用户能通过 meta-agent 自然语言描述工作流,产出可立即 `ccteam new --team` 跑通的 team
- [ ] 工厂产物是合法 Claude Code plugin 格式(`plugin.json` + 标准子目录),
  Claude Code plugin install 命令也能加载
- [ ] 工厂产物 `team.yaml` 是 plugin 顶级 unknown 字段,Claude Code plugin
  pipeline 自动 strip,不报错
- [ ] `ccteam doctor --validate-team <name>` 校验全 pass
- [ ] `ccteam team publish <name>` 命令把 staging 目录(`~/.config/ccteam/teams/<name>/`)
  转成 plugin repo 或推到 marketplace `directory` source
- [ ] 用户手动改过的 phase markdown,二次跑工厂不会被冲掉(因为协议在
  frontmatter,正文是用户全权,工厂不重写正文)

§5 团队布局重构:
- [ ] 仓内 team 全部迁到 `teams/<name>/team.yaml` + `teams/<name>/phases/`
  布局,old `phases-*/` 目录不再存在
- [ ] `ccteam-core` grep 不到 `phases-` / 任何 team 名字面量
- [ ] 旧 state.json(team=`dev`,无 alias)仍可加载并跑通(serde 兼容)
- [ ] 用户直接 edit `teams/<name>/phases/<phase>.md` 改 prompt,下次 `ccteam new`
  立即生效,无需重启 daemon
- [ ] team 加载走 `TEAM_SOURCES` enum 数组,project > user > repo 整团 first-source-wins;
  缺层静默 fall-through,读容错写严格
- [ ] 369 测试 baseline 不退步;clippy 不新增 warning

§5.3 领域 / 编排分离(详见 `docs/phase-prompt-architecture.md`):
- [ ] frontmatter 加 `completion_signal` / `escalate_grammar_ref` /
  `outbox_question_protocol` 字段;serde alias 兼容旧 yaml
- [ ] orchestrator inject prompt 模板化,据 frontmatter 字段差异化拼装;
  最终 short prompt ≤ 1 KB
- [ ] team.yaml `golden_rules` 拆 `protocol` / `domain` 两段;旧扁平 list
  serde alias 当 `protocol` 加载
- [ ] shipped 三个 team 共 12 个 phase markdown 正文清理:删协议级片段
  (PHASE_DONE 关键词、required_outputs 路径声明、ESCALATE grammar 例子)
- [ ] doctor 加校:frontmatter schema 完整;phase IO 契约一致;正文非空;
  正文含协议关键词 warn(不 fail)
- [ ] `ccteam phase show <team> <phase>` 命令上线,渲染最终 inject prompt + 正文
- [ ] 故意改 phase 正文加废话 — phase 行为不变;故意删 frontmatter
  `completion_signal` — doctor fail-loud

§6 ccteam-core 反模式重构(详见 `docs/v0-2-claude-code-alignment-review.md` §5/§7):
- [ ] **红线检查**:`grep -rE "\b(dev|product-research|meta-agent)\b"` 在
  `crates/ccteam-core/src/` 应只命中注释 / 测试 — 候选 2/5
- [ ] **协议关键字单一 source**:`grep -rE "PHASE_DONE|ESCALATE"` 在
  `crates/ccteam-core/src/` 应只命中 frontmatter 字段读取 / inject prompt
  template;`phases/*.md` / `phases-product-research/*.md` 正文应零命中 — 候选 1/8
- [ ] **TEAM_BUNDLES const 删除**:`templates.rs::TEAM_BUNDLES` /
  `memory_bridge.rs::TEAMS` 不再存在;dev/product-research 改 seed-on-bootstrap — 候选 3
- [ ] **meta-agent declarative**:`if state.team == META_TEAM_NAME` grep 0 命中;
  改用 `TeamSpec.evergreen` flag;V0.3 加新 evergreen team 走同路径 — 候选 5
- [ ] **plugin pipeline 启用**:`tool_surface.rs::RECOMMENDED_AGENTS` const
  删除,ln -sf 路径删除;spawned project session `.claude/settings.json` 写
  `enabledPlugins` 启用 plugin pipeline;现有 ln -sf 用户的 `~/.claude/agents/`
  迁移文档;Task(subagent_type=...) 仍按 `pluginName:` 命名空间正常工作 — 候选 7

## 9. 不在范围

- LLM-as-judge 仲裁 cycle cap(把控制决策搬到 orchestrator,违反红线)
- per-project supervisor session
- 观察性 LLM(observer-only)
- 任何"未来可能用到"的 phase 字段(YAGNI)
- 重写 fix-loop / auto_loop 状态机(在现有机制上扩,不重构)
- team 重命名(`dev` → `software-development` 等领域命名 — V0.3 单独评估)
- phase 双文件分离 / marker 双段 / 正文 lint(方案 A/B/C — 跟 Claude Code 哲学不对齐,直接 deprecate,不留作 backup)
- inject prompt 模板完全外部化(允许用户改协议层) — 破红线
- phase prompt 自动化 dry-run 测试(`ccteam phase test`)— V0.3
- LLM-aware 校验("用 LLM 看 phase prompt 改坏没")— 违反控制平面无 LLM 红线
- ccteam 自营 team 注册中心 / 自营 marketplace — Claude Code plugin pipeline 已覆盖,不另起
- 字段维度 layered merge(team default + phase override 字段级合并)— 整团维度足够,V0.3 看场景驱动
- 反编译细节深挖(eg async hook stdout JSON 协议、bundled skill 解压机制)— `references/claude-code` 是 reverse-engineered 仓,只信顶层模式;细节风险高

---

## Changelog

- 2026-05-07:初稿。基于 V0.1 实测痛点列表 + 一轮架构讨论达成。
- 2026-05-07:§4 加 team 工厂(用户自定义工作流);§5 加团队布局重构。
- 2026-05-07:§5.3 推荐方案从 B+C 切换 D(协议外移到 orchestrator inject prompt);
  详细设计落 `docs/phase-prompt-architecture.md`。
- 2026-05-07:5 路并行 fork 综合 review 后大幅升级:
  - §2 加 Stop hook `exit 2 + stop_hook_active` 防递归实现细节
  - §2.4 PreToolUse 拦截 AskUserQuestion 从 V0.3 待评估升级到 V0.2 必做
  - §3 加 watchdog 信号源说明(不用 SessionEnd,用外部 timer + Stop hook)
  - §4 重写为 Claude Code plugin 模型(团队产物兼容 plugin 格式,share 走 marketplace,
    删 ccteam 自营注册中心)
  - §5.2 加 `TEAM_SOURCES` enum 设计(借鉴 layered settings)+ plugin pipeline 兼容
  - 新加 §6 ccteam-core 反模式重构(6 条 V0.2 必做 + 2 条 V0.3 deferred)
  - 综合分析依据落 `docs/v0-2-claude-code-alignment-review.md`
