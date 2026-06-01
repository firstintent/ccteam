# ccteam V0.8.2 — real WS IM path closure

> Released workspace version: `0.8.2`.

V0.8.2 turns the v8.1 gateway cutover into a verified real path:

- Real WS smoke covers one chat routing across multiple projects and sessions.
- Claude Code runs through the tmux TUI adapter; Codex runs through app-server JSON-RPC.
- Both harnesses can be active in the same chat; Codex `/compact` and `/review` use native RPC.
- Daemon restart resumes persisted Claude tmux and Codex app-server sessions.
- Failure handling is user-visible for gateway submit/turn timeouts, Claude tmux death, and Codex app-server disconnect.
- The legacy `ccteam-im` supervisor tick coordinator is retired from production daemon code.
- User-facing docs now describe the gateway model directly:
  `docs/quickstart.md` is the install/start/pair path,
  `docs/user-manual.md` is the operator guide, and
  `docs/troubleshooting.md` covers gateway/TG/harness failures.
- Real Telegram transport now has an opt-in round-trip gate:
  `CCTEAM_REAL_IM_TELEGRAM=1 CCTEAM_TELEGRAM_BOT_TOKEN=... CCTEAM_TELEGRAM_CHAT_ID=... bash scripts/smoke-im.sh --real`.
  It sends a unique code to the chat, waits for that exact reply, then sends a PASS ACK.

Gate evidence from the release session:

- `cargo build --workspace`
- `cargo build -p ccteam-harness`
- `cargo test --workspace --locked --no-fail-fast --exclude ccteam-web` = `1743 passed, 15 ignored`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `bash scripts/smoke-im.sh`
- `CCTEAM_REAL_CODEX_RPC=1 CCTEAM_REAL_IM_WS=1 CCTEAM_REAL_IM_WS_FAULTS=1 bash scripts/smoke-im.sh --real`
- `cargo test -p ccteam-im real_telegram_channel_roundtrip_smoke` (default skip path; live Telegram requires the env above)
- `grep -niE 'workflow.yaml|orchestrat|spawn' docs/quickstart.md docs/user-manual.md` = no matches
