# ccteam Makefile — thin convenience wrappers around cargo / npm / the CLI.
#
#   make gate            # full pre-push gate: fmt + clippy + tests + SPA
#   make install         # THE install: build release + copy to $(BIN_DIR)/ccteam
#                        #   + `ccteam daemon restart` (self-managed setsid daemon;
#                        #   migrates any legacy systemd/launchd unit) + next steps
#   make start           # run the daemon in the FOREGROUND (dev / one-off)
#   make wipe            # reset runtime state (keeps secrets + config + routing)
#
# Override locations:  make BIN_DIR=/usr/local/bin install     # this run only
#                       CCTEAM_INSTALL_DIR=/usr/local/bin ...   # both install modes
#                       make WEB_PORT=8080 start
#                       make CCTEAM_HOME=~/.ccteam2 wipe
#
# `make install` and `install.sh` install the SAME file: BIN_DIR is resolved by
# calling `install.sh --print-install-dir`, so prefer CCTEAM_INSTALL_DIR — it is
# the knob both modes read.

# --- Configuration -----------------------------------------------------------

# Install destination. `make install` and `install.sh` MUST land on the same
# file, or a machine ends up with two ccteam binaries and whichever sorts first
# on PATH silently wins ("I rebuilt and it still misbehaves").
#
# So the ladder lives in exactly one place — install.sh — and this asks it:
# explicit CCTEAM_INSTALL_DIR/BIN_DIR → wherever ccteam already lives (skipping
# build trees) → $(HOME)/.local/bin. Reimplementing it here would recreate the
# very drift it prevents. The fallback covers a checkout without install.sh.
BIN_DIR      ?= $(shell sh $(CURDIR)/install.sh --print-install-dir 2>/dev/null || echo $(HOME)/.local/bin)
BIN_NAME     := ccteam
BIN_LINK     := $(BIN_DIR)/$(BIN_NAME)
# Cargo may redirect its target directory through CARGO_TARGET_DIR or
# .cargo/config.toml. Resolve the same directory Cargo uses instead of assuming
# every build lands below this checkout. The first arm avoids invoking
# `cargo metadata` when the caller already supplied the environment variable.
CARGO_TARGET_DIR_RESOLVED = $(or $(strip $(CARGO_TARGET_DIR)),$(shell cargo metadata --no-deps --format-version 1 2>/dev/null | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p'))
RELEASE_BIN  = $(abspath $(CARGO_TARGET_DIR_RESOLVED))/release/$(BIN_NAME)
CCTEAM_HOME  ?= $(HOME)/.ccteam
WEB_DIR      := $(CURDIR)/crates/ccteam-web/web
WEB_PORT     ?= 7331
# The actual bind passed to `ccteam start --web-bind`. 0.0.0.0 => LAN-reachable
# with token auth auto-enabled; use WEB_BIND=127.0.0.1:$(WEB_PORT) for loopback.
WEB_BIND     ?= 0.0.0.0:$(WEB_PORT)

# Runtime dirs reset by `make wipe` (~/.ccteam/{state,run,cache}). Kept:
# secrets/ (web-token, IM creds, per-user files), config.yaml, routing.md,
# hooks/ — so creds + prefs survive a reset.
WIPE_DIRS    := state run cache

# v0.9.7 — the daemon is self-managed via `ccteam daemon *` (Codex-style
# setsid pid-detach); systemd/launchd are retired. The daemon log lives at
# $(CCTEAM_HOME)/daemon.log on every platform.
DAEMON_LOG   := $(CCTEAM_HOME)/daemon.log

.DEFAULT_GOAL := help
.PHONY: help build release clean fmt fmt-check clippy check test test-web \
        web web-deps web-check gate \
        install install-binary next-steps uninstall reinstall require-cli require-node \
        config init start stop status doctor \
        daemon-start daemon-stop \
        daemon-restart daemon-status daemon-logs \
        wipe nuke

# --- Help --------------------------------------------------------------------

help:
	@printf '\033[1mccteam Makefile\033[0m\n\n'
	@printf '\033[1mBuild & test\033[0m\n'
	@printf '  make build         cargo build (debug)\n'
	@printf '  make release       cargo build --release\n'
	@printf '  make fmt           cargo fmt --all\n'
	@printf '  make clippy        cargo clippy --workspace --all-targets -D warnings\n'
	@printf '  make test          cargo test (workspace, excl ccteam-web)\n'
	@printf '  make test-web      cargo test -p ccteam-web\n'
	@printf '  make web           build the SPA (tsc + vite)\n'
	@printf '  make web-check     SPA eslint + vitest\n'
	@printf '  \033[1mmake gate\033[0m          full pre-push gate (fmt+clippy+test+test-web+web-check)\n'
	@printf '  make clean         cargo clean + rm SPA dist\n\n'
	@printf '\033[1mInstall\033[0m\n'
	@printf '  \033[1mmake install\033[0m       build release + atomic copy + `ccteam daemon restart` (self-managed daemon)\n'
	@printf '  make uninstall     stop the daemon + remove the executable (state untouched)\n'
	@printf '  make reinstall     uninstall + install\n\n'
	@printf '\033[1mRun foreground (daemon = IM gateway + web UI + MCP, one process)\033[0m\n'
	@printf '  make config        ccteam config   (register MCP + IM creds + prefs)\n'
	@printf '  make init          ccteam init     (initialize the current dir as a project)\n'
	@printf '  make start         ccteam start    (foreground; web UI at %s)\n' '$(WEB_BIND)'
	@printf '  make status        ccteam status   (daemon health + sessions)\n'
	@printf '  make doctor        ccteam doctor --verify-mcp\n'
	@printf '  make stop          ccteam stop     (stop the daemon; sessions resume on next start)\n\n'
	@printf '\033[1mDaemon ops (self-managed setsid daemon; Linux / macOS / WSL, one mechanism)\033[0m\n'
	@printf '  make daemon-status     ccteam daemon status  (pid / ready / running-vs-binary version)\n'
	@printf '  make daemon-logs       ccteam daemon logs -f  (%s)\n' '$(DAEMON_LOG)'
	@printf '  make daemon-restart    rebuild + deploy release + ccteam daemon restart\n'
	@printf '  make daemon-stop       ccteam daemon stop\n'
	@printf '  make daemon-start      ccteam daemon start\n\n'
	@printf '\033[1mState reset (destructive)\033[0m\n'
	@printf '  make wipe          rm %s/{%s} (keeps secrets/, hooks/, config.yaml, routing.md)\n' '$(CCTEAM_HOME)' 'state,run,cache'
	@printf '  make nuke          rm -rf %s   (requires CONFIRM=1)\n' '$(CCTEAM_HOME)'

# --- Build & test ------------------------------------------------------------

# Propagate an explicit skip to the ccteam-web build.rs whether it arrives as an
# environment variable (CCTEAM_SKIP_WEB_BUILD=1 make ...) or a make variable
# (make CCTEAM_SKIP_WEB_BUILD=1 ...) — make exports only env-origin vars by default.
ifdef CCTEAM_SKIP_WEB_BUILD
export CCTEAM_SKIP_WEB_BUILD
endif

# A command-line make override must reach Cargo too. Environment-origin
# variables are already exported, but spelling this out also covers
# `make CARGO_TARGET_DIR=... install`.
ifdef CARGO_TARGET_DIR
export CARGO_TARGET_DIR
endif

# Preflight for from-source builds. `make build/release/web/clippy/test/...`
# compile the ccteam-web crate, whose build.rs shells out to `npm` to bundle the
# web console — so this machine needs Node.js (node + npm). This is a DEVELOPER
# prerequisite ONLY: end users install the prebuilt binary
# (`curl ... install.sh | sh`) with the SPA already baked in and never touch
# Node. Fail fast here with the exact fix instead of letting build.rs die
# cryptically mid-compile. Bypass for a CLI/daemon-only build (placeholder web
# console — also how cross-machine satellites build): CCTEAM_SKIP_WEB_BUILD=1.
require-node:
	@if [ "$(CCTEAM_SKIP_WEB_BUILD)" = "1" ]; then \
	    printf '\033[33m==>\033[0m CCTEAM_SKIP_WEB_BUILD=1 set — skipping the SPA build (placeholder web console).\n'; \
	elif command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then \
	    :; \
	else \
	    printf '\033[31merror:\033[0m Node.js (node + npm) not found — required to build the web console from source.\n' >&2; \
	    printf '    Fix one of:\n' >&2; \
	    if [ "$$(uname -s)" = "Darwin" ]; then \
	        printf '      brew install node                          # then re-run\n' >&2; \
	    else \
	        printf '      sudo apt install nodejs npm                 # (or your distro equivalent), then re-run\n' >&2; \
	    fi; \
	    printf '      CCTEAM_SKIP_WEB_BUILD=1 make %s   # CLI/daemon only, placeholder web console\n' '$(MAKECMDGOALS)' >&2; \
	    printf '\n    End users need neither Rust nor Node — install the prebuilt binary instead:\n' >&2; \
	    printf '      curl -sSL https://raw.githubusercontent.com/firstintent/ccteam/main/install.sh | sh\n' >&2; \
	    exit 1; \
	fi

build: require-node
	cargo build

release: require-node
	cargo build --release

clean:
	cargo clean
	@rm -rf $(WEB_DIR)/dist

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy: require-node
	cargo clippy --workspace --all-targets -- -D warnings

# Quick correctness gate without tests (fmt + clippy; clippy type-checks too).
check: fmt-check clippy

# Rust core gate — ccteam-web runs separately (its WS/PTY tests need a real
# terminal). `--no-fail-fast` so one env-flaky test doesn't mask the count.
test: require-node
	cargo test --workspace --exclude ccteam-web --no-fail-fast

# Deterministic baseline gate — the subset whose pass-count is recorded as the
# baseline in `.loop/state.md` (「基线只增不减」). The target selectors keep the
# hang-prone / env-flaky integration tests (`tests/*.rs`) out, so the count is
# stable. `--bins` is load-bearing: a binary-only crate has NO lib target, so a
# `--lib`-only run covered ZERO of `ccteam-cli`'s tests — that blind spot let a
# broken `web_chat_bridge` restart test rot unnoticed on main. Lives here, not
# as prose in `.loop/`, so the gate has one executable home.
test-baseline: require-node
	cargo test --workspace --exclude ccteam-web --lib --bins --no-fail-fast

test-web: require-node
	cargo test -p ccteam-web

web-deps: require-node
	cd $(WEB_DIR) && npm ci

web: require-node
	cd $(WEB_DIR) && npm run build

web-check: require-node
	cd $(WEB_DIR) && npm run lint && npm run test:unit

# The full pre-push gate. Mirrors CI + the ship discipline.
gate: fmt-check clippy test test-web web-check
	@printf '\n\033[32mgate green.\033[0m\n'

# --- Install / uninstall -----------------------------------------------------
#
# `make install` is THE product install: release build → atomic executable
# copy → `ccteam daemon restart` (self-managed setsid daemon). The installed
# executable is deliberately independent of Cargo's build tree: shared target
# cleanup or a redirected CARGO_TARGET_DIR must never break the live daemon.
# `daemon restart` runs the one-time legacy-unit takeover, so a dev box that
# used to run the old systemd/launchd service migrates cleanly on this install.
#
# It also stamps ~/.ccteam/install-channel as `source`. That matters BECAUSE the
# destination is now shared with install.sh: `ccteam update` can no longer tell
# the two apart by path, and without this marker it classifies a locally built
# binary as `standalone` (the ~/.local/bin heuristic) and replaces it with the
# latest published release — silently discarding the build under test. The
# schema is owned by `ccteam_core::install_channel::InstallMarker`; unknown
# fields are ignored, and `tag` is absent here on purpose (no release tag).

install-binary: release
	@set -eu; \
	_release_bin="$(RELEASE_BIN)"; \
	if [ ! -x "$$_release_bin" ]; then \
	    printf '\033[31merror:\033[0m Cargo release binary not found: %s\n' "$$_release_bin" >&2; \
	    exit 1; \
	fi; \
	mkdir -p "$(BIN_DIR)"; \
	_tmp="$$(mktemp "$(BIN_DIR)/.$(BIN_NAME).install.XXXXXX")"; \
	trap 'rm -f "$$_tmp"' EXIT HUP INT TERM; \
	install -m 755 "$$_release_bin" "$$_tmp"; \
	mv -f "$$_tmp" "$(BIN_LINK)"; \
	trap - EXIT HUP INT TERM; \
	printf 'installed: %s (copied from %s)\n' '$(BIN_LINK)' "$$_release_bin"; \
	_marker="$(CCTEAM_HOME)/install-channel"; \
	mkdir -p "$(CCTEAM_HOME)" 2>/dev/null || true; \
	printf '{\n  "channel": "source",\n  "bin": "%s",\n  "installed_at": "%s"\n}\n' \
	    '$(BIN_LINK)' "$$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo '')" \
	    > "$$_marker" 2>/dev/null || true

install: install-binary
	@case ":$$PATH:" in \
	    *:$(BIN_DIR):*) ;; \
	    *) printf '\033[33mwarning:\033[0m %s is not on PATH; add it to your shell rc.\n' '$(BIN_DIR)' ;; \
	esac
	@$(BIN_LINK) daemon restart
	@$(MAKE) --no-print-directory next-steps

