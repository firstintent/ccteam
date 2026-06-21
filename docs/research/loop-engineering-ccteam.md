# Loop Engineering × ccteam —— 运维控制面定位

> **类型**:战略/定位 thesis(非版本 PRD,非竞品报告)。
> **日期**:2026-06-21 · **状态**:讨论稿,代码未动。
> **源**:Addy Osmani《Loop Engineering》+ chainup `agent-research/loop-dev-paradigm`(Oracle-First)+ 新智元《Claude Code 之父删了 IDE!干掉提示词,只写循环》。归档:`/home/ubuntu/chainup/agent-research/{loop-engineering-20260621,loop-dev-paradigm-20260621,loop-engineering-xinzhiyuan202606}.md`。
> **原型**:[`loop-onramp.html`](../versions/v0-8-18/prototype/loop-onramp.html)(入门启动:loop 库→云端起跑)+ [`loop-ops-console.html`](../versions/v0-8-18/prototype/loop-ops-console.html)(控制面板:门可点)。

---

## 0. 一句话

> **ccteam = loop 的运维控制面(operations control plane)。只拥有 loop 外面那层壳,永远不碰 loop 本身的机械。**

这不是给 ccteam「加一个 loop 功能」。是 **loop 这个范式回头解释了 ccteam 为什么长这样** —— 所以 owner 说它「也许是 ccteam 的初心」,对。

---

## 1. 为什么是初心:loop 回头解释了 ccteam

ccteam 的原始定位是「云端常驻的元 AI 团队,从 IM 和 web 驱动」。在 loop 之前,这套架构的用例是「**从手机跟 agent 聊天**」—— 成立,但不够刚需(桌面 TUI 也能聊)。

loop engineering 把用例换成:**一个 agent 在云机上无人值守跑几小时/一整夜,自己发现工作、派发、验证、记录、决定下一步,只在该叫你时叫你。** 这个用例对底座的要求,逐条正是 ccteam 已有的形状:

| loop 无人值守跑起来后最缺的 | ccteam 现成能力 | 状态 |
|---|---|---|
| 跑在云机上,你得从**任何地方看见** | IM/web 网关 + 持久 sid | ✅ 已落地 |
| 跑一夜会**烧光额度** | `budgets.*.max_cost_usd_per_24h` 触顶 auto-disable + cost pill | ✅ 已落地 |
| 部署/重启/掉线后 loop 会话**能续** | resume-by-sid + 保住 model/effort | ✅ 刚修(见 [[goal-statusline-and-model-preserve]]) |
| 跨会话**记忆在磁盘**不在 context | dual-SoT(`progress.jsonl` + `turns.jsonl` per sid) | ✅ 已落地 |
| loop 的**人工门**得能从手机放行 | HITL `PermissionRequest` hook → IM `[同意][拒绝]` | ✅ 已落地 |
| triage **发现推给你** | IM gateway 出站 | ✅ 已落地 |

**loop engineering 不给 ccteam 添新东西 —— 它给 ccteam 已有的形状一个杀手级理由。** 壳一直在等这个 loop。

---

## 2. 三层各归各家(干净到不可能踩红线)

loop 的两个文档都强调:机械层是**商品件**(全内置 vendor),真工程在**预言机 + 薄配置**。把它和 ccteam 的红线叠在一起,得到一个三层 ownership,各层归属唯一、零重叠:

| 层 | 谁拥有 | 内容 | ccteam 立场 |
|---|---|---|---|
| **loop 引擎** | **vendor**(Claude/Codex) | `/goal` `/loop` 动态 Workflow · worktree · subagent · hooks · skills 运行时 | **只透传,绝不在 Rust 重写。** 重写 = 顶撞 [[orchestration-pattern-agnostic]] + [[ccteam-moat-is-the-shell-not-features]] = 战略自杀 |
| **预言机 + state** | **repo**(人) | `AGENTS.md`(约束/INV)+ `vN/vectors`(金样)+ `.loop/state.md`(脊柱) | **天然不碰** —— ccteam 红线本就「不改写项目知识层」 |
| **loop 外的壳** | **ccteam** | 启动 / 看见 / 卡门 / 控预算 / 续命 / 推 triage | **这是 ccteam 的活,且大半已落地(§1)** |

> 这套切分跟 [[orchestration-pattern-agnostic]] 完全一致:编排/智能逻辑是 prompt 层(vendor + skill),ccteam 守结构位置。loop 没有改变这个结论,只是把「结构位置」具体化成「loop 的运维台」。

