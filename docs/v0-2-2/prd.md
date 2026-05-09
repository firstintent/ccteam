# PRD V0.2.2 — 用户反馈 patch round

> 范围:V0.2.2 增量 patch。基于 2026-05-08 用户首批 ccteam 实战反馈
> (6 个 issue 跨 slug 命名 / auto-loop 触发模型 / send-keys 路由 / meta-agent
> 决策树),做最小侵入的根因修复。
>
> base = `origin/main` `170f5a8`(V0.2.1 ship);测试 baseline 511/0。
>
> 这是首个独立目录的 patch 版本(V0.2.1 折在 `docs/v0-2/`);因为本轮 4 条
> finding 实现量比 V0.2.1 dust patch 大,单独建 `docs/v0-2-2/` 留档,日后
> 同样体量的 patch 走同样规约。`docs/README.md` 维护规约配套加一行注。

---

## 1. 背景 — 用户反馈源

2026-05-08 用户在 `dev-dex-ai` / `dev-ccteam-ui` 两个真实项目里跑 ccteam,
撞出 6 类问题。原始反馈见本目录 `feedback.md`(本 PR 一并落档)。归类到
4 个反馈 finding(F34-F37);2026-05-09 用户追加 1 个 UX 增强 finding(F38),
对 F35 enriched outbox 是天然补:

| F | 性质 | 涵盖反馈 issue / 来源 |
|---|---|---|
| **F34** slug 命名失控 | 用户体验 — 缺 `--slug` 入口 | issue #1(`dev-dex-ai` ≠ `hermestrade-home`)+ #2(`dev-ccteam-ui-...-3` 太长) |
| **F35** auto-loop 过度依赖 Stop event | 控制平面盲区 — 静默不分类 | issue #3(API tool-call hang)+ #5(synthesis) |
| **F36** send-keys 注入到活跃 subagent | 控制平面盲区 — 不感知 subagent 状态 | issue #4(/btw 落到 code-reviewer subagent) |
| **F37** meta-agent 绕开 pipeline 自调研 | 决策树软约束被漂移 | issue #6(Multica 调研用 Agent subagent 直出) |
| **F38** 终端截图 PNG | UX 增强 — 给 watchdog/outbox 通知附直观屏幕 | 用户 2026-05-09 追加;补 F35 文本 pane_tail 的视觉维度 |
| **F39** `cct` 短前缀约定 sweep | 命名约定 — binary `ccteam` → `cct`,自带 skill `ccteam-*` → `cct-*`,CLAUDE.md 加约定 | 用户 2026-05-09 追加;F34 skill 重构同源,统一抽出独立 PR(机械重命名,跟逻辑变更解耦)|
| **F40** team 名缩短 + alias 支持 | 命名约定 — `product-research` → `research`(team.yaml `name`),`team.yaml::aliases` 字段载老名兼容老项目 | 用户 2026-05-09 追加;V0.2 PRD §5.4 当时设计 alias 但 deferred V0.3,本次拉回 V0.2.2 做 |

7 finding 不耦合主架构,不破红线;V0.2.2 是渐进 patch,不开新版本主线。

---

## 2. 范围

实现 5 finding + 配套:

- **F34**:`ccteam new --slug <name>` CLI flag + meta-agent role prompt 强制
  派单前确认 slug + B2 自动加 team 前缀语义
- **F35**:orchestrator 事件感知 silence classifier(progress.jsonl 末事件
  语义 × 静默时长 → 4 类响应);capture-pane tail 入 enriched outbox;meta-agent
  propose-confirm UX
- **F36**:send-keys 前检测 active subagent(PreToolUse(Task) 未配 SubagentStop
  → defer);max-defer-minutes 兜底
- **F37**:`meta_agent_role.md` §2 决策树加固(项目请求 vs 问答边界明确
  化 / "调研 X" 默认派 product-research / 反例显式列)
- **F38**:`tmux capture-pane -e` 输出 in-process 灌给 `vt100` Parser(终端状态机,
  纯 Rust,MIT);取 `Screen` 的 cell grid → `imageproc::drawing::draw_filled_rect_mut`
  + `draw_text_mut`(`ab_glyph` 字体)→ `image` 存 PNG。无 font-kit / 无 system
  deps(`ab_glyph::FontRef::try_from_slice` 直接读 TTF 字节)。`mcp__cct__screenshot`
  MCP 工具对外暴露;F35 enriched outbox 加 `screenshot_path` 字段(best-effort,
  vt100 解析 / imageproc 渲染 / 字体加载 / tmux 任一失败时 graceful degrade 到无 PNG,
  主路径不挂)
- **F40**:**team 名缩短 + alias 软迁移**:`teams/product-research/team.yaml::name` 改
  `research`,`aliases: ["product-research"]` 字段保兼容;ccteam-core `team_resolver`
  按 alias 匹配老项目;新项目目录 `~/projects/research-<slug>/`,老项目仍走
  `~/projects/product-research-<slug>/` 不动;`team.yaml::description` 字段载全称
  ("Product research team — kickoff → research → verdict → next-steps")用于工厂 /
  doctor / cct-project-creator skill 展示场景;不动用户数据,无破坏性迁移
- **F39**:**`cct` 短前缀约定 sweep**:`crates/ccteam-cli/Cargo.toml::[[bin]] name`
  从 `ccteam` 改 `cct`;自带 skill 三个全部 `cct-*` 命名(`cct-control` / `cct-team-author` /
  `cct-project-creator`);移到顶层 `skills/` 目录;CLAUDE.md / README.md /
  tech-design.md / interfaces.md 全文 sweep `ccteam <cmd>` → `cct <cmd>`;`ccteam
  doctor` 自动迁移 V0.1/V0.2 用户的老 settings.json 中绝对路径 + 老 skill 目录;
  历史 `docs/v0-1/` `docs/v0-2/` 不改

配套:

- **Cargo workspace version bump**:`workspace.package.version` `"0.0.1"` → `"0.2.2"`
  (V0.1 / V0.2 ship 时未同步,V0.2.2 起正式跟进 — 每个 minor 与 patch 都 bump)
- **CLAUDE.md §五 PR 纪律**追加 "patch 开发流程"小节(≤ 8 行),记 doc-first
  → worktree-per-fix subagent → PR → main review → fix → merge → cargo bump
- **`docs/README.md` 维护规约**追加 patch 版本目录约定(本轮起每个 patch 单独
  目录)

---

## 3. F34 — slug 命名控制

### 3.1 两个根本问题

| # | 问题 | 表现 |
|---|---|---|
| **1** | **生成算法本身质量差** — `pick_unused_slug` 用 `slugify(brief)` 做字符级 normalize(只截 40-char、按 `-` 边界回滚),无 token / 语义层裁剪 | brief "ccteam ui — V1.2 session subagent 3" → 38 char,过 40 cap → `dev-ccteam-ui-v1-2-session-subagent-3` 全文塞进去 |
| **2** | **缺用户/meta-agent 指定接口** — `ccteam new` 没 `--slug` flag,只能任由算法生成 | 用户原话"ccteam-ui",meta-agent 没法把它直接派给 ccteam,只能等结果再后悔 |

两条**正交**:即便算法升级到 token-aware,用户仍需要一个 override 入口
(eg 品牌名 `hermestrade-home` 算法永远猜不到);即便有 `--slug`,用户/meta-agent
不传时的 default 仍要够用。两条都修。

### 3.2 设计 — 四层调用,智能优先

slug 决定按以下优先级,**首层智能 fallback 优先**,deterministic 算法降为最终兜底:

```
Tier 1: --slug <name> 显式            (用户 / meta-agent 直传)         零延迟,确定
       ↓ 缺
Tier 2: meta-agent NL 推荐 + 用户确认  (派单工作流内,LLM 已在场)        零额外 LLM call
       ↓ 不在 meta-agent 工作流(用户直接 CLI 跑)
Tier 3: ccteam new 内 shell out claude -p 智能生成 + Y/n 确认           ~2-5s,~$0.0001
       ↓ claude 不可用 / network 失败 / 用户 --no-auto-slug
Tier 4: 字符级 deterministic slugify_brief()                             零延迟,最低质量
```

红线检查(在 §3.2.6 末段):**"tmux 长 session 不用 claude -p"** 针对项目 session
lifecycle,slug 生成是 < 5s 一次性 utility,不在该红线范围;**"控制平面无 LLM"**
针对控制决策(re-inject / kill / 改 phase),slug 命名是数据转换(brief → name),
不在该红线范围。两条都 OK。

#### 3.2.1 Tier 1 — `--slug` CLI flag(显式)

`ccteam new` 加 `--slug <name>` 可选 flag(`crates/ccteam-cli/src/main.rs:71`
`Commands::New` 加字段;`commands.rs:138` `run_new` 多接 `slug: Option<&str>`)。

**前缀语义 — B2 自动加**(decided):

| 用户输入 | team | 实际 slug | 说明 |
|---|---|---|---|
| `--slug ccteam-ui` | `dev` | `dev-ccteam-ui` | 自动加 team 前缀 |
| `--slug dev-ccteam-ui` | `dev` | `dev-ccteam-ui` | 已带前缀 verbatim |
| `--slug hermestrade-home` | `dev` | `dev-hermestrade-home` | 无 team 前缀就加 |
| `--slug product-research-foo` | `dev` | `dev-product-research-foo` | `dev` ≠ `product-research`,加 |

规则:若 `slug.starts_with(&format!("{team}-"))` 则 verbatim;否则 prepend
`<team>-`。slug 必须满足 `[a-z0-9-]+`;非法字符 fail-loud。撞名 append `-{4hex}`
retry(沿用 `pick_unused_slug` 现逻辑)。

#### 3.2.2 Tier 2 — `cct-project-creator` skill(meta-agent 派单工作流)

派单工作流封装到一个**新 skill**:`cct-project-creator`,跟已 ship 的
`ccteam-control`(M1.8)+ `ccteam-team-author`(M0.22)同模式。skill 把
项目创建的全流程(需求澄清 → slug 推荐 → team 选择 → 派单)从 `meta_agent_role.md`
里抽出来,role prompt 只保留"什么时候该走 skill"的入口规则,流程细节走 skill
body。

