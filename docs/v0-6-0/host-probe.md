# V0.6.0 — host-probe results

> **Scope**: 5 preset E2E validations + 3 Codex scenario probes on the
> real probe host (`nas-box005`, 192.168.1.19, `/home/rob/nasworkspace/ccteam`).
>
> **Why**: V0.6.0 ships dual-vendor pricing, three execution modes
> (in-proc / bg / chat), real Telegram TUI wiring and Codex Option B.
> A pure `cargo test --workspace` pass doesn't prove the binaries
> actually compose against real `claude` / `codex` / `tmux` / a real
> TG token. This doc records what we ran on a clean host before
> tagging V0.6.0.
>
> **Driver**: `scripts/host-probe/{deploy-to-nas.sh, run-probes.sh}` —
> see `scripts/host-probe/README.md`.

## Summary table

| # | Scenario | Mode | Status | Cost (USD) | Notes |
|---|---|---|---|---|---|
| 1 | Solo Sidekick     | mode 1 in-proc | _pending_ | _-_ | user-driven Claude session probe |
| 2 | Team Sprint       | mode 1 in-proc | _pending_ | _-_ | user-driven (`/ccteam-team 3`) |
| 3 | Overnight Builder | mode 2 bg      | _pending_ | _-_ | ccteam-creator preset + daemon |
| 4 | Pocket Assistant  | mode 3 chat    | _pending_ | _-_ | TG DM e2e (mock fallback) |
| 5 | IM Squad          | mode 3 chat    | _pending_ | _-_ | TG group + bot-to-bot |
| A | Codex /ccteam-advise | parallel    | _pending_ | _-_ | must be real on remote |
| B | Codex auto-critic | creator-driven  | _pending_ | _-_ | must be real on remote |
| C | Codex fallback    | opt-in pref     | _pending_ | _-_ | must be real on remote |

> Rows are filled in after `scripts/host-probe/run-probes.sh` runs
> on `nas-box005`. The team-lead (main session) drives the actual ssh
> sweep; this file is the canonical record. Each scenario block
> below tells the operator what to paste in.

---

## Preset 1: Solo Sidekick (mode 1 in-proc)

- **Trigger**: user runs `/ccteam "<NL ask>"` inside their daily-driver
  Claude session (V0.6.0 F113 entrypoint).
- **What's exercised**: `ccteam-control` skill → Task subagent →
  in-proc agent → reply rendered in the same Claude session.
- **Driver path**: `scripts/host-probe/run-probes.sh solo-sidekick`
  only smoke-checks the binary on the remote — the real probe
  needs a human in a Claude session.
- **Expected status**: `manual` (per the wave-4 ship gate, manual
  probes count as satisfied when the unit-test coverage of the
  underlying code path is in place — see `crates/ccteam-cli/tests/`
  and the `mcp__ccteam__*` integration tests).

**Observed (fill from `.probe-results/<TS>/solo-sidekick/`)**:

- _cmd_: _paste from `cmd.txt`_
- _result_: _paste log tail_
- _wall-clock_: _-_
- _cost_: _paste from `cost.txt`_
- _findings_: _-_

---

## Preset 2: Team Sprint (mode 1 in-proc, 3 teammates)

- **Trigger**: `/ccteam-team 3 "<task>"` inside a Claude session.
- **What's exercised**: `ccteam-team` skill → 3 parallel Task
  subagents under one orchestrator → consensus / hand-off, finish.
- **Driver path**: `scripts/host-probe/run-probes.sh team-sprint`
  smoke-checks only.
- **Expected status**: `manual` (same reasoning as preset 1).

**Observed**: _-_

---

## Preset 3: Overnight Builder (mode 2 bg)

- **Trigger**: `/ccteam-creator "<long-running ask>"`.
- **What's exercised**: ccteam-creator phase 1-3 → workflow.yaml
  rendered → `ccteam` daemon spawned (`claude --bg --agent <role>`)
  → artifact relay → fix-loop budget cap → workflow_done.
- **V0.6 delta from V0.5**: only the new vendor / model fields on
  `AgentSpec` + per-vendor budget caps (F117) are net-new; the
  artifact-watcher path is unchanged.
- **Driver path**: `scripts/host-probe/run-probes.sh overnight-builder`
  smokes the daemon entrypoint.
