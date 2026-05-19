# Mock-terminal helpers for demo recording (sourced).
# Goal: produce a believable Claude-Code-style transcript inside a fixed
# 90x30 terminal, with stable pacing so asciinema/agg output is small.

# ANSI helpers
C_RESET="\033[0m"
C_BOLD="\033[1m"
C_DIM="\033[2m"
C_USER="\033[38;5;39m"   # blue   — user prompt
C_AGENT="\033[38;5;141m" # purple — agent name
C_TOOL="\033[38;5;108m"  # green  — tool call
C_OK="\033[38;5;114m"    # green  — done
C_WARN="\033[38;5;215m"  # amber  — pending / approval
C_DIMK="\033[38;5;245m"  # grey   — meta
C_BOT="\033[38;5;177m"   # pink   — IM bot
C_HEAD="\033[48;5;236m\033[38;5;255m"

# Hide cursor for the duration of the cast (re-shown on exit).
hide_cursor() { printf "\033[?25l"; }
show_cursor() { printf "\033[?25h"; }
trap show_cursor EXIT

# Slow-type a line (per-char), then newline.
type_line() {
    local s="$1" delay="${2:-0.012}"
    local i ch
    for (( i=0; i<${#s}; i++ )); do
        ch="${s:i:1}"
        printf '%s' "$ch"
        sleep "$delay"
    done
    printf '\n'
}

# Print a banner / chrome header at top of screen.
header() {
    local title="$1"
    printf "${C_HEAD}%-90s${C_RESET}\n" " $title"
}

# Claude-Code-style user prompt line.
user_prompt() {
    local text="$1"
    printf "${C_USER}> ${C_RESET}"
    type_line "$text" 0.015
}

# Agent / tool log line (instant).
log() { printf '%b\n' "$1"; }

# Pause helper.
pause() { sleep "$1"; }
