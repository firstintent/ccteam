# V0.6.1 — Dev Plan(3 Wave / 11 teammate)

> **Plan-first 原则**(CLAUDE.md §五):本文件 + `README.md` + `prd.md` user review pass 后才 TeamCreate。
>
> **Pre-v1.0 不留技术债**:F-finding 实现里**不写 backward-compat shim**;workflow.yaml schema 直接加字段(no `#[serde(default)]` legacy compat unless 必要);CLI surface 直接扩(no deprecated alias)。

---

## Wave 0 — doc-first(本 session,~30 min)

- ✅ 落 `docs/versions/v0-6-1/README.md`
- ✅ 落 `docs/versions/v0-6-1/prd.md`
- ✅ 落 `docs/versions/v0-6-1/dev-plan.md`(本文件)
- ⏭️ user review → "go"

---

## Wave 1 — Cleanup & bridges(4 teammate 并行,~1h)

**目标**:V0.6.0 retained risks 全清 + EN README 落 + tier-1 doc drift 0 命中 → 干净基线给 Wave 2 + 3 接力。

### Teammate W1-T1: `probe-fix` — F119 + F120

**Worktree**:`git worktree add -b v061-w1-probe-fix /tmp/ccteam-v061-w1-probe-fix origin/main`
**Briefing 关键点**:
- 读 `docs/versions/v0-6-1/prd.md` §F119 + §F120
- 读 `docs/versions/v0-6-0/host-probe.md` §Observations 1-3
- 实现:`scripts/host-probe/run-probes.sh` 加 daemon-start/stop block + overnight-builder fake workflow runner
- 实现:`crates/ccteam-imd/src/daemon.rs` 加 health-write loop + `ccteam-imd health` CLI flag
- 测试本地跑通(`./scripts/host-probe/run-probes.sh pocket-assistant` rc=0 + cost.txt 非 0)
- **不**改任何 Wave 2/3 finding 涉及代码

