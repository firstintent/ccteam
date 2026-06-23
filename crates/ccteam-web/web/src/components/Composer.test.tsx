// v0.8.19 W1 — locks the composer's Enter-to-send decision, especially the
// IME guard (the owner-reported #1 bug: pressing Enter to confirm a Chinese
// candidate must NOT send a half-typed message).

import { describe, expect, it } from "vitest";
import { shouldSubmitOnEnter } from "./Composer";

const base = { key: "Enter", shiftKey: false, isComposing: false, keyCode: 13 };

describe("shouldSubmitOnEnter", () => {
  it("sends on a plain Enter for a finished line", () => {
    expect(shouldSubmitOnEnter(base)).toBe(true);
  });

  it("does NOT send while an IME candidate is composing (the #1 bug)", () => {
    expect(shouldSubmitOnEnter({ ...base, isComposing: true })).toBe(false);
  });

  it("does NOT send on the legacy keyCode 229 (IME in progress)", () => {
    expect(shouldSubmitOnEnter({ ...base, keyCode: 229 })).toBe(false);
  });

  it("does NOT send on Shift+Enter (newline)", () => {
    expect(shouldSubmitOnEnter({ ...base, shiftKey: true })).toBe(false);
  });

  it("ignores non-Enter keys", () => {
    expect(shouldSubmitOnEnter({ ...base, key: "a" })).toBe(false);
  });

  it("still sends Cmd/Ctrl+Enter (no shift, not composing)", () => {
    expect(shouldSubmitOnEnter({ ...base, key: "Enter" })).toBe(true);
  });
});
