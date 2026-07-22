#!/usr/bin/env sh
# ccteam zero-Rust installer (F166).
#
# Detects the host OS/arch, resolves the latest GitHub Release tarball,
# verifies its SHA-256 against the release-wide SHA256SUMS, and writes a
# single binary to ${CCTEAM_INSTALL_DIR:-$HOME/.local/bin}/ccteam.
#
# Design notes:
#   * POSIX sh only — no bashisms; tested with `dash`.
#   * Strong checksum verification; mismatch aborts non-zero.
#   * No sudo. Writes to $HOME/.local/bin by default.
#   * Override target dir: CCTEAM_INSTALL_DIR=/usr/local/bin sh install.sh
#   * Override tag (CI / pin): CCTEAM_VERSION=<tag> sh install.sh
#   * Post-install daemon launch (v0.9.7): ccteam self-manages the daemon
#     via `ccteam daemon start` (Codex-style setsid pid-detach) — systemd /
#     launchd are retired. On an UPGRADE from a pre-0.9.7 box that was
#     systemd/launchd-managed, we invoke `ccteam daemon start`, whose
#     built-in one-time takeover disables + removes the installer-written
#     unit and restores the service under pid-detach. On a FRESH install we
#     only print the next step (装包 ≠ 启服) unless asked to start.
#     Force behaviour: CCTEAM_POST_INSTALL=start|none sh install.sh
#     (none = detect + print only, never launch).
#   * Uninstall (stops the daemon, removes any legacy unit + the binary;
#     keeps ~/.ccteam):  curl -sSL .../install.sh | sh -s -- --uninstall
#   * Windows is not supported — run ccteam under WSL2 and use the
#     linux-x64 binary (tmux + inotify + POSIX signals are foundational).
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh

set -eu

REPO="${CCTEAM_REPO:-firstintent/ccteam}"
INSTALL_DIR="${CCTEAM_INSTALL_DIR:-$HOME/.local/bin}"

# ---- pretty output helpers (no color when not a TTY) ----
if [ -t 1 ]; then
    BOLD="$(printf '\033[1m')"
    RED="$(printf '\033[31m')"
    GREEN="$(printf '\033[32m')"
    YELLOW="$(printf '\033[33m')"
    RESET="$(printf '\033[0m')"
else
    BOLD=""
    RED=""
    GREEN=""
    YELLOW=""
    RESET=""
fi

info() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
warn() { printf '%s==>%s %swarning:%s %s\n' "$BOLD" "$RESET" "$YELLOW" "$RESET" "$*" >&2; }
err()  { printf '%s==>%s %serror:%s %s\n'   "$BOLD" "$RESET" "$RED"    "$RESET" "$*" >&2; }

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "required command '$1' not found in PATH"
        exit 1
    fi
}

# ---- OS / arch detection ----
detect_target() {
    _os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    _arch="$(uname -m)"
    case "$_os-$_arch" in
        linux-x86_64|linux-amd64)
            SUFFIX="linux-x64"
            EXT="tar.gz"
            ;;
        linux-aarch64|linux-arm64)
            SUFFIX="linux-arm64"
            EXT="tar.gz"
            ;;
        darwin-arm64|darwin-aarch64)
            SUFFIX="macos-arm64"
            EXT="tar.gz"
            ;;
        darwin-x86_64|darwin-amd64)
            SUFFIX="macos-x64"
            EXT="tar.gz"
            ;;
        *)
            err "unsupported platform: $_os-$_arch"
            err "Supported: linux-x86_64, linux-aarch64, darwin-arm64, darwin-x86_64."
            err "Workaround: cargo install --git https://github.com/$REPO ccteam-cli"
            exit 1
            ;;
    esac
}

# ---- pick a downloader (curl preferred, wget fallback) ----
detect_downloader() {
    if command -v curl >/dev/null 2>&1; then
        DL="curl"
    elif command -v wget >/dev/null 2>&1; then
        DL="wget"
    else
        err "need 'curl' or 'wget' on PATH"
        exit 1
    fi
}

download() {
    # download <url> <out-path>
    if [ "$DL" = "curl" ]; then
        curl -fsSL "$1" -o "$2"
    else
        wget -q "$1" -O "$2"
    fi
}

