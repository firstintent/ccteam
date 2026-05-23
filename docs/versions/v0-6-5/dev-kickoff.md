# V0.6.5 — 新会话开发提示词

> 把下面整段(从 `=== START ===` 到 `=== END ===`)**整体**粘到新 Claude session 第一条消息里。
>
> 设计目标:**主会话 context 不膨胀**。主会话只做 (1) 读 PRD,(2) 派 worktree 给 subagent,(3) 验 acceptance gate。Worktree 内代码细节 / 测试输出 / 文件 diff 全部留在 subagent 上下文里,主会话只看一句摘要。
>
> 模型选 **Opus 4.7 (1M)** 或 **Sonnet 4.6**(可选 Opus 主会话 + Sonnet subagent)。

---

```
=== START ===

你是 V0.6.5 release 的主会话 orchestrator。当前位置:/home/rob/workplace/agents/ccteam(main 分支)。

## 上下文(必读 3 份文档,顺序)

1. `@CLAUDE.md` ── 项目红线 + 当前版本基线
2. `@docs/versions/v0-6-5/README.md` ── 19 finding 4 Epic 全景
3. `@docs/versions/v0-6-5/prd.md` ── 19 finding 完整需求(only read when dispatching a specific worktree;不要一次读完)
4. `@docs/versions/v0-6-5/dev-plan.md` ── Wave 1-4 worktree 分配 + 验收 gate

## 你的工作

V0.6.5 共 4 wave,19 finding。**你必须在本会话内完成全部 ── 不允许把 wave 推到 V0.6.6 / V0.7。** 验收不过的 finding,中断 + 问用户 escalate(继续做 / 主动 EOL 删除),不写 "TODO V0.6.6"。

你**不亲自**实现任何 finding。你的执行模型:

```
主会话(你)
  ├── 读 PRD 找到当下 wave 的 worktree 列表
  ├── 起 git worktree
  ├── Task subagent 派工(per worktree,各自 briefing)
  ├── 等 subagent 回报 acceptance gate (cargo test / clippy / 验收 checklist)
  ├── 拉 subagent diff stat + 验证 commit message + PR push
  ├── PR auto-merge
  └── wave handoff doc 落 + 进下一 wave
```

## 关键 hard rules

1. **主会话 context 紧抠**:不读 worktree 内 .rs 文件。Subagent 干完拿回 verification log + diff stat 就 OK。你看不懂的 rust 细节就让 subagent 解释,不要自己读源码。
2. **worktree 标准**:每个 finding 独立 git worktree,路径 `/tmp/ccteam-v065-w<N>-<name>`,branch `v065-w<N>-<name>`。完事 `git worktree remove`。**主仓 main 永远 clean**。
3. **不能跳过 acceptance gate**:每 finding 的 prd.md §"验收" 是 hard gate。任何一项不过 → 不让 subagent 继续 → 不 PR。
4. **PR 流程**(每个 worktree):
   - `git push -u "https://${GH_TOKEN}@github.com/firstintent/ccteam.git" <branch>:<branch>`(SSH push 经常失败,直接 HTTPS + token)
   - `gh pr create --base main ...`
   - `gh pr merge <N> --auto --squash`
5. **wave 顺序**:Wave 1 → 2 → 3 → 4。**不允许并行跨 wave**(每 wave 必须 baseline ≥ 上 wave 数字 + clippy 0 警告才能下一 wave)。
6. **wave 内部并行**:dev-plan.md 标"独立"的 worktree 同时派出去;有依赖关系的(如 F148 依赖 F146)等前置 merge 后派工。
7. **Strict no-leftover**:wave 结束 handoff doc 五段固定(Decided / Rejected / Risks / Files / Remaining);Remaining 段**只能写当 wave 真发现的下版本候选,不能写"本版欠的 finding"**。任何欠的 finding ── 中断主流程 escalate 给我(用户)。

## 派工模板(每 worktree)

调用 `Task` 工具(subagent_type=general-purpose 或 code-simplifier 取决于工作性质),prompt 大致:

```
你是 V0.6.5 Wave <N> worktree <T-id> 的实现者。

任务:实现 finding <F-N>(在 docs/versions/v0-6-5/prd.md 找完整 spec)。

设置:
1. git worktree add -b v065-w<N>-<name> /tmp/ccteam-v065-w<N>-<name> origin/main
2. cd /tmp/ccteam-v065-w<N>-<name>
3. 读 @docs/versions/v0-6-5/prd.md §<F-N> 完整 spec
4. 实现(按 prd 设计 / 文件 / 验收)
5. cargo test --workspace --locked --no-fail-fast — baseline 必须 ≥ <wave 累计目标>
6. cargo clippy --workspace --all-targets --locked -- -D warnings — 0 warning
7. cargo fmt -- <仅你改的 .rs 文件>(不要 fmt sweep 整个 workspace,会爆 PR diff)
8. git commit + push via HTTPS token + gh pr create + auto-merge

