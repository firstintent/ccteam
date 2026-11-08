// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CswapAccountPicker } from "./CswapAccountPicker";
import { createSession, switchSessionClaudeAccount, type CswapAccount } from "../lib/sessionsApi";

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
    const account = { id: "a".repeat(64), slot: "2", label: "work", active: false };
    const { container, root, button, menu } = await mount({ available: true, accounts: [account] }, onChange);
    await act(() => button.click());
    const items = container.querySelectorAll<HTMLButtonElement>("[role=menuitemradio]");
    expect(items[1].textContent).toContain("#2 work");
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

  it("shows number, name and the quota windows the plan has, never an email domain", async () => {
    const { container, root, button } = await mount({ available: true, accounts: [
      { id: "a".repeat(64), slot: "1", label: "cloud", active: false, five_hour_pct: 11, seven_day_pct: 100 },
      { id: "b".repeat(64), slot: "5", label: "sddenter", active: true, seven_day_pct: 100 },
      { id: "c".repeat(64), slot: "6", label: "sudongdong", active: false, usage_status: "relogin_required" },
      { id: "d".repeat(64), slot: "7", label: "stale", active: false, five_hour_pct: 3, seven_day_pct: 9, usage_status: "unavailable" },
    ] });
    await act(() => button.click());
    const rows = [...container.querySelectorAll("[role=menuitemradio]")].slice(1).map((row) => row.textContent);
    expect(rows[0]).toBe("#1 cloud5h:11%7d:100%✓");
    expect(rows[1]).toBe("#5 sddenter7d:100%当前默认✓");
    expect(rows[2]).toBe("#6 sudongdong需重新登录✓");
    expect(rows[3]).toContain("7d:9%上次");
    expect(container.textContent).not.toContain("@");
    await act(() => root.unmount());
    container.remove();
  });

  it("in a running session names the default login's account and switches in place (#236)", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    const cloud = { id: "a".repeat(64), slot: "1", label: "cloud", active: true };
    const work = { id: "b".repeat(64), slot: "2", label: "work", active: false };
    const fetcher = vi.fn().mockImplementation(() =>
      Promise.resolve(new Response(JSON.stringify({ available: true, accounts: [cloud, work] }))));
    vi.stubGlobal("fetch", fetcher);
    let attempt = 0;
    const onChange = vi.fn(() => (attempt++ === 0
      ? Promise.reject(new Error("a turn is running; wait for it to finish or interrupt it first"))
      : Promise.resolve()));
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(() => root.render(<CswapAccountPicker value="" onChange={onChange} lang="zh"
      session={{ sid: "s1", bound: null }} />));
    expect(fetcher).toHaveBeenCalledOnce();
    const button = container.querySelector<HTMLButtonElement>("[data-testid=claude-account-btn]")!;
    expect(button.textContent).toBe("默认 · #1 cloud");
    await act(() => button.click());
    const menu = () => container.querySelector(".sel")!;
    const rows = () => container.querySelectorAll<HTMLButtonElement>("[role=menuitemradio]");
    expect(container.textContent).toContain("下一条消息在同一会话里按新账号继续");
    await act(() => rows()[2].click());
    expect(onChange).toHaveBeenLastCalledWith(work.id);
    expect(menu().classList.contains("open")).toBe(true);
    expect(container.querySelector(".account-note.err")?.textContent).toContain("a turn is running");
    await act(() => rows()[2].click());
    expect(menu().classList.contains("open")).toBe(false);
    await act(() => root.unmount());
    container.remove();
  });

  it("stays out of a session's way without cswap unless the session is bound", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() =>
      Promise.resolve(new Response(JSON.stringify({ available: false, accounts: [], message: "cswap is not installed" })))));
    const bound: CswapAccount = { id: "c".repeat(64), slot: "3", label: "solo", active: false };
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(() => root.render(<CswapAccountPicker value="" onChange={() => {}} lang="zh"
      session={{ sid: "s1", bound: null }} />));
    expect(container.querySelector("[data-testid=claude-account-btn]")).toBeNull();
    await act(() => root.render(<CswapAccountPicker key="s2" value={bound.id} onChange={() => {}} lang="zh"
      session={{ sid: "s2", bound }} />));
    expect(container.querySelector("[data-testid=claude-account-btn]")?.textContent).toBe("#3 solo");
    await act(() => root.unmount());
    container.remove();
  });

  it("asks the daemon to switch with the account id, or null for the default login", async () => {
    const fetcher = vi.fn().mockImplementation(() => Promise.resolve(new Response(
      JSON.stringify({ sid: "s1", claude_account: null, changed: true, released: true }))));
    vi.stubGlobal("fetch", fetcher);
    await switchSessionClaudeAccount("s1", "stable-account-id");
    await switchSessionClaudeAccount("s1", null);
    expect(fetcher.mock.calls[0][0]).toBe("/api/v1/sessions/s1/claude-account");
    expect(fetcher.mock.calls[0][1].method).toBe("PUT");
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({ claude_account: "stable-account-id" });
    expect(JSON.parse(fetcher.mock.calls[1][1].body)).toEqual({ claude_account: null });
    fetcher.mockImplementation(() => Promise.resolve(new Response(
      JSON.stringify({ error: "history is still private" }),
      { status: 422, headers: { "content-type": "application/json" } })));
    await expect(switchSessionClaudeAccount("s1", null)).rejects.toThrow("history is still private");
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