# ---- pick a sha256 verifier ----
detect_sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        SHA256="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        SHA256="shasum -a 256"
    else
        err "need 'sha256sum' or 'shasum' on PATH"
        exit 1
    fi
}

# ---- resolve tag: env override → HTML redirect (primary) → GH API (fallback) ----
resolve_tag() {
    if [ -n "${CCTEAM_VERSION:-}" ]; then
        TAG="$CCTEAM_VERSION"
        info "Using pinned version: $TAG"
        return
    fi
    info "Resolving latest release..."
    # Primary: HTML redirect (no GitHub API rate limit ~60 req/hr per IP).
    # github.com/.../releases/latest 302-redirects to /releases/tag/<version>;
    # grab the Location header (no -L = don't follow, we only want the redirect target).
    TAG="$(curl -sI "https://github.com/$REPO/releases/latest" 2>/dev/null \
        | grep -i '^location:' \
        | sed -E 's|.*/tag/([^[:space:]]+).*|\1|' \
        | tr -d '\r')"
    if [ -n "$TAG" ]; then
        info "Latest release: $TAG"
        return
    fi
    # Fallback: GitHub API (rate-limited; only hit if HTML redirect parse failed).
    info "Redirect parse failed; falling back to GitHub API..."
    _api="https://api.github.com/repos/$REPO/releases/latest"
    _tmp="$(mktemp)"
    if ! download "$_api" "$_tmp"; then
        err "failed to query GitHub API: $_api"
        err "Override: CCTEAM_VERSION=<tag> sh install.sh"
        exit 1
    fi
    # POSIX grep + sed (no jq dependency).
    TAG="$(grep -m1 '"tag_name"' "$_tmp" | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
    rm -f "$_tmp"
    if [ -z "$TAG" ]; then
        err "could not parse tag_name from GitHub API response"
        err "Override: CCTEAM_VERSION=<tag> sh install.sh"
        exit 1
    fi
    info "Latest release (via API fallback): $TAG"
}

# ---- post-install (v0.9.7): ccteam self-manages the daemon via
#      `ccteam daemon start` (setsid pid-detach). systemd / launchd are
#      retired: no unit generation here. On an upgrade from a legacy-managed
#      box we launch `ccteam daemon start`, whose one-time takeover migrates
#      the old unit and restores the service; a fresh install only prints the
#      next step unless explicitly asked to start. ----

daemon_running() {
    command -v pgrep >/dev/null 2>&1 && pgrep -f "ccteam start" >/dev/null 2>&1
}

# True if this box carries a pre-0.9.7 installer-written service unit
# (systemd --user or launchd). Detection only — the actual disable+remove
# lives in `ccteam daemon start`'s Rust takeover (single implementation).
legacy_service_present() {
    _unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ccteam.service"
    _plist="$HOME/Library/LaunchAgents/com.firstintent.ccteam.plist"
    [ -f "$_unit" ] || [ -f "$_plist" ]
}

# Bring the daemon up via the self-managed launcher, then poll `ccteam
# status` for the web URL (up to ~20s) and print it. $1 = binary path.
start_daemon() {
    _bin="$1"
    info "Starting the ccteam daemon (self-managed; log: ~/.ccteam/daemon.log)..."
    # `daemon start` runs the legacy-unit takeover, detaches (setsid), and
    # blocks until the daemon is ready — so status below sees a live socket.
    "$_bin" daemon start || {
        warn "\`ccteam daemon start\` did not report success; check: ccteam daemon status"
        return 0
    }
    _i=0
    _url=""
    while [ "$_i" -lt 20 ]; do
        _url="$("$_bin" status 2>/dev/null | grep -i 'web url:' | head -n1 \
            | sed -E 's/.*web url:[[:space:]]*//' | tr -d '\r')" || _url=""
        if [ -n "$_url" ]; then break; fi
        sleep 1
        _i=$((_i + 1))
    done
    printf '\n%s==>%s %sccteam is up.%s\n\n' "$BOLD" "$RESET" "$GREEN" "$RESET"
    printf '    Restart it with:  ccteam daemon restart\n'
    printf '    Stop it with:     ccteam daemon stop\n\n'
    if [ -n "$_url" ]; then
        printf '    Open the web console to configure Telegram / Feishu and add projects:\n\n'
        printf '      %s\n\n' "$_url"
    else
        warn "daemon did not report a web URL within 20s; check it with: ccteam status"
    fi
}

