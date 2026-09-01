#!/bin/sh
# write -> run -> evaluate -> improve, closed except the last hop.
#
# The loop cannot edit the flow script itself: script space has no filesystem
# and no process access, by design (docs/hook-dynamic-workflows.md, §Determinism
# — that discipline is what makes --resume exact). So "improve" hands off, and
# the hand-off is explicit:
#
#   default            stop with the edits on stderr, exit 3; you apply them and
#                      rerun with RESUME=<run-dir> to keep the journal
#   IMPROVE_CMD=<cmd>  run <cmd> with the edits as JSON on stdin and let the
#                      loop continue — that command is your delegated agent
#                      (e.g. a `ccteam flow run`-driven patch flow, or any
#                      editor script). Nothing here writes to the script.
#
# Usage:  examples/flows/self-review-loop.sh <flow.js> [extra `flow run` args...]
#   ROUNDS=3      how many run->evaluate passes at most (only reachable with
#                 IMPROVE_CMD; without it the first pass always hands off)
#   MIN_SCORE=7   stop once the LOWEST of the grader's 1-10 scores reaches this
#                 (higher is better on every dimension, `waste` included)
#   RESUME=<dir>  continue an earlier run's journal instead of starting fresh
#
# Requires: the evaluator installed where `ccteam flow eval` looks —
#   cp examples/flows/flow-review.flow.js .agents/flows/_eval.flow.js
# Exit: 0 good enough · 2 setup/plumbing problem · 3 edits proposed, your turn.
set -eu

script=${1:?usage: self-review-loop.sh <flow.js> [flow run args...]}
shift
rounds=${ROUNDS:-3}
min_score=${MIN_SCORE:-7}
run_dir=${RESUME:-}

command -v ccteam >/dev/null 2>&1 || { echo "self-review-loop: no ccteam on PATH" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "self-review-loop: needs jq" >&2; exit 2; }
[ -f "$script" ] || { echo "self-review-loop: no flow script at $script" >&2; exit 2; }

round=1
while [ "$round" -le "$rounds" ]; do
  echo "=== round $round: run ===" >&2
  # A run that ends not-ok still prints its report and is still worth grading —
  # a failed run is exactly when the evaluation earns its keep.
  if [ -z "$run_dir" ]; then
    report=$(ccteam flow run "$script" "$@") || true
  else
    # Same journal, edited script: --resume re-pays only from the first
    # changed call onward.
    report=$(ccteam flow run "$script" --resume "$run_dir") || true
  fi
  run_dir=$(printf '%s' "$report" | jq -r '.run_dir // empty')
  [ -n "$run_dir" ] || { echo "self-review-loop: no run_dir in the report" >&2; exit 2; }

  echo "=== round $round: evaluate $run_dir ===" >&2
  # `flow eval` IS a flow run, so its stdout is an ordinary RunReport whose
  # .returned is flow-review's {grade, patch}. Its own .run_dir is the
  # evaluation's directory, NOT the run under review — don't reassign from it.
  review=$(ccteam flow eval "$run_dir") || true
  scores=$(printf '%s' "$review" | jq -c '.returned.grade.scores // {}' 2>/dev/null || echo '{}')
  # An ABSENT grade must not collapse to 0 — "no verdict" and "graded 0" are
  # different answers and only one of them means the loop is working.
  score=$(printf '%s' "$review" | jq -r \
    '(.returned.grade.scores // {}) as $s
     | if ($s | length) > 0 then ([$s[]] | min | floor) else "" end' 2>/dev/null || echo '')
  # A grader that failed its schema yields null, and `flow eval` can fail
  # outright. Either way there is no verdict — say so, rather than falling
  # through to "here are your edits" with nothing behind it. (`set -e` does
  # not fire inside an `if`, so an empty $score would otherwise sail past the
  # comparison below as a non-fatal `[: Illegal number`.)
  case "$score" in
    '' | *[!0-9]*)
      echo "self-review-loop: no usable grade — inspect $run_dir yourself" >&2
      exit 2
      ;;
  esac
  echo "round $round: scores=$scores worst=$score run_dir=$run_dir" >&2

  if [ "$score" -ge "$min_score" ]; then
    echo "good enough (worst dimension $score >= $min_score), stopping" >&2
    exit 0
  fi

  edits=$(printf '%s' "$review" | jq -c '.returned.patch.edits // []')
  printf '%s' "$review" | jq -r '.returned.patch.edits[]? | "EDIT: \(.what) — \(.why)"' >&2
  if [ "$edits" = "[]" ]; then
    echo "no edits proposed and score $score < $min_score — read $run_dir yourself" >&2
    exit 3
  fi

  if [ -z "${IMPROVE_CMD:-}" ]; then
    echo "apply the edits above to $script, then:" >&2
    echo "  RESUME=$run_dir $0 $script" >&2
    exit 3
  fi

  echo "=== round $round: improve via \$IMPROVE_CMD ===" >&2
  printf '%s' "$edits" | sh -c "$IMPROVE_CMD" >&2
  round=$((round + 1))
done

echo "out of rounds after $rounds pass(es); last run_dir=$run_dir" >&2
exit 3
