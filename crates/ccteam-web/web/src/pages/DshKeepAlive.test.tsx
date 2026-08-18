// @vitest-environment jsdom
// v0.10.2 (WEB-DSH-1) — THE regression test for "every click on the DSH menu
// reloads the page". Mount the REAL ChatConsole shell in jsdom, visit /dsh,
// navigate away, come back: the <iframe> must be the SAME DOM node throughout
// (an unmount+remount would create a new node and re-load the whole DSH SPA).
// Also covers the lazy gate (zero dsh requests before the first visit) and the
// deliberate reload boundary (stop→start yields a fresh iframe).

import { act, useEffect } from "react";
import { createRoot } from "react-dom/client";
import { MemoryRouter, Route, Routes, useNavigate } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import ChatConsole from "./ChatConsole";
import type { DshStatus } from "../lib/dshApi";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const running: DshStatus = {
  state: "running",
  port: 35479,
  companion_port: 7332,
  home_kind: "own",
  dsh_version: "0.1.0-rc.6",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

let navigate: (to: string) => void = () => {};
function NavProbe() {
  const nav = useNavigate();
  useEffect(() => {
    navigate = nav;
  }, [nav]);
  return null;
}

describe("DSH keep-alive across navigation (WEB-DSH-1)", () => {
  let container: HTMLDivElement;
  let root: ReturnType<typeof createRoot>;
  const realFetch = globalThis.fetch;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("first visit loads the iframe; leaving + returning never remounts it", async () => {
    // A tiny state machine: status reflects the last lifecycle action, so the
    // shell behaves like the real backend through stop→start.
    let dsh: DshStatus = running;
    globalThis.fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/api/v1/dsh/status")) return Promise.resolve(jsonResponse(200, dsh));
      if (url.endsWith("/api/v1/dsh/stop")) {
        dsh = { state: "stopped", companion_port: null };
        return Promise.resolve(jsonResponse(200, dsh));
      }
      if (url.endsWith("/api/v1/dsh/start")) {
        dsh = running;
        return Promise.resolve(jsonResponse(200, dsh));
      }
      // Everything else (me / projects / sessions / SSE) hangs — the shell
      // renders its loading state, which is all this test needs.
      return new Promise<Response>(() => {});
    });
    const dshCalls = () =>
      vi.mocked(globalThis.fetch).mock.calls.filter(([input]) =>
        String(input).includes("/api/v1/dsh/"),
      );

    await act(async () => {
      root = createRoot(container);
      root.render(
        <MemoryRouter initialEntries={["/"]}>
          <NavProbe />
          <Routes>
            <Route path="/" element={<ChatConsole />} />
            <Route path="/dsh" element={<ChatConsole />} />
          </Routes>
        </MemoryRouter>,
      );
    });

    // Lazy gate: on the Home route the store was never visited → zero dsh
    // requests, no iframe anywhere in the DOM.
    expect(dshCalls()).toHaveLength(0);
    expect(container.querySelector('[data-testid="dsh-frame"]')).toBeNull();
    expect(container.querySelector('[data-testid="dsh-frame-host"]')).toBeNull();

    // First visit: DshView mounts, marks the store visited, status loads,
    // and the persistent host renders the iframe.
    await act(async () => navigate("/dsh"));
    const frame = container.querySelector('[data-testid="dsh-frame"]') as HTMLIFrameElement | null;
    expect(frame).not.toBeNull();
    expect(frame?.getAttribute("src")).toBe("http://localhost:7332/");
    const host = container.querySelector('[data-testid="dsh-frame-host"]');
    expect(host).not.toBeNull();
    expect((host as HTMLDivElement).hidden).toBe(false);
    // The head-only view makes room for the host's stage below it.
    expect(container.querySelector('[data-testid="dsh-view"]')?.className).toContain(
      "dsh-view--flat",
    );

    // Navigate away: DshView unmounts, but the iframe stays in the DOM — same
    // node, same src — only the host wrapper is hidden.
    await act(async () => navigate("/"));
    expect(container.querySelector('[data-testid="dsh-view"]')).toBeNull();
    const hostHidden = container.querySelector('[data-testid="dsh-frame-host"]') as HTMLDivElement;
    expect(hostHidden).not.toBeNull();
    expect(hostHidden.hidden).toBe(true);
    expect(container.querySelector('[data-testid="dsh-frame"]')).toBe(frame);
    expect(frame?.getAttribute("src")).toBe("http://localhost:7332/");

    // Navigate back: still the SAME iframe node (a remount would have created
    // a new element and re-fetched the DSH SPA).
    await act(async () => navigate("/dsh"));
    expect(container.querySelector('[data-testid="dsh-frame"]')).toBe(frame);
    expect(
      (container.querySelector('[data-testid="dsh-frame-host"]') as HTMLDivElement).hidden,
    ).toBe(false);
    expect(container.querySelectorAll('[data-testid="dsh-frame"]')).toHaveLength(1);

    // Reload boundary: stop→start is a NEW instance, so a fresh iframe is
    // expected (src passes through null) — this path is not keep-alive.
    const stopButton = container.querySelector('[data-testid="dsh-stop"]') as HTMLButtonElement;
    await act(async () => stopButton.click());
    expect(container.querySelector('[data-testid="dsh-frame"]')).toBeNull();
    const startButton = container.querySelector('[data-testid="dsh-start"]') as HTMLButtonElement;
    await act(async () => startButton.click());
    const fresh = container.querySelector('[data-testid="dsh-frame"]') as HTMLIFrameElement | null;
    expect(fresh).not.toBeNull();
    expect(fresh).not.toBe(frame);
  });
});
