#!/usr/bin/env bash
#
# scripts/host-probe/run-probes.sh
#
# V0.6.0 Wave 4 host-probe execution step + V0.6.1 F119/F120
# enhancements. Runs the 5 preset E2E scenarios + the 3 Codex scenarios
# against an already-deployed remote (see `deploy-to-nas.sh`) or, with
# `CCTEAM_PROBE_LOCAL=1`, against the local checkout for dry-run.
#
# V0.6.1 F119 — pocket-assistant / im-squad now actively manage the
# ccteam-imd daemon lifecycle: spawn → health-wait → exercise → stop +
# capture daemon stderr on crash. `CCTEAM_PROBE_SKIP_DAEMON_START=1`
# leaves daemon management to the caller.
#
# V0.6.1 F120 — overnight-builder now scaffolds a fake artifact-driven
# workflow under `/tmp/host-probe-overnight/`, plants a stub claude
# binary (no real LLM cost), starts `ccteam start` in the background,
# trips the trigger, and asserts `agent_spawn` + `agent_done` events
# in `progress.jsonl`. Previously it was `ccteam --help` smoke only.
#
# Output layout (per scenario):
#   $OUT_DIR/<scenario>/cmd.txt            — actual command(s) executed
#   $OUT_DIR/<scenario>/log                — combined stdout/stderr tail
#   $OUT_DIR/<scenario>/cost.txt           — `ccteam cost summary` snapshot
#   $OUT_DIR/<scenario>/daemon-stderr.log  — F119 daemon stderr (when spawned)
#   $OUT_DIR/<scenario>/status             — "happy" | "mock" | "skip" | "fail"
#   $OUT_DIR/summary.md                    — table the team-lead pastes into
#                                            docs/v0-6-X/host-probe.md
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
#   CCTEAM_NAS_HOST              — ssh target (default: nas-box005)
#   CCTEAM_NAS_PATH              — remote checkout (default:
#                                  /home/rob/nasworkspace/ccteam)
#   CCTEAM_PROBE_OUT_DIR         — local output dir (default:
#                                  ./.probe-results/<UTC-timestamp>)
#   CCTEAM_PROBE_REAL_TG         — "1" enables real Telegram probes for
#                                  pocket-assistant / im-squad; default "0"
#                                  runs the mock-channel path.
#   CCTEAM_PROBE_LOCAL           — "1" runs scenarios locally (no SSH).
#                                  Used by `cargo` callers to dry-run the
#                                  script during PR review.
#   CCTEAM_PROBE_SKIP_DAEMON_START — "1" leaves ccteam-imd daemon lifecycle
#                                    to the caller (F119); the script
#                                    assumes a daemon is already running.
#
# Each scenario function MUST be safe to skip independently. A
# scenario records `status=skip` with a reason when its prerequisites
# (tooling, auth, allowed_chat_ids) are missing; the wave gate accepts
# mock/skip per the wave handoff message.
#
# Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>

set -euo pipefail

NAS_HOST="${CCTEAM_NAS_HOST:-nas-box005}"
NAS_PATH="${CCTEAM_NAS_PATH:-/home/rob/nasworkspace/ccteam}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${CCTEAM_PROBE_OUT_DIR:-./.probe-results/$TS}"
LOCAL_MODE="${CCTEAM_PROBE_LOCAL:-0}"
SKIP_DAEMON_START="${CCTEAM_PROBE_SKIP_DAEMON_START:-0}"

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

