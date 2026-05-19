# V0.6.1 Wave 1 — Handoff

> **Status**: Shipped 2026-05-19 via 4 squash-merge to main(commits `cd958c4` + `2016b8c` + `2f6d75f` + `17ecba4`).
> **Baseline**: **1296 / 1**(V0.6.0 baseline 1283/1 + 13 net new tests:F121 +4 + F122 +5 + F119 +4)。1 fail = pre-existing SSE flake `workflow_summary_reflects_agent_spawn_and_done_events`(CLAUDE.md §一 已挂账)。
> **Clippy**: 0 errors,0 warnings(`-D warnings` clean — V0.6.0 ship 红线持续守)。
> **Wall time**: 4 teammate 并行 worktree ~25 min;主 session 整合 + 4 PR review/merge ~10 min。

## Decided

### F125 doc sweep + F126 EN README + CLAUDE.md 2 红线(PR #73)

- **F125 drift sweep**:`docs/interfaces.md` 3× `spawn_session` → `start_thread`(V0.5 → V0.6 API rename 漏 sweep);`docs/tech-design.md` L714 drop "flip" historical aside(F108 决策注释从过程描述改为终态描述)。全 `prd.md §F125 §2` drift greps 在 tier-1 evergreen docs 0 命中。**未动** `docs/versions/v0-X-Y/` 历史归档 + user-facing manuals(F127 manual-prover Wave 3 scope)。
- **F126 EN README**:root `README.md` EN-only rewrite(80 行),保 V0.6.0 3-mode 平等 + 5 preset + 三入口 narrative。**删 Status / V0.6.x 蓄势 段**(per 用户指令"README 始终呈现最新可用状态,版本进展放别处")。footer 一行 `See docs/versions/v0-6-1/README.md for release notes`。dropped broken `docs/architecture/` link。
- **F126 CLAUDE.md §三 2 红线 row**:
  - `| **root README.md MUST be English** | 守 | 守 | 守 |`
  - `| **README.md 不含版本进展/状态信息** | 守 | 守 | 守 |`

  下方加解释段:版本进展 / `V0.x.y in production` / shipped 日期 / baseline / candidate finding 全去 `docs/versions/v0-X-Y/README.md`;F-finding 索引去 `dev-coupling-audit.md`。

### F121 ccteam doctor --check-pricing-version(PR #74)

- **per-vendor 2-line 报告**(`[pricing.anthropic]` + `[pricing.openai]`)+ 3-state classifier:`OK` / `warn pricing aging` / `ERROR ship needs re-pull`(180d warn / 365d error)
- **`CCTEAM_TEST_NOW=YYYY-MM-DD` env override**(deterministic test mock)
- **`ccteam doctor`(无 flag)隐式跑 pricing check**,再 append help block(让 first-time user 仍发现 explicit modes)
- **`pricing_schema_version_for(Vendor)` re-export** through `ccteam-cost` → `ccteam-core`(per-vendor schema_version 入口)
- **4 new integration tests**(`crates/ccteam-cli/tests/doctor_pricing_test.rs`):pin 3 状态分类 + env override + implicit-mode flow

### F122 CodexAppServerAdapter → progress.jsonl bridge(PR #75)

- **`ProgressBridgeCtx`** per-thread struct(`progress_path` + `role` + `sid` + `slug` + `model`)
- **`register_bridge` + `register_bridge_for_test`** — `start_thread` 写入,事件流读出。无 ctx = V0.6.0 行为不变(translation only)。
- **`turn/completed` / `turn/failed` / `error`** notifications → `progress.jsonl` `agent_done` rows tagged `vendor: codex` + `cost_usd` via `ccteam_cost::estimate_cost`
- **`cost_24h_by_vendor["codex"]` 自动 picks up** bridged rows
- **5 new tests**(`crates/ccteam-core/tests/codex_app_server_progress_bridge_test.rs`,~350 行):4 unit + 1 end-to-end UDS mock round-trip
- 闭 V0.6.0 Wave 3 D9 retained risk

### F119 + F120 probe-fix(PR #76)

- **F119 `ccteam-imd health` CLI subcommand**(`--timeout-seconds 30 --poll-ms 200`)+ `wait_for_health(started_at, timeout, poll) -> HealthResult::{Ready,Timeout}`(exit 0 ready / exit 1 timeout)
  - `started_at` 是 caller pre-spawn 抓的 `SystemTime::now()`,防 stale heartbeat 假阳性 ready
