#!/usr/bin/env bash
# Demo 3: Overnight Builder — /ccteam-creator + qa-loop, with TG push frame.
# Target ~60s, 90x30.
set -eu
cd "$(dirname "$0")"
source ./_lib.sh
hide_cursor
clear

header "Claude Code  ·  ccteam-demo  ·  mode-2 bg (ccteam-creator)"
log ""
pause 0.4

user_prompt "/ccteam-creator '我今晚睡了,跑 qa-loop:test→fix→re-test,直到通过或撞预算'"
pause 0.6
log ""
log "${C_AGENT}ccteam-creator${C_RESET} ${C_DIMK}─ NL → workflow.yaml…${C_RESET}"
pause 0.7
log "  ${C_OK}✓${C_RESET} drafted ${C_BOLD}.claude/workflows/qa-loop.yaml${C_RESET}"
log "  ${C_DIMK}  agents: tester ─▸ fixer (cap 8 hops) ─▸ tester (re-run)${C_RESET}"
log "  ${C_DIMK}  budget: \$5/24h  ·  vendor: claude  ·  mode: bg${C_RESET}"
pause 0.8
log ""
log "${C_TOOL}● workflow_start${C_RESET} qa-loop ${C_DIMK}→ background daemon${C_RESET}"
pause 0.4
log "  ${C_OK}✓${C_RESET} ccteam-imd PID 8421, watching artifacts ${C_DIMK}(detached, survives shell exit)${C_RESET}"
pause 0.8
log ""
log "${C_DIMK}─ user logs off, hours pass (compressed) ─${C_RESET}"
pause 0.7
log ""
log "${C_DIM}[00:14]${C_RESET} ${C_AGENT}tester${C_RESET}  ▸ 247/253 pass, 6 fail"
pause 0.3
log "${C_DIM}[00:14]${C_RESET} ${C_AGENT}fixer${C_RESET}   ▸ hop 1/8 — patching crates/api/src/route.rs"
pause 0.3
log "${C_DIM}[00:17]${C_RESET} ${C_AGENT}tester${C_RESET}  ▸ 251/253 pass, 2 fail"
pause 0.3
log "${C_DIM}[00:18]${C_RESET} ${C_AGENT}fixer${C_RESET}   ▸ hop 2/8 — patching crates/api/tests/it.rs"
pause 0.3
log "${C_DIM}[00:21]${C_RESET} ${C_AGENT}tester${C_RESET}  ▸ 253/253 ${C_OK}✓${C_RESET}"
pause 0.4
log "${C_DIM}[00:21]${C_RESET} ${C_AGENT}ccteam${C_RESET}  ▸ qa-loop ${C_OK}DONE${C_RESET}  cost \$0.74/\$5  hops 2/8"
pause 0.8
log ""
header " Telegram  ·  @ccteam_bot  ·  DM "
log "${C_DIMK}┌──────────────────────────────────────────────────────────────┐${C_RESET}"
log "${C_DIMK}│${C_RESET}  ${C_BOT}@ccteam_bot${C_RESET} ${C_DIM}03:21${C_RESET}                                          ${C_DIMK}│${C_RESET}"
log "${C_DIMK}│${C_RESET}  ${C_OK}✅${C_RESET} qa-loop done. 253/253 green.                          ${C_DIMK}│${C_RESET}"
log "${C_DIMK}│${C_RESET}     hops 2/8 · cost \$0.74 · branch  ${C_BOLD}qa/auto-fix-night${C_RESET}     ${C_DIMK}│${C_RESET}"
log "${C_DIMK}│${C_RESET}     [Reply ${C_BOLD}merge${C_RESET}] or [${C_BOLD}show diff${C_RESET}] to continue.            ${C_DIMK}│${C_RESET}"
log "${C_DIMK}└──────────────────────────────────────────────────────────────┘${C_RESET}"
pause 2.0
log ""
log "${C_DIMK}─ end of demo (mode 2 ran overnight, IM ping on done) ─${C_RESET}"
pause 1.2