# Run a bash script in the right execution context (remote SSH or
# local shell). Both paths strip HTTP_PROXY (V0.6 lesson: the 64
# ccteam-web tests false-positive 502 against proxied connections).
remote_run() {
    # remote_run <scenario> <bash-script>
    local scenario="$1"; shift
    local script="$1"
    local d="$OUT_DIR/$scenario"
    mkdir -p "$d"
    if [[ "$LOCAL_MODE" == "1" ]]; then
        echo "local: bash -s (LOCAL_MODE=1)" > "$d/cmd.txt"
        printf '%s\n' "$script" >> "$d/cmd.txt"
        # NAS_PATH is interpreted as "the checkout root" — locally that
        # is the repo root (two levels up from this script).
        local local_root
        local_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
        (
            cd "$local_root"
            env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
                bash -c "$script"
        ) >"$d/log" 2>&1
        echo $? > "$d/rc"
    else
        echo "ssh $NAS_HOST 'cd $NAS_PATH && ...'" > "$d/cmd.txt"
        printf '%s\n' "$script" >> "$d/cmd.txt"
        ssh "$NAS_HOST" bash -s <<EOF >"$d/log" 2>&1
set -uo pipefail
cd "$NAS_PATH"
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy bash -c '$script'
EOF
        echo $? > "$d/rc"
    fi
}

snapshot_cost() {
    local scenario="$1"
    local d="$OUT_DIR/$scenario"
    if [[ "$LOCAL_MODE" == "1" ]]; then
        local local_root
        local_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
        if [[ -x "$local_root/target/release/ccteam" ]]; then
            "$local_root/target/release/ccteam" cost summary --json \
                > "$d/cost.txt" 2>/dev/null \
                || echo '{"note":"cost summary unavailable (local)"}' > "$d/cost.txt"
        elif [[ -x "$local_root/target/debug/ccteam" ]]; then
            "$local_root/target/debug/ccteam" cost summary --json \
                > "$d/cost.txt" 2>/dev/null \
                || echo '{"note":"cost summary unavailable (local debug)"}' > "$d/cost.txt"
        else
            echo '{"note":"ccteam binary not built locally"}' > "$d/cost.txt"
        fi
    else
        ssh "$NAS_HOST" "cd $NAS_PATH && ./target/release/ccteam cost summary --json" \
            > "$d/cost.txt" 2>/dev/null \
            || echo '{"note":"cost summary unavailable"}' > "$d/cost.txt"
    fi
}

# V0.6.1 F119 — pull daemon-stderr back from the run (remote or local)
# into the per-scenario output dir. The daemon-lifecycle inline block
# writes stderr to a known path; this fn copies it.
fetch_daemon_stderr() {
    local scenario="$1"
    local d="$OUT_DIR/$scenario"
    local remote_path="/tmp/ccteam-imd-probe-$scenario.stderr"
    if [[ "$LOCAL_MODE" == "1" ]]; then
        if [[ -f "$remote_path" ]]; then
            cp "$remote_path" "$d/daemon-stderr.log" 2>/dev/null || true
        fi
    else
        ssh "$NAS_HOST" "cat $remote_path 2>/dev/null" > "$d/daemon-stderr.log" 2>/dev/null || true
        if [[ ! -s "$d/daemon-stderr.log" ]]; then
            rm -f "$d/daemon-stderr.log"
        fi
    fi
}

mark() {
    # mark <scenario> <status>
    echo "$2" > "$OUT_DIR/$1/status"
}

# ---------- F119 daemon-lifecycle snippets ----------
#
# `daemon_start_snippet <scenario>` returns a bash snippet that:
#   - spawns `ccteam-imd run` in background (writes pid + stderr to /tmp)
#   - polls `ccteam-imd health --timeout-seconds 30` for readiness
#   - on health failure: dumps daemon stderr, kills the pid, exits 11
#
# When CCTEAM_PROBE_SKIP_DAEMON_START=1, the snippet becomes a no-op
# stub so callers managing their own daemons aren't disturbed.
daemon_start_snippet() {
    local scenario="$1"
    local pidfile="/tmp/ccteam-imd-probe-$scenario.pid"
    local stderrfile="/tmp/ccteam-imd-probe-$scenario.stderr"
    if [[ "$SKIP_DAEMON_START" == "1" ]]; then
        cat <<EOF
echo "[probe/$scenario] CCTEAM_PROBE_SKIP_DAEMON_START=1 — assuming caller-managed daemon"
EOF
        return
    fi
    cat <<EOF
echo "[probe/$scenario] F119: starting ccteam-imd daemon"
nohup ./target/release/ccteam-imd run --tick-seconds 2 \\
    >/tmp/ccteam-imd-probe-$scenario.stdout 2>$stderrfile &
echo \$! > $pidfile
echo "[probe/$scenario] daemon pid=\$(cat $pidfile)"
if ! ./target/release/ccteam-imd health --timeout-seconds 30 --poll-ms 200; then
    echo "[probe/$scenario] F119: health-wait FAILED — dumping last 50 lines of daemon stderr:"
    echo "=== DAEMON STDERR (tail 50) ==="
    tail -50 $stderrfile 2>/dev/null || true
    echo "=== END DAEMON STDERR ==="
    kill -TERM \$(cat $pidfile) 2>/dev/null || true
    exit 11
fi
echo "[probe/$scenario] F119: daemon ready"
EOF
}

