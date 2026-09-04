#!/bin/sh
# pre-agent policy: steer hires off claude while its 5h window runs hot.
#
# Install:  cp examples/hooks/quota-route.sh <project>/.ccteam/hooks/pre-agent
#           chmod +x <project>/.ccteam/hooks/pre-agent
# Contract: stdin = one JSON line of facts (caller, request, usage, counts);
#           exit 0 = allow, exit 2 = deny with stderr as the reason.
#           Edit the threshold below — it takes effect on the next call.

payload=$(cat)
command -v jq >/dev/null 2>&1 || exit 0   # no jq on this box: stand aside

vendor=$(printf '%s' "$payload" | jq -r '.request.vendor // "claude"')
pct=$(printf '%s' "$payload" | jq -r \
  '[.usage.claude.windows[]? | select(.w=="5h") | .pct] | max // 0')

if [ "$vendor" = "claude" ] && [ "${pct%.*}" -ge 80 ] 2>/dev/null; then
  echo "claude 5h window at ${pct}% — hire codex or kimi for this task instead" >&2
  exit 2
fi
exit 0
