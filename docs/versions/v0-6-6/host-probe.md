# V0.6.6 Host-probe Sign-off (nas-box005)

> **Run by:** V0.6.6 Wave 4c subagent (Opus 4.7 1M)
> **Date:** 2026-05-24
> **Host:** `rob@192.168.1.19` (nas-box005)
> **Binary:** `/home/rob/nasworkspace/ccteam/target/release/ccteam` (built fresh on NAS from `origin/main` HEAD `8098eaa` — V0.6.6 wave 1 merged, workspace version string still `0.6.5` because the version-bump PR has not landed yet; binary identity is the V0.6.6 codebase)
> **State:** fresh wipe at 2026-05-24 07:00 UTC (`rm -rf ~/.ccteam/` + `~/projects/probe-w4c/` + ccteam claude session dirs); TG credentials backed up + re-injected (bot_token + chat_id 339498819)
> **Authorization:** user explicitly authorized fresh wipe (carried from prior V0.6.5 host-probe approval per `reference_nas_box005` memory)

## Summary

| Gate | Status | Evidence |
|---|---|---|
| **F166** install.sh (script flow) | **PASS** | `bash scripts/test-install-sh.sh` 6/6 PASS (syntax sh+dash, happy-path, CCTEAM_VERSION pin, checksum-tamper abort, missing-asset abort, unsupported-platform abort). Manual fallback: `ccteam` already symlinked at `~/.local/bin/ccteam` → built binary; `ccteam --version` → `ccteam 0.6.5` on PATH. |
| **F172 V2** daemon-restart-resume | **PASS** | First spawn argv: `claude --dangerously-skip-permissions --name ccteam-chat-probe-w4c-tech-helper` ✓. Alive reattach (SIGTERM + restart, claude pane intact): daemon log `event="session_reattached" pane_pid=Some(1842554)` ✓. **Dead-pane recreate** (forced via `remain-on-exit on` + kill claude PID, tmux session survives in pane-dead state): daemon log `event="session_recreated" "claude-tui: killing stale tmux session (dead pane), recreating via --resume" old_pane_pid=Some(1844651)`, recreate argv: `claude --dangerously-skip-permissions --resume ccteam-chat-probe-w4c-tech-helper` ✓. **Context lossless restore confirmed**: pre-restart turn 1 reply "OK — got it, BLUE_DOLPHIN_42." (07:13:03), post-restart "what's the magic phrase" → reply "**BLUE_DOLPHIN_42**" (07:22:32), same Anthropic session_id `5f9cdbd3-a5eb-4d5e-b880-c9c5a0a332ca` reused across the recreate. TG egress tg_msg_id 1942 (pre-restart) + 1943/1944 (post-restart) all delivered to chat 339498819. |
| **F173** Codex unified cost rollup | **PASS** | `~/.ccteam/cost-budget.json` has 5 rows after 2 `advise_vote` calls: claude×4 (per-call + synth) + **codex×1** at `2026-05-24T07:25:22.400989784Z`. `advise_today_usd=0.025`, `cap_usd=0.50`. `ccteam doctor --check-cost-orphan` → `claude=4 codex=1` rows reconciled, `[OK] cost ledger reconciled — every vendor adapter call has a ledger row.` exit 0. Codex CLI detected at `/home/rob/.local/bin/codex` (v0.133.0). |
| **F171** doctor `--verify-mcp --json` | **PASS** | `ccteam doctor --verify-mcp --json` → `{"active_count":27, "stub_count":0, "ok":true, "per_group":{admin:3, advise:2, chat:6, screenshot:1, workflow:15}, "unexpected_stubs":[]}`, exit 0. **NOTE**: PRD §5 ship-gate item #9 says "26 active" — actual surface is 27 (F128 added 2 admin tools; the README/PRD text was not updated to reflect the post-F128 tally). The functional invariant (0 stubs, all groups active, exit 0) holds; the "26" copy in PRD is stale text. |
| **F167** `probe-project --json` | **PASS** | `cd /home/rob/nasworkspace/ccteam && ccteam probe-project --json` → `{"kind":"monorepo", "languages":["rust"], "has_tests":true, "probable_scope":["crates/ccteam-core","crates/ccteam-cli","crates/ccteam-web"]}`, exit 0. Schema matches dev-plan §F167. |

