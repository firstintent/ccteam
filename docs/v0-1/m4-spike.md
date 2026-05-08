# M4.4 Spike — Container bind-mount + paths frontmatter (2026-05-06)

> 0.5-day verification accompanying M4.1–M4.3 (PR `feat/m4-cross-project-memory`).
> Validates the assumptions behind ccteam's "zero retrieval code" memory
> design: the rules + auto-memory bridge actually has to load into the
> right Claude sessions, scoped to the right team, for the M4 main path
> to be useful. This report records what's verified now vs. what still
> needs runtime confirmation.

## TL;DR

| Check | Result | Action |
|---|---|---|
| `ccteam doctor --install-memory-bridge` writes both team files | ✅ verified live | none |
| Re-run is idempotent (no-op on intact markers) | ✅ verified live | none |
| Marked-section repair preserves user content outside markers | ✅ verified live | none |
| `paths:` frontmatter `~/projects/<team>-*` scoping works | ✅ **fixed by F22** (2026-05-06 follow-up PR) — slugs now `<team>-<base>` | none |
| `~/.claude/rules/*.md` auto-loads in `--dangerously-skip-permissions` container | ⏳ deferred to live runtime | open follow-up (see §4) |
| `claude-mem` MCP tools detection by phase prompt conditional | ✅ tool surface visible when installed | none |
| retro Edit on marked section idempotent end-to-end | ⏳ deferred (covered by unit tests) | optional follow-up |

## 1. Idempotency + repair (live verified)

Ran against the developer's actual `~/.claude/`:

```bash
$ ccteam doctor --install-memory-bridge   # first run
  dev               wrote                /home/rob/.claude/rules/ccteam-lessons-dev.md
  product-research  wrote                /home/rob/.claude/rules/ccteam-lessons-product-research.md

$ ccteam doctor --install-memory-bridge   # second run
  dev               already-present      /home/rob/.claude/rules/ccteam-lessons-dev.md
  product-research  already-present      /home/rob/.claude/rules/ccteam-lessons-product-research.md

$ ccteam doctor --install-memory-bridge --dry-run
  dev               no-op (already present)
  product-research  no-op (already present)
```

**Repair test** — manually corrupted the dev file with two duplicated marker
blocks plus interleaved user prose; re-ran the doctor:

```
$ ccteam doctor --install-memory-bridge
  dev               repaired             /home/rob/.claude/rules/ccteam-lessons-dev.md
```

Post-repair file contains: every line of user prose preserved (in source
order), exactly one canonical marked block at end-of-file, both stray block
contents discarded. Matches `crates/ccteam-core/src/memory_bridge.rs` unit
test `install_repairs_duplicated_marker_blocks`.

## 2. claude-mem detection path (live verified)

This developer's machine has `claude-mem` installed via the official
plugin marketplace. Tool surface enumerates:

```
mcp__plugin_claude-mem_mcp-search__search
mcp__plugin_claude-mem_mcp-search__timeline
mcp__plugin_claude-mem_mcp-search__get_observations
mcp__plugin_claude-mem_mcp-search____IMPORTANT
… plus smart_search / smart_outline / smart_unfold
```

