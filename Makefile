# ccteam Makefile — thin convenience wrappers around cargo / npm / the CLI.
#
#   make gate            # full pre-push gate: fmt + clippy + tests + SPA
#   make install         # build release + symlink to $(BIN_DIR)/ccteam
#   make start           # run the daemon in the FOREGROUND (dev / one-off)
#   make daemon-install  # supervise the daemon via systemd --user (survives
#                        #   logout, auto-restarts on crash/OOM, starts at boot)
#   make wipe            # reset runtime state (keeps secrets + config)
#
# Override locations:  make BIN_DIR=/usr/local/bin install
#                       make WEB_PORT=8080 start
#                       make CCTEAM_HOME=~/.ccteam2 wipe

# --- Configuration -----------------------------------------------------------

BIN_DIR      ?= $(HOME)/.local/bin
BIN_NAME     := ccteam
BIN_LINK     := $(BIN_DIR)/$(BIN_NAME)
RELEASE_BIN  := $(CURDIR)/target/release/$(BIN_NAME)
CCTEAM_HOME  ?= $(HOME)/.ccteam
WEB_DIR      := $(CURDIR)/crates/ccteam-web/web
WEB_PORT     ?= 7331
# The actual bind passed to `ccteam start --web-bind`. 0.0.0.0 => LAN-reachable
# with token auth auto-enabled; use WEB_BIND=127.0.0.1:$(WEB_PORT) for loopback.
WEB_BIND     ?= 0.0.0.0:$(WEB_PORT)

# Runtime dirs reset by `make wipe` (~/.ccteam/{state,run,cache}). Kept:
# secrets/ (web-token, IM creds, per-user files), config.yaml, hooks/ — so
# creds + prefs survive a reset.
WIPE_DIRS    := state run cache

# systemd --user supervision (see the "Daemon" section).
SYSTEMD_DIR  := $(HOME)/.config/systemd/user
UNIT_NAME    := ccteam.service
UNIT_FILE    := $(SYSTEMD_DIR)/$(UNIT_NAME)

.DEFAULT_GOAL := help
.PHONY: help build release clean fmt fmt-check clippy check test test-web \
        web web-deps web-check gate \
        install uninstall reinstall require-cli \
        config init start stop status doctor \
        daemon-install daemon-uninstall daemon-start daemon-stop \
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
	@printf '  make install       symlink %s -> %s\n' '$(RELEASE_BIN)' '$(BIN_LINK)'
	@printf '  make uninstall     remove the symlink (state untouched)\n'
	@printf '  make reinstall     uninstall + install\n\n'
	@printf '\033[1mRun foreground (daemon = IM gateway + web UI + MCP, one process)\033[0m\n'
	@printf '  make config        ccteam config   (register MCP + IM creds + prefs)\n'
	@printf '  make init          ccteam init     (initialize the current dir as a project)\n'
	@printf '  make start         ccteam start    (foreground; web UI at %s)\n' '$(WEB_BIND)'
	@printf '  make status        ccteam status   (daemon health + sessions)\n'
	@printf '  make doctor        ccteam doctor --verify-mcp\n'
	@printf '  make stop          ccteam stop     (stop the daemon; sessions resume on next start)\n\n'
	@printf '\033[1mDaemon supervision (systemd --user: survives logout, auto-restart, boot)\033[0m\n'
	@printf '  make daemon-install    write %s, enable + start\n' '$(UNIT_NAME)'
	@printf '  make daemon-status     systemctl --user status\n'
	@printf '  make daemon-logs       journalctl --user -u %s -f\n' '$(UNIT_NAME)'
	@printf '  make daemon-restart    rebuild release + restart the service\n'
	@printf '  make daemon-stop       stop (does NOT disable; boot-start stays on)\n'
	@printf '  make daemon-uninstall  disable + remove the unit\n\n'
	@printf '\033[1mState reset (destructive)\033[0m\n'
	@printf '  make wipe          rm %s/{%s} (keeps secrets/, hooks/, config.yaml)\n' '$(CCTEAM_HOME)' 'state,run,cache'
	@printf '  make nuke          rm -rf %s   (requires CONFIRM=1)\n' '$(CCTEAM_HOME)'

# --- Build & test ------------------------------------------------------------

build:
	cargo build

release:
	cargo build --release

clean:
	cargo clean
	@rm -rf $(WEB_DIR)/dist

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# Quick correctness gate without tests (fmt + clippy; clippy type-checks too).
check: fmt-check clippy

# Rust core gate — ccteam-web runs separately (its WS/PTY tests need a real
# terminal). `--no-fail-fast` so one env-flaky test doesn't mask the count.
test:
	cargo test --workspace --exclude ccteam-web --no-fail-fast

test-web:
	cargo test -p ccteam-web

web-deps:
	cd $(WEB_DIR) && npm ci

web:
	cd $(WEB_DIR) && npm run build

web-check:
	cd $(WEB_DIR) && npm run lint && npm run test:unit

