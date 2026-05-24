# Install Patterns Research — 3 References + ccteam Compare

> **Read-only audit.** Purpose: inform V0.6.7 install/integration architecture decisions.
> **Date**: 2026-05-24
> **Repos surveyed**: `yvgude/lean-ctx` (2.1k★, Rust) / `rtk-ai/rtk` (53k★, Rust) / `tirth8205/code-review-graph` (17k★, Python)
> **Method**: `gh api /repos/<o>/<n>/contents` + `curl raw.githubusercontent.com` — no clone, no auth side effects.

---

## TL;DR

All three reference repos converge on a **two-step UX**: `(1) install binary via curl-piped install.sh or a package manager`, then `(2) run a single `<tool> init/setup/install` command that auto-detects every editor/agent on the host and writes per-agent MCP/hook configs in one shot`. None of them use Anthropic's `/plugin marketplace add` flow — they all bypass it by directly writing `~/.codex/config.toml`, `~/.cursor/mcp.json`, `.claude/settings.json` etc. **ccteam's biggest gap is step 2**: today ccteam ships an Anthropic plugin manifest that requires `/plugin marketplace add` (git-clone-based, fragile on macOS without CLT) and offers no `ccteam install --agent <name>` flow to bootstrap Codex / Cursor / Windsurf / Gemini-CLI directly. ccteam's biggest unique asset is its IM-bot supervisor (`ccteam-imd` / chat mode) — none of the 3 reference repos has anything comparable. Recommended V0.6.7 direction: **add `ccteam install [--agent <name>]` that bypasses `/plugin marketplace` and writes config directly** (modelled on `code-review-graph install`), **add a `SessionStart` hook that prints status + version-checks** (modelled on `code-review-graph` + `lean-ctx`), and **add `ccteam update` self-updater** using LaunchAgent/systemd timers (modelled on `lean-ctx`).

---

## Methodology

- All public-repo reads via `gh api /repos/<owner>/<name>/contents/<path>` (resolves default branch) + `curl -sL https://raw.githubusercontent.com/<owner>/<name>/<branch>/<path>`.
- No `git clone` was performed; no local checkouts mutated.
- Three repos verified public and accessible as of 2026-05-24.
- Default branches captured: lean-ctx `main`, rtk `develop`, code-review-graph `main`.
- Versions probed: lean-ctx `v3.6.16` (3.3.6 in plugin manifest — drift), rtk `v0.28.2`, code-review-graph PyPI latest.

---

## Repo 1: `yvgude/lean-ctx`

**Tagline**: "Cognitive context layer for agentic systems. 62 MCP tools, 10 read modes, 60+ shell patterns. Up to 99% token savings."

### Main language / runtime
Rust workspace (root `Cargo.toml` at `rust/Cargo.toml`, bin name `lean-ctx`). Cargo lib + bin layout (`src/lib.rs` + `src/main.rs`). Heavy dependency on `dirs` for cross-OS HOME resolution.

### Binary distribution
Most-supported install matrix of the three:
- **`install.sh` curl-pipe** (`curl -fsSL https://leanctx.com/install.sh | sh`) — POSIX `sh`, downloads pre-built tarball from GitHub Releases, **detects glibc version** (2.35+ → `gnu` else `musl`), verifies SHA256 against `SHA256SUMS`, and on macOS runs `xattr -cr` + `codesign --force --sign -` (ad-hoc) to defang Gatekeeper.
- **Cargo**: `cargo install lean-ctx` (published to crates.io, has `cargo-binstall` metadata in `[package.metadata.binstall]`).
- **Homebrew**: `brew tap yvgude/lean-ctx && brew install lean-ctx` (custom tap, not core).
- **npm**: `npm install -g lean-ctx-bin`.
- **AUR**: Arch Linux package `lean-ctx`.
- **Pi.dev**: `pi install npm:pi-lean-ctx`.

Releases workflow has 5 jobs: `build` (matrix per target) / `release` (tarball + SHA256SUMS upload) / `publish-crates` / `publish-npm` / `update-homebrew` — fully automated cross-publish on tag.

### Install step count (zero-to-useful)
2 steps: `curl ... | sh` → `lean-ctx setup`. `setup` runs an interactive wizard that auto-detects all installed editors, writes per-agent MCP configs, installs shell hook, asks one y/N question (enable auto-update?), and exits. Total time well under a minute.

