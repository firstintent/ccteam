#!/bin/sh
# cswap-auto.sh — `cswap auto` that can also land on an account a `cswap run`
# session is using.
#
# `cswap auto` never switches onto an account while a Claude process lives in
# that account's session profile (anything started with `cswap run <account>`,
# by hand or by a tool). With several such sessions around it can run out of
# accounts to move to and stay on one that is at its limit.
#
# This loop leaves every decision to cswap and only adds the last step:
#   1. each interval it runs `cswap auto --once` (your options passed through);
#   2. when cswap wanted to switch but could use none of its candidates
#      (exit 3, `no-viable-target`), it asks cswap which account it would
#      have picked (`cswap auto --once --dry-run`, which ranks the accounts
#      without that in-use check) and switches there with `cswap switch`, the
#      manual switch, which goes ahead with a warning where auto steps aside.
#
# What a forced switch costs — this is cswap's own warning, not removed here:
# the account is then the default login AND still held by the running
# session, two copies of one credential, and when the server rotates the
# token the other copy stops working. If that session later fails to
# authenticate, exit it and start it again with `cswap run <account>` (it
# then runs on the default login). cswap refuses the switch outright when the
# session has already rotated the account's token; the only way to use that
# account is to exit the session, and the loop waits CSWAP_AUTO_FORCE_BACKOFF
# before trying it again.
#
# Needs only cswap and a POSIX shell. Nothing here talks to any other program.
#
# Usage:   cswap-auto.sh [cswap auto options...]
#          (run it instead of `cswap auto`; do not run both)
# Example: cswap-auto.sh --threshold 85 --strategy weekly-first
#
# Environment:
#   CSWAP_AUTO_INTERVAL        seconds between checks (default 60, minimum 15)
#   CSWAP_AUTO_FORCE_COOLDOWN  seconds after a forced switch before another
#                              may be forced (default 300, cswap's own
#                              cooldown between proactive switches)
#   CSWAP_AUTO_FORCE_BACKOFF   seconds to wait after cswap refused a forced
#                              switch (default 600)
#   CSWAP_BIN                  cswap executable (default: cswap)
set -u

seconds() { # name value
    case $2 in
        '' | *[!0-9]*) echo "cswap-auto: $1 must be a number of seconds" >&2; exit 64 ;;
    esac
}
interval=${CSWAP_AUTO_INTERVAL:-60}
cooldown=${CSWAP_AUTO_FORCE_COOLDOWN:-300}
backoff=${CSWAP_AUTO_FORCE_BACKOFF:-600}
seconds CSWAP_AUTO_INTERVAL "$interval"
seconds CSWAP_AUTO_FORCE_COOLDOWN "$cooldown"
seconds CSWAP_AUTO_FORCE_BACKOFF "$backoff"
[ "$interval" -ge 15 ] || interval=15
cswap=${CSWAP_BIN:-cswap}

for arg in "$@"; do
    case $arg in
        --once | --interval | --interval=* | --dry-run)
            echo "cswap-auto: $arg is not supported here (set CSWAP_AUTO_INTERVAL; use \`cswap auto $arg\` directly)" >&2
            exit 64
            ;;
    esac
done
command -v "$cswap" >/dev/null 2>&1 || { echo "cswap-auto: $cswap not found on PATH" >&2; exit 127; }

log() { printf '%s cswap-auto: %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >&2; }

# One `cswap auto --once`; its output is passed through and kept in $out.
tick() {
    "$cswap" auto --once "$@" >"$out"
    status=$?
    cat "$out"
    return "$status"
}

# The slot cswap's own ranking would switch to, in-use accounts included.
# A dry run decides and stops: it refreshes no token and writes no state.
would_switch_to() {
    "$cswap" auto --once --dry-run --json "$@" 2>/dev/null |
        sed -n '/"event": *"switch"/s/.*"to": *{"number": *\([0-9][0-9]*\).*/\1/p' |
        head -n 1
}

out=$(mktemp "${TMPDIR:-/tmp}/cswap-auto.XXXXXX") || exit 1
trap 'rm -f "$out"; exit 0' INT TERM
force_after=0
while :; do
    tick "$@"
    if [ $? -eq 3 ] && grep -q 'no-viable-target' "$out" && [ "$(date +%s)" -ge "$force_after" ]; then
        target=$(would_switch_to "$@")
        if [ -n "$target" ]; then
            log "no free account; cswap's pick, Account-$target, is in use by a cswap run session: switching to it anyway"
            if "$cswap" switch "$target"; then
                force_after=$(($(date +%s) + cooldown))
            else
                force_after=$(($(date +%s) + backoff))
                log "cswap refused Account-$target; exit the session using it to free the account (next attempt in ${backoff}s)"
            fi
        fi
    fi
    sleep "$interval" &
    wait $!
done