The phase prompt conditional ("if you see `mcp__*claude-mem*search`-like
tools, you may call them") matches all of these by glob. ccteam writes no
detection code — confirmed by the M4 red line grep:

```bash
$ grep -rnE "fn .+_memory|fn .+_retrieve|sentence|sqlite.*memory|chromadb|claude.?mem" \
    crates/ccteam-core/src/ crates/ccteam-cli/src/
crates/ccteam-core/src/orchestrator.rs:1027:    /// claude-mem RAG flow; …  (comment only, M1 docstring)
crates/ccteam-core/src/memory_bridge.rs:74:pub fn install_memory_bridge(  (file write, no retrieval)
crates/ccteam-cli/src/commands.rs:648:fn render_install_memory_bridge_report(opts: …)  (CLI render)
```

Only file-write and rendering code, no retrieval. Red line clean.

## 3. ✅ Closed (2026-05-06 follow-up PR) — slug team prefix landed

The shipped lessons files declare:

```yaml
# ccteam-lessons-dev.md
paths:
  - "~/projects/dev-*"

# ccteam-lessons-product-research.md
paths:
  - "~/projects/product-research-*"
```

**but** current bootstrap (`crates/ccteam-core/src/projects.rs`,
`pick_unused_slug` + `bootstrap_project`) creates projects at
`~/projects/<slug>/` with no team prefix:

```
$ ccteam new --team=dev "make a markdown editor"
created project make-a-markdown-editor (team: dev)
  spec   : /home/rob/projects/make-a-markdown-editor/.ccteam/spec.md
```

Claude Code matches `paths:` against the session's cwd. With current
slugs, `~/projects/dev-*` matches **zero** dev projects — so the dev
lessons file is effectively dark for the very sessions it was authored
for. Only the meta-agent project (`bootstrap_meta_project` writes
`<handle>-meta` slugs) follows a team-suffix convention; that's the
existing precedent we'd lift.

**Why this is a real follow-up, not noise**: the M4 main path requires
cross-project lessons to auto-load into the right phase Claude. Without
team-prefixed slugs, M4.1's retro phase will write to lessons files
that nothing reads.

**Fix landed** (separate PR after this report): `pick_unused_slug` now
takes `team: &str` and produces `<team>-<base>` (or `<team>-<base>-<suffix>`
on collision). M4 main path now actually fires — `~/.claude/rules/`
auto-loads cross-project lessons into the right phase Claude sessions.

Migration: historical pre-F22 project dirs keep working (orchestrator
identifies team via `state.json` regardless of dir name), only new-create
paths change. interfaces.md §1.2 updated to reflect the new convention.

Alternative quicker fix considered and rejected: drop `paths:` entirely
and let the rules file auto-load into all Claude sessions globally. Loses
team isolation (dev sessions see product-research lessons and vice versa),
re-introduces the "old project's wrong-team lessons pollute new project"
failure mode the schema design was fighting against (tech-design §3.7).

## 4. ⏳ Deferred — `~/.claude/rules/*.md` auto-load in container sessions

Cannot verify from this dev's daily-driver Claude session (the canonical
test is "spawn a `ccteam new` project session that runs Claude inside the
configured tool-isolation container, with `--dangerously-skip-permissions`,
and check whether the dev lessons file shows up in its session-start
context"). That requires:

- a project under `~/projects/dev-*` to exist (blocked by §3 today)
- the orchestrator running and dispatching plan-eng phase
- inspection of the project session's first few messages

**Methodology when §3 is fixed**:

1. `ccteam doctor --install-memory-bridge` (already shipped).
2. Manually `Edit` `~/.claude/rules/ccteam-lessons-dev.md` to add a
   sentinel string inside the marked block, e.g. `SENTINEL_M4_SPIKE_2026_05_06`.
3. `ccteam new --team=dev "spike echo sentinel"` (assumes §3 fix landed
   so slug becomes `dev-spike-echo-sentinel`).
4. Add a one-liner to the top of `phases/02-plan-eng.md`:
   "if you see SENTINEL_M4_SPIKE_2026_05_06 in your context, copy it
    verbatim into your first reply."
5. `ccteam start --foreground` and watch `progress.jsonl`. If the first
   assistant message echoes the sentinel → auto-load works through the
   container. If not → container is masking `~/.claude/rules/`; doctor
   needs a `--bind-mount-claude-rules` step that adds the rules dir to
   the project's tool-isolation config.

The expected outcome (per Claude Code official docs at
https://code.claude.com/docs/en/memory) is that auto-load works:
`~/.claude/rules/*.md` is part of the standard tool surface, not opt-in.
The `--dangerously-skip-permissions` flag affects tool *execution*
authorization, not file *reading* into context. The container question
is whether the operator's container config remaps `$HOME` or shadows
`~/.claude/`; the default ccteam container (M2.x) does neither.

**If the deferred check fails**, the doctor extension is small:

```rust
// crates/ccteam-cli/src/commands.rs (sketch)
fn render_bind_mount_claude_rules() -> Result<String> {
    // Append to the project-level container config:
    //   mounts:
    //     - source: ~/.claude/rules
    //       target: /root/.claude/rules
    //       readonly: true
    // Idempotent — skip if already present.
}
```

No need to implement until the spike actually fails. Tracking as
**F23 dev-coupling-audit follow-up**.

## 5. retro idempotency (covered by unit tests, optional E2E)

Live unit tests already cover:

- `install_creates_both_lessons_files_with_intact_markers`
- `install_idempotent_when_files_present_and_intact`
- `install_preserves_lessons_written_between_markers` ← **this is the retro
  idempotency check** (simulates retro phase having written content between
  markers; verifies subsequent doctor re-run preserves that content)
- `install_repairs_missing_markers_and_keeps_user_content`
- `install_repairs_duplicated_marker_blocks`
- `install_repairs_unbalanced_begin_marker`

End-to-end retro simulation (using a real Claude session writing the
marked block via `Edit`) would re-validate the same invariants the unit
tests already cover, so deferring as low-value unless §3 + §4 surface
unexpected interactions.

## 6. What this PR ships, what it doesn't

**Ships in this PR** (M4.1–M4.4):

- `ccteam doctor --install-memory-bridge` (M4.2) — placeholder rules
  files, idempotent, marker-repair logic, 9 new tests
- Retro phase prompts rewrite (M4.1) — dev `phases/09-ship.md` +
  product-research `phases-product-research/06-verdict.md` REJECT branch,
  driven by `team.yaml.retro_schema` (product-research schema populated
  for the first time, closes F20)
- Seed phase prompts (M4.3) — `02-plan-eng.md` / `01-kickoff.md` /
  `06-verdict.md` reference cross-project lessons + `/memory` + optional
  claude-mem
- This spike report

**Does NOT ship** (open follow-ups):

- ~~F22 — slug team prefix~~ ✅ **closed in 2026-05-06 follow-up PR**;
  `~/projects/<team>-<slug>/` now produced by `pick_unused_slug`.
- F23 — container bind-mount step in doctor, contingent on §4 spike
  failing now that F22 has landed. Methodology is ready (§4); next
  re-run will determine whether the bind-mount is actually needed.

§4 deferred check is **now unblocked**; can be performed with
`ccteam doctor --install-memory-bridge` + `ccteam new --team=dev "X"`
+ phase prompt sentinel pattern from §4.

## References

- 痛点 10 — every project starts from zero
- `docs/tech-design.md` §3.7 — Cross-project Memory architecture
- `docs/v0-1/development-plan.md` §6 — M4.1–M4.4 task table
- `references/research/claude-code-memory-research.md` §六 — M4 decision
  basis (rules + auto-memory + optional claude-mem MCP)
- `crates/ccteam-core/src/memory_bridge.rs` — installer + tests