### Claude Code integration
- **Plugin manifest exists**: `.claude-plugin/manifest.json` (note: **not** Anthropic's `marketplace.json` shape — it's a leaner schema with `install.command` + `mcp.command/args/env` + `skills: ["skills/lean-ctx"]` + `capabilities[]`). The manifest references the install one-liner but does NOT use Anthropic's `/plugin marketplace add` git-clone protocol — it just documents the binary install command.
- **MCP integration**: `lean-ctx --mcp` is the MCP stdio entrypoint. `lean-ctx setup` rewrites `~/.claude.json` (or `.mcp.json`) directly via `core::editor_registry::claude_mcp_json_path()` — no plugin marketplace involved.
- **Skill landing**: ships `skills/lean-ctx/SKILL.md` (159 lines, Anthropic frontmatter). Bundled in the binary as templates + extracted/symlinked by `lean-ctx setup` into user-local skill dirs (per-agent).
- **Rules injection**: `.claude/rules/lean-ctx.md` (46 lines, terse tool-mapping table) — auto-merged into `~/.claude/CLAUDE.md` via `rules_inject.rs`.

### Codex integration
First-class. `lean-ctx init --agent codex` resolves `~/.codex/` via `crate::core::home::resolve_codex_dir()` and writes both `~/.codex/config.toml` (MCP server entry under `[mcp_servers]`) and `~/.codex/AGENTS.md` (73-line tool-mapping doc, "Integration Mode: Hybrid"). No `codex exec --json` adapter — lean-ctx is invoked **by** Codex (as a tool), not the other way around. Same model as Claude Code integration: stdio MCP server.

### Lifecycle hooks
- **Shell hook** (`shell_hook.rs` + `shell_init.rs`): wraps the user's shell prompt to compress every command's output. Activation modes: `always` or `agents-only` (only when shell is owned by an AI agent process).
- **No Claude Code `SessionStart` / `PreToolUse` hook** — relies on shell-level interception.
- **macOS LaunchAgent** (`core::update_scheduler::install_macos_launchagent`): `~/Library/LaunchAgents/com.leanctx.autoupdate.plist` with `StartInterval` = `interval_hours * 3600`. Logs to `~/.lean-ctx/autoupdate-{stdout,stderr}.log`. Default interval 6h.
- **Linux systemd user timer** (`install_linux_systemd`): writes `~/.config/systemd/user/lean-ctx-autoupdate.{service,timer}`. Falls back to user cron (`install_linux_cron`) when systemd absent.
- **Windows Task Scheduler** (`install_windows_task`) for completeness.

### Binary self-update
**Yes, first-class and the most sophisticated of the three.** `lean-ctx update` re-runs the install path. `lean-ctx update --schedule on|off` toggles the OS-native scheduler. `lean-ctx setup` asks "Enable automatic updates? [y/N]" mid-wizard. State stored in `~/.lean-ctx/last-check`. Disable via env: `LEAN_CTX_NO_UPDATE_CHECK=1` or config `update_check_disabled = true`.

### Cross-platform
linux x86_64 (gnu + musl auto-select), linux aarch64, macOS arm64 + x86_64, Windows x86_64 (zip). 5 OS/arch targets shipped per release. CD pipeline matrixed.

### User-facing docs size
README: 345 lines. Companion: `ARCHITECTURE.md`, `CONTRACTS.md`, `BENCHMARKS.md`, `VISION.md`, `LEANCTX_FEATURE_CATALOG.md`, `discord-faq.md`, `cookbook/`, `blog/`. **Heavy on marketing copy** (ASCII logo, star-history graph, GIF demos). Tutorial mostly off-site at `leanctx.com/docs`.