# Print the fresh-install next step without launching (装包 ≠ 启服).
print_start_hint() {
    info "Installed. Start the daemon when ready:"
    printf '      ccteam daemon start        # background (setsid); prints the web URL\n'
    printf '      ccteam daemon status       # health, pid, version\n'
    printf '      ccteam daemon logs -f      # follow ~/.ccteam/daemon.log\n'
}

post_install() {
    _bin="$1"
    _action="${CCTEAM_POST_INSTALL:-}"

    # `none` → detect + print only, never launch.
    if [ "$_action" = "none" ] || [ "$_action" = "skip" ]; then
        if legacy_service_present; then
            warn "A legacy systemd/launchd ccteam unit is present. systemd/launchd management"
            warn "is retired — migrate on your next start:  ccteam daemon start  (auto-takeover)."
        fi
        print_start_hint
        return 0
    fi

    # Upgrade path: a legacy unit or an already-running daemon means the
    # service was live before this reinstall → `daemon start` takes over the
    # unit and restores the service (restoring prior state ≠ 装包=启服).
    if [ "$_action" = "start" ] || [ "$_action" = "nohup" ] || [ "$_action" = "background" ] \
        || [ "$_action" = "bg" ] || legacy_service_present || daemon_running; then
        if legacy_service_present; then
            info "Detected a legacy systemd/launchd unit — migrating to self-managed daemon."
        fi
        start_daemon "$_bin"
        return 0
    fi

    # Fresh install, no explicit request: offer to start (interactive) or
    # just print the hint (non-interactive / CI).
    if [ -r /dev/tty ]; then
        printf '\n%s==>%s Start the ccteam daemon now? [Y/n] ' "$BOLD" "$RESET"
        read -r _reply </dev/tty || _reply=""
        case "$_reply" in
            [Nn]*) print_start_hint ;;
            *)     start_daemon "$_bin" ;;
        esac
    else
        print_start_hint
    fi
}

# ---- uninstall: stop the daemon, remove any legacy service unit + the
#      binary. State (~/.ccteam) is kept — remove it manually for a purge. ----
do_uninstall() {
    _unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ccteam.service"
    _label="com.firstintent.ccteam"
    _plist="$HOME/Library/LaunchAgents/$_label.plist"
    if [ -x "$INSTALL_DIR/ccteam" ]; then
        # Graceful stop of a self-managed (pid-detach) daemon.
        "$INSTALL_DIR/ccteam" daemon stop >/dev/null 2>&1 || true
    fi
    # Sweep any pre-0.9.7 service unit left behind (retired mechanism).
    if [ -f "$_unit" ]; then
        systemctl --user disable --now ccteam.service >/dev/null 2>&1 || true
        systemctl --user daemon-reload >/dev/null 2>&1 || true
        rm -f "$_unit"
        info "Removed legacy service unit: $_unit"
    fi
    if [ -f "$_plist" ]; then
        launchctl bootout "gui/$(id -u)/$_label" >/dev/null 2>&1 || true
        rm -f "$_plist"
        info "Removed legacy launchd agent: $_plist"
    fi
    if [ -x "$INSTALL_DIR/ccteam" ]; then
        rm -f "$INSTALL_DIR/ccteam"
        info "Removed binary: $INSTALL_DIR/ccteam"
    else
        info "No binary at $INSTALL_DIR/ccteam."
    fi
    info "Kept: ~/.ccteam (config, secrets, state) and per-project .ccteam/ — delete manually for a full purge."
    exit 0
}

