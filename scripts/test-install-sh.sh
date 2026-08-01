#!/usr/bin/env bash
# Smoke test for install.sh — exercises the OS/arch detection, checksum
# verification, and PATH-hint branches against a fake local "release"
# served via a Python HTTP server on a random port (no network, no GH).
#
# Runs all expectations and exits non-zero on first failure. Wire into
# CI / call before pushing a release-related change. Not a `cargo test`
# (it shells out and uses a fake HTTP server), so it lives under scripts/.

set -eu
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL="$REPO_ROOT/install.sh"

if [ ! -f "$INSTALL" ]; then
    echo "install.sh not found at $INSTALL" >&2
    exit 1
fi

# ---- syntax checks ----
sh -n "$INSTALL"
if command -v dash >/dev/null 2>&1; then
    dash -n "$INSTALL"
fi
echo "PASS  syntax (sh + dash)"

# ---- shellcheck (best-effort: only run if installed) ----
if command -v shellcheck >/dev/null 2>&1; then
    shellcheck -s sh "$INSTALL"
    echo "PASS  shellcheck"
fi

# ---- install-location ladder (--print-install-dir) ----
# `make install` resolves BIN_DIR by calling this, so the two install modes
# cannot drift onto two different binaries. Needs no network/server.
ladder_tmp="$(mktemp -d)"
mkdir -p "$ladder_tmp/target/release" "$ladder_tmp/realbin" "$ladder_tmp/linkdir" "$ladder_tmp/ro"
printf '#!/bin/sh\n' >"$ladder_tmp/target/release/ccteam"
printf '#!/bin/sh\n' >"$ladder_tmp/realbin/ccteam"
chmod +x "$ladder_tmp/target/release/ccteam" "$ladder_tmp/realbin/ccteam"
ln -sf "$ladder_tmp/target/release/ccteam" "$ladder_tmp/linkdir/ccteam"
cp "$ladder_tmp/realbin/ccteam" "$ladder_tmp/ro/ccteam"

expect_dir() {
    _want="$1"; _got="$2"; _label="$3"
    if [ "$_got" != "$_want" ]; then
        echo "FAIL  $_label: want '$_want', got '$_got'" >&2
        exit 1
    fi
    echo "PASS  $_label"
}

# Explicit override always wins (CI / packagers pin it).
expect_dir "/opt/ccteam-bin" \
    "$(CCTEAM_INSTALL_DIR=/opt/ccteam-bin HOME="$ladder_tmp" sh "$INSTALL" --print-install-dir)" \
    "ladder: explicit CCTEAM_INSTALL_DIR wins"

# An already-installed ccteam is where the upgrade must land — that is what
# stops a second binary from appearing.
expect_dir "$ladder_tmp/realbin" \
    "$(PATH="$ladder_tmp/realbin:/usr/bin:/bin" HOME="$ladder_tmp" sh "$INSTALL" --print-install-dir)" \
    "ladder: lands on the existing install"

# A build tree is NOT an install location: `cargo clean` or a redirected
# CARGO_TARGET_DIR would otherwise take the daemon's binary with it.
expect_dir "$ladder_tmp/.local/bin" \
    "$(PATH="$ladder_tmp/target/release:/usr/bin:/bin" HOME="$ladder_tmp" sh "$INSTALL" --print-install-dir)" \
    "ladder: skips target/release"

# Same when PATH holds a symlink INTO a build tree (the dangling-symlink
# incident: the link outlived the file it pointed at).
expect_dir "$ladder_tmp/.local/bin" \
    "$(PATH="$ladder_tmp/linkdir:/usr/bin:/bin" HOME="$ladder_tmp" sh "$INSTALL" --print-install-dir)" \
    "ladder: resolves symlinks before judging"

# Existing copy somewhere unwritable (root-owned /usr/local/bin) → fall back
# rather than fail: install.sh is a no-sudo installer.
chmod 555 "$ladder_tmp/ro"
expect_dir "$ladder_tmp/.local/bin" \
    "$(PATH="$ladder_tmp/ro:/usr/bin:/bin" HOME="$ladder_tmp" sh "$INSTALL" --print-install-dir)" \
    "ladder: unwritable existing dir falls back"
chmod 755 "$ladder_tmp/ro"

# Nothing installed yet → the documented default.
expect_dir "$ladder_tmp/.local/bin" \
    "$(PATH="/usr/bin:/bin" HOME="$ladder_tmp" sh "$INSTALL" --print-install-dir)" \
    "ladder: default when nothing is installed"

# ---- shadow-copy warning ----
# A second ccteam earlier on PATH silently wins, so an upgrade looks like it did
# nothing. install.sh reports (never deletes) the others. Source just the two
# helpers so this needs no network.
sed -n '/^canonical_bin()/,/^}/p;/^warn_shadow_copies()/,/^}/p' "$INSTALL" >"$ladder_tmp/fns.sh"
ln -sf "$ladder_tmp/realbin/ccteam" "$ladder_tmp/linkdir/alias-ccteam"
mkdir -p "$ladder_tmp/samefile" && ln -sf "$ladder_tmp/realbin/ccteam" "$ladder_tmp/samefile/ccteam"

