#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

colon=':'
forbidden="persona${colon}|SKILL[.]md|agents/[*][.]md"

if grep -RIn --exclude-dir=node_modules --exclude-dir=dist --exclude-dir=.git -E "$forbidden" "$root"; then
  echo "forbidden content pattern found" >&2
  exit 1
fi