**关系**:F37 改 `meta_agent_role.md` §1 自检(项目请求 vs 问答),F34 加
project-creator skill 接管 §2-§4 派单子流程;两个 finding 互补,**同 PR 落地**
(F34 + F37 PR #1)。

**skill 命名 / 顶层目录 / 迁移逻辑** — 全部归 **F39**(`cct` 约定 sweep,见 §8)
处理。F34 PR 只**消费** F39 已经建好的 `skills/` 目录 + `cct-*` 命名,**新加**
一个 `skills/cct-project-creator/SKILL.md` 文件(skill body 内容是 F34 设计核心)。
F39 PR 必须先 merge,F34 PR 才在新 directory + 新命名上做 follow-up。

**前置约定 — `AskUserQuestion` 在 meta-agent 允许**:

V0.2 §2.4 用 PreToolUse hook 在 **project session** 拦截 `AskUserQuestion`(替换为
outbox 协议),但**不拦 meta-agent**(`~/projects/<handle>-meta/.claude/settings.json`
不写该 matcher)。所以 project-creator skill 跑在 meta-agent 里时,**鼓励用
`AskUserQuestion` 做 slug / team / 关键澄清的结构化选择**,UX 显著优于纯 NL Q&A
(用户点选 < 反复打字)。

> 红线 reminder:本 skill 显式只在 meta-agent 调用,不被 phase 模板 reference,
> 不被 project session 触发。AskUserQuestion 在 project session 触发会被 hook deny
> (V0.2 已 ship)。

**skill body 结构**(草稿,实施时写到模板文件):

```markdown
---
name: ccteam-project-creator
description: 当用户要创建新 ccteam 项目时调用 — 走需求澄清 → slug 推荐 → team 选择 → 派单四步对话流。本 skill 仅在 meta-agent context 内使用,自带使用 AskUserQuestion 做结构化选择。
trigger:
  - 用户说"新项目" / "建一个 X" / "做个 X" / "我想做 X"
  - 用户说"调研 X" / "评估 X" / "看看 X 值不值"(走 product-research team)
---

## 角色边界

你是项目创建对话向导,**不是 worker**。本 skill 跑完后调 `ccteam new --slug <X> --team <Y> "<refined brief>"` 派给项目 session 干活,你不写代码。

## Phase A — 需求澄清

读用户原始 brief。若信息密度足够(≥ 2 句明确技术形态 / 目标 / 约束)→ 跳 Phase B。

若 brief 单词级(eg "做个 todo"),用 `AskUserQuestion` 问 **1 个**最关键澄清,
options 给典型选择 + "Other" 让用户自定:

```
AskUserQuestion({
  question: "项目什么形态?",
  options: [
    { label: "Web 应用", description: "浏览器跑,带前端" },
    { label: "CLI 工具", description: "命令行,纯文本" },
    { label: "移动端", description: "iOS / Android" },
  ]
})
```

只问 1 次,不连珠炮。

## Phase B — slug 推荐 + 用户确认(用 AskUserQuestion)

基于 brief + Phase A 答复,算推荐 slug:

规则:
- **优先用户提到的品牌名 / 专有名词**(eg "HermesTrade DEX" → `hermestrade-home`)
- **不 verb-leading**:用 `todo-cli` 不用 `build-todo-cli`
- **2-4 token,kebab-case**
- **不带 team 前缀**(`ccteam new` 自动加 dev- / product-research- 等)

用 `AskUserQuestion` 给用户三选一:

```
AskUserQuestion({
  question: "项目 slug 用什么?",
  options: [
    { label: "<推荐 slug>", description: "基于 brief 的 <X>;推荐用这个" },
    { label: "我来定", description: "选这个我下面会问你想用什么" },
    { label: "再来一个", description: "换思路重算一次" },
  ]
})
```

用户选"我来定" → 紧接一句 NL "你想用什么 slug?(eg `hermestrade-home`)";
校 `[a-z0-9-]+` + 长度 ≤ 60。
用户选"再来一个" → 换 角度重算 + 再 AskUserQuestion。

## Phase C — team 选择(用 AskUserQuestion)

按 `meta_agent_role.md` §2 团队决策树(F37 加固后)默认推断;不确定时:

```
AskUserQuestion({
  question: "派给哪支团队?",
  options: [
    { label: "dev", description: "立即开发(plan-eng → implement → ship)" },
    { label: "product-research", description: "先调研判断 idea 值不值得做" },
  ]
})
```

## Phase D — 派单 + 通知

```bash
Bash: ccteam new --slug <slug> --team <team> "<refined brief>"
```

派完写 outbox `event_kind: reply`:
- 项目 slug + 派的团队
- 第一个里程碑预期(dev: plan-eng ~30 min;research: kickoff 反向面试)
- 跟踪命令(`ccteam show <slug>` / `ccteam attach <slug>`)
```

**meta-agent role prompt 改动**(`meta_agent_role.md` §2):

> §2 决策树第 4 步派单段简化为:
> "走 `ccteam-project-creator` skill(已自动装好,在 `~/.claude/skills/`)。skill 会
> 引导你跑需求澄清 → slug 推荐 → team 选择 → 派单 全流程。"

派单后**不支持改名**(state.json / `~/.claude/rules/` paths / tmux session
全要重命名,V0.3 评估)。

#### 3.2.3 Tier 3 — `ccteam new` shell out `claude -p` 智能 fallback

用户**不在 meta-agent 工作流**直接跑 `ccteam new "<brief>"`(无 `--slug`)时,
ccteam 自己 shell-out 一次短命 `claude -p` 生成推荐 slug。

**机制**:

```rust
// crates/ccteam-cli/src/commands.rs::run_new
//
// 1. 若用户传 --slug → 跳过本节
// 2. 检测 stdin 是否 tty + claude 是否在 PATH + --no-auto-slug 是否传
//    任一缺 → 跳到 Tier 4(slugify_brief)
// 3. shell out claude -p:
let prompt = format!(
    "Generate a 2-4 token kebab-case slug for a project with this brief:\n\
     '{brief}'\n\n\
     Rules:\n\
     - Capture the core noun/concept (brand name if present), not action verbs\n\
     - Drop stop words (a, the, of, etc) and pure-digit tokens\n\
     - Output ONLY the slug, no explanation, no quotes, no markdown\n\
     Examples:\n\
     - 'AI recipe generator from fridge photo' -> recipe-generator\n\
     - 'Build a todo cli with ratatui' -> todo-cli\n\
     - 'HermesTrade DEX prediction market' -> hermestrade-dex\n\
     Slug:"
);
let suggestion = Command::new("claude")
    .args(["-p", "--model", "claude-haiku-4-5-20251001"])  // 用 haiku 降本+提速
    .stdin(piped(prompt))
    .timeout(Duration::from_secs(15))  // 超时硬截
    .output()?;
let suggestion = sanitize(suggestion.stdout);  // [a-z0-9-]+ 校验 + trim
```

**用户 UX**:

```
$ ccteam new "Build HermesTrade DEX home"
[ccteam] querying claude for slug recommendation...
[ccteam] suggested: dev-hermestrade-home
[ccteam] accept? [Y/n] (or rerun with --slug to override):
```

- 用户回车 / `y` / `Y` → 用建议
- 用户 `n` → 退出 + print "rerun with --slug <name>"
- 非 tty 上下文(脚本 / e2e harness)→ **auto-accept** 建议(15s 超时硬截兜)

**flag 语义**:

| flag | 行为 |
|---|---|
| `--slug X` | 跳过 Tier 2/3,用 X(B2 前缀语义) |
| `--no-auto-slug` | 跳过 Tier 3,直接 Tier 4 deterministic |
| `--auto-slug-model haiku/sonnet` | 选 claude -p 模型,默认 haiku(成本最低) |
| 默认无 flag | Tier 3 智能 + Y/n 确认(交互);非 tty 时 auto-accept |
| env `CCTEAM_AUTO_SLUG=off` | 全局禁用 Tier 3 |

**成本与延迟**:

- haiku-4.5 一次 ~60 token in / ~10 token out → ~$0.0001 / 次
- 网络延迟 + 模型推理 ~2-5s,15s 硬超时
- 一次 `ccteam new` 投入,项目 lifecycle 长(小时-天级),边际成本可忽略

**失败 fallback 链**:

| 失败模式 | 行为 |
|---|---|
| `which claude` 不在 PATH | log warn,降级 Tier 4 |
| `claude -p` exit code 非 0 | 同上 + reason "claude returned error" |
| 15s 超时 | kill child + 降级 Tier 4 + reason "claude timeout" |
| stdout 空 / 含非法字符 / 长度异常(> 60 char) | sanitize 失败 → 降级 Tier 4 |
| 用户拒绝建议(交互模式)| exit 1,提示 "rerun with --slug <name>" |

#### 3.2.4 Tier 4 — `slugify_brief()` deterministic 兜底

LLM 不可用时仍要给个能跑的 slug。新函数 `slugify_brief()`(**不动 `slugify()`**,
后者被 meta-agent path `<handle>-meta` 用,handle 已短不能裁):

```rust
/// 把自由文本 brief 压成 ≤ 3 个有意义 token 的 slug base。
/// 流程:
///   1. 字符 normalize(复用 slugify 的 [a-z0-9] / `-` 折叠)
///   2. 按 `-` 切 token,过滤:
///      - 英文 stop word(a / an / the / of / to / for / with / that / and / or / in / on / at / is / are)
///      - 纯数字 token(保留 `v2` / `2k` 这类含字母的)
///      - 长度 < 2 的 token
///   3. 去重连续重复(`ccteam ccteam ui` → `ccteam ui`)
///   4. 取前 3 个剩余 token,join `-`
///   5. 兜底兜底:过滤后 0 token → fall back 到旧 `slugify()` 输出
pub fn slugify_brief(input: &str) -> String { /* ~30 LoC */ }
```

预期产出对照(Tier 4 路径):

| brief | 旧 `slugify()`(40-char cap) | 新 `slugify_brief()` |
|---|---|---|
| `ccteam ui — V1.2 session subagent 3` | `ccteam-ui-v1-2-session-subagent-3` | `ccteam-ui-v1` |
| `Build a tiny Python CLI that converts CSV to JSON` | `build-a-tiny-python-cli-that-converts` | `build-tiny-python` |
| `AI recipe generator from fridge photo` | `ai-recipe-generator-from-fridge-photo` | `ai-recipe-generator` |
| `Predict market + DEX` | `predict-market-dex` | `predict-market-dex` |
| `HermesTrade DEX home` | `hermestrade-dex-home` | `hermestrade-dex-home` |
| `do the thing` | `do-the-thing` | `do-thing` |
| `to of and` | `to-of-and` | fall-back `to-of-and`(全 stopword) |

Tier 4 是 deterministic 字符 / token 规则,**不引入 NLP 库**;质量明显低于
Tier 2/3 但保证可用。

#### 3.2.5 红线 / cost / latency 总结

| 关注点 | Tier 1 显式 | Tier 2 meta-agent | Tier 3 claude -p | Tier 4 deterministic |
|---|---|---|---|---|
| 红线 | OK | OK(meta-agent 已是 LLM session,内嵌推荐) | OK(utility 性,非控制路径) | OK |
| LLM cost | 0 | 0(已在 meta-agent turn 内) | ~$0.0001 / 次(haiku) | 0 |
| 延迟 | 0 | 0(派单流程内) | 2-5s | 0 |
| 智能度 | 用户决定 | 高(meta-agent 看完 brief) | 高(独立 LLM judgment) | 低 |
| 触发条件 | `--slug` flag | 走 meta-agent 派单 | 直接 CLI + 无 `--slug` + 默认 | LLM 不可用 / `--no-auto-slug` |

### 3.3 不做

- **slug 重命名**:涉及 state.json 迁移 / `~/.claude/rules/` paths regex / tmux
  session 名 / `.ccteam/auto-loop.state.md::slug` 字段 / outbox / progress.jsonl
  全条线。V0.3 单独评估
- **强制 `--slug`**:Tier 2/3 智能默认已大幅改善质量,不强制
- **`claude -p` 异步并行 scaffold**:slug 决定后才能建项目目录,顺序强相关,
  不可并行;15s 超时是上限
- **缓存 `claude -p` 结果**:每次 brief 唯一,缓存意义不大
- **NLP / POS 标注库**(spacy / nltk):Tier 4 deterministic 是兜底,故意保持
  字符级简单;真要 NLP 走 Tier 3 LLM 路径
- **多语言 brief**(中文 / 日文):Tier 4 char-set 是 [a-z0-9],中文 brief 走 Tier 4
  会全部丢字 → fallback "project";Tier 3 LLM 自然支持多语言,推荐用户走默认

---

## 4. F35 — 事件感知 silence classifier

### 4.1 问题

`auto_loop.rs::decide()` 只在 Stop hook 触发时跑(input = `last_assistant_text`)。
两个失效场景:

- **API tool-call hang**:`PreToolUse` 发了但 `PostToolUse` / `Stop` 永不来 → auto-loop
  根本不触发 → 项目永远卡在 iteration 1
- **send-keys 路由错误后**(F36 case):`phase_inject` event 发了但主 agent 没
  收到(prompt 落到 subagent 上下文)→ 没 Stop → auto-loop 不触发

`stall.rs` 已有 5/15/30 min 软告警,但只是 watchdog 翻译信号,不**驱动**任何
recovery 动作。

### 4.2 设计 — 事件感知分级

#### 4.2.1 分类规则(deterministic)

orchestrator daemon 主循环新增 silence classifier 步骤,读 `progress.jsonl`
末事件 + 静默时长,按下表分 4 类:

| 末事件 | 静默时长 | 分类 | 响应 |
|---|---|---|---|
| `PreToolUse(tool=Task)` 后无 `SubagentStop` | < phase escalate 阈值(默认 30min) | `SubagentBusy` | 耐心,不动 |
| `PreToolUse(tool=Task)` + 静默 ≥ escalate 阈值 | — | `SubagentRunaway` | enriched escalate(subagent 跑超时) |
| `PreToolUse(tool ≠ Task)` 后无 `PostToolUse` 且 ≥ warn 阈值 | — | `MidToolHung` | enriched escalate(tool 看起来 hang) |
| `Stop` / `SubagentStop` 后 `auto-loop.state.md` 未更新 + 无新事件 ≥ warn 阈值 | — | `PostStopLimbo` | deterministic re-inject 1 次,再失败 → enriched escalate |
| `phase_inject` 后无任何 event ≥ warn 阈值(F36 case) | — | `InjectLimbo` | deterministic re-inject 1 次(配合 F36 subagent guard:若仍有 active subagent,等 SubagentStop 后真发) |
| 其他(`PostToolUse` / `phase_done` / `escalate` / 空) | — | `Healthy` 或 `Terminal` | 不动 |

`<phase>.stall_warn_minutes` 仍是阈值基底(`stall.rs::StallThresholds::from_phase`
驱动 warn / suspicious / escalate);classifier 复用,不另定义。

#### 4.2.2 enriched escalate outbox payload

写到 `<project>/.ccteam/needs_attention.outbox.json`(已有路径,Stop hook L3
fail-safe 也写这里 — 共享 schema)。新字段:

```json
{
  "schema_version": 1,
  "event_kind": "escalation",
  "priority": "high",
  "ccteam_classification": "MidToolHung",  // 新字段
  "ccteam_silent_seconds": 900,            // 新字段
  "ccteam_last_event": {                    // 新字段:末 progress event 摘要
    "ts": "...",
    "event": "PreToolUse",
    "tool": "Read"
  },
  "ccteam_pane_tail": "...30 行 capture-pane...",  // 已有协议,新 case 复用
  "body": "项目 X 在 implement 第 12 分钟卡在 Read(...) 后无 PostToolUse..."
}
```

`capture_pane_tail` 从 `parse_phase_end.rs:313` 提到 `ccteam-core/src/tmux.rs`
做共享 helper(F35 + 现有 Stop hook L3 共用),保留"永不解析终端输出"红线
(只入 outbox 给 meta-agent / 用户读,不进 orchestrator 状态机)。

#### 4.2.3 meta-agent surface(propose-confirm,不 autonomous)

`meta_agent_role.md` §7 watchdog 段补充:见到 enriched escalate 时,**生成
informed proposal** 给用户:

> "项目 X 在 implement 第 12 分钟卡在 Read(file.md) 后无 PostToolUse,看起来
> tool hang 而非 subagent 慢工。要不要 (a) `ccteam attach dev-x` 自看 (b)
> 让我等再 5 min 重看 (c) 这条不管了?"

**红线**:meta-agent 只 surface 选项,**不自己执行 Ctrl+C / 重发 / kill**;
`PostStopLimbo` / `InjectLimbo` 的 deterministic re-inject 由 orchestrator 直接
做(已是 deterministic 路径,不经 LLM)。

#### 4.2.4 触发节奏

orchestrator daemon 主循环已有 `process_project` per-project tick;classifier
塞进 tick(每个 project 每 5-10s 检查一次,无新 event 时跑 classify)。计算
廉价(读 progress.jsonl 末几行 + 简单 match),不增加 IO 压力。

### 4.3 不做

- meta-agent autonomous decide(直接发 Ctrl+C / 重发):破"控制平面无 LLM"红线
- LLM-aware classification("用 LLM 看屏幕判断卡没卡"):同上
- 增加新 phase YAML 字段(`idempotent_inject` 之类):phase prompt 本质是
  叙事性指令,re-inject 已知安全(LLM 自看 `.ccteam/` 状态决定下一步,不重复
  执行已做动作);限流靠 1 次 retry cap 和 `auto_loop` 现有 cycle cap

---

## 5. F36 — Send-keys subagent guard

### 5.1 问题

`dispatch_phase_with_state` 在 `progress.jsonl` 末事件是 `PreToolUse(tool=Task)`
(subagent 活跃)时,仍发 `/btw <prompt>` 到 tmux pane。tmux send-keys 实际
打到主 agent 的 input buffer,但主 agent 此刻在 wait subagent 完成,**Claude
Code 把这段文本交给 subagent 处理**(行为已被用户 #4 实测)。

`is_idle()` 当前把 `PreToolUse` 归 busy → 走 `/btw` 包裹路径,但 `/btw` 在
subagent 活跃场景行为退化。

### 5.2 设计 — Defer until SubagentStop(C1)

`progress.rs` 新增 `subagent_active(events: &[Value]) -> bool`:扫 progress.jsonl
末事件序列,counting `PreToolUse(tool=Task)` 和 `SubagentStop`,若开多于关 → true。

`Orchestrator` 注入路径(`dispatch_phase_with_state` 等)前加 guard:

```rust
if subagent_active(&recent_events) {
    self.queue_pending_inject(slug, phase, attachment_refs);
    return Ok(());  // 或返 PendingInject 状态,daemon 主循环按 SubagentStop event 触发真发
}
```

**Pending inject 落盘**:`<project>/.ccteam/pending-inject.json`(单文件,最新覆盖
旧的;不积累队列 — 用户 ccteam new / dispatch 不会高频触发,排队不必)。orchestrator
daemon 主循环在每次 progress event tick 时 check:若文件存在 + subagent 已不活跃
+ 距 enqueue 时间未超 `max_defer_minutes`(默认 10) → 真发,删文件;若超时 → fail-loud
escalate(`<project>/.ccteam/needs_attention.outbox.json` 加新 classification
`InjectDeferTimeout`,走 F35 同 channel surface)。

### 5.3 跟 F35 的协同

F35 的 `InjectLimbo` 类(phase_inject 后无后续事件)+ F36 的 pending-inject
是同一信号的两个面:

- **F36 主路径**:发的当下检测到 active subagent,主动 defer
- **F35 兜底**:即使 F36 没接住(eg subagent 过几秒才 emit `PreToolUse(Task)`,
  F36 检测时 race window 没 catch),phase_inject 后无 follow-up 事件超 warn 阈值
  → F35 的 `InjectLimbo` 兜底重发

两个 finding 同 PR / 不同 PR 都行(见 §7 PR sequencing 推荐 F35 先 / F36 后,
F36 复用 F35 的 enriched outbox 入口)。

### 5.4 不做

- C2 项目侧多文件队列(`<project>/.ccteam/pending-inject/<ts>.json` 累积):没场景
  支撑队列(每个项目每次只关心最新待发的 phase prompt)
- C3 改 inbox 文件机制 dispatch:V0.3 architecture-scale 改造,V0.2.2 不开

---

## 6. F37 — meta-agent 决策树加固

### 6.1 问题

`meta_agent_role.md` §2 决策树写明"问答 vs 项目请求"分流,但**软约束被漂移**:
2026-05-08 用户问"调研 Multica 项目",meta-agent 没派 product-research,而是
直接用 Agent subagent 做 web 搜索 + 直出结论。

根因:决策树 §1 步把"X 是什么?"判为问答(对),但"调研 X / X 值不值得做 / X
有人做过吗"这种语义在 §2 团队选择段才区分,§1 步的反例也没写"调研 X = 项目
请求"。LLM 看到 §1 "问答" 路径就直答,跳过 §2。

### 6.2 设计 — 决策树加固

`crates/ccteam-core/src/templates/meta_agent_role.md` §2 改:

#### 6.2.1 §1 步加反例 + 显式 hand-off

```markdown
### 第 1 步:这是问答还是项目请求?

- **问答** —— 边界很窄:用户在问一个**事实** / **定义** / **状态**
  - 例:"ccteam 的 Seed Gate 是什么意思?"/ "Multica 的 GitHub 地址是什么?" / "我的 todo-cli 项目跑到哪了?"
  - 直接回答。可以用 `Bash` 调 `ccteam ls --format json` 之类拿数据
  - **绝不**自己起 `Agent(subagent_type=...)` 做调研、检索、分析 — 那是项目请求
- **项目请求** —— 任何"做 / 写 / 调研 / 分析 / 评估 / 看看 X 值不值得"
  - 例:"调研 Multica" / "看看这个 idea 能不能做" / "做个 todo cli" / "X 项目市场怎么样"
  - **不要直接干** — 进入第 2 步
- **边界不清**:用户措辞模棱两可("X 项目怎么样?"既可能是事实询问也可能是产研请求)
  - **问一句**:"是要我快速答你一个事实问题,还是走 product-research 团队正式产研?"
  - 不要默默选一边
```

#### 6.2.2 §3 克制规则强化反例

`§3` 已写"❌ 不要 Edit / Write 用户代码";新增显式反例:

```markdown
- ❌ **不要**自己起 `Agent(subagent_type=general-purpose)` / 调用 web 搜索工具
  做调研、市场分析、技术对比 — 这是 product-research 团队的活,绕过 = 失去
  6 phase pipeline + verdict 结构化判断 + 可审计调研记录
- ✅ "调研 X" / "评估 X 值不值得" → `ccteam new --team=product-research --slug=<name> "<brief>"`
```

### 6.3 跟 F34 的协同

F34 派单前确认 slug 的约束(§3.2.3)合并写在同一个 §2.4 派单段;两条改同
一文件 `meta_agent_role.md`,**同 PR 落地**(见 §7 PR sequencing)。

### 6.4 不做

- 编程层强制(eg orchestrator 拦截 meta-agent 的 Agent tool call):破"meta-agent
  是 LLM session,管理靠 prompt 不靠程序拦截"边界
- 所有 "research X" 强制问 confirmation:误报多(用户真想要快答时)。靠决策
  树边界 + 软约束 + 用户回弹纠错

---

## 7. F38 — 终端截图(PNG)

### 7.1 问题 / 目标

F35 enriched outbox 已带 `pane_tail`(纯文本 30 行),解决了 meta-agent 翻译
所需的语义信号,但**用户体感缺一个直观维度** — 终端 ANSI 颜色 / progress bar /
框线字符在文本里全失真。channel layer(M2+ Telegram / Web UI)上线后,
PNG 是天然附件。即便 V0.2.2 没 channel adapter,outbox 里附 `screenshot_path` +
NL 提示 "(截图 file:///path/to.png)",用户可以 `xdg-open` 立即看。

也独立暴露成 MCP 工具 `mcp__ccteam__screenshot`,meta-agent / 用户的 daily-driver
claude 可以 ad-hoc 抓一张当前 session 截图("我项目卡住了,截个图给我看看")。

### 7.2 设计

#### 7.2.1 渲染管线

```
tmux capture-pane -e -p -t ccteam-<slug> -S -<lines>
        │  (stdout = ANSI-escaped 字节流;同步 query 一下 pane width)
        ▼  in-process,纯 Rust 全栈
vt100::Parser::new(rows, cols, 0).process(&bytes)
        │  - 完整 VT 谱状态机(光标 / 滚动 / 颜色属性 / italic / reverse / 等)
        │  - parser.screen().cell(r, c) → Cell { contents, fgcolor, bgcolor, italic, ... }
        ▼
imageproc::drawing
        │  - 加载 TTF 字节(`ab_glyph::FontRef::try_from_slice(fs::read(path)?)`)
        │  - 遍历 grid:每 cell 先 draw_filled_rect_mut(bg) 再 draw_text_mut(fg + char)
        ▼
image::ImageBuffer::save("<project>/.ccteam/screenshots/<utc>.png")
```

**纯 Rust 全栈,无 Linux system deps**(关键改进:跳过 `font-kit`,直接 `ab_glyph::FontRef::try_from_slice`
读 TTF 字节;cargo build 不再要 `libfontconfig` + `libfreetype` 系统包)。

#### 7.2.2 实施载体 — `vt100 + imageproc + image + ab_glyph` DIY(选定)

```toml
# crates/ccteam-core/Cargo.toml 新增 deps
vt100      = "0.15"     # 解析 ANSI → 终端 cell grid(Parser / Screen / Cell)
image      = "0.25"     # RgbImage / PNG 编码
imageproc  = "0.25"     # draw_filled_rect_mut + draw_text_mut + text_size
ab_glyph   = "0.2"      # FontRef + PxScale(imageproc 已经依赖,显式声明)
```

| 维度 | `vt100 + imageproc` DIY(选定) | `ansee` crate(已废) | `freeze` shell-out(已废) |
|---|---|---|---|
| ccteam-core 实现量 | ~200-250 LoC Rust(parser → cell grid → 渲染层 + ANSI_256 表) | ~80 LoC(crate 包装) | ~80 LoC(shell-out + degrade) |
| 用户运行时依赖 | 0 | 0 | Go static binary 单装 |
| **编译期系统依赖** | **0**(全纯 Rust) | Linux:`libfontconfig1-dev` + `libfreetype6-dev`(font-kit) | 0 |
| ANSI 完整度 | **完整 VT 谱**(vt100 是 mature 终端状态机) | `ansi-parser 0.9` 早期 | freeze 自处理 |
| 颜色覆盖 | 16 / 256 / RGB(`vt100::Color::Rgb` / `Idx` / `Default` 全 match) | 同 | 全 |
| 字体策略 | vendored TTF + `include_bytes!`(binary 自洽);env `CCTEAM_SCREENSHOT_FONT_TTF` 运行时覆盖 | font-kit 系统查 | freeze 自处理 |
| 字体 fallback | 单字体(env 切覆盖 CJK / emoji 字体)| 同 | freeze 自处理 |
| 维护风险 | 自维护 ~200 LoC,跟 vt100 / imageproc 上游版本 drift | 早期 v0.1.x crate 边缘 ANSI 可能 panic | 上游 segfault 已暴露 |
| 平台兼容 | **macOS / Linux / Windows 全 OK** | Linux 5.15 OK | **Linux 5.15 segfault** |
| 跟 V0.2.2 patch 边界 | 边界内(单 PR ~250 LoC + 150 LoC 测试) | 边界内 | 边界内 |

**选定 `vt100 + imageproc` DIY**,综合优势:
- 用户诉求"纯 Rust"直命中,完全无 system C 库依赖(比 ansee 路径关键改进)
- vt100 是 mature 终端状态机,ANSI 完整度高于 ansee 用的 `ansi-parser 0.9`
- ccteam-core 完全自主可控渲染细节(cell 大小 / theme / 颜色映射)
- 单 PR ~250 LoC 跟 F35 同量级,V0.2.2 patch 边界守住

**字体获取策略**(自洽,无系统字体 API 介入):
- **首选**:vendor `JetBrainsMono-Regular.ttf`(Apache 2.0 / OFL,~150 KB)进
  `crates/ccteam-core/assets/fonts/`,`include_bytes!` 编译时打包进 binary
- **运行时覆盖**:env `CCTEAM_SCREENSHOT_FONT_TTF=/path/to/other.ttf`
  (用户 / project 想换 CJK / emoji 覆盖字体时设)
- **不做**:`/usr/share/fonts/...` 硬编码路径(在 build host 不一定存在,影响
  cross-compile / packaging);font-kit 系统字体查询(拉系统 C deps)
- **license 干净**:JetBrains Mono 是 OFL(Open Font License,允许 embed +
  redistribute);ccteam 整体 license 表加一条 third-party-fonts 注

#### 7.2.3 Rust 调用路径

`crates/ccteam-core/src/screenshot.rs`(新模块,~250 LoC 含 ANSI_256 表 + 字体加载):

```rust
use ab_glyph::{FontRef, PxScale};
use image::{Rgb, RgbImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;
use vt100::Parser;

// 编译期 vendored TTF(JetBrains Mono,OFL)
const VENDORED_TTF: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

// ANSI 256 色表(完整定义在 src/screenshot/ansi_palette.rs)
const ANSI_256: [Rgb<u8>; 256] = [ /* ... 16 标准 + 216 立方 + 24 灰阶 ... */ ];

pub fn render_screenshot(
    paths: &CcteamPaths,
    slug: &str,
    lines: usize,
) -> Result<Option<PathBuf>> {
    // 1. capture-pane bytes(ANSI escape 保留)
    let ansi_bytes = match capture_pane_with_ansi(slug, lines) {
        Some(b) => b,
        None => return Ok(None),  // tmux 不可用 → 主路径不挂
    };

    // 2. vt100 状态机:rows / cols 从 tmux display-message 查 pane 实际尺寸
    let (rows, cols) = query_pane_dims(slug).unwrap_or((24, 80));
    let mut parser = Parser::new(rows, cols, 0);
    parser.process(&ansi_bytes);
    let screen = parser.screen();

    // 3. 字体加载:env 覆盖优先,缺则 vendored
    let ttf_bytes: std::borrow::Cow<[u8]> = match std::env::var("CCTEAM_SCREENSHOT_FONT_TTF") {
        Ok(p) => std::fs::read(&p)
            .map(Cow::Owned)
            .with_context(|| format!("read {p}"))?,
        Err(_) => Cow::Borrowed(VENDORED_TTF),
    };
    let font = FontRef::try_from_slice(&ttf_bytes).context("parse ttf")?;
    let scale = PxScale::from(14.0);

    // 4. cell 度量:量一个 'M' 字符的实际宽高(monospace 假设)
    let (cell_w, cell_h) = text_size(scale, &font, "M");
    let img_w = cols as u32 * cell_w + 2 * PADDING;
    let img_h = rows as u32 * cell_h + 2 * PADDING;
    let mut img = RgbImage::from_pixel(img_w, img_h, Rgb([30, 30, 30])); // 默认 dark bg

    // 5. 遍历 cell:画 bg 矩形 + fg 字符
    for r in 0..rows {
        for c in 0..cols {
            let cell = match screen.cell(r, c) { Some(c) => c, None => continue };
            let bg = vt100_color_to_rgb(cell.bgcolor(), Rgb([30, 30, 30]));
            let fg = vt100_color_to_rgb(cell.fgcolor(), Rgb([204, 204, 204]));
            let x = (PADDING + c as u32 * cell_w) as i32;
            let y = (PADDING + r as u32 * cell_h) as i32;
            draw_filled_rect_mut(&mut img, Rect::at(x, y).of_size(cell_w, cell_h), bg);
            let s = cell.contents();
            if !s.is_empty() {
                draw_text_mut(&mut img, fg, x, y, scale, &font, s);
            }
        }
    }

    // 6. 保存(整步 catch_unwind 兜潜在 panic)
    let out = paths.project_screenshot_path(slug);
    std::fs::create_dir_all(out.parent().unwrap())?;
    img.save(&out).context("save png")?;
    Ok(Some(out))
}

fn vt100_color_to_rgb(c: vt100::Color, default: Rgb<u8>) -> Rgb<u8> {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => ANSI_256[i as usize],
        vt100::Color::Rgb(r, g, b) => Rgb([r, g, b]),
    }
}
```

完整模块还要写:`capture_pane_with_ansi`(`tmux capture-pane -e -p ...`)+
`query_pane_dims`(`tmux display-message -p '#{pane_height} #{pane_width}'`)+
`ANSI_256` 调色板常量(16 标准 ANSI + 216 RGB cube + 24 灰阶 = 256 项)。

graceful degrade 红线:**screenshot 永不阻塞 enriched escalate / outbox 写入**。
任何环节失败 → 不写 `screenshot_path`,outbox 主路径仍写,F35 文本 `pane_tail`
保留(ASCII 维度的兜底已经在)。

#### 7.2.4 outbox payload 集成 / MCP 工具

`needs_attention.outbox.json` 在 F35 字段基础上加 1 个:

```json
{
  ...(F35 字段)
  "ccteam_screenshot_path": "/abs/path/to/screenshots/2026-05-09T12-30-15Z.png"
}
```

字段缺失 → 当次未截图(脚本不可用 / 渲染失败)。meta-agent NL 翻译时
有路径就附"(屏幕截图:file:///<path>)",没有就跳过。

新 MCP 工具 `mcp__ccteam__screenshot`:

```
input: {
  slug: string,             // 项目 slug
  lines?: number,           // 默认 50;capture-pane -S -<lines>
}
output: {
  ok: boolean,
  path?: string,            // 写入的 PNG 绝对路径(成功时)
  reason?: string,          // 失败原因(脚本缺 / 字体缺 / 渲染异常)
}
```

落 `crates/ccteam-cli/src/mcp_serve.rs`,跟现有 9 工具(ls / show / new ...)
并列;`interfaces.md` §12 同步加。

#### 7.2.5 graceful degrade 表

| 失败场景 | 行为 |
|---|---|
| `tmux capture-pane -e` 失败(session 不存在 / tmux 未装) | log warn,返回 `Ok(None)`;outbox 不写 `screenshot_path`;MCP 工具返回 `{ok:false, reason:"tmux capture failed: ..."}` |
| `tmux display-message` 查 pane 尺寸失败 | 同上,reason "tmux pane query failed" |
| `vt100::Parser::process` panic(理论极小,vt100 mature) | `std::panic::catch_unwind` 兜,log warn,返回 `Ok(None)` |
| 字体 probe 全失败(env 未配 + 预设 path 全没装) | log warn,返回 `Ok(None)`;reason "no monospace ttf found; set CCTEAM_SCREENSHOT_FONT_TTF or install jetbrains-mono / dejavu" |
| `FontRef::try_from_slice` 解析 ttf 失败(文件损坏) | 同上,reason "ttf parse failed: ..." |
| `imageproc::draw_text_mut` panic(罕见) | `catch_unwind` 兜,log warn |
| 字体不覆盖输入字符(CJK / emoji 渲成 ▢) | imageproc 不感知,PNG 仍生成但视觉降级;用户层兜底 = `CCTEAM_SCREENSHOT_FONT_TTF` 切换到覆盖字体(eg `NotoSansMonoCJKsc-Regular.otf`);V0.3 评估 detect-script 自动切 |
| 写 PNG 失败(磁盘满 / 路径权限) | log warn,返回 `Ok(None)` |

不再有"缺 helper / 缺 Python / 缺 Pillow / 缺 freeze binary / 缺 system fontconfig"
这一类失败 — vt100 / imageproc / ab_glyph / image 都是 Cargo 编译期纯 Rust 依赖,
跟 ccteam binary 同生共死。`ccteam doctor --screenshot-smoke <slug>` 跑一次端到端
渲染,verify PNG 可生成 + print 命中的 ttf path / 失败 reason(字体 / tmux / IO
哪一层)。

### 7.3 不做

- **`freeze` shell-out**:Linux 5.15 segfault 实测不可用,直接废
- **Pillow / Python 兜底层**:用户原始诉求"不引入 Python 依赖",兜底层会把
  Python + Pillow + 字体三件套又拉回来,违原话;V0.2.2 选 ansee 单路径 +
  graceful degrade 到无 PNG(F35 文本 `pane_tail` 是 ASCII 维度的天然兜底,
  已在 outbox 写)。若 ansee 真撞 CJK / emoji ▢ 等问题,V0.3 评估"切更厚 Rust
  栈"(`alacritty_terminal` + `cosmic-text` + `tiny-skia`,§10 deferred)而不是
  Python 兜底
- **PNG 直接 base64 入 outbox JSON**:大文件冗余;路径引用清晰
- **meta-agent 自动多模态读 PNG**(Read 工具拉到 LLM context):cost / context
  budget 重,V0.2.2 不做。channel adapter / 用户人眼是主消费者
- **截图历史保留策略**(`<project>/.ccteam/screenshots/` 自动清理):V0.3
  跟 channel layer 一起评估
- **detect-script 自动切字体**(扫 ANSI 文本判断 CJK / emoji 比例 → 选 Noto
  Sans Mono CJK SC 或 Symbola):V0.3 if 真痛
- **vendor 字体进 ccteam release**:用户系统字体生态(font-kit / fontconfig)
  自给自足;ccteam 不内置 ttf 文件膨胀 binary

---

## 8. F39 — `cct` 短前缀约定 sweep

### 8.1 问题

V0.1/V0.2 ship 时命名前缀都是 `ccteam-`(`ccteam` binary、`ccteam-control` skill、
`ccteam-team-author` skill)。两个痛点:

1. **冗长**:`~/.claude/skills/` listing 三个 `ccteam-*` 看一遍要扫长字符串;命令行
   `ccteam new ...` 比 `cct new ...` 多打 4 字符 × 数百次/月
2. **无统一约定文档化**:CLAUDE.md / README / 文档里散布"ccteam command",但是
   "ccteam 是项目名,执行时缩写为啥"这条没明文,新用户看到 `ccteam new` 就照搬,
   工程内部没有统一缩写约定

V0.2.2 顺手做一波约定 sweep,跟 F34 加新 skill `cct-project-creator` 同步落地。

### 8.2 设计

#### 8.2.1 三个对象同步重命名

| 对象 | 老名 | 新名 | 影响面 |
|---|---|---|---|
| **可执行文件** | `ccteam` | `cct` | `crates/ccteam-cli/Cargo.toml::[[bin]]` + Rust 内 `current_ccteam_bin()` 等改名 + 所有 shell-out callsite + settings.json 模板 hook 命令 |
| **skill: control** | `ccteam-control`(M1.8 ship) | `cct-control` | skill template + Rust 函数名 + meta_agent_role.md 引用 + V0.1/V0.2 用户安装目录迁移 |
| **skill: team-author** | `ccteam-team-author`(M0.22 ship) | `cct-team-author` | 同上 |

`cct-project-creator`(F34 新增)直接用新约定 ship,无 rename。

#### 8.2.2 顶层 `skills/` 目录(配套)

skill markdown 从 `crates/ccteam-core/src/templates/` 迁到 **repo 根目录 `skills/`**
(每个 skill 一个子目录 + `SKILL.md`):

```
skills/                                              # ← 新顶层目录(F39 PR 建)
├── cct-control/SKILL.md                             # ← 从 templates/ccteam_control_skill.md 迁 + 改名
├── cct-team-author/SKILL.md                         # ← 从 templates/ccteam_team_author_skill.md 迁 + 改名
└── cct-project-creator/SKILL.md                     # ← F34 PR 加(F39 merge 后 follow-up)
```

`crates/ccteam-core/src/skill.rs::CCTEAM_*_SKILL_MD` 常量改用
`include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/cct-<name>/SKILL.md"))`
跨目录引用;Rust 函数同步重命名:

| 老 | 新 |
|---|---|
| `install_ccteam_control_skill` | `install_cct_control_skill` |
| `install_ccteam_team_author_skill` | `install_cct_team_author_skill` |

#### 8.2.3 二进制 rename — Cargo.toml + shell-out callsite

```toml
# crates/ccteam-cli/Cargo.toml
[[bin]]
name = "cct"            # ← 从 "ccteam" 改
path = "src/main.rs"
```

`current_ccteam_bin()` 函数靠 `std::env::current_exe()`,binary 改名后路径仍正确,
**函数本身可以继续叫 `current_ccteam_bin()` 不必改**(内部 API,不影响行为)。
但为可读性建议同步改 `current_cct_bin()`。

代码内 shell-out callsite 字面 `"ccteam"` 改 `"cct"` 的位置通过 grep 定位
(预估 10-20 处,主要在 hook command 拼装 / settings.json 模板生成 / e2e tests):

```bash
git grep -nE '"ccteam"' crates/  # rust source 字面字符串
git grep -nE '`ccteam ' docs/    # docs CLI 例子
```

#### 8.2.4 settings.json hook 命令模板

`crates/ccteam-core/src/templates/settings.json` 里所有 hook command 串当前形如:

```json
"command": "/path/to/ccteam hook progress-append"
```

改成 `cct`:

```json
"command": "/path/to/cct hook progress-append"
```

**绝对路径仍由 doctor 在 install 时填入**(`current_exe()` 路径),所以模板用占位符
`{{CCT_BIN}}` 即可。doctor 安装 hook 时把占位符替换为 `current_exe()` 的真实路径
(已经支持,只是 binary 名字变 cct 后路径自然变)。

#### 8.2.5 V0.1/V0.2 用户升级迁移

**升级路径**:用户 `cargo install --path crates/ccteam-cli --force` 重装后,
`~/.cargo/bin/ccteam` 旧文件还在(cargo 不会自动删 rename 后的 binary),
`~/.cargo/bin/cct` 是新装的。两个都能跑(老 ccteam 是 V0.2.1 ship,新 cct 是
V0.2.2);老的会过期,但不会崩。

`cct doctor` 自动跑以下迁移:

| 检测 | 行为 |
|---|---|
| `~/.cargo/bin/ccteam`(或同 PATH 位置)存在 | warn "old `ccteam` binary detected; safe to `rm` (V0.2.2 起命令名 = `cct`)";不主动删 |
| 任何 `~/projects/<slug>/.claude/settings.json` 里 hook command 包含 `/ccteam ` | rewrite 为 `/cct `,使用 `current_exe()` 真实路径;atomic 写 |
| `~/.claude/skills/ccteam-control/` 存在 | 检测 marker(`<!-- ccteam-managed -->` 或 frontmatter `name: ccteam-control`),匹配 → `rm -rf`;不匹配(用户手改)→ 保留 + warn |
| `~/.claude/skills/ccteam-team-author/` 存在 | 同上 |
| `~/.claude/CLAUDE.md`(全局)含 "ccteam-control" 引用 | 不动(用户管理) — F39 在 ccteam 自己产出物里改,用户全局 CLAUDE.md 由用户决策 |

不加新 doctor flag,迁移逻辑放 `cct doctor`(不带 args 默认 run)+ `cct doctor
--install-skill` / `--install-meta-agent` 触发时同步跑。跟 V0.2.0 的
`--migrate-recommended-agents` 同模式("安装时清旧 + 新装")。

#### 8.2.6 文档 sweep

| 文件 | 处理 |
|---|---|
| `CLAUDE.md`(主仓) | F39 已加 §三 红线一条 "cct 短前缀约定" + §四 Skills 行更新 |
| `README.md` | sweep `ccteam <cmd>` → `cct <cmd>`;install 命令 + 用法示例 |
| `docs/tech-design.md` | sweep `ccteam ` → `cct ` (forward-looking 部分;§7 里程碑路线图 V0.2 ship 段引用 ccteam 历史名留档不动)|
| `docs/interfaces.md` | sweep CLI 例子;§10 命令清单同步 |
| `docs/requirements.md` | sweep |
| `docs/v0-2-2/*.md` | 本 PRD + dev-plan + e2e-retro 全用 `cct` 写(已是新约定起点)|
| `docs/v0-1/*` | **不动**(历史归档,反映 V0.1 ship 实情)|
| `docs/v0-2/*` | **不动**(历史归档,反映 V0.2 ship 实情)|

### 8.3 不做

- **保留 `ccteam` 命令 alias / 符号链接**:不做向后兼容 alias,不写 `ln -s cct ccteam`。
  理由:增加 sustaining 负担;真要兼容用户老脚本,他们手 `alias ccteam=cct` 在 bashrc 即可;
  doctor 的 settings.json 迁移已自动覆盖项目内 hook 路径
- **MCP 工具改名**:`mcp__ccteam__*` 工具命名空间是 Claude Code 配置文件里的 server name,
  改名要碰 `~/.claude.json::mcpServers` 用户文件,风险大、收益低;V0.3 评估
  (V0.2.2 PRD §7 F38 里写 `mcp__cct__screenshot` 是预期的,但若 MCP 命名空间
  保留 `ccteam`,实际工具名是 `mcp__ccteam__screenshot`;以 PR 实施时定)
- **重命名 git repo / cargo workspace 名 / crate 名**:`crates/ccteam-cli` /
  `crates/ccteam-core` / `crates/ccteam-hooks` 是 Rust crate path,改了破依赖图 + git history,
  V0.2.2 不动
- **历史文档命令 sweep**:V0.1 / V0.2 子目录归档不改,反映当时 ship 实情

---

## 9. F40 — team 名缩短 + alias 软迁移

### 9.1 问题

`product-research` 是 V0.2 M3.4 起的 team 名,实际用起来三个 friction:

1. **冗长**:命令行 `cct new --team product-research "<brief>"`、`~/projects/product-research-<slug>/`
   目录,跟简短的 `dev` 对比读 / 写都繁
2. **领域名 vs team 名混淆**:`product-research` 既是技术 team 名(state.json / slug
   前缀 / rules 文件名),又是领域描述,两者绑死;短 team 名 + 单独 description
   字段更清晰
3. **V0.2 §5.4 alias 方案当时 deferred**:V0.2 PRD §5.4 已 spec 出 `team.yaml::aliases`
   实施路径但 push 到 V0.3,本次 V0.2.2 拉回(代价 < 评估时低)

### 9.2 设计 — 软 rename via alias

**核心**:不动用户数据,新项目用新名,老项目继续工作。

#### 9.2.1 仓内 team 重命名

```
teams/product-research/  →  teams/research/        # git mv 保 history
└── team.yaml::
    name: research                                  # ← 改
    aliases: [product-research]                    # ← 新字段;老名仍可解析
    description: |                                  # ← description 字段载全称
      Product research team —
      kickoff → research → verdict → next-steps;
      用于"判断 idea 值不值得做"场景。
    # 其他字段不变
└── phases/                                         # 路径同
```

`teams/product-research/` 改 `teams/research/` 走 `git mv`(保 history)。phase markdown
内容不动(它们不引用自己 team 名)。

#### 9.2.2 `team.yaml::aliases` 字段语义

```rust
// crates/ccteam-core/src/team.rs
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeamSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,        // ← V0.2.2 新字段
    pub description: String,         // 已存在
    // ...
}
```

`team_resolver::resolve_team(name)`:

```rust
fn resolve_team(query: &str) -> Result<TeamSpec> {
    for source in TEAM_SOURCES {
        for entry in source.scan() {
            let spec = entry.parse_team_yaml()?;
            if spec.name == query || spec.aliases.iter().any(|a| a == query) {
                return Ok(spec);
            }
        }
    }
    Err(anyhow!("team {query} not found"))
}
```

老项目 state.json `team: "product-research"` → resolver 匹配 alias → 加载 `teams/research/team.yaml`(canonical name = "research")→ phase pipeline 正常跑。

#### 9.2.3 文件系统隔离 — 不迁老项目目录

| 数据 | 老项目(state.json team=product-research) | 新项目(state.json team=research) |
|---|---|---|
| 项目目录 | `~/projects/product-research-<slug>/`(不动)| `~/projects/research-<slug>/` |
| `state.json::team` | `"product-research"`(不动)| `"research"` |
| `~/.claude/rules/ccteam-lessons-*.md` | `ccteam-lessons-product-research.md`(不动)| `ccteam-lessons-research.md`(新生成)|
| auto-memory bridge 写哪个 | rules 文件名按 `state.json::team` 字段 | 同 |
| team 加载 | resolver alias 匹配 → `teams/research/team.yaml` | 同 |

**老项目的 rules 文件 `ccteam-lessons-product-research.md`**:F40 PR 同时
**doctor 写一份新的 `ccteam-lessons-research.md`**(从 `teams/research/team.yaml::retro_schema`
渲染),老的留着不删(已积累的跨项目记忆有价值;`paths:` frontmatter 仍
匹配老项目目录 `~/projects/product-research-*`,继续生效);新项目 paths:
匹配 `~/projects/research-*`。两份文件并存,各自服务对应代际项目。

#### 9.2.4 cct-project-creator skill / `cct new` UI

`cct new --team` 接受 `research` 或 `product-research`;后者 stderr warn
"deprecated alias, use 'research'"(不 fail-loud,过渡期友好)。

`cct-project-creator` skill body(F34 设计的 Phase C team 选择)用 canonical
名 `research`;描述里展示 `team.yaml::description` 全文。AskUserQuestion option:

```
{ label: "research", description: "判断 idea 值不值得做(kickoff → research → verdict → next-steps)" }
```

#### 9.2.5 测试 / 文档

- `crates/ccteam-cli/tests/m3_product_research_e2e_test.rs` 改用 `research` 名
  + 增 1 个 alias resolution 测试(`team: product-research` 也通)
- `dev-coupling-audit.md` F-finding 列表加 F40 close 标
- `interfaces.md` §5.5 team.yaml schema 加 `aliases` 字段
- `tech-design.md` §3 团队抽象表更新("领域命名"目标的描述,从 V0.3 deferred 改为 V0.2.2 ship)
- `docs/v0-2-2/feedback.md` 不动(本 finding 是用户主动追加,无原始反馈条目)

### 9.3 不做

- **硬 rename(touches 用户数据)**:不重命名 `~/projects/product-research-*/` 目录,
  不改老项目 `state.json::team` 字段,不改 / 删老 rules 文件。原因:用户数据触碰
  风险高,alias 软迁移 100% 行为兼容
- **dev → software-development / engineering 等领域命名**:`dev` 已经短,不缩
  (V0.2 PRD §5.4 当时也只 deferred"扩展为领域名",没说改 dev)。本次 F40 只改
  `product-research`
- **MCP 工具命名 namespace 改名**:同 F39 §8.3,V0.2.2 不动
- **多 team alias mapping**(eg `product-research` / `pr` / `research` 三别名):aliases
  数组保留,但语义 = "老名 → 新名" 单向兼容;不开放为通用 nicknames(避免歧义)

---

## 10. PR sequencing 与 worktree 分配

5 finding 落 5 个 PR,推荐顺序与 worktree 分支:

| PR # | finding | worktree 分支 | touchpoints | 依赖 |
|---|---|---|---|---|
| 1 | **F39** | `v0-2-2-cct-rename` | **binary**:`crates/ccteam-cli/Cargo.toml::[[bin]]name = "cct"` / Rust 函数 `current_ccteam_bin` → `current_cct_bin`(以及内部 callsite sweep)/ settings.json template hook command 用 `{{CCT_BIN}}` 占位 / **skill rename + 顶层目录**:新建 `skills/`,迁 `ccteam-control` + `ccteam-team-author` 两 skill 进 + 改名 `cct-control` / `cct-team-author`,删 `crates/ccteam-core/src/templates/ccteam_*_skill.md`,`skill.rs` 常量 + 函数同步 / **migration**:`cct doctor` 自动 detect + clean `~/.claude/skills/ccteam-{control,team-author}/`(marker 校验) + rewrite 老 settings.json hook command 路径 / **docs sweep**:README.md / tech-design.md / interfaces.md / requirements.md 全 `ccteam <cmd>` → `cct <cmd>`(historical v0-1 / v0-2 不动)/ **CLAUDE.md** §三 §四 已加(本 PR 把 PRD 驱动的 §13 "patch 流程"段也加上)| 无 |
| 2 | **F34** + **F37** | `v0-2-2-meta-agent-and-slug` | **新 skill**:`skills/cct-project-creator/SKILL.md`(F39 已建 `skills/` 目录 + cct 命名)/ **Rust 改动**:`skill.rs::install_cct_project_creator_skill` 新增 / **CLI**:`commands.rs::run_new` 新 `--slug` / `--no-auto-slug` / `--auto-slug-model` flag + Tier 3 `claude -p` 调用 + Tier 4 `slugify_brief` 兜底 + `main.rs`(Commands::New 新字段) / `projects.rs::slugify_brief` 新函数 + `pick_unused_slug` 内部切换 / doctor `--install-skill` extend 装 3 skills / **F37 决策树**:`meta_agent_role.md` §1 自检反例 + §2 派单段改指 cct-project-creator skill + §3 克制规则加"不起 Agent 自调研"反例 | **依赖 PR #1 merge**(F39 把目录 + 命名先 ready) |
| 3 | **F35** | `v0-2-2-silence-classifier` | `orchestrator.rs` / `progress.rs` / `tmux.rs`(提 capture_pane_tail)/ `parse_phase_end.rs`(refactor 引用)/ 新模块或 stall.rs 升级 | 无(F39 命名 sweep 不撞 F35 surface) |
| 4 | **F36** | `v0-2-2-subagent-guard` | `orchestrator.rs::dispatch_phase_with_state` / `progress.rs::subagent_active` / 新 `<project>/.ccteam/pending-inject.json` 协议 | 软依赖 PR #3(复用 enriched outbox classification 字段) |
| 5 | **F38** | `v0-2-2-screenshot` | 新 `crates/ccteam-core/src/screenshot.rs`(vt100 + imageproc DIY in-process)+ vendored `assets/fonts/JetBrainsMono-Regular.ttf` / `Cargo.toml` 加 `vt100 = "0.15"` + `image = "0.25"` + `imageproc = "0.25"` + `ab_glyph = "0.2"` / `mcp_serve.rs`(加 screenshot 工具,工具名取决于 mcp namespace 决议)/ `commands.rs`(`cct doctor --screenshot-smoke <slug>`)/ `tmux.rs`(共享 `capture_pane_with_ansi(-e)` helper)/ `interfaces.md` | 软依赖 PR #3(F35 outbox 加 `screenshot_path` 字段) |
| 6 | **chore**:cargo workspace.version + CLAUDE.md §五 dev-flow 段 + docs/README.md patch 规约 + this PRD/dev-plan + e2e retro 后续 | `v0-2-2-chore` | `Cargo.toml` / `CLAUDE.md` / `docs/README.md` / `docs/v0-2-2/*.md` | 最后,V0.2.2 ship gate |

PR sequencing 关键约束:**PR #1(F39)必须先 merge**,把 binary + skill 命名 + 顶层
`skills/` 目录立起来;PR #2(F34+F37)依赖 PR #1 在新 directory + 新命名上做 follow-up。
PR #3 / #4 / #5 跟 #1 解耦,可平行起 worktree;merge 顺序仍按 PR # 推。冲突点:

- **PR #1 vs #2 — `skill.rs`**:#1 改三个老 install fn 名(team-author + control)
  + 加 `cct-` 路径常量,#2 加 `install_cct_project_creator_skill`;按 merge 顺序 rebase
- **`meta_agent_role.md`**:F34 + F37 同 PR(#2)解决
- **`orchestrator.rs`**:F35 (#3) 加新 classifier 调用,F36 (#4) 改 dispatch_phase 注入路径;两段相邻但不冲突;先 merge PR #3,PR #4 rebase
- **`progress.rs`**:F35 加 enriched outbox 写,F36 加 `subagent_active`;两段独立函数;按 merge 顺序 rebase
- **`mcp_serve.rs`** + **`commands.rs`**:F38 (#5) 加工具 / doctor flag;跟其他 PR 不撞
- **enriched outbox schema**:F35 (#3) 定字段,F38 (#5) 加 `screenshot_path` 字段;PR #3 先 merge,PR #5 rebase 后加

每个 PR 描述映射:

- `requirements.md` 痛点(F34 痛点 1 命名;F35 + F36 痛点 4 控制平面;F37 痛点 6 meta-agent 边界;F39 命名约定 sweep)
- `tech-design.md` 章节(F35 §6.9 idle injection / §3.5 fix-loop;F36 §6.9;F37 §3.8 用户接口层 / §6.8 watchdog)
- `dev-coupling-audit.md` F-finding 编号(F34/F35/F36/F37/F38/F39 加进去)
- `interfaces.md` 同步(F34 加 `--slug` flag schema;F35 加 enriched outbox 字段;F36 加 pending-inject.json schema;F38 加 MCP screenshot 工具 schema;F39 全 CLI 例子 sweep)

---

## 10. 验收

### F34
- [ ] `cct new --slug <name> --team <team>` 创建项目用 `<team>-<name>` 或 verbatim
- [ ] 撞名 retry 仍工作(`--slug ccteam-ui` 撞 → `dev-ccteam-ui-{4hex}`)
- [ ] 非法 slug(含大写 / 特殊字符)fail-loud
- [ ] **新 skill**:`skills/cct-project-creator/SKILL.md` ship +
  `install_cct_project_creator_skill()` 落 `~/.claude/skills/`;
  `cct doctor --install-skill` 一并安装(跟 cct-control / cct-team-author 并列)
- [ ] skill body 用 `AskUserQuestion`(meta-agent context 允许;V0.2 §2.4 PreToolUse
  拦截只 scope 到 project session)做 slug / team / 关键澄清的结构化选择
- [ ] **Tier 3 智能 fallback**:`ccteam new` 无 `--slug` + 在 tty 时,shell out
  `claude -p --model claude-haiku-4-5-20251001` 拿推荐 + Y/n 确认;15s 超时硬截
  + 失败降级 Tier 4
- [ ] **Tier 3 flag**:`--no-auto-slug` / `--auto-slug-model <model>` / env `CCTEAM_AUTO_SLUG=off` 全可控
- [ ] **Tier 4 deterministic**:新函数 `slugify_brief()`(`crates/ccteam-core/src/projects.rs`),
  token-aware + stop-word filter + dedupe + 取前 3 token;现 `slugify()` 不动
  (meta-agent path 仍用)
- [ ] `pick_unused_slug` 内部从 `slugify(base)` 切到 `slugify_brief(base)`
- [ ] `meta_agent_role.md` §2 派单段改为指向 `ccteam-project-creator` skill,不再
  inline 派单细则

### F35
- [ ] `silence_classifier`(或同等模块)4 类分类 unit 测试覆盖
- [ ] `MidToolHung` / `SubagentRunaway` 触发 enriched outbox 写入,字段完整
  (classification / silent_seconds / last_event / pane_tail)
- [ ] `PostStopLimbo` / `InjectLimbo` 触发 deterministic re-inject 1 次,再触发
  → enriched escalate(测试)
- [ ] capture-pane helper 提到 ccteam-core,`parse_phase_end.rs` 改引用,Stop hook
  L3 行为不变(回归测试)
- [ ] meta-agent role prompt 加 enriched outbox NL 翻译模板

### F36
- [ ] `subagent_active(events)` unit 测试(开 / 关 / 嵌套)
- [ ] `dispatch_phase_with_state` 检测 active subagent → pending-inject.json 落盘
  + 不发 send-keys
- [ ] daemon tick 在 SubagentStop event 后真发 pending-inject + 删文件
- [ ] max-defer-minutes 兜底:超时 → enriched escalate,不无限 defer

### F37
- [ ] `meta_agent_role.md` §1 决策树加"调研 X" 反例;§3 克制规则加 "不起
  Agent subagent 自调研" 反例
- [ ] `ccteam doctor --install-meta-agent <handle>` 重写 CLAUDE.md 包含新 §

### F39
- [ ] `crates/ccteam-cli/Cargo.toml::[[bin]] name` 改 `cct`;`cargo build --release` 产物名变成 `cct`;`cargo install` 后 `~/.cargo/bin/cct` 在 PATH 可调
- [ ] 顶层 `skills/` 目录建立,`cct-control` / `cct-team-author` 从 `crates/ccteam-core/src/templates/` 迁入并改名;`crates/ccteam-core/src/templates/ccteam_*_skill.md` 删除
- [ ] `crates/ccteam-core/src/skill.rs` 三个常量 + 三个 install 函数 rename(`CCT_*_SKILL_MD` / `install_cct_*_skill`);跨目录 `include_str!` 用 `concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/cct-<name>/SKILL.md")`
- [ ] `crates/ccteam-core/src/templates/settings.json` hook command 占位符 `{{CCT_BIN}}`;doctor install 时填 `current_exe()` 真路径
- [ ] `cct doctor` 自动迁移 V0.1/V0.2 用户:detect + clean `~/.claude/skills/ccteam-{control,team-author}/`(marker 校验) + rewrite `~/projects/<slug>/.claude/settings.json` hook command 老路径(原子写)
- [ ] `meta_agent_role.md` 全文 `ccteam-control` 引用替换为 `cct-control`(F37 PR 一并改)
- [ ] **docs sweep**:README.md / docs/tech-design.md / docs/interfaces.md / docs/requirements.md `ccteam <cmd>` → `cct <cmd>`(forward-looking;`docs/v0-1/` `docs/v0-2/` 不动)
- [ ] CLAUDE.md §三 红线 + §四 Skills 行已加 cct 约定(F39 PR 同步加 §五 dev-flow 段 — 见 §13)
- [ ] cargo test --workspace 全绿;binary 改名后 e2e tests 调 `cct` 通过

### F38
- [ ] `Cargo.toml` 加 deps:`vt100 = "0.15"` + `image = "0.25"` + `imageproc = "0.25"` + `ab_glyph = "0.2"`
- [ ] `crates/ccteam-core/assets/fonts/JetBrainsMono-Regular.ttf` vendor(OFL,~150 KB);
  license 注脚加在 ccteam README + `LICENSES.md`(若无则建)
- [ ] `crates/ccteam-core/src/screenshot.rs` 新模块,`render_screenshot(slug, lines)` API 上线
- [ ] `ANSI_256: [Rgb<u8>; 256]` 调色板常量(16 + 216 cube + 24 grayscale,标准映射)
- [ ] `mcp__<ns>__screenshot(slug, lines?)` MCP 工具 — 项目存在 → in-process
  vt100 渲染,写 PNG 到 `<project>/.ccteam/screenshots/<utc>.png` 返路径(MCP namespace `ccteam` vs `cct` 取决于 F39 §8.3 的 MCP 命名空间决议;V0.2.2 默认 namespace 保留 `ccteam` 不动)
- [ ] `CCTEAM_SCREENSHOT_FONT_TTF` env 覆盖字体(指 ttf path,eg `/usr/share/fonts/opentype/noto/NotoSansMonoCJKsc-Regular.otf`);
  缺则用 vendored JetBrains Mono
- [ ] graceful degrade 全覆盖:tmux 失败 / vt100 panic / ttf 解析失败 / imageproc panic /
  IO 失败 — 全部返 `Ok(None)` + `{ok:false, reason:...}`,主路径不挂
  (`std::panic::catch_unwind` 兜潜在 panic)
- [ ] F35 enriched outbox 在 PR #2 merge 后 rebase 加 `screenshot_path` 字段
  (best-effort 写入,失败 silent)
- [ ] `ccteam doctor --screenshot-smoke <slug>` 新 flag,跑一次端到端渲染 verify
  字体 / tmux / IO 全链路;失败 print 具体 reason
- [ ] `interfaces.md` §12 加新 MCP 工具 schema
- [ ] 颜色映射黄金值测试:输入预设 ANSI escape,断言 cell 颜色映射符合 ANSI_256 表

### 配套
- [ ] `Cargo.toml` `workspace.package.version` `"0.2.2"`
- [ ] `CLAUDE.md` §五 PR 纪律加 patch 流程小节(≤ 8 行)
- [ ] `docs/README.md` 加 patch 版本目录约定
- [ ] `dev-coupling-audit.md` F34-F38 加条目;V0.2.2 patch 段标记 close
- [ ] `cargo test --workspace` 不退步(baseline 511 → ≥ 511 + 新增 F34/F35/F36/F38/F39 测试)
- [ ] clippy 不新增 warning

---

## 11. 不在范围 / V0.3 deferred

- **slug rename**:state.json / `~/.claude/rules/` paths regex / tmux session 全条线迁移
- **MCP 工具 `mcp__ccteam__interrupt(slug)`**:V0.3 跟 channel layer 一起评估 ergonomics
- **autonomous meta-agent 决定 re-inject / Ctrl+C**:破红线;V0.3 评估"用户
  opt-in 信任配置"是否值得开口子
- **session liveness 模型重构**:F35 + F36 修的是症状,根因是"控制平面对
  session 状态的盲区";V0.3 跟 Web UI / channel layer 一起谈 SoT
- **强制 `--slug`**:破向后兼容
- **新 phase YAML 字段** `idempotent_inject` 之类:不必,re-inject 已 deterministic safe
- **F38 channel adapter 推送**(把 PNG 自动转发到 Telegram / Web UI)— V0.3 跟
  channel layer 一起;V0.2.2 只做生成 + outbox 引用
- **F38 多模态 meta-agent**(LLM 自己读 PNG):context cost 重,V0.2.2 不开
- **F38 截图历史清理策略**:V0.3 channel layer 评估
- **F38 切更厚 Rust 渲染栈**(`alacritty_terminal` + `cosmic-text` + `tiny-skia`,
  完整 VT 谱 + 多字体 compose + emoji COLR/CPAL):若 ansee 真撞早期 crate
  panic / CJK 渲染问题且 V0.3 升级 ansee 也救不了再切;~300-500 LoC + 3 deps
- **F38 Pillow / Python 兜底层**:V0.2.2 明确不做(违"不引入 Python");V0.3
  评估时也优先切 Rust 栈而非 Python
- **F38 detect-script 自动选字体**:扫 capture-pane 内容判断 CJK / emoji 比例
  动态切 `Font::name`;V0.3 if ansee 单字体方案真出现用户痛点

---

## 12. Workspace version bump

`Cargo.toml::workspace.package.version` `"0.0.1"` → `"0.2.2"`。retroactive 修正
(V0.1 / V0.2 ship 时未 sync,V0.2.1 PR 也未 sync)。新政策:每个 minor / patch
release **必须 bump** + 在 commit message subject 一致(`v0.2.2: ...`)。

未来若需要 crates 单独版本管控,改 `version.workspace = true` 为各自字面量;
本 patch 不涉及。

---

## 13. CLAUDE.md dev-flow 追加段(草案)

落到 `CLAUDE.md` §五 PR / 实现纪律 末尾,新增小节:

```markdown
### Patch 版本(V0.x.y)开发流程

1. **doc-first**:PRD + dev-plan 落 `docs/v0-x-y/`;用户 review 后才动代码
2. **worktree-per-PR**:每个 finding 单独 `git worktree add /tmp/ccteam-<branch> origin/main`
3. **subagent 派工**:主 session 用 Agent 工具派每个 worktree(briefing 含
   PRD section + 验收条目)
4. **PR review/fix/merge**:主 session 拉 PR diff review → 退回 fix 或本地补 → merge
5. **cargo bump**:`workspace.package.version` 同步 bump,commit subject 用 `vX.Y.Z:` 前缀
6. **CLAUDE.md baseline 更新**:`cargo test --workspace` 通过新数后回填 §一表格
```

(主仓 main 不变 dirty;worktree 工具流详 §五"多 session 并行编辑同一仓库"段)

---

## Changelog

- 2026-05-09:**F39 抽出独立 finding** — 用户连追三条:`ccteam-control` skill
  前缀改 `cct-control` → 二进制 `ccteam` → `cct` → CLAUDE.md 加 `cct` 约定。三条
  同源 = "cct 短前缀约定 sweep",归并为 F39。F39 退出 F34 scope,改成独立 PR #1
  机械 rename 跨 binary + 全 3 skill + docs sweep + V0.1/V0.2 用户迁移逻辑。
  F34 PR 退回到专注 slug 逻辑 + 新 `cct-project-creator` skill content,基于 F39
  已建好的 `skills/` 目录 + 命名 follow-up。CLAUDE.md §三 红线 + §四 Skills 行
  本次直接改(2026-05-09 写入,反映 V0.2.2 计划)
- 2026-05-09:F34 经用户多轮迭代深化:初稿仅 `--slug` flag → 加 `slugify_brief()`
  token-aware 算法 → 用户要求"一定要用上智能" → 加 Tier 3 `claude -p` 智能 fallback +
  Y/n 确认 + 四层调用栈 → 用户要求"封装成 cct-project-creator skill" → 重写
  Tier 2 为 dedicated skill(meta-agent 调用,AskUserQuestion 结构化选项)→ 用户
  要求"所有自带 skill 移到 repo 根目录 `skills/`" → 抽到 F39 整套约定 sweep。
  F34 最终 scope 是 slug 四层调用 + 新 skill body 内容,~200 LoC + skill 模板
- 2026-05-09:初稿。基于 2026-05-08 用户首批 ccteam 实战反馈(F34-F37);F35
  设计经 advisor + 用户两轮迭代敲定为"事件感知分级 + capture-pane 入 outbox +
  meta-agent propose-confirm"(meta-agent autonomous decide 选项被红线驳回)。
  base = origin/main `170f5a8`
- 2026-05-09:用户追加 F38 截图(PNG)需求 — UX 增强,补 F35 enriched outbox 视觉
  维度,也独立 MCP 工具暴露。PR sequencing 增加 PR #4(screenshot)+ PR #5
  (chore ship gate)
- 2026-05-09:F38 实施载体经用户 / WebFetch 反复验证迭代(5 轮):
  - 第 1 轮:bundled Python + Pillow + 三级字体 fallback → 用户驳回 Python 依赖
  - 第 2 轮:`vte + tiny-skia / resvg` Rust 自拼 ~300-500 LoC → 复杂度过高,patch 容不下
  - 第 3 轮:shell out **`freeze`**(charmbracelet Go binary)→ 用户实测 Linux 5.15 segfault 不可用
  - 第 4 轮:**`ansee` crate**(纯 Rust,ANSI→PNG)→ 验证发现拉 font-kit + Linux
    系统 C 依赖(`libfontconfig` + `libfreetype`),且 `ansi-parser 0.9` 早期
  - 第 5 轮(最终):用户提议 **`vt100 + imageproc + image + ab_glyph` DIY**;
    验证 vt100 v0.16(pin 0.15 跟用户 reference)是 mature 终端状态机,imageproc
    有 `draw_filled_rect_mut` + `draw_text_mut`,**绕过 font-kit / 系统 C 依赖**
    (vendored JetBrains Mono TTF + `include_bytes!` 编译期打包);ccteam-core
    +~250 LoC,license MIT 干净
  - 最终:**vt100 + imageproc DIY in-process 单路径**,`std::panic::catch_unwind`
    兜潜在 panic,`CCTEAM_SCREENSHOT_FONT_TTF` env 覆盖字体(用户切 CJK / emoji
    覆盖字体时用),vendored JetBrains Mono(OFL)默认
