# v0.8.12 handoff — track-upstream marketplace

> Shipped on `dev` (+ merged `main`); **git tag HELD** for owner review.
> Cross-repo: **ccteam** `dev` (engine) + **ccteam-hub** `main` (`c691c0b`).
> PRD: `prd.md`; contract: `dev-plan.md`.

## Decided

- **Track-upstream over vendor-copy**: `index.json` stores per-plugin `upstream`
  (raw URL @pinned-sha) + `content_sha`, NOT vendored bodies. Content is fetched
  into the user project at install time. This is what makes **multi-file /
  directory skills** installable (the vendor-copy model couldn't represent them).
- **First-party stays hub-local**: `source=="ccteam"` content (pk, autoloop)
  keeps its body in the hub; its `upstream` points at the hub's own raw tree
  (the hub IS the upstream). Uniform fetch path for everything.
- **Multi-file = `manifest`**: a skill with siblings carries
  `manifest:[{relpath, content_sha}]` (incl. `SKILL.md`); the engine derives each
  file URL from `dirname(upstream) + "/" + relpath`. Single-file → no manifest.
- **Skill id from the dir name** (fixes the `*/SKILL.md` stem=`SKILL` dup-crash);
  **global id collision resolution** in `sync.py` (unique across sources + types).
- **Host allowlist** (`raw.githubusercontent.com` + loopback) at the single fetch
  choke point → `HubError::HostNotAllowed`. content_sha gate unchanged.
- **Atomic multi-file install**: fetch + verify ALL files before writing any.
- `fetch_plugin_body(plugin)` / `install_plugin(.., force)` **drop the `base`
  arg** (fetch from `plugin.upstream`); `index.json` still fetched from hub base.

## Rejected / deferred

- Engine fetch-cache (PRD §九): availability cost is light (install = permanent
  local copy), so NOT added.
- Workflow install: still `UnsupportedType`.
- Runtime federation; non-GitHub raw hosts (future host-allowlist extension).

## Risks

- **GitHub raw CDN lag**: after a hub push, first-party `upstream` (→ hub `main`)
  serves stale content for ~5 min, so its live `content_sha` transiently
  mismatches. External plugins (pinned-sha paths) match immediately. Self-heals.
- **Old engine vs new index**: a pre-0.8.12 engine can't read the new
  pointer-only `index.json` (no `path`). Pre-v1.0 no-compat — redeploy the engine.
- Empty manifest files would be rejected by the non-empty body gate (no such
  file in the registered sources today).

## Files

- ccteam-hub (`main`): `scripts/sync.py` (rewrite), `sources.json` (+mattpocock),
  `index.json` (223 pointers), `skills/pk/SKILL.md` (+tags), 192 vendored bodies
  deleted, `agents/.gitkeep`.
- ccteam (`dev`): `crates/ccteam-im/src/hub.rs`, `crates/ccteam-core/src/{admin_actions,lib}.rs`,
  `crates/ccteam-web/src/routes/marketplace.rs`, `crates/ccteam-cli/src/commands.rs`,
  tests `crates/ccteam-im/tests/{hub_test,hub_live_smoke}.rs` +
  `crates/ccteam-web/tests/marketplace_test.rs`, tier-1 docs + version 0.8.12.

## Verification

- workspace `1999/0`, ccteam-web `279/0`, clippy + fmt clean.
- **Real-machine**: `hub_live_smoke` (`#[ignore]`) installs the live `diagnose`
  multi-file skill through the real engine → all files land, `installed_status =
  Installed`. content_sha verified against live raw URLs (agent + multi-file).

## Remaining / follow-ups

- HITL production resolver wiring (carried from v0.8.11).
- Auto-update flow for `UpdateAvailable` (manual re-add today).
- Optional: surface multi-file skill file-count in the web marketplace row.
