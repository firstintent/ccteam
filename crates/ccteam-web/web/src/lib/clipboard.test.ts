import { afterEach, describe, expect, it, vi } from "vitest";
import { copyText, legacyCopy } from "./clipboard";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("copyText", () => {
  it("uses the async Clipboard API when available", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await expect(copyText("hello")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello");
  });

  it("falls back to execCommand when writeText rejects (non-secure quirk)", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("NotAllowedError"));
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const ta = { value: "", setAttribute: vi.fn(), style: {}, select: vi.fn(), setSelectionRange: vi.fn() };
    vi.stubGlobal("document", {
      createElement: vi.fn().mockReturnValue(ta),
      execCommand: vi.fn().mockReturnValue(true),
      body: { appendChild: vi.fn(), removeChild: vi.fn() },
    });
    await expect(copyText("hello")).resolves.toBe(true);
    expect(ta.value).toBe("hello");
  });

  it("resolves false when clipboard is undefined and no DOM exists (http:// daemon)", async () => {
    // Non-secure context: `navigator.clipboard` is undefined — the exact
    // shape that made the join-card copy button a silent no-op.
    vi.stubGlobal("navigator", {});
    await expect(copyText("hello")).resolves.toBe(false);
  });
});

describe("legacyCopy", () => {
  it("returns false without a document (never throws)", () => {
    expect(legacyCopy("x")).toBe(false);
  });
});
