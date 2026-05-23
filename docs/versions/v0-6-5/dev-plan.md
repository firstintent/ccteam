# V0.6.5 — Dev Plan(4 Wave / worktree-per-finding · 19 finding · 4 Epic)

> **Plan-first 原则**(CLAUDE.md §五):本文件 + `README.md` + `prd.md` + `dev-kickoff.md` user review pass 后才进 Wave 1。
> **Pre-v1.0 不留技术债**:`workflow.yaml` / BotRegistration / MCP tool schema 直接前进;移除 `chat_lifecycle` STUB 时**不留 deprecated alias**;旧 user 手工 poke 的 registry JSON 文件 schema 不变,自然兼容。
> **多 session 并行**:worktree-per-finding,主仓 `main` 不变 dirty;每 Wave 完成 PR 提交后再开下 Wave。
> **Strict no-wave-leftover**:任何 finding 验收不过 ── 主会话 escalate 给用户 → 当场决策(继续做 / 主动 EOL 删除),**禁止**写 "TODO ship in V0.6.6 / V0.7"。本版收账,不开新账。
> **执行模型**:主会话用 agent-team / Task subagent 派工各 worktree,主会话只做 dispatch + acceptance gate 验,不读各 worktree 实现细节,防 context 膨胀。详 `dev-kickoff.md`。

---

## Wave 0 — doc-first(本 session,~45 min)

- ✅ `docs/versions/v0-6-5/README.md`
- ✅ `docs/versions/v0-6-5/prd.md`
- ✅ `docs/versions/v0-6-5/dev-plan.md`(本文件)
- ⏭️ user review pass → `go` → Wave 1 kick-off

---

## Wave 1 — Epic E(MCP chat 桥 + creator)+ Epic H(运维健壮性)(P0)

**目标**:V0.6.0 立项就承诺、从未真正交付的 `/ccteam-creator` end-to-end onboarding 这次走通,同时关掉 2026-05-23 nas-box005 实战发现的两条运维 blocker(SIGTERM 不响应 + tmux session 不 reattach)。Wave 1 完成后第一次:fresh machine + 零手工编辑 → `/ccteam-creator` → daemon → TG 收回复;daemon restart 周期里 bot 自动 reattach 老 tmux 不失能。

### 串行 + 并行依赖

```
F146 (register/list/unregister MCP)
  ↓
F148 (creator Phase 5.6 真调 F146)  ──┐
F151 (remove --purge 也清 registry)  ──┴── 各自 worktree,F148/F151 可并行
  ↓
F147 (send_input/history/reset MCP)  ── 独立(可与 F148/F151 并行)
F149 (/ccteam dispatcher 文案 sweep) ── 独立(F146 ship 后才动)
F150 (/ccteam-control 接 admin_*)    ── 独立(全 wave 任何时点都可启)
F163 (SIGINT/SIGTERM graceful)       ── 独立(完全不动 chat 路径)
F164 (claude-tui reattach)           ── 独立(完全不动 MCP 路径)
```

### 工作树分配

