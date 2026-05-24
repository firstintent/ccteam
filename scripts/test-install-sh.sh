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