daemon_stop_snippet() {
    local scenario="$1"
    local pidfile="/tmp/ccteam-imd-probe-$scenario.pid"
    local stderrfile="/tmp/ccteam-imd-probe-$scenario.stderr"
    if [[ "$SKIP_DAEMON_START" == "1" ]]; then
        cat <<EOF
echo "[probe/$scenario] CCTEAM_PROBE_SKIP_DAEMON_START=1 — not stopping caller-managed daemon"
EOF
        return
    fi
    cat <<EOF
echo "[probe/$scenario] F119: stopping ccteam-imd daemon"
if [[ -f $pidfile ]]; then
    PID=\$(cat $pidfile)
    kill -TERM \$PID 2>/dev/null || true
    for _i in 1 2 3 4 5; do
        if ! kill -0 \$PID 2>/dev/null; then
            break
        fi
        sleep 1
    done
    if kill -0 \$PID 2>/dev/null; then
        echo "[probe/$scenario] F119: daemon did not exit gracefully; SIGKILL"
        kill -KILL \$PID 2>/dev/null || true
    fi
    rm -f $pidfile
fi
echo "=== DAEMON STDERR (tail 30) ==="
tail -30 $stderrfile 2>/dev/null || true
echo "=== END DAEMON STDERR ==="
EOF
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

# V0.6.1 F120 — real artifact-driven workflow, not a `--help` smoke test.
#
# Steps (inside the remote bash block):
#   1. Wipe + scaffold /tmp/host-probe-overnight/ with a 1-agent
#      artifact-driven workflow.yaml.
#   2. Plant a stub claude binary that prints the `backgrounded · <id>`
#      marker and writes a synthetic state.json so the orchestrator's
#      `poll_completions` will emit `agent_done` without burning real
#      LLM cost.
#   3. Start `ccteam start` in the background pointed at the fake
#      project; tail progress.jsonl until both `agent_spawn` and
#      `agent_done` land, or a 60s deadline.
#   4. Clean up the daemon + tempdir; exit nonzero iff either event
#      is missing.
probe_overnight_builder() {
    log "preset 3: overnight-builder (mode 2 bg) — F120 real workflow"
    remote_run overnight-builder '
        set +e
        # F120 root: holds an isolated CCTEAM_HOME + projects_root so
        # the probe does not touch the user latency real ~/.ccteam state.
        ROOT=/tmp/host-probe-overnight
        rm -rf $ROOT
        mkdir -p $ROOT
        export CCTEAM_HOME=$ROOT/ccteam-home
        export CCTEAM_PROJECTS_ROOT=$ROOT/projects
        mkdir -p $CCTEAM_HOME $CCTEAM_PROJECTS_ROOT
        PROJ=$CCTEAM_PROJECTS_ROOT/overnight-probe
        mkdir -p $PROJ/.ccteam/triggers/worker
        mkdir -p $PROJ/.claude/agents

        # Stash the abs path to the ccteam + ccteam-imd binaries before
        # any cd, so background invocations are unambiguous.
        CCTEAM_BIN="$(pwd)/target/release/ccteam"

        # F120 step 1 — minimal artifact-driven workflow.yaml
        cat > $PROJ/workflow.yaml <<YAML
name: overnight-probe
description: |
  F120 host-probe minimal artifact-driven workflow. One worker agent
  fires on a trigger marker; the stub claude binary terminates
  instantly so agent_done lands within the probe deadline.
agents:
  worker:
    executor: claude
    trigger: watch:.ccteam/triggers/worker/
    parallelism: 1
YAML

        cat > $PROJ/.claude/agents/worker.md <<MD
# worker

Stub agent for F120 host-probe.
MD

        # F120 step 2 — stub claude binary. Prints `backgrounded · <id>`
        # (parsed by ClaudeBgAdapter.start_thread) + writes synthetic
        # state.json so poll_completions emits agent_done.
        cat > $ROOT/fake-claude <<STUB
#!/bin/sh
echo "backgrounded · stub\$\$"
JOBS_DIR=\${CCTEAM_CLAUDE_JOBS_DIR:-\$HOME/.claude/jobs}
mkdir -p \$JOBS_DIR/stub\$\$
cat > \$JOBS_DIR/stub\$\$/state.json <<JSON
{"status":"completed","cost_usd":0.0,"started_at":"2026-01-01T00:00:00Z","ended_at":"2026-01-01T00:00:01Z"}
JSON
exit 0
STUB
        chmod +x $ROOT/fake-claude
        export CCTEAM_CLAUDE_BIN=$ROOT/fake-claude
        export CCTEAM_CLAUDE_JOBS_DIR=$ROOT/claude-jobs
        mkdir -p $CCTEAM_CLAUDE_JOBS_DIR

        echo "[probe/overnight] scaffold ready at $PROJ (root=$ROOT)"

        # F120 step 3 — start orchestrator (no-web). It scans
        # CCTEAM_PROJECTS_ROOT for projects.
        nohup "$CCTEAM_BIN" start --no-web --tick-seconds 1 \
            >$ROOT/start.stdout 2>$ROOT/start.stderr &
        ORCH_PID=$!
        echo $ORCH_PID > $ROOT/orch.pid
        echo "[probe/overnight] orchestrator pid=$ORCH_PID"

        sleep 3
        touch $PROJ/.ccteam/triggers/worker/wake.md
        echo "[probe/overnight] trigger placed; polling progress.jsonl"

        SPAWN_SEEN=0
        DONE_SEEN=0
        for i in $(seq 1 60); do
            if [[ -f $PROJ/.ccteam/progress.jsonl ]]; then
                if grep -q "\"event\":\"agent_spawn\"" $PROJ/.ccteam/progress.jsonl 2>/dev/null; then
                    SPAWN_SEEN=1
                fi
                if grep -q "\"event\":\"agent_done\"" $PROJ/.ccteam/progress.jsonl 2>/dev/null; then
                    DONE_SEEN=1
                    break
                fi
            fi
            sleep 1
        done

        echo "[probe/overnight] progress.jsonl dump:"
        if [[ -f $PROJ/.ccteam/progress.jsonl ]]; then
            cat $PROJ/.ccteam/progress.jsonl
            echo "[probe/overnight] event types observed:"
            (command -v jq >/dev/null && jq -r .event $PROJ/.ccteam/progress.jsonl | sort -u) \
                || grep -oE "\"event\":\"[^\"]*\"" $PROJ/.ccteam/progress.jsonl | sort -u
        else
            echo "[probe/overnight] progress.jsonl was never created"
        fi

        kill -TERM $ORCH_PID 2>/dev/null || true
        for _i in 1 2 3 4 5; do
            kill -0 $ORCH_PID 2>/dev/null || break
            sleep 1
        done
        kill -KILL $ORCH_PID 2>/dev/null || true

        echo "[probe/overnight] orchestrator stderr (tail 30):"
        tail -30 $ROOT/start.stderr 2>/dev/null || true

        if [[ "$SPAWN_SEEN" == "1" && "$DONE_SEEN" == "1" ]]; then
            echo "[probe/overnight] PASS: agent_spawn + agent_done observed"
            rm -rf $ROOT
            exit 0
        elif [[ "$SPAWN_SEEN" == "1" ]]; then
            echo "[probe/overnight] PARTIAL: agent_spawn seen but no agent_done within 60s"
            rm -rf $ROOT
            exit 2
        else
            echo "[probe/overnight] FAIL: no agent_spawn within 60s"
            rm -rf $ROOT
            exit 1
        fi
    '
    snapshot_cost overnight-builder
    local rc
    rc="$(cat "$OUT_DIR/overnight-builder/rc" 2>/dev/null || echo '-')"
    case "$rc" in
        0) mark overnight-builder happy ;;
        2) mark overnight-builder partial ;;
        *) mark overnight-builder fail ;;
    esac
}

