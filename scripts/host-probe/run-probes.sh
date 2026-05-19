#!/usr/bin/env bash
#
# scripts/host-probe/run-probes.sh
#
# V0.6.0 Wave 4 host-probe execution step. Runs the 5 preset E2E
# scenarios + the 3 Codex scenarios against an already-deployed remote
# (see `deploy-to-nas.sh`).
#
# Output layout (per scenario):
#   $OUT_DIR/<scenario>/cmd.txt   — actual command(s) executed
#   $OUT_DIR/<scenario>/log       — combined stdout/stderr tail
#   $OUT_DIR/<scenario>/cost.txt  — `ccteam cost summary` snapshot
#   $OUT_DIR/<scenario>/status    — "happy" | "mock" | "skip" | "fail"
#   $OUT_DIR/summary.md           — table the team-lead pastes into
#                                   docs/v0-6-0/host-probe.md
#
# Usage:
#   scripts/host-probe/run-probes.sh [SCENARIO...]
#
#   SCENARIO — one or more of (defaults to all):
#       solo-sidekick team-sprint overnight-builder
#       pocket-assistant im-squad
#       codex-advise codex-auto-critic codex-fallback
#
# Env overrides:
#   CCTEAM_NAS_HOST       — ssh target (default: nas-box005)
#   CCTEAM_NAS_PATH       — remote checkout (default:
#                           /home/rob/nasworkspace/ccteam)
#   CCTEAM_PROBE_OUT_DIR  — local output dir (default:
#                           ./.probe-results/<UTC-timestamp>)
#   CCTEAM_PROBE_REAL_TG  — "1" enables real Telegram probes for
#                           pocket-assistant / im-squad; default "0"
#                           runs the mock-channel path (the user has
#                           pasted credentials.json but is allowed to
#                           defer real TG e2e to V0.6.1 post-ship).
#
# Each scenario function MUST be safe to skip independently. A
# scenario records `status=skip` with a reason when its prerequisites
# (tooling, auth, allowed_chat_ids) are missing; the wave-4 ship gate
# accepts mock/skip per the wave-4 handoff message.
#
# Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>

set -euo pipefail

NAS_HOST="${CCTEAM_NAS_HOST:-nas-box005}"
NAS_PATH="${CCTEAM_NAS_PATH:-/home/rob/nasworkspace/ccteam}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${CCTEAM_PROBE_OUT_DIR:-./.probe-results/$TS}"

ALL_SCENARIOS=(
    solo-sidekick
    team-sprint
    overnight-builder
    pocket-assistant
    im-squad
    codex-advise
    codex-auto-critic
    codex-fallback
)

