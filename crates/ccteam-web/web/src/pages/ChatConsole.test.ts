import { describe, expect, it } from "vitest";

import { DEFAULT_ROLE, ROLE_SUGGESTIONS } from "./chatDefaults";

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
