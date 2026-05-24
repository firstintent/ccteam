# V0.6.6 — Dev Plan(patch flow,8 worktree 并行,1-2 wave)

> **Plan-first 原则**(CLAUDE.md §五):本文件 + `README.md` + `prd.md` user review pass 后才进 Wave 1。
> **Pre-v1.0 不留技术债**(CLAUDE.md §五 #3):本版 8 finding 全 ship,不向 V0.6.7 / V0.7 拖单,验收不过 → 主会话 escalate 用户。
> **多 session 并行**(CLAUDE.md §五 #5):worktree-per-finding,主仓 `main` 不变 dirty;每 finding 一 worktree 一 PR。
> **执行模型**:主会话 dispatch 8 个独立 Opus subagent 各跑一个 worktree;主会话只做 dispatch + acceptance gate 验,不读各 worktree 实现细节,防 context 膨胀。
> **Strict no-finding-leftover**:沿 V0.6.5 加严的「strict no-wave-leftover」── 任一 finding 验收不过 → 主会话 escalate → 当场决策(继续做 / 主动 EOL 删 / V0.7 显式 defer-with-justification);**禁止**写 "TODO ship in V0.6.7"。

---

## Wave 0 — doc-first(本 PR,~45 min,本 session 完成)

- `docs/versions/v0-6-6/README.md` ✅
- `docs/versions/v0-6-6/prd.md` ✅
- `docs/versions/v0-6-6/dev-plan.md`(本文件)✅
- user review pass → `go` → Wave 1 kick-off

---

## Wave 1 — 8 finding 全并行 worktree dispatch

**目标:** 8 个独立 Opus subagent 各跑一个 worktree;F166-F173 完全独立(prd 附录 A 依赖图),全并行;主会话依次接收 PR,按 site-overlap 顺序 merge。

### 工作树分配

| Teammate | Branch | Finding | 体量估算 | Briefing 重点 |
|---|---|---|---|---|
| W1-T1 `release-binary` | `v066-w1-release-binary` | **F166** | 1.5 d | `.github/workflows/release.yml` 4-matrix CI + `install.sh` POSIX-compliant + checksum 强校验 + README quickstart 改造;windows MSVC build 若 `tokio::signal::unix` fail → `#[cfg(unix)]` gate;macOS-arm64 GH runner 排队等;**关键 invariant**:历史 tag(v0.6.0-v0.6.5)force-push 不触发 release |
| W1-T2 `sensible-defaults` | `v066-w1-sensible-defaults` | **F167** | 1 d | `project_probe.rs` 启发式探测(Monorepo/SingleRepo/DocsOnly/ScriptsOnly)+ `render_workflow_template` 加 `probe` 参数 + `ccteam probe-project --json` 新 CLI sub-cmd + `ccteam-creator` SKILL.md Phase 3.6;**边界守:不**加新 preset、不 LLM-assisted role auto-gen |
| W1-T3 `todo-sweep` | `v066-w1-todo-sweep` | **F168** | 1 d | 9 个 site 逐条决断(prd §F168 表)+ V0.7 defer site 改注释为「V0.7 deferred (justification): ...」格式 + `no_silent_todo_test.rs` 回归 grep test;**关键**:实数为准(实际 grep 数 ≠ 9 时,以实际为准 + 全部决断,**禁止悄悄不决断**);F168 site #1/#5/#7 与 F173/F169/F170 PR site-overlap → 同 PR 一并清 |
| W1-T4 `cost-today-ledger` | `v066-w1-cost-today-ledger` | **F169** | 0.5 d | `nl_admin::cost_today` 重写接 `load_budget_ledger` + `sum_24h(vendor)` helper + budget cap warning + 4 test;ledger schema 与 F152 align(本 finding 第一步 read `<root>/cost-budget.json` 实际 shape) |
| W1-T5 `doc-scrub` | `v066-w1-doc-scrub` | **F170** | 0.5 d | 4 个 doc-comment site 各 1-line patch(dashboard.rs:10 / team.rs:1503 / pricing.rs:51 / project_mcp_json.rs:18);#3 若发现 `Vendor` 未 re-export → 加 `pub use` 后再改 doc(扩 ~5 LOC);grep verify 0 命中 |
| W1-T6 `doctor-verify-mcp` | `v066-w1-doctor-verify-mcp` | **F171** | 0.5 d | `--verify-mcp` flag + `run_verify_mcp` + `VerifyMcpReport` struct + static `STUB_TOOLS: &[&str] = &[]` const + 6 test(`doctor_verify_mcp_test.rs`);输出格式对齐 V0.6.5 ship gate item #9 措辞「MCP tool surface: 26 active, 0 stubs」 |
| W1-T7 `chat-snapshot` | `v066-w1-chat-snapshot` | **F172** | 2.5 d | `chat_snapshot` event const + `build_chat_snapshot_event` helper + `SnapshotPolicy` + `BotSupervisor::maybe_snapshot` 在 `chat_turn_completed` hook 调 + daemon restart recovery flow + idempotent re-attach `recovery_applied_in_restart_cycle` BoolMap;**红线守**:不新建第 9 类业务 event(扩 chat-mode 子家族)/ 不主动 kill / 不 capture-pane(grep verify);29-30 test |
| W1-T8 `codex-critic-ledger` | `v066-w1-codex-critic-ledger` | **F173** | 2 d | `default_adapter_factory` Codex arm 改用 `CodexExecAdapter` + `orchestrator::adapter_for_chat` 同步 + `CodexExecAdapter::submit_turn` 加 ledger hook + `ccteam doctor` cost-orphan invariant + `skills/ccteam-team/SKILL.md` F156 文案改「shipped V0.6.6 F173」+ `daemon.rs:84` TODO marker 清(F168 #1 决断同 PR);**关键**:`turn.completed` JSONL 实际字段名 verify;`BudgetExceeded` hard-fail(不自动 bypass) |

**并行启动:** T1-T8 day1 同时起,各自独立 worktree,不读其他 worktree 实现。

### Wave 1 PR 合入顺序(由主会话 dispatch agent 控制)

主会话不规定固定顺序,但建议:

1. **T5 F170**(0.5 d,纯 doc)+ **T6 F171**(0.5 d,小独立 CLI)→ 最快可 merge,清场地
2. **T4 F169**(0.5 d)→ ledger read-only path,无依赖
3. **T8 F173**(2 d,ledger write path)→ 与 T4 site-overlap(F168 #1)在 T3 之前 merge
4. **T3 F168**(1 d)→ 等 T4/T8 merge 后再 merge(避免 #1/#5 决断与 T4/T8 重复改)
5. **T2 F167**(1 d)→ 独立
6. **T1 F166**(1.5 d)→ 独立但需 tag 测,**最后 merge**(避免误触 release CI on intermediate tags)
7. **T7 F172**(2.5 d,体量最大)→ 与 T8 时间重叠,但代码路径完全分离,无冲突;最后 merge

**冲突处理:** 任两个 PR rebase 冲突 → 主会话 dispatch 一个 fix-rebase subagent 处理,**不**让 worktree agent 自己解决跨-finding 冲突(防 context 污染)。

### Wave 1 验收(集成 gate)

- `cargo test --workspace --locked --no-fail-fast` ≥ **1660 / 1**(1583 起点 + 各 finding 估算见 README §0)
- `cargo clippy --workspace --all-targets --locked -- -D warnings` 0 命中
- 每 finding `prd.md §F1XX` 验收段全过(各 PR 描述链接)
- F168 决断列表(9 项 + 三档分类)写入 `docs/versions/v0-6-6/wave-1-handoff.md`(Decided / Rejected / Risks / Files / Remaining 五段固定,per V0.6.0 4-wave 范式)
- 每 PR 描述含 `prd.md` finding 链接 + verification log + `git push -u origin <branch>`(per V0.6 学到的陷阱)

---

## Wave 2 — doc-syncer + host-probe + ship gate(必要时与 Wave 1 末段并行)

**目标:** Tier-1 docs sync + nas-box005 真机验 + version bump + tag。

### 任务分配

| Task | Owner | 内容 | Est. |
|---|---|---|---|
| doc-sync | doc-syncer subagent | CLAUDE.md §一 baseline 表更新(test pass count + workspace version + 当前最新版 + 上一版 + V0.6.x 延期候选);`docs/dev-coupling-audit.md` 加 F166-F173 索引段;`docs/versions/v0-6-6/wave-1-handoff.md` 5 段;`docs/versions/v0-6-6/host-probe.md` host-probe 模板 | 0.5 d |
| host-probe | host-probe subagent on nas-box005 | F166 install.sh(fresh wipe → `curl ... \| sh` → version verify);F167 sensible defaults(`/ccteam-creator` 跑 monorepo + single repo + docs-only 三场景,verify yaml scope 段非空);F169 真 ledger(advise call → IM `cost today` 出真数);F170 grep 0 命中验;F171 `doctor --verify-mcp` PASS;F172 daemon restart recovery(10+ turn → `kill -TERM` → restart → 11th turn 引用早 turn);F173 Codex critic ledger(advise_vote claude+codex → `<root>/cost-budget.json` 含两 row);手动签字 → `host-probe.md` | 1 d |
| version bump + tag | main session | `workspace.package.version` `0.6.5` → `0.6.6`;commit `v0.6.6: <summary>`;git tag `v0.6.6`;push tag → 触发 F166 release CI;verify GH Release 自动创建 + 4 个 artifact + SHA256SUMS | 0.3 d |

### Wave 2 ship gate(README §5 sync,15 项)

1. ✅ baseline `≥1660/1`
2. ✅ clippy `-D warnings` clean
3. ✅ F166 GH Actions release CI(tag push → 4-matrix build → release artifact 全齐)
4. ✅ F166 install.sh nas-box005 真验
5. ✅ F167 sensible defaults host-probe(三 project 类型)
6. ✅ F168 grep 实数验(剩 ≤2 个 site,其余 V0.7 deferred 格式)
7. ✅ F169 真 ledger 验(IM + CLI 同数字)
8. ✅ F170 doc-comment scrub clean(`grep -rn "V0.3.3 cleanup\|F49 wires\|once Wave 2 lands\|Wave 2 wires it into the"` 0 命中)
9. ✅ F171 `doctor --verify-mcp` PASS + 自动化 test 覆盖
10. ✅ F172 daemon restart recovery host-probe + `grep -rn "tmux capture-pane"` 0 命中(红线守)
11. ✅ F173 Codex critic ledger 真验 + `doctor` 无 cost-orphan + `skills/ccteam-team/SKILL.md` F156 文案 cleanup
12. ✅ CLAUDE.md §一 baseline 表 V0.6.6 数字
13. ✅ dev-coupling-audit.md F166-F173 索引补
14. ✅ F168 9 site 决断列表 in wave-1-handoff.md
15. ✅ tag `v0.6.6` push + GH Release notes(F166 install instructions 嵌入)

### Wave 2 验收 → ship

15 项 ship gate 全过 → `v0.6.6` tag + GH Release ship。

---

## 失败处理 / 升级路径(CLAUDE.md §五 #6:测试不过不算完成)

| 场景 | 处理 |
|---|---|
| 某 worktree subagent 卡 ≥ 0.5 d 无 progress | 主会话 dispatch 一个 unblock subagent 接手(briefing 含原 PRD section + 已 commit 的 partial work);**不**让原 agent 继续 spin |
| 某 finding test 通不过 + 估算无法在 +0.5 d 内修 | 主会话 escalate 给用户 → 当场决策三档(EOL 删 / V0.7 显式 defer-with-justification / 加 +0.5 d 继续);**禁止**自动决定推 V0.6.7 |
| 实际 grep TODO 数(F168)≠ 9 | 实数为准,wave-1-handoff 列实际数 + 全部决断;**禁止**对 spec 数字 freelance(per task block 模式) |
| F170 实际 site ≠ 4 | 实数为准,标题改 "F170 scrub N sites"(per task block 模式) |
| F172 设计与红线冲突(如必须新加第 9 类业务 event) | 立即停 + 通报 "v066 SPEC QUESTION: F172 设计 violates <红线>, 主会话需重审"(per task block 模式) |
| F173 F156 R8 cross-ref(V0.6.5 wave-2-handoff)实际不存在 | 立即停 + 通报(per task block 模式)── doc-first session 已 verify line 36 存在 R8(`docs/versions/v0-6-5/wave-2-handoff.md`),代码 PR 阶段若 wave-2-handoff 内容被改动须重审 |
| nas-box005 host-probe 失败(网络 / 物理) | 主会话 dispatch retry(最多 2 次)+ 失败留 log;若硬体不可达 → ship gate 走 docker-based fallback verify(F166 install.sh 在 ubuntu:24.04 container 内跑) |

---

## 工时估算汇总

| Wave | Tasks | 估算 | 并行 / 串行 |
|---|---|---|---|
| 0 (doc-first) | 本 PR | 0.05 d | 单 session |
| 1 (8 worktree) | F166-F173 | max(各 finding 估算) ≈ **2.5 d**(F172 体量最大) | 8 worktree 并行 |
| 2 (doc-sync + host-probe + tag) | 收尾 | 1.5 d | 部分与 Wave 1 末段并行 |
| **总** | | **~3 d**(并行)/ **~10 d**(串行单线) | |

对比 V0.6.5(19 finding,5 d 并行 / 10-13 d 单线)── V0.6.6 体量 ~50% V0.6.5,patch flow 节奏匹配。

---

## 红线核对(关 CLAUDE.md §三 + README §3)

每 worktree subagent briefing 必含:

- **F166**:install.sh 不破坏用户 PATH;不要求 sudo;不下载未签名 binary 不验 checksum;不静默安装到系统目录
- **F167**:probe 仅看文件存在性(不 parse 代码);不引入 prompt injection(probe 结果是 yaml ctx,user-visible)
- **F168**:决断不留 silent TODO;每 V0.7 defer 必带 justification 1-2 sentence
- **F169**:不破坏 ledger schema(只 read);若需 helper API 加,与 F173 共享
- **F170**:纯 doc chore,行为零变化;若 #3 需 `pub use` 加,扩 ~5 LOC 不超
- **F171**:不引入新 MCP tool(F171 是 CLI flag,不动 MCP 总数 26);STUB_TOOLS const 是 invariant 守门员
- **F172**:**红线最严**:不新建第 9 类业务 event(扩 chat-mode 子家族)/ 不主动 kill / 不 capture-pane / recovery prompt 是 user prompt 形式不是 system prompt
- **F173**:不动 F124 HumanApprovalAdapter scope(已 V0.7 defer);F156 SKILL.md 文案改「shipped」不留 alias;BudgetExceeded hard-fail 不 bypass

---

## 完成判据(本 dev-plan 验收)

- [ ] Wave 1 8 个 PR 全 merge 到 main(或单一 collation PR)
- [ ] Wave 2 doc-sync + host-probe + version bump + tag 全完成
- [ ] 15 项 ship gate 全过
- [ ] `v0.6.6` GH Release ship 含 4 个 prebuilt binary + SHA256SUMS
- [ ] CLAUDE.md §一 baseline 表反映 V0.6.6
- [ ] 无 finding 推 V0.6.7 / V0.7(strict no-leftover 守)
