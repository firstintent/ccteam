// v0.9.11 TEAM-2 — charter editor state machine (pure reducer) tests.

import { describe, expect, it } from "vitest";

import { charterReducer, initialCharter, type CharterState } from "./charterState";
import type { RoutingDoc } from "./routingApi";

function doc(over: Partial<RoutingDoc> = {}): RoutingDoc {
  return {
    exists: true,
    source: "project",
    path: "/srv/demo/.ccteam/routing.md",
    fallback_path: null,
    content: "# charter\n",
    sha256: "abc",
    updated_at: "2026-07-29T00:00:00+00:00",
    ...over,
  };
}

describe("charterReducer", () => {
  it("a project-owned doc opens straight into a clean editable draft", () => {
    const s = charterReducer(initialCharter, { kind: "loaded", doc: doc() });
    expect(s.loading).toBe(false);
    expect(s.draft).toBe("# charter\n");
    expect(s.baseline).toBe("# charter\n");
    expect(s.dirty).toBe(false);
  });

  it("a global fallback doc stays read-only until 拷入起稿 seeds a dirty draft", () => {
    const global = doc({ source: "global", fallback_path: "/home/u/.ccteam/routing.md" });
    let s = charterReducer(initialCharter, { kind: "loaded", doc: global });
    expect(s.draft).toBeNull(); // read-only view, no editor yet
    expect(s.dirty).toBe(false);

    s = charterReducer(s, { kind: "start-draft", from: "copy" });
    expect(s.draft).toBe("# charter\n"); // copied from the global content
    expect(s.dirty).toBe(true); // the project file doesn't exist → unsaved

    // 空白起稿 from the same doc starts empty, still dirty.
    const blank = charterReducer(
      charterReducer(initialCharter, { kind: "loaded", doc: global }),
      { kind: "start-draft", from: "blank" },
    );
    expect(blank.draft).toBe("");
    expect(blank.dirty).toBe(true);
  });

  it("source none allows only a blank draft (no content to copy)", () => {
    const none = doc({ source: "none", exists: false, content: "", sha256: null, updated_at: null });
    let s = charterReducer(initialCharter, { kind: "loaded", doc: none });
    expect(s.draft).toBeNull();
    s = charterReducer(s, { kind: "start-draft", from: "blank" });
    expect(s.draft).toBe("");
    expect(s.dirty).toBe(true);
  });

  it("edit toggles dirty against the last-saved baseline", () => {
    let s = charterReducer(initialCharter, { kind: "loaded", doc: doc() });
    s = charterReducer(s, { kind: "edit", content: "# charter\nv2" });
    expect(s.dirty).toBe(true);
    s = charterReducer(s, { kind: "edit", content: "# charter\n" });
    expect(s.dirty).toBe(false); // back to the baseline → clean again
  });

  it("saved flips the doc to source=project and re-baselines the draft", () => {
    const global = doc({ source: "global", fallback_path: "/home/u/.ccteam/routing.md" });
    let s = charterReducer(initialCharter, { kind: "loaded", doc: global });
    s = charterReducer(s, { kind: "start-draft", from: "copy" });
    s = charterReducer(s, { kind: "edit", content: "# mine\n" });
    s = charterReducer(s, { kind: "save-begin" });
    expect(s.saving).toBe(true);
    s = charterReducer(s, {
      kind: "saved",
      result: { sha256: "deadbeef", updated_at: "2026-07-29T01:00:00+00:00" },
    });
    expect(s.saving).toBe(false);
    expect(s.dirty).toBe(false);
    expect(s.saved?.sha256).toBe("deadbeef");
    expect(s.doc?.source).toBe("project");
    expect(s.doc?.fallback_path).toBeNull();
    expect(s.doc?.content).toBe("# mine\n");
    // A follow-up edit is dirty against the NEW baseline.
    expect(charterReducer(s, { kind: "edit", content: "# mine\nv2" }).dirty).toBe(true);
  });

  it("save-failed keeps the dirty draft and surfaces the error", () => {
    let s: CharterState = charterReducer(initialCharter, { kind: "loaded", doc: doc() });
    s = charterReducer(s, { kind: "edit", content: "v2" });
    s = charterReducer(s, { kind: "save-begin" });
    s = charterReducer(s, { kind: "save-failed", error: "HTTP 413" });
    expect(s.saving).toBe(false);
    expect(s.dirty).toBe(true);
    expect(s.draft).toBe("v2");
    expect(s.error).toBe("HTTP 413");
  });

  it("reset returns to loading (project switch)", () => {
    const s = charterReducer(
      charterReducer(initialCharter, { kind: "loaded", doc: doc() }),
      { kind: "reset" },
    );
    expect(s).toEqual(initialCharter);
  });
});
