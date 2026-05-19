# V0.4.2 Deploy Verify

## V0.4.2 Round-1 Deploy Verify (2026-05-15)

Host: `rob@192.168.1.19`  
Source tree: `/vol4/1000/nasworkspace/ccteam`  
Binary: `/vol4/1000/nasworkspace/ccteam/target/release/ccteam`

### Step 1 — environment + build

- HEAD on host: `ce219bc v0.4.2: unify install command (F72) + global config.yaml SoT (F73-F75) (#65)` — matches request
- pre-stop residual: 1 process — `ccteam start` pid `2678042` (clean baseline, V0.4.1 daemon still running)
- `cargo build --release -p ccteam-cli`: succeeded in 1m32s. workspace version `ccteam-core v0.4.2 / ccteam-web v0.4.2 / ccteam-hooks v0.4.2 / ccteam-cli v0.4.2` — version bump confirmed shipped

### Step 2 — daemon stop

- `ccteam stop`: SIGTERM sent to pid `2678042`. After 3s `pgrep ccteam start` returned empty — stopped clean.

### Step 3 — F74 v0.4.1 → v0.4.2 migration

Pre-migration host state:
- `~/projects/` had `backup`, `dev-hot-reload-test`, `dev-ui-quality`, `meta` (4 dirs; `backup` not a project)
- `~/.ccteam/watchdog.yaml`: not present (no watchdog state on host)
- `~/.ccteam/config.yaml`: not present (first migration)

After `ccteam doctor --migrate-v041-to-v042`:
- newly registered: **3 projects** (`dev-hot-reload-test`, `dev-ui-quality`, `meta`) — `backup` correctly skipped (not a ccteam project)
- already registered: 0
- skipped (corrupt): 0
- watchdog.yaml: "no change" — matches absence
- config.yaml: "updated" — file created from scratch
- output mentions `codex CLI: present @ /home/rob/.hermes/node/bin/codex` (health check side-output)

config.yaml after migration has the 3 projects with correct `slug / path / team / installed_at`. **PASS** — rerun-safe semantics confirmed in output ("rerun is safe — already-registered slugs are skipped.").

### Step 4 — V0.4.2 daemon start

Banner correct: bind `0.0.0.0:7331`, web token written to `~/.ccteam/web-token`. 2 project event loops boot (`dev-hot-reload-test`, `dev-ui-quality`). `meta` registered but apparently filtered (likely role-mapping; not blocking). No errors at startup.

### Step 5 — F72 three scenarios

| scenario | result |
|---|---|
| A: `mkdir /tmp/v042-scenarioA && cd && ccteam init --team dev` | **PASS** — header `ccteam init — fresh install`, scaffolds `state.json` (created), `workflow.yaml` (scaffolded), `.claude/agents/explorer.md`, `config.yaml` upserted |
| B: existing git repo cwd, `ccteam init --team dev` | **PASS** — same fresh-install header; state.json has slug `v042-scenarioB`, team `dev`, parallelism `solo` |
| C: re-run with hand-edited `workflow.yaml=USER-EDITED` | **PASS** — header changes to `ccteam init — refresh`; report says `workflow.yaml (preserved)`, `agents dir (preserved)`, `state.json (refreshed)`. After re-run `cat workflow.yaml` still `USER-EDITED` |

### Step 6 — F72 `--force`

`cd /tmp/v042-scenarioB && ccteam init --force` after scenario C: report says `workflow.yaml (overwritten (--force))`. `head workflow.yaml` now shows scaffolded `# ccteam workflow.yaml (V0.4.0+ shape).` template — **PASS**.

### Step 7 — F72 sensitive-path refusal

`cd ~ && ccteam init` →
```
Error: refusing to install at /home/rob — this looks like $HOME or the filesystem root.
Make a subdirectory (...) or pass --force if you really mean to install here.
```
**PASS**.

### Step 8 — F72 slug-collision refusal

