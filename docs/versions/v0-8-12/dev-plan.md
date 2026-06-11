# v0.8.12 dev-plan — track-upstream marketplace (implementation contract)

> Companion to `prd.md`. Locks the schema/signature contract the PRD left to dev,
> + the phase order. Cross-repo: **ccteam** `dev` (engine) + **ccteam-hub** `main`
> (sync.py + sources.json + index.json). dev/main **no-PR**, per-phase push.

## Locked index-entry schema (`index.json` plugin)

```jsonc
{
  "id", "type", "name", "description",
  "upstream":    "<raw-fetchable URL @sha>",   // REPLACES hub-local `path`
  "content_sha": "<sha256 of the primary body>",
  "source", "license", "tags": [...],
  "manifest": [ { "relpath": "SKILL.md", "content_sha": "…" }, … ] // OPTIONAL — multi-file skills only
}
```

- **`path` removed.** `upstream` is the body's raw URL.
  - external source: `raw.githubusercontent.com/<owner>/<repo>/<sha>/<path>`.
  - **first-party** (`source=="ccteam"`): the hub IS the upstream →
    `raw.githubusercontent.com/firstintent/ccteam-hub/main/<hub-path>`. First-party
    bodies STAY vendored in the hub; external bodies do NOT (pure pointer).
- **multi-file skill**: `upstream` → the `SKILL.md`; `manifest` enumerates every
  file (relpath relative to the skill dir, incl. `SKILL.md`) + its `content_sha`.
  The engine derives each file URL = `dirname(upstream) + "/" + relpath`.
  Single-file agent / SKILL.md-only skill → **no** `manifest`.

## sync.py (ccteam-hub, stdlib-only, idempotent, byte-identical @ same sha)

1. **external sources** (`sources.json`): clone @ref → glob → entries with
   `upstream` = raw URL @sha, `content_sha` READ from the clone (no copy), and a
   `manifest` for skill dirs (enumerate `<dir>/**`). **skill id = dir name** (fix
   `*/SKILL.md` → stem "SKILL" dup-crash). NO `shutil.copyfile`.
2. **first-party scan**: glob the hub's own `agents/**.md`, `skills/*/SKILL.md`
   (+ dir for manifest), `workflows/*/*` → entries `source="ccteam"`,
   `upstream` = hub raw URL @ `main`, `content_sha` from the hub file, `tags` from
   frontmatter `tags:`.
3. drop all vendored EXTERNAL bodies from the hub; first-party stays.

## engine (`crates/ccteam-im/src/hub.rs` + `ccteam-core` + web + CLI)

- `HubPlugin`: drop `path`; `upstream` load-bearing; add
  `manifest: Option<Vec<ManifestEntry>>` (`{relpath, content_sha}`).
- **host allowlist**: fetches allowed only from `raw.githubusercontent.com`
  (+ loopback for tests) → new `HubError::HostNotAllowed`. `index.json` still from
  hub base (also that host). `content_sha` gate unchanged.
- `fetch_plugin_body(plugin)` — **drop `base`**; fetch from `plugin.upstream`.
- `install_plugin(project_dir, plugin, target_stem, force)` — **drop `base`**;
  `manifest` present → fetch each relpath (URL derived from `upstream` dir),
  verify sha, write `.claude/skills/<id>/<relpath>`; else single-file as today.
- `installed_status` — `manifest` present → all files present+sha = `Installed`,
  some missing/diff = `UpdateAvailable`, none = `NotInstalled`.
- new `ccteam_core` helper: write a file under `.claude/skills/<id>/<relpath>`.
- tests: deterministic fake-HTTP source (loopback) → content_sha verify +
  multi-file landing + host-allowlist reject. baseline ≥ prior; clippy/fmt clean.

## phase order (per-phase verify + push)

- **P1** ccteam-hub: rework sync.py + add `mattpocock/skills` to sources.json +
  `tags:` into pk frontmatter + run sync → rebuilt `index.json` (pointers,
  multi-file manifests) + drop vendored external bodies → commit+push `main`.
- **P2** ccteam engine: hub.rs + core + web + CLI rework + fake-HTTP tests →
  `cargo test`/clippy/fmt ≥ baseline → commit `dev`.
- **P3** real verify against live hub: browse + install a multi-file mattpocock
  skill into `.claude/skills/<id>/` (files complete) → then push `dev` → ff `main`.
