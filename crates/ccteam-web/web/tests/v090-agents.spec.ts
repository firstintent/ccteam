import { expect, test, type Page } from "@playwright/test";

// Team visualization browser smoke: the graph snapshot renders as a tree,
// and a live `delegation` SSE frame (`dispatched`) highlights its child row —
// all traffic mocked at the /api/v1 seam
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
    const originalFetch = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      if (!url.includes("/api/v1/agents/events")) return originalFetch(input, init);
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          const bag = window as unknown as { __pushAgentsEvent?: (payload: Record<string, unknown>) => void };
          bag.__pushAgentsEvent = (payload) => controller.enqueue(new TextEncoder().encode(`event: delegation\ndata: ${JSON.stringify(payload)}\n\n`));
          init?.signal?.addEventListener("abort", () => controller.close(), { once: true });
        },
      });
      return Promise.resolve(new Response(body, { headers: { "content-type": "text/event-stream" } }));
    };
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

/** Exercise the actual fetch-backed SSE reader, after its stream is ready. */
async function pushAgentsEvent(page: Page, payload: Record<string, unknown>): Promise<void> {
  await expect.poll(() => page.evaluate(() => typeof (window as unknown as { __pushAgentsEvent?: unknown }).__pushAgentsEvent)).toBe("function");
  await page.evaluate((p) => {
    (window as unknown as { __pushAgentsEvent: (payload: Record<string, unknown>) => void }).__pushAgentsEvent(p);
  }, payload);
}

test("team view renders the delegation tree; a dispatched SSE frame highlights the child", async ({
  page,
}) => {
  await mockAgentsApi(page);
  await page.goto("/app/agents");

  await expect(page.getByTestId("agents-view")).toBeVisible();
  await expect(page.getByTestId("agents-tree")).toBeVisible();
  await expect(page.getByTestId("agents-tree-project-demo")).toBeVisible();
  await expect(page.getByTestId("agents-tree-row-s0")).toBeVisible();
  await expect(page.getByTestId("agents-tree-row-s1")).toBeVisible();

  const child = page.getByTestId("agents-tree-row-s1");
  await expect(child).toHaveAttribute("data-delegating", "false");

  await pushAgentsEvent(page, {
    kind: "delegation",
    slug: "demo",
    relation: "dispatched",
    parent_sid: "s0",
    child_sid: "s1",
    title: "research task",
  });

  await expect(child).toHaveAttribute("data-delegating", "true");
  await expect(child).toHaveClass(/delegating/);

  // Selecting a row opens the existing detail side panel.
  await child.click();
  await expect(page.getByTestId("agents-panel")).toBeVisible();
  await expect(page.getByTestId("agents-open-chat")).toBeVisible();
});
