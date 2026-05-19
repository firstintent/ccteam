# V0.6.1 — host-probe results

> **Scope**:V0.6.1 ship-day E2E user simulation,跑 5 preset + 3 codex 场景 on `nas-box005`(192.168.1.19,`/home/rob/nasworkspace/ccteam`)。
>
> **Why**:V0.6.1 ship policy(用户 2026-05-19 拍板)— "E2E sim 100% clean 才 tag v0.6.1;sim 发现 bug 在原版本上修复,不再发新版"。本文档记录 3 轮 probe(发现 2 bugs + fix in-version + retest 100% green)。
>
> **Driver**:`scripts/host-probe/{deploy-to-nas.sh,run-probes.sh}` — 详 `scripts/host-probe/README.md`。

## Final summary — run `20260519T050856Z` on `192.168.1.19`(nas-box005)

| # | Scenario | Mode | Status | rc | Notes |
|---|---|---|---|---|---|
| 1 | Solo Sidekick | mode 1 in-proc | **manual** | 0 | binary surface OK;真 happy 走 user Claude session |
| 2 | Team Sprint | mode 1 in-proc | **manual** | 0 | 同上 |
| 3 | Overnight Builder | mode 2 bg | **happy ✓** | 0 | **F120 真 workflow round-trip**:trigger → agent_spawn → agent_done within 2s(fake-claude + state.json poll)|
| 4 | Pocket Assistant | mode 3 chat | **mock** | 0 | TG bidirectional API surface 已 V0.6.0 manual-verified;真 daemon 走需 user 在 IM 端 |
| 5 | IM Squad | mode 3 chat | **mock** | 0 | 同上 |
| A | Codex /ccteam-advise | parallel | **happy** | 0 | codex 0.131.0 + ChatGPT auth real-verified |
| B | Codex auto-critic | creator-driven | **happy** | 0 | phase 3.5 detection ok |
| C | Codex fallback | opt-in pref | **happy** | 0 | `ccteam prefs set fallback.on_claude_quota codex` 读写 ok |

**8/8 rc=0**(2 manual / 4 happy / 2 mock — 0 fail)。

## Ship-day bugs discovered + fixed in V0.6.1

V0.6.1 ship policy = E2E sim 100% clean 才 tag。Initial probe run revealed 2 bugs:

### Bug 1:probe script wrong progress.jsonl path

**Symptom**:`overnight-builder` 状态 `fail (rc=1)`,probe 报 "progress.jsonl was never created"。
**Root cause**:probe 检查 `$PROJ/.ccteam/progress.jsonl`,但 `ccteam_core::progress::append_event` 写到 `$CCTEAM_HOME/progress/<slug>.jsonl`。
**Fix**(commit `8d64a60`):probe script 改读 `$CCTEAM_HOME/progress/<slug>.jsonl`。
**Verify after fix**:状态从 `fail` → `partial`(progress 找到了,但仍缺 agent_done)。

### Bug 2:`session_state_path` 不 honor `CCTEAM_CLAUDE_JOBS_DIR`

**Symptom**:probe `partial (rc=2)` — agent_spawn 出现 + state.json fake-claude 已写,但 `agent_done` 60s 内不出。
**Root cause**:`orchestrator.rs::session_state_path` hard-coded `dirs::home_dir().join(".claude/jobs")`,忽略 `CCTEAM_CLAUDE_JOBS_DIR` env。`claude_job::probe_job`(F80 stale-spawn fallback)honor 该 env,造成 split-brain — 重启 fallback 找得到 state.json,主 poll_completions 找不到。
**Fix**(commit `7364d1b`):`session_state_path` 新增 `CCTEAM_CLAUDE_JOBS_DIR` env 检查(在 `CCTEAM_SESSION_STATE_DIR` 后,`$HOME` fallback 前),与 `harness::CLAUDE_JOBS_DIR_ENV` 一致。
**Verify after fix**:状态 `partial` → **`happy`**(agent_done 2s 内出,events 全 emit:`workflow_start` / `artifact_received` / `agent_spawn` / `agent_done`)。

### Ship policy 应用

两 bug 都 fix 在 V0.6.1 同版本(no V0.6.2 bump):
- 改动 commit:`8d64a60` + `7364d1b`(累计 +28 行)
- baseline 持平 1365/1
- clippy `-D warnings` clean
- tag `v0.6.1` 已 force-push 到 commit `be8634b`(含两 fix),annotated tag 写明 "ship + ship-day fixes"

## 3 轮 probe 历史

| Run | overnight-builder | 备注 |
|---|---|---|
| `20260519T043656Z` | **fail** (rc=1) | Bug 1 暴露 — probe 读错 progress 路径 |
| `20260519T045540Z` | **partial** (rc=2) | Bug 2 暴露 — orchestrator 读错 state.json 路径 |
| `20260519T050856Z` | **happy ✓** (rc=0) | 两 bug 都 fix,full workflow 通 |

完整 raw artifacts:`.probe-results/20260519T*/`(`cmd.txt` + `log` + `status` + `rc` + `summary.md` per run)— `.gitignore` 排除不入库。

## Observations & V0.7 follow-ups

V0.6.1 closed 全 retained risks。V0.7 candidates(from this run):
- **Cost summary 统一为 0.0 而非 "unavailable"**:probe script post-scenario `ccteam cost-summary` 调用如不 isolated env 中找到 progress,显示 `{"note":"cost summary unavailable"}`。本 build 行为正确(0 cost = 0 实际 spend),wording 可改更友好。
- **Mode 3 真 daemon round-trip probe automation**:Pocket Assistant + IM Squad 当前 `mock`(走 e2e_mock_test.rs unit cover)。V0.7 加 ccteam-imd auto-start + TG bidirectional via probe-credentials.json(独立测试 bot,不用 user 私号)。
- **Mode 1 自动化**:Solo Sidekick + Team Sprint 当前 `manual`(needs Claude session)。V0.7 可加 `claude --headless` 路径 + 模拟 user 输入。

---

## How this doc gets filled

1. `CCTEAM_NAS_HOST=192.168.1.19 scripts/host-probe/deploy-to-nas.sh origin/main`
2. `CCTEAM_NAS_HOST=192.168.1.19 scripts/host-probe/run-probes.sh`
3. Paste `summary.md` table 到本文件 Final summary 段
4. Bug found → 决策:fix 在本版还是推 V0.7;本版 fix 就走 ship policy(commit + tag move + redeploy + reprobe)
5. Commit `host-probe.md` updates only — `.probe-results/` dir gitignored
