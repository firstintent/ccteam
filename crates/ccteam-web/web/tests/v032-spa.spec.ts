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
    host: "local",
    host_online: true,
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

for (const width of [1360, 390, 320]) {
  test(`compact header, current context and Markdown reply copy at ${width}px`, async ({ page }, testInfo) => {
    await mockCcteamApi(page);
    await page.setViewportSize({ width, height: 1000 });
    await page.addInitScript(() => {
      Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
        writeText: async (value: string) => { (window as typeof window & { copiedReply?: string }).copiedReply = value; },
      } });
    });
    const raw = "## Answer\n\n**Keep Markdown** and [the link](https://example.com).\n\n```ts\nconst answer = 42;\n```\n\n| A | B |\n| - | - |\n| 1 | 2 |";
    const facts = { ...sessionRows[0], role: "", vendor: "codex", title: "一个很长的会话标题用于验证窄屏省略与工具栏布局", model: "gpt-test", host: "satellite-1", residency: "released", status: "idle", parent_sid: "s1497", delegation_depth: 2, cost_usd: 716.425, tokens_total: 9000000, last_active: "2026-11-01T22:00:00Z" };
    // The shell is deliberately stale/wrong: Context must use its own sid
    // response, including for the history-only path after a release.
    await page.route("**/api/v1/projects/dev-team/sessions", route => route.fulfill({ json: [] }));
    await page.route("**/api/v1/projects/dev-team/sessions/history", route => route.fulfill({ json: [{...facts,slug:"dev-team",host:"local",parent_sid:null,delegation_depth:0}] }));
    await page.route("**/api/v1/sessions/s1", route => route.fulfill({ json: { sid:"s1",events:[
      {turn_id:"t1",ts:"2026-11-02T01:00:00Z",role:"",user:"first",assistant:raw},
      {turn_id:"t2",ts:"2026-11-02T02:00:00Z",role:"",user:"second",assistant:"Second **response**"},
    ] } }));
    await page.route("**/api/v1/sessions/s1/status", route => route.fulfill({ json: {sid:"s1",model:"gpt-test",context:{used_tokens:8000,window_tokens:100000,pct:8},status_line:null} }));
    let refreshes = 0;
    await page.route("**/api/v1/sessions/s1/context", route => {
      refreshes += 1;
      return route.fulfill({ json: {sid:"s1",live:false,session:{...facts,cost_usd:716.425+refreshes-1},breakdown:{vendor:"codex",model:"gpt-test",captured_at:"2026-11-02T01:00:00Z",used_tokens:8000,window_tokens:100000,categories:[{id:"context_total",name:"Used context",tokens:8000,kind:"used"},{id:"free_space",name:"Free space",tokens:92000,kind:"free"}],last_request:{input_tokens:6000,cache_read_tokens:1000,cache_creation_tokens:0,output_tokens:1000,reasoning_output_tokens:200}}} });
    });
    await page.goto("/app/chat/s/s1");
    const head = page.locator(".conv-head");
    await expect(head.locator(".tabs")).toHaveCount(0);
    await expect(head.locator(".cost-pill")).toHaveCount(0);
    await expect(head.getByTestId("session-role")).toHaveCount(0);
    await expect(head).not.toContainText("无 role");
    await expect(page.getByTestId("ctx-toggle")).toContainText("8%");
    await expect(page.getByTestId("files-toggle")).toBeVisible();
    await expect(page.getByTestId("ctx-toggle")).toBeVisible();
    expect(await head.evaluate(node => node.scrollWidth <= node.clientWidth)).toBe(true);
    expect(await page.getByTestId("ctx-toggle").evaluate(node => node.getBoundingClientRect().height)).toBeLessThanOrEqual(44);
    expect(await page.locator(".right-panel-tools").evaluate(node => node.getBoundingClientRect().width)).toBeGreaterThan(150);
    const copies = page.getByTestId("reply-copy");
    await expect(copies).toHaveCount(2);
    await copies.first().click();
    expect(await page.evaluate(() => (window as typeof window & { copiedReply?: string }).copiedReply)).toBe(raw);
    await expect(page.locator(".reply-copy-feedback").first()).toHaveText(/已复制|Copied/);
    await copies.last().click();
    expect(await page.evaluate(() => (window as typeof window & { copiedReply?: string }).copiedReply)).toBe("Second **response**");
    await page.evaluate(() => {
      Object.defineProperty(navigator, "clipboard", {configurable:true,value:{writeText:async()=>{throw Error("denied");}}});
      document.execCommand = () => false;
    });
    await copies.last().click();
    await expect(page.locator(".reply-copy-feedback").last()).toHaveText(/复制失败|Copy failed/);
    await page.getByTestId("ctx-toggle").click();
    const status = page.getByTestId("ctx-session-status");
    await expect(status).toContainText("satellite-1");
    await expect(status).toContainText("s1497");
    await expect(status).toContainText("$716.4250");
    await expect(page.getByTestId("ctx-codex-scope")).toBeVisible();
    await expect(page.getByTestId("ctx-last-request")).toContainText("200");
    await page.getByTestId("ctx-refresh").click();
    await expect(status).toContainText("$717.4250");
    await page.screenshot({path:testInfo.outputPath(`context-${width}.png`),fullPage:true});
    await page.getByTestId("ctx-close").click();
    await expect(page.getByTestId("ctx-toggle")).toBeFocused();
  });
}

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
    route.fulfill({
      status: 200,
      json: { id: "admin", handle: "owner", is_admin: true },
    }),
  );
  await page.route("**/api/v1/status", (route) =>
    route.fulfill({ status: 200, json: statusSnapshot }),
  );
  await page.route("**/api/v1/hosts", (route) =>
    route.fulfill({
      status: 200,
      json: {
        hosts: [
          {
            host: "local",
            hostname: "dev01",
            is_local: true,
            agent_count: 5,
            agents_ready: 5,
          },
        ],
      },
    }),
  );
  await page.route("**/api/v1/hosts/join-token", (route) =>
    route.fulfill({
      status: 200,
      json: {
        token: "e2e-join-token",
        command:
          "ccteam host join --daemon <daemon-url> --token e2e-join-token",
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
            tool_surface: "native_mcp_config",
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
            tool_surface: "native_mcp_config",
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
            tool_surface: "native_mcp_config",
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
            tool_surface: "native_mcp_config",
            status: "ready",
            hint: null,
          },
          {
            vendor: "pi",
            harness_id: "pi",
            installed: true,
            version: "0.83.0",
            bin: "pi",
            mcp_registered: false,
            tool_surface: "managed_session_bridge",
            tool_surface_note:
              "Managed Pi sessions get the ccteam bridge; a plain `pi` started in a shell does not.",
            status: "ready",
            hint: null,
          },
        ],
      },
    }),
  );
  await page.route("**/api/v1/models", (route) =>
    route.fulfill({
      status: 200,
      json: {
        vendors: [
          {
            vendor: "pi",
            source: "Pi RPC get_available_models",
            models: [
              {
                id: "anthropic/claude-opus-4-6",
                display_name: "Claude Opus 4.6",
                efforts: ["off", "minimal", "low", "medium", "high", "xhigh"],
              },
              {
                id: "anthropic/claude-sonnet-4-6",
                display_name: "Claude Sonnet 4.6",
                efforts: [],
              },
            ],
            efforts: ["off", "minimal", "low", "medium", "high", "xhigh"],
          },
        ],
      },
    }),
  );
  await page.route("**/api/v1/projects", (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({
        status: 201,
        json: { slug: "new-proj", path: "/tmp/new-proj" },
      });
    }
    return route.fulfill({ status: 200, json: dashboardRows });
  });
  await page.route("**/api/v1/projects/dev-team/roles", (route) =>
    route.fulfill({
      status: 200,
      json: [{ role: "cto", description: "默认管家", model: "" }],
    }),
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
      json: {
        sid: "s1",
        model: null,
        effort: null,
        context: null,
        status_line: null,
      },
    }),
  );

  async function captureJson(
    route: Parameters<Page["route"]>[1] extends (route: infer R) => unknown
      ? R
      : never,
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

test("conversation posts a turn; sidebar hover-stop stops the session", async ({
  page,
}) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/chat/s/s1");

  await page.getByTestId("composer-textarea").fill("ship gate note");
  await page.getByTestId("composer-send").click();
  await expect
    .poll(() =>
      captured.some((r) => r.url.endsWith("/api/v1/sessions/s1/turn")),
    )
    .toBe(true);

  // The sidebar row's hover stop (prototype `.srow .stop`) → POST /stop.
  await page.getByText("评审登录页").first().hover();
  await page
    .getByRole("button", { name: "停止(状态保留,可 resume) s1", exact: true })
    .click();
  await expect
    .poll(() =>
      captured.some((r) => r.url.endsWith("/api/v1/sessions/s1/stop")),
    )
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

for (const state of ["missing", "not_granted", "installed"] as const) {
  test(`optional cswap account picker beside the model menu: ${state}`, async ({ page }) => {
    const captured = await mockCcteamApi(page);
    let discoveries = 0;
    const account = "a".repeat(64);
    await page.route("**/api/v1/vendors/claude/accounts", route => {
      discoveries++;
      return route.fulfill({ json: state === "installed"
        ? { available: true, accounts: [{ id: account, slot: "2", label: "work (t***@example.test)", active: false }] }
        : state === "not_granted"
          ? { available: false, reason: "not_granted", accounts: [] }
          : { available: false, accounts: [] } });
    });
    await page.goto("/app/");
    await expect(page.getByTestId("home-view")).toBeVisible();
    // The account control is its own composer button, not a model-menu section.
    await page.getByTestId("model-btn").click();
    await expect(page.getByTestId("model-menu")).toBeVisible();
    await expect(page.getByTestId("model-menu")).not.toContainText("Claude 账号");
    await expect(page.getByTestId("model-menu")).not.toContainText("高级选项");
    expect(discoveries).toBe(0);
    const accountBtn = page.getByTestId("claude-account-btn");
    await expect(accountBtn).toContainText("默认登录");
    await accountBtn.click();
    await expect(page.getByTestId("model-menu")).toBeHidden();
    const menu = page.getByTestId("claude-account-menu");
    await expect(menu).toBeVisible();
    await expect.poll(() => discoveries).toBe(1);
    if (state === "installed") {
      await menu.getByRole("menuitemradio", { name: /cswap 2/ }).click();
      await expect(menu).toBeHidden();
      await expect(accountBtn).toContainText("cswap 2");
    } else {
      await expect(menu).toContainText(state === "not_granted" ? "管理员尚未给你授权" : "cswap 不可用");
      await expect(menu.getByRole("menuitemradio", { name: "默认登录" })).toHaveAttribute("aria-checked", "true");
      await accountBtn.click();
    }
    await page.getByTestId("composer-textarea").fill("Use the selected login");
    await page.getByTestId("home-send").click();
    await expect.poll(() => captured.some(row => row.url.endsWith("/projects/dev-team/sessions"))).toBe(true);
    const create = captured.find(row => row.url.endsWith("/projects/dev-team/sessions"));
    if (state === "installed") expect(create?.body).toHaveProperty("claude_account", account);
    else expect(create?.body).not.toHaveProperty("claude_account");
    await expect(page).toHaveURL(/\/app\/chat\/s\/s2$/);
  });
}

test("cswap account control fits the composer row on a phone", async ({ page }) => {
  await mockCcteamApi(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/");
  const accountBtn = page.getByTestId("claude-account-btn");
  await expect(accountBtn).toBeVisible();
  const send = await page.getByTestId("home-send").boundingBox();
  expect(send).not.toBeNull();
  expect(send!.x + send!.width).toBeLessThanOrEqual(390);
});

test("admin grants a user cswap accounts from the users table", async ({ page }) => {
  await mockCcteamApi(page);
  const first = "a".repeat(64);
  const second = "b".repeat(64);
  const stale = "c".repeat(64);
  let saved: unknown = null;
  await page.route("**/api/v1/users", route => route.fulfill({ json: [
    { id: "u1", handle: "rob", linked_chat: null, created_at: "2026-11-06T00:00:00Z" },
  ] }));
  await page.route("**/api/v1/vendors/claude/accounts", route => route.fulfill({ json: {
    available: true, accounts: [
      { id: first, slot: "1", label: "home (h***@example.test)", active: true },
      { id: second, slot: "2", label: "work (w***@example.test)", active: false },
    ],
  } }));
  await page.route("**/api/v1/users/u1/cswap-accounts", route => {
    if (route.request().method() === "PUT") {
      saved = route.request().postDataJSON();
      return route.fulfill({ json: saved });
    }
    return route.fulfill({ json: { accounts: [first, stale] } });
  });
  await page.goto("/app/settings/admin");
  await page.getByTestId("settings-user-cswap-u1").click();
  const editor = page.getByTestId("settings-user-cswap-editor-u1");
  await expect(editor.getByRole("checkbox", { name: /cswap 1/ })).toBeChecked();
  await expect(editor.getByRole("checkbox", { name: /cswap 2/ })).not.toBeChecked();
  await expect(editor).toContainText("cccccccccccc…");
  await editor.getByRole("checkbox", { name: /cswap 2/ }).check();
  await editor.getByRole("checkbox", { name: /cccccccccccc/ }).uncheck();
  await page.getByTestId("settings-user-cswap-save-u1").click();
  await expect.poll(() => saved).toEqual({ accounts: [first, second] });
  await expect(editor).toBeHidden();
});

test("home lazy-create: first message POSTs session without a host override, then the turn", async ({
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
        (r) =>
          r.url.endsWith("/api/v1/projects/dev-team/sessions") &&
          r.method === "POST",
      ),
    )
    .toBe(true);
  const create = captured.find((r) =>
    r.url.endsWith("/projects/dev-team/sessions"),
  );
  expect(create?.body).toMatchObject({
    vendor: "claude",
    protocol: "stream-json",
    permission_mode: "hitl",
  });
  expect(create?.body).not.toHaveProperty("host");

  // The first message rides as the new session's first turn → Conversation.
  await expect
    .poll(() =>
      captured.some((r) => r.url.endsWith("/api/v1/sessions/s2/turn")),
    )
    .toBe(true);
  const turn = captured.find((r) => r.url.endsWith("/api/v1/sessions/s2/turn"));
  expect(turn?.body).toEqual({ text: "修复登录页布局" });
  await expect(page).toHaveURL(/\/app\/chat\/s\/s2$/);
});

test("Pi picker keeps role, uses provider/model-id, and exposes strict-HITL trade-off", async ({
  page,
}) => {
  const captured = await mockCcteamApi(page);
  await page.goto("/app/");

  await page.getByTestId("ctx-role").getByRole("button").first().click();
  await page.getByRole("button", { name: /cto/ }).click();

  await page.getByTestId("model-btn").click();
  await page
    .getByRole("button", { name: /anthropic\/claude-opus-4-6/ })
    .click();
  await expect(page.getByTestId("model-btn")).toContainText(
    "Pi · anthropic/claude-opus-4-6",
  );
  await page.getByTestId("model-btn").click();
  await page.getByRole("button", { name: "xhigh", exact: true }).click();

  await page.getByTestId("hitl-toggle").click();
  await expect(page.getByTestId("pi-hitl-tradeoff")).toContainText("用户扩展");
  await expect(page.getByTestId("pi-hitl-tradeoff")).toContainText(
    "技能、上下文文件和提示词模板不受影响",
  );

  await page.getByTestId("composer-textarea").fill("验证 Pi 配方");
  await page.getByTestId("home-send").click();
  await expect
    .poll(
      () =>
        captured.find((request) =>
          request.url.endsWith("/projects/dev-team/sessions"),
        )?.body,
    )
    .toMatchObject({
      role: "cto",
      vendor: "pi",
      protocol: "stream-json",
      model: "anthropic/claude-opus-4-6",
      effort: "xhigh",
      permission_mode: "hitl",
    });
});

test("工作流 and 设置 render as set-nav views; ops merges Status + Hosts responsively", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.goto("/app/flow");
  await expect(page.getByTestId("workflow-view")).toBeVisible();
  await expect(page.getByTestId("workflow-tab-evolution")).toBeVisible();

  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/app/settings/status");
  await expect(page).toHaveURL(/\/app\/settings\/ops$/);
  await expect(page.getByTestId("settings-view")).toBeVisible();
  await expect(page.getByTestId("ops-view")).toBeVisible();
  await expect(page.getByTestId("status-view")).toBeVisible();
  await expect(page.getByTestId("hosts-view")).toBeVisible();
  await expect(page.getByText(/1 .*live/).first()).toBeVisible();
  await expect(page.getByTestId("ops-view")).toHaveCSS(
    "flex-direction",
    "column",
  );

  // Both legacy settings and flat routes canonicalize to the merged tab.
  await page.goto("/app/settings/hosts");
  await expect(page).toHaveURL(/\/app\/settings\/ops$/);
  await page.goto("/app/status");
  await expect(page).toHaveURL(/\/app\/settings\/ops$/);
  await page.goto("/app/hosts");
  await expect(page).toHaveURL(/\/app\/settings\/ops$/);

  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByTestId("ops-view")).toHaveCSS(
    "flex-direction",
    "column",
  );
  await expect
    .poll(() =>
      page
        .getByTestId("ops-view")
        .evaluate((node) => node.getBoundingClientRect().right),
    )
    .toBeLessThanOrEqual(391);
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
  await page
    .getByTestId("side-backdrop")
    .click({ position: { x: 350, y: 400 } });
  await expect(sidebar).not.toBeInViewport();
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
  await page.route("**/api/v1/me", (route) =>
    route.fulfill({ status: 401, json: { error: "auth required" } }),
  );
  await page.route("**/sse/**", (route) =>
    route.fulfill({ status: 200, body: "\n" }),
  );

  await page.goto("/app/");
  await expect(page.getByLabel("访问令牌")).toBeVisible();
  await page.getByLabel("访问令牌").fill("abc123");
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(page).toHaveURL(/\/\?token=ccteam%3Aabc123$/);
});

