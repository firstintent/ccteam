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
#   * Override tag (CI / pin): CCTEAM_VERSION=v0.6.6 sh install.sh
#   * Windows is not supported — use the GitHub Releases page (zip).
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
        darwin-arm64|darwin-aarch64)
            SUFFIX="macos-arm64"
            EXT="tar.gz"
            ;;
        darwin-x86_64|darwin-amd64)
            SUFFIX="macos-x64"
            EXT="tar.gz"
            ;;
        linux-aarch64|linux-arm64)
            err "linux-arm64 prebuilt is not yet published (planned post-V0.7)."
            err "Workaround: cargo install --git https://github.com/$REPO ccteam-cli"
            exit 1
            ;;
        *)
            err "unsupported platform: $_os-$_arch"
            err "Supported: linux-x86_64, darwin-arm64, darwin-x86_64."
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

# ---- resolve tag: env override → GH API "latest" ----
resolve_tag() {
    if [ -n "${CCTEAM_VERSION:-}" ]; then
        TAG="$CCTEAM_VERSION"
        info "Using pinned version: $TAG"
        return
    fi
    info "Resolving latest release..."
    _api="https://api.github.com/repos/$REPO/releases/latest"
    _tmp="$(mktemp)"
    if ! download "$_api" "$_tmp"; then
        err "failed to query GitHub API: $_api"
        exit 1
    fi
    # POSIX grep + sed (no jq dependency).
    TAG="$(grep -m1 '"tag_name"' "$_tmp" | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
    rm -f "$_tmp"
    if [ -z "$TAG" ]; then
        err "could not parse tag_name from GitHub API response"
        err "Override: CCTEAM_VERSION=v0.6.6 sh install.sh"
        exit 1
    fi
    info "Latest release: $TAG"
}

# ---- main install ----
main() {
    need_cmd uname
    need_cmd tar
    need_cmd mktemp
    need_cmd grep
    need_cmd sed
    detect_target
    detect_downloader
    detect_sha256
    resolve_tag

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

    info "Next: open a Claude session and run /ccteam \"<what you want>\"."
}

main "$@"
