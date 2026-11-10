// @vitest-environment jsdom
// #237 / #239 — the Home launch funnel (project → session → first message)
// settles each step that succeeds: a retry redoes only what failed, so a
// failed session start never creates the project a second time, and the
// prompt is never lost.
import { act } from "react";
import { createRoot } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import HomeView from "./HomeView";
import { toastBus } from "../lib/toastBus";

const PROMPT = "review the SDK before release";
const HOME_DRAFT = "ccteam.draft.v1.home";

type Reply = { status: number; body: unknown };

/** Unmounted after every test, so one failure cannot leak into the next. */
const mounted: (() => Promise<void>)[] = [];

afterEach(async () => {
  for (const unmount of mounted.splice(0)) await unmount();
  vi.unstubAllGlobals();
  localStorage.clear();
  toastBus.handler = null;
});

/** Route POSTs to queued replies; everything else (catalogs, hosts, roles)
 *  never settles, so the page stays on its static defaults. */
function backend(replies: Record<string, Reply[]>) {
  const posts: { url: string; body: unknown }[] = [];
  const fetcher = vi.fn((url: string, init?: RequestInit) => {
    if (init?.method !== "POST") return new Promise<Response>(() => {});
    posts.push({ url, body: JSON.parse(String(init.body)) });
    const reply = replies[url]?.shift();
    if (!reply) return Promise.reject(new Error(`unexpected POST ${url}`));
    return Promise.resolve(new Response(JSON.stringify(reply.body), { status: reply.status }));
  });
  vi.stubGlobal("fetch", fetcher);
  return posts;
}

async function settle() {
  for (let i = 0; i < 5; i += 1) {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
}

async function mountNewProject(path: string) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.setItem(HOME_DRAFT, PROMPT);
  const toasts = { error: vi.fn(), info: vi.fn(), push: vi.fn() };
  toastBus.handler = toasts;
  const onLaunched = vi.fn();
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(() =>
    root.render(
      <MemoryRouter>
        <HomeView
          lang="zh"
          projects={["demo"]}
          projectPaths={{ demo: "/w/demo" }}
          onLaunched={onLaunched}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    ),
  );
  await act(() => container.querySelector<HTMLButtonElement>(".sel-item.new")!.click());
  const input = container.querySelector<HTMLInputElement>("#newproj-path")!;
  await act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, path);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const textarea = container.querySelector<HTMLTextAreaElement>("[data-testid=composer-textarea]")!;
  const send = async () => {
    await act(() => {
      textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    await settle();
  };
  const projectLabel = () =>
    container.querySelector("[data-testid=ctx-project] .ctx-btn .v")!.textContent;
  mounted.push(async () => {
    await act(() => root.unmount());
    container.remove();
  });
  return { textarea, send, onLaunched, toasts, projectLabel };
}

const created = (slug: string, status = 201): Reply => ({
  status,
  body: { slug, host: "local", path: `/w/${slug}` },
});

describe("Home launch funnel (#239)", () => {
  it("a failed session start keeps the prompt and the retry reuses the created project", async () => {
    const posts = backend({
      "/api/v1/projects": [created("sdk")],
      "/api/v1/projects/sdk/sessions": [
        { status: 500, body: { ok: false, error: "spawn failed" } },
        { status: 201, body: { sid: "s9" } },
      ],
      "/api/v1/sessions/s9/turn": [{ status: 200, body: { accepted: true } }],
    });
    const home = await mountNewProject("/w/sdk");

    await home.send();
    expect(posts.map((p) => p.url)).toEqual([
      "/api/v1/projects",
      "/api/v1/projects/sdk/sessions",
    ]);
    expect(home.textarea.value).toBe(PROMPT);
    expect(localStorage.getItem(HOME_DRAFT)).toBe(PROMPT);
    expect(home.toasts.error).toHaveBeenCalledWith(expect.stringContaining("输入内容已保留"));
    expect(home.projectLabel()).toBe("sdk");
    expect(home.onLaunched).not.toHaveBeenCalled();

    await home.send();
    expect(posts.map((p) => p.url)).toEqual([
      "/api/v1/projects",
      "/api/v1/projects/sdk/sessions",
      "/api/v1/projects/sdk/sessions",
      "/api/v1/sessions/s9/turn",
    ]);
    expect(posts[3].body).toEqual({ text: PROMPT });
    expect(home.onLaunched).toHaveBeenCalledWith("s9");
    expect(localStorage.getItem(HOME_DRAFT)).toBeNull();
  });

  it("a session whose first message failed opens with the prompt in its own draft", async () => {
    backend({
      "/api/v1/projects": [created("sdk")],
      "/api/v1/projects/sdk/sessions": [{ status: 201, body: { sid: "s9" } }],
      "/api/v1/sessions/s9/turn": [{ status: 503, body: { ok: false, error: "busy" } }],
    });
    const home = await mountNewProject("/w/sdk");

    await home.send();
    expect(home.onLaunched).toHaveBeenCalledWith("s9");
    expect(localStorage.getItem("ccteam.draft.v1.s9")).toBe(PROMPT);
    expect(localStorage.getItem(HOME_DRAFT)).toBeNull();
    expect(home.toasts.error).toHaveBeenCalledWith(expect.stringContaining("已放回该会话的输入框"));
  });

  it("a directory that already is a project is used, and the user is told", async () => {
    const posts = backend({
      "/api/v1/projects": [created("dexos-sdk", 200)],
      "/api/v1/projects/dexos-sdk/sessions": [{ status: 201, body: { sid: "s9" } }],
      "/api/v1/sessions/s9/turn": [{ status: 200, body: { accepted: true } }],
    });
    const home = await mountNewProject("/w/dexos-sdk");

    await home.send();
    expect(posts.map((p) => p.url)).toContain("/api/v1/projects/dexos-sdk/sessions");
    expect(home.toasts.info).toHaveBeenCalledWith(expect.stringContaining("该目录已是项目 dexos-sdk"));
    expect(home.onLaunched).toHaveBeenCalledWith("s9");
  });
});