test("fresh unauthenticated load surfaces the token entry page, not a wedged shell", async ({
  page,
}) => {
  // Auth-enabled server with no session: every /api/* call 401s — INCLUDING the
  // `/api/v1/auth/token` bootstrap probe, which never reaches its handler
  // because the server's auth gate rejects it first (so no `wire_token` comes
  // back). The gate must still open on the observed 401; the regression was it
  // stayed shut because `authRequired` was derived only from `wire_token`.
  await page.route("**/api/**", (route) =>
    route.fulfill({ status: 401, body: "auth required" }),
  );
  await page.route("**/sse/**", (route) =>
    route.fulfill({ status: 200, body: "\n" }),
  );

  await page.goto("/app/");
  await expect(page.getByLabel("访问令牌")).toBeVisible();
});

test("file workspace preserves Context, refreshes paths, and previews HTML", async ({
  page,
}) => {
  await mockCcteamApi(page);
  const paths = ["index.html", "README.md", "src/", "src/lib.rs"];
  await page.route("**/api/v1/projects/dev-team/files", (route) =>
    route.fulfill({
      json: {
        root: "/repo",
        paths,
        git_status: { "README.md": "modified" },
        truncated: false,
      },
    }),
  );
  await page.route("**/api/v1/projects/dev-team/file?*", (route) => {
    const path = new URL(route.request().url()).searchParams.get("path");
    return route.fulfill({
      json: {
        path,
        content:
          path === "README.md"
            ? "# Project\nRead me"
            : "<h1>Hello preview</h1>",
        size: 30,
        binary: false,
      },
    });
  });
  await page.route("**/api/v1/projects/dev-team/files/preview", (route) =>
    route.fulfill({
      json: {
        url: "/api/v1/projects/dev-team/files/preview/test/index.html",
        expires_in: 3600,
      },
    }),
  );
  await page.route("**/files/preview/test/index.html", (route) =>
    route.fulfill({
      contentType: "text/html",
      headers: { "content-security-policy": "sandbox allow-scripts" },
      body: "<h1>Hello preview</h1><button onclick=\"this.textContent='Clicked'\">Click me</button>",
    }),
  );
  await page.route("**/api/v1/sessions/s1/context", (route) =>
    route.fulfill({ json: { sid: "s1", live: false, breakdown: null } }),
  );
  await page.goto("/app/chat/s/s1");
  await page.getByTestId("files-toggle").click();
  await expect(page.getByTestId("ctx-toggle")).toBeVisible();
  await page
    .getByTestId("project-file-tree")
    .getByRole("treeitem", { name: "index.html", exact: true })
    .click();
  await expect(
    page
      .frameLocator("iframe.files-preview")
      .getByRole("heading", { name: "Hello preview" }),
  ).toBeVisible();
  await page
    .frameLocator("iframe.files-preview")
    .getByRole("button", { name: "Click me" })
    .click();
  await expect(
    page
      .frameLocator("iframe.files-preview")
      .getByRole("button", { name: "Clicked" }),
  ).toBeVisible();
  await page.getByTestId("ctx-toggle").click();
  await expect(page.getByTestId("context-panel")).toBeVisible();
  await expect(page.getByTestId("file-workspace")).not.toBeVisible();
  await page.getByTestId("files-toggle").click();
  await expect(page.getByRole("tab", { name: "index.html" })).toBeVisible();
  await page.getByRole("button", { name: /^(源码|Source)$/ }).click();
  await expect(page.locator(".cm-content")).toContainText("Hello preview");
  paths.push("new.txt");
  await page.getByRole("button", { name: /刷新文件|Refresh files/ }).click();
  await expect(
    page
      .getByTestId("project-file-tree")
      .getByRole("treeitem", { name: "new.txt", exact: true }),
  ).toBeVisible();
  await expect(page.locator("iframe.files-preview")).toHaveCount(0);
  await expect(page.locator(".cm-content")).toContainText("Hello preview");
  await page
    .getByTestId("project-file-tree")
    .getByRole("treeitem", { name: "README.md", exact: true })
    .dblclick();
  await page.getByRole("button", { name: /^(源码|Source)$/ }).click();
  await expect(page.locator(".cm-content")).toContainText("Read me");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("file-workspace")).not.toBeVisible();
  await expect(page.getByTestId("files-toggle")).toBeFocused();
});

