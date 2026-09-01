#!/bin/sh
# pre-agent policy: leads plan, workers execute — below the top level this
# project only hires codex or kimi (keep the expensive harness for planning).
#
# Install as <project>/.ccteam/hooks/pre-agent (see quota-route.sh header).

payload=$(cat)
command -v jq >/dev/null 2>&1 || exit 0

depth=$(printf '%s' "$payload" | jq -r '.caller.depth // 0')
vendor=$(printf '%s' "$payload" | jq -r '.request.vendor // "claude"')

case "$vendor" in codex|kimi) exit 0 ;; esac
if [ "$depth" -ge 1 ]; then
  echo "below the lead, this project hires codex or kimi only (asked: $vendor)" >&2
  exit 2
fi
exit 0