| Teammate | Branch | Findings | Briefing 重点 | Est. |
|---|---|---|---|---|
| W1-T1 `mcp-chat-register` | `v065-w1-mcp-chat-register` | **F146** | `mcp_chat_tools.rs::dispatch` 拆 stub;register/unregister/list 三个真 dispatch;**vendor lowercase enforcement**;**daemon heartbeat file** 给 `bot_running_status` 用 | 1.5 d |
| W1-T2 `mcp-chat-runtime` | `v065-w1-mcp-chat-runtime` | **F147** | send_input mailbox 写 + history tail + reset signal;reset 时 OutboundCursor `force_set(0)` + prior_offsets.clear()(V0.6.4 Bug B 防线);新 ArtifactWatcher 监 signals/reset.signal | 2 d |
| W1-T3 `creator-bridge` | `v065-w1-creator-bridge` | **F148** + **F151** | (F148) `/ccteam-creator` SKILL.md Phase 5.6/5.9 文案改 + e2e_creator_full_path_test.rs (stub TG/claude);(F151) `cmd_remove::purge` 加 `imd/registry/<slug>/` + 优先 MCP unregister fallback fs delete | 1.5 d |
| W1-T4 `dispatcher-sweep` | `v065-w1-dispatcher-sweep` | **F149** | `skills/ccteam/SKILL.md` 6 处 stale Wave 1 fallback 删除;intent table 描述对齐当前实状态 | 0.5 d |
| W1-T5 `control-admin-smoke` | `v065-w1-control-admin` | **F150** | audit `skills/ccteam-control/SKILL.md` MCP 调用 + 6 个 admin_* smoke test + `docs/user-manual.md` admin 段 | 1 d |
| W1-T6 `graceful-shutdown` | `v065-w1-graceful-shutdown` | **F163** | `run_start` 加 `tokio::signal::ctrl_c()` + SIGTERM listener + watch::channel cancel + 5s timeout abort + pidfile unlink + 不 kill tmux 子进程;`graceful_shutdown_test.rs` 子进程 spawn + kill -TERM + 5s 验 ps clear | 1 d |
| W1-T7 `tmux-reattach` | `v065-w1-tmux-reattach` | **F164** | `claude_tui::start_thread` 加 "session exists → 健康检查 → reattach OR recreate dead";`TmuxSession::list_pane_pids()` + `kill()` helpers;两个 test case(alive reattach / dead recreate);**不**改 `resume_thread` | 1.5 d |

**并行**:T1 / T4 / T5 / T6 / T7 day1 同时起;T2 day1 等 T1 框架定下来(共享 mcp_chat_tools.rs 文件);T3 day2 等 T1 PR merge 后启(依赖 chat_register_bot MCP)。

### Wave 1 PR 合入顺序

1. **T1 F146** → merge first(基础设施,F147/F148/F151 都依赖)
2. **T6 F163** + **T7 F164** + **T4 F149** + **T5 F150** → 并行 PR,各自 review pass 后 merge(都不依赖 T1)
3. **T2 F147** + **T3 F148+F151** → 并行 PR(F148 依赖 T1)

### Wave 1 验收

- `cargo test --workspace --locked --no-fail-fast` ≥ **1516 / 1**(1482 + 34 新测试:F146 +6 / F147 +8 / F148 +2 / F150 +6 / F151 +2 / F163 +4 / F164 +6)
- clippy `-D warnings` clean
- F148 `e2e_creator_full_path_test.rs` 通过(stub adapter)
- F163 `graceful_shutdown_test.rs` 通过
- F164 `claude_tui_reattach_test.rs`(alive + dead 双 case)通过
- **真机 host-probe**(nas-box005,fresh wipe):走通 `/ccteam-creator` + TG round-trip + SIGTERM graceful + tmux reattach,**手动签字** → `docs/versions/v0-6-5/wave-1-handoff.md`
- 每 PR 描述含 `prd.md` finding 链接 + verification log + `git push -u origin <branch>`(per V0.6 学到的陷阱)

### Wave 1 handoff 文档

`docs/versions/v0-6-5/wave-1-handoff.md` 五段固定(per V0.6.0 4-wave 范式):
- **Decided**:每 finding 落地决策(如:F146 chat_lifecycle 移除不留 alias)
- **Rejected**:本 wave 不做的(如:F146 不在本版搞 multi-tenant `im_chat_ids: Vec<String>`)
- **Risks**:发现的新风险(如:F147 reset 期 outbound 行为)
- **Files**:本 wave 改的文件列表
- **Remaining**:遗留给 Wave 2/3/4 的(如:F162 corpus 收 V0.6.6)

---

## Wave 2 — Epic F:Advise vote/parallel + Codex critic(P1)

**目标**:V0.6.0 Wave 3 承诺的 `/ccteam-advise` 真路径接通;Codex auto-critic 验证 deterministic。

### 工作树分配

