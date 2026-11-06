// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Markdown } from "./Markdown";

let container: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Markdown code copy", () => {
  it("adds an accessible icon for each block and copies only its exact code", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    await act(() => root.render(<Markdown content={'Intro\n\n```ts\nconst x = "<value>";\n```\n\n```\nsecond\n```'} />));
    const buttons = container.querySelectorAll<HTMLButtonElement>("pre button");
    expect(buttons).toHaveLength(2);
    expect(buttons[0].getAttribute("aria-label")).toBe("复制代码");
    expect(buttons[0].querySelector("svg")).not.toBeNull();
    await act(() => root.render(<Markdown className="updated" content={'Intro\n\n```ts\nconst x = "<value>";\n```\n\n```\nsecond\n```'} />));
    expect(container.querySelectorAll("pre button")).toHaveLength(2);
    await act(() => buttons[0].click());
    expect(writeText).toHaveBeenCalledWith('const x = "<value>";\n');
    expect(buttons[0].getAttribute("aria-label")).toBe("已复制");
    expect(buttons[1].getAttribute("aria-label")).toBe("复制代码");
  });

  it("uses the shared fallback and reports a failed copy honestly", async () => {
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    const exec = vi.fn().mockReturnValue(false);
    Object.defineProperty(document, "execCommand", { configurable: true, value: exec });
    await act(() => root.render(<Markdown content={'```\nhello\n```'} />));
    const button = container.querySelector("button")!;
    await act(() => button.click());
    expect(exec).toHaveBeenCalledWith("copy");
    expect(button.getAttribute("aria-label")).toBe("复制失败，点击重试");
  });
});

describe("Markdown file previews", () => {
  it("replaces document hrefs with minted same-site previews while retaining ordinary links", async () => {
    const resolve = vi.fn().mockResolvedValue("/api/v1/projects/team/files/preview/fresh/docs/issue.md");
    const origin = window.location.origin;
    await act(() => root.render(<Markdown
      content={`[issue](${origin}/repo/docs/issue.md) [web](https://other.example/a.md) [line](/repo/README.md:12)`}
      resolveFilePreview={resolve}
    />));
    expect(resolve).toHaveBeenCalledExactlyOnceWith({ path: "/repo/docs/issue.md" });
    const links = container.querySelectorAll("a");
    expect(links[0].getAttribute("href")).toBe("/api/v1/projects/team/files/preview/fresh/docs/issue.md");
    expect(links[0].target).toBe("_blank");
    expect(links[1].getAttribute("href")).toBe("https://other.example/a.md");
    expect(links[2].getAttribute("href")).toBe("/repo/README.md:12");
  });

  it("renews normal, modified and middle-click previews after backend tickets are lost", async () => {
    const resolve = vi.fn().mockResolvedValue("/api/v1/projects/team/files/preview/old/README.md");
    const replace = vi.fn();
    vi.spyOn(window, "open").mockReturnValue({ opener: null, location: { replace }, close: vi.fn() } as unknown as Window);
    await act(() => root.render(<Markdown content="[doc](README.md)" resolveFilePreview={resolve} />));
    resolve.mockResolvedValue("/api/v1/projects/team/files/preview/new/README.md");
    const link = container.querySelector("a")!;
    for (const [type, init] of [["click", {}], ["click", { ctrlKey: true }], ["auxclick", { button: 1 }]] as const) {
      await act(() => { link.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, ...init })); });
      expect(resolve).toHaveBeenLastCalledWith({ path: "README.md" }, true);
      expect(replace).toHaveBeenLastCalledWith("/api/v1/projects/team/files/preview/new/README.md");
    }
  });
});
