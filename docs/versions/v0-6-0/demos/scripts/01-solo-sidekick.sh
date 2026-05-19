#!/usr/bin/env bash
# Demo 1: Solo Sidekick — /ccteam "扫 TODO" (in-proc Task subagent).
# Target ~30s, 90x30.
set -eu
cd "$(dirname "$0")"
source ./_lib.sh
hide_cursor
clear

header "Claude Code  ·  ccteam-demo  ·  mode-1 in-proc"
log ""
log "${C_DIMK}/help for help, /status for your current setup${C_RESET}"
log ""
pause 0.6

user_prompt "/ccteam 扫一遍 src/ 的 TODO,挑可一行修的"
pause 0.5
log ""
log "${C_TOOL}● Task${C_RESET}(${C_AGENT}todo-scanner${C_RESET}) ${C_DIMK}via mcp__ccteam__workflow_invoke${C_RESET}"
log "  ${C_DIMK}└ in-proc subagent, fresh ctx, shares session${C_RESET}"
pause 0.7
log ""
log "${C_AGENT}todo-scanner${C_RESET} ${C_DIMK}─ Read src/ recursively…${C_RESET}"
pause 0.5
log "  ${C_DIMK}scanned 47 files${C_RESET}"
pause 0.4
log ""
log "${C_AGENT}todo-scanner${C_RESET} ${C_DIMK}─ found 6 TODO, 3 are one-liners:${C_RESET}"
pause 0.4
log "  ${C_BOLD}1.${C_RESET} src/api/client.ts:88   ${C_DIMK}// TODO: timeout 5s${C_RESET}"
pause 0.25
log "  ${C_BOLD}2.${C_RESET} src/ui/Modal.tsx:42    ${C_DIMK}// TODO: aria-label${C_RESET}"
pause 0.25
log "  ${C_BOLD}3.${C_RESET} src/lib/cache.ts:15    ${C_DIMK}// TODO: LRU max=500${C_RESET}"
pause 0.6
log ""
log "${C_OK}✓ Task done${C_RESET} ${C_DIMK}(3.2s, 4.1k tokens, \$0.012 mocked)${C_RESET}"
log ""
pause 0.8

user_prompt "修第 1 个"
pause 0.5
log ""
log "${C_TOOL}● Edit${C_RESET} src/api/client.ts"
pause 0.3
log "  ${C_DIMK}-  const r = await fetch(url);${C_RESET}"
log "  ${C_OK}+  const r = await fetch(url, { signal: AbortSignal.timeout(5000) });${C_RESET}"
pause 0.6
log ""
log "${C_OK}✓${C_RESET} 1 file changed. ${C_DIMK}Run tests? (y/N)${C_RESET}"
pause 1.6
log ""
log "${C_DIMK}─ end of demo (mode 1 stays inside one Claude session) ─${C_RESET}"
pause 1.0
