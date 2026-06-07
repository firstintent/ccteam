# v0.8.9 Wave 5 Handoff — docs + version + final gate

> Phase 5 (the ship-gate phase) of the v0.8.9 run. Closes the version.

## Decided
- **Version `0.8.8 → 0.8.9`**: `Cargo.toml` (workspace.package) + the 4 plugin-manifest sites (`.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json` ×2, `.codex-plugin/plugin.json`) — `plugin_manifests_match_workspace_version` 2/2.
- **Tier-1 docs synced** to the shipped v0.8.9: `CLAUDE.md` (§〇 v0.8.9 + marketplace/unified-UI/byte-faithful-terminal red-lines; §一 version + baseline 1898/0 + 229+4 + vitest 128 + MCP 15 + the v0.8.9 current-state cell; §四 hub-marketplace — **156 lines, under the 200 cap**), `docs/tech-design.md` (new §插件市场 + the `GET /api/v1/status` + status routes + unified shell + **the 协议→代码 pointer-table repointed** off the deleted `role_catalog`/`role_import`/`agency_agents_catalog` onto `hub.rs`/`marketplace.rs` — closes the Phase 2 review P2), `README.md` (English, current-capability, no version timeline), `docs/usage.md` (marketplace install + unified UI + migration).
- **`docs/versions/v0-8-9/rmux-update.md` corrected**: top note that v0.8.9 shipped on rmux-sdk **0.3.1 with no dep bump** (the body's "needs 0.5" framing is superseded).
- **Archive**: `docs/versions/v0-8-9/README.md` (frozen milestone) + `wave-0..5-handoff.md`.

## Rejected
- Touching `settings.agent-team.json` / `write_project_settings_agent_team` — a settings template (not prompt content), pub-dead, left for a future cleanup (not referenced as current in any tier-1 doc).

## Risks
- Large doc rewrite — verified: CLAUDE.md ≤200 lines; README has no version/baseline/date strings; no tier-1 doc references a deleted file (`role_catalog`/`agency_agents_catalog`/`role_import`) as current (only in frozen `docs/versions/**`); MCP = 15 everywhere.

## Files
- `Cargo.toml` + `.claude-plugin/{plugin,marketplace}.json` + `.codex-plugin/plugin.json` (version).
- `CLAUDE.md`, `docs/tech-design.md`, `README.md`, `docs/usage.md`, `docs/versions/v0-8-9/{rmux-update.md, README.md, wave-5-handoff.md}`.

## Remaining
- **Git tag `v0.8.9` HELD** for user sign-off (per ship-flow) — not created.
- **User real-machine verify**: byte-faithful terminal (bug4/bug6), marketplace install round-trip, live cost/Status — sandbox has no rmux daemon / live PTY.
- **Make `ccteam-hub` public** for the github-raw marketplace read.
- Deferred cleanups listed in `docs/versions/v0-8-9/README.md`.

## Gate
Final v0.8.9 ship gate: `cargo test --workspace --exclude ccteam-web` **1898/0**; clippy `--workspace --all-targets` 0; fmt clean; `ccteam-web` 229 + 4 env-gated `ws_*`; vitest **128/128**; `doctor --verify-mcp` 15; skill-gate N/A (no `skills/`). (Numbers reconfirmed at the version-bumped + docs-synced HEAD.)