# The full pre-push gate. Mirrors CI + the ship discipline.
gate: fmt-check clippy test test-web web-check
	@printf '\n\033[32mgate green.\033[0m\n'

# --- Install / uninstall -----------------------------------------------------
#
# Symlink (not copy): a fresh `cargo build --release` is picked up without a
# re-install. For pinned-binary semantics use `install -m0755 $(RELEASE_BIN)`.

install: release
	@mkdir -p $(BIN_DIR)
	@ln -sf $(RELEASE_BIN) $(BIN_LINK)
	@printf 'installed: %s -> %s\n' '$(BIN_LINK)' '$(RELEASE_BIN)'
	@case ":$$PATH:" in \
	    *:$(BIN_DIR):*) ;; \
	    *) printf '\033[33mwarning:\033[0m %s is not on PATH; add it to your shell rc.\n' '$(BIN_DIR)' ;; \
	esac

uninstall:
	@if [ -L $(BIN_LINK) ] || [ -f $(BIN_LINK) ]; then \
	    rm -f $(BIN_LINK) && printf 'removed: %s\n' '$(BIN_LINK)'; \
	else \
	    printf 'not installed: %s\n' '$(BIN_LINK)'; \
	fi

reinstall: uninstall install

# Shared guard: the run/ops targets need the CLI on PATH.
require-cli:
	@command -v $(BIN_NAME) >/dev/null 2>&1 || { \
	    printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }

# --- Run (foreground) --------------------------------------------------------
#
# `ccteam start` runs ONE process: the IM gateway + the embedded web UI + the
# MCP socket. Foreground only — for an unattended, auto-restarting service use
# the systemd targets below. Sessions spawn on demand and resume by sid across
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

# --- Daemon supervision (systemd --user) -------------------------------------
#
# Why: `ccteam start` is a FOREGROUND process tied to its terminal. Started by
# hand (e.g. `... >/tmp/ccteam.log 2>&1 &`) it dies on SIGHUP when the shell /
# SSH session closes — healthy one moment, gone the next, no crash trace. A
# systemd --user service fixes that: it owns the process, restarts it on
# crash/OOM, and (with linger, already enabled here) keeps it running across
# logout and across reboot. The daemon traps SIGTERM into its graceful
# shutdown, so `systemctl --user stop` is a clean stop (sessions resume by sid
# on next start). Logs go to the journal: `make daemon-logs`.
#
# The unit is generated below (no checked-in template). ExecStart + PATH honor
# $(BIN_DIR); the daemon spawns claude/codex from PATH, so $(BIN_DIR) MUST be
# on it. Tune the (commented) memory caps to your host before enabling if you
# want the daemon contained rather than relying on restart-after-OOM.

define CCTEAM_UNIT
[Unit]
Description=ccteam daemon (IM gateway + web UI + MCP)
Documentation=https://github.com/firstintent/ccteam
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=exec
ExecStart=$(BIN_LINK) start --web-bind $(WEB_BIND)
WorkingDirectory=$(HOME)
# Daemon discovers claude/codex via PATH — user-local bins must be visible.
Environment=PATH=$(BIN_DIR):/usr/local/bin:/usr/bin:/bin
# Graceful: SIGTERM -> the daemon's own clean shutdown path. Give it room
# before systemd escalates to SIGKILL (CLI `stop` waits ~35s).
KillSignal=SIGTERM
TimeoutStopSec=40
# Restart on crash/OOM/any non-`systemctl stop` exit, rate-limited above.
Restart=always
RestartSec=2
# Optional memory containment — uncomment + tune to host RAM:
# MemoryHigh=4G
# MemoryMax=6G

[Install]
WantedBy=default.target
endef
export CCTEAM_UNIT

daemon-install: install
	@mkdir -p $(SYSTEMD_DIR)
	@printf '%s\n' "$$CCTEAM_UNIT" > $(UNIT_FILE)
	@printf 'wrote: %s\n' '$(UNIT_FILE)'
	@loginctl enable-linger $(USER) >/dev/null 2>&1 || true
	systemctl --user daemon-reload
	systemctl --user enable --now $(UNIT_NAME)
	@printf '\033[32mccteam is now supervised.\033[0m  logs: make daemon-logs | status: make daemon-status\n'

daemon-uninstall:
	-systemctl --user disable --now $(UNIT_NAME)
	@rm -f $(UNIT_FILE)
	systemctl --user daemon-reload
	@printf 'removed: %s (state/config untouched)\n' '$(UNIT_FILE)'

daemon-start:
	systemctl --user start $(UNIT_NAME)

daemon-stop:
	systemctl --user stop $(UNIT_NAME)

# Rebuild release then restart — the symlinked binary makes the new build live.
daemon-restart: release
	systemctl --user restart $(UNIT_NAME)
	@printf 'restarted with fresh release build.\n'

daemon-status:
	@systemctl --user status $(UNIT_NAME) --no-pager || true

daemon-logs:
	journalctl --user -u $(UNIT_NAME) -f

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