- **F119 `run-probes.sh` daemon-start block**:probe 前 `ccteam-imd start &` + `ccteam-imd health --timeout-seconds 30` + 跑完 `ccteam-imd stop` + capture daemon stderr to `.probe-results/<TS>/<scenario>/daemon-stderr.log`
- **F119 `CCTEAM_PROBE_SKIP_DAEMON_START=1` env override**(allow user-managed daemon)
- **F119 `CCTEAM_PROBE_LOCAL=1`** 本地 dry-run mode(probe-fix 用以本地 verify rc=0 + cost.txt 非 0)
- **F120 overnight-builder real probe**:create fake workflow.yaml + fake artifact → `ccteam start <slug>` + `wait_for_event agent_done --timeout 60s` + assert progress.jsonl 含 spawn+done + cleanup
- **F120 caveat**:NAS round-trip 未在本地跑(no NAS access during teammate phase);script logic 已就位,Wave 3 doc-syncer / manual-prover 在 nas-box005 跑完整验证
- **4 new tests**(daemon health + sanity)

## Rejected

- ~~probe-fix 用 `cargo fmt -- <files>`~~ — incidental sweep across ~50 unrelated files,probe-fix 自己 revert(只留实际 authored hunks)。本仓存量 fmt drift 清理仍在独立 chore PR scope。
- ~~branch 名严格按 `v061-w<N>-<finding>` 约定~~ — 3/4 teammate 自创了变体(`v0.6.1-w1-probe-fix` / `w1-f121-doctor-pricing` / `v0.6.1-w1-f122-codex-bridge`);squash-merge 后 branch name 不留痕,放过。
- ~~probe-fix 在 NAS 上跑 F120 真实 round-trip~~ — teammate 没有 NAS 访问;script 逻辑就位,Wave 3 实地跑(nas-box005)。

## Risks(待 Wave 2/3 兜)

- **Wave 1 没动 user-facing docs**(`docs/{quickstart,user-manual,recipes,troubleshooting}.md` + `docs/advanced/*`)— 那是 Wave 3 F127 manual-prover 的活,等 F128 + F129 落了再扫。
- **F120 NAS 端 round-trip 未亲跑** — script 逻辑 OK,Wave 3 doc-syncer + manual-prover 在 nas-box005 跑完整 5 preset + 3 codex probe。
- **probe-fix 报 baseline 1287/1**(本机)与整合后 1296/1(post-merge)差异 — 因为 probe-fix 基于 921f9a7 跑,F121 + F122 + F125+F126 还没 land;merge 后 PR-level baseline 各 +5 / +5 / +4 / 0 stack 起来 1296。
- **clippy 0 warnings 持续守** — V0.6.0 ship 立的 -D warnings 红线 Wave 1 全 4 PR 守住,Wave 2 + 3 必持续守。

## Files

新文件(7):
- `crates/ccteam-cli/tests/doctor_pricing_test.rs`(F121,141 行)
- `crates/ccteam-core/tests/codex_app_server_progress_bridge_test.rs`(F122,350 行)
- `docs/versions/v0-6-1/wave-1-handoff.md`(本文件)

修改:
- `README.md`(F126,EN rewrite,80 行)
- `CLAUDE.md`(F126,§三 2 红线 row + 解释段)
- `docs/{interfaces,tech-design}.md`(F125 sweep)
- `crates/ccteam-cli/src/commands.rs`(F121)
- `crates/ccteam-core/src/{lib,execution/codex_app_server}.rs`(F121 re-export + F122 bridge)
- `crates/ccteam-cost/src/lib.rs`(F121 `pricing_schema_version_for`)
- `crates/ccteam-imd/src/{lib,main,daemon}.rs` + `tests/daemon_test.rs`(F119 health)
- `scripts/host-probe/{run-probes,README}.{sh,md}`(F119+F120)

## Remaining(Wave 2 / Wave 3)

Wave 2(4 teammate 并行):
- W2-T1 plan-approval F98(workflow.yaml `plan_approval:` block + IM round-trip)
- W2-T2 hitl F124(narrow scope `mode: human-approval`,rebase on plan-approval)
- W2-T3 control-ext F128(`/ccteam-control change-persona` + `add-tool`)
- W2-T4 im-nl-admin F129(`@ccteam` IM NL admin via meta-agent)

Wave 3(3 teammate 并行 + 主 session 整合):
- W3-T1 manual-prover F127(user-manual.md 100% 亲测 sweep,post-F128/F129 land)
- W3-T2 demo-recorder F123(5 GIF asciinema → agg)
- W3-T3 doc-syncer F125 finalize + tier-1 docs sync + CLAUDE.md baseline 回填 + dev-coupling-audit F98+F119-F129
- 主 session:integration + version 0.6.0 → 0.6.1 + nas-box005 full probe + tag v0.6.1 + ping ship

V0.7 + 后续(本版不在):国内 IM(WeChat/飞书/DingTalk/QQ)+ chat memory 跨设备同步 + monorepo-aware `.mcp.json` + migrate-from-claude + 6 号编排模式深化。
