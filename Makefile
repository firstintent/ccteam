# ccteam Makefile — thin convenience wrappers around cargo / npm / the CLI.
#
#   make gate            # full pre-push gate: fmt + clippy + tests + SPA
#   make install         # THE install: build release + symlink to $(BIN_DIR)/ccteam
#                        #   + supervised service (systemd --user on Linux, launchd
#                        #   on macOS: starts at boot/login, restarts on crash) +
#                        #   first-run next steps
#   make start           # run the daemon in the FOREGROUND (dev / one-off)
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

# launchd supervision — macOS parity for the same section.
PLIST_LABEL  := com.firstintent.ccteam
PLIST_FILE   := $(HOME)/Library/LaunchAgents/$(PLIST_LABEL).plist
MAC_LOG      := $(CCTEAM_HOME)/daemon.log

.DEFAULT_GOAL := help
.PHONY: help build release clean fmt fmt-check clippy check test test-web \
        web web-deps web-check gate \
        install next-steps uninstall reinstall require-cli \
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
	@printf '  \033[1mmake install\033[0m       build release + symlink + service (systemd --user / launchd: boot + auto-restart)\n'
	@printf '  make uninstall     stop + remove the service and the symlink (state untouched)\n'
	@printf '  make reinstall     uninstall + install\n\n'
	@printf '\033[1mRun foreground (daemon = IM gateway + web UI + MCP, one process)\033[0m\n'
	@printf '  make config        ccteam config   (register MCP + IM creds + prefs)\n'
	@printf '  make init          ccteam init     (initialize the current dir as a project)\n'
	@printf '  make start         ccteam start    (foreground; web UI at %s)\n' '$(WEB_BIND)'
	@printf '  make status        ccteam status   (daemon health + sessions)\n'
	@printf '  make doctor        ccteam doctor --verify-mcp\n'
	@printf '  make stop          ccteam stop     (stop the daemon; sessions resume on next start)\n\n'
	@printf '\033[1mDaemon ops (service from `make install`; Linux systemd --user, macOS launchd)\033[0m\n'
	@printf '  make daemon-status     service status\n'
	@printf '  make daemon-logs       follow logs (journalctl / %s)\n' '$(MAC_LOG)'
	@printf '  make daemon-restart    rebuild release + restart the service\n'
	@printf '  make daemon-stop       stop (Linux keeps boot-start; macOS unloads until daemon-start)\n'
	@printf '  make daemon-start      start it again\n\n'
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
# `make install` is THE product install: release build → symlink → systemd
# --user service (unit generated below) → first-run next steps. Symlink (not
# copy): a fresh `cargo build --release` goes live on the next service
# restart. Without systemd (macOS / no user D-Bus) it falls back to the
# symlink + a foreground hint — never nohup.

install: release
	@mkdir -p $(BIN_DIR)
	@ln -sf $(RELEASE_BIN) $(BIN_LINK)
	@printf 'installed: %s -> %s\n' '$(BIN_LINK)' '$(RELEASE_BIN)'
	@case ":$$PATH:" in \
	    *:$(BIN_DIR):*) ;; \
	    *) printf '\033[33mwarning:\033[0m %s is not on PATH; add it to your shell rc.\n' '$(BIN_DIR)' ;; \
	esac
	@if [ "$$(uname -s)" = "Linux" ] && command -v systemctl >/dev/null 2>&1; then \
	    mkdir -p $(SYSTEMD_DIR); \
	    printf '%s\n' "$$CCTEAM_UNIT" > $(UNIT_FILE); \
	    printf 'service:   %s\n' '$(UNIT_FILE)'; \
	    loginctl enable-linger $(USER) >/dev/null 2>&1 || true; \
	    if systemctl --user daemon-reload >/dev/null 2>&1 \
	        && systemctl --user enable $(UNIT_NAME) >/dev/null 2>&1 \
	        && systemctl --user restart $(UNIT_NAME); then \
	        $(MAKE) --no-print-directory next-steps; \
	    else \
	        printf '\033[33mwarning:\033[0m could not start the systemd --user service (no login session / D-Bus?).\n'; \
	        printf '    Retry in a login session:  systemctl --user enable --now %s\n' '$(UNIT_NAME)'; \
	        printf '    Or run in the foreground:   %s start\n' '$(BIN_NAME)'; \
	    fi; \
	elif [ "$$(uname -s)" = "Darwin" ] && command -v launchctl >/dev/null 2>&1; then \
	    mkdir -p $(HOME)/Library/LaunchAgents $(CCTEAM_HOME); \
	    printf '%s\n' "$$CCTEAM_PLIST" > $(PLIST_FILE); \
	    printf 'service:   %s\n' '$(PLIST_FILE)'; \
	    launchctl bootout gui/$$(id -u)/$(PLIST_LABEL) >/dev/null 2>&1 || true; \
	    if launchctl bootstrap gui/$$(id -u) $(PLIST_FILE) >/dev/null 2>&1; then \
	        $(MAKE) --no-print-directory next-steps; \
	    else \
	        printf '\033[33mwarning:\033[0m could not bootstrap the launchd agent (SSH / no GUI session?).\n'; \
	        printf '    Retry after logging in:  launchctl bootstrap gui/$$(id -u) %s\n' '$(PLIST_FILE)'; \
	        printf '    Or run in the foreground:  %s start\n' '$(BIN_NAME)'; \
	    fi; \
	else \
	    printf 'no systemd --user / launchd here — run the daemon in the foreground:  %s start\n' '$(BIN_NAME)'; \
	fi

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
	printf '\n\033[32mccteam is up\033[0m — supervised service: starts at boot/login, restarts on crash.\n\n'; \
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
	@if [ -f $(UNIT_FILE) ]; then \
	    systemctl --user disable --now $(UNIT_NAME) >/dev/null 2>&1 || true; \
	    rm -f $(UNIT_FILE); \
	    systemctl --user daemon-reload >/dev/null 2>&1 || true; \
	    printf 'removed service: %s\n' '$(UNIT_FILE)'; \
	fi
	@if [ -f $(PLIST_FILE) ]; then \
	    launchctl bootout gui/$$(id -u)/$(PLIST_LABEL) >/dev/null 2>&1 || true; \
	    rm -f $(PLIST_FILE); \
	    printf 'removed service: %s\n' '$(PLIST_FILE)'; \
	fi
	@if [ -L $(BIN_LINK) ] || [ -f $(BIN_LINK) ]; then \
	    rm -f $(BIN_LINK) && printf 'removed: %s (state/config untouched)\n' '$(BIN_LINK)'; \
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