shadow_report() {
    PATH="$1" sh -c '
        warn() { printf "WARN %s\n" "$1"; }
        . "$1"
        warn_shadow_copies "$2"
    ' _ "$ladder_tmp/fns.sh" "$2"
}

# `ro/ccteam` is a genuine second copy → reported.
_out="$(shadow_report "$ladder_tmp/realbin:$ladder_tmp/ro:/usr/bin:/bin" "$ladder_tmp/realbin/ccteam")"
case "$_out" in
    *"$ladder_tmp/ro/ccteam"*) echo "PASS  shadow: reports a rival copy" ;;
    *) echo "FAIL  shadow: rival copy not reported. got: $_out" >&2; exit 1 ;;
esac

# `samefile/ccteam` is a SYMLINK to the installed binary — the same file, not a
# rival. Reporting it would be a false alarm on every symlinked install.
_out="$(shadow_report "$ladder_tmp/realbin:$ladder_tmp/samefile:/usr/bin:/bin" "$ladder_tmp/realbin/ccteam")"
if [ -z "$_out" ]; then
    echo "PASS  shadow: symlink to the same binary is not a rival"
else
    echo "FAIL  shadow: false positive on a symlink. got: $_out" >&2
    exit 1
fi

# Nothing else on PATH → silent.
_out="$(shadow_report "$ladder_tmp/realbin:/usr/bin:/bin" "$ladder_tmp/realbin/ccteam")"
if [ -z "$_out" ]; then
    echo "PASS  shadow: silent when the install is the only copy"
else
    echo "FAIL  shadow: spurious warning. got: $_out" >&2
    exit 1
fi
# Removed explicitly, not via `trap`: the end-to-end section below installs its
# own EXIT trap, which would replace ours and leak this dir.
rm -rf "$ladder_tmp"

if ! command -v python3 >/dev/null 2>&1; then
    echo "SKIP  end-to-end (python3 not installed)"
    exit 0
fi

tmp="$(mktemp -d)"
cleanup() {
    if [ -n "${HTTP_PID:-}" ]; then
        kill "$HTTP_PID" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

tag="v0.0.0-test"
suffix="linux-x64"
asset="ccteam-${tag}-${suffix}.tar.gz"
stage="$tmp/ccteam-${tag}-${suffix}"
mkdir -p "$stage"
cat > "$stage/ccteam" <<EOF
#!/bin/sh
echo "ccteam fake $tag"
EOF
chmod +x "$stage/ccteam"
(cd "$tmp" && tar -czf "$asset" "$(basename "$stage")")
(cd "$tmp" && sha256sum "$asset" > SHA256SUMS)

# Fake release tree.
root="$tmp/srv"
fake_repo="fakeorg/fakerepo"
api="$root/api/repos/$fake_repo/releases"
dl="$root/$fake_repo/releases/download/$tag"
mkdir -p "$api" "$dl"
printf '{"tag_name":"%s"}\n' "$tag" > "$api/latest"
cp "$tmp/$asset" "$dl/$asset"
cp "$tmp/SHA256SUMS" "$dl/SHA256SUMS"

# ---- start a quiet HTTP server on a random port, recover the port ----
port_file="$tmp/port"
python3 - "$root" "$port_file" <<'PY' &
import http.server, socketserver, sys, os
root, port_file = sys.argv[1], sys.argv[2]
os.chdir(root)
class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a, **k): pass
with socketserver.TCPServer(("127.0.0.1", 0), Quiet) as httpd:
    with open(port_file, "w") as f:
        f.write(str(httpd.server_address[1]))
    httpd.serve_forever()
PY
HTTP_PID=$!

# Wait up to ~5s for the port to be written.
for _ in $(seq 1 50); do
    if [ -s "$port_file" ]; then
        break
    fi
    sleep 0.1
done
if [ ! -s "$port_file" ]; then
    echo "FAIL  http server didn't start (no port file)" >&2
    exit 1
fi
port="$(cat "$port_file")"

# Patch install.sh on the fly to talk to our fake server.
shim="$tmp/install-shim.sh"
sed \
    -e "s|https://api.github.com/repos/|http://127.0.0.1:${port}/api/repos/|g" \
    -e "s|https://github.com/|http://127.0.0.1:${port}/|g" \
    "$INSTALL" > "$shim"
chmod +x "$shim"

# ---- happy-path install ----
sandbox="$tmp/install"
mkdir -p "$sandbox"
if CCTEAM_REPO="$fake_repo" CCTEAM_INSTALL_DIR="$sandbox" \
       sh "$shim" > "$tmp/install.log" 2>&1; then
    if [ -x "$sandbox/ccteam" ] && [ "$("$sandbox/ccteam")" = "ccteam fake $tag" ]; then
        echo "PASS  happy-path install (binary placed + executable)"
    else
        echo "FAIL  binary missing/non-functional after install" >&2
        cat "$tmp/install.log" >&2
        exit 1
    fi
