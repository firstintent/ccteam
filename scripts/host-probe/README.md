# scripts/host-probe/

V0.6.0 Wave 4 host-probe tooling. Drives the 5 preset E2E + 3 Codex
scenarios against the real probe host (`nas-box005`,
192.168.1.19, `/home/rob/nasworkspace/ccteam`) so the V0.6.0 ship
isn't a "cargo test pass only" paper release.

## Files

| Path                | Role |
|---------------------|------|
| `deploy-to-nas.sh`  | rsync-equivalent: `git fetch` + `git reset --hard <ref>` on the remote, then `cargo build --release`. Idempotent; safe to re-run before each probe sweep. |
| `run-probes.sh`     | Executes the 5 preset + 3 Codex scenarios over ssh, writes per-scenario `cmd.txt`, `log`, `cost.txt`, `status` into `./.probe-results/<UTC>/`, and produces a `summary.md` that the operator pastes back into `docs/v0-6-0/host-probe.md`. |
| `README.md`         | This file. |

## Typical operator flow

```bash
# 1. Deploy current main to the probe host
scripts/host-probe/deploy-to-nas.sh origin/main

# 2. Run the full sweep (mock TG, no real telegram messages sent)
scripts/host-probe/run-probes.sh

# 3. Or just one scenario:
scripts/host-probe/run-probes.sh codex-advise

# 4. Real TG e2e (user must have pasted credentials.json + /start'd the bot)
CCTEAM_PROBE_REAL_TG=1 \
    scripts/host-probe/run-probes.sh pocket-assistant im-squad
```

## Env knobs

- `CCTEAM_NAS_HOST` — ssh alias (default `nas-box005`).
- `CCTEAM_NAS_PATH` — remote ccteam checkout (default
  `/home/rob/nasworkspace/ccteam`).
- `CCTEAM_NAS_WIPE_HOME` — when `1`, `deploy-to-nas.sh` does
  `rm -rf ~/.ccteam` on the remote first (user-confirmed clean slate).
- `CCTEAM_PROBE_OUT_DIR` — local output dir (default
  `./.probe-results/<UTC-timestamp>`).
- `CCTEAM_PROBE_REAL_TG` — when `1`, `pocket-assistant` /
  `im-squad` use the real Telegram channel instead of the mock
  channel. Default is mock — wave-4 ship gate accepts mock for
  Telegram scenarios per the wave-4 handoff.
- `CCTEAM_PROBE_LOCAL` (V0.6.1 F119) — when `1`, scenarios run
  locally (no SSH; `NAS_PATH` is interpreted as the repo root).
  Used for PR-review dry-run of script changes without touching
  the NAS.
- `CCTEAM_PROBE_SKIP_DAEMON_START` (V0.6.1 F119) — when `1`, the
  `pocket-assistant` / `im-squad` scenarios skip the new
  daemon-spawn + health-wait + stop block and assume the caller
  has a `ccteam-imd` daemon already running. Default is `0`
  (probe owns lifecycle).

## V0.6.1 F119 — daemon lifecycle in mode-3 probes

`pocket-assistant` and `im-squad` now own the `ccteam-imd` daemon
end-to-end:

1. `nohup ./target/release/ccteam-imd run --tick-seconds 2 &` →
   stash pid + stderr to `/tmp/ccteam-imd-probe-<scenario>.{pid,stderr}`.
2. `./target/release/ccteam-imd health --timeout-seconds 30 --poll-ms 200`
   blocks until the daemon writes a heartbeat with `mtime ≥` the
   moment the health check started (rejects stale heartbeats from
   prior runs).
3. Run the scenario's bot interactions.
4. `kill -TERM <pid>`; wait 5s for graceful exit; `kill -KILL` if
   still alive; tail stderr into the scenario log.

`run-probes.sh` then `scp`s `/tmp/ccteam-imd-probe-<scenario>.stderr`
back into `<out>/<scenario>/daemon-stderr.log` so post-mortems are
self-contained.

## V0.6.1 F120 — overnight-builder real workflow

The `overnight-builder` scenario is no longer a `ccteam --help`
smoke. It now:

1. Scaffolds `/tmp/host-probe-overnight/` with an isolated
   `CCTEAM_HOME` + `CCTEAM_PROJECTS_ROOT`, a 1-agent
   artifact-driven `workflow.yaml`, and `.claude/agents/worker.md`.
2. Plants a stub `claude` binary (via `CCTEAM_CLAUDE_BIN`) that
   prints `backgrounded · <id>` + writes a synthetic
   `state.json`, so the orchestrator's `poll_completions` emits
   `agent_done` without burning real LLM cost.
3. Backgrounds `ccteam start --no-web --tick-seconds 1`, drops a
   trigger marker, polls `progress.jsonl` for both `agent_spawn`
   and `agent_done` (60s deadline).
4. Exit codes: `0` = both events, `2` = spawn only (partial), `1` =
   neither (orchestrator didn't pick up the trigger). The wrapper
   marks status `happy` / `partial` / `fail` accordingly.

## Output layout

```
.probe-results/2026-05-19T17-23-00Z/
├── summary.md
├── solo-sidekick/
│   ├── cmd.txt
│   ├── cost.txt
│   ├── log
│   ├── rc
│   └── status   # happy | mock | manual | skip | real
├── team-sprint/
├── overnight-builder/
├── pocket-assistant/
├── im-squad/
├── codex-advise/
├── codex-auto-critic/
└── codex-fallback/
```

## How probes are classified

- **happy** — full automated e2e ran ok on remote (any Codex
  scenario where `codex` binary + auth are present).
- **mock** — the scenario exercises the mock channel / mock
  artifact path that already has unit coverage; the host-probe
  smoke checks the binary boots and the entrypoint is wired.
- **manual** — the scenario requires a user-driven Claude session
  (`/ccteam`, `/ccteam-team`, `/ccteam-creator`) and can't be fully
  automated from ssh; the script only verifies the binary boots.
- **real** — real Telegram round-trip (only when
  `CCTEAM_PROBE_REAL_TG=1`).
- **skip** — prerequisite missing (e.g. `codex` not installed).

Per the wave-4 handoff: `mock` / `manual` / `skip` do **not** fail
the ship gate for the Telegram and user-driven scenarios. Real Codex
probes A/B/C are the must-be-real subset.

## Pre-flight checklist

Run on a fresh probe host before the first deploy:

- [ ] ssh alias `nas-box005` resolves
- [ ] remote has `git`, `rustup`, `cargo`, `claude >= 2.1.x`
- [ ] remote has `codex >= 0.131.0` + ChatGPT auth (for Codex
      scenarios)
- [ ] `~/.ccteam/im/credentials.json` exists with mode 0600 if
      `CCTEAM_PROBE_REAL_TG=1`

## Co-ordination with V0.6.1

GIF recording of the 8 scenarios is deferred to V0.6.1 post-ship —
see `docs/v0-6-0/demos/README.md`. The probe scripts here capture
text logs + cost snapshots that are sufficient for the V0.6.0
ship-readiness gate; visual demos can be re-shot off the same
scripts later.
