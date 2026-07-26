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
import { evolutionUrl, mcpServersUrl } from "../lib/workflowApi";

describe("WorkflowView", () => {
  it("renders five workflow tabs with marketplace between roles and MCP", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<WorkflowView />);
    expect(html).toContain("workflow-view");
    expect(html).toContain("workflow-tab-skills");
    expect(html).toContain("workflow-tab-roles");
    expect(html).toContain("workflow-tab-market");
    expect(html).toContain("workflow-tab-mcp");
    expect(html).toContain("workflow-tab-evolution");
    expect(html.indexOf("workflow-tab-roles")).toBeLessThan(html.indexOf("workflow-tab-market"));
    expect(html.indexOf("workflow-tab-market")).toBeLessThan(html.indexOf("workflow-tab-mcp"));
  });

  it("renders MarketplaceView in the market tab", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<WorkflowView tab="market" />);
    expect(html).toContain('data-testid="marketplace-view"');
  });

  it("MCP tab: admin sees the register form + prefill templates; tenant does not", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const admin = renderToString(<WorkflowView tab="mcp" isAdmin />);
    expect(admin).toContain('data-testid="mcp-register-form"');
    expect(admin).toContain('data-testid="mcp-tpl-context7"');
    expect(admin).toContain('data-testid="mcp-tpl-playwright"');
    expect(admin).toContain('data-testid="mcp-rows"');
    // Templates prefill only — nothing auto-executes (copy says so).
    expect(admin).toContain("不执行");

    const tenant = renderToString(<WorkflowView tab="mcp" />);
    expect(tenant).not.toContain('data-testid="mcp-register-form"');
    expect(tenant).toContain("仅 admin");
  });

});

describe("workflowApi urls", () => {
  it("builds evolution and MCP-server paths", () => {
    expect(evolutionUrl("my-app")).toBe("/api/v1/projects/my-app/evolution");
    expect(mcpServersUrl("demo")).toBe("/api/v1/projects/demo/mcp-servers");
  });
});
