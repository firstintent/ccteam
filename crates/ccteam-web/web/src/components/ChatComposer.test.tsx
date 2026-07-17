// v0.8.19 W1 — locks the composer's Enter-to-send decision, especially the
// IME guard (the owner-reported #1 bug: pressing Enter to confirm a Chinese
// candidate must NOT send a half-typed message).

import { describe, expect, it } from "vitest";
import {
  attachmentsBlockSend,
  attachmentsPayload,
  shouldSubmitOnEnter,
  type ComposerAttachment,
} from "./ChatComposer";

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

// ── attachment pure helpers ────────────────────────────────────────────────────

const fileChip = (over: Partial<ComposerAttachment> = {}): ComposerAttachment => ({
  id: "att-1",
  kind: "file",
  name: "notes.txt",
  path: "/p/.ccteam/uploads/1-notes.txt",
  status: "ready",
  ...over,
});

describe("attachmentsPayload", () => {
  it("maps ready file/image chips to {kind, path, name}", () => {
    expect(attachmentsPayload([fileChip()])).toEqual([
      { kind: "file", path: "/p/.ccteam/uploads/1-notes.txt", name: "notes.txt" },
    ]);
  });

  it("maps a skill chip to {kind, name} — the server resolves the path", () => {
    expect(
      attachmentsPayload([
        fileChip({ id: "att-2", kind: "skill", name: "deep-research", path: undefined }),
      ]),
    ).toEqual([{ kind: "skill", name: "deep-research" }]);
  });

  it("drops chips that are still uploading or errored", () => {
    expect(
      attachmentsPayload([
        fileChip({ status: "uploading", path: undefined }),
        fileChip({ id: "att-3", status: "error", path: undefined }),
      ]),
    ).toEqual([]);
  });
});

describe("attachmentsBlockSend", () => {
  it("blocks while any chip uploads; clear once all settle", () => {
    expect(attachmentsBlockSend([fileChip({ status: "uploading" })])).toBe(true);
    expect(attachmentsBlockSend([fileChip(), fileChip({ id: "att-4", status: "error" })])).toBe(
      false,
    );
    expect(attachmentsBlockSend([])).toBe(false);
  });
});
