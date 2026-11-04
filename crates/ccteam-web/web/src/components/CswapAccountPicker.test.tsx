// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CswapAccountPicker } from "./CswapAccountPicker";
import { createSession } from "../lib/sessionsApi";

afterEach(() => { vi.unstubAllGlobals(); });

describe("optional cswap account selection", () => {
  it("does not discover on mount and keeps default login usable without cswap", async () => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify({ available: false, accounts: [] })));
    vi.stubGlobal("fetch", fetcher);
    const container = document.createElement("div");
    const root = createRoot(container);
    await act(() => root.render(<CswapAccountPicker value="" onChange={() => {}} lang="zh" />));
    expect(fetcher).not.toHaveBeenCalled();
    await act(() => {
      const details = container.querySelector("details")!;
      details.open = true;
      details.dispatchEvent(new Event("toggle"));
    });
    expect(fetcher).toHaveBeenCalledOnce();
    expect(container.querySelector("select")?.disabled).toBe(false);
    expect(container.querySelector("option")?.value).toBe("");
    expect(container.textContent).toContain("继续使用默认登录");
    await act(() => root.unmount());
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