test.describe("touch workspace", () => {
  test.use({ hasTouch: true, isMobile: true });

  async function openWorkspace(page: Page, width: number, height: number) {
    await mockCcteamApi(page);
    await page.setViewportSize({ width, height });
    await page.route("**/api/v1/projects/dev-team/files", route => route.fulfill({json:{root:"/repo",paths:["mobile.txt"],git_status:{},truncated:false}}));
    await page.route("**/api/v1/projects/dev-team/file?*", route => route.fulfill({json:{path:"mobile.txt",content:"Keep this reading position",size:26,binary:false}}));
    await page.route("**/api/v1/sessions/s1/context", route => route.fulfill({json:{sid:"s1",live:false,breakdown:null}}));
    await page.goto("/app/chat/s/s1");
    await page.getByTestId("files-toggle").tap();
    await expect(page.getByTestId("file-workspace")).toBeVisible();
  }

  async function expectToolbarClickable(page: Page) {
    for (const id of ["files-toggle", "ctx-toggle"]) {
      await expect(page.getByTestId(id)).toBeInViewport();
      expect(await page.getByTestId(id).evaluate(element => {
        const bounds = element.getBoundingClientRect();
        return document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2)?.closest("button") === element;
      })).toBe(true);
    }
  }

  for (const [width, height] of [[320,720],[390,844],[768,1024],[820,1180],[834,1194],[1024,768],[1180,820],[1440,900]]) {
    test(`single taps open, close and switch panels at ${width}x${height}`, async ({ page }, testInfo) => {
      await openWorkspace(page, width, height);
      await expectToolbarClickable(page);
      await page.getByTestId("files-toggle").tap();
      await expect(page.getByTestId("file-workspace")).not.toBeVisible();
      await page.getByTestId("files-toggle").tap();
      await expect(page.getByTestId("file-workspace")).toBeVisible();
      await page.getByTestId("project-file-tree").getByRole("treeitem", {name:"mobile.txt",exact:true}).tap();
      await expect(page.locator(".cm-content")).toContainText("Keep this reading position");
      const before = await page.getByTestId("file-workspace").boundingBox();
      await page.getByRole("button", {name:/放大工作区|Expand workspace/}).tap();
      const expanded = await page.getByTestId("file-workspace").boundingBox();
      const body = await page.locator(".conv-body").boundingBox();
      expect(expanded!.width).toBeGreaterThan(before!.width + 2);
      expect(Math.abs(expanded!.width - body!.width)).toBeLessThan(2);
      await expectToolbarClickable(page);
      await page.screenshot({path:testInfo.outputPath(`files-expanded-${width}x${height}.png`)});
      await page.getByRole("button", {name:/恢复并排布局|Restore split view/}).tap();
      await page.getByTestId("ctx-toggle").tap();
      await expect(page.getByTestId("context-panel")).toBeVisible();
      await expect(page.getByTestId("file-workspace")).not.toBeVisible();
      await expectToolbarClickable(page);
      await page.screenshot({path:testInfo.outputPath(`workspace-${width}x${height}.png`)});
      await page.getByTestId("ctx-toggle").tap();
      await expect(page.getByTestId("context-panel")).not.toBeVisible();
      await page.getByTestId("files-toggle").tap();
      await expect(page.locator(".cm-content")).toContainText("Keep this reading position");
      await page.getByRole("button", {name:/关闭文件面板|Close file panel/}).last().tap();
      await expect(page.getByTestId("file-workspace")).not.toBeVisible();
      await expect(page.getByTestId("files-toggle")).toBeFocused();
    });
  }

  test("expanded tablet workspace stays operable through rotation", async ({page}) => {
    await openWorkspace(page, 1180, 820);
    await page.getByRole("button", {name:/放大工作区|Expand workspace/}).tap();
    await page.setViewportSize({width:820,height:1180});
    await expectToolbarClickable(page);
    await page.getByRole("button", {name:/恢复并排布局|Restore split view/}).tap();
    await page.getByTestId("ctx-toggle").tap();
    await expect(page.getByTestId("context-panel")).toBeVisible();
    await page.setViewportSize({width:1180,height:820});
    await expectToolbarClickable(page);
    await page.getByTestId("files-toggle").tap();
    await expect(page.getByTestId("file-workspace")).toBeVisible();
  });

  test("saved desktop layout survives narrow drawers, backdrop taps and rotation", async ({page}) => {
    const layout = JSON.stringify({"session-chat":60,"session-right-workspace":40});
    await page.addInitScript(value => {
      localStorage.setItem("ccteam.workspace.layout", value);
      localStorage.setItem("ccteam.files.open", "1");
    }, layout);
    // A remembered desktop panel must still open on the first phone tap.
    await openWorkspace(page, 390, 844);
    const savedLayout = () => page.evaluate(() => localStorage.getItem("ccteam.workspace.layout"));
    expect(await savedLayout()).toBe(layout);
    await page.locator(".files-backdrop").tap({position:{x:1,y:100}});
    await expect(page.getByTestId("file-workspace")).not.toBeVisible();
    await expect(page.getByTestId("files-toggle")).toBeFocused();
    await page.getByTestId("ctx-toggle").tap();
    await page.getByTestId("ctx-backdrop").tap({position:{x:1,y:100}});
    await expect(page.getByTestId("context-panel")).not.toBeVisible();
    await expect(page.getByTestId("ctx-toggle")).toBeFocused();
    await page.getByTestId("files-toggle").tap();
    await page.setViewportSize({width:834,height:1194});
    await expectToolbarClickable(page);
    expect(await savedLayout()).toBe(layout);
    await page.setViewportSize({width:390,height:844});
    await expectToolbarClickable(page);
    await page.getByTestId("files-toggle").tap();
    await expect(page.getByTestId("file-workspace")).not.toBeVisible();
    expect(await savedLayout()).toBe(layout);
  });
});