### Notable design (steal-worthy)
1. **`detect_target()` glibc probe** — `ldd --version | head -1` parses major.minor, picks `gnu` if ≥2.35 else `musl`. ccteam currently ships only `linux-x64` (assumed gnu). Worth adopting on long-tail distros.
2. **macOS `codesign --force --sign -`** in install.sh: ad-hoc-signs the freshly downloaded binary so Gatekeeper doesn't quarantine. ccteam currently tells users to `xattr -d com.apple.quarantine` manually. lean-ctx does it preemptively in one line.
3. **OS-native scheduled auto-update** (LaunchAgent + systemd timer + cron + Task Scheduler) — clean abstraction, opt-in, logs to a stable path. This is the **single biggest gap** in ccteam's current install story.
4. **Plugin manifest schema** different from Anthropic's — sidesteps the `/plugin marketplace add` git-clone path entirely.
5. **`cargo-binstall` metadata** (`[package.metadata.binstall]`) — users with `cargo-binstall` get prebuilt binary instead of source compile. Free upgrade for Rust users.

---

## Repo 2: `rtk-ai/rtk`

**Tagline**: "CLI proxy that reduces LLM token consumption by 60-90% on common dev commands. Single Rust binary, zero dependencies."

### Main language / runtime
Rust (single crate, `Cargo.toml` at root). Most popular of the three by stars (53k). `release-please` for changelog automation, semver-strict.

