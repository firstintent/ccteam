// @vitest-environment jsdom
// #237 — an unsent message stays in the composer (and its localStorage
// draft) until the send handler confirms the server has it.
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatComposer, remainderAfterSend, type SendOutcome } from "./ChatComposer";
import { defaultDraft } from "../lib/vendors";

const DRAFT = "写了很久的提示词:review the SDK";
const KEY = "ccteam.draft.v1.t237";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

async function mount(onSend: (text: string) => SendOutcome) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}")));
  localStorage.setItem(KEY, DRAFT);
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(() =>
    root.render(
      <ChatComposer
        draftKey="t237"
        lang="zh"
        draft={defaultDraft()}
        onDraftChange={() => {}}
        catalog={{}}
        onSend={onSend}
      />,
    ),
  );
  const textarea = container.querySelector<HTMLTextAreaElement>("[data-testid=composer-textarea]")!;
  const button = container.querySelector<HTMLButtonElement>("[data-testid=composer-send]")!;
  const enter = () =>
    act(() => {
      textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
  const unmount = async () => {
    await act(() => root.unmount());
    container.remove();
  };
  return { textarea, button, enter, unmount };
}

function deferred() {
  let resolve!: (value: boolean | void) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<boolean | void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("composer draft survives a failed send (#237)", () => {
  it.each([
    ["rejects", (d: ReturnType<typeof deferred>) => d.reject(new Error("HTTP 500"))],
    ["resolves false", (d: ReturnType<typeof deferred>) => d.resolve(false)],
  ])("keeps text and stored draft when the send %s", async (_label, fail) => {
    const pending = deferred();
    const onSend = vi.fn(() => pending.promise);
    const { textarea, button, enter, unmount } = await mount(onSend);
    expect(textarea.value).toBe(DRAFT);

    await enter();
    expect(onSend).toHaveBeenCalledWith(DRAFT, []);
    // In flight: still there, and a second send is refused.
    expect(textarea.value).toBe(DRAFT);
    expect(button.disabled).toBe(true);
    await enter();
    expect(onSend).toHaveBeenCalledTimes(1);

    await act(async () => {
      fail(pending);
      await pending.promise.catch(() => {});
    });
    expect(textarea.value).toBe(DRAFT);
    expect(localStorage.getItem(KEY)).toBe(DRAFT);
    expect(button.disabled).toBe(false);
    await unmount();
  });

  it("clears text and stored draft once the send is confirmed", async () => {
    const pending = deferred();
    const { textarea, enter, unmount } = await mount(() => pending.promise);
    await enter();
    expect(localStorage.getItem(KEY)).toBe(DRAFT);
    await act(async () => {
      pending.resolve(true);
      await pending.promise;
    });
    expect(textarea.value).toBe("");
    expect(localStorage.getItem(KEY)).toBeNull();
    await unmount();
  });

  it("drops the stored draft even when the surface left before the confirmation", async () => {
    // Home navigates to the new session before the composer hears back.
    const pending = deferred();
    const { enter, unmount } = await mount(() => pending.promise);
    await enter();
    await unmount();
    expect(localStorage.getItem(KEY)).toBe(DRAFT);
    await act(async () => {
      pending.resolve(true);
      await pending.promise;
    });
    expect(localStorage.getItem(KEY)).toBeNull();
  });

  it("a synchronous handler still clears at once, and `false` still keeps", async () => {
    const first = await mount(() => false);
    await first.enter();
    expect(first.textarea.value).toBe(DRAFT);
    await first.unmount();

    const second = await mount(() => undefined);
    await second.enter();
    expect(second.textarea.value).toBe("");
    expect(localStorage.getItem(KEY)).toBeNull();
    await second.unmount();
  });
});

describe("remainderAfterSend", () => {
  it("keeps only what was typed after the sent text", () => {
    expect(remainderAfterSend("hello", "hello")).toBe("");
    expect(remainderAfterSend("hello\nnext one", "hello")).toBe("next one");
    expect(remainderAfterSend("rewritten", "hello")).toBe("rewritten");
    expect(remainderAfterSend("", "hello")).toBe("");
  });
});
