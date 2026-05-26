# V0.8 rmux Integration — Branch-Local Progress Log

> **Branch**:`v0-8-rmux-integration`(off origin/main `446e33a`)
> **Goal**:100% rmux integration to production-grade; mode 1/2/3 child supervision unified under `MuxBackend` trait + embedded rmux daemon via `ccteam mux daemon` re-exec.
> **No PR / no release** — continuous development on this branch.
> **Design SoT**:`docs/research/embedded-mux-unified-architecture.md`(1571 lines, on main)
> **Worktree**:`/tmp/ccteam-rmux/`(references/ symlinked to main checkout's `references/`)

---

## Wave plan(from research §六 + §13.8 + §15.6)

| Wave | Subject | Status |
|---|---|---|
| **W0** | spike — Cargo dep + tmux surface audit + rmux SDK smoke | in progress |
| W1 | MuxBackend trait + TmuxBackend wrap(behavior-equivalent) | pending |
| W2 | RmuxBackend + `ccteam mux daemon` re-exec + Claude mode 3a + 10 base patterns | pending |
| W3 | mode 2 bg(Claude + Codex)into mux + Codex 10 patterns + EnrichedEvent merger | pending |
| W4 | typed events → progress.jsonl bridge + Codex app-server in mux(mode 3b)| pending |
| W5 | `ccteam attach` via rmux-client(cross-mode + Windows ConPTY) | pending |
| W6 | macOS + Windows CI matrix + Claude Code hook → daemon UDS reroute | pending |
| W7 | flip default to rmux + doc sync + production polish | pending |

## Acceptance gates per wave

- Each wave keeps cargo test `--workspace --exclude ccteam-web` baseline ≥ **1549/1**(CLAUDE.md §一)
- Clippy `--workspace --all-targets --locked -- -D warnings` 0 errors + 0 warnings
- `cargo fmt --all -- --check` clean
- New code includes integration tests behind `--feature rmux` flag where applicable

## Open log

(Append entries here as waves progress.)
