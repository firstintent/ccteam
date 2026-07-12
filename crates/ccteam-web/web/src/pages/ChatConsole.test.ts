import { describe, expect, it } from "vitest";

import { DEFAULT_ROLE, ROLE_SUGGESTIONS, ROLELESS, resolveRole } from "./chatDefaults";

// v0.9.0 (engine neutralization) — the web new-session default is ROLELESS: an
// empty wire `role` spawns a bare vendor that reads the project's own
// CLAUDE.md/AGENTS.md. ccteam seeds no `cto` role, so DEFAULT_ROLE is "".
describe("ChatConsole defaults", () => {
  it("defaults new sessions to roleless (empty role), not a seeded cto", () => {
    expect(DEFAULT_ROLE).toBe("");
  });

  it("suggests named work-roles, never the roleless default or assistant", () => {
    // Suggestions are concrete work-roles; roleless ("") is the default, not a hint.
    expect(ROLE_SUGGESTIONS[0]).toBe("reviewer");
    expect(ROLE_SUGGESTIONS).not.toContain("");
    expect(ROLE_SUGGESTIONS).not.toContain("assistant");
  });
});

// v0.8.8 F2-web — resolveRole maps the new-session <select> value to the wire
// role. The sentinel ROLELESS → "" (bare claude); a concrete pick → that role;
// nothing picked / blank → DEFAULT_ROLE (v0.9.0: now "" = roleless).
describe("resolveRole (F2-web sentinel)", () => {
  it("maps the roleless sentinel to an empty string", () => {
    expect(resolveRole(ROLELESS)).toBe("");
    // The sentinel itself must never leak as a wire role.
    expect(resolveRole(ROLELESS)).not.toBe(ROLELESS);
  });

  it("passes a concrete role through verbatim (trimmed)", () => {
    expect(resolveRole("reviewer")).toBe("reviewer");
    expect(resolveRole("  api  ")).toBe("api");
  });

  it("resolves a blank / whitespace-only selection to the roleless default (empty)", () => {
    expect(resolveRole("")).toBe(DEFAULT_ROLE);
    expect(resolveRole("   ")).toBe(DEFAULT_ROLE);
    // DEFAULT_ROLE is now "", so blank resolves to roleless — no `cto` fallback.
    expect(resolveRole("")).toBe("");
  });
});
