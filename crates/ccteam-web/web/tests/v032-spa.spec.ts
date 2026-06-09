import { expect, test, type Page } from "@playwright/test";

// Browser smoke for the current unified chat shell. It intentionally stays on
// existing surfaces only: `/api/v1/projects`, per-project sessions,
// per-session history/turn/stop, Status, and the token-entry gate.
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

const sessionRows = [
  {
    sid: "s1",
    project: "dev-team",
    role: "reviewer",
    vendor: "claude",
    permission_mode: "skip",
    current: true,
    status: "live",
    last_activity_seconds: 12,
  },
];

const sessionHistory = {
  sid: "s1",
  events: [
    {
      turn_id: "t1",
      ts: "2026-06-08T00:01:00Z",
      role: "reviewer",
      user: "hello",
      assistant: "hi from history",
    },
  ],
};

const statusSnapshot = {
  daemon_healthy: true,
  sessions_live: 1,
  sessions_idle: 0,
  cost_24h_usd: 0.42,
  cost_24h_by_vendor: { claude: 0.42 },
  budget_cap_24h: 3,
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
  await page.route("**/api/v1/status", (route) =>
    route.fulfill({ status: 200, json: statusSnapshot }),
  );
  await page.route("**/api/v1/projects", (route) =>
    route.fulfill({ status: 200, json: dashboardRows }),
  );
  await page.route("**/api/v1/projects/dev-team/sessions", (route) =>
    route.fulfill({ status: 200, json: sessionRows }),
  );
  await page.route("**/api/v1/sessions/s1", (route) =>
    route.fulfill({ status: 200, json: sessionHistory }),
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

  await page.route("**/api/v1/sessions/s1/turn", captureJson);
  await page.route("**/api/v1/sessions/s1/stop", captureJson);

  return captured;
}

test("chat shell lists projects and opens a sid-scoped session", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.goto("/app/");

  await expect(page.getByText("ccteam").first()).toBeVisible();
  await expect(page.getByText("dev-team").first()).toBeVisible();
  await expect(page.getByText("reviewer").first()).toBeVisible();
  await expect(page.getByText("s1").first()).toBeVisible();

  await page.getByRole("button", { name: /claude.*reviewer.*s1/ }).click();
  await expect(page).toHaveURL(/\/app\/chat\/s\/s1$/);
  await expect(
    page.getByText("hi from history"),
  ).toBeVisible();
});

test("session view posts turn and stop through the sid resource API", async ({
  page,
}) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/chat/s/s1");

  await page.getByPlaceholder("发消息 / 命令(/compact /clear …)…").fill("ship gate note");
  await page.getByTitle("发送").click();
  await expect
    .poll(() => captured.some((r) => r.url.endsWith("/api/v1/sessions/s1/turn")))
    .toBe(true);

  await page.getByRole("button", { name: /停止/ }).click();
  await expect
    .poll(() => captured.some((r) => r.url.endsWith("/api/v1/sessions/s1/stop")))
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

test("status route renders inside the unified shell", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.goto("/app/status");

  await expect(page.getByText("Status").first()).toBeVisible();
  await expect(page.getByTestId("status-sessions")).toBeVisible();
  await expect(page.getByText(/1 live · 0 idle/).first()).toBeVisible();
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