| Teammate | Branch | Findings | Briefing 重点 | Est. |
|---|---|---|---|---|
| W2-T1 `advise-vote` | `v065-w2-advise-vote` | **F152** | `crates/ccteam-core/src/advise.rs`(新)`advise_vote()` fn + Claude API + Codex spawn 并行 + verdict synthesizer;budget enforcement;Codex-unavailable display | 2 d |
| W2-T2 `advise-parallel` | `v065-w2-advise-parallel` | **F153** + **F154** | (F153) `advise_parallel()` fn 共享 spawn helper;(F154) skill body 文案 sweep ── 同 wave 起,文档跟代码 | 1 d |
| W2-T3 `codex-critic-gate` | `v065-w2-codex-critic` | **F155** + **F156** | (F155) `ccteam doctor --check-codex-auto-critic` flag + stub binary tests;(F156) `/ccteam-team` §3.5 验证 Codex critic 真路径或显式 V0.7 deferral(默认实装) | 1.5 d |

### Wave 2 PR 合入顺序

1. **T1 F152** → merge first(F153 复用 spawn helper)
2. **T2 F153+F154** + **T3 F155+F156** → 并行 merge

### Wave 2 验收

- baseline ≥ **1534 / 1**(1516 + 18 新测试)
- clippy clean
- F152 真路径(Codex installed)+ Codex-unavailable path 双 e2e
- `ccteam doctor --check-codex-auto-critic` exit code 行为正确
- `docs/versions/v0-6-5/wave-2-handoff.md` 五段固定

---

## Wave 3 — Epic G:UX cohesion + F113 验收补做

**目标**:零依赖 60s 体验、决策树替代架构词汇、F113 验收 #5 第一次真做。

### 工作树分配

| Teammate | Branch | Findings | Briefing 重点 | Est. |
|---|---|---|---|---|
| W3-T1 `scan-quick` | `v065-w3-scan-quick` | **F157** | `ccteam-scan --quick` mode(1 sonnet agent + 3 fixed questions, 60-90s target)+ `/ccteam` intent 5 (code-scan) 路由 + intent-corpus 加 5 条 sample | 1.5 d |
| W3-T2 `decision-tree` | `v065-w3-decision-tree` | **F158** | 新文档 `docs/task-to-command.md` + `docs/quickstart.md` 重写第一节 + `docs/user-manual.md` 加 lead + `README.md` 英文决策树 | 1 d |
| W3-T3 `dispatcher-hide` | `v065-w3-dispatcher-hide` | **F159** + **F161** | (F159) `/ccteam` SKILL.md hide-unimpl red line + dispatcher 4-options dynamic;(F161) cross-docs grep sweep "Wave [123]/STUB/NotImplemented" → 全 0 命中 | 0.5 d |
| W3-T4 `intent-corpus` | `v065-w3-intent-corpus` | **F162** | `tests/intent-corpus.yaml` 50 query(7 intent 各 5-8 条,中英混合)+ `scripts/host-probe/intent-accuracy.sh` + 跑 + `docs/versions/v0-6-5/intent-accuracy.md` 归档 | 2 d |

### Wave 3 PR 合入顺序

全部并行,各自 PR review pass 后合入(无相互依赖)。

### Wave 3 验收

- baseline ≥ **1540 / 1**(1534 + 6 新测试)
- clippy clean
- F162 `intent-accuracy.md` 落,accuracy ≥ **0.90**(< 0.90 不阻 ship 但单独 finding 跟)
- F157 `ccteam-scan --quick` 在 sample repo 90s 内出报告(host-probe)
- F158 决策树 `grep -E "mode:.*chat|preset:.*chat-pocket" docs/{quickstart,user-manual}.md README.md` 0 命中
- F161 `grep -E "Wave [123]|STUB|NotImplemented" docs/{quickstart,user-manual,recipes,troubleshooting}.md docs/advanced/*.md skills/*/SKILL.md README.md` 0 命中
- `docs/versions/v0-6-5/wave-3-handoff.md` 五段固定

---

## Wave 4 — doc-syncer + host-probe + ship gate

**目标**:Tier-1 docs 同步、`CLAUDE.md §一` baseline 回填、版本号 bump、tag。

### 工作分配(单 teammate / 1 day)

