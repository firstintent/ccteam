// The rename edit decision — the one rule both rename surfaces (rail rows +
// conversation header) obey. Kept pure so it is testable without a DOM
// harness; the component around it is a thin input.

import { describe, expect, it } from "vitest";

import { resolveRenameInput } from "./InlineRename";

describe("resolveRenameInput", () => {
  it("submits a changed title, trimmed", () => {
    expect(resolveRenameInput("  ship the rename  ", "old")).toEqual({
      action: "submit",
      title: "ship the rename",
    });
  });

  it("cancels on a blank input — a rename must never CLEAR a title", () => {
    expect(resolveRenameInput("", "old")).toEqual({ action: "cancel" });
    expect(resolveRenameInput("   \t ", "old")).toEqual({ action: "cancel" });
  });

  it("cancels when nothing actually changed (no request, no toast)", () => {
    expect(resolveRenameInput("old", "old")).toEqual({ action: "cancel" });
    expect(resolveRenameInput("  old  ", "old")).toEqual({ action: "cancel" });
  });

  it("treats a first title (previously empty) as a change", () => {
    expect(resolveRenameInput("first name", "")).toEqual({
      action: "submit",
      title: "first name",
    });
  });
});
