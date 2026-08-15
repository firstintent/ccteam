// @vitest-environment jsdom
// v0.9.15 — DSH page: API contract + loopback/disabled/embed logic + an SSR
// smoke render. The lifecycle UI itself is thin; the load-bearing bits are the
// pure gating helpers (native link only on loopback, embed only when serving)
// and the /api/v1/dsh/* wire shape.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";

import DshView from "./DshView";
import {
  embedSrc,
  getDshStatus,
  isDisabled,
  isLoopbackHost,
  nativeHref,
  startDsh,
  stopDsh,
  type DshStatus,
} from "../lib/dshApi";

const status = (over: Partial<DshStatus> = {}): DshStatus => ({
  state: "running",
  port: 35479,
  companion_port: 7332,
  home_kind: "own",
  dsh_version: "0.1.0-rc.6",
  native_url: "http://127.0.0.1:3080/",
  ...over,
});

const realFetch = globalThis.fetch;
function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("isLoopbackHost", () => {
  it("accepts loopback names only", () => {
    expect(isLoopbackHost("127.0.0.1")).toBe(true);
    expect(isLoopbackHost("localhost")).toBe(true);
    expect(isLoopbackHost("::1")).toBe(true);
    expect(isLoopbackHost("ccteam.example.com")).toBe(false);
    expect(isLoopbackHost("192.168.1.10")).toBe(false);
  });
});

describe("isDisabled — tolerate both off encodings", () => {
  it("treats state:'disabled' and disabled:true alike, else false", () => {
    expect(isDisabled(status({ state: "disabled" }))).toBe(true);
    expect(isDisabled(status({ disabled: true }))).toBe(true);
    expect(isDisabled(status({ state: "running" }))).toBe(false);
    expect(isDisabled(null)).toBe(false);
  });
});

describe("embedSrc — only when serving", () => {
  it("is the companion origin when running/attached, null otherwise", () => {
    // jsdom serves http://localhost/ by default.
    expect(embedSrc(status({ state: "running" }))).toBe("http://localhost:7332/");
    expect(embedSrc(status({ state: "attached" }))).toBe("http://localhost:7332/");
    expect(embedSrc(status({ state: "starting" }))).toBeNull();
    expect(embedSrc(status({ state: "stopped" }))).toBeNull();
    expect(embedSrc(null)).toBeNull();
  });
});

describe("nativeHref — loopback gate", () => {
  it("returns the native url on a loopback host (jsdom = localhost)", () => {
    expect(nativeHref(status())).toBe("http://127.0.0.1:3080/");
  });
  it("is null without a native_url (tenant) even on loopback", () => {
    expect(nativeHref(status({ native_url: null }))).toBeNull();
  });
});

describe("dsh API wire", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GET /api/v1/dsh/status with same-origin creds", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, status({ state: "running" })));
    const got = await getDshStatus();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/dsh/status", {
      method: "GET",
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.state).toBe("running");
    expect(got.companion_port).toBe(7332);
  });

  it("POST start / stop", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, status({ state: "starting" })));
    expect((await startDsh()).state).toBe("starting");
    expect(fetchMock).toHaveBeenLastCalledWith("/api/v1/dsh/start", expect.objectContaining({ method: "POST" }));

    fetchMock.mockResolvedValueOnce(jsonResponse(200, status({ state: "stopped" })));
    expect((await stopDsh()).state).toBe("stopped");
    expect(fetchMock).toHaveBeenLastCalledWith("/api/v1/dsh/stop", expect.objectContaining({ method: "POST" }));
  });

  it("maps 401 to UNAUTHENTICATED", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(401, { error: "auth required" }));
    await expect(getDshStatus()).rejects.toThrow("UNAUTHENTICATED");
  });
});

describe("DshView SSR smoke", () => {
  it("renders the shell without throwing (loading state, no effects on SSR)", () => {
    const html = renderToString(<DshView lang="en" />);
    expect(html).toContain("dsh-view");
    expect(html).toContain("DSH");
  });
});
