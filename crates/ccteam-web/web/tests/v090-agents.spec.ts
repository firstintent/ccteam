import { expect, test, type Page } from "@playwright/test";

// v0.9.0 W4 (F4) — team visualization browser smoke: the graph snapshot
// renders its nodes, and a live `delegation` SSE frame (`dispatched`) flips
// the corresponding edge to active — all traffic mocked at the /api/v1 seam
// (mirrors `v032-spa.spec.ts`'s MockEventSource technique, extended to track
// instances by URL so the test can inject a named SSE frame mid-run, which a
// static `page.route` fulfill body cannot do for a live stream).

const agentsGraph = {
  nodes: [
    {
      sid: "s0",
      slug: "demo",
      role: "brain",
      vendor: "claude",
      host: "local",
      status: "live",
      depth: 0,
      cost_usd: 0.12,
      title: "brain session",
      last_active: "2026-07-13T00:00:00Z",
      turn_count: 4,
    },
    {
      sid: "s1",
      slug: "demo",
      role: "worker",
      vendor: "grok",
      host: "local",
      status: "live",
      parent_sid: "s0",
      depth: 1,
      cost_usd: 0.02,
      title: "research task",
      last_active: "2026-07-13T00:01:00Z",
      turn_count: 1,
    },
  ],
  edges: [{ parent: "s0", child: "s1", title: "research task", active: false }],
  hosts: ["local"],
};

async function mockAgentsApi(page: Page): Promise<void> {
  await page.addInitScript(() => {
    class MockEventSource extends EventTarget {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSED = 2;

      readonly url: string;
      readonly withCredentials = false;
      readyState = MockEventSource.CONNECTING;
      onopen: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;

      constructor(url: string | URL) {
        super();
        this.url = String(url);
        const bag = (window as unknown as { __agentsEventSources: MockEventSource[] });
        bag.__agentsEventSources = bag.__agentsEventSources || [];
        bag.__agentsEventSources.push(this);
        queueMicrotask(() => {
          this.readyState = MockEventSource.OPEN;
          const event = new Event("open");
          this.onopen?.(event);
          this.dispatchEvent(event);
        });
      }

      close() {
        this.readyState = MockEventSource.CLOSED;
      }
    }
    (window as unknown as { EventSource: typeof EventSource }).EventSource =
      MockEventSource as unknown as typeof EventSource;
  });

  await page.route("**/api/**", (route) =>
    route.fulfill({ status: 404, json: { error: `unmocked ${route.request().url()}` } }),
  );
  await page.route("**/api/v1/auth/token", (route) =>
    route.fulfill({ status: 200, json: { wire_token: null } }),
  );
  await page.route("**/api/v1/me", (route) =>
    route.fulfill({ status: 200, json: { id: "admin", handle: "owner", is_admin: true } }),
  );
  await page.route("**/api/v1/projects", (route) => route.fulfill({ status: 200, json: [] }));
  await page.route("**/api/v1/agents/graph", (route) =>
    route.fulfill({ status: 200, json: agentsGraph }),
  );
}

/** Dispatch a named SSE frame into the `/api/v1/agents/events` mock
 *  EventSource instance the page created (there is exactly one — AgentsView
 *  opens it once on mount). */
async function pushAgentsEvent(page: Page, payload: Record<string, unknown>): Promise<void> {
  await page.evaluate((p) => {
    const bag = window as unknown as { __agentsEventSources?: EventTarget[] };
    const sources = bag.__agentsEventSources ?? [];
    const es = sources.find((s) => (s as unknown as { url: string }).url.includes("/agents/events"));
    es?.dispatchEvent(new MessageEvent("delegation", { data: JSON.stringify(p) }));
  }, payload);
}

test("team view renders the graph snapshot; a dispatched SSE frame activates the edge", async ({
  page,
}) => {
  await mockAgentsApi(page);
  await page.goto("/app/agents");

  await expect(page.getByTestId("agents-view")).toBeVisible();
  // v0.9.1 — the roster tree table is the default tab; both sessions row up.
  await expect(page.getByTestId("agents-roster-row-s0")).toBeVisible();
  await expect(page.getByTestId("agents-roster-row-s1")).toBeVisible();
  // The topology graph lives in its own tab now.
  await page.getByTestId("agents-tab-topology").click();
  await expect(page.getByTestId("agents-node-s0")).toBeVisible();
  await expect(page.getByTestId("agents-node-s1")).toBeVisible();

  const edge = page.getByTestId("agents-edge-s0-s1");
  await expect(edge).toHaveAttribute("data-active", "false");

  await pushAgentsEvent(page, {
    kind: "delegation",
    slug: "demo",
    relation: "dispatched",
    parent_sid: "s0",
    child_sid: "s1",
    title: "research task",
  });

  await expect(edge).toHaveAttribute("data-active", "true");

  // Selecting a node opens the detail side panel.
  await page.getByTestId("agents-node-s1").click();
  await expect(page.getByTestId("agents-panel")).toBeVisible();
  await expect(page.getByTestId("agents-open-chat")).toBeVisible();
});
