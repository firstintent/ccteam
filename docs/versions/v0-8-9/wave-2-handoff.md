# v0.8.9 Wave 2 Handoff — marketplace backend + install logic

> Phase 2 of the v0.8.9 run. Sub-commits on `dev`: `ed55325` (2a backend), `2eb6458` (2b web REST), `ccdcfad` (2c CLI repoint + delete v0.8.7 catalog/importer), + a review-fix (`HubError::BadStem`→400).

## Decided
- **3 sub-units** (sequential foreground opus agents, gate between): 2a the core+im hub backend, 2b the ccteam-web REST layer, 2c the CLI repoint + deletion of the superseded v0.8.7 path. The old catalog/importer were kept INTACT through 2a/2b (coexist → tree green), deleted only in 2c after the new path was proven by tests.
- **Hub config**: `ccteam_core::HUB_RAW_BASE = raw.githubusercontent.com/firstintent/ccteam-hub/main` (tracks `main` — the hub is the curated SoT; pinning happens at ingestion-time IN the hub, not at read-time). `CCTEAM_HUB_BASE` env seam → fake-hub tests. Local cache at `~/.ccteam/hub-cache/index.json` (`+"hub-cache"` to `canonical_home_dirs()`).
- **Backend** (`ccteam_im::hub`): `fetch_index` / `load_catalog(refresh)` (cache for offline browse) / `fetch_plugin_body` (sha256-verified) / `install_plugin` / `installed_status`. Reuses verbatim: the hardened reqwest fetch (no-redirect + 30s + 1 MiB cap + bounded read), `write_role`, `sanitize_role_stem`, `raw_url`, the `spawn_oneshot_http` test harness.
- **Integrity**: `fetch_plugin_body` computes sha256 over the fetched bytes and rejects (`ShaMismatch`) if ≠ the index `content_sha` — closes the v0.8.7 "URL-pin only, no content verification" gap. Size cap fires before hashing.
- **Installed-state = on-the-fly** (no sidecar): `installed_status` = on-disk-absent → NotInstalled / sha256(on-disk)==content_sha → Installed / differs → UpdateAvailable. Respects the `.ccteam`-layout red-line (no new project state file).
- **Install targets**: agent → `write_role` `.claude/agents/<id>.md`; skill → new `write_skill` `.claude/skills/<id>/SKILL.md` (hub has 0 skills now, but the path is type-complete); workflow → `UnsupportedType` (deferred). Install → immediately usable (the roles route reads the live FS).
- **REST** (4 routes, auto-auth-gated): `GET /api/v1/marketplace` (hub catalog, `?refresh`) · `GET /api/v1/marketplace/{id}/body` (sha-verified body preview = review-before-install) · `GET /api/v1/projects/{slug}/marketplace` (catalog decorated with per-project `installed_status`) · `POST /api/v1/projects/{slug}/marketplace/install`. HubError→HTTP: UnknownId 404 / Exists 409 / UnsupportedType+bad-stem 400 / integrity+fetch 502 / Write 500.
- **CLI repoint** (names unchanged): `ccteam role search/add` now read the hub (`load_catalog` + `install_plugin`); `role list` unchanged (local `.claude/agents/`).

## Rejected
- **Sidecar `installed-plugins.json`** (recon's alternative): on-the-fly sha is simpler, can't drift, and adds no project file — chosen instead.
- **Pinned-sha hub URL**: the hub tracks `main` (it is itself the curated/pinned SoT; per-source pinning is done in `ccteam-hub/sources.json` at ingestion).
- **Keeping `role_catalog.rs` whole**: split — moved the 2 still-used helpers (`raw_url`, `sanitize_role_stem`) into `core/hub.rs`, deleted the catalog-specific code.

## Risks
- **The hub repo is PRIVATE** (`firstintent/ccteam-hub`): github-raw 404s anonymously (verified). The production github-raw read path needs the hub made **PUBLIC** (it's a public marketplace by design — MIT open-source plugins). **User action (repo owner)**: make `ccteam-hub` public. Phase 2 is fully tested against a *fake* hub (`CCTEAM_HUB_BASE`), so the impl is verified; the real-hub read is exercised only once public. (Separate from the GitHub-Actions Workflow-permissions item from Phase 1, which the user has now enabled.)
- **workflow-type install deferred** (`UnsupportedType`) — hub has 0 workflows; revisit when a workflow source is ingested + the multi-workflow project layout is decided (marketplace-design §四.4).
- No conditional-GET/ETag — `?refresh` re-fetches the whole `index.json` (fine for one small file).

## Files
- **New**: `core/hub.rs` (HUB_RAW_BASE + moved `raw_url`/`sanitize_role_stem`), `im/hub.rs` (backend), `im/tests/hub_test.rs`, `web/routes/marketplace.rs`, `web/tests/marketplace_test.rs`.
- **Edited**: `core/{paths.rs, admin_actions.rs (write_skill/skill_md_path), lib.rs}`, `im/lib.rs`, `web/routes/{mod.rs, openapi.rs}`, `web/tests/openapi_test.rs`, `cli/{commands.rs, main.rs, tests/role_command_test.rs}`.
- **Deleted**: `core/role_catalog.rs`, `core/templates/agency_agents_catalog.json` (1346 lines), `im/role_import.rs`, `im/tests/role_import_test.rs` (guards covered by `hub_test.rs`).

## Remaining (later phases)
- **Phase 4**: the marketplace BROWSER UI (consumes these 4 REST routes — category tabs/source filter/search/cards/detail-drawer-with-body-preview/install + installed/update badges).
- **Deployment**: make `ccteam-hub` public for the github-raw read.
- workflow-type install + non-agent sources (when ingested).
- **Phase 5**: docs (marketplace + that the catalog is gone); specifically repoint the `docs/tech-design.md` 协议→代码 pointer-table row off the deleted `role_catalog.rs`/`agency_agents_catalog.json`/`role_import.rs` onto `hub.rs`/`marketplace.rs` (review P2 #2).

## Gate
`cargo test --workspace --exclude ccteam-web` **1896/0** (Phase-0 1894 + hub backend tests − deleted catalog/importer tests; fail=0); `cargo clippy --workspace --all-targets -D warnings` **0**; `cargo fmt --all --check` clean; `ccteam-web` 230 + 4 env-gated `ws_*` (4 marketplace integration + 2 unit + openapi-drift all pass). MCP surface unchanged (15). **Adversarial review: essentially clean, no P0/P1** — all 7 security/red-line checks pass (hardening carried forward, sha-integrity fires before hashing, no lost guard coverage, helpers preserved, install verbatim, REST sound, fake-hub-only). 2 P2: bad-stem install status (500→400) FIXED this wave via `HubError::BadStem`; the `tech-design.md` pointer-table still naming the 2c-deleted files is deferred to Phase 5 (see Remaining).