# Internal: poll the fresh daemon for its web URL (≤20s; the `web url:` line of
# `ccteam status` is a ready-to-click login link), then print where to finish
# setup — MCP registration, projects, and IM credentials all live in the console.
next-steps:
	@_url=""; _i=0; \
	while [ "$$_i" -lt 20 ]; do \
	    _url="$$($(BIN_LINK) status 2>/dev/null | grep -i 'web url:' | head -n1 \
	        | sed -E 's/.*web url:[[:space:]]*//' | tr -d '\r')"; \
	    [ -n "$$_url" ] && break; \
	    sleep 1; _i=$$((_i + 1)); \
	done; \
	printf '\n\033[32mccteam is up\033[0m — self-managed daemon (setsid; survives logout).\n'; \
	printf '  Not auto-started on boot: re-run `ccteam daemon start` after a reboot\n'; \
	printf '  (or add it to your login shell / a @reboot cron).\n\n'; \
	if [ -n "$$_url" ]; then \
	    printf '  Web console:  %s\n' "$$_url"; \
	else \
	    printf '  Web console:  daemon still starting — get the login link with:  %s status\n' '$(BIN_NAME)'; \
	fi; \
	printf '                (token also at %s/secrets/web-token)\n\n' '$(CCTEAM_HOME)'; \
	printf '  Finish setup in the console:\n'; \
	printf '    1. Register MCP (one-time)\n'; \
	printf '    2. Create a project\n'; \
	printf '    3. Settings -> IM: connect Telegram (bot token) or Lark (App ID/Secret)\n\n'; \
	printf '  Ops: make daemon-logs | make daemon-restart | make daemon-stop\n'