| Step | 内容 |
|---|---|
| 4.1 | `CLAUDE.md §一 当前状态` 表回填:HEAD / version / baseline 数 / kLOC / 当前最新版 / 上一版 / V0.6.x 候选 / V0.7 主线候选 |
| 4.2 | `CLAUDE.md §四 Skills` 表 status 描述 sync |
| 4.3 | `docs/tech-design.md` § (有关 chat MCP / advise MCP / auto-critic 的)同步当前 ship 状态 |
| 4.4 | `docs/interfaces.md` MCP §(原 chat_lifecycle row 替换 chat_register_bot / chat_unregister_bot,advise tools 从 STUB 升真)|
| 4.5 | `docs/claude-code-tool-surface.md`(如有)── MCP 工具新清单 |
| 4.6 | 任何 V0.6.4 漏回填的(如 V0.6.4 OutboundCursor PR 当时也未回填 CLAUDE.md baseline)|
| 4.7 | workspace version bump:`Cargo.toml` `0.6.4 → 0.6.5` + `cargo check --offline` 触发 `Cargo.lock` update |
| 4.8 | commit message:`v0.6.5: chat MCP bridge + advise vote + UX cohesion + F113 verification (closes V0.6 promises)` |
| 4.9 | PR 描述含全部 17 finding 链接 + ship gate clear list + verification logs |
| 4.10 | git tag `v0.6.5`(在 PR merge 后,from `main`)|

### Wave 4 ship gate(per README §5,prd.md §Ship gate)

merge 前 11 项全 ✓:

- [ ] cargo test ≥ **1540 / 1**
- [ ] clippy `-D warnings` clean
- [ ] F148 host-probe(nas-box005 fresh)pass + 手动签字
- [ ] F157 host-probe(`scan --quick` ≤90s 出报告)pass + 手动签字
- [ ] F162 intent-accuracy.md ≥ 0.90
- [ ] F163 host-probe(`kill -TERM` 5s graceful)pass + 手动签字
- [ ] F164 host-probe(daemon restart reattach)pass + 手动签字
- [ ] tier-1 docs grep clean
- [ ] CLAUDE.md §一 baseline 表更新
- [ ] `ccteam doctor` 报告 MCP "26 active, 0 stubs"
- [ ] version bump + commit prefix `v0.6.5:`

---

## 估算汇总

| Wave | Teammates | Calendar days | 累计 baseline 增量 |
|---|---|---|---|
| 0 (doc-first) | 1 | 0.5 | — |
| 1 (Epic E + Epic H) | 7 parallel | 2.5 (T1 + T2/T3 后续) | +34 测试 → 1516/1 |
| 2 (Epic F) | 3 parallel | 2 | +18 测试 → 1534/1 |
| 3 (Epic G) | 4 parallel | 2 | +6 测试 → 1540/1 |
| 4 (doc-syncer + host-probe) | 1 | 1 | — |
| **总计** | 7 peak / 15 unique worktree role | **8 calendar days** | **+58 测试** |

**单线作业** estimate:10-13 天(无 worktree 并行)。
**主会话 + agent-team 并行 estimate**:5 calendar days(主会话不读 worktree 实现,只 dispatch + verify)。

---

## 主仓 dirty 管控(CLAUDE.md §五 多 session 规则)

每 wave 开 worktree:

```bash
git worktree add -b v065-w<N>-<finding> /tmp/ccteam-v065-w<N>-<finding> origin/main
cd /tmp/ccteam-v065-w<N>-<finding>
# 开干
```

完事 PR + `git worktree remove /tmp/ccteam-v065-w<N>-<finding>`。

**主仓 main 不变 dirty**。跨 session 看到主仓 dirty:`git stash push -m "v065-doc-first WIP"` 再切。

---

## V0.6.5 ship 后

1. 关闭本版所有 17 finding 的对应 issue / 内部 finding tracker
2. CLAUDE.md `§一 V0.7 主线候选` 行更新:删除 "国内 IM 启用" 表述里跟 V0.6.5 重叠的部分(本版只关 V0.6 账,V0.7 仍以国内 IM + monorepo 为主)
3. `docs/versions/v0-6-5/host-probe.md`(新)留作执行实证存档
4. 通知 / 文档:用户面 Telegram 用户(如有)告知 chat-mode end-to-end onboarding 第一次真正可用