probe_pocket_assistant() {
    log "preset 4: pocket-assistant (mode 3 chat — Telegram DM)"
    if [[ "${CCTEAM_PROBE_REAL_TG:-0}" == "1" ]]; then
        remote_run pocket-assistant "
            $(daemon_start_snippet pocket-assistant)
            echo '[probe] pocket-assistant: real TG e2e against @web3op_bot'
            echo '[probe] expects ~/.ccteam/im/credentials.json on remote'
            test -f \$HOME/.ccteam/im/credentials.json || { echo 'missing credentials.json'; exit 9; }
            ./target/release/ccteam-imd status 2>&1 || true
            $(daemon_stop_snippet pocket-assistant)
        "
        fetch_daemon_stderr pocket-assistant
        snapshot_cost pocket-assistant
        local rc
        rc="$(cat "$OUT_DIR/pocket-assistant/rc" 2>/dev/null || echo '-')"
        case "$rc" in
            0) mark pocket-assistant real ;;
            9) mark pocket-assistant skip ;;
            *) mark pocket-assistant fail ;;
        esac
    else
        remote_run pocket-assistant "
            $(daemon_start_snippet pocket-assistant)
            echo '[probe] pocket-assistant: mock channel e2e'
            echo '[probe] F119 daemon-up smoke: status output + heartbeat fresh'
            ./target/release/ccteam-imd status 2>&1 | head -10 || true
            $(daemon_stop_snippet pocket-assistant)
        "
        fetch_daemon_stderr pocket-assistant
        snapshot_cost pocket-assistant
        mark pocket-assistant mock
    fi
}

