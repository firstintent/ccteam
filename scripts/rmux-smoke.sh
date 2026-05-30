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
# Portable to Linux and macOS (both PTY-capable; uses only bash + cargo,
# no GNU-isms). CI runs it on an [ubuntu-latest, macos-latest] matrix —
# the macOS leg is the sole automated Darwin validation (audit G8 /
# flip-default gate). Windows ConPTY is deferred to a later wave.

set -euo pipefail

# Resolve the workspace root (this script lives in <root>/scripts/).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> rmux smoke: building ccteam binary (debug, --locked)"
cargo build -p ccteam-cli --locked

# Resolve the binary, accounting for the Windows `.exe` suffix so the
# script is portable across the CI matrix (Linux/macOS = `ccteam`,
# Windows = `ccteam.exe`).
BIN="$ROOT/target/debug/ccteam"
if [[ ! -x "$BIN" && -x "$BIN.exe" ]]; then
  BIN="$BIN.exe"
fi
if [[ ! -x "$BIN" ]]; then
  echo "FATAL: expected ccteam binary at $BIN (or .exe) after build, not found" >&2
  exit 1
fi
echo "==> ccteam binary: $BIN"

echo "==> running rmux real-daemon roundtrip (--ignored)"
RMUX_SDK_DAEMON_BINARY="$BIN" \
  cargo test -p ccteam-mux --test rmux_backend_session_roundtrip --locked \
  -- --ignored --nocapture

# Adapter-layer coverage: drives the production spawn path
# (ClaudeBgAdapter -> default_backend() -> rmux daemon -> live session)
# so the composition is exercised, not just the bare trait. Uses a fake
# `claude` (sleep) under an isolated per-test HOME, so no real claude is
# needed. CCTEAM_TEST_BIN points the binary locator at the built ccteam.
echo "==> running rmux adapter-layer spawn coverage (--ignored)"
CCTEAM_TEST_BIN="$BIN" RMUX_SDK_DAEMON_BINARY="$BIN" \
  cargo test -p ccteam-core --test claude_bg_rmux_adapter_test --locked \
  -- --ignored --nocapture

# Typed-event pipeline coverage: with CCTEAM_TYPED_EVENTS=1 a live rmux
# session's rate-limit pane line is mirrored into progress.jsonl as a
# `typed_event` row. The end-to-end test is `#[ignore]` (needs a real
# daemon + PTY), so run it explicitly with `-- --ignored`.
echo "==> running typed-event pipeline coverage (--ignored)"
CCTEAM_TEST_BIN="$BIN" RMUX_SDK_DAEMON_BINARY="$BIN" \
  cargo test -p ccteam-core --test typed_event_pipeline_test --locked \
  -- --ignored --nocapture

echo "==> rmux smoke: PASS"
