# V0.6.0 — demo GIFs

Five preset demos for the V0.6.0 README hero / docs site / Discord announcement.
Each GIF is a **scripted reenactment** of the user-visible behaviour — recorded
deterministically so we can re-record on any host without a real Claude
session, real Telegram account, or real cost charges. The underlying code paths
that these scripts reenact are covered by integration tests
(`crates/ccteam-imd/tests/e2e_mock_test.rs`, `crates/ccteam-core/tests/orchestrator_thin_test.rs`).

| File | Mode | Scenario | Size |
|---|---|---|---|
| `30s-solo-sidekick.gif`     | 1 (in-proc)         | `/ccteam "扫 TODO"` — single Task subagent over an existing session.                  | ~57 KB |
| `30s-team-sprint.gif`       | 1 (in-proc fan-out) | `/ccteam-team 3 "fix TS errors"` — 3 parallel Task subagents merging back.            | ~65 KB |
| `60s-overnight-builder.gif` | 2 (bg)              | `/ccteam-creator "夜里跑 qa-loop"` — daemon-spawned workflow + TG push on done.        | ~117 KB |
| `30s-tg-bot-team.gif`       | 3 (tmux IM)         | Pocket Assistant — TG DM round-trip, the GIF the root `README.md` hero links to.       | ~44 KB |
| `60s-im-squad.gif`          | 3 (tmux IM)         | IM Squad — 2 bots in a TG group, `@`-chain escalation (critic → fixer → user).         | ~142 KB |

All files are ≤500 KB (GitHub README cold-load budget), 90×30 chars, dracula
theme, 14pt mono, 2.0× playback.

## Re-record from scratch

```bash
# 1. Install pipeline (one-time, no sudo needed):
pip install --user asciinema                              # → ~/.local/bin/asciinema
cargo install --git https://github.com/asciinema/agg      # → ~/.cargo/bin/agg

# 2. Re-record any single demo:
cd docs/versions/v0-6-0/demos
asciinema rec --cols 90 --rows 30 --overwrite \
    --command "bash ./scripts/01-solo-sidekick.sh" /tmp/solo.cast
agg --theme dracula --font-size 14 --speed 2.0 \
    /tmp/solo.cast 30s-solo-sidekick.gif

# 3. Re-record all five at once:
for spec in \
    "01-solo-sidekick.sh:30s-solo-sidekick.gif" \
    "02-team-sprint.sh:30s-team-sprint.gif" \
    "03-overnight-builder.sh:60s-overnight-builder.gif" \
    "04-tg-bot-team.sh:30s-tg-bot-team.gif" \
    "05-im-squad.sh:60s-im-squad.gif"; do
    s="${spec%:*}"; g="${spec##*:}"; c="/tmp/${g%.gif}.cast"
    asciinema rec --cols 90 --rows 30 --overwrite --command "bash ./scripts/$s" "$c"
    agg --theme dracula --font-size 14 --speed 2.0 "$c" "$g"
done
```

Each `scripts/*.sh` source-includes `_lib.sh` (cursor-hide, ANSI palette,
slow-type helper). To tweak pacing edit the `pause N.N` calls in-place.

## Recording do / don't

**Don't**
- Leak a real Telegram bot token: the scripts use `@assistant_demo_bot` and
  `@critic_bot` / `@fixer_bot` placeholders. Never replace with a token from
  your own dev account, even for "just one re-record".
- Show a real cost number: all dollar figures in the scripts are mocked
  (`$0.012`, `$0.74/$5`). Never paste a real `/cost` line into the recording —
  it may reveal account-level pricing you're under NDA for.
- Use a system-themed terminal (Powerlevel10k prompt, custom font, etc.).
  Scripts assume a vanilla 90×30 dracula palette so the GIF looks the same on
  every host.

**Do**
- Hide the cursor before recording (`_lib.sh::hide_cursor` does this with an
  `EXIT` trap that restores it). Cursor blink eats GIF frames for no reason.
- Re-record into `/tmp/*.cast` first, eyeball with `agg ... /tmp/preview.gif`,
  then commit the GIF only. The intermediate `.cast` is not checked in.
- Keep GIFs ≤500 KB. If `agg` output exceeds, drop `--font-size` to 12, raise
  `--speed` to 2.5, or trim long `pause` calls in the script.

## Why scripted reenactments instead of live recordings

The V0.6.0 PRD names these GIFs as the marketing artifact for the V0.6.0
announcement, **not** as a test gate (behavioural verification lives in the
host-probe + integration tests at `docs/versions/v0-6-0/host-probe.md`). A
scripted reenactment is:

1. **Reproducible** — anyone can re-record on any host, no Anthropic auth, no
   real Telegram account, no real $ spend.
2. **Token-stable** — no risk of accidentally embedding an API key, bot token,
   or internal hostname in a GIF that ships to GitHub.
3. **Pacing-controlled** — `pause N.N` calls give us deterministic frame
   spacing; live recording adds 20–40% bloat from idle frames.

When the underlying CLI/IM output format changes, edit the matching
`scripts/NN-*.sh` to match the new look and re-record. The scripts double as
ASCII specs for what each preset's UX should feel like.

## Where these GIFs are referenced

- `README.md` (root) line 5 — hero image points at `30s-tg-bot-team.gif`.
- `docs/quickstart.md` — preset entry-points may link individual GIFs.

If you rename a GIF, grep both files and update the link in the same PR.

## Commit history

- V0.6.0 ship: directory created, `.gitkeep` placeholder, recording deferred.
- V0.6.1 W3 (F123): five GIFs + scripts landed; `.gitkeep` removed.