**Acceptance**:
- baseline ≥ 1283 / 1
- clippy `-D warnings` clean
- `./scripts/host-probe/run-probes.sh` 本地 dry-run pass
- PR description 含两 finding 链接 + verification log
- **end 跑 `git push -u origin v061-w1-probe-fix`**(per V0.6 学到的陷阱 #3)

### Teammate W1-T2: `cost-doctor` — F121

**Worktree**:`git worktree add -b v061-w1-cost-doctor /tmp/ccteam-v061-w1-cost-doctor origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F121
- 实现 `ccteam doctor --check-pricing-version` flag
- mock `chrono::Local::now` 用 `CCTEAM_TEST_NOW=YYYY-MM-DD` env override
- 3 状态测试(ok / warn / error)pin 在新 test file
- `ccteam doctor`(无 flag)隐式跑 pricing check

**Acceptance**:同 W1-T1 + 新 test pass。

### Teammate W1-T3: `codex-bridge` — F122

**Worktree**:`git worktree add -b v061-w1-codex-bridge /tmp/ccteam-v061-w1-codex-bridge origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F122
- 读 `crates/ccteam-core/src/execution/{codex_app_server, claude_tui}.rs`(claude_tui 是 reference 模式)
- 实现 codex_app_server adapter 持 ProgressJsonlWriter Arc + bridge turn/done 等关键事件
- mock UDS server test verify bridge

**Acceptance**:同上。

### Teammate W1-T4: `doc-curator` — F125 sweep + F126 EN README + CLAUDE.md 红线

**Worktree**:`git worktree add -b v061-w1-doc-curator /tmp/ccteam-v061-w1-doc-curator origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F125 + §F126
- **F126 EN README**:`README.md` EN-only rewrite,保 V0.6.0 3-mode 平等 narrative + 5 preset 表;**删现 "Status / V0.6.x 蓄势" 段**(用户看的 README 不夹版本进展);footer 只留 1 行 `See [docs/versions/v0-6-1/README.md](docs/versions/v0-6-1/README.md) for release notes`;长度 ≤80 行
- **F126 CLAUDE.md 红线 2 row**:
  1. `| **root README.md MUST be English** | 守 | 守 | 守 |`
  2. `| **README.md 不含版本进展/状态信息** | 守 | 守 | 守 |`(版本进展去 `docs/versions/v0-X-Y/README.md`)
- **F125 sweep**:跑 `.audit/doc-inventory.txt` + drift grep(prd.md §F125 内 listed)+ 修 tier-1 doc 内命中 — V0.5 旧 API / V0.4 phase / V0.5 unprefixed MCP / V0.5 cost path / V0.5 mode 3 路径
- **不**动 `docs/versions/v0-X-Y/` 历史归档(per CLAUDE.md §五)
- 不动用户面 docs/{quickstart,user-manual,recipes,troubleshooting}.md(那是 F127 manual-prover 的活)

**Acceptance**:
- baseline ≥ 1283 / 1(改 docs 不应改 test count)
- clippy clean
- prd.md §F125 §2 drift grep 全 0 命中
- `head -3 README.md` 全英文 + `grep -c '[\\u4e00-\\u9fff]' README.md` = 0
- `grep 'root README.md MUST be English' CLAUDE.md` 命中

---

## Wave 1 集成(主 session,~30 min)

- 4 PR 同时 review(顺序无所谓 — 互不冲突)
- 全绿 → `gh pr merge --squash --delete-branch`
- 每 PR merge 后:`git pull --rebase origin main`
- 整合 baseline:`env -u HTTP_PROXY ... cargo test --workspace --locked --no-fail-fast`
- 整合 clippy:`env -u HTTP_PROXY ... cargo clippy --workspace --all-targets --locked -- -D warnings`
- 落 `docs/versions/v0-6-1/wave-1-handoff.md`(Decided / Rejected / Risks / Files / Remaining 5 段)
- TG ping milestone:`[ccteam v0.6.1|W1|cleanup+bridges] 4/4 PR merged, baseline X/Y, clippy clean`

---

## Wave 2 — User-claim 实现 + Plan/Approval(4 teammate 并行,~2h)

**目标**:Epic E + F 全实现 = user-manual.md 漂移项全有代码支撑 + plan-approval 闭环。

### Teammate W2-T1: `plan-approval` — F98

**Worktree**:`git worktree add -b v061-w2-plan-approval /tmp/ccteam-v061-w2-plan-approval origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F98
- 实现 workflow.yaml `plan_approval:` block schema(serde struct on `AgentSpec`)
- orchestrator artifact watcher 加 plan file pattern(`.ccteam/plans/<agent>-*.md`)+ 检测 → emit `plan_pending` event + 调 ccteam-imd outbox API → send IM message
- ccteam-imd inbound parse `APPROVE` / `REJECT` / `EDIT <comment>` 文本 → emit `plan_decision` event → orchestrator resume agent
- 60min timeout escalate path

**Acceptance**:
- baseline ≥ Wave 1 数字
- 新 test `plan_approval_test.rs` mock IM full loop pass
- workflow.yaml schema test pass
- 不破已有 mode dispatch

### Teammate W2-T2: `hitl` — F124 narrow scope

**Worktree**:`git worktree add -b v061-w2-hitl /tmp/ccteam-v061-w2-hitl origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F124
- workflow.yaml `mode: human-approval`(第 4 mode)— `WorkflowMode` enum 加 `HumanApproval`
- orchestrator `pick_adapter(Executor, WorkflowMode)` 加 human-approval 分支(基础用 ClaudeBg adapter,但 step done → hold)
- CLAUDE.md §三 红线表加 row `| **HITL approval state SoT** | — | progress.jsonl::plan_decision | 同 |`
- 集成测试 verify mode workflow.yaml round-trip

**注意**:F124 narrow scope = workflow.yaml mode key 接入;true HITL UX 流靠 F98(plan-approval)实现 — 两 finding 协作。

**Acceptance**:
- baseline ≥ Wave 1
- `mode: human-approval` workflow round-trip test pass
- CLAUDE.md 红线 row 入

### Teammate W2-T3: `control-ext` — F128

**Worktree**:`git worktree add -b v061-w2-control-ext /tmp/ccteam-v061-w2-control-ext origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F128
- MCP 新工具 2 个:`admin_change_persona` + `admin_add_tool`(走 `mcp_admin_tools.rs`,V0.6.0 5-group 内 admin group)
- skill `ccteam-control` 加两子命令 + LLM 提示(skill 内做 NL → md merge)
- CLI 命令 `ccteam admin change-persona <slug> <bot> "<NL>"` + `ccteam admin add-tool <slug> <bot> "<NL>"`(走 MCP path)
- 2 个新 test:`admin_change_persona_test.rs` + `admin_add_tool_test.rs`

**Acceptance**:
- baseline ≥ Wave 1
- 2 MCP tool 注册 + schema 验证
- 2 test pass
- skill `ccteam-control` SKILL.md 子命令文档化

### Teammate W2-T4: `im-nl-admin` — F129

**Worktree**:`git worktree add -b v061-w2-im-nl-admin /tmp/ccteam-v061-w2-im-nl-admin origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F129
- `crates/ccteam-imd/src/inbound.rs` 加 `@ccteam` mention 检测(在 `@<bot>` route 之前)
- 5 keyword admin action + 危险动作 confirm flow(`stop everything` 二次确认)
- 简单走 keyword match;复杂的留 Task subagent 路径
- 集成测试 `im_nl_admin_test.rs` mock 5 NL admin path + 危险 confirm flow

**Acceptance**:
- baseline ≥ Wave 1
- new test pass + mock TG inbound 5 admin path 验证

---

## Wave 2 集成(主 session,~45 min)

- 4 PR review;**plan-approval + hitl 有联动 risk**(F98 + F124 同改 orchestrator state machine);先 merge plan-approval,rebase hitl,review,merge
- 4 PR 全绿 → squash merge
- 落 `docs/versions/v0-6-1/wave-2-handoff.md`
- TG ping milestone

---

## Wave 3 — Verification + ship(3 teammate 并行 + 主 session 整合,~1.5h)

**目标**:F127 manual-prover 跑通 + 5 demo GIF 录 + tier-1 doc 最终同步 → version bump + tag + push。

### Teammate W3-T1: `manual-prover` — F127(扩展为端到端用户操作模拟)

**Worktree**:`git worktree add -b v061-w3-manual-prover /tmp/ccteam-v061-w3-manual-prover origin/main`

**重要 ship policy(用户 2026-05-19 拍板)**:V0.6.1 在 E2E sim 100% clean 之前**不 tag v0.6.1**;sim 发现 bug **在同版本修**(no V0.6.2 bump,no V0.7 split),iterative sim-fix-resim 直到清。

**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F127(含用户额外要求段)
- 逐节扫 `docs/user-manual.md` + `docs/quickstart.md` + `docs/troubleshooting.md` + `docs/recipes.md` + `docs/advanced/*.md`
- 每 verifiable claim 起一行 `.audit/manual-prover-report.md` + 验证命令 + status
- claim 未实现 → fix in 本 wave;claim 实现差 → 直接 fix;claim 写错 → 改 user-manual.md
- `/ccteam-doctor` claim 在 troubleshooting.md L1 — 改成 `ccteam doctor` CLI 或新增 `/ccteam-doctor` slash skill(简单 wrapper around `ccteam-control` skill)

**E2E sim scope(扩,逐 preset 完整 user journey)**:
1. **Solo Sidekick**:本机 Claude session 跑 `/ccteam "扫 TODO"` → verify agent done + 结果回主 session
2. **Team Sprint**:`/ccteam-team 3 "<task>"` → 3 teammate 起 + fleetview 可见 + 完成
3. **Overnight Builder**:`/ccteam-creator "夜里跑 qa-loop"` → daemon spawn + cost cap auto-pause verify
4. **Pocket Assistant**:`/ccteam-im-setup` + `/ccteam-creator "TG 助理 bot"` → TG DM round-trip + `/ccteam-control change-persona` + `add-tool` 改活 bot
5. **IM Squad**:multi-bot TG group + `@ccteam pause/resume/list bots/cost today/stop everything` NL admin
6. **Plan-approval flow**:workflow.yaml `plan_approval:` agent 写 plan → TG message → `APPROVE` → resume
7. **Codex paths**:`/ccteam-advise` + auto-critic + opt-in fallback
8. **HITL mode**:`mode: human-approval` workflow round-trip

发现的 bug **iterative fix in 本 wave**(可能多轮):
- 简单 bug(typo / NL 路由 / event format)直接 commit 本 worktree
- 中等 bug(orchestrator state / adapter)如 in scope of 已 ship F# → fix in 本 PR;否则 SendMessage team-lead 决策(临时 fix-teammate 或主 session 接)
- 严重 bug(架构层) → SendMessage team-lead + TG ping

**Acceptance**:
- `.audit/manual-prover-report.md` 每 claim 行 status ∈ {PASS,FIXED,N/A};0 FAIL
- E2E sim 8 个 path 每个出 1 行 PASS/N/A status(N/A 限于"需 user 私设 IM token 才能跑"等环境约束;真 bug 必 fix)
- baseline ≥ Wave 2 数字
- nas-box005 host probe 跑通(F119 enhanced script + plan-approval flow + HITL mode)
- **PR title 含 `[ship-gate-pass]` 后缀**,告知主 session 可以 tag v0.6.1

### Teammate W3-T2: `demo-recorder` — F123

**Worktree**:`git worktree add -b v061-w3-demo-recorder /tmp/ccteam-v061-w3-demo-recorder origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F123
- 5 GIF 录到 `docs/versions/v0-6-0/demos/`(注意:V0.6.0 dir,本版补)
- 用 `asciinema rec` → `agg --theme dracula --font-size 14 --speed 2.0` → GIF
- 每 ≤500KB,90×30
- 录 do/don't:不漏 TG token,不录真 cost(用 mock workflow + fixed cost display)
- 落 `docs/versions/v0-6-0/demos/README.md` recipe

**Acceptance**:
- 5 GIF 存在 + ≤500KB
- root README + quickstart.md 引用全 valid
- demos/README.md 可复刻

### Teammate W3-T3: `doc-syncer` — F125 finalize + tier-1 doc 同步

**Worktree**:`git worktree add -b v061-w3-doc-syncer /tmp/ccteam-v061-w3-doc-syncer origin/main`
**Briefing**:
- 读 `docs/versions/v0-6-1/prd.md` §F125 §4 CLAUDE.md baseline 同步
- CLAUDE.md §一 表格:workspace version → 0.6.1 / baseline → 本版数字 / 当前最新版 → V0.6.1 + F98 + F119-F129 / V0.6.x delayed → V0.7 候选(国内 IM / chat memory sync / monorepo `.mcp.json` / migrate-from-claude / 6 号 mode 深化)
- 同步 `docs/tech-design.md`(加 plan-approval flow 节 / F128 admin 工具 / F129 IM NL admin)
- 同步 `docs/interfaces.md`(MCP 工具 27→30 + workflow.yaml `mode: human-approval` + `plan_approval:` block)
- 同步 `docs/dev-coupling-audit.md`(F98 + F119-F129 各 1 行索引)
- 同步 `docs/claude-code-tool-surface.md`(若 30 工具影响)
- 落 `docs/versions/v0-6-1/wave-3-handoff.md`(Decided / Rejected / Risks / Files / Remaining)

**Acceptance**:
- baseline ≥ Wave 2
- F125 drift grep 全 0
- CLAUDE.md baseline / version 数字 sync
- dev-coupling-audit 加 11 行(F98 + F119-F129)

---

## Wave 3 集成 + ship(主 session,~1h)

1. 3 PR 顺序 review/merge(顺序无所谓 — 各动各 file)
2. integration baseline:`env -u HTTP_PROXY ... cargo test --workspace --locked --no-fail-fast`
3. integration clippy:`env -u HTTP_PROXY ... cargo clippy --workspace --all-targets --locked -- -D warnings`
4. `workspace.package.version` bump `0.6.0 → 0.6.1`(`Cargo.toml`)
5. `cargo build --workspace --release`(regenerate Cargo.lock)
6. nas-box005 deploy + full host probe(scripts/host-probe/deploy-to-nas.sh + run-probes.sh)
7. commit: `v0.6.1: F98 + F119-F129 (12 findings) — cleanup + user-claim + plan-approval`
8. `git tag v0.6.1 && git push origin main v0.6.1`
9. update root README "Status" section 最终 V0.6.1 ship date + next candidate findings
10. TG `@web3op_bot` ping ship done:`[ccteam v0.6.1] shipped 🎉 — F98 + F119-F129 (12 findings)`

---

## Critical reminders for teammates(briefing 必含)

每 teammate briefing 必包含这 5 行(从 V0.6 实战累积):

1. **HTTP_PROXY env 必 strip**:每次 `cargo test/clippy` 必加 `env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy`(否则 64 ccteam-web 测试假阳性 502)
2. **`isolation: worktree` 你的 worktree 必须在 `/tmp/ccteam-<name>/` 路径**(默认 worktree 自动这样;如发现 commit 落在 main 工作树,STOP + ping team-lead 救援)
3. **完成必跑 `git push -u origin <branch>`**(不自动)
4. **遇 Anthropic API backoff `Sautéed`** → 不放弃,wait 然后 retry,只 ping team-lead 如 >15min
5. **不动 v0-X-Y/ 历史归档**(per CLAUDE.md §五 Pre-v1.0 不留技术债 / EOL 内容去版本 dir)

---

## Estimated wall time

| Wave | 并行度 | Wall time(主时间 vs 端到端)|
|---|---|---|
| 0 doc-first | 1(main)| 30 min |
| 1 cleanup | 4 teammate parallel | 1h(teammate)+ 30 min(integration)= 1.5h |
| 2 user-claim + plan | 4 teammate parallel | 2h(teammate)+ 45 min(integration)= 2.75h |
| 3 verification + ship | 3 teammate parallel | 1.5h(teammate)+ 1h(integration + ship)= 2.5h |
| **Total** | — | **~7h end-to-end**(主 session 实际 active ~3.5h,其余 teammate 自跑)|

User 离开期间:Wave 1 + Wave 2 全程 + Wave 3 大部分自动跑;主 session 只在 wave 集成 + integration baseline 时活跃。

---

## Ping policy(沿用 V0.6)

TG `@web3op_bot` 推送:
- **ping**:每 wave PR all merged ✓ / final ship ✓ / 硬 blocker(baseline regression >30min 修不了 / PR auto-merge 拒 / 决不了的架构决策)
- **不 ping**:每 teammate done / 编译错 / clippy warning / merge conflict(plumbing 自解)
- format:`[ccteam v0.6.1|W<N>|<finding>] <one-line>` ≤300 chars

---

详 `prd.md` 各 finding;TeamCreate briefing 用本文件每 teammate section 内容拼出。
