#!/usr/bin/env bash
# Demo 4: Pocket Assistant — TG DM round-trip (the GIF root README links).
# Target ~30s, 90x30.
set -eu
cd "$(dirname "$0")"
source ./_lib.sh
hide_cursor
clear

header " Telegram  ·  @assistant_demo_bot  ·  DM with you "
log ""
pause 0.4

# Render a fake DM bubble exchange.
log "${C_DIMK}─────────────────────────────  Wed, May 19  ─────────────────────────────${C_RESET}"
log ""
pause 0.5
log "                                                            ${C_USER}you ${C_DIM}09:14${C_RESET}"
log "                                       ${C_USER}今天有几个 PR 待我 review?${C_RESET}"
pause 1.0
log ""
log "${C_BOT}@assistant_demo_bot${C_RESET} ${C_DIM}09:14${C_RESET} ${C_DIMK}is typing…${C_RESET}"
pause 1.2
log ""
log "${C_BOT}@assistant_demo_bot${C_RESET} ${C_DIM}09:14${C_RESET}"
log "  3 PR 等你:"
pause 0.3
log "   ${C_BOLD}1.${C_RESET} #214 ${C_DIMK}feat: cost-cap ui${C_RESET}        ${C_OK}CI green${C_RESET}  ${C_DIMK}2 comments${C_RESET}"
pause 0.25
log "   ${C_BOLD}2.${C_RESET} #218 ${C_DIMK}fix: tg webhook retry${C_RESET}     ${C_WARN}CI yellow${C_RESET} ${C_DIMK}flake re-run?${C_RESET}"
pause 0.25
log "   ${C_BOLD}3.${C_RESET} #221 ${C_DIMK}docs: v0.6.1 wave-3${C_RESET}      ${C_OK}CI green${C_RESET}  ${C_DIMK}你的 PR${C_RESET}"
pause 1.2
log ""
log "                                                            ${C_USER}you ${C_DIM}09:15${C_RESET}"
log "                                            ${C_USER}#218 帮我 re-run 一下${C_RESET}"
pause 1.0
log ""
log "${C_BOT}@assistant_demo_bot${C_RESET} ${C_DIM}09:15${C_RESET}"
log "  ${C_TOOL}● gh run rerun --failed${C_RESET}  ${C_DIMK}(PR #218)${C_RESET}"
pause 0.5
log "  ${C_OK}✓${C_RESET} re-run queued · CI URL ↓"
log "    ${C_DIMK}github.com/you/repo/actions/runs/9182…${C_RESET}"
pause 1.2
log ""
log "                                                            ${C_USER}you ${C_DIM}09:15${C_RESET}"
log "                                                  ${C_USER}thx, fly safe${C_RESET}"
pause 1.0
log ""
log "${C_DIMK}─ end of demo (mode 3: tmux-resident claude TUI behind the bot) ─${C_RESET}"
pause 1.2
