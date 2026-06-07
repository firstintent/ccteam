// v0.8.8 F5 — RolesPage smoke tests.
//
// No DOM env (no jsdom): use React's `renderToString` to assert the initial
// HTML shape, mirroring SessionsListPage.test.tsx / SettingsPage.test.tsx.
// renderToString does NOT run effects, so a never-resolving fetch leaves the
// page in its first synchronous state (the project picker's loading
// placeholder). Stateful, post-fetch paths (the role list / detail success
// views) are asserted by rendering the sub-components directly with seeded
// props — the same pattern the SettingsPage section tests use.
//
// The non-scalar frontmatter fallback (a red line) is unit-tested on the pure
// `renderFrontmatterValue` helper.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import RolesPage, {
  RoleCard,
  RoleDetailBody,
  RoleListView,
} from "./RolesPage";
import { renderFrontmatterValue } from "./rolesView";

const realFetch = globalThis.fetch;

describe("RolesPage initial render", () => {
  beforeEach(() => {
    // Never-resolving fetch keeps the project picker in its loading state
    // (renderToString won't run the useEffect that resolves it).
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the page shell + project-picker loading placeholder", () => {
    const html = renderToString(
      <MemoryRouter>
        <RolesPage />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="roles-page"');
    expect(html).toContain('data-testid="roles-projects-loading"');
    // No project selected yet ⇒ neither the role list nor a detail mounts.
    expect(html).not.toContain('data-testid="roles-list"');
    expect(html).not.toContain('data-testid="role-detail"');
  });
});

describe("RoleListView", () => {
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("shows the loading state synchronously before the fetch resolves", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    // renderToString won't run the useEffect that resolves the fetch, so the
    // list view stays in its initial loading state.
    const html = renderToString(<RoleListView slug="demo" onOpen={() => {}} />);
    expect(html).toContain('data-testid="roles-list-loading"');
  });
});

describe("RoleCard", () => {
  it("renders role name + description + a model pill when present", () => {
    const html = renderToString(
      <RoleCard
        role={{ role: "reviewer", description: "reviews diffs", model: "sonnet" }}
        onOpen={() => {}}
      />,
    );
    expect(html).toContain('data-testid="role-card-reviewer"');
    expect(html).toContain("reviewer");
    expect(html).toContain("reviews diffs");
    expect(html).toContain("sonnet");
  });

  it("omits the model pill + shows a no-description hint when both absent", () => {
    const html = renderToString(
      <RoleCard role={{ role: "bare", description: "", model: "" }} onOpen={() => {}} />,
    );
    expect(html).toContain('data-testid="role-card-bare"');
    expect(html).toContain("无描述");
  });
});

describe("RoleDetailBody", () => {
  it("renders frontmatter rows + the markdown body via the cockpit container", () => {
    const html = renderToString(
      <RoleDetailBody
        detail={{
          role: "reviewer",
          frontmatter: { description: "reviews diffs", model: "sonnet" },
          body: "# Reviewer\n\nYou review code.",
        }}
      />,
    );
    expect(html).toContain('data-testid="role-frontmatter"');
    expect(html).toContain("description");
    expect(html).toContain("reviews diffs");
    // Body went through marked → an <h1> inside the .cockpit-markdown container.
    expect(html).toContain("cockpit-markdown");
    expect(html).toContain("<h1");
    expect(html).toContain("Reviewer");
  });

  it("shows the empty-frontmatter + empty-body hints", () => {
    const html = renderToString(
      <RoleDetailBody detail={{ role: "x", frontmatter: {}, body: "" }} />,
    );
    expect(html).toContain("无 frontmatter");
    expect(html).toContain("空 body");
  });
});

// Red line: frontmatter values may be non-scalar — they must NOT render as
// "[object Object]". Scalars pass through; arrays/objects JSON-stringify.
describe("renderFrontmatterValue", () => {
  it("passes scalars through verbatim", () => {
    expect(renderFrontmatterValue("sonnet")).toBe("sonnet");
    expect(renderFrontmatterValue(42)).toBe("42");
    expect(renderFrontmatterValue(true)).toBe("true");
  });

  it("JSON-stringifies arrays + objects (never [object Object])", () => {
    expect(renderFrontmatterValue(["a", "b"])).toBe('["a","b"]');
    const obj = renderFrontmatterValue({ tools: ["Read"] });
    expect(obj).toBe('{"tools":["Read"]}');
    expect(obj).not.toContain("[object Object]");
  });

  it("renders null/undefined as an em dash", () => {
    expect(renderFrontmatterValue(null)).toBe("—");
    expect(renderFrontmatterValue(undefined)).toBe("—");
  });
});