# --- Daemon supervision (Linux systemd --user · macOS launchd) ----------------
#
# Why: `ccteam start` is a FOREGROUND process tied to its terminal. Started by
# hand (e.g. `... >/tmp/ccteam.log 2>&1 &`) it dies on SIGHUP when the shell /
# SSH session closes — healthy one moment, gone the next, no crash trace. The
# service `make install` sets up fixes that: it owns the process, restarts it
# on crash/OOM, and keeps it running across logout (linger) and across reboot.
# The daemon traps SIGTERM into its graceful shutdown, so a service stop is a
# clean stop (sessions resume by sid on next start). Logs: `make daemon-logs`
# (journal on Linux; $(MAC_LOG) on macOS — launchd has no journal).
#
# Both units are generated below (no checked-in template). ExecStart + PATH
# honor $(BIN_DIR); the daemon spawns claude/codex from PATH, so $(BIN_DIR)
# MUST be on it. Tune the (commented) memory caps to your host if you want the
# daemon contained rather than relying on restart-after-OOM.

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
# User-local bins (claude/codex/grok) + OpenCode's default install prefix.
Environment=PATH=$(BIN_DIR):$(HOME)/.opencode/bin:/usr/local/bin:/usr/bin:/bin
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

# macOS LaunchAgent: RunAtLoad = start at login, KeepAlive = restart on any
# exit (so stopping goes through `launchctl bootout`, i.e. `make daemon-stop`).
define CCTEAM_PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>$(PLIST_LABEL)</string>
	<key>ProgramArguments</key>
	<array>
		<string>$(BIN_LINK)</string>
		<string>start</string>
		<string>--web-bind</string>
		<string>$(WEB_BIND)</string>
	</array>
	<key>WorkingDirectory</key><string>$(HOME)</string>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key><string>$(BIN_DIR):/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
	</dict>
	<key>RunAtLoad</key><true/>
	<key>KeepAlive</key><true/>
	<key>ProcessType</key><string>Background</string>
	<key>StandardOutPath</key><string>$(MAC_LOG)</string>
	<key>StandardErrorPath</key><string>$(MAC_LOG)</string>
</dict>
</plist>
endef
export CCTEAM_PLIST

daemon-start:
	@if [ "$$(uname -s)" = "Darwin" ]; then \
	    launchctl bootstrap gui/$$(id -u) $(PLIST_FILE) 2>/dev/null \
	        || launchctl kickstart gui/$$(id -u)/$(PLIST_LABEL); \
	else \
	    systemctl --user start $(UNIT_NAME); \
	fi

daemon-stop:
	@if [ "$$(uname -s)" = "Darwin" ]; then \
	    launchctl bootout gui/$$(id -u)/$(PLIST_LABEL); \
	else \
	    systemctl --user stop $(UNIT_NAME); \
	fi

# Rebuild release then restart — the symlinked binary makes the new build live.
daemon-restart: release
	@if [ "$$(uname -s)" = "Darwin" ]; then \
	    launchctl kickstart -k gui/$$(id -u)/$(PLIST_LABEL); \
	else \
	    systemctl --user restart $(UNIT_NAME); \
	fi
	@printf 'restarted with fresh release build.\n'

daemon-status:
	@if [ "$$(uname -s)" = "Darwin" ]; then \
	    { launchctl print gui/$$(id -u)/$(PLIST_LABEL) 2>/dev/null \
	        || printf 'not loaded: %s\n' '$(PLIST_LABEL)'; } | sed -n '1,14p'; \
	else \
	    systemctl --user status $(UNIT_NAME) --no-pager || true; \
	fi

daemon-logs:
	@if [ "$$(uname -s)" = "Darwin" ]; then \
	    tail -f $(MAC_LOG); \
	else \
	    journalctl --user -u $(UNIT_NAME) -f; \
	fi

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