---

## 2.5 owner 定调:ccteam 三件事 + 永不硬编码 loop

owner 把它收成三件事(2026-06-21):**入门启动 + 稳定运行 + 控制面板**,外加一条硬红线。

🚫 **红线:ccteam 永不硬编码 loop。** loop 的 `plan → act → verify → reflect` 住在**用户写的 skill**(`SKILL.md`,声明式语义),**不进 Rust**。ccteam 代码里没有固定 loop 状态机 —— 它只在任何 loop 外面提供壳。开发者会写多种多样的 loop,ccteam 提供基座、不替他们定义循环。(与「不 vendor 编排逻辑」「moat=壳」一脉。)

| 件 | 是什么 | ccteam 怎么做 |
|---|---|---|
| **A. 入门启动(on-ramp)** | 把门槛从「写一堆 bash」降到「装个 skill 点启动」 | hub 里一个**起步 loop-skill 库**(那「七条可抄循环」+ elorm + oracle-first 模板)→ 一条命令「装这 loop + 指向 repo + 设预算/节奏 + 云端起跑」。原型 `loop-onramp.html` |
| **B. 稳定运行(差异化)** | CC/Codex **给不了**的、让无人值守 loop 跑稳的功能 | 见下表 —— ccteam 在「loop 稳定运行」上的独有价值 |
| **C. 控制面板** | 看 N 个 loop 的预言机 🟢🔴⏸ + 成本 + 等哪道门 | 原型 `loop-ops-console.html` |

**B 的清单(= CC/Codex 没有)**:新智元里 THE HIVE 第一层 `/loop`「关掉电脑就停」—— ccteam 正好补这一类:

- 🌥 **云端常驻**:关机/掉线/重启/重部署都不停(resume-by-sid)
- 💰 **硬预算上限**:跨多 agent 扇出触顶 auto-disable(治 5-10x token 爆炸)
- 📱 **门到手机**:loop 半夜要合 PR/改契约 → 你一拍放行,非桌面弹窗
- ♻️ **状态可恢复**:dual-SoT 跨基建 churn
- ⛓ **跨 vendor**:同一 loop 跑 Claude 或 Codex(Anthropic Routines 只 Claude)

**loop 有版本(reflect 步)**:owner 的「人之后只改循环本身的 v1/v2」—— loop 想改自己(改它那个 skill)= 跟改预言机/CLAUDE.md 同级 → **升人工门**到你手机。ccteam 把「哪个 loop@哪版在哪跑」做成一等(loop-skill 带 pin 版本,经 hub 分发)。

> THE HIVE 三层映射:本地 `/loop`(关机停)→ 云端 Routines → `/batch` 集群,飞轮每周蒸馏 CLAUDE.md。ccteam 不造这三层(vendor 的),但把它们**搬上云端常驻壳 + 跨 vendor + 门到手机** —— 让本地层不再「关机即停」,让蒸馏改 CLAUDE.md 走人工门。

---

## 3. ccteam 为 loop 加的壳(逐条 = 已有能力)

见原型每张卡。一个 loop 卡 = 预言机就绪档(native/buildable/weak)+ 模式(深度 `/goal` · 持续 `/loop` · 宽度 Workflow)+ 预言机 🟢/🔴/⏸ + 状态脊柱(读自 repo `.loop/state.md`,**只读**)+ 成本条。其中:

- **杀手级耦合 = loop 的三个人工门 → ccteam HITL → 手机放行。** Oracle-First §5 的人工门是「动预言机 / 架构跃迁 / 合并」;ccteam 的 `PermissionRequest`→IM `[同意][拒绝]` 正好是「stay the engineer」那道闸。**一个会为你的手机一拍而暂停的 loop,才是能放心走开的 loop。**
- **成本是 loop 的安全带。** Addy + Oracle-First §6 都把 token 成本列为头号风险;ccteam 的 budget 上限 + 模型分层(贵模型抠验证 / 便宜模型做只读心跳)就是治「半夜烧光额度」。loop 让这条从 nice-to-have 变必需。

---

## 4. 唯一要新加、且 ccteam-shaped 的一件:oracle-diff 门

Oracle-First 的核心约束(§4 件1 + §10):**预言机 loop 只读,碰它必过人工门** —— 否则 loop 能靠改裁判给自己作弊。落到 ccteam 是一个**纯壳、零智能**的新门:

> verifier 子代理跑 `git diff --stat <预言机目录>`(`AGENTS.md` / `vN/vectors`)→ 非空 → ccteam 升一个人工门到你手机:「本轮想改契约,放行?」

ccteam **只路由不评判**(不读 diff 内容做判断、不注入、pattern-agnostic)。它兜住 Oracle-First §10 那条「碰 `AGENTS.md`/`vectors` 必过人工门」,而判断「该不该改契约」永远是人的。原型第一张卡演示的就是它。

这是本 thesis 里**唯一**需要在 ccteam 侧写代码的东西,且它落在已有 HITL 路径上(多一个触发源),不是新子系统。

---

## 5. 分发:loop skill 住 hub

loop 的「自著层」里可共享的部分(oracle-first 填空模板、`excore-verify` 这类跑全套门禁的 skill、elorm 风格的 Ship-PR-Until-Green 骨架)= **skill**。按 [[orchestration-pattern-agnostic]],这些住 **ccteam-hub**,经插件市场一键装进项目 `.claude/skills/`。机械永远留 vendor,ccteam 是**渠道**不是作者。

> 注意 Oracle-First §10.4:每个 repo 的 loop 实例(`.loop/state.md`)**自包含**、不引用外部范式文档。所以 hub 分发的是「怎么做」的 skill(可共享),不是「这个 repo 的状态」(自包含)。

---

## 6. 诚实边界(Addy 自己也 skeptical)

- **ccteam 造不出预言机。** loop 的价值上限 = 预言机强度(Oracle-First §0)。ccteam 能把门路由到你手机、能卡预算,但 oracle-weak 项目(UI/审美/开放研究)上 loop 收益本就低 —— ccteam 救不了。**ccteam 价值最高的恰是 oracle-native 仓。**
- **验证仍在人。** ccteam surface 门、surface 红绿,但「done 是 claim 不是 proof」;ccteam 不替你判对错。
- **comprehension debt / cognitive surrender** 是用户侧风险,工具治不了 —— ccteam 能做的是把 loop 写的东西**可见**(turns/progress/状态脊柱都在),不让它变黑箱。

---

## 7. 狗粮路径:excore

excore 是 Oracle-First 的 worked example(按构造 oracle-first:先写 spec+vectors+INV 才写实现),也是 owner 正在跑的真实仓(s26)。所以第一条狗粮就是 **ccteam 跑 excore 的 loop**:`/goal` 抠 INV-3 状态根、Workflow 做 D 类分布式 harden、oracle-diff 门拦住改 `vectors/` 的轮次。原型三张卡就是照 excore 的真实 backlog 画的。

---

## 8. 与 v0.8.18 环境驾驶舱的关系

上一轮聊的「驾驶舱」分两条腿:A 环境体检(装机/健康)、B 舰队(跨会话观测)。**loop 运维台 = B 腿长大后的样子** —— 舰队视图的杀手内容就是 loop(每个 loop 的预言机状态 + 成本 + 等哪道门)。两个原型共用同一手机壳、同一底栏(插件市场 / 环境 / Loops / Settings)。

→ 落地次序建议:先 A(v0.8.18,小、低风险、Day-0 价值)→ 再把 B 直接做成 loop 运维台(本 thesis)。A 是入口,loop 台是它的灵魂。

---

## 9. 不做清单(红线复述)

- ❌ 在 Rust 里实现 `/goal`/`/loop`/Workflow/worktree —— vendor 的。
- ❌ 生成/改写预言机、`AGENTS.md`、`.loop/state.md` —— repo 的。
- ❌ 跨 vendor 路由/fallback —— prompt 层 `pk` skill,不进 Rust。
- ❌ 替项目「判断」loop 对不对、契约该不该改 —— 人的。ccteam 只路由门、只 surface 状态。

---

## 10. 若拍板的落地姿势

doc-first(本文)→ owner review 原型 + thesis → 真正要写的 ccteam 代码只有两块:① **loop 运维台**(B 腿:读 repo `.loop/state.md` + per-session 成本/模型 → 卡片视图,REST + SPA);② **oracle-diff 门**(verifier 侧 `git diff --stat` 触发 → 既有 HITL 升门)。其余全是组合既有能力 + hub 分发 loop skill。按 [[ship-flow]] 直接在 dev 落、不发 tag。

> Build the loop. But build it like someone who intends to stay the engineer, not just the person who presses go. —— Addy Osmani
> ccteam 的活,就是把那道「stay the engineer」的闸,接到你手机上。
