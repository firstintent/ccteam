// FIX-2 — web new-session defaults, kept in a dependency-free module so they
// are the single source of truth AND importable from unit tests without
// pulling in the React/`window`-touching ChatConsole import chain.

// The product's default role: the chat-first manager `cto`, seeded by
// `ccteam init`. Picking an undefined agent (the old `assistant` default)
// spawned a dead pane that never replied.
export const DEFAULT_ROLE = "cto";

// Role autocomplete suggestions; `cto` leads so the default is the first hint.
export const ROLE_SUGGESTIONS = [DEFAULT_ROLE, "reviewer", "api", "ui", "qa", "docs"];

// v0.8.8 F2-web — the sentinel `<select>` value for the explicit "no role /
// bare claude" choice. A real role name can never collide (roles are
// `[a-z0-9_-]+`, never starting with `_`), so this is unambiguous.
export const ROLELESS = "__none";

/** Resolve the new-session `<select>` value to the wire `role` string.
 *
 *  Three cases (FIX-2 + F2-web):
 *   - the explicit roleless sentinel → "" (passthrough; the backend now
 *     accepts an empty role as a bare-claude session — do NOT fall back to
 *     the default, that would silently re-add `cto`).
 *   - a concrete role → that role (trimmed).
 *   - nothing picked / blank → {@link DEFAULT_ROLE} (`cto`), so an
 *     un-touched modal still spawns the chat-first manager, not a dead pane.
 *
 *  Pure + dependency-free so the sentinel semantics are unit-testable. */
export function resolveRole(selected: string, sentinel: string = ROLELESS): string {
  if (selected === sentinel) return "";
  return selected.trim() || DEFAULT_ROLE;
}
