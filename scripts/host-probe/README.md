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
