# v0.8.9 — plugin marketplace + unified web UI + byte-faithful terminal + zero-prompt engine

> Frozen milestone archive. Current architecture lives in `CLAUDE.md` + `docs/tech-design.md`; this is the v0.8.9 record. Scope SoT: `prd.md` + `marketplace-design.md` + `rmux-update.md` + `dev-prompt.md` + `prototype.html`. Per-phase detail: `wave-0..5-handoff.md`.

## One line
Turned ccteam into a **pure engine + curated plugin marketplace**: the repo carries zero prompt content (only `cto_role.md`), role/skill/workflow plugins live in **`ccteam-hub`** and install into projects via a browsable marketplace; the web SPA collapsed into **one chat-style shell**; and the default-rmux web terminal became **byte-faithful**.

## The 6 phases (run direct-on-`dev`, no PR; ccteam-hub on `main`)
- **Phase 0 — zero-prompt-content + dead-link cleanup** (`7d9e86c`/`40db1d1`/`8162a35`): deleted the legacy agent-team/meta-agent prompt templates + `meta_agent.rs` + root `agents/`+`workflows/`; retired `InitMode::AgentTeam` (no `--mode`); removed the dead `chat_history`/`chat_send_input` MCP tools (**17→15**, chat 6→4) + the dead `BotSupervisor`/`outbound`/cross-bot chain; added the **zero-prompt-content red-line** (CLAUDE.md §三).
- **Phase 1 — ccteam-hub fill + ingestion** (hub `ce8b485`): `sources.json` + idempotent `scripts/sync.py` + GH Action vendor agency-agents (`wshobson/agents` @ pinned sha, MIT, **192 agents**) verbatim into the hub + `index.json`.
- **Phase 2 — marketplace backend + install** (`ed55325`/`2eb6458`/`ccdcfad` + review-fix `d547613`): ccteam reads the hub `index.json` (github-raw + `~/.ccteam/hub-cache/`), **sha256-verifies** plugin bodies, installs into project `.claude/{agents,skills}/`; 4 REST routes; CLI `role search/add` repointed to the hub; deleted the bundled catalog + `role_import`.
- **Phase 3 — byte-faithful rmux terminal** (`10d3694`): `rmux_backend` streams raw pane bytes (`output_stream()`/`PaneOutputChunk::Bytes`) + `capture` drains the `Oldest` backlog → fixes v0.8.8 bug4 (blank-on-connect) + bug6 (line-wrap). The byte API was already in rmux-sdk **0.3.1** (the "needs 0.5" assumption was stale), so the fix shipped on 0.3.1; **post-run the rmux pin was bumped to 0.5** per user request — clean additive, call-sites byte-identical, zero drift.
- **Phase 4 — unified web UI** (`7e0c8a6`/`3806790`/`7845d1b`): one chat-style shell (`ChatConsole`); deleted the legacy operator UI (37 files); marketplace browser + Status view + cost pill; new `GET /api/v1/status`; vendor design tokens; mobile drawer.
- **Phase 5 — docs + version + gate**: tier-1 docs synced; version `0.8.8 → 0.8.9`; this archive + the wave handoffs.

## Architecture deltas (vs v0.8.8)
- **New red-line**: ccteam repo carries zero plugin prompt content (role/agent/skill/workflow) — only `cto_role.md` (the bootstrap exception); everything else lives in `ccteam-hub`.
- **Plugin marketplace** triangle: ccteam (engine/UI) ↔ ccteam-hub (curated index + content; ingests open source) ↔ user project (`.claude/`). Reads hub over HTTPS + local cache; sha256 integrity; install = `write_role`/`write_skill`.
- **Web UI** = one unified chat shell (no operator/chat fork); bottom nav 插件市场 / Status / Settings; `GET /api/v1/status` powers the cost pill + Status view.
- **Terminal**: default rmux is byte-faithful (no tmux backend needed for fidelity).
- **MCP surface 15** (admin 3 / chat 4 / advise 2 / session 5 / screenshot 1).
- **Retired**: `InitMode::AgentTeam` init mode, the bundled `agency_agents_catalog.json` + direct-github `role_import`, the dead supervisor/outbound chain, the legacy operator SPA views.

## Final gate
`cargo test --workspace --exclude ccteam-web` **1898/0** (down from v0.8.8's 1998 — net from intentional dead-code deletion, fail=0); clippy `--workspace --all-targets` 0; fmt clean; `ccteam-web` 229 + 4 env-gated `ws_*` (tmux pipe-pane PTY, CI/专机); vitest **128/128**; `doctor --verify-mcp` 15; skill-gate N/A (no `skills/`). Adversarial review per phase: no P0/P1 (P2s fixed or deferred — see the wave handoffs).

## Deferred / user-verify (NOT done in this version)
- **User real-machine verification** (sandbox has no rmux daemon / live PTY): the byte-faithful terminal (bug4/bug6 actually fixed) + the marketplace install round-trip + the cost/Status live data — verify on a real rmux box.
- **Make `ccteam-hub` public**: the marketplace's github-raw read needs the hub public (it's a public marketplace by design — MIT plugins).
- **GitHub Actions Workflow permissions** on the hub for auto re-sync (the user enabled this mid-run).
- Deferred cleanups: the pub-dead `settings.agent-team.json`/`write_project_settings_agent_team`, the `--restart-team` residue in the (unreachable) ccteam-flow orchestrator, the harness `marker_reporter`/`inbound::process_inbound*` dead fns, the 3 pre-existing ChatConsole react-hooks eslint warnings.
- The `ccteam-flow` orchestrator remains deferred (daemon doesn't run it).

## Migration (pre-v1.0 — no compat shim)
Clear `~/.ccteam` + each project `.ccteam/` and re-`init`. Prompt content moved to `ccteam-hub` (the in-repo catalog is gone); `ccteam init` no longer has `--mode`.
