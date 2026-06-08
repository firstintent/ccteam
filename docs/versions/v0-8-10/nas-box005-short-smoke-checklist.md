# v0.8.10 nas-box005 Short Smoke Checklist

Status: AUTOMATED SHORT SMOKE PASSED; MANUAL HOST FAULTS PENDING.

This checklist is the required real-machine short smoke record for v0.8.10.
CI-fake green does not satisfy these boxes, and this file must not be marked
complete until the manual host-fault section is run on nas-box005.

## Preflight

- [x] `git rev-parse HEAD` is recorded:
      `b7edeeeaf64d58ecba3f2f9fa014e3e09651b58d`
- [x] `git rev-parse origin/dev` matches the commit under test:
      `b7edeeeaf64d58ecba3f2f9fa014e3e09651b58d`
- [ ] `cargo test --workspace --exclude ccteam-web` is green on the box:
      not run on nas-box005; passed on local host at the same commit.
- [ ] `cargo test -p ccteam-web` is green on the box:
      not run on nas-box005; passed on local host at the same commit.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green:
      not run on nas-box005; passed on local host at the same commit.
- [ ] `cargo fmt --all -- --check` is green:
      not run on nas-box005; passed on local host at the same commit.
- [ ] `npm run lint` in `crates/ccteam-web/web` is green:
      not run on nas-box005; passed on local host at the same commit.
- [ ] `npm run test:unit` in `crates/ccteam-web/web` is green:
      not run on nas-box005; passed on local host at the same commit.

## Automated Short Smoke

Run:

```bash
scripts/smoke-v0-8-10-real-short.sh
```

The script refuses to run unless the hostname is `nas-box005`. If
`CCTEAM_ALLOW_NON_NAS_SMOKE=1` was used, this was only a rehearsal and must not
be marked PASS here. It also refuses to run unless the worktree is clean and
`HEAD` matches `origin/dev`; `--preflight-only` can be used on nas-box005 to
check host, tools, worktree cleanliness, and commit identity before running the
fault legs.

- [x] real rmux daemon smoke passed:
      `scripts/smoke-v0-8-10-real-short.sh`, 7 passed.
- [x] real IM WebSocket dual-harness smoke passed:
      `scripts/smoke-v0-8-10-real-short.sh`.
- [x] daemon restart leg passed:
      `real_im_ws_restart_faults`.
- [x] Claude pane death produced one user-visible failure message:
      `发送失败: tmux session missing: ...`.
- [ ] Codex app-server disconnect produced one user-visible failure message:
      not run; default short smoke keeps Codex real probes opt-in.

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
Date: 2026-06-09
Commit: `b7edeeeaf64d58ecba3f2f9fa014e3e09651b58d`
Notes: Automated script PASS on nas-box005. Manual host suspend/netdrop and
no-silent-failure checks remain pending; do not treat as final tag-ready yet.