test("mobile Files drawer accepts clicks above its backdrop", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.route("**/api/v1/projects/dev-team/files", (route) =>
    route.fulfill({
      json: {
        root: "/repo",
        paths: ["mobile.txt"],
        git_status: {},
        truncated: false,
      },
    }),
  );
  await page.route("**/api/v1/projects/dev-team/file?*", (route) =>
    route.fulfill({
      json: {
        path: "mobile.txt",
        content: "Mobile source",
        size: 13,
        binary: false,
      },
    }),
  );
  await page.goto("/app/chat/s/s1");
  await page.getByTestId("files-toggle").click();
  await page
    .getByTestId("project-file-tree")
    .getByRole("treeitem", { name: "mobile.txt", exact: true })
    .click();
  await expect(page.locator(".cm-content")).toContainText("Mobile source");
  await page
    .getByRole("button", { name: /关闭文件面板|Close file panel/ })
    .last()
    .click();
  await expect(page.getByTestId("file-workspace")).not.toBeVisible();
});

test("file workspace follows repeated line links into the same file", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.route("**/api/v1/sessions/s1", (route) =>
    route.fulfill({
      json: {
        sid: "s1",
        events: [
          {
            turn_id: "links",
            role: "reviewer",
            user: "inspect",
            assistant:
              "[Lower](/repo/src/lib.rs:130) [Upper](/repo/src/lib.rs:10)",
          },
        ],
      },
    }),
  );
  await page.route("**/api/v1/projects/dev-team/files", (route) =>
    route.fulfill({
      json: {
        root: "/repo",
        paths: ["other.txt", "src/", "src/lib.rs"],
        git_status: {},
        truncated: false,
      },
    }),
  );
  await page.route("**/api/v1/projects/dev-team/file?*", (route) => {
    const path = new URL(route.request().url()).searchParams.get("path");
    return route.fulfill({
      json: {
        path,
        content:
          path === "other.txt"
            ? "Other file"
            : Array.from({ length: 160 }, (_, i) => `// line ${i + 1}`).join(
                "\n",
              ),
        size: 2000,
        binary: false,
      },
    });
  });
  await page.goto("/app/chat/s/s1");
  await page.getByRole("link", { name: "Lower", exact: true }).click();
  await expect(page.locator(".cm-content")).toBeVisible();
  await expect(
    page
      .getByTestId("project-file-tree")
      .getByRole("treeitem", { name: "lib.rs", exact: true }),
  ).toBeVisible();
  await expect
    .poll(() => page.locator(".cm-scroller").evaluate((el) => el.scrollTop))
    .toBeGreaterThan(1000);
  await page.getByRole("link", { name: "Upper", exact: true }).click();
  await expect
    .poll(() => page.locator(".cm-scroller").evaluate((el) => el.scrollTop))
    .toBeLessThan(500);
  await expect(
    page.getByRole("tab", { name: "lib.rs", exact: true }),
  ).toHaveCount(1);
  await page.locator(".cm-scroller").hover();
  await page.mouse.wheel(0, 64);
  await expect
    .poll(() => page.locator(".cm-scroller").evaluate((el) => el.scrollTop))
    .toBeGreaterThan(20);
  const readingPosition = await page
    .locator(".cm-scroller")
    .evaluate((el) => el.scrollTop);
  await page
    .getByTestId("project-file-tree")
    .getByRole("treeitem", { name: "other.txt", exact: true })
    .click();
  await expect(page.locator(".cm-content")).toContainText("Other file");
  await page.getByRole("tab", { name: "lib.rs", exact: true }).click();
  await expect
    .poll(() => page.locator(".cm-scroller").evaluate((el) => el.scrollTop))
    .toBeGreaterThan(readingPosition - 5);
  await expect
    .poll(() => page.locator(".cm-scroller").evaluate((el) => el.scrollTop))
    .toBeLessThan(readingPosition + 5);
});

