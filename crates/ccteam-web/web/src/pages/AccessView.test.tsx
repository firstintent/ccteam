import { beforeEach, describe, expect, it, vi } from "vitest";

const identity = vi.hoisted(() => {
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
  return { isAdmin: true };
});

// The scope defaults to a project when one is visible, so the label rule (only
// the machine-user slot needs one) is only reachable with the store settled.
const projectsState = vi.hoisted(() => ({
  projects: null as { slug: string }[] | null,
  loading: true,
}));

vi.mock("../hooks/useProjectsStore", () => ({
  useProjectsStore: () => ({
    projects: projectsState.projects,
    loading: projectsState.loading,
    error: null,
  }),
}));

vi.mock("../hooks/useMe", () => ({
  useMe: () => ({
    me: identity.isAdmin
      ? { id: "admin", handle: "owner", is_admin: true }
      : { id: "u1", handle: "alice", is_admin: false },
    isAdmin: identity.isAdmin,
  }),
}));

import { renderToString } from "react-dom/server";
import AccessView, {
  ExternalAgentCard,
  externalRestSnippet,
  LoginLinkRow,
} from "./AccessView";

/** The mint `<button>`'s own attributes, minus `class` — Tailwind ships a
 *  `disabled:` variant in there, which would match any naive substring check. */
function mintButtonTag(html: string): string {
  const at = html.indexOf('data-testid="access-mcp-mint"');
  expect(at).toBeGreaterThan(-1);
  const tag = html.slice(html.lastIndexOf("<", at), html.indexOf(">", at) + 1);
  return tag.replace(/ class="[^"]*"/, "");
}

describe("AccessView", () => {
  beforeEach(() => {
    identity.isAdmin = true;
    projectsState.projects = null;
    projectsState.loading = true;
    window.localStorage.setItem("aoe_auth_token", "fake-login-token");
    window.localStorage.setItem("aoe_auth_token_exp", String(Date.now() + 60_000));
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });

  // The external-agent card no longer hand-rolls a snippet around the
  // operator's own login token: it mints a SCOPED credential server-side and
  // renders the bodies the daemon produced. So the card's contract here is its
  // controls (scope · label · mint) plus the absence of any client-built config.
  it("offers the scope/label/mint controls and never pastes the login token as the credential", () => {
    const html = renderToString(<ExternalAgentCard lang="en" />);
    expect(html).toContain('data-testid="access-mcp"');
    expect(html).toContain('data-testid="access-mcp-scope"');
    expect(html).toContain('data-testid="access-mcp-label"');
    expect(html).toContain('data-testid="access-mcp-mint"');
    // Machine-user scope is always offerable; a project is the default when one
    // is visible (the list arrives from an effect, so SSR shows just this one).
    expect(html).toContain("Machine user (all my projects)");
    // Nothing to copy — and no credential on the page — until a mint happens.
    expect(html).not.toContain('data-testid="access-mcp-snippet"');
    expect(html).not.toContain("fake-login-token");
    expect(html).not.toContain("mcpServers");
  });

  // The unlabelled machine-user slot is the daemon's own credential — the one
  // written into the harness global configs — so the form must not offer a
  // user-scoped mint without a label. The API refuses it regardless; this is
  // the half that stops the console from asking.
  it("requires a label for the machine-user slot and offers no mint until it has one", () => {
    projectsState.projects = [];
    projectsState.loading = false;
    const html = renderToString(<ExternalAgentCard lang="en" />);
    expect(html).toContain("Label (required)");
    expect(html).toContain('data-testid="access-mcp-label-why"');
    expect(html).toContain("machine-user slot is the daemon");
    expect(mintButtonTag(html)).toContain("disabled");
  });

  // ...and the rule is the USER slot's alone: a project-scoped credential
  // cannot collide with it, so that mint keeps working unlabelled.
  it("leaves the label optional for a project-scoped credential", () => {
    projectsState.projects = [{ slug: "alpha" }];
    projectsState.loading = false;
    const html = renderToString(<ExternalAgentCard lang="en" />);
    expect(html).toContain("Label (optional)");
    expect(html).not.toContain('data-testid="access-mcp-label-why"');
    expect(mintButtonTag(html)).not.toContain("disabled");
  });

  it("keeps the admin shape with all six access cards in people → programs → machines order", () => {
    const html = renderToString(<AccessView lang="zh" />);
    expect(html).toContain('data-testid="settings-telegram"');
    expect(html).toContain('data-testid="settings-lark"');
    expect(html).toContain('data-testid="access-login-links"');
    expect(html).toContain('data-testid="access-im"');
    expect(html).toContain('data-testid="access-mcp"');
    expect(html).toContain('data-testid="access-api"');
    expect(html).toContain('data-testid="access-satellite"');
    expect(html).not.toContain('data-testid="access-my-im"');
    expect(html).toContain('data-testid="access-mcp-mint"');
    const people = html.indexOf('data-testid="access-people"');
    const programs = html.indexOf('data-testid="access-programs"');
    const machines = html.indexOf('data-testid="access-machines"');
    expect(people).toBeGreaterThan(-1);
    expect(people).toBeLessThan(programs);
    expect(programs).toBeLessThan(machines);
    // The REST card still inlines the caller's own login token (that IS its
    // credential); the MCP card deliberately does not.
    expect(html).toContain("https://team.example/api/v1");
    expect(html).toContain("fake-login-token");
  });

  it("renders the tenant shape without global IM config or login links", () => {
    identity.isAdmin = false;
    const html = renderToString(<AccessView lang="zh" />);
    expect(html).toContain('data-testid="access-my-im"');
    expect(html).toContain('data-testid="settings-my-im"');
    expect(html).toContain('data-testid="access-mcp"');
    expect(html).toContain('data-testid="access-api"');
    expect(html).toContain('data-testid="access-satellite"');
    expect(html).not.toContain('data-testid="settings-telegram"');
    expect(html).not.toContain('data-testid="settings-lark"');
    expect(html).not.toContain('data-testid="settings-transport-warning"');
    expect(html).not.toContain('data-testid="access-login-links"');
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
    expect(snippet).toContain("claude|codex|grok|opencode|kimi|pi|dsh");
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
