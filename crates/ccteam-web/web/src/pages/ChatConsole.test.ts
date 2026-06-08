import { describe, expect, it } from "vitest";

import { DEFAULT_ROLE, ROLE_SUGGESTIONS, ROLELESS, resolveRole } from "./chatDefaults";

// FIX-2 — the web new-session default role must be the product's chat-first
// manager `cto` (seeded by `ccteam init`), NOT the old `assistant` which named
// an undefined agent and spawned a dead pane that never replied.
describe("ChatConsole defaults", () => {
  it("defaults new sessions to cto, not assistant", () => {
    expect(DEFAULT_ROLE).toBe("cto");
  });

  it("leads role suggestions with the default (cto), no assistant", () => {
    expect(ROLE_SUGGESTIONS[0]).toBe(DEFAULT_ROLE);
    expect(ROLE_SUGGESTIONS).not.toContain("assistant");
  });
});

// v0.8.8 F2-web — resolveRole maps the new-session <select> value to the wire
// role. The sentinel ROLELESS → "" (bare claude, NO fallback); a concrete pick
// → that role; nothing picked / blank → DEFAULT_ROLE (cto). The last clause
// keeps FIX-2: an un-touched modal must NOT silently become roleless.
describe("resolveRole (F2-web sentinel)", () => {
  it("maps the roleless sentinel to an empty string (does NOT fall back to cto)", () => {
    expect(resolveRole(ROLELESS)).toBe("");
    // The sentinel itself must never leak as a wire role.
    expect(resolveRole(ROLELESS)).not.toBe(ROLELESS);
    expect(resolveRole(ROLELESS)).not.toBe(DEFAULT_ROLE);
  });

  it("passes a concrete role through verbatim (trimmed)", () => {
    expect(resolveRole("reviewer")).toBe("reviewer");
    expect(resolveRole("  api  ")).toBe("api");
  });

  it("falls back to DEFAULT_ROLE (cto) when nothing is picked / blank", () => {
    expect(resolveRole("")).toBe(DEFAULT_ROLE);
    expect(resolveRole("   ")).toBe(DEFAULT_ROLE);
  });
});