uninstall:
	@if command -v $(BIN_NAME) >/dev/null 2>&1; then \
	    $(BIN_NAME) daemon stop >/dev/null 2>&1 || true; \
	    printf 'stopped the self-managed daemon (if running).\n'; \
	fi
	@if [ -L $(BIN_LINK) ] || [ -f $(BIN_LINK) ]; then \
	    rm -f $(BIN_LINK) && printf 'removed: %s (state/config untouched)\n' '$(BIN_LINK)'; \
	else \
	    printf 'not installed: %s\n' '$(BIN_LINK)'; \
	fi
	@printf 'note: any pre-0.9.7 systemd/launchd unit is swept by `ccteam daemon start` (auto-takeover) or `install.sh --uninstall`.\n'

reinstall: uninstall install

# Shared guard: the run/ops targets need the CLI on PATH.
require-cli:
	@command -v $(BIN_NAME) >/dev/null 2>&1 || { \
	    printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }

# --- Run (foreground) --------------------------------------------------------
#
# `ccteam start` runs ONE process: the IM gateway + the embedded web UI + the
# MCP socket. Foreground only — the unattended, auto-restarting service is what
# `make install` sets up. Sessions spawn on demand and resume by sid across
# restarts.

config: require-cli
	$(BIN_NAME) config

