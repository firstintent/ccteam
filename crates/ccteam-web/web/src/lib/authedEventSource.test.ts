// createAuthedEventSource — fetch-backed SSE with Bearer + cookie credentials.
// Node-env vitest: mock fetch + localStorage (no real EventSource / network).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createAuthedEventSource } from "./authedEventSource";
import { clearToken, saveToken } from "./token";

function sseBody(frames: string): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      controller.enqueue(encoder.encode(frames));
      controller.close();
    },
  });
}

describe("createAuthedEventSource", () => {
  beforeEach(() => {
    clearToken();
    vi.stubGlobal(
      "window",
      Object.assign(globalThis, {
        localStorage: (() => {
          const store = new Map<string, string>();
          return {
            getItem: (k: string) => store.get(k) ?? null,
            setItem: (k: string, v: string) => {
              store.set(k, v);
            },
            removeItem: (k: string) => {
              store.delete(k);
            },
          };
        })(),
        location: { href: "http://localhost/", origin: "http://localhost" },
      }),
    );
  });

  afterEach(() => {
    clearToken();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("sends Authorization Bearer when localStorage has a token", async () => {
    saveToken("ccteam:deadbeef");
    let sawAuth: string | null = null;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init?: RequestInit) => {
        const headers = new Headers(init?.headers);
        sawAuth = headers.get("Authorization");
        return new Response(sseBody('event: progress\ndata: {"kind":"answer","content":"hi"}\n\n'), {
          status: 200,
          headers: { "Content-Type": "text/event-stream" },
        });
      }),
    );

    const events: MessageEvent[] = [];
    const es = createAuthedEventSource("/api/v1/sessions/s1/events");
    es.addEventListener("progress", (e) => events.push(e as MessageEvent));
    es.addEventListener("open", () => {});

    // Wait for the async fetch + stream parse.
    await vi.waitFor(() => expect(events.length).toBe(1));
    expect(sawAuth).toBe("Bearer ccteam:deadbeef");
    expect(events[0].data).toContain("hi");
    es.close();
  });

  it("does not attach Bearer on a cross-origin absolute URL", async () => {
    saveToken("ccteam:deadbeef");
    let sawAuth: string | null = "sentinel";
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init?: RequestInit) => {
        const headers = new Headers(init?.headers);
        sawAuth = headers.get("Authorization");
        return new Response(sseBody("event: progress\ndata: {}\n\n"), {
          status: 200,
          headers: { "Content-Type": "text/event-stream" },
        });
      }),
    );
    const es = createAuthedEventSource("https://evil.example/events");
    const opened: boolean[] = [];
    es.addEventListener("open", () => opened.push(true));
    await vi.waitFor(() => expect(opened.length).toBe(1));
    expect(sawAuth).toBeNull();
    es.close();
  });

  it("still opens without a token (cookie-only path; credentials same-origin)", async () => {
    let sawCredentials: RequestCredentials | undefined;
    let sawAuth: string | null = "sentinel";
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: string, init?: RequestInit) => {
        sawCredentials = init?.credentials;
        const headers = new Headers(init?.headers);
        sawAuth = headers.get("Authorization");
        return new Response(sseBody("event: progress\ndata: {}\n\n"), {
          status: 200,
          headers: { "Content-Type": "text/event-stream" },
        });
      }),
    );

    const opened: boolean[] = [];
    const es = createAuthedEventSource("/api/v1/agents/events");
    es.addEventListener("open", () => opened.push(true));
    await vi.waitFor(() => expect(opened.length).toBe(1));
    expect(sawCredentials).toBe("same-origin");
    expect(sawAuth).toBeNull();
    es.close();
  });

  it("emits error on non-OK status (e.g. 401)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("auth required", { status: 401 })),
    );
    const errors: Event[] = [];
    const es = createAuthedEventSource("/api/v1/sessions/s1/events");
    es.addEventListener("error", (e) => errors.push(e));
    await vi.waitFor(() => expect(errors.length).toBe(1));
    es.close();
  });

  it("parses id: into MessageEvent.lastEventId for reconnect watermark", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody('id: 42\nevent: progress\ndata: {"kind":"progress","content":"x"}\n\n'),
          { status: 200, headers: { "Content-Type": "text/event-stream" } },
        ),
      ),
    );
    const events: MessageEvent[] = [];
    const es = createAuthedEventSource("/api/v1/sessions/s1/events");
    es.addEventListener("progress", (e) => events.push(e as MessageEvent));
    await vi.waitFor(() => expect(events.length).toBe(1));
    expect(events[0].lastEventId).toBe("42");
    es.close();
  });
});
