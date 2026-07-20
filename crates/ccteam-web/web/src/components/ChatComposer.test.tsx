// v0.8.19 W1 — locks the composer's Enter-to-send decision, especially the
// IME guard (the owner-reported #1 bug: pressing Enter to confirm a Chinese
// candidate must NOT send a half-typed message).

import { describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import { renderToString } from "react-dom/server";
import {
  attachmentsBlockSend,
  attachmentsPayload,
  ChatComposer,
  shouldSubmitOnEnter,
  type ComposerAttachment,
} from "./ChatComposer";
import { defaultDraft } from "../lib/vendors";

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

// ── model button label (vendor always spelled out) ────────────────────────────

describe("model button label", () => {
  const renderBtn = (over: Partial<Parameters<typeof ChatComposer>[0]> = {}) =>
    renderToString(
      <ChatComposer
        draftKey="test"
        lang="zh"
        isAdmin
        draft={defaultDraft()}
        onDraftChange={() => {}}
        onSend={() => {}}
        {...over}
      />,
    );

  it("new-session draft: the vendor prefixes the default/picked model", () => {
    expect(renderBtn()).toContain("<span>claude · 默认</span>");
    expect(renderBtn({ draft: { ...defaultDraft(), model: "opus" } })).toContain(
      "<span>claude · opus</span>",
    );
    expect(
      renderBtn({
        draft: { ...defaultDraft(), vendor: "codex", model: "默认", protocol: "app-server" },
      }),
    ).toContain("<span>codex · 默认</span>");
  });

  it("conversation: the live model rides after the vendor; an unreported model leaves the vendor alone", () => {
    expect(
      renderBtn({
        modelLabel: "gpt-5.5",
        draft: { ...defaultDraft(), vendor: "codex", model: "", protocol: "app-server" },
      }),
    ).toContain("<span>codex · gpt-5.5</span>");
    const html = renderBtn({ modelLabel: "" });
    expect(html).toContain("<span>claude</span>");
    expect(html).not.toContain("<span>claude · ");
  });

  it("conversation: a live kimi session shows the same vendor · model · effort shape", () => {
    const html = renderBtn({
      modelLabel: "kimi-code/k3",
      draft: { ...defaultDraft(), vendor: "kimi", model: "", protocol: "acp" },
    });
    expect(html).toContain("<span>Kimi · kimi-code/k3</span>");
    expect(html).toContain('<span class="eff">默认</span>');
  });
});

// ── prefill (Home 快速开始 templates) ──────────────────────────────────────────

describe("prefill", () => {
  const renderComposer = (prefill?: { text: string; nonce: number }) =>
    renderToString(
      <ChatComposer
        draftKey="test"
        lang="zh"
        isAdmin
        draft={defaultDraft()}
        onDraftChange={() => {}}
        onSend={() => {}}
        prefill={prefill}
      />,
    );

  it("a bumped nonce replaces the draft text", () => {
    expect(renderComposer({ text: "帮我修一个 bug。", nonce: 1 })).toContain("帮我修一个 bug。");
  });

  it("nonce 0 means no prefill (draft untouched)", () => {
    expect(renderComposer({ text: "不该出现", nonce: 0 })).not.toContain("不该出现");
  });
});