init: require-cli
	$(BIN_NAME) init

start: require-cli
	@printf 'web UI: http://%s  (token printed below if auth is on)\n' '$(WEB_BIND)'
	$(BIN_NAME) start --web-bind $(WEB_BIND)

status: require-cli
	$(BIN_NAME) status

doctor: require-cli
	$(BIN_NAME) doctor --verify-mcp

stop: require-cli
	$(BIN_NAME) stop

# --- Daemon ops (self-managed setsid daemon; one mechanism, all platforms) ----
#
# Why: `ccteam start` is a FOREGROUND process tied to its terminal. `ccteam
# daemon start` detaches it (setsid): stdout/stderr → $(DAEMON_LOG), a JSON pid
# record proves ownership, and the daemon traps SIGTERM into a graceful
# shutdown (sessions resume by sid on next start). This is the SAME mechanism
# on Linux / macOS / WSL — systemd/launchd are retired (v0.9.7). The daemon
# spawns claude/codex from PATH, so the detaching shell's PATH must include
# $(BIN_DIR) and the vendor CLIs (setsid inherits the caller's env — a win over
# the old unit's hand-maintained PATH).
#
# Honest tradeoff: no supervisor means no crash-restart / boot-start. Re-run
# `make daemon-start` (or `ccteam daemon start`) after a reboot; `ccteam
# status` / `ccteam doctor` surface a down daemon at a glance.