- **Expected status**: `mock` (the workflow path is unit-tested
  end-to-end in `crates/ccteam-core/tests/orchestrator_thin_test.rs`
  and `artifact_watcher_test.rs`; an opt-in real overnight run on
  the user's dex-ui repo is a V0.6.1 follow-up).

**Observed**: _-_

---

## Preset 4: Pocket Assistant (mode 3 chat) ⭐ V0.6 flagship

- **Trigger**: `/ccteam-creator "做个 TG 私聊助理 bot…"`.
- **What's exercised**:
  1. ccteam-creator detects "TG private DM" → Pocket Assistant
     preset.
  2. `tmux` long session + `claude --resume` (TUI) per F108
     ClaudeTuiAdapter.
  3. `ccteam-imd` registers bot (`~/.ccteam/imd/registry/<slug>/<role>.json`).
  4. TG inbound → router → mailbox → `tmux send-keys -l` → claude
     processes → output hook → `turns.jsonl` mirror → ccteam-imd
     outbound tailer → `sendMessage` reply.
- **Pre-reqs**:
  - `~/.ccteam/im/credentials.json` (0600) with `@web3op_bot` +
    `chat_id 339498819`.
  - User has `/start`'d the bot.
- **Modes**:
  - `CCTEAM_PROBE_REAL_TG=1` → real TG round-trip (status `real`).
  - default → MockChannel injection (status `mock`); the wave-3
    e2e test `crates/ccteam-imd/tests/e2e_mock_test.rs` already
    proves the full pipeline at the unit-test level.
- **Driver path**:
  `CCTEAM_PROBE_REAL_TG=1 scripts/host-probe/run-probes.sh pocket-assistant`
- **Expected status**: `mock` for the default sweep; `real` once
  the user can be online to send the first message.

**Observed**: _-_

---

## Preset 5: IM Squad (mode 3 chat — group + bot-to-bot) ⭐

- **Trigger**: `/ccteam-creator "做个 TG 多 bot 团队:1 个 critic + 1 个 fixer 在群里互相 @"`.
- **What's exercised**:
  1. 2 bots register; ccteam-creator produces `workflow.yaml` with
     `chat:` topology.
  2. User sends `@critic_bot 检查这个方案` in a TG group.
  3. `ccteam-imd` parses `@` → routes to critic mailbox.
  4. critic replies `@fixer_bot ...` → `inbound.rs` increments
     `hop` field, routes to fixer.
  5. After `hop_limit` (default 4), an `escalation` event is
     emitted instead of routing further.
- **Pre-reqs**: same `credentials.json` + a TG group both bots are
  members of (group probes deferred to user op when convenient).
- **Driver path**:
  `CCTEAM_PROBE_REAL_TG=1 scripts/host-probe/run-probes.sh im-squad`
- **Expected status**: `mock` for the default sweep; `real` is
  user-driven.

**Observed**: _-_

---

## Codex A: `/ccteam-advise` parallel Claude+Codex verdict

- **Trigger**: user `/ccteam-advise "what's the best approach to X"`
  in a Claude session.
- **What's exercised**:
  - `ccteam-advise` skill calls Claude + Codex in parallel (Codex
    via the `codex exec --json` adapter F112).
  - Verdict synth — both opinions rendered + a merged
    recommendation.
  - `progress.jsonl` records `agent_done` with `vendor: codex` and
    `cost_usd > 0` (per-vendor pricing table F107).
- **Pre-reqs on `nas-box005`**:
  - `codex --version` ≥ 0.131.0
  - `codex login status` → `Logged in via ChatGPT`
- **Driver path**:
  `scripts/host-probe/run-probes.sh codex-advise`
- **Acceptance**:
  - `agent_done` event with `vendor: codex` lands in `progress.jsonl`
  - `cost_usd` non-zero
  - `model_id` matches what `SpawnCtx::model_id` was set to (the
    wave-4 D14 plumb)
- **Expected status**: `happy` (must-be-real).

**Observed**: _-_

---

## Codex B: Auto-critic in ccteam-creator

- **Trigger**: `/ccteam-creator "做个 code reviewer bot"` (critic
  persona keyword).
- **What's exercised**:
  - ccteam-creator phase 3.5 detection runs
    `codex --version && codex login status`.
  - On success, renders the role with `executor: codex` in the
    intermediate workflow.yaml.
  - User never sees the raw YAML.
- **Driver path**:
  `scripts/host-probe/run-probes.sh codex-auto-critic`
- **Acceptance**:
  - Rendered YAML (captured by the script to a temp file) contains
    `executor: codex` for the reviewer role.
  - Detection succeeds in <2s.
- **Expected status**: `happy` (must-be-real).

**Observed**: _-_

---

## Codex C: Opt-in fallback on Claude budget_exceeded

- **Trigger**: `ccteam prefs set fallback.on_claude_quota codex`
  then a Claude `budget_exceeded` event is emitted (mock-injected
  into `progress.jsonl` for the probe).
- **What's exercised**:
  - Next agent spawn switches `SpawnCtx` to Codex executor.
  - `budget_exceeded` event is emitted with
    `{ vendor: claude, vendor_fallback_to: codex }`.
- **Driver path**:
  `scripts/host-probe/run-probes.sh codex-fallback`
- **Acceptance**:
  - prefs are persisted (visible via `ccteam prefs get`).
  - Mock-injected `budget_exceeded` is observed by the orchestrator
    and the next role-spawn uses Codex.
- **Expected status**: `happy` (must-be-real).

**Observed**: _-_

---

## Wave-4 D14 verification (model-id plumb)

This wave also wires `SpawnCtx::model_id` through to `ccteam_cost`.
The probe must show that:

- `AgentSpec::model` is read from `workflow.yaml`.
- `try_spawn_with_prompt` plumbs it into `SpawnCtx::model_id`.
- `translate_thread_event(..., model)` passes it to
  `ccteam_cost::estimate_cost(usage, vendor, model)` — *not* the
  empty string that falls back to the vendor's `fallback_model`.

Unit coverage: see the per-vendor model-specific cost tests in
`crates/ccteam-core/tests/cost_summary_test.rs::per_vendor_model_specific_pricing`.

---

## Risks / friction observed

- _-_  (populated post-sweep)

## V0.6.1 follow-ups identified during probes

- _-_  (populated post-sweep)

---

## How to fill this doc

1. `scripts/host-probe/deploy-to-nas.sh origin/main`
2. `scripts/host-probe/run-probes.sh` (or one scenario at a time)
3. Paste `summary.md` rows into the table at the top.
4. For each scenario block, paste `cmd.txt` excerpt + `log` tail +
   `cost.txt` snapshot into the `Observed` block.
5. Anything that came up that wasn't expected → "Risks / friction"
   or "V0.6.1 follow-ups".
6. Commit `host-probe.md` updates only — the probe-results dir is
   gitignored.