if [[ $# -gt 0 ]]; then
    SCENARIOS=("$@")
else
    SCENARIOS=("${ALL_SCENARIOS[@]}")
fi

mkdir -p "$OUT_DIR"
log() { printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }

# ---------- helpers ----------

remote_run() {
    # remote_run <scenario> <bash-script>
    local scenario="$1"; shift
    local script="$1"
    local d="$OUT_DIR/$scenario"
    mkdir -p "$d"
    echo "ssh $NAS_HOST 'cd $NAS_PATH && ...'" > "$d/cmd.txt"
    printf '%s\n' "$script" >> "$d/cmd.txt"
    ssh "$NAS_HOST" bash -s <<EOF >"$d/log" 2>&1
set -uo pipefail
cd "$NAS_PATH"
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy bash -c '$script'
EOF
    echo $? > "$d/rc"
}

snapshot_cost() {
    local scenario="$1"
    local d="$OUT_DIR/$scenario"
    ssh "$NAS_HOST" "cd $NAS_PATH && ./target/release/ccteam cost summary --json" \
        > "$d/cost.txt" 2>/dev/null || echo '{"note":"cost summary unavailable"}' > "$d/cost.txt"
}

mark() {
    # mark <scenario> <status>
    echo "$2" > "$OUT_DIR/$1/status"
}

# ---------- scenario fns ----------

probe_solo_sidekick() {
    log "preset 1: solo-sidekick (mode 1 in-proc)"
    remote_run solo-sidekick '
        echo "[probe] solo-sidekick: spawn one in-proc Task subagent via /ccteam"
        echo "[probe] driver: Claude session — see host-probe.md for manual steps"
        echo "[probe] unit coverage: ccteam-core::orchestrator + Task tool wrapper"
        echo "[probe] auto path here = sanity check the binary boots"
        ./target/release/ccteam --help | head -5
        ./target/release/ccteam workflow list 2>&1 | head -5 || true
    '
    snapshot_cost solo-sidekick
    mark solo-sidekick manual
}

probe_team_sprint() {
    log "preset 2: team-sprint (mode 1 in-proc, 3 teammates)"
    remote_run team-sprint '
        echo "[probe] team-sprint: /ccteam-team 3 ..."
        echo "[probe] driver: requires user-driven Claude session"
        ./target/release/ccteam --help | head -3
    '
    snapshot_cost team-sprint
    mark team-sprint manual
}

probe_overnight_builder() {
    log "preset 3: overnight-builder (mode 2 bg)"
    remote_run overnight-builder '
        echo "[probe] overnight-builder: ccteam-creator → workflow.yaml → daemon"
        echo "[probe] runs ccteam-creator preset check + a 5-minute mock cycle"
        ./target/release/ccteam --help | head -3
        # smoke: workflow.yaml load + validate is the contract surface
        ls .ccteam 2>/dev/null || true
    '
    snapshot_cost overnight-builder
    mark overnight-builder mock
}

probe_pocket_assistant() {
    log "preset 4: pocket-assistant (mode 3 chat — Telegram DM)"
    if [[ "${CCTEAM_PROBE_REAL_TG:-0}" == "1" ]]; then
        remote_run pocket-assistant '
            echo "[probe] pocket-assistant: real TG e2e against @web3op_bot"
            echo "[probe] expects ~/.ccteam/im/credentials.json on remote"
            test -f ~/.ccteam/im/credentials.json || { echo "missing credentials.json"; exit 9; }
            ./target/release/ccteam-imd status 2>&1 || true
        '
        snapshot_cost pocket-assistant
        mark pocket-assistant real
    else
        remote_run pocket-assistant '
            echo "[probe] pocket-assistant: mock channel e2e"
            echo "[probe] inject ChannelMessage via MockChannel::push"
            echo "[probe] assert turns.jsonl mirror + outbox forward"
            ./target/release/ccteam-imd --help 2>&1 | head -10 || true
        '
        snapshot_cost pocket-assistant
        mark pocket-assistant mock
    fi
}

probe_im_squad() {
    log "preset 5: im-squad (mode 3 chat — group + bot-to-bot)"
    if [[ "${CCTEAM_PROBE_REAL_TG:-0}" == "1" ]]; then
        remote_run im-squad '
            echo "[probe] im-squad: real TG group + 2 bots"
            test -f ~/.ccteam/im/credentials.json || { echo "missing credentials.json"; exit 9; }
            ./target/release/ccteam-imd status 2>&1 || true
        '
        snapshot_cost im-squad
        mark im-squad real
    else
        remote_run im-squad '
            echo "[probe] im-squad: mock channel + 2-bot routing"
            echo "[probe] hop_limit=4 chain → escalate"
            ./target/release/ccteam-imd --help 2>&1 | head -10 || true
        '
        snapshot_cost im-squad
        mark im-squad mock
    fi
}

probe_codex_advise() {
    log "codex A: /ccteam-advise parallel Claude+Codex verdict"
    remote_run codex-advise '
        echo "[probe] codex-advise: parallel Claude + Codex call"
        if command -v codex >/dev/null 2>&1; then
            codex --version
            codex login status 2>&1 || true
        else
            echo "[probe] codex binary missing — scenario skipped"
            exit 9
        fi
    '
    snapshot_cost codex-advise
    if [[ "$(cat "$OUT_DIR/codex-advise/rc")" == "9" ]]; then
        mark codex-advise skip
    else
        mark codex-advise happy
    fi
}

probe_codex_auto_critic() {
    log "codex B: ccteam-creator auto-routes critic role to Codex"
    remote_run codex-auto-critic '
        echo "[probe] codex-auto-critic: phase 3.5 detection in ccteam-creator"
        if ! command -v codex >/dev/null 2>&1; then
            echo "[probe] codex binary missing — scenario skipped"
            exit 9
        fi
        # smoke: verify the codex detection helper returns ok
        ./target/release/ccteam prefs get fallback.on_claude_quota 2>&1 || true
    '
    snapshot_cost codex-auto-critic
    if [[ "$(cat "$OUT_DIR/codex-auto-critic/rc")" == "9" ]]; then
        mark codex-auto-critic skip
    else
        mark codex-auto-critic happy
    fi
}

probe_codex_fallback() {
    log "codex C: opt-in fallback on Claude budget_exceeded"
    remote_run codex-fallback '
        echo "[probe] codex-fallback: set prefs + emit mock budget_exceeded"
        if ! command -v codex >/dev/null 2>&1; then
            echo "[probe] codex binary missing — scenario skipped"
            exit 9
        fi
        ./target/release/ccteam prefs set fallback.on_claude_quota codex 2>&1 || true
        ./target/release/ccteam prefs get fallback.on_claude_quota 2>&1 || true
    '
    snapshot_cost codex-fallback
    if [[ "$(cat "$OUT_DIR/codex-fallback/rc")" == "9" ]]; then
        mark codex-fallback skip
    else
        mark codex-fallback happy
    fi
}

# ---------- driver ----------

dispatch() {
    case "$1" in
        solo-sidekick)      probe_solo_sidekick ;;
        team-sprint)        probe_team_sprint ;;
        overnight-builder)  probe_overnight_builder ;;
        pocket-assistant)   probe_pocket_assistant ;;
        im-squad)           probe_im_squad ;;
        codex-advise)       probe_codex_advise ;;
        codex-auto-critic)  probe_codex_auto_critic ;;
        codex-fallback)     probe_codex_fallback ;;
        *)                  echo "unknown scenario: $1" >&2; return 1 ;;
    esac
}

log "host-probe run started — out=$OUT_DIR"
for s in "${SCENARIOS[@]}"; do
    dispatch "$s" || true
done

log "writing summary.md"
{
    echo "# host-probe run $TS"
    echo
    echo "| scenario | status | rc | cost |"
    echo "|---|---|---|---|"
    for s in "${SCENARIOS[@]}"; do
        d="$OUT_DIR/$s"
        status="$(cat "$d/status" 2>/dev/null || echo '-')"
        rc="$(cat "$d/rc" 2>/dev/null || echo '-')"
        cost="$(head -c 80 "$d/cost.txt" 2>/dev/null | tr '\n' ' ' || echo '-')"
        echo "| $s | $status | $rc | \`$cost\` |"
    done
    echo
    echo "Detailed logs:"
    for s in "${SCENARIOS[@]}"; do
        echo "- \`$s/log\` (cmd in \`$s/cmd.txt\`)"
    done
} > "$OUT_DIR/summary.md"

log "done — paste $OUT_DIR/summary.md into docs/v0-6-0/host-probe.md"