test.describe("touch workspace chat links and code copy", () => {
  test.use({ hasTouch: true });
  for (const width of [390, 1360]) {
    test(`document previews and per-block copy at ${width}px`, async ({ page }) => {
      await mockCcteamApi(page);
      await page.setViewportSize({ width, height: 1000 });
      await page.addInitScript(() => {
        Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
          writeText: async (text: string) => { (window as typeof window & { copiedCode?: string }).copiedCode = text; },
        } });
      });
      const origin = new URL(page.url() === "about:blank" ? test.info().project.use.baseURL! : page.url()).origin;
      await page.route("**/api/v1/sessions/s1", route => route.fulfill({ json: {
        sid: "s1", events: [{ turn_id: "links-copy", role: "reviewer", user: "inspect", assistant:
          `[issue](/repo/docs/issue.md) [same site](${origin}/repo/docs/issue.md) [external](https://elsewhere.example/issue.md)\n\n` +
          '```ts\nconst answer = "<hello>";\n```\n\n```\nsecond block\n```',
        }],
      } }));
      await page.route("**/api/v1/projects/dev-team/files", route => route.fulfill({ json: {
        root: "/repo", paths: ["docs/", "docs/issue.md"], git_status: {}, truncated: false,
      } }));
      let minted = 0;
      await page.route("**/api/v1/projects/dev-team/files/preview", route => {
        expect(route.request().postDataJSON()).toEqual({ path: "docs/issue.md" });
        return route.fulfill({ json: {
          url: `/api/v1/projects/dev-team/files/preview/ticket-${++minted}/docs/issue.md`, expires_in: 3600,
        } });
      });
      await page.context().route("**/files/preview/ticket-*/docs/issue.md", route => route.fulfill({
        contentType: "text/html", body: "<!doctype html><h1>Issue preview</h1>",
      }));
      await page.goto("/app/chat/s/s1");
      const expected = "/api/v1/projects/dev-team/files/preview/ticket-1/docs/issue.md";
      await expect(page.getByRole("link", { name: "issue", exact: true })).toHaveAttribute("href", expected);
      await expect(page.getByRole("link", { name: "same site", exact: true })).toHaveAttribute("href", expected);
      await expect(page.getByRole("link", { name: "external", exact: true })).toHaveAttribute("href", "https://elsewhere.example/issue.md");
      expect(minted).toBe(1);
      const popupEvent = page.waitForEvent("popup");
      await page.getByRole("link", { name: "same site", exact: true }).tap();
      const popup = await popupEvent;
      await expect(popup.getByRole("heading", { name: "Issue preview" })).toBeVisible();
      expect(new URL(popup.url()).origin).toBe(origin);
      await popup.close();
      const buttons = page.locator("pre .code-copy");
      await expect(buttons).toHaveCount(2);
      await expect(buttons.first().locator("svg")).toBeVisible();
      await expect(buttons.first()).toHaveCSS("opacity", "1");
      await buttons.first().tap();
      await expect(buttons.first()).toHaveAttribute("aria-label", "已复制");
      expect(await page.evaluate(() => (window as typeof window & { copiedCode?: string }).copiedCode)).toBe('const answer = "<hello>";\n');
      await buttons.last().focus();
      await page.keyboard.press("Enter");
      expect(await page.evaluate(() => (window as typeof window & { copiedCode?: string }).copiedCode)).toBe("second block\n");
    });
  }
});

test("file workspace previews Markdown and opens the rendered document in a new page", async ({
  page,
}) => {
  await mockCcteamApi(page);
  await page.route("**/api/v1/projects/dev-team/files", (route) =>
    route.fulfill({
      json: {
        root: "/repo",
        paths: ["README.md"],
        git_status: {},
        truncated: false,
      },
    }),
  );
  await page.route("**/api/v1/projects/dev-team/file?*", (route) =>
    route.fulfill({
      json: {
        path: "README.md",
        content: "# Markdown document\n\n| Name | Value |",
        size: 44,
        binary: false,
      },
    }),
  );
  await page.route("**/api/v1/projects/dev-team/files/preview", (route) =>
    route.fulfill({
      json: {
        url: "/api/v1/projects/dev-team/files/preview/markdown/README.md",
        expires_in: 3600,
      },
    }),
  );
  const markdownResponse = {
    contentType: "text/html",
    headers: { "content-security-policy": "sandbox" },
    body: "<!doctype html><title>README.md</title><h1>Markdown document</h1><table><tr><td>world</td></tr></table>",
  };
  await page.route("**/files/preview/markdown/README.md", (route) => route.fulfill(markdownResponse));
  // A popup's initial navigation belongs to the context, before its Page
  // exists. Page-scoped mocks only cover the parent iframe request.
  await page.context().route("**/files/preview/markdown/README.md", (route) => route.fulfill(markdownResponse));
  await page.goto("/app/chat/s/s1");
  await page.getByTestId("files-toggle").click();
  await page
    .getByTestId("project-file-tree")
    .getByRole("treeitem", { name: "README.md", exact: true })
    .click();
  await expect(
    page
      .frameLocator("iframe.files-preview")
      .getByRole("heading", { name: "Markdown document" }),
  ).toBeVisible();
  await page.getByRole("button", { name: /^(源码|Source)$/ }).click();
  await expect(page.locator(".cm-content")).toContainText(
    "# Markdown document",
  );
  await page.getByRole("button", { name: /^(预览|Preview)$/ }).click();
  const popup = page.waitForEvent("popup");
  await page
    .getByRole("link", { name: /新标签页打开|Open in new tab/ })
    .click();
  const standalone = await popup;
  await expect(
    standalone.getByRole("heading", { name: "Markdown document" }),
  ).toBeVisible();
  await expect(standalone).toHaveTitle("README.md");
});