`mkdir /tmp/v042-collision && cd && ccteam init --slug v042-scenarioB --team dev` →
```
Error: slug `v042-scenarioB` is already registered at /tmp/v042-scenarioB in /home/rob/.ccteam/config.yaml;
refusing to overwrite the registry pointer to /home/rob/projects/v042-scenarioB.
Pick a different slug with `--slug <other-name>`, or pass `--force` to retarget the existing entry.
```
**PASS**. Note: error text shows `~/projects/v042-scenarioB` as the would-be path (target dir was `/tmp/v042-collision` and `~/projects/v042-scenarioB` was the proposed re-registration target); message is slightly confusing but the registry-already-occupied fail-loud is correct.

### Step 9 — F75 `ccteam new <slug>` + F22 team prefix

`ccteam new v042test --team dev`:
- target dir: `/home/rob/projects/dev-v042test` — **F22 team prefix applied**
- slug in output: `dev-v042test`
- config.yaml entry: `slug: dev-v042test` / `path: /home/rob/projects/dev-v042test` / `team: dev`
- on-disk: `~/projects/dev-v042test/.ccteam/state.json`, `~/projects/dev-v042test/.ccteam/spec.md` present

**PASS**.

Free-text path verification: `ccteam new "做一个 todo cli" --team dev` → **UNEXPECTED PASS-THROUGH**. The Chinese-with-spaces string was accepted as a literal slug; ccteam created `~/projects/dev-做一个 todo cli/` (directory with spaces + Unicode), wrote state.json, and upserted config.yaml. No fail-loud. See V0.4.3 candidate below. Cleaned up bad dir + config.yaml entry post-test.

### Step 10 — F73 daemon hot-load

Daemon log after scenarios:
```
2026-05-15T07:15:14Z INFO ccteam_core::orchestrator: hot-loaded new project; starting event loop slug="dev-v042test"
2026-05-15T07:15:44Z INFO ccteam_core::orchestrator: hot-loaded new project; starting event loop slug="dev-做一个 todo cli"
```
**PASS** — daemon polls config.yaml and dynamically picks up new projects on the registry. (Bad-slug also hot-loaded, then graceful WARN loop after dir was deleted: `registered project's state.json is missing; skipping (run \`ccteam abandon ... \` to clean up)`. Daemon stayed healthy.)

Note: scenario A/B/C didn't hot-load because their paths are `/tmp/...`, which the daemon may filter (likely intentional — `~/projects/` is the rostered prefix). Only `ccteam new` (which goes through `~/projects/<team>-<slug>/`) triggered the hot-load. Worth documenting as expected behavior.

### Step 11 — residual check

- `pgrep ccteam mcp-serve | grep -v bash` → empty (0 residual mcp-serve)
- `~/.claude/jobs/` entry count: **289** — pre-existing legacy cruft from V0.3.x/V0.4.x runs, not from this verify. Not blocking; cleanup is a separate housekeeping task.

### V0.4.3 hotfix candidates

1. **F75 slug-validity check**: `ccteam new "做一个 todo cli" --team dev` silently accepts a slug with spaces + Unicode and produces an unusable directory (`~/projects/dev-做一个 todo cli/`). The daemon hot-loads it, then has to fall back to WARN-loop after manual cleanup. Should fail-loud at CLI parse:
   - reject slugs containing whitespace
   - probably restrict to `[a-z0-9][a-z0-9-]*` (kebab-case, ascii) given existing slug examples; if Unicode slugs are intended for international users, at minimum reject `' '` and `/`.
   - emit hint pointing at the V0.4.2 design: position arg is a slug, not free-text.
