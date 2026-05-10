# ccteam Makefile — manual-verification convenience.
#
# Targets are deliberately thin wrappers around `cargo` / shell. The point
# is to make repeated "rebuild → reinstall → wipe state → run a project →
# observe → wipe" loops cheap. Anything that touches user state is in a
# named target, never on a default code path.
#
# Quick start:
#   make help            # list everything
#   make install         # build release + symlink to $(BIN_DIR)/ccteam
#   make doctor          # health-check (binary must be installed)
#   make wipe            # safe reset: kills tmux sessions, removes our 8
#                        # agent symlinks, clears runtime state dirs.
#                        # Keeps ~/.ccteam/phases & ~/.ccteam/memory.
#   make uninstall       # remove the symlink (does not touch state).
#
# Override the install location with `make BIN_DIR=/usr/local/bin install`.

# --- Configuration -----------------------------------------------------------

BIN_DIR        ?= $(HOME)/.local/bin
BIN_NAME       := ccteam
BIN_LINK       := $(BIN_DIR)/$(BIN_NAME)
RELEASE_BIN    := $(CURDIR)/target/release/$(BIN_NAME)

CCTEAM_HOME    ?= $(HOME)/.ccteam
PROJECTS_ROOT  ?= $(HOME)/projects
CLAUDE_AGENTS  := $(HOME)/.claude/agents

# The eight plugin-agent files `bootstrap_project` symlinks into
# ~/.claude/agents/. Kept in sync with crates/ccteam-core/src/tool_surface.rs
# RECOMMENDED_AGENTS. `make agents-clean` only touches these names so user-
# authored agents in the same dir survive.
PLUGIN_AGENTS  := \
    code-reviewer.md \
    silent-failure-hunter.md \
    pr-test-analyzer.md \
    type-design-analyzer.md \
    comment-analyzer.md \
    code-architect.md \
    code-explorer.md \
    code-simplifier.md

# Runtime dirs under $(CCTEAM_HOME) that `make state-clean` resets. Phases
# and memory are intentionally preserved (phases are part of install, memory
# is the cross-project RAG store).
RUNTIME_DIRS   := state progress inbox queue control log

.DEFAULT_GOAL := help
.PHONY: help build release clean check fmt clippy test \
        install uninstall reinstall \
        setup start attach \
        doctor tool-surface \
        tmux-clean agents-clean state-clean wipe nuke

# --- Help --------------------------------------------------------------------

help:
	@printf '\033[1mccteam Makefile\033[0m — manual-verification helpers\n\n'
	@printf '\033[1mBuild & test\033[0m\n'
	@printf '  make build         cargo build (debug)\n'
	@printf '  make release       cargo build --release\n'
	@printf '  make test          cargo test (full suite)\n'
	@printf '  make check         cargo check + fmt --check + clippy\n'
	@printf '  make fmt           cargo fmt\n'
	@printf '  make clippy        cargo clippy -- -D warnings\n'
	@printf '  make clean         cargo clean\n\n'
	@printf '\033[1mInstall\033[0m\n'
	@printf '  make install       symlink %s → %s\n' '$(RELEASE_BIN)' '$(BIN_LINK)'
	@printf '  make uninstall     remove %s (state untouched)\n' '$(BIN_LINK)'
	@printf '  make reinstall     uninstall + install\n\n'
	@printf '\033[1mFirst-time setup & run (end-to-end)\033[0m\n'
	@printf '  make setup HANDLE=<h>   one-shot: init + 4 doctor installs + tool-surface\n'
	@printf '  make start              ccteam start --foreground (Terminal A)\n'
	@printf '  make attach HANDLE=<h>  tmux attach -t ccteam-meta-<h> (Terminal B)\n\n'
	@printf '\033[1mHealth check\033[0m\n'
	@printf '  make doctor        ccteam doctor --tool-surface\n'
	@printf '  make tool-surface  alias for doctor\n\n'
	@printf '\033[1mState reset (destructive — read help text)\033[0m\n'
	@printf '  make tmux-clean    kill all ccteam-* tmux sessions\n'
	@printf '  make agents-clean  remove the 8 plugin agent symlinks we installed\n'
	@printf '                     (user-authored agents in %s preserved)\n' '$(CLAUDE_AGENTS)'
	@printf '  make state-clean   wipe runtime dirs under %s\n' '$(CCTEAM_HOME)'
	@printf '                     (kept: phases, memory)\n'
	@printf '  make wipe          tmux-clean + agents-clean + state-clean\n'
	@printf '  make nuke          rm -rf %s + rm -rf %s' '$(CCTEAM_HOME)' '$(PROJECTS_ROOT)'
	@printf ' (requires CONFIRM=1)\n\n'
	@printf '\033[1mOverrides\033[0m\n'
	@printf '  BIN_DIR        install location (default $(BIN_DIR))\n'
	@printf '  CCTEAM_HOME    ccteam state root (default $(CCTEAM_HOME))\n'
	@printf '  PROJECTS_ROOT  projects root      (default $(PROJECTS_ROOT))\n'

# --- Build & test ------------------------------------------------------------

build:
	cargo build

release:
	cargo build --release

clean:
	cargo clean

check: fmt-check clippy
	cargo check --workspace --all-targets

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

# --- Install / uninstall -----------------------------------------------------
#
# We install via symlink, not copy: rebuild → newer binary auto-picked up,
# no need to `make install` after every `cargo build --release`. Use a
# copy if you prefer pinned-binary semantics:
#   install -m 0755 $(RELEASE_BIN) $(BIN_LINK)

