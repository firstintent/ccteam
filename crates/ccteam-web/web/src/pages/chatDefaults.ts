// FIX-2 — web new-session defaults, kept in a dependency-free module so they
// are the single source of truth AND importable from unit tests without
// pulling in the React/`window`-touching ChatConsole import chain.

// The product's default role: the chat-first manager `cto`, seeded by
// `ccteam init`. Picking an undefined agent (the old `assistant` default)
// spawned a dead pane that never replied.
export const DEFAULT_ROLE = "cto";

// Role autocomplete suggestions; `cto` leads so the default is the first hint.
export const ROLE_SUGGESTIONS = [DEFAULT_ROLE, "reviewer", "api", "ui", "qa", "docs"];
