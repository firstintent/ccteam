# V0.6.0 — demo GIFs (placeholder)

The V0.6.0 README references five 30-second demo GIFs:

| File | Scenario |
|---|---|
| `30s-solo-sidekick.gif`     | Preset 1 — single in-proc Task subagent over `/ccteam`. |
| `30s-team-sprint.gif`       | Preset 2 — `/ccteam-team 3 "..."` 3 parallel teammates. |
| `30s-overnight-builder.gif` | Preset 3 — ccteam-creator → mode-2 bg daemon + artifact relay. |
| `30s-tg-bot-team.gif`       | Preset 4 — Pocket Assistant (TG DM round-trip via claude TUI). |
| `30s-im-squad.gif`          | Preset 5 — IM Squad (TG group, 2 bots, `@`-routing, hop escalate). |

## Status

**Deferred to V0.6.1 post-ship.** Recording each GIF needs:

1. a real Claude Code session
2. a real Telegram account for presets 4–5
3. an asciinema → gif pipeline (`asciinema rec` + `agg` /
   `terminalizer`) or OBS screen capture

Wave-4 ship gate accepts a placeholder here because:

- the underlying code paths are unit-tested
  (`crates/ccteam-imd/tests/e2e_mock_test.rs`,
  `crates/ccteam-core/tests/orchestrator_thin_test.rs`)
- host-probe text logs + cost snapshots
  (`docs/v0-6-0/host-probe.md`) capture the same evidence in less
  bandwidth
- recording GIFs would block ship by ~1 wall-clock day for
  cosmetics, with no behavioural delta

## How to record (post-ship)

Suggested setup:

```bash
# Solo sidekick (Claude session inside a tmux pane, 100×30)
asciinema rec /tmp/solo.cast --command "claude"
# inside claude, run: /ccteam "fix the ts errors in src/foo.ts"
# stop with ctrl-d after the agent finishes
agg --speed 1.5 --font-size 18 /tmp/solo.cast \
    docs/v0-6-0/demos/30s-solo-sidekick.gif
```

Target ≤30 s per GIF; trim with `gifsicle -O3` afterwards.

## Why GIFs at all

The V0.6.0 PRD names "5-minute-to-first-IM-bot" as Epic A's
acceptance criterion. The 5 GIFs serve as the marketing artifact
for the V0.6.0 announcement (Discord / docs site / README hero),
not as a test gate.

## Commit

`.gitkeep` keeps this directory in tree until the real GIFs land.
