// FIX-2 — web new-session defaults, kept in a dependency-free module so they
// are the single source of truth AND importable from unit tests without
// pulling in the React/`window`-touching ChatConsole import chain.

// v0.9.0 (engine neutralization) — the product's default is ROLELESS: a bare
// vendor session that reads the project's own `CLAUDE.md` / `AGENTS.md` as its
// brain. ccteam no longer seeds a `cto` role, so the default wire `role` is the
// empty string (an un-touched modal spawns a roleless session, not `cto`).
export const DEFAULT_ROLE = "";

// Role autocomplete suggestions — named work-roles the user may pick. Roleless
// is the *default* (the empty string above), not a suggestion, so it does NOT
// lead this list.
export const ROLE_SUGGESTIONS = ["reviewer", "api", "ui", "qa", "docs"];

// v0.8.8 F2-web — the sentinel `<select>` value for the explicit "no role /
// bare claude" choice. A real role name can never collide (roles are
// `[a-z0-9_-]+`, never starting with `_`), so this is unambiguous.
export const ROLELESS = "__none";

/** Resolve the new-session `<select>` value to the wire `role` string.
 *
 *  Three cases (FIX-2 + F2-web; v0.9.0 roleless default):
 *   - the explicit roleless sentinel → "" (passthrough; a bare-claude session).
 *   - a concrete role → that role (trimmed).
 *   - nothing picked / blank → {@link DEFAULT_ROLE} (now "" = roleless), so an
 *     un-touched modal spawns a bare vendor that reads the project
 *     CLAUDE.md/AGENTS.md — no `cto` is silently re-added.
 *
 *  Pure + dependency-free so the sentinel semantics are unit-testable. */
export function resolveRole(selected: string, sentinel: string = ROLELESS): string {
  if (selected === sentinel) return "";
  return selected.trim() || DEFAULT_ROLE;
}
