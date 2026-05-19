#!/bin/sh
# ccteam hook dispatcher — V0.6.1 F139.
#
# Claude Code per-hook entry. Instead of paying the ~200 ms `ccteam`
# Rust binary startup per hook firing (4 hooks × ~1.5 turns/sec was a
# user-visible 1+ s of chat sluggishness), this script POSTs the hook
# payload to the long-running ccteam daemon's axum HTTP server. ~10 ms
# round-trip vs ~200 ms cold start ⇒ ~20× faster.
#
# Usage:
#   hook.sh <kind> [<action>]
#
# `<kind>` is the ccteam hook subcommand (`progress-append` /
# `load-context` / `intercept-ask` / `chat-progress`). `<action>` is the
# subcommand argument (`Stop`, `PostToolUse`, `user-prompt`, ...) when
# the hook takes one. `load-context` and `intercept-ask` take no
# argument; the script forwards whatever the call site passed verbatim.
#
# Fallback path: when the daemon is unreachable (token file missing,
# connection refused, HTTP error), the script execs the original
# `ccteam internal hook <kind> [<action>]` binary so behaviour is
# preserved while the user has no daemon running.

KIND="$1"
ACTION="$2"

if [ -z "$KIND" ]; then
    echo "hook.sh: missing <kind> argument" >&2
    exit 2
fi

CCTEAM_HOME="${CCTEAM_HOME:-$HOME/.ccteam}"
TOKEN_FILE="$CCTEAM_HOME/web-token"
PORT="${CCTEAM_WEB_PORT:-7331}"

if [ -n "$ACTION" ]; then
    URL="http://127.0.0.1:${PORT}/internal/hook/${KIND}/${ACTION}"
else
    URL="http://127.0.0.1:${PORT}/internal/hook/${KIND}"
fi

# Try the daemon fast path when a token is on disk. Buffer stdin to a
# tempfile so the fallback can replay it if curl fails (stdin pipes are
# single-use).
if [ -r "$TOKEN_FILE" ]; then
    TOKEN=$(cat "$TOKEN_FILE" 2>/dev/null)
    if [ -n "$TOKEN" ]; then
        TMP=$(mktemp 2>/dev/null) || TMP="/tmp/ccteam-hook-$$"
        # shellcheck disable=SC2064  # we want $TMP expanded now
        trap "rm -f \"$TMP\"" EXIT INT TERM
        cat > "$TMP"
        if curl -sS --max-time 5 --connect-timeout 1 -f \
                -H "Authorization: Bearer ccteam:${TOKEN}" \
                -H "Content-Type: application/json" \
                --data-binary "@${TMP}" \
                "$URL" 2>/dev/null; then
            exit 0
        fi
        # Daemon unreachable / 4xx / 5xx — fall back through the CLI
        # with the buffered stdin so behaviour matches the pre-F139 path.
        if [ -n "$ACTION" ]; then
            ccteam internal hook "$KIND" "$ACTION" < "$TMP"
        else
            ccteam internal hook "$KIND" < "$TMP"
        fi
        exit $?
    fi
fi

# No token file ⇒ daemon not running with auth (or never started).
# Direct CLI dispatch is the legitimate slow path.
if [ -n "$ACTION" ]; then
    exec ccteam internal hook "$KIND" "$ACTION"
fi
exec ccteam internal hook "$KIND"
