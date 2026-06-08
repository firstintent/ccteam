# v0.8.9 Wave 1 Handoff — ccteam-hub fill + agency-agents ingestion

> Phase 1 of the v0.8.9 run. In the **ccteam-hub** repo (separate git, `firstintent/ccteam-hub`): commit `ce8b485` on `main`.

## Decided
- **Ingestion model = copy-into-hub, not runtime federation** (per `marketplace-design.md`): the hub is the single index+content source; open-source repos are vendored in by an idempotent sync, ccteam only ever reads the hub.
- **Source**: `agency-agents` = `github.com/wshobson/agents` (MIT), pinned at sha `cf6059d030bf4fe96623ae2e596d2f31e35fedc0`. Real layout verified = `plugins/<division>/agents/<name>.md` → **192 agent personas across 82 divisions**.
- **Pipeline** (3 pieces in the hub):
  - `sources.json` — declares the source (repo/license/ref/glob-map).
  - `scripts/sync.py` — stdlib-only idempotent ingester: full clone + `checkout <sha>` → glob per map → copy each `.md` **verbatim** into `agents/<id>.md` → parse frontmatter for name/description → sha256 `content_sha` → rebuild `index.json`. Cleans the source's prior files before re-copy (no lingering removed-upstream files). Captures the repo LICENSE into `LICENSES/agency-agents.LICENSE` with provenance.
  - `.github/workflows/sync.yml` — `workflow_dispatch` + weekly cron, runs sync + commits any diff.
- **id scheme**: `sanitize(stem)` to `[a-z0-9_-]`; a stem appearing in >1 division → ALL its instances get a `<division>-` prefix (deterministic). 30 colliding stems → 95 prefixed + 97 bare = **192 globally-unique ids, 0 secondary collisions**. `id == sanitize(upstream name)` for all 192 (matches upstream's own `name:` convention).
- **Idempotency = hard gate, achieved**: `generated_at` derived from the source commit date (`git show -s --format=%cI` → `2026-06-05T17:23:01+09:00`), NOT wall-clock; `plugins` sorted by id. 3 consecutive runs byte-identical; stale-file cleanup verified.

## Rejected
- **Runtime multi-source federation**: rejected for single-source-of-truth + offline cache + a curation/security chokepoint + upstream-change isolation (marketplace-design §一).
- **Wall-clock `generated_at`**: breaks `git diff` idempotency — derived from the source commit date instead.
- **Reusing ccteam's `agency_agents_catalog.json` ids**: the hub derives ids independently from the upstream layout (the catalog is deleted in Phase 2; the hub `index.json` is the new SoT).

## Risks
- **GitHub Action push perms**: `sync.yml` commits as `github-actions[bot]`; for the scheduled re-sync to push, the hub repo needs *Settings → Actions → Workflow permissions = Read and write*. **User action** (repo owner) if auto-resync is wanted; manual `python3 scripts/sync.py` always works.
- **Ingested personas are open-source prompts that WILL be executed** when a user installs + runs one. Mitigations: pinned sha (no floating ref), verbatim copy with `content_sha`, MIT attribution preserved, and **Phase 4's marketplace shows the persona body BEFORE install** (review-before-run). Ingestion is a curated chokepoint (only declared sources, not arbitrary URLs).
- Hub now carries 192 `.md` files — expected (it's the marketplace content).

## Files (hub repo, commit `ce8b485`)
- New: `sources.json`, `scripts/sync.py`, `.github/workflows/sync.yml`, `agents/*.md` (192), `LICENSES/agency-agents.LICENSE`; removed redundant `agents/.gitkeep` (skills//workflows/ keep theirs as empty-dir anchors).
- Modified: `index.json` (192 entries), `README.md` (hub purpose / layout / schema / sources / sync / id+idempotency design).

## Remaining (later phases)
- **Phase 2**: ccteam reads the hub `index.json` (github-raw + local cache) + install logic (write to project `.claude/agents|skills/` + workflow dir, reuse `write_role`) + REST API (GET catalog / POST install); then DELETE the ccteam-side `agency_agents_catalog.json` + the old direct-github role-import.
- Skills/workflows ingestion: agency-agents is agents-only; future sources (or builtin self-made skills/workflows) populate those hub dirs — `index.json` schema + `type` already support them.

## Gate
`jq '.plugins|length'` = **192**; every entry has id/type/name/description/path/content_sha/source/upstream/license/tags; all `path` files exist; ids unique + `^[a-z0-9_-]+$`; `content_sha` matches on-disk bytes; copied files byte-identical to upstream; **idempotent** (runs 2→3 byte-identical); MIT LICENSE vendored with sha provenance. Reviewed by self-spot-check (schema + verbatim fidelity + license + the agent's 3× idempotency proof); full adversarial-review budget reserved for the code-heavy phases (2/3/4).
