// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CswapAccountPicker } from "./CswapAccountPicker";
import { createSession } from "../lib/sessionsApi";

afterEach(() => { vi.unstubAllGlobals(); });

async function mount(body: unknown, onChange: (value: string) => void = () => {}) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify(body)));
  vi.stubGlobal("fetch", fetcher);
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(() => root.render(<CswapAccountPicker value="" onChange={onChange} lang="zh" />));
  const button = container.querySelector<HTMLButtonElement>("[data-testid=claude-account-btn]")!;
  const menu = () => container.querySelector(".sel")!;
  return { fetcher, container, root, button, menu };
}

describe("optional cswap account selection", () => {
  it("does not discover on mount and keeps default login usable without cswap", async () => {
    const { fetcher, container, root, button, menu } = await mount({ available: false, accounts: [] });
    expect(fetcher).not.toHaveBeenCalled();
    expect(button.textContent).toContain("默认登录");
    await act(() => button.click());
    expect(fetcher).toHaveBeenCalledOnce();
    expect(menu().classList.contains("open")).toBe(true);
    const items = container.querySelectorAll("[role=menuitemradio]");
    expect(items).toHaveLength(1);
    expect(items[0].getAttribute("aria-checked")).toBe("true");
    expect(container.textContent).toContain("继续使用默认登录");
    expect(container.textContent).toContain("重试");
    await act(() => root.unmount());
    container.remove();
  });

  it("picks an account from its own menu and closes on outside press", async () => {
    const onChange = vi.fn();
    const account = { id: "a".repeat(64), slot: "2", label: "work (t***@example.test)", active: false };
    const { container, root, button, menu } = await mount({ available: true, accounts: [account] }, onChange);
    await act(() => button.click());
    const items = container.querySelectorAll<HTMLButtonElement>("[role=menuitemradio]");
    expect(items[1].textContent).toContain("cswap 2 · work");
    await act(() => items[1].click());
    expect(onChange).toHaveBeenCalledWith(account.id);
    expect(menu().classList.contains("open")).toBe(false);
    await act(() => button.click());
    expect(menu().classList.contains("open")).toBe(true);
    await act(() => { document.body.dispatchEvent(new Event("pointerdown", { bubbles: true })); });
    expect(menu().classList.contains("open")).toBe(false);
    await act(() => root.unmount());
    container.remove();
  });

  it("sends an explicit reference only when the caller selected an account", async () => {
    const fetcher = vi.fn().mockImplementation(() => Promise.resolve(new Response('{"sid":"s1"}')));
    vi.stubGlobal("fetch", fetcher);
    await createSession("project", { role: "", vendor: "claude" });
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).not.toHaveProperty("claude_account");
    await createSession("project", { role: "", vendor: "claude", claude_account: "stable-account-id" });
    expect(JSON.parse(fetcher.mock.calls[1][1].body).claude_account).toBe("stable-account-id");
  });
});
