// v0.8.8 bug — mergeProjectSlugs is the union fixing the chicken-and-egg bug
// (a registered project with NO session must still be listed). Moved out of
// the retired NewSessionModal test file.

import { describe, expect, it } from "vitest";

import { mergeProjectSlugs } from "./projectList";
import type { SessionView } from "../lib/sessionsApi";

describe("mergeProjectSlugs", () => {
  it("lists a registered project even with NO sessions (the bug)", () => {
    expect(mergeProjectSlugs(["demo2"], [])).toEqual(["demo2"]);
  });

  it("unions registered projects with session projects, sorted + de-duped", () => {
    const sessions: Pick<SessionView, "project">[] = [
      { project: "alpha" },
      { project: "zeta" },
      { project: "alpha" },
    ];
    expect(mergeProjectSlugs(["alpha", "demo2"], sessions)).toEqual(["alpha", "demo2", "zeta"]);
  });

  it("returns [] when nothing is registered and there are no sessions", () => {
    expect(mergeProjectSlugs([], [])).toEqual([]);
  });

  it("falls back to session projects when the registered list is empty", () => {
    expect(mergeProjectSlugs([], [{ project: "live-only" }])).toEqual(["live-only"]);
  });
});
