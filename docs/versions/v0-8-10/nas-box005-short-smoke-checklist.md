# v0.8.10 nas-box005 Short Smoke Checklist

Status: SPECIAL MACHINE PENDING.

This checklist is the required real-machine short smoke record for v0.8.10.
CI-fake green does not satisfy these boxes, and this file must not be marked
complete until it is run on nas-box005.

## Preflight

- [ ] `git rev-parse HEAD` is recorded:
- [ ] `git rev-parse origin/dev` matches the commit under test:
- [ ] `cargo test --workspace --exclude ccteam-web` is green on the box:
- [ ] `cargo test -p ccteam-web` is green on the box:
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green:
- [ ] `cargo fmt --all -- --check` is green:
- [ ] `npm run lint` in `crates/ccteam-web/web` is green:
- [ ] `npm run test:unit` in `crates/ccteam-web/web` is green:

## Automated Short Smoke

Run:

```bash
scripts/smoke-v0-8-10-real-short.sh
```

- [ ] real rmux daemon smoke passed:
- [ ] real IM WebSocket dual-harness smoke passed:
- [ ] daemon restart leg passed:
- [ ] Claude pane death produced one user-visible failure message:
- [ ] Codex app-server disconnect produced one user-visible failure message:

## Manual Host Faults

Use the same checked-out commit and keep the smoke logs under
`${TMPDIR:-/tmp}/ccteam-v0-8-10-real-short`.

- [ ] Host suspend/resume: suspend nas-box005 during an active Claude turn,
      resume it, and observe recovery within 30s without duplicate answers.
- [ ] Network drop: block outbound network during an active IM/web turn,
      restore it, and observe exactly-once delivery or one clear failure
      message with a retry next step.
- [ ] Restart after fault: restart the daemon after the previous two checks
      and confirm `/sessions` still shows the same sid values.
- [ ] No silent failure: for every injected fault above, IM or active web SSE
      showed a human-readable message; no turn was left with zero visible
      output and zero failure message.

## Result

- [ ] PASS
- [ ] FAIL

Operator:
Date:
Commit:
Notes:
