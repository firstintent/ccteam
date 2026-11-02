import { describe, expect, it } from "vitest";
import { fileLinkTarget, openFileTab, projectRelativePath } from "./filesApi";

describe("project file navigation", () => {
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