### Binary distribution
- **Homebrew**: `brew install rtk` (in homebrew-core, **not** a custom tap — that's a real distribution win).
- **`install.sh`**: `curl -fsSL https://raw.githubusercontent.com/rtk-ai/rtk/refs/heads/master/install.sh | sh`. POSIX sh, 151 lines. **Resolves "latest" via HTTP 302 redirect on `/releases/latest`** (parses `Location:` header) before falling back to GitHub REST API — sidesteps the 60-req/hour anonymous rate limit. Allows `RTK_VERSION=vX.Y.Z` pin. Verifies archive contents against path-traversal (`grep -qE '^/|(^|/)\.\.(/|$)'`) before `tar -x`.
- **Cargo**: `cargo install --git https://github.com/rtk-ai/rtk` (NOT crates.io — explicit warning that `cargo install rtk` from crates.io hits a different namesake package "Rust Type Kit"). This is a published-but-not-on-crates.io pattern.
- **GH Releases**: Linux musl + gnu, macOS x86_64 + arm64, Windows x86_64 MSVC.

### Install step count
2 steps: `brew install rtk` → `rtk init -g`. `init -g` writes a Claude Code `PreToolUse` hook to `~/.claude/hooks/rtk-rewrite.sh`, patches `~/.claude/settings.json` (creates `.bak`), drops `~/.claude/RTK.md` (10 lines), adds `@RTK.md` reference in `~/.claude/CLAUDE.md`. Interactive y/N prompt; `--auto-patch` for CI.

### Claude Code integration
**No `.claude-plugin/` directory — no Anthropic plugin manifest at all.** Pure post-install configuration via `rtk init`:
- Drops shell script hook at `~/.claude/hooks/rtk-rewrite.sh` (101 lines, thin delegator that calls `rtk rewrite "$cmd"` and emits Claude's PreToolUse JSON response with `updatedInput.command`).
- Patches `~/.claude/settings.json` `hooks.PreToolUse` array (with backup).
- Drops `~/.claude/RTK.md` + `@RTK.md` reference in CLAUDE.md.
- **Hook version cache** (`$XDG_CACHE_HOME/rtk-hook-version-ok`) avoids re-checking `rtk --version >= 0.23.0` on every Bash invocation. Smart cache invalidation pattern.

### Codex integration
**Awareness-document only, no programmatic hook.** `rtk init --codex` resolves `$CODEX_HOME` else `~/.codex/` and injects `rtk-awareness.md` reference into `AGENTS.md`. Codex integration explicitly described as "prompt-level guidance — no programmatic hook" because Codex's hook model is less mature than Claude Code's. Comparison: ccteam goes further — it actually spawns `codex exec --json` and tracks costs (F173).

### Lifecycle hooks
**Claude Code `PreToolUse` only.** No `SessionStart`. The PreToolUse hook is the only mechanism. Per-agent hook scripts:
- Claude: shell script (`rtk-rewrite.sh`).
- VS Code Copilot Chat / CLI: Rust binary subcommand (`rtk hook copilot`).
- Cursor: shell script with Cursor JSON shape.
- Gemini CLI: Rust binary (`rtk hook gemini`).
- Cline / Roo / Windsurf / Codex: rules-file injection only (no programmatic hook).
- OpenCode: TypeScript plugin (`zx`, `tool.execute.before` event, in-place mutation).
- Pi: TypeScript extension.
- Hermes: Python plugin (`pre_tool_call`).

### Binary self-update
**None.** Reinstall via package manager (`brew upgrade rtk` or rerun `install.sh`). No `rtk update` subcommand. Relies on Homebrew's nightly upgrade discipline. Manual upgrade path is the only path.

### Cross-platform
linux x86_64-musl + aarch64-gnu, macOS arm64 + x86_64, Windows x86_64-MSVC. **Native Windows partially supported** (CLAUDE.md injection mode only — hook script is `.sh`, can't run natively, requires WSL for full features). Honest tradeoff documented.

### User-facing docs size
README: 490 lines, dedicated `INSTALL.md`: 397 lines, `CLAUDE.md` (for AI agents working ON rtk): 171 lines, hooks `README.md` per-agent (9 subdirs each with own README). **6-language README translation** (en/fr/zh/ja/ko/es). Most install-doc-heavy of the three. Includes name-collision warning prominently (Rust Type Kit confusion).

### Notable design (steal-worthy)
1. **HTTP redirect parsing for latest version** (`curl -sI .../releases/latest | grep Location:` → parse `/tag/<TAG>`) — completely free vs. authenticated GitHub API. ccteam currently uses the `/releases/latest` JSON API which is rate-limited.
2. **Path-traversal validation before `tar -x`** — `grep -qE '^/|(^|/)\.\.(/|$)'` rejects malicious archive entries. ccteam currently extracts blind.
3. **Hook version check cache** (`~/.cache/rtk-hook-version-ok`) — avoid spawning a `rtk --version` subprocess on every Bash hook call.
4. **`rtk rewrite` as single-source-of-truth subcommand** — all hook scripts are thin delegators that call this. Add a new rewrite rule → edit Rust registry, all 9 agent hooks pick it up. ccteam already does similar with `mcp-serve` but doesn't apply this pattern to per-agent hook scripts.
5. **`settings.json.bak` before mutation** — defensive backup pattern. ccteam currently doesn't write to user `settings.json` at all, but if V0.6.7 starts to, this pattern is the right one.
6. **`--show` flag for verification** (`rtk init --show`) — print install state without doing anything. Diagnostic gold.

---

## Repo 3: `tirth8205/code-review-graph`

**Tagline**: "Local knowledge graph for Claude Code. Builds a persistent map of your codebase so Claude reads only what matters — 6.8x fewer tokens on reviews and up to 49x on daily coding tasks."

### Main language / runtime
**Python 3.10+**. `pyproject.toml`, dist via PyPI. Single console script `code-review-graph`. Uses `uv` lock file (`uv.lock` checked in, 783 KB). Lib at `code_review_graph/`, dedicated `tools/` subdir for MCP tool implementations.

### Binary distribution
- **PyPI**: `pip install code-review-graph` / `pipx install code-review-graph` / `uvx code-review-graph` (all three documented).
- **No binary tarballs, no install.sh** — entire flow rides on PyPI.
- **Publish workflow** (`.github/workflows/publish.yml`): 32 lines, triggered on GitHub Release published, `python -m build` → `twine upload`. Trivial compared to lean-ctx's 295-line release matrix.

### Install step count
3 steps: `pip install` → `code-review-graph install` → `code-review-graph build`. `install` is the bootstrap — auto-detects 12 platforms (Codex, Claude Code, Cursor, Windsurf, Zed, Continue, OpenCode, Antigravity, Gemini CLI, Qwen, Kiro, Copilot) and configures each one's MCP config file. **Detection is filesystem-based** (e.g. `(Path.home() / ".codex").exists()`).

### Claude Code integration
Same model as lean-ctx + rtk — **bypass `/plugin marketplace`, write configs directly**:
- Writes `<repo>/.mcp.json` `mcpServers.code-review-graph = {command: "uvx", args: ["code-review-graph", "serve"]}` (or `command: "code-review-graph"` if uvx absent — runtime-adaptive).
- Writes `<repo>/.claude/skills/<name>/skill.md` × 7 skills (one per workflow: build-graph, debug-issue, explore-codebase, refactor-safely, review-changes, review-delta, review-pr) — skill files generated from a Python dict template.
- Writes `<repo>/.claude/settings.json` `hooks` block with `SessionStart` + `PostToolUse` (Write|Edit|Bash) — `settings.json.bak` backup made before mutation.
- Appends a marker-delimited `## MCP Tools: code-review-graph` section to `<repo>/CLAUDE.md` (idempotent via `<!-- code-review-graph MCP tools -->` marker).
- Installs `.git/hooks/pre-commit` (idempotent, marker-guarded, appends to existing hook if present).

### Codex integration
**Best Codex integration of the three.** `code-review-graph install --platform codex`:
- Writes `~/.codex/config.toml` `[mcp_servers.code-review-graph]` (TOML merge with backup).
- Writes `~/.codex/hooks.json` with **Codex-native** `PostToolUse` (matcher `Write|Edit|Bash`) + `SessionStart` (matcher `startup|resume`) hooks. Uses Codex's `statusMessage` field for UX. Hook entries are merged into existing hooks.json (preserving user-defined hooks), backup made.
- Appends instruction section to `AGENTS.md` (CLAUDE.md analog for Codex).

### Lifecycle hooks
**Most complete hook story of the three** (both Claude Code AND Codex):

```jsonc
// .claude/settings.json (auto-injected, merged with existing)
{
  "hooks": {
    "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "code-review-graph status --repo <root>", "timeout": 10}]}],
    "PostToolUse": [
      {"matcher": "EnterWorktree", "hooks": [{"type": "command", "command": "code-review-graph build >/dev/null 2>&1 &"}]},
      {"matcher": "Write|Edit|Bash", "hooks": [{"type": "command", "command": "code-review-graph update --skip-flows", "timeout": 30}]}
    ]
  }
}
```

**Key insight**: hooks are **incremental graph-keepers** — every file edit triggers a sub-2-second graph delta update, keeping MCP query results fresh without manual rebuild. SessionStart greets user with current graph status (file count, last build time).

### Binary self-update
**None.** `pip install -U` / `uvx --refresh` / `pipx upgrade` — relies on Python package manager. Detection of uvx-vs-pip happens at install-time and locks the MCP `command` accordingly.

### Cross-platform
Pure Python, runs everywhere with Python 3.10+. **No special install path for Windows** — same `pip install` works. By far the broadest reach with the least effort. The tradeoff: no zero-Rust-needed binary, requires `python` + `pip` already installed (less of an issue today than 5 years ago).

### User-facing docs size
README: 536 lines, 5-language translation (en/zh/ja/ko/hi). Strong on diagrams (9 PNGs in `diagrams/`). Distinct AGENTS.md (123 lines) and GEMINI.md.

### Notable design (steal-worthy)
1. **`install` is the killer command** — one bullet, every editor. `PLATFORMS` dict-of-dicts (`config_path` lambda + `detect` lambda + `key` + `format` + `needs_type`) is a clean abstraction; adding a new editor = +1 dict entry. ccteam can copy this design verbatim.
2. **`uvx` vs `pip` runtime detection** — install auto-picks the right MCP `command` based on what's on PATH. ccteam analog: if user has Rust toolchain, MCP command could be `cargo run -p ccteam-cli`; otherwise `ccteam`.
3. **Marker-guarded idempotent CLAUDE.md injection** (`<!-- code-review-graph MCP tools -->`). ccteam's install probably benefits from the same when injecting any user-file content.
4. **`.git/hooks/pre-commit` append-vs-create logic** (idempotent, marker-guarded, preserves existing hook). Best-practice pattern for shared infra.
5. **Per-platform format dispatch** (`toml` / `object` / `array`) — Zed and Continue need different JSON shapes; the install knows. ccteam likely needs similar when expanding beyond Claude Code.
6. **JSONC-tolerant parser** (`re.sub` strips `//` comments + trailing commas before `json.loads`) — Zed allows JSON-with-comments; this avoids corrupting Zed's config. Worth keeping in mind for any cross-editor config writer.
7. **Two `SessionStart` styles** — Claude SessionStart uses matcher `""` (always fires); Codex SessionStart uses matcher `startup|resume` (skips compaction events). Required for cross-vendor parity.

---

## Comparison Matrix

| 维度 | lean-ctx | rtk | code-review-graph | ccteam (current V0.6.6) |
|---|---|---|---|---|
| **主语言 / runtime** | Rust (workspace) | Rust (single crate) | Python 3.10+ | Rust (workspace) |
| **Stars / scale** | 2.1k★ / 73 kLOC equiv. | 53k★ | 17k★ | n/a (private) |
| **Binary 分发** | install.sh + brew tap + crates.io + npm + AUR + Pi.dev | brew (core!) + install.sh + cargo --git + GH Releases | PyPI (pip/pipx/uvx) | install.sh + GH Releases + cargo --git fallback |
| **install.sh 行数** | 237 (glibc-detect, codesign macOS) | 151 (302-redirect ver, path-trav guard) | n/a | 233 (SHA256 verify) |
| **install 步骤数(用户视角)** | 2 (curl\|sh → setup) | 2 (brew → init -g) | 3 (pip → install → build) | 2 (curl\|sh → /plugin install) |
| **Claude Code 集成路径** | `lean-ctx setup` writes ~/.claude.json directly | `rtk init -g` writes ~/.claude/hooks/ + patches settings.json | `code-review-graph install` writes .mcp.json + .claude/skills/ + .claude/settings.json hooks + CLAUDE.md marker injection | **Anthropic /plugin marketplace add** (git clone) — ccteam binary 0 lines run at install time |
| **Codex 集成深度** | First-class: ~/.codex/config.toml + AGENTS.md (lean-ctx as MCP server) | Awareness-doc only (no programmatic hook) | First-class: ~/.codex/config.toml + ~/.codex/hooks.json (PostToolUse + SessionStart) + AGENTS.md | F173 — `codex exec --json` + CodexExecAdapter + unified cost ledger (ccteam spawns codex) |
| **Lifecycle hooks** | Shell-level (zsh/bash prompt-wrap); no Claude SessionStart | PreToolUse only (per-agent script delegators) | SessionStart + PostToolUse(Write\|Edit\|Bash\|EnterWorktree) for Claude AND Codex; git pre-commit | **none currently** |
| **Binary self-update** | `lean-ctx update` + LaunchAgent / systemd timer / cron / Task Scheduler; opt-in via setup wizard | none — `brew upgrade` / rerun install.sh | none — `pip install -U` / `pipx upgrade` | none — rerun install.sh |
| **跨平台** | linux x86_64 (gnu+musl), linux aarch64, macOS arm64+x86_64, Windows x64 | linux x86_64-musl, linux aarch64-gnu, macOS arm64+x86_64, Windows x64-MSVC (limited) | All Python platforms | linux x86_64, macOS arm64+x86_64; Windows out of scope (WSL) |
| **macOS Gatekeeper 处理** | `xattr -cr` + `codesign --force --sign -` in install.sh | none (relies on brew tap) | n/a (Python) | manual `xattr -d` documented |
| **`.claude-plugin/` 存在** | yes — non-Anthropic schema (install.command + mcp + skills + capabilities) | **no** — no plugin manifest at all | no | **yes — Anthropic schema** (marketplace.json + plugin.json) |
| **MCP tool count** | 62 | 0 (rtk is hook-driven, not MCP) | ~10 (graph query tools) | 27 |
| **Agent matrix breadth** | 30 agents auto-detect | 9 agents (Claude/Copilot/Cursor/Cline/Windsurf/Codex/OpenCode/Pi/Hermes) | 12 agents (Claude/Codex/Cursor/Windsurf/Zed/Continue/OpenCode/Antigravity/Gemini/Qwen/Kiro/Copilot) | 1 (Claude Code only; Codex spawned as subprocess) |
| **README 行数** | 345 | 490 | 536 | n/a |
| **README 多语言** | en only | en + fr/zh/ja/ko/es | en + zh/ja/ko/hi | en (per CLAUDE.md red line) |
| **ccteam 独有(他们无)** | n/a | n/a | n/a | IM bot supervisor (`ccteam-imd` 24/7 chat) / multi-vendor orchestration (Claude×Codex mix) / workflow.yaml-declared agent topology / file-system-as-control-plane / squad routing |

---

## Recommendations for ccteam V0.6.7

Ranked by impact × effort. Each item references a concrete pattern from the matrix.

### Tier 1: Critical (close the install gap)

#### R1. Add `ccteam install [--agent <name>]` direct-config bootstrap (Effort: M, 1-2 days)

**Borrow from**: `code-review-graph install` (cleanest implementation) + `rtk init -g` (settings.json mutation w/ backup).

Today ccteam relies on Anthropic's `/plugin marketplace add` flow, which:
- Requires `git clone` — broken on macOS without Command Line Tools (xcrun complaint).
- Only registers ccteam to Claude Code — does nothing for Cursor / Windsurf / Codex / Gemini-CLI.
- Re-runs cost 30s+ git ops every `/plugin marketplace update`.

Add a `ccteam install` subcommand that:
1. Auto-detects installed agents (mirror code-review-graph's `PLATFORMS` dict with `detect: lambda: Path.home() / ".codex").exists()` style).
2. For each detected agent, writes the appropriate MCP config (`.mcp.json` / `~/.codex/config.toml` / `~/.cursor/mcp.json` / `~/.config/zed/settings.json`) **directly**, with backup-before-write.
3. For Claude Code: also drop the 7 skills into `~/.claude/skills/<plugin>/<skill>/SKILL.md` (or use the existing plugin path; either way bypasses `/plugin marketplace`).
4. Idempotent: re-running `ccteam install` is a no-op or upgrade.
5. `ccteam install --show` (a la `rtk init --show`) prints current install state across agents.

Keep `.claude-plugin/marketplace.json` shipped for users who **do** prefer the Anthropic plugin path — make it a documented alternative, not the primary.

**Why this is #1**: the macOS-no-CLT case is presently a hard install failure; this fixes it. Plus it opens ccteam to Codex/Cursor/Zed users in a single move.

#### R2. Add `SessionStart` Claude Code hook (Effort: S, half-day)

**Borrow from**: `code-review-graph` `hooks/hooks.json` (5 lines) — fires `code-review-graph status` on every session start, telling the model what graph state exists.

ccteam analog:
- `SessionStart` matcher `""` → `ccteam status --json` (or similar): prints workflow registry, active bots, last 24h cost ledger summary, IM bot health. Forces the model to ground itself in the actual ccteam state before responding.
- Optional: `SessionStart` could also self-version-check (compare installed `ccteam --version` against `gh api /releases/latest` cached) and surface "ccteam vX.Y.Z+1 available — run `ccteam update`".

CLAUDE.md red line check: this doesn't violate "no prompt injection" — it's read-only state surfacing via Anthropic's sanctioned hook mechanism, same as docs/tech-design recommends for status awareness. **Confirmation needed**: §三红线 mentions "agent 行为住 `.claude/agents/<role>.md`,**不**向 tmux pane 注入 system prompt" — this is mode-3 specific. SessionStart is Claude-Code session-wide and is the harness-level hook. Likely OK; flag for review.

#### R3. Add `ccteam update` self-updater + LaunchAgent / systemd timer (Effort: M, 1-2 days)

**Borrow from**: `lean-ctx` `core::update_scheduler` (best-in-class abstraction across 4 OSes; ~400 LOC pulled almost verbatim).

Today: ccteam binary is install-and-forget. `/plugin marketplace update` doesn't bump the binary. Users running V0.6.0 don't know V0.6.6 exists.

`ccteam update`:
1. Resolves latest tag (use rtk's 302-redirect trick to avoid GH API rate-limit).
2. Verifies SHA256 (already implemented in install.sh; extract into a Rust helper).
3. Replaces the binary atomically.
4. Optional: `ccteam update --schedule on` → installs LaunchAgent (`~/Library/LaunchAgents/io.firstintent.ccteam.autoupdate.plist`) on macOS, systemd-user timer (`~/.config/systemd/user/ccteam-autoupdate.timer`) on Linux. Default off; opt-in during a future `ccteam install` wizard.
5. If the user **never** runs `ccteam install`, R2's SessionStart hook can show "update available" passively.

### Tier 2: Nice-to-have (polish)

#### R4. install.sh polish: glibc auto-detect + macOS codesign + path-traversal guard (Effort: S, 2-4h)

Three small fixes pulled from lean-ctx + rtk into ccteam's existing 233-line install.sh:
- **glibc probe** (lean-ctx lines 76-86): on linux, parse `ldd --version` major.minor; pick `gnu` if ≥2.35 else publish a `musl` tarball. Today ccteam only ships `linux-x64` (assumed gnu); breaks on Alpine, old Debian. ~10 LOC change.
- **macOS ad-hoc codesign** (lean-ctx lines 162-165): `xattr -cr "$binary" && codesign --force --sign - "$binary"` after extract. Defangs Gatekeeper preemptively instead of telling user to `xattr -d` later. ~3 LOC.
- **tar path-traversal guard** (rtk line 104): `tar -tzf "$ARCHIVE" | grep -qE '^/|(^|/)\.\.(/|$)' && error` before extract. ~2 LOC defensive depth.

#### R5. PyPI / npm / brew secondary publish (Effort: L, multi-day; defer to V0.7+)

lean-ctx ships via 6 channels; rtk on brew-core; ccteam currently 1 + cargo fallback. Worth it for adoption but not urgent — install.sh + cargo install covers >90% of intended user base. Defer.

### Tier 3: Compose-mode opportunities

#### R6. Reuse rtk's `rewrite` pattern for ccteam's tmux pane scraping (out-of-scope-now)

**Not directly install-related** but worth flagging: rtk's "thin delegator hooks call a single Rust subcommand" is a clean pattern for ccteam's per-vendor adapters (claude/codex). Currently `ccteam-core` does this internally for vendors; the same pattern could expose hook-level integration with Cursor/Zed users who want ccteam orchestration without Claude Code. Far-future, V0.8+.

---

## Open Questions

1. **Plugin manifest schema**: ccteam ships Anthropic's `.claude-plugin/marketplace.json + plugin.json` pair. lean-ctx ships its own `.claude-plugin/manifest.json` with leaner shape. Should V0.6.7 keep **both** (marketplace.json for users who insist on `/plugin marketplace add`, direct-config for users on `ccteam install`)? Or drop the marketplace path entirely once direct-config lands? CLAUDE.md §五.3 "deprecated 直接删,breaking rename 不留 alias" suggests dropping is fine pre-v1.0.

2. **SessionStart vs no-prompt-injection red line**: §三红线 says "agent 行为住 `.claude/agents/<role>.md`,**不**向 tmux pane 注入 system prompt" but that's mode-3 specific. Claude Code's `SessionStart` hook fires once per session and prints to the tool's stdout (model sees as system tool output) — is that "prompt injection" by this rule? Need a §三 update to clarify "harness-level hooks emitting state are OK; persona/system-prompt injection is not".

3. **macOS no-CLT**: did anyone validate that the existing install.sh works without Command Line Tools? `tar`, `curl`, `sha256sum/shasum`, `mktemp` are in macOS base, so likely yes — the CLT issue is purely the `/plugin marketplace add` git clone. Worth a host-probe before finalizing R1's framing.

4. **Auto-update schedule defaults**: lean-ctx asks during setup (`auto_update: y/N`). What's ccteam's stance? IM-bot supervisor is long-running; mid-session binary swap could break tmux sessions. Likely default off, but explicit answer needed.

5. **`uvx`-style runtime adapter**: code-review-graph picks `uvx` over `code-review-graph` based on PATH. ccteam analog: if user has Rust toolchain, MCP `command` could be `cargo run -p ccteam-cli` (dev users), else compiled `ccteam`. Worth it? Probably no — adds confusion, dev users can override via `CCTEAM_BIN`.

6. **Bypass `/plugin marketplace` entirely?**: rtk does this — no `.claude-plugin/` at all. If V0.6.7 ships `ccteam install`, is the plugin manifest still useful? Marketing footprint at marketplace listings may matter; check if Anthropic surfaces ccteam-marketplace in any directory.

---

## References

- lean-ctx repo: https://github.com/yvgude/lean-ctx (commit at time of audit: HEAD of `main`)
- rtk repo: https://github.com/rtk-ai/rtk (commit at time of audit: HEAD of `develop`)
- code-review-graph repo: https://github.com/tirth8205/code-review-graph (commit at time of audit: HEAD of `main`)
- ccteam current install.sh: `/install.sh` (F166 zero-Rust installer)
- ccteam plugin manifest: `/.claude-plugin/marketplace.json` + `/.claude-plugin/plugin.json`
- ccteam current MCP config: `/.mcp.json`