daemon-start:
	@$(BIN_NAME) daemon start

daemon-stop:
	@$(BIN_NAME) daemon stop

# Rebuild, atomically deploy the executable, then restart that exact binary.
daemon-restart: install-binary
	@$(BIN_LINK) daemon restart

daemon-status:
	@$(BIN_NAME) daemon status

daemon-logs:
	@$(BIN_NAME) daemon logs -f

# --- State reset (destructive) -----------------------------------------------

wipe:
	@if [ ! -d "$(CCTEAM_HOME)" ]; then printf 'no %s; nothing to wipe\n' '$(CCTEAM_HOME)'; exit 0; fi
	@for d in $(WIPE_DIRS); do \
	    full="$(CCTEAM_HOME)/$$d"; \
	    if [ -d "$$full" ]; then rm -rf "$$full" && printf 'wiped: %s\n' "$$full"; fi; \
	done
	@printf 'kept: %s/{secrets,hooks}  %s/config.yaml\n' '$(CCTEAM_HOME)' '$(CCTEAM_HOME)'

nuke:
	@if [ "$(CONFIRM)" != "1" ]; then \
	    printf '\033[31mrefusing to nuke without CONFIRM=1\033[0m — would `rm -rf %s`.\n' '$(CCTEAM_HOME)'; \
	    printf 'rerun:  make nuke CONFIRM=1\n'; \
	    exit 1; \
	fi
	@if [ -d "$(CCTEAM_HOME)" ]; then rm -rf "$(CCTEAM_HOME)" && printf 'rmrf: %s\n' '$(CCTEAM_HOME)'; fi
