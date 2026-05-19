#!/usr/bin/env bash
# Demo 5: IM Squad — multi-bot TG group, @-chain escalation.
# Target ~60s, 90x30.
set -eu
cd "$(dirname "$0")"
source ./_lib.sh
hide_cursor
clear

header " Telegram  ·  ccteam-squad (group)  ·  3 members + 2 bots "
log ""
pause 0.4
log "${C_DIMK}─────────────────────────────  Wed, May 19  ─────────────────────────────${C_RESET}"
log ""
pause 0.4

log "                                                            ${C_USER}you ${C_DIM}10:02${C_RESET}"
log "                              ${C_USER}@critic_bot prod 报 latency p99 4.2s,看下${C_RESET}"
pause 1.2
log ""
log "${C_BOT}@critic_bot${C_RESET} ${C_DIM}10:02${C_RESET}"
log "  拉了最近 6h trace,瓶颈在 ${C_BOLD}cache miss → db round-trip${C_RESET}"
pause 0.4
log "   · /api/feed     hit 32%   ${C_WARN}should be >85%${C_RESET}"
pause 0.3
log "   · /api/profile  hit 91%   ${C_OK}ok${C_RESET}"
pause 0.5
log "  根因像 feed 的 cache key 没带 version,昨晚 schema 变了 → 全 miss。"
pause 0.4
log "  @${C_AGENT}fixer_bot${C_RESET} 接一下,改 key + bump 版本。"
pause 1.2
log ""
log "${C_BOT}@fixer_bot${C_RESET} ${C_DIM}10:03${C_RESET}  ${C_DIMK}claimed by critic_bot · hop 1/3${C_RESET}"
log "  ${C_TOOL}● Read${C_RESET} src/cache/feed.ts"
pause 0.3
log "  ${C_TOOL}● Edit${C_RESET} src/cache/feed.ts ${C_DIMK}(+\`v\${SCHEMA_VERSION}\` to key)${C_RESET}"
pause 0.4
log "  ${C_TOOL}● Bash${C_RESET} pnpm test cache.feed.spec"
pause 0.5
log "   ${C_OK}✓${C_RESET} 14/14 pass"
pause 0.4
log "  ${C_TOOL}● Bash${C_RESET} gh pr create"
pause 0.5
log "   ${C_OK}✓${C_RESET} PR #224 opened · ${C_DIMK}fix(cache): version feed key${C_RESET}"
pause 0.6
log "  @you ready to review.  @${C_AGENT}critic_bot${C_RESET} cross-check?"
pause 1.4
log ""
log "${C_BOT}@critic_bot${C_RESET} ${C_DIM}10:04${C_RESET}"
log "  ${C_TOOL}● Read${C_RESET} PR #224 diff"
pause 0.4
log "  LGTM — also adds metric ${C_BOLD}cache.feed.hit_rate${C_RESET} to dashboard. ${C_OK}✓${C_RESET}"
pause 1.0
log ""
log "                                                            ${C_USER}you ${C_DIM}10:04${C_RESET}"
log "                                              ${C_USER}merge 吧,谢二位${C_RESET}"
pause 0.8
log ""
log "${C_BOT}@fixer_bot${C_RESET} ${C_DIM}10:04${C_RESET}  ${C_TOOL}● gh pr merge --squash --auto${C_RESET}  ${C_OK}✓${C_RESET}  ${C_DIMK}#224 will merge after CI${C_RESET}"
pause 1.4
log ""
log "${C_DIMK}─ end of demo (mode 3: 2 bots, @-chain, hop_limit=3, cost-capped) ─${C_RESET}"
pause 1.2