else
    echo "FAIL  install.sh returned non-zero in happy path" >&2
    cat "$tmp/install.log" >&2
    exit 1
fi

# ---- PINNED-tag path (CCTEAM_VERSION) ----
rm -rf "$sandbox" && mkdir -p "$sandbox"
if CCTEAM_REPO="$fake_repo" CCTEAM_INSTALL_DIR="$sandbox" CCTEAM_VERSION="$tag" \
       sh "$shim" > "$tmp/install-pin.log" 2>&1; then
    if grep -q "Using pinned version: $tag" "$tmp/install-pin.log"; then
        echo "PASS  CCTEAM_VERSION pin uses env tag (skips API)"
    else
        echo "FAIL  CCTEAM_VERSION pin not honoured" >&2
        cat "$tmp/install-pin.log" >&2
        exit 1
    fi
else
    echo "FAIL  pinned-version install failed" >&2
    cat "$tmp/install-pin.log" >&2
    exit 1
fi

# ---- checksum-tamper: corrupt SHA256SUMS, expect abort ----
cp "$dl/SHA256SUMS" "$dl/SHA256SUMS.orig"
sed -e 's/^[0-9a-f]\{8\}/00000000/' "$dl/SHA256SUMS.orig" > "$dl/SHA256SUMS"
rm -rf "$sandbox" && mkdir -p "$sandbox"
if CCTEAM_REPO="$fake_repo" CCTEAM_INSTALL_DIR="$sandbox" \
       sh "$shim" > "$tmp/install-bad.log" 2>&1; then
    echo "FAIL  install.sh accepted a corrupted SHA256SUMS (security regression!)" >&2
    cat "$tmp/install-bad.log" >&2
    exit 1
fi
if ! grep -qi "checksum.*FAIL" "$tmp/install-bad.log"; then
    echo "FAIL  expected 'checksum FAILED' diagnostic in log" >&2
    cat "$tmp/install-bad.log" >&2
    exit 1
fi
echo "PASS  checksum-tamper aborts with non-zero exit"
cp "$dl/SHA256SUMS.orig" "$dl/SHA256SUMS"

# ---- missing-asset entry in SHA256SUMS ----
grep -v "$asset" "$dl/SHA256SUMS" > "$dl/SHA256SUMS.empty" || true
mv "$dl/SHA256SUMS.empty" "$dl/SHA256SUMS"
rm -rf "$sandbox" && mkdir -p "$sandbox"
if CCTEAM_REPO="$fake_repo" CCTEAM_INSTALL_DIR="$sandbox" \
       sh "$shim" > "$tmp/install-missing.log" 2>&1; then
    echo "FAIL  install.sh accepted missing asset entry" >&2
    cat "$tmp/install-missing.log" >&2
    exit 1
fi
if ! grep -qi "not listed in SHA256SUMS" "$tmp/install-missing.log"; then
    echo "FAIL  expected 'not listed in SHA256SUMS' diagnostic" >&2
    cat "$tmp/install-missing.log" >&2
    exit 1
fi
echo "PASS  missing-asset aborts cleanly"
cp "$dl/SHA256SUMS.orig" "$dl/SHA256SUMS"

# ---- unsupported-platform branch (force via env-mock of `uname`) ----
# We can't easily change uname inside the same process; instead exercise
# the detection helper by running install.sh in a sub-shell where PATH
# is prefixed with a wrapper-uname that lies about the arch.
fakebin="$tmp/fakebin"
mkdir -p "$fakebin"
cat > "$fakebin/uname" <<'EOF'
#!/bin/sh
if [ "$1" = "-s" ]; then echo "FreeBSD"; exit 0; fi
if [ "$1" = "-m" ]; then echo "x86_64";  exit 0; fi
exec /usr/bin/uname "$@"
EOF
chmod +x "$fakebin/uname"
rm -rf "$sandbox" && mkdir -p "$sandbox"
if PATH="$fakebin:$PATH" CCTEAM_REPO="$fake_repo" CCTEAM_INSTALL_DIR="$sandbox" \
       sh "$shim" > "$tmp/install-unsupported.log" 2>&1; then
    echo "FAIL  install.sh accepted unsupported platform" >&2
    cat "$tmp/install-unsupported.log" >&2
    exit 1
fi
if ! grep -qi "unsupported platform" "$tmp/install-unsupported.log"; then
    echo "FAIL  expected 'unsupported platform' diagnostic" >&2
    cat "$tmp/install-unsupported.log" >&2
    exit 1
fi
echo "PASS  unsupported-platform aborts with friendly message"

echo
echo "All install.sh smoke tests passed."
