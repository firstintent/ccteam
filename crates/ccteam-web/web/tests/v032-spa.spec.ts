import { expect, test, type Page } from "@playwright/test";

// Mocks mirror the current `/api/v1` shapes (post-W5a flex EOL): the
// dashboard row + ProjectSummary no longer carry `is_flex` / `sessions`
// / `current_phase`, and `kind` is the `team_kind_label` set
// (`workflow` / `multi_workflow`). The slug name is just an identifier.
const dashboardRows = [
  {
    slug: "dev-team",
    team: "dev",
    kind: "workflow",
    last_event_label: "idle",
    badge_class: "badge-ok",
    badge_label: "Running",
    cost_label: "$0.42",
  },
];

const projectSummary = {
  slug: "dev-team",
  team: "dev",
  kind: "workflow",
  badge_class: "badge-ok",
  badge_label: "Running",
  cost_label: "$0.42",
  created_at: "2026-05-13T00:00:00Z",
  state: { user_pause_pending: false },
  events: [
    {
      ts: "2026-05-13T00:02:00Z",
      event: "agent_start",
      detail: "implement",
    },
  ],
  outbox: [],
  workflow_summary: null,
};

const sessionDetail = {
  slug: "dev-team",
  sid: "claude-1",
  team: "dev",
  kind: "workflow",
  harness: "claude",
  harness_class: "harness-claude",
  tmux_session: "ccteam-dev-team-claude-1",
  started_at: "2026-05-13T00:01:00Z",
  status_class: "badge-ok",
  status_label: "Running",
  cost_label: "$0.17",
  events: [
    {
      ts: "2026-05-13T00:03:00Z",
      event: "PostToolUse",
      detail: "Read",
      tool: "Read",
    },
  ],
  outbox: [],
  harness_snapshot: null,
};

type CapturedRequest = {
  url: string;
  method: string;
  body: unknown;
};

async function mockCcteamApi(page: Page): Promise<CapturedRequest[]> {
  const captured: CapturedRequest[] = [];

  await page.addInitScript(() => {
    class MockWebSocket extends EventTarget {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSING = 2;
      static CLOSED = 3;

      readonly url: string;
      readonly protocol = "ccteam-pty.v1";
      binaryType: BinaryType = "arraybuffer";
      readyState = MockWebSocket.CONNECTING;
      onopen: ((event: Event) => void) | null = null;
      onclose: ((event: CloseEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;

      constructor(url: string | URL) {
        super();
        this.url = String(url);
        queueMicrotask(() => {
          this.readyState = MockWebSocket.OPEN;
          const event = new Event("open");
          this.onopen?.(event);
          this.dispatchEvent(event);
        });
      }

      send() {}

      close() {
        this.readyState = MockWebSocket.CLOSED;
        const event = new CloseEvent("close");
        this.onclose?.(event);
        this.dispatchEvent(event);
      }
    }

    (window as unknown as { WebSocket: typeof WebSocket }).WebSocket =
      MockWebSocket as unknown as typeof WebSocket;
  });

  await page.route("**/api/**", (route) =>
    route.fulfill({
      status: 404,
      json: { error: `unmocked ${route.request().url()}` },
    }),
  );
  await page.route("**/sse/**", (route) =>
    route.fulfill({
      status: 200,
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
      },
      body: "\n",
    }),
  );

  await page.route("**/api/v1/auth/token", (route) =>
    route.fulfill({ status: 200, json: { wire_token: null } }),
  );
  await page.route("**/api/v1/projects", (route) =>
    route.fulfill({ status: 200, json: dashboardRows }),
  );
  await page.route("**/api/v1/projects/dev-team", (route) =>
    route.fulfill({ status: 200, json: projectSummary }),
  );
  await page.route("**/api/v1/projects/dev-team/sessions/claude-1", (route) =>
    route.fulfill({ status: 200, json: sessionDetail }),
  );

  async function captureJson(route: Parameters<Page["route"]>[1] extends (route: infer R) => unknown ? R : never) {
    const req = route.request();
    captured.push({
      url: req.url(),
      method: req.method(),
      body: req.postDataJSON(),
    });
    await route.fulfill({ status: 200, json: { ok: true } });
  }

  await page.route("**/api/dev-team/btw", captureJson);
  await page.route("**/api/dev-team/pause", captureJson);
  await page.route("**/api/dev-team/resume", captureJson);
  await page.route("**/api/dev-team/claude-1/btw", captureJson);
  await page.route("**/api/dev-team/claude-1/pause", captureJson);
  await page.route("**/api/dev-team/claude-1/resume", captureJson);

  return captured;
}

test("dashboard opens project detail through the V0.3.2 SPA routes", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.goto("/app/");

  await expect(page.getByText("dev-team").first()).toBeVisible();
  await expect(page.getByText("dev / workflow").first()).toBeVisible();

  await page.getByRole("button", { name: /dev-team/ }).click();
  await expect(page).toHaveURL(/\/app\/p\/dev-team$/);
  await expect(
    page.getByRole("heading", { name: "dev-team" }),
  ).toBeVisible();
});

test("project detail posts BTW and pause through JSON write actions", async ({
  page,
}) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/p/dev-team");

  await page.getByLabel("BTW (1..=4000 chars)").fill("ship gate note");
  await page.getByRole("button", { name: "Submit BTW" }).click();
  await expect
    .poll(() => captured.some((r) => r.url.endsWith("/api/dev-team/btw")))
    .toBe(true);

  await page.getByRole("button", { name: "Pause" }).click();
  await expect
    .poll(() => captured.some((r) => r.url.endsWith("/api/dev-team/pause")))
    .toBe(true);

  expect(captured).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        method: "POST",
        body: { text: "ship gate note" },
      }),
      expect.objectContaining({
        method: "POST",
        body: {},
      }),
    ]),
  );
});

test("session detail renders identity, workflow notice, and sid-scoped BTW", async ({
  page,
}) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/p/dev-team/s/claude-1");

  await expect(page.getByRole("heading", { name: "claude-1" })).toBeVisible();
  // Workflow sessions (kind !== "flex") hide the HarnessPanel + terminal
  // mount and surface the workflow notice; the sid-scoped BTW stays.
  await expect(
    page.getByTestId("workflow-session-notice"),
  ).toBeVisible();
  await expect(page.locator('[data-term="agent"]')).toHaveCount(0);

  await page.getByLabel("BTW (1..=4000 chars)").fill("session-scoped note");
  await page.getByRole("button", { name: "Submit BTW" }).click();
  await expect
    .poll(() =>
      captured.some((r) => r.url.endsWith("/api/dev-team/claude-1/btw")),
    )
    .toBe(true);
  expect(captured.at(-1)?.body).toEqual({ text: "session-scoped note" });
});

test("401 after auth-required bootstrap shows the token entry flow", async ({
  page,
}) => {
  await page.route("**/api/v1/auth/token", (route) =>
    route.fulfill({ status: 200, json: { wire_token: "ccteam:deadbeef" } }),
  );
  await page.route("**/api/v1/projects", (route) =>
    route.fulfill({ status: 401, json: { error: "auth required" } }),
  );
  await page.route("**/sse/**", (route) =>
    route.fulfill({ status: 200, body: "\n" }),
  );

  await page.goto("/app/");
  await expect(page.getByLabel("Token or URL")).toBeVisible();
  await page.getByLabel("Token or URL").fill("abc123");
  await page.getByRole("button", { name: "Connect" }).click();
  await expect(page).toHaveURL(/\/\?token=ccteam%3Aabc123$/);
});
