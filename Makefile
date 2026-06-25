# ccteam Makefile — thin convenience wrappers around cargo / npm / the CLI.
#
#   make gate            # full pre-push gate: fmt + clippy + tests + SPA
#   make install         # build release + symlink to $(BIN_DIR)/ccteam
#   make start           # run the daemon (IM gateway + web UI + MCP)
#   make wipe            # reset runtime state (keeps secrets + config)
#
# Override locations:  make BIN_DIR=/usr/local/bin install
#                       make CCTEAM_HOME=~/.ccteam2 wipe

# --- Configuration -----------------------------------------------------------

BIN_DIR      ?= $(HOME)/.local/bin
BIN_NAME     := ccteam
BIN_LINK     := $(BIN_DIR)/$(BIN_NAME)
RELEASE_BIN  := $(CURDIR)/target/release/$(BIN_NAME)
CCTEAM_HOME  ?= $(HOME)/.ccteam
WEB_DIR      := $(CURDIR)/crates/ccteam-web/web
WEB_PORT     ?= 7331

# Runtime dirs reset by `make wipe` (W7 layout: ~/.ccteam/{hooks,run,state,
# secrets,cache}). Kept: secrets/ (web-token, IM creds, per-user files),
# config.yaml, hooks/ — so creds + prefs survive a reset.
WIPE_DIRS    := state run cache

.DEFAULT_GOAL := help
.PHONY: help build release clean fmt fmt-check clippy check test test-web \
        web web-deps web-check gate \
        install uninstall reinstall \
        config init start stop status doctor \
        wipe nuke

# --- Help --------------------------------------------------------------------

help:
	@printf '\033[1mccteam Makefile\033[0m\n\n'
	@printf '\033[1mBuild & test\033[0m\n'
	@printf '  make build        cargo build (debug)\n'
	@printf '  make release      cargo build --release\n'
	@printf '  make fmt          cargo fmt --all\n'
	@printf '  make clippy       cargo clippy --workspace --all-targets -D warnings\n'
	@printf '  make test         cargo test (workspace, excl ccteam-web)\n'
	@printf '  make test-web     cargo test -p ccteam-web\n'
	@printf '  make web          build the SPA (tsc + vite)\n'
	@printf '  make web-check    SPA eslint + vitest\n'
	@printf '  \033[1mmake gate\033[0m         full pre-push gate (fmt+clippy+test+test-web+web-check)\n'
	@printf '  make clean        cargo clean + rm SPA dist\n\n'
	@printf '\033[1mInstall\033[0m\n'
	@printf '  make install      symlink %s -> %s\n' '$(RELEASE_BIN)' '$(BIN_LINK)'
	@printf '  make uninstall    remove the symlink (state untouched)\n'
	@printf '  make reinstall    uninstall + install\n\n'
	@printf '\033[1mRun (daemon = IM gateway + web UI + MCP, one process)\033[0m\n'
	@printf '  make config       ccteam config   (register MCP + IM creds + prefs)\n'
	@printf '  make init         ccteam init     (initialize the current dir as a project)\n'
	@printf '  make start        ccteam start    (web UI at http://localhost:%s)\n' '$(WEB_PORT)'
	@printf '  make status       ccteam status   (daemon health + sessions)\n'
	@printf '  make doctor       ccteam doctor --verify-mcp\n'
	@printf '  make stop         ccteam stop     (stop the daemon; sessions resume on next start)\n\n'
	@printf '\033[1mState reset (destructive)\033[0m\n'
	@printf '  make wipe         rm %s/{%s} (keeps secrets/, config.yaml, hooks/)\n' '$(CCTEAM_HOME)' 'state,run,cache'
	@printf '  make nuke         rm -rf %s   (requires CONFIRM=1)\n' '$(CCTEAM_HOME)'

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

check: fmt-check clippy
	cargo check --workspace --all-targets

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

# --- Run ---------------------------------------------------------------------
#
# `ccteam start` runs ONE process: the IM gateway + the embedded web UI
# (default bind 0.0.0.0:$(WEB_PORT)) + the MCP socket. Foreground is the only
# mode. Sessions are spawned on demand and resume by sid across restarts.

config:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) config

init:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) init

start:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	@printf 'web UI: http://localhost:%s  (token printed below if auth is on)\n' '$(WEB_PORT)'
	$(BIN_NAME) start

status:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) status

doctor:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) doctor --verify-mcp

stop:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) stop

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
