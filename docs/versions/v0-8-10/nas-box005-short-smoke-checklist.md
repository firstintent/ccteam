# v0.8.10 Target-Host Short Smoke Checklist

Status: LOCAL TARGET-HOST SHORT SMOKE PASSED ON `rob-ws`.

This checklist is the required real-machine short smoke record for v0.8.10.
CI-fake green does not satisfy these boxes, and this file must not be marked
complete until the host-fault section is run on the selected target host.

Latest user direction moved this checklist from the original `nas-box005`
target to the local workstation. This record is therefore for:

- Host: `rob-ws`
- Scope: real rmux + real Claude IM/WS smoke on local hardware.
- Host fault: `SIGSTOP`/`SIGCONT` freezes the daemon test process for 600s.
- Netdrop equivalent: WebSocket client disconnect/reconnect with backlog
  replay exactly once.
- Not claimed: full ACPI system suspend, RTC wake, or system-level outbound
  network blocking.

## Preflight

- [x] `git rev-parse HEAD` is recorded:
      `e38ff81425ffafc9b75f2df0820ba92eb481da18`
- [x] `git rev-parse origin/dev` matches the commit under test:
      `e38ff81425ffafc9b75f2df0820ba92eb481da18`
- [x] `cargo test --workspace --exclude ccteam-web` is green on the box:
      `1920 passed, 19 ignored`.
- [x] `cargo test -p ccteam-web` is green on the box:
      `276 passed`.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` is green:
      0 issues.
- [x] `cargo fmt --all -- --check` is green.
- [x] `npm run lint` in `crates/ccteam-web/web` is green:
      0 warnings/errors.
- [x] `npm run test:unit` in `crates/ccteam-web/web` is green:
      `142 passed`.
- [x] `npm test` in `crates/ccteam-web/web` is green:
      `4 passed`.

## Automated Short Smoke

Run:

```bash
CCTEAM_REAL_SMOKE_HOST=rob-ws \
scripts/smoke-v0-8-10-real-short.sh
```

The script refuses to run unless the hostname matches `CCTEAM_REAL_SMOKE_HOST`
(default `nas-box005`). `CCTEAM_ALLOW_NON_NAS_SMOKE=1` is only a rehearsal and
must not be marked PASS here. It also refuses to run unless the worktree is
clean and `HEAD` matches `origin/dev`; `--preflight-only` can be used to check
host, tools, worktree cleanliness, and commit identity before running the fault
legs.

- [x] real rmux daemon smoke passed:
      `scripts/smoke-v0-8-10-real-short.sh`, 7 passed.
- [x] real IM WebSocket dual-harness smoke passed:
      `real_ws_dual_harness_smoke`, 1 passed in 616.66s.
- [x] daemon restart leg passed:
      `real_im_ws_restart_faults`.
- [x] local host freeze/resume leg passed:
      daemon test process was stopped for 600s with `SIGSTOP` and resumed with
      `SIGCONT`; `CCTEAM-CLAUDE-WS-SIGSTOP-OK` was delivered exactly once within
      the 30s recovery window.
- [x] WebSocket disconnect/reconnect leg passed:
      disconnected active WS client, injected backlog message while offline,
      reconnected, and observed `CCTEAM-WS-NETDROP-OK` exactly once.
- [x] Restart after local host faults passed:
      daemon restart after SIGSTOP + WS reconnect restored the same Claude sid.
- [x] Claude pane death produced one user-visible failure message:
      `发送失败: tmux session missing: ...`.
- N/A, Codex app-server disconnect:
      not run; default short smoke keeps Codex real probes opt-in/best-effort.

## Host Fault Scope

Use the same checked-out commit and keep the smoke logs under
`${TMPDIR:-/tmp}/ccteam-v0-8-10-real-short`.

- [x] Host suspend/resume local equivalent: `SIGSTOP`/`SIGCONT` of the daemon
      test process during an active Claude turn; recovery within 30s without
      duplicate answer.
- [x] Network drop local equivalent: WebSocket client disconnect/reconnect;
      offline backlog replayed exactly once.
- [x] Restart after fault: daemon restarted after the local host-fault legs and
      `/sessions` still showed the same sid value.
- [x] No silent failure: the local injected faults either recovered visibly
      exactly once or produced the expected human-readable failure message.
- N/A, full ACPI suspend / system-level outbound network block:
      not run on this local checklist; latest user direction chose local
      execution instead of nas-box005 host-level operations.

## Result

- [x] PASS
- [ ] FAIL

Operator: Codex
Date: 2026-06-09
Commit: `e38ff81425ffafc9b75f2df0820ba92eb481da18`
Notes: Local target-host smoke PASS on `rob-ws`. Evidence log directory:
`/tmp/ccteam-v0-8-10-real-short`. This record does not claim full ACPI suspend
or system-level outbound network blocking.
