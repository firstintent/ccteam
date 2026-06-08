# v0.8.9 Wave 4 Handoff — web UI overhaul (unified chat shell + marketplace)

> Phase 4 of the v0.8.9 run. Commits on `dev`: `7e0c8a6` (4a backend `/api/v1/status`), `3806790` (4b SPA demolition + shell skeleton), `7845d1b` (4c features + 3 review-fixes). The biggest phase — collapse the two forked SPA layouts into ONE chat-style shell, delete the legacy operator UI, build the plugin marketplace browser + Status view + cost pill.

## Decided
- **`ChatConsole.tsx` IS the unified shell** — the recon found it was already ~80% of the prototype (session-tree sidebar + crumb + Chat|终端 toggle + transcript/HITL + NewSessionModal + terminal). Phase 4 = promote it to the only shell + add the 3 missing pieces (global-views bottom nav, cost pill, Status/Marketplace), NOT a rewrite.
- **3-agent decomposition** (backend ∥ frontend-demolition → frontend-features; the first two are disjoint build systems Rust/TS so they ran in parallel):
  - **4a** — `GET /api/v1/status` (Rust): the daemon-wide rollup the cost pill + Status view need (no browser-reachable aggregate existed; `run_status` did it in-process). Returns `{daemon_healthy, sessions_live, sessions_idle, cost_24h_usd, cost_24h_by_vendor, budget_cap_24h}`: health via `check_daemon_health`, sessions from the gateway map, cost summed via `cost_summary` over `collect_projects`, budget cap summed from per-project `workflow.yaml::budgets_v060` (best-effort; degrades to 0/false/null). +`ccteam-cost` dep, 2 tests, drift list updated.
  - **4b** — SPA demolition + shell skeleton: deleted **37 files** (8 operator pages, the operator shell WorkspaceSidebar/TopBar/ContentSplit, 9 orphaned operator panels, 3 team panels, 4 newly-dead lib clients + useProgressStream, 9 dead tests). `App.tsx` → single shell at every route incl. global views `/marketplace` `/status` `/settings`. ChatConsole gained bottom-nav + global-view switching (hide Chat|终端 tabs on global pages) + a `CostPill` slot + `MarketplacePlaceholder`/`StatusPlaceholder` stubs + SettingsPage re-homed.
  - **4c** — features: `marketplaceApi.ts`/`statusApi.ts` + pure `marketplaceFormat.ts` helpers; `MarketplaceView` (project-picker install target + category tabs + source filter + search + cards w/ `installed_status` Install/已装/更新 + detail drawer with **`marked` body preview = review-before-install** + install→toast→re-fetch); `StatusView` (daemon health + sessions live/idle + today's cost/budget, polls 15s); `CostPill` (`今日 $X / $Y`, warn-color, polls 20s + focus, → `/status`); `@theme` vendor tokens (`vendor-claude #d97757` / `vendor-codex #10a37f` / `brand-dim`); **26 bare-color lines** in ChatConsole stripped to tokens; **mobile sidebar drawer** (hamburger + backdrop); 5 new test files (+47 tests).
- **RolesPage → superseded by the marketplace** (role = one category; the marketplace catalog is the install-able superset; installed agents already surface via `listProjectRoles`).

## Rejected
- **Reusing `WorkspaceSidebar`/`TopBar`** (the recon's flagged "trap"): they're the OLD operator-shell versions (team/kind grouping, `/p/` links) — the prototype's sidebar/topbar live INSIDE ChatConsole. Deleted them.
- **A per-metric backend endpoint**: one `GET /api/v1/status` covers both the cost pill and the Status view.
- **Deriving budget client-side**: today's cost is summable client-side (`cost_label`), but the 24h budget cap had no browser-readable source → the new endpoint.

## Risks
- **End-to-end UNTESTABLE in sandbox at the live-daemon level** (the 4 `ws_*` PTY tests are env-gated): the real marketplace install round-trip + the byte-faithful terminal + the live cost/status need a real daemon. Verified here by `npm build` + vitest (124) + the Rust integration tests (fake hub / temp `CcteamPaths`). **User verifies** the real marketplace install + terminal on a box.
- **The hub must be made PUBLIC** for the real marketplace browse/install (the Phase 2 github-raw deployment item) — still pending the repo owner.
- **`budget_cap_24h`** is summed from per-project `workflow.yaml` budgets (the daemon doesn't run the orchestrator); if no project configures a budget the pill shows just the cost (cap `null`).
- **eslint**: 4 pre-existing errors remain (3 ChatConsole history-seed `set-state-in-effect` + 1 `useSessionEvents`); 4c added **0** new — a future cleanup.

## Files
- **4a** (`7e0c8a6`): `ccteam-web/src/routes/status.rs` (new), `tests/status_test.rs` (new), `routes/mod.rs`, `routes/openapi.rs`, `tests/openapi_test.rs`, `Cargo.toml`/`Cargo.lock` (+ccteam-cost).
- **4b** (`3806790`): deleted 37 `web/src/**` files; `App.tsx` + `ChatConsole.tsx` rewritten.
- **4c**: NEW `web/src/lib/{marketplaceApi,statusApi,marketplaceFormat}.ts`, `web/src/pages/{MarketplaceView,StatusView}.tsx`, `web/src/components/CostPill.tsx` + 5 test files; `web/src/index.css` (tokens); `web/src/pages/ChatConsole.tsx` (wire + mobile drawer + 26-line token cleanup).

## Remaining
- **Phase 5**: docs — the unified UI (one chat shell, removed operator views), the marketplace browser, the new `GET /api/v1/status` endpoint, the `/api/v1` route list.
- **User**: make `ccteam-hub` public (real marketplace); verify the real terminal (Phase 3) + marketplace install on a real box.
- The 3 pre-existing ChatConsole react-hooks eslint errors (history-seed effects) — future cleanup.

## Gate
`cargo test --workspace --exclude ccteam-web` **1898/0**; clippy `--workspace --all-targets` **0**; fmt clean; full `ccteam-web` build (SPA via build.rs) clean; `ccteam-web` rust 229 + 4 env-gated `ws_*`; **vitest 128/128** (15 files). **Adversarial review: substantially clean, no P0/P1** — demolition complete (0 dangling refs to deleted modules), 0 bare-color literals, review-before-install body preview present, error/4-state handling solid, tests assert real behavior. **3 P2 FIXED this wave**: StatusView per-session badge now keys on `s.status` (was the global `daemon_healthy` → showed every session "live"); the `marked` body preview is now sanitized via **DOMPurify** (the hub ingests third-party `.md` verbatim → stored-XSS defense-in-depth); first-install of a never-installed plugin is gated through the drawer body preview (先看后装; `update_available` stays one-click). Note: a transient `.mcp.json` working-tree clobber (a dev-session tool side-effect, NOT Phase 4 code) was restored — a full `cargo test --workspace` run confirmed it stays canonical.