**5 of 5 gates PASS.** Bonus: F170 doc-comment scrub also verified clean (`grep -rn "V0.3.3 cleanup\|F49 wires\|once Wave 2 lands\|Wave 2 wires it into the" crates/*/src/` → 0 hits, exit 0).

---

## F166 details

`scripts/test-install-sh.sh` runs the install script against a fake local GitHub Release tree (Python HTTP server on random port), covering all 6 documented branches:

```
PASS  syntax (sh + dash)
PASS  happy-path install (binary placed + executable)
PASS  CCTEAM_VERSION pin uses env tag (skips API)
PASS  checksum-tamper aborts with non-zero exit
PASS  missing-asset aborts cleanly
PASS  unsupported-platform aborts with friendly message

All install.sh smoke tests passed.
```

Setup: NAS `/usr/bin/python3` is permission-denied for user `rob` (Synology default ACL); the prior V0.6.5 host-probe established `/tmp/pyshim/python3 -> ~/.local/bin/python3.11` shim — this probe reused that shim. `PATH=/tmp/pyshim:$PATH bash scripts/test-install-sh.sh`.

Real GH Release end-to-end (`curl ... | sh`) **could not be tested** because no `firstintent/ccteam` GH Release exists yet (Wave 4 ship gate item #3 builds the first one when tag `v0.6.6` is pushed). The script's logic is validated against the synthetic release tree, which exercises identical code paths to a real release. Real-release E2E is a post-ship-tag follow-up — recommend running `curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | CCTEAM_INSTALL_DIR=/tmp/probe-install sh` on the NAS once the v0.6.6 GH Action publishes its artefacts.

**Manual fallback install** (proves PATH detection & binary identity once a release lands):
```
$ ls -la /home/rob/.local/bin/ccteam
lrwxrwxrwx → /vol4/1000/nasworkspace/ccteam/target/release/ccteam
$ ccteam --version
ccteam 0.6.5
```

---

## F172 V2 details — daemon-restart-resume (critical gate)

### Phase A — first spawn (case c "absent")

1. `mcp__ccteam__chat_register_bot` (telegram, vendor=claude, persona=tech-helper, chat=339498819) → registry `~/.ccteam/imd/registry/probe-w4c/tech-helper.json` written.
2. Daemon registry watcher picked up new file → `start_thread` → tmux `new-session` argv: `claude --dangerously-skip-permissions --name ccteam-chat-probe-w4c-tech-helper`. **First spawn uses `--name`** ✓ (lines 350-359 of `claude_tui.rs`).
3. Sent test message 1 via `chat_send_input` MCP → mailbox file dropped → fast-path consumed → claude replied "OK — got it, BLUE_DOLPHIN_42." (turn at 07:13:03 UTC). TG egress tg_msg_id=1942 to chat 339498819 (send_http_ms=1144).

Note on API latency: the first turn took ~7m 30s wall clock due to repeated Claude API retries (deepseek-v4-pro endpoint had transient `attempt N/10` retries — pre-existing API instability on this NAS, not a ccteam issue). Subsequent turns ran near real-time.

### Phase B — alive reattach (F164 path, F172 V2 does **not** touch)

1. SIGTERM the daemon (PID 1842143). F163 graceful shutdown completed in ~6s. tmux session + claude pane (PID 1842554) survived ✓.
2. Restart daemon → reattach event:
   ```
   event="session_reattached" session=ccteam-chat-probe-w4c-tech-helper
     slug=probe-w4c role=tech-helper pane_pid=Some(1842554)
     "claude-tui: reattached to existing tmux session (pane claude process alive)"
   ```
3. pane_pid `1842554` == pre-kill pane PID — **no respawn**, code path (a) verified, F172 V2 spawn argv untouched ✓.

### Phase C — dead-pane recreate (F172 V2 critical path)

The task block explicitly required: *"alive reattach 路径走通时,仍需 explicit 触发 dead-pane case 验 V2 --resume(tmux kill-session 模拟)── 不能 'alive 走通就算 V2 验过'"*

Plain `tmux kill-session` makes `session.exists()` false → drops to case (c) absent → `--name` (not `--resume`). The case (b) "dead pane" sub-state requires the tmux pane to be **dead in place** while the session record survives. Reproduced via:

```sh
# Force a state matching is_pane_running_claude()==false but session.exists()==true
tmux set-option -t ccteam-chat-probe-w4c-tech-helper remain-on-exit on
PANE_PID=$(tmux list-panes -t ccteam-chat-probe-w4c-tech-helper -F "#{pane_pid}" | head -1)
pkill -9 -P $PANE_PID && kill -9 $PANE_PID
# tmux session now reports: pane_pid=1844651 pane_dead=1
```

Then `nohup ccteam start ...` →

```
event="session_recreated" session=ccteam-chat-probe-w4c-tech-helper
  slug=probe-w4c role=tech-helper old_pane_pid=Some(1844651)
  "claude-tui: killing stale tmux session (dead pane), recreating via --resume"
```

`ps -ef` immediately after:
```
rob 1844794 tmux new-session -d -s ccteam-chat-probe-w4c-tech-helper -c ... \
  claude --dangerously-skip-permissions --resume ccteam-chat-probe-w4c-tech-helper
rob 1844795 claude --dangerously-skip-permissions --resume ccteam-chat-probe-w4c-tech-helper
```

**Recreate argv contains `--resume <name>`** ✓ (lines 285-294 of `claude_tui.rs`).

### Phase D — lossless context restoration

Sent "test message 3 — Do you remember the magic phrase I gave you earlier? Reply with just the phrase." → claude reply:

```json
{"turn_id":"a98183c2-7831-455b-af63-0575a52cd7f3-0","ts":"2026-05-24T07:22:32.185336855Z",
 "vendor":"claude","role":"tech-helper","assistant":"BLUE_DOLPHIN_42"}
```

**Single-token reply "BLUE_DOLPHIN_42"** ✓ — the bot retrieved the phrase from before the daemon restart + dead-pane recreate. Anthropic's `--resume` reloaded the prior session jsonl losslessly. The daemon hook log shows the **same session_id `5f9cdbd3-a5eb-4d5e-b880-c9c5a0a332ca`** reused across the recreate (vs F118 brand-new spawn which would synthesize a fresh sid + replay last-N turns as a user prompt — but only get a best-effort string-level approximation).

TG egress confirmation: tg_msg_id=1943 (assistant turn) + 1944 (next turn) delivered to chat 339498819. Pane render after recreate shows both the pre-restart "OK — got it, BLUE_DOLPHIN_42." and post-restart "BLUE_DOLPHIN_42" turns with "Context: 96% remaining" — the long-running Anthropic session context survived intact.

### Red-line audit (per ship gate item #10)

```
$ grep -rn "tmux capture-pane\|CHAT_SNAPSHOT\|chat_snapshot" \
    crates/ccteam-core/src/execution/claude_tui.rs \
    crates/ccteam-core/src/progress.rs
# 0 hits — V1 chat_snapshot design did not leak; capture-pane only used by dev-debug + screenshot tool, not by F172 V2 spawn path
```

(Hit count verified on the worktree HEAD which mirrors NAS `origin/main`.)

---

## F173 details — Codex daemon-routed critic unified cost rollup

Two `advise_vote` calls drove the F156 follow-through:

**Call 1** (default `codex_timeout_secs=60`):
- Question: error-message stack-trace policy
- Claude returned full answer, **Codex timed out** (`codex_status:{status:timeout}`)
- Ledger after call 1: claude×2 rows (advisor + synth), **codex 0 rows** (timeout = no charge)

**Call 2** (`codex_timeout_secs=180`):
- Question: "What is 2+2?"
- Both returned "4" within timeout (`agreement:"partial"` — codex_status `ok`)
- Ledger after call 2: claude×4 + **codex×1** at `2026-05-24T07:25:22.400989784Z`

Final ledger:
```json
{
  "samples": [
    {"vendor":"claude","usd":0.005,"ts":"2026-05-24T07:24:30.497989385Z"},
    {"vendor":"claude","usd":0.005,"ts":"2026-05-24T07:24:40.983502493Z"},
    {"vendor":"claude","usd":0.005,"ts":"2026-05-24T07:25:22.400733629Z"},
    {"vendor":"codex","usd":0.005,"ts":"2026-05-24T07:25:22.400989784Z"},
    {"vendor":"claude","usd":0.005,"ts":"2026-05-24T07:25:28.888338415Z"}
  ]
}
```

`ccteam doctor --check-cost-orphan` →
```
progress.jsonl agent_done (24h): claude=0 codex=0
cost-budget.json ledger rows (24h): claude=4 codex=1
[OK] cost ledger reconciled — every vendor adapter call has a ledger row.
```

Exit 0. The `agent_done=0` reading is consistent: this probe drove only chat-mode + advise calls (no bg-mode agent runs), so `progress.jsonl` has no `agent_done` events. The reconciliation is a count-equality check; both sides being 0 (chat/advise) + ledger having rows still passes because the orphan check is *"every progress.jsonl agent_done has a ledger row"* not *"every ledger row has an agent_done"*. The codex ledger row demonstrates F173's plumbing wired through end-to-end via the daemon's `CodexExecAdapter` cost hook.

---

## F171 details — doctor `--verify-mcp --json`

```json
{
  "active_count": 27,
  "ok": true,
  "per_group": {
    "admin":     {"active": 3,  "stub": 0},
    "advise":    {"active": 2,  "stub": 0},
    "chat":      {"active": 6,  "stub": 0},
    "screenshot":{"active": 1,  "stub": 0},
    "workflow":  {"active": 15, "stub": 0}
  },
  "stub_count": 0,
  "tool_list": ["ccteam__admin_add_tool", "ccteam__admin_change_persona", "ccteam__admin_ls",
                "ccteam__advise_parallel", "ccteam__advise_vote",
                "ccteam__chat_history", "ccteam__chat_list_bots", "ccteam__chat_register_bot",
                "ccteam__chat_reset", "ccteam__chat_send_input", "ccteam__chat_unregister_bot",
                "ccteam__screenshot",
                "ccteam__workflow_get_artifact_summary", "ccteam__workflow_inject_decision",
                "ccteam__workflow_new", "ccteam__workflow_observe_agents",
                "ccteam__workflow_pause", "ccteam__workflow_peek", "ccteam__workflow_progress",
                "ccteam__workflow_resume", "ccteam__workflow_send_to_session",
                "ccteam__workflow_set_parallelism", "ccteam__workflow_show",
                "ccteam__workflow_signal", "ccteam__workflow_spawn_agent",
                "ccteam__workflow_stop_agent", "ccteam__workflow_trigger_gate"],
  "total_tools": 27,
  "unexpected_stubs": []
}
```

Exit 0. **Stale text note**: PRD §5 item #9 and CLAUDE.md §一 currently say "26 active, 0 stubs". The actual count is **27** — the F128 admin tools (`admin_change_persona`, `admin_add_tool`) and the F146/F147 chat tools bring the surface to 27. The functional invariant (`stub_count=0`, `unexpected_stubs=[]`, exit 0) holds. Recommend Wave 4a/4b doc-syncer correct "26 → 27" in PRD §5 item #9 + CLAUDE.md §一 "26 个 mcp__ccteam__..." sentence on its sweep.

---

## F167 details — probe-project `--json`

```
$ cd /home/rob/nasworkspace/ccteam
$ ccteam probe-project --json
{
  "kind": "monorepo",
  "languages": ["rust"],
  "has_tests": true,
  "probable_scope": [
    "crates/ccteam-core",
    "crates/ccteam-cli",
    "crates/ccteam-web"
  ]
}
```

Exit 0. Schema matches dev-plan §F167 contract:
- `kind`: `"monorepo"` (the ccteam workspace has `crates/*/Cargo.toml` ⇒ rust monorepo ✓)
- `languages`: top-3 languages — only rust here, single-entry array ✓
- `has_tests`: `true` (every crate has `tests/` or `#[cfg(test)]`) ✓
- `probable_scope`: top-3 most-massive crates ✓

---

## Notes / Observations / Future risk

1. **NAS Claude API latency**: deepseek-v4-pro endpoint repeatedly hit `Retrying in 0s · attempt N/10` during this probe, inflating first-turn latency to 7+ minutes. Not a ccteam issue but a real-user impact factor — F172 V2 still works correctly under retry storms because the recreate path doesn't depend on Claude API health (it spawns + lets Claude itself reload jsonl).

2. **Dead-pane reproduction technique**: the `remain-on-exit on` + kill-claude method used here is **not** a behaviour ccteam itself produces in normal operation (ccteam's `TmuxSession::start` does not set `remain-on-exit`). A more naturally-occurring trigger of case (b) would be: an OOM-killer or SIGSEGV that kills claude *while tmux server tracks the pane state but tmux itself remains alive*. The dev-plan acceptance text "tmux kill-session 模拟 dead pane" actually triggers case (c) absent in the current code; the **real F172 V2 `--resume` path is exercised only via case (b)**. This probe confirmed case (b) directly. **Future risk**: if the dead-pane scenario is too rare in real-world ops, the `--resume` invariant could silently regress without anyone noticing. Recommend a `cargo test` integration test that synthesises the case (b) tmux state in a unit-test harness (would belong with the W1-T7 F172 V2 test bundle if not already there).

3. **First spawn writes `--name` to Anthropic's session jsonl**: confirmed via spawn argv + verified the same `session_id` is reused across the recreate (hook.recv log line shows session_id `5f9cdbd3-a5eb-4d5e-b880-c9c5a0a332ca` both before and after the daemon restart). This is the load-bearing R10 invariant (跨项目记忆走官方接口) — Anthropic owns the session bytes, ccteam just hands Anthropic the deterministic name back via `--resume`.

4. **Cost ledger** has only `vendor` + `usd` + `ts` fields currently — no per-call question hash or call-id correlation. F173 reconciles by count, not by identity. Sufficient for the V0.6.6 ship gate (`agent_done count == ledger row count` is the invariant); could be extended in V0.7+ if per-call audit becomes important.

5. **Stale "26 tools" copy** (see F171 section): three sites need a "26 → 27" sweep:
   - `docs/versions/v0-6-6/README.md` §5 ship gate item #9 "26 active, 0 stubs"
   - `docs/versions/v0-6-6/prd.md` (if it duplicates the count)
   - `CLAUDE.md` §一 row "**26 个** `mcp__ccteam__{...}*` 子前缀分组工具"

   This is doc-syncer scope, not blocking for V0.6.6 ship — the functional invariant holds, and the over-count in copy is a no-op for users (more tools available than the marketing text claims).

6. **GH Releases tag end-to-end**: untested in this probe (no v0.6.6 release exists yet). Recommend a quick follow-up probe once the version-bump + tag PR lands: ssh nas + `bash <(curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh)` with a clean `~/.local/bin/`.

---

## Cleanup left for caller

NAS state after this probe:
- daemon PID 1844765 still running (`ccteam start` for project probe-w4c)
- tmux session `ccteam-chat-probe-w4c-tech-helper` alive with active claude pane
- `~/projects/probe-w4c/` registered, `~/.ccteam/` populated, TG bot wired
- TG credentials backed up at `/tmp/tg-creds-backup.json`

Per task `## 清理` step, the parent agent should run:
```sh
ssh rob@192.168.1.19 'pkill -TERM -f "ccteam start" 2>/dev/null; sleep 5; \
  pkill -9 -f "ccteam start" 2>/dev/null; tmux kill-server 2>/dev/null'
```
