#!/usr/bin/env bash
# v0.8 rmux G2 — real-daemon smoke for the rmux MuxBackend.
#
# Exercises spawn → exists → send → capture → kill through a LIVE rmux
# daemon. ccteam hosts the daemon itself: the rmux SDK re-execs the
# ccteam binary via `--__internal-daemon <socket>`, so NO system `rmux`
# binary is needed — only the built ccteam binary, pointed at by
# `RMUX_SDK_DAEMON_BINARY`.
#
# The roundtrip test (`rmux_backend_session_roundtrip.rs`) is `#[ignore]`
# (it needs a PTY-capable env + a real daemon), so we run it explicitly
# with `-- --ignored`. The test spawns `sh -c "echo hello && sleep 30"`,
# so no `claude`/`codex` binary is required — it runs in a headless CI
# container.
#
# Usage:
#   scripts/rmux-smoke.sh
#
# Linux-only for now (macOS/Windows PTY paths deferred per the audit).

set -euo pipefail

# Resolve the workspace root (this script lives in <root>/scripts/).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> rmux smoke: building ccteam binary (debug, --locked)"
cargo build -p ccteam-cli --locked

BIN="$ROOT/target/debug/ccteam"
if [[ ! -x "$BIN" ]]; then
  echo "FATAL: expected ccteam binary at $BIN after build, not found" >&2
  exit 1
fi
echo "==> ccteam binary: $BIN"

echo "==> running rmux real-daemon roundtrip (--ignored)"
RMUX_SDK_DAEMON_BINARY="$BIN" \
  cargo test -p ccteam-mux --test rmux_backend_session_roundtrip --locked \
  -- --ignored --nocapture

echo "==> rmux smoke: PASS"
