import { afterEach, describe, expect, it, vi } from "vitest";
import {
  fileLinkTarget,
  createFilePreviewResolver,
  openFileTab,
  projectRelativePath,
  supportsFilePreview,
} from "./filesApi";

describe("project file navigation", () => {
  it("recognizes file URLs on the current origin without hardcoding a host", () => {
    for (const origin of ["https://console.example", "http://localhost:7331"]) {
      expect(fileLinkTarget(`${origin}/repo/docs/issue.md`, origin)).toEqual({
        path: "/repo/docs/issue.md",
      });
      expect(fileLinkTarget(`${origin}/api/v1/projects/a`, origin)).toBeNull();
      expect(fileLinkTarget("https://elsewhere.example/repo/issue.md", origin)).toBeNull();
    }
  });
  it("shares preview controls between HTML and Markdown documents", () => {
    for (const path of [
      "README.md",
      "docs/guide.markdown",
      "README.MD",
      "index.html",
      "INDEX.HTM",
    ])
      expect(supportsFilePreview(path)).toBe(true);
    for (const path of ["lib.rs", "app.mdx", "image.png"])
      expect(supportsFilePreview(path)).toBe(false);
  });
  it("recognizes bare filenames with numeric line suffixes", () => {
    expect(fileLinkTarget("Cargo.toml:12")).toEqual({
      path: "Cargo.toml",
      line: 12,
    });
    expect(fileLinkTarget("Makefile:20")).toEqual({
      path: "Makefile",
      line: 20,
    });
    expect(fileLinkTarget("javascript:1")).toBeNull();
  });
  it("keeps pinned tabs and replaces only the temporary preview", () => {
    let tabs = openFileTab([], "a.rs");
    tabs = openFileTab(tabs, "a.rs", true);
    tabs = openFileTab(tabs, "b.rs");
    tabs = openFileTab(tabs, "c.rs");
    expect(tabs).toEqual([
      { path: "a.rs", pinned: true },
      { path: "c.rs", pinned: false },
    ]);
  });
  it("recognizes file/line references while preserving web and app links", () => {
    expect(fileLinkTarget("/repo/src/lib.rs:42:5")).toEqual({
      path: "/repo/src/lib.rs",
      line: 42,
    });
    expect(fileLinkTarget("src/a.ts#L9-L12")).toEqual({
      path: "src/a.ts",
      line: 9,
    });
    expect(fileLinkTarget("/repo/Makefile:20")).toEqual({
      path: "/repo/Makefile",
      line: 20,
    });
    expect(fileLinkTarget("scripts/run:5")).toEqual({
      path: "scripts/run",
      line: 5,
    });
    for (const href of [
      "https://example.com/file.html",
      "javascript:alert(1)",
      "//example.com/a.ts",
      "#section",
      "/api/v1/projects",
    ])
      expect(fileLinkTarget(href)).toBeNull();
  });
  it("resolves absolute project links and rejects other roots and traversal", () => {
    expect(projectRelativePath("/repo", "/repo/src/a.rs")).toBe("src/a.rs");
    expect(projectRelativePath("/repo", "./src/a.rs")).toBe("src/a.rs");
    for (const p of ["/other/a.rs", "../a.rs", "/repository/a.rs"])
      expect(projectRelativePath("/repo", p)).toBeNull();
  });
});

describe("chat document preview links", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("resolves project paths and shares fresh tickets, then renews expired ones", async () => {
    vi.useFakeTimers();
    let minted = 0;
    const fetcher = vi.fn(async (_url: string, init?: RequestInit) =>
      new Response(JSON.stringify(init?.method === "POST"
        ? { url: `/api/v1/projects/team/files/preview/t${++minted}/docs/issue.md`, expires_in: 3600 }
        : { root: "/repo", paths: [], git_status: {}, truncated: false })),
    );
    vi.stubGlobal("fetch", fetcher);
    const resolve = createFilePreviewResolver("team");
    const first = await Promise.all([
      resolve({ path: "/repo/docs/issue.md" }),
      resolve({ path: "docs/issue.md" }),
    ]);
    expect(first).toEqual(Array(2).fill("/api/v1/projects/team/files/preview/t1/docs/issue.md"));
    expect(fetcher).toHaveBeenCalledTimes(2);
    expect(JSON.parse(fetcher.mock.calls[1][1]?.body as string)).toEqual({ path: "docs/issue.md" });
    vi.advanceTimersByTime(3600_000);
    expect(await resolve({ path: "/repo/docs/issue.md" })).toContain("/preview/t2/");
    expect(await resolve({ path: "/repo/docs/issue.md" }, true)).toContain("/preview/t3/");
  });

  it("does not mint previews for sources, line links or files outside the project", async () => {
    const fetcher = vi.fn(async () => new Response(JSON.stringify({ root: "/repo" })));
    vi.stubGlobal("fetch", fetcher);
    const resolve = createFilePreviewResolver("team");
    expect(await resolve({ path: "src/lib.rs" })).toBeNull();
    expect(await resolve({ path: "README.md", line: 10 })).toBeNull();
    expect(await resolve({ path: "/repository/issue.md" })).toBeNull();
    expect(await resolve({ path: "../issue.md" })).toBeNull();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});