# ---- main install ----
main() {
    case "${1:-}" in
        --uninstall|uninstall) do_uninstall ;;
    esac
    if [ -n "${CCTEAM_UNINSTALL:-}" ]; then do_uninstall; fi
    need_cmd uname
    need_cmd tar
    need_cmd mktemp
    need_cmd grep
    need_cmd sed
    detect_target
    detect_downloader
    detect_sha256
    resolve_tag

    # Short-circuit if already at TAG (CCTEAM_FORCE=1 to bypass).
    # `ccteam --version` may print either `ccteam 0.6.8` or `0.6.8`; the
    # release TAG may be `v0.6.8` — compare against both forms.
    if [ -z "${CCTEAM_FORCE:-}" ] && command -v ccteam >/dev/null 2>&1; then
        _current="$(ccteam --version 2>/dev/null | awk 'NR==1{print $NF}')"
        _tag_no_v="${TAG#v}"
        if [ -n "$_current" ] && { [ "$_current" = "$TAG" ] || [ "$_current" = "$_tag_no_v" ]; }; then
            info "Already at $TAG; nothing to do."
            info "(Set CCTEAM_FORCE=1 to reinstall anyway.)"
            exit 0
        fi
    fi

    _asset="ccteam-${TAG}-${SUFFIX}.${EXT}"
    _base="https://github.com/${REPO}/releases/download/${TAG}"
    _url="${_base}/${_asset}"
    _sums_url="${_base}/SHA256SUMS"

    _tmp="$(mktemp -d)"
    trap 'rm -rf "$_tmp"' EXIT INT TERM

    info "Downloading $_asset..."
    if ! download "$_url" "$_tmp/$_asset"; then
        err "download failed: $_url"
        err "If the platform tarball is missing on this release, file an issue."
        exit 1
    fi

    info "Downloading SHA256SUMS..."
    if ! download "$_sums_url" "$_tmp/SHA256SUMS"; then
        err "download failed: $_sums_url"
        exit 1
    fi

    info "Verifying checksum..."
    # `sha256sum -c` insists on running in the same dir as the file paths
    # inside SHA256SUMS; SHA256SUMS uses bare basenames so we cd into
    # $_tmp first. We filter to just our asset line to keep the output
    # tight and to fail fast if it's missing from the manifest.
    (
        cd "$_tmp"
        if ! grep -E "[[:space:]]${_asset}\$" SHA256SUMS > "${_asset}.expected"; then
            err "asset ${_asset} not listed in SHA256SUMS"
            exit 1
        fi
        if ! $SHA256 -c "${_asset}.expected" >/dev/null 2>&1; then
            err "checksum verification FAILED for ${_asset}"
            err "Do not trust this binary. Aborting."
            exit 1
        fi
    )
    info "Checksum OK."

    info "Extracting..."
    tar -xzf "$_tmp/$_asset" -C "$_tmp"
    # Archive layout: ccteam-<tag>-<suffix>/ccteam
    _extracted="$_tmp/ccteam-${TAG}-${SUFFIX}/ccteam"
    if [ ! -f "$_extracted" ]; then
        err "archive layout unexpected — ccteam binary not found at $_extracted"
        exit 1
    fi

    mkdir -p "$INSTALL_DIR"

    # Warn if the daemon is currently running — the new binary won't be
    # picked up until the supervisor is cycled onto it. `post_install` below
    # does exactly that (`ccteam daemon start`/`restart`) when it detects a
    # running or legacy-managed daemon; this is just an early heads-up.
    # `pgrep -f "ccteam start"` matches the argv (not just the comm name),
    # so it won't false-positive on the installer's own shell children.
    if command -v pgrep >/dev/null 2>&1 && pgrep -f "ccteam start" >/dev/null 2>&1; then
        warn "ccteam daemon is running; it will be restarted onto the new binary below."
    fi

    mv "$_extracted" "$INSTALL_DIR/ccteam"
    chmod +x "$INSTALL_DIR/ccteam"
    info "${GREEN}Installed:${RESET} $INSTALL_DIR/ccteam ($TAG)"

    # ---- PATH hint ----
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            ;;
        *)
            warn "$INSTALL_DIR is not on your PATH."
            printf '    Add this to your shell rc file (~/.bashrc, ~/.zshrc, etc.):\n\n'
            printf '      export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
            printf '    Then restart your shell or run: source ~/.bashrc\n'
            ;;
    esac

    # ---- macOS Gatekeeper hint ----
    if [ "$(uname -s)" = "Darwin" ]; then
        warn "macOS users: if Gatekeeper blocks the binary on first run, allow it with:"
        printf '      xattr -d com.apple.quarantine %s/ccteam\n' "$INSTALL_DIR"
    fi

    # ---- version probe (informational; non-fatal if not yet on PATH) ----
    if command -v ccteam >/dev/null 2>&1; then
        info "Version:"
        ccteam --version || true
    fi

    post_install "$INSTALL_DIR/ccteam"
}

main "$@"
