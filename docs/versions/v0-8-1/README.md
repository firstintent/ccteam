# ccteam V0.8.1 — v8.1 cloud CC/Codex + IM gateway cutover

> **Scope**: v8.1 architecture vertical slice. The plan label was `v8.1`;
> the released workspace version is `0.8.1`.

---

## 1. What Shipped

V0.8.1 moves ccteam from the old "orchestrator + IM supervisor tick" shape to
a resident routing gateway for cloud Claude Code / Codex sessions:

- Rename cleanup: `ccteam-mux` is now `ccteam-harness`, `MuxBackend` is
  `ProcessBackend`, and `ccteam-imd` is `ccteam-im`.
- Layering cleanup: orchestration code lives in `ccteam-flow`; `ccteam-core`
  remains primitives. Keep topology `core -> harness -> cost`.
- Execution boundary: `HarnessAdapter` drives the vendor protocol and
  `ProcessBackend` hosts the process. Tmux pane operations stay in the
  `PaneBackend` subtrait.
- Claude adapter: tmux TUI sessions run with `--dangerously-skip-permissions`,
  send literal user content through `tmux send-keys -l`, and emit neutral
  `CanonicalEvent` values.
- Codex adapter: app-server RPC uses Codex-native methods; `/compact` maps to
  `thread/compact/start`, and `/review` maps to `review/start`.
- IM gateway: inbound messages route directly through `Gateway`; gateway owns
  spawn-on-demand session lifecycle for `/pair`, `/new`, registered bot
  templates, and `@handle` routing.
- Daemon: no-slug `ccteam start` runs web + IM gateway + embedded MCP Unix
  socket + optional hook sink in one process. It does not construct or tick a
  `ccteam-flow` orchestrator.
- Init: `ccteam init` writes project-owned `.ccteam/{agents,skills,state.json}`
  and still writes Claude Code agent files under `.claude/agents`.
- Progress bridge: `harness/progress_bridge` is the single schema authority;
  `core` re-exports it for compatibility.

## 2. Runtime Contract

`ccteam start` is now a gateway daemon:

- IM messages are accepted, authorized, and handed to `Gateway` immediately.
- The legacy supervisor tick/outbox safety-net path is not on the daemon hot
  path.
- MCP remains available through stdio via `ccteam mcp-serve`; the daemon also
  binds `~/.ccteam/run/mcp.sock` for line-delimited JSON-RPC clients.
- `ccteam stop`, SIGTERM, and Ctrl-C share one shutdown path. `ccteam stop`
  writes the target daemon PID into `/tmp/ccteam-$USER.shutdown`, avoiding
  parallel-daemon test races.
- Long-running chat sessions remain external resources; daemon shutdown does
  not kill them.

## 3. Deferred

- Human approval over IM remains deferred. `ApprovalIR` is a neutral type
  boundary, but v8.1 runs skip-permissions and has no approval loop.
- `ccteam-flow` orchestration is not part of the IM gateway daemon. Future
  orchestration work should build on the separated crate, not reintroduce a
  tick loop into `ccteam-im`.
- Non-Unix daemon MCP transport is deferred; `mcp-serve` stdio remains the
  portable transport.

## 4. Gates

Local phase gates run during the cutover:

- `cargo test -p ccteam-harness claude_tui`
- `cargo test -p ccteam-im`
- `cargo clippy -p ccteam-im --all-targets -- -D warnings`
- `cargo test -p ccteam-cli --test start_with_imd_test`
- `cargo test -p ccteam-cli --test graceful_shutdown_test`
- `cargo clippy -p ccteam-cli --all-targets -- -D warnings`
- `bash scripts/smoke-im.sh`
- `cargo fmt --all -- --check`

Ship gate remains the workspace baseline in `CLAUDE.md`: workspace tests with
`--exclude ccteam-web`, clippy `-D warnings`, and fmt clean.