probe_im_squad() {
    log "preset 5: im-squad (mode 3 chat — group + bot-to-bot)"
    if [[ "${CCTEAM_PROBE_REAL_TG:-0}" == "1" ]]; then
        remote_run im-squad "
            $(daemon_start_snippet im-squad)
            echo '[probe] im-squad: real TG group + 2 bots'
            test -f \$HOME/.ccteam/im/credentials.json || { echo 'missing credentials.json'; exit 9; }
            ./target/release/ccteam-imd status 2>&1 || true
            $(daemon_stop_snippet im-squad)
        "
        fetch_daemon_stderr im-squad
        snapshot_cost im-squad
        local rc
        rc="$(cat "$OUT_DIR/im-squad/rc" 2>/dev/null || echo '-')"
        case "$rc" in
            0) mark im-squad real ;;
            9) mark im-squad skip ;;
            *) mark im-squad fail ;;
        esac
    else
        remote_run im-squad "
            $(daemon_start_snippet im-squad)
            echo '[probe] im-squad: mock channel + 2-bot routing'
            echo '[probe] F119 daemon-up smoke: status output + heartbeat fresh'
            ./target/release/ccteam-imd status 2>&1 | head -10 || true
            $(daemon_stop_snippet im-squad)
        "
        fetch_daemon_stderr im-squad
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

log "host-probe run started — out=$OUT_DIR (LOCAL_MODE=$LOCAL_MODE, SKIP_DAEMON_START=$SKIP_DAEMON_START)"
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
        d="$OUT_DIR/$s"
        echo "- \`$s/log\` (cmd in \`$s/cmd.txt\`)"
        if [[ -f "$d/daemon-stderr.log" ]]; then
            echo "  - daemon stderr: \`$s/daemon-stderr.log\`"
        fi
    done
} > "$OUT_DIR/summary.md"

log "done — paste $OUT_DIR/summary.md into docs/v0-6-X/host-probe.md"
