import { beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  const store = new Map<string, string>();
  const localStorage = {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
    removeItem: (key: string) => store.delete(key),
  };
  g.window = { location: { origin: "https://team.example", href: "https://team.example/" }, localStorage, addEventListener() {}, removeEventListener() {} };
  g.localStorage = localStorage;
});

import { renderToString } from "react-dom/server";
import AccessView, {
  externalMcpConfig,
  externalRestSnippet,
  LoginLinkRow,
} from "./AccessView";

describe("AccessView", () => {
  beforeEach(() => {
    window.localStorage.setItem("aoe_auth_token", "fake-login-token");
    window.localStorage.setItem("aoe_auth_token_exp", String(Date.now() + 60_000));
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });

  it("builds the external MCP JSON from the current origin and fake login token", () => {
    const json = externalMcpConfig("https://team.example", "fake-login-token");
    expect(JSON.parse(json)).toEqual({ mcpServers: { ccteam: { transport: "http", url: "https://team.example/mcp", headers: { Authorization: "Bearer fake-login-token" }, disabled: false } } });
  });

  it("renders all six access cards in people → programs → machines order", () => {
    const html = renderToString(<AccessView lang="zh" />);
    expect(html).toContain('data-testid="settings-telegram"');
    expect(html).toContain('data-testid="settings-lark"');
    expect(html).toContain('data-testid="access-login-links"');
    expect(html).toContain('data-testid="access-im"');
    expect(html).toContain('data-testid="access-mcp"');
    expect(html).toContain('data-testid="access-api"');
    expect(html).toContain('data-testid="access-satellite"');
    expect(html).toContain('data-testid="access-mcp-copy"');
    const people = html.indexOf('data-testid="access-people"');
    const programs = html.indexOf('data-testid="access-programs"');
    const machines = html.indexOf('data-testid="access-machines"');
    expect(people).toBeGreaterThan(-1);
    expect(people).toBeLessThan(programs);
    expect(programs).toBeLessThan(machines);
    expect(html).toContain("https://team.example/mcp");
    expect(html).toContain("fake-login-token");
  });

  it("inlines the real origin and token in the REST example and links the API docs", () => {
    const html = renderToString(<AccessView lang="en" />);
    expect(html).toContain('data-testid="access-api-snippet"');
    expect(html).toContain("TOKEN=&#x27;fake-login-token&#x27;");
    expect(html).toContain("https://team.example/api/v1/projects/&lt;project-slug&gt;/sessions");
    expect(html).toContain('href="/api/docs"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain("/api/v1/openapi.json");
  });

  it("builds the three-step REST example with the documented resource routes", () => {
    const snippet = externalRestSnippet("https://team.example", "real-token", "en");
    expect(snippet).toContain("TOKEN='real-token'");
    expect(snippet).toContain("/api/v1/projects/<project-slug>/sessions");
    expect(snippet).toContain("/api/v1/sessions/s42/turn");
    expect(snippet).toContain("/api/v1/sessions/s42/events");
    expect(snippet).toContain("claude|codex|grok|opencode|kimi");
  });

  it("renders a compact tenant row with its copy-login-link action", () => {
    const html = renderToString(<LoginLinkRow user={{ id: "u1", handle: "alice", linked_chat: null, created_at: "2026-01-01" }} label="复制登录链接" onCopy={() => {}} />);
    expect(html.replace(/<!-- -->/g, "")).toContain("@alice");
    expect(html).toContain('data-testid="access-copy-link-u1"');
  });

  it("renders bilingual Access headings", () => {
    expect(renderToString(<AccessView lang="zh" />)).toContain("外部 Agent 接入 (MCP)");
    expect(renderToString(<AccessView lang="en" />)).toContain("External agent access (MCP)");
  });
});