install: release
	@mkdir -p $(BIN_DIR)
	@ln -sf $(RELEASE_BIN) $(BIN_LINK)
	@printf 'installed: %s → %s\n' '$(BIN_LINK)' '$(RELEASE_BIN)'
	@printf 'verify with: ccteam --version\n'
	@case ":$$PATH:" in \
	    *:$(BIN_DIR):*) ;; \
	    *) printf '\033[33mwarning:\033[0m %s is not on PATH; add it to your shell rc.\n' '$(BIN_DIR)' ;; \
	esac

uninstall:
	@if [ -L $(BIN_LINK) ] || [ -f $(BIN_LINK) ]; then \
	    rm -f $(BIN_LINK); \
	    printf 'removed: %s\n' '$(BIN_LINK)'; \
	else \
	    printf 'not installed: %s\n' '$(BIN_LINK)'; \
	fi

reinstall: uninstall install

# --- First-time setup & run --------------------------------------------------
#
# `make setup HANDLE=rob` bundles the six idempotent first-time steps so users
# don't have to memorize / paste-and-edit the doctor command list. `make start`
# and `make attach` wrap the canonical Quick start two-terminal flow.

HANDLE ?=

setup:
	@if [ -z "$(HANDLE)" ]; then \
	    printf '\033[31merror:\033[0m HANDLE is required. Example: make setup HANDLE=rob\n' >&2; \
	    exit 1; \
	fi
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install` first\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) init
	$(BIN_NAME) doctor --install-skill
	$(BIN_NAME) doctor --install-mcp
	$(BIN_NAME) doctor --install-memory-bridge
	$(BIN_NAME) doctor --install-meta-agent $(HANDLE)
	$(BIN_NAME) doctor --tool-surface
	@printf '\n\033[32mready.\033[0m\n'
	@printf 'next:\n'
	@printf '  Terminal A: make start                 (or: ccteam start --foreground)\n'
	@printf '  Terminal B: make attach HANDLE=%s     (or: tmux attach -t ccteam-meta-%s)\n' '$(HANDLE)' '$(HANDLE)'

start:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) start --foreground

attach:
	@if [ -z "$(HANDLE)" ]; then \
	    printf '\033[31merror:\033[0m HANDLE is required. Example: make attach HANDLE=rob\n' >&2; \
	    exit 1; \
	fi
	@command -v tmux >/dev/null || { printf '\033[31merror:\033[0m tmux not installed\n' >&2; exit 1; }
	tmux attach -t ccteam-meta-$(HANDLE)

# --- Health check ------------------------------------------------------------

doctor:
	@command -v $(BIN_NAME) >/dev/null || { printf '\033[31merror:\033[0m %s not on PATH; run `make install`\n' '$(BIN_NAME)' >&2; exit 1; }
	$(BIN_NAME) doctor --tool-surface

tool-surface: doctor

# --- State reset (destructive) -----------------------------------------------

tmux-clean:
	@if ! command -v tmux >/dev/null; then \
	    printf 'tmux not installed; nothing to clean\n'; exit 0; \
	fi; \
	sessions=$$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^ccteam-' || true); \
	if [ -z "$$sessions" ]; then \
	    printf 'no ccteam-* tmux sessions\n'; \
	else \
	    for s in $$sessions; do \
	        tmux kill-session -t "$$s" && printf 'killed: %s\n' "$$s"; \
	    done; \
	fi

agents-clean:
	@for f in $(PLUGIN_AGENTS); do \
	    target="$(CLAUDE_AGENTS)/$$f"; \
	    if [ -L "$$target" ]; then \
	        rm -f "$$target" && printf 'unlinked: %s\n' "$$target"; \
	    elif [ -f "$$target" ]; then \
	        printf '\033[33mskip\033[0m (regular file, user-authored?): %s\n' "$$target"; \
	    fi; \
	done

state-clean:
	@if [ ! -d "$(CCTEAM_HOME)" ]; then \
	    printf 'no $(CCTEAM_HOME); nothing to clean\n'; exit 0; \
	fi; \
	for d in $(RUNTIME_DIRS); do \
	    full="$(CCTEAM_HOME)/$$d"; \
	    if [ -d "$$full" ]; then \
	        rm -rf "$$full" && printf 'wiped: %s\n' "$$full"; \
	    fi; \
	done; \
	printf 'kept: %s/phases  %s/memory\n' '$(CCTEAM_HOME)' '$(CCTEAM_HOME)'

wipe: tmux-clean agents-clean state-clean
	@printf '\nwipe complete. binary still installed; run `make doctor` to verify.\n'

nuke:
	@if [ "$(CONFIRM)" != "1" ]; then \
	    printf '\033[31mrefusing to nuke without CONFIRM=1\033[0m\n'; \
	    printf 'this would `rm -rf %s` and `rm -rf %s`.\n' '$(CCTEAM_HOME)' '$(PROJECTS_ROOT)'; \
	    printf 'rerun:  make nuke CONFIRM=1\n'; \
	    exit 1; \
	fi
	$(MAKE) tmux-clean
	$(MAKE) agents-clean
	@if [ -d "$(CCTEAM_HOME)" ]; then \
	    rm -rf "$(CCTEAM_HOME)" && printf 'rmrf: %s\n' '$(CCTEAM_HOME)'; \
	fi
	@if [ -d "$(PROJECTS_ROOT)" ]; then \
	    rm -rf "$(PROJECTS_ROOT)" && printf 'rmrf: %s\n' '$(PROJECTS_ROOT)'; \
	fi
	@printf '\nnuke complete. binary still installed; run `make doctor` to verify.\n'
