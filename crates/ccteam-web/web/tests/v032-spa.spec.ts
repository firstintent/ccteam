import { expect, test, type Page } from "@playwright/test";

// v0.8.24 Track A — browser smoke for the prototype shell:
//   `.app` = sidebar + main, four views (Home / Conversation / 工作流 / 设置),
//   Home lazy-create (session minted on the FIRST message), sidebar
//   project-grouped sessions with hover-stop, the ≤820px drawer, and the
//   token-entry gate. All backend traffic is mocked at the /api/v1 seam.

const dashboardRows = [
  {
    slug: "dev-team",
    path: "/home/u/dev-team",
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
    protocol: "stream-json",
    title: "评审登录页",
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
  sessions: [],
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
  await page.route("**/api/v1/me", (route) =>
    route.fulfill({ status: 200, json: { id: "admin", handle: "owner", is_admin: true } }),
  );
  await page.route("**/api/v1/status", (route) =>
    route.fulfill({ status: 200, json: statusSnapshot }),
  );
  await page.route("**/api/v1/hosts", (route) =>
    route.fulfill({
      status: 200,
      json: {
        hosts: [
          { host: "local", hostname: "dev01", is_local: true, agent_count: 4, agents_ready: 4 },
        ],
      },
    }),
  );
  await page.route("**/api/v1/hosts/join-token", (route) =>
    route.fulfill({
      status: 200,
      json: {
        token: "e2e-join-token",
        command: "ccteam host join --daemon <daemon-url> --token e2e-join-token",
      },
    }),
  );
  await page.route("**/api/v1/hosts/local*", (route) =>
    route.fulfill({
      status: 200,
      json: {
        host: "local",
        hostname: "dev01",
        is_local: true,
        os: "linux",
        arch: "x86_64",
        ccteam_version: "0.8.24",
        agents: [
          {
            vendor: "claude",
            harness_id: "claude-code",
            installed: true,
            version: "claude 2.0.35",
            bin: "~/.local/bin/claude",
            mcp_registered: true,
            mcp_registrable: true,
            status: "ready",
            hint: null,
          },
          {
            vendor: "codex",
            harness_id: "codex",
            installed: true,
            version: "codex 0.48.0",
            bin: "codex",
            mcp_registered: true,
            mcp_registrable: true,
            status: "ready",
            hint: null,
          },
          {
            vendor: "grok",
            harness_id: "grok",
            installed: true,
            version: "grok 0.2.93",
            bin: "grok",
            mcp_registered: false,
            mcp_registrable: false,
            status: "ready",
            hint: null,
          },
          {
            vendor: "opencode",
            harness_id: "opencode",
            installed: true,
            version: "opencode 0.6.4",
            bin: "opencode",
            mcp_registered: false,
            mcp_registrable: false,
            status: "ready",
            hint: null,
          },
        ],
      },
    }),
  );
  await page.route("**/api/v1/projects", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ status: 201, json: { slug: "new-proj", path: "/tmp/new-proj" } });
    }
    return route.fulfill({ status: 200, json: dashboardRows });
  });
  await page.route("**/api/v1/projects/dev-team/roles", (route) =>
    route.fulfill({ status: 200, json: [{ role: "cto", description: "默认管家", model: "" }] }),
  );
  await page.route("**/api/v1/projects/dev-team/sessions", (route) => {
    if (route.request().method() === "POST") {
      captured.push({
        url: route.request().url(),
        method: "POST",
        body: route.request().postDataJSON(),
      });
      return route.fulfill({ status: 201, json: { sid: "s2" } });
    }
    return route.fulfill({ status: 200, json: sessionRows });
  });
  await page.route("**/api/v1/projects/dev-team/sessions/history", (route) =>
    route.fulfill({ status: 200, json: [] }),
  );
  await page.route("**/api/v1/sessions/s1", (route) =>
    route.fulfill({ status: 200, json: sessionHistory }),
  );
  await page.route("**/api/v1/sessions/s2", (route) =>
    route.fulfill({ status: 200, json: { sid: "s2", events: [] } }),
  );
  await page.route("**/api/v1/sessions/*/status", (route) =>
    route.fulfill({
      status: 200,
      json: { sid: "s1", model: null, effort: null, context: null, status_line: null },
    }),
  );

  async function captureJson(
    route: Parameters<Page["route"]>[1] extends (route: infer R) => unknown ? R : never,
  ) {
    const req = route.request();
    captured.push({
      url: req.url(),
      method: req.method(),
      body: req.postDataJSON(),
    });
    await route.fulfill({ status: 200, json: { ok: true } });
  }

  await page.route("**/api/v1/sessions/s1/turn", captureJson);
  await page.route("**/api/v1/sessions/s2/turn", captureJson);
  await page.route("**/api/v1/sessions/s1/stop", captureJson);
  await page.route("**/api/v1/sessions/s1/interrupt", captureJson);

  return captured;
}

test("desktop shell: sidebar groups sessions by project and opens a sid conversation", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.goto("/app/");

  // Prototype shell chrome: sidebar brand + 新建会话 + 工作流 + 设置, no top bar.
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await expect(page.getByTestId("side-new")).toBeVisible();
  await expect(page.getByTestId("side-flow")).toBeVisible();
  await expect(page.getByTestId("side-settings")).toBeVisible();
  // Home landing is the default view.
  await expect(page.getByTestId("home-view")).toBeVisible();
  await expect(page.getByText("开工吧!")).toBeVisible();

  // The project group + its session row (title from the session-title system).
  await expect(page.getByText("dev-team").first()).toBeVisible();
  await page.getByText("评审登录页").first().click();

  await expect(page).toHaveURL(/\/app\/chat\/s\/s1$/);
  await expect(page.getByTestId("conversation-view")).toBeVisible();
  await expect(page.getByText("hi from history")).toBeVisible();
});

test("conversation posts a turn; sidebar hover-stop stops the session", async ({ page }) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/chat/s/s1");

  await page.getByTestId("composer-textarea").fill("ship gate note");
  await page.getByTestId("composer-send").click();
  await expect
    .poll(() => captured.some((r) => r.url.endsWith("/api/v1/sessions/s1/turn")))
    .toBe(true);

  // The sidebar row's hover stop (prototype `.srow .stop`) → POST /stop.
  await page.getByText("评审登录页").first().hover();
  await page.getByRole("button", { name: "停止(状态保留,可 resume) s1", exact: true }).click();
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

test("home lazy-create: first message POSTs session (vendor/protocol/hitl/host) then the turn", async ({
  page,
}) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/");

  await expect(page.getByTestId("home-view")).toBeVisible();
  // Arm HITL from the composer pill.
  await page.getByTestId("hitl-toggle").click();
  await page.getByTestId("composer-textarea").fill("修复登录页布局");
  await page.getByTestId("home-send").click();

  await expect
    .poll(() =>
      captured.some(
        (r) => r.url.endsWith("/api/v1/projects/dev-team/sessions") && r.method === "POST",
      ),
    )
    .toBe(true);
  const create = captured.find((r) => r.url.endsWith("/projects/dev-team/sessions"));
  expect(create?.body).toMatchObject({
    vendor: "claude",
    protocol: "stream-json",
    permission_mode: "hitl",
  });

  // The first message rides as the new session's first turn → Conversation.
  await expect
    .poll(() => captured.some((r) => r.url.endsWith("/api/v1/sessions/s2/turn")))
    .toBe(true);
  const turn = captured.find((r) => r.url.endsWith("/api/v1/sessions/s2/turn"));
  expect(turn?.body).toEqual({ text: "修复登录页布局" });
  await expect(page).toHaveURL(/\/app\/chat\/s\/s2$/);
});

test("工作流 and 设置 render as set-nav views; Status keeps its cards", async ({ page }) => {
  await mockCcteamApi(page);
  await page.goto("/app/flow");
  await expect(page.getByTestId("workflow-view")).toBeVisible();
  await expect(page.getByTestId("workflow-tab-compare")).toBeVisible();

  await page.goto("/app/settings/status");
  await expect(page.getByTestId("settings-view")).toBeVisible();
  await expect(page.getByTestId("status-view")).toBeVisible();
  await expect(page.getByText(/1 .*live/).first()).toBeVisible();

  // legacy flat route still lands on the new IA.
  await page.goto("/app/status");
  await expect(page).toHaveURL(/\/app\/settings\/status$/);
});

test("mobile ≤820px: sidebar is a drawer behind the hamburger with a backdrop", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/");

  // Drawer closed: the fixed sidebar sits off-canvas (translateX(-100%)).
  const sidebar = page.getByTestId("sidebar");
  await expect(page.getByTestId("hamb")).toBeVisible();
  await expect(sidebar).not.toBeInViewport();

  // Hamburger slides it in + shows the backdrop.
  await page.getByTestId("hamb").click();
  await expect(sidebar).toBeInViewport();
  await expect(page.getByTestId("side-backdrop")).toBeVisible();

  // Backdrop click closes it again.
  await page.getByTestId("side-backdrop").click({ position: { x: 350, y: 400 } });
  await expect(sidebar).not.toBeInViewport();
});

test("401 after auth-required bootstrap shows the token entry flow", async ({ page }) => {
  await page.route("**/api/v1/auth/token", (route) =>
    route.fulfill({ status: 200, json: { wire_token: "ccteam:deadbeef" } }),
  );
  await page.route("**/api/v1/projects", (route) =>
    route.fulfill({ status: 401, json: { error: "auth required" } }),
  );
  await page.route("**/api/v1/me", (route) =>
    route.fulfill({ status: 401, json: { error: "auth required" } }),
  );
  await page.route("**/sse/**", (route) => route.fulfill({ status: 200, body: "\n" }));

  await page.goto("/app/");
  await expect(page.getByLabel("Token or URL")).toBeVisible();
  await page.getByLabel("Token or URL").fill("abc123");
  await page.getByRole("button", { name: "Connect" }).click();
  await expect(page).toHaveURL(/\/\?token=ccteam%3Aabc123$/);
});
