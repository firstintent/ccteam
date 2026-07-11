import { describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import WorkflowView from "./WorkflowView";
import { compareUrl, evolutionUrl } from "../lib/workflowApi";

describe("WorkflowView", () => {
  it("renders five tabs and compare affordances", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<WorkflowView />);
    expect(html).toContain("workflow-view");
    expect(html).toContain("workflow-tab-skills");
    expect(html).toContain("workflow-tab-roles");
    expect(html).toContain("workflow-tab-mcp");
    expect(html).toContain("workflow-tab-evolution");
    expect(html).toContain("workflow-tab-compare");
  });
});

describe("workflowApi urls", () => {
  it("builds compare and evolution paths", () => {
    expect(compareUrl("demo")).toBe("/api/v1/projects/demo/compare");
    expect(evolutionUrl("my-app")).toBe("/api/v1/projects/my-app/evolution");
  });
});