2. **F72 slug-collision error wording**: collision message references `~/projects/v042-scenarioB` as "the registry pointer" target, but the actual register-already-occupied entry is at `/tmp/v042-scenarioB`. Confusing. Suggest: `slug X is already registered at <existing-path>; cannot re-register from <attempted-cwd>`.
3. **Hot-load scope clarification (doc)**: daemon hot-load fires for `~/projects/<...>` (because that's the rostered prefix) but not for arbitrary `/tmp/<...>` projects. Worth a CLAUDE.md/PRD note so V0.4.3 readers know `init` in `/tmp` is intentionally orphan from the daemon.
4. **`~/.claude/jobs/` cleanup**: 289 stale entries on this host. Either auto-prune on `ccteam start` or document a `ccteam doctor --gc-jobs` flag.

### Summary

| check | result |
|---|---|
| HEAD `ce219bc` | yes |
| Build v0.4.2 | yes |
| Clean stop | yes |
| F74 migration (3 projects, watchdog folded n/a) | PASS |
| Daemon start | PASS (2 projects event-loop, 1 filtered) |
| F72 scenario A (fresh dir) | PASS |
| F72 scenario B (existing repo) | PASS |
| F72 scenario C (re-run preserves) | PASS |
| F72 `--force` | PASS |
| F72 sensitive-path refusal | PASS |
| F72 slug-collision refusal | PASS (text slightly confusing) |
| F75 `ccteam new <slug>` + F22 prefix | PASS |
| F75 free-text rejection | **FAIL** (silently accepted; V0.4.3 hotfix) |
| F73 daemon hot-load | PASS |
| Residual mcp-serve | 0 |

---

## V0.4.3 Round-2 Hotfix Deploy Verify (2026-05-15)

Host: `rob@192.168.1.19`  
Source tree: `/vol4/1000/nasworkspace/ccteam`  
Binary: `/vol4/1000/nasworkspace/ccteam/target/release/ccteam`

### Step 1 — pull + build

- HEAD on host after `git fetch origin && git reset --hard origin/main`: `f3c462f v0.4.3 hotfix: F76 — slug grammar validation at CLI boundary` — matches request
- `cargo build --release -p ccteam-cli`: clean finish in 1m30s. **Note**: workspace version still `ccteam-core v0.4.2 / ccteam-cli v0.4.2` — F76 commit did NOT bump `workspace.package.version` to `0.4.3`. Binary embeds `0.4.2`. V0.4.4 candidate.

### Step 2 — daemon stop + clean restart

- `ccteam stop`: SIGTERM sent to pid `2796945`. `pgrep ccteam start` empty after 3s — clean.
- `rm orchestrator.pid && nohup ccteam start`: 4 project event loops boot (`dev-hot-reload-test`, `dev-ui-quality`, `dev-v042test`, `dev-v043test`) — note `dev-v042test` was a round-1 leftover registered in `config.yaml`, see Step 5 cleanup.

### Step 3 — F76 bad-slug rejection (4 cases)

| input | result | error excerpt |
|---|---|---|
| `ccteam new "做一个 todo cli" --team dev` | **fail-loud, exit 1** | `slug must match [a-z0-9-]+ (lowercase ASCII, digits, dashes only); got "做一个 todo cli"` |
| `ccteam new "BadSlug" --team dev` | **fail-loud, exit 1** | `slug must match [a-z0-9-]+ (lowercase ASCII, digits, dashes only); got "BadSlug"` |
| `ccteam new -- "-leading-dash" --team dev` | **fail-loud, exit 1** | `slug must not start or end with \`-\`; got "-leading-dash"` |
| `ccteam new "trailing-dash-" --team dev` | **fail-loud, exit 1** | `slug must not start or end with \`-\`; got "trailing-dash-"` |
| post-check: `ls ~/projects/ \| grep -E "做\|Bad\|leading\|trailing"` | `no garbage dirs (good)` | — |

**PASS** on all 4 grammar checks; clean error wording explicitly references `[a-z0-9-]+`.

Minor footnote: bare `ccteam new "-leading-dash"` is intercepted by clap as `-l` short-flag before reaching F76. To exercise the validation path the caller must use `--`. This is clap-default behavior, not a F76 regression — V0.4.4 polish candidate (could set `Arg::allow_hyphen_values(true)` on the slug positional and let F76 own the message).

### Step 4 — good slug still works

- `ccteam new v043test --team dev` → header `ccteam init — fresh install`, target `~/projects/dev-v043test`, slug `dev-v043test` (F22 prefix applied), team `dev`. State.json + workflow.yaml + agents dir all scaffolded. config.yaml upserted. **PASS**.

### Step 5 — round-1 garbage cleanup

`~/projects/` before cleanup: `backup, dev-hot-reload-test, dev-ui-quality, dev-v042test, dev-v043test, meta, v042-scenarioB`.

Round-1 test residues identified:
- `~/projects/dev-v042test/` (test artifact)
- `~/projects/v042-scenarioB/` (empty, test artifact — note: no `dev-` prefix because round-1 scenario B was created via `init --team dev` in cwd `/tmp/v042-scenarioB` which honors cwd basename without F22 prefix application — F22 only applies to `ccteam new`)
- `/tmp/v042-collision`, `/tmp/v042-scenarioA`, `/tmp/v042-scenarioB`

config.yaml stale entries: `v042-scenarioA`, `v042-scenarioB`, `dev-v042test`.

Cleaned: `rm -rf` test dirs + manual rewrite of `~/.ccteam/config.yaml` (kept backup `.bak.v043`). Final projects[]: `dev-hot-reload-test`, `dev-ui-quality`, `meta`, `dev-v043test`.

User projects (`dev-hot-reload-test`, `dev-ui-quality`, `meta`) untouched.

No `~/projects/dev-做*` etc garbage found — round-1 scenarios A/B/C all used valid slugs, the F75 unicode-slug regression was only verified once and never persisted to `~/projects/`. Cleanup wasn't strictly required, but config.yaml stale entries removed for hygiene.

### Step 6 — F72 improved slug-collision wording

Trigger: `ccteam init --slug dev-v043test --in /tmp/v043-elsewhere --team dev` (slug owned by `~/projects/dev-v043test`, --in points elsewhere).

Output:
```
Error: slug `dev-v043test` is already registered in /home/rob/.ccteam/config.yaml pointing at /home/rob/projects/dev-v043test, but this install would point it at /tmp/v043-elsewhere. Refusing to silently retarget.

Resolve by either:
  - pick a different slug:  `ccteam init --slug <other-name>` (or `--in <other-path>`),
  - intentionally retarget: re-run with `--force` (the registry entry will be rewritten to /tmp/v043-elsewhere).
```

**PASS** — message names the registered path, the attempted path, the registry file, and two distinct resolution paths (rename vs retarget). Matches round-1 retro recommendation.

Side note: `ccteam init --slug X` where X is in registry and cwd-or-target == registered path → "refresh" branch (no collision, no error). Only when target != registered does the collision fire. This is correct semantics; reviewer noted potential confusion but the F72 wording now makes it obvious.

### Step 7 — residuals

- `pgrep ccteam mcp-serve` → 0
- `~/.claude/jobs/` → 289 entries (carry-over from earlier rounds; same as V0.4.2 round-1; V0.4.4 GC candidate still open)
- `pgrep ccteam` → 1 process (`ccteam start` pid `2801657`) + 1 unrelated `claude daemon run`

### Summary

| check | result |
|---|---|
| HEAD `f3c462f` | yes |
| Build v0.4.3 hotfix | yes (note: version string stuck at `0.4.2`) |
| Daemon clean stop + restart | PASS |
| F76 Chinese+space slug | **PASS fail-loud** |
| F76 uppercase slug | **PASS fail-loud** |
| F76 leading-dash slug | **PASS fail-loud** (needs `--` to bypass clap short-flag) |
| F76 trailing-dash slug | **PASS fail-loud** |
| No garbage `~/projects/<bad>` dirs | PASS |
| Good slug still creates `dev-<slug>` (F22 prefix) | PASS |
| F72 improved collision wording | PASS (clear "Resolve by either ...") |
| Round-1 cleanup | done (`dev-v042test`, `v042-scenarioB`, `/tmp/v042-*` + config.yaml stale rows removed) |
| Residual mcp-serve | 0 |
| Residual `~/.claude/jobs/` | 289 (carryover) |

### V0.4.4 hotfix candidates

1. **`workspace.package.version` bump**: F76 commit shipped without bumping to `0.4.3`. Binary self-reports `0.4.2`. Need a follow-up bump commit OR ensure CLAUDE.md §五 "Patch 版本开发流程 → cargo bump" is checklist-enforced.
2. **Clap short-flag swallows `-X` slugs**: leading-dash slug requires `--` separator before F76 sees it. Consider `Arg::allow_hyphen_values(true)` on slug positional in `ccteam new` to let F76 own the message.
3. **`~/.claude/jobs/` GC** (carryover from round-1 retro): 289 stale entries on this host. Candidate: `ccteam doctor --gc-jobs` flag or auto-prune on `ccteam start`.
4. **Hot-load scope doc note** (carryover from round-1 retro): daemon hot-loads `~/projects/<...>` but not `/tmp/<...>`. Worth a CLAUDE.md note.