回报给主会话:
- 一行 "F<N> done baseline X/Y clippy clean PR #<N> merged"
- 验收 checklist 每项 ✓ / ✗
- diff stat 行数 (+a -b 几文件)
- 任何对 prd 设计的偏离(必须 escalate,不要自由发挥)

不要回报:
- .rs 实现细节
- 完整 cargo test 输出(只回报 numbers)
- 完整文件 listing
```

## Wave 1 → Wave 4 高层流程

**Wave 1** (Epic E + Epic H,7 worktree)
- T1 `mcp-chat-register` (F146) — 先派,merge 后再派 T2/T3
- 同时派 T4 (F149) / T5 (F150) / T6 (F163) / T7 (F164) — 不依赖 T1
- T1 merge 后派 T2 (F147) + T3 (F148+F151)
- 全 merge + baseline ≥ 1516/1 → 落 wave-1-handoff.md → Wave 2

**Wave 2** (Epic F,3 worktree)
- T1 (F152) 先派,T2/T3 等 advise.rs 落
- 同时派 T3 (F155+F156) — 不依赖
- baseline ≥ 1534/1 → wave-2-handoff.md → Wave 3

**Wave 3** (Epic G,4 worktree,全并行)
- T1 (F157) / T2 (F158) / T3 (F159+F161) / T4 (F162) 同时派
- baseline ≥ 1540/1 → wave-3-handoff.md → Wave 4

**Wave 4** (doc-syncer + host-probe + ship,单 worktree)
- CLAUDE.md §一 baseline 表回填 + §四 skill 状态注释清
- tech-design.md / interfaces.md / claude-code-tool-surface.md sync
- workspace version 0.6.4 → 0.6.5 bump
- nas-box005 真机跑 F148 / F157 / F162 / F163 / F164 host-probe → host-probe.md 签字落
- ship gate 11 项全 ✓ → PR + tag v0.6.5

## 真机 host-probe(Wave 4)

ssh `rob@192.168.1.19`,代码在 `/home/rob/nasworkspace/ccteam`。Wave 4 派一个 host-probe 专属 subagent,只跑 nas-box005 不写代码,签字回报。

按需 wipe `~/.ccteam/ /home/rob/projects/<slug>/.ccteam/ ~/.claude/projects/-home-rob-projects-<slug>/`(`docs/versions/v0-6-5/README.md §5 Ship gate` 列出具体场景);F148 host-probe 严格要 fresh wipe(不能用任何老 registry 蒙混)。

## 失败模式

任何下列情况 → **立即中断 + escalate 给我**:
1. 任何 finding 的验收 checklist 有一项过不去
2. baseline 退步(任何 wave 后 cargo test pass count 降)
3. clippy 0 warning gate 破
4. 任何 wave handoff 的 Remaining 段出现 "TODO 留 V0.6.6"
5. nas-box005 真机 host-probe 失败(F148 fresh wipe 走不通,或 F163 graceful shutdown 5s 不出,或 F164 reattach 报错)
6. SSH push 失败(改 HTTPS,见 prd.md §"GitHub push fallback")
7. PR 上有 mergeable=false / mergeStateStatus=DIRTY(rebase 解 conflict)

## 关于上下文经济

最 token-省的策略:
- 主会话本身只 read README.md / dev-plan.md / 当下要派的 finding 那一段 prd.md
- prd.md 全 826+ 行,**不要一次全读**;按 finding 按需读
- subagent 看完 cargo test 几千行输出,只回 "1540/1 clean" 一行
- 用 TaskGet 看 subagent 状态,不要重复发指令
- 每个 worktree 派出去就让它自己跑到 PR 自动合,不来回 ping 状态

## 启动

1. 读 @CLAUDE.md
2. 读 @docs/versions/v0-6-5/README.md
3. 读 @docs/versions/v0-6-5/dev-plan.md
4. 给我(用户)简短回报:"V0.6.5 Wave 1 即将派 7 worktree,F146 先行;无需等待 review,直接开始?"
5. 我回 "go" → 派 Wave 1

如有任何 spec 模糊处 ── 先问我,不要 freelance。

=== END ===
```

---

## 备注:为啥要这么做

**V0.6.5 体量比一般 patch 大**(19 finding · 4 Epic · +58 测试 · 估 8 calendar days 并行)。如果主会话亲自实现每条:
- prd.md 826 行 + 各 finding 实现文件代码 + cargo test 输出 + clippy 输出 + PR review pass + ... 主会话 context **必撞** 1M 上限
- 撞了上限触发 auto-compaction,丢失主线策略,后期 wave 失去前期决策连续性

**Agent-team / subagent 派工**让 worktree 各自烧 context,主会话只持有"调度 + 验收"心智模型,~80% context 一直空着备用 escalation/decision。这是 ccteam 设计哲学本身:**meta-harness 不亲自动手,套上去的 sub-harness 干活**。我们自己 dogfood。

**Strict no-wave-leftover** 是这一版的纪律 ── 之前 V0.6.0 → V0.6.4 五个 patch 累积留账(MCP STUB / F113 验收 / 5.6 桥)就是 wave 留尾的恶果。V0.6.5 必须自己刹住这个习惯。
