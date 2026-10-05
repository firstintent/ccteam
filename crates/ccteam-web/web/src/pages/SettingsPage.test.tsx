// @vitest-environment jsdom
// v0.8.8 F4 — Settings section smoke tests.
//
// AccessView owns the admin's masked IM-status fetch and composes the named
// Telegram/Lark sections. User management stays on the 管理员 · Admin tab.
//
// Shape tests use React's `renderToString` to assert each named section's
// initial HTML, mirroring SessionsListPage.test.tsx. We assert:
//   - configured sections default to compact masked summaries
//   - unconfigured sections default to empty forms, and that the
//     masked status NEVER echoes a plaintext secret (red-line guard)
// Interactive paths (token save → chat_id poll loop, overwrite confirm) are
// covered by configApi.test.ts + manual / Playwright host E2E. The one
// interactive surface tested here is the require-@-mention switch (mounted in
// jsdom, fetch mocked): it is the same row on six cards against two APIs.

import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ImConfigStatus } from "../lib/configApi";
import { toastBus } from "../lib/toastBus";
import {
  LarkSection,
  MyImSection,
  SlackSection,
  TelegramSection,
  UserManagementSection,
} from "./SettingsPage";

describe("Settings sections", () => {
  it("TelegramSection (configured) defaults to its compact masked summary", () => {
    const html = renderToString(
      <TelegramSection
        status={{
          configured: true,
          bot_token_last4: "…wxyz",
          chat_id_count: 1,
          allowed_chat_ids: ["42"],
          require_mention: false,
        }}
        onSaved={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-telegram"');
    expect(html).toContain('data-testid="settings-telegram-summary"');
    expect(html).toContain("…wxyz");
    expect(html).toContain("bound chats");
    expect(html).toContain("重置");
    // Collapsed means no secret field exists until the operator explicitly edits.
    expect(html).not.toContain('type="password"');
    expect(html).not.toContain('value="…wxyz"');
    // A configured bot carries the group @-mention switch, off by default,
    // with the generic hint (the thread clause is Slack's alone).
    expect(html).toContain('data-testid="settings-telegram-require-mention"');
    expect(html).toContain('role="switch"');
    expect(html).toContain('aria-checked="false"');
    expect(html).toContain("群聊/频道里需要 @ 机器人才回复");
    expect(html).not.toContain("线程里可免 @");
  });

  it("TelegramSection (unconfigured) shows the not-configured state", () => {
    const html = renderToString(
      <TelegramSection status={null} onSaved={() => {}} />,
    );
    expect(html).toContain('data-testid="settings-telegram"');
    // v0.8.19 W3b — the not-configured state now reads via the "未配置" status
    // badge (the card-based redesign replaced the English "Not configured"
    // copy). Also assert the readout shows no fingerprint (em-dash) and the
    // token field still renders empty (red line: never pre-filled).
    expect(html).toContain("未配置");
    expect(html).toContain('data-testid="settings-telegram-token"');
    expect(html).toContain('type="password"');
    expect(html).toContain('value=""');
    // Nothing to gate until a bot exists.
    expect(html).not.toContain('data-testid="settings-telegram-require-mention"');
  });

  it("LarkSection (configured) renders its testid + masked app id + region", () => {
    const html = renderToString(
      <LarkSection
        status={{
          configured: true,
          app_id_last4: "…cli9",
          use_feishu: true,
          allowed_user_id_count: 2,
          allowed_user_ids: ["ou_1", "ou_2"],
          require_mention: true,
        }}
        onSaved={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-lark"');
    expect(html).toContain('data-testid="settings-lark-summary"');
    expect(html).toContain("…cli9");
    expect(html).toContain("Feishu (CN)");
    expect(html).not.toContain('type="password"');
    // The switch reflects the server value (here: already requiring @).
    expect(html).toContain('data-testid="settings-lark-require-mention"');
    expect(html).toContain('aria-checked="true"');
  });

  it("LarkSection (unconfigured) uses the compact two-column form and region segment", () => {
    const html = renderToString(
      <LarkSection status={null} onSaved={() => {}} />,
    );
    expect(html).toContain('data-testid="settings-lark"');
    expect(html).toContain("sm:grid-cols-2");
    expect(html).toContain('data-testid="settings-lark-region"');
    expect(html).toContain('rows="2"');
    expect(html).toContain('type="password"');
    expect(html).toContain('value=""');
    // Default textarea is empty → fail-closed warning is visible.
    expect(html).toContain("fail-closed");
  });

  it("SlackSection (bound) shows masked token tails and the allowed members, no secret field", () => {
    const html = renderToString(
      <SlackSection
        status={{
          configured: true,
          bot_token_last4: "…bot1",
          app_token_last4: "…app1",
          allowed_user_ids: ["U0ALICE", "U0BOB"],
          require_mention: false,
        }}
        onSaved={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-slack"');
    expect(html).toContain("已连接");
    expect(html).toContain('data-testid="settings-slack-summary"');
    expect(html).toContain("…bot1");
    expect(html).toContain("…app1");
    expect(html).toContain('data-testid="settings-slack-remove-U0ALICE"');
    expect(html).toContain('data-testid="settings-slack-remove-U0BOB"');
    expect(html).not.toContain('type="password"');
    // The @-mention switch sits in step ③ with Slack's extra thread clause.
    expect(html).toContain('data-testid="settings-slack-require-mention"');
    expect(html).toContain("机器人已有会话的线程里可免 @ 继续对话");
  });

  it("SlackSection (tokens saved, nobody allowed) asks for step 3 with sender capture", () => {
    const html = renderToString(
      <SlackSection
        status={{
          configured: true,
          bot_token_last4: "…bot1",
          app_token_last4: "…app1",
          allowed_user_ids: [],
          require_mention: false,
        }}
        onSaved={() => {}}
      />,
    );
    expect(html).toContain("待绑定");
    expect(html).toContain('data-testid="settings-slack-capture"');
    expect(html).toContain('id="settings-slack-users"');
    expect(html).toContain("未允许前 bot 谁也不回");
  });

  it("SlackSection (unconfigured) walks through create → tokens → allow", () => {
    const html = renderToString(<SlackSection status={null} onSaved={() => {}} />);
    expect(html).toContain('data-testid="settings-slack"');
    expect(html).toContain("未配置");
    // ① create the app from a link (the name drives its slash command)
    expect(html).toContain('data-testid="settings-slack-step-create"');
    expect(html).toContain('id="settings-slack-app-name"');
    expect(html).toContain('data-testid="settings-slack-create"');
    expect(html).toContain('data-testid="settings-slack-copy-manifest"');
    // ② the two tokens start empty, each with where-to-find-it
    expect(html).toContain('id="settings-slack-bot-token"');
    expect(html).toContain('id="settings-slack-app-token"');
    expect(html).toContain('placeholder="xoxb-…"');
    expect(html).toContain('placeholder="xapp-…"');
    expect(html).toContain("Bot User OAuth Token");
    expect(html).toContain("connections:write");
    expect(html).not.toMatch(/type="password"[^>]*value="[^"]+"/);
    // ③ binding waits for saved tokens
    expect(html).toContain('data-testid="settings-slack-step-bind"');
    expect(html).not.toContain('data-testid="settings-slack-capture"');
    expect(html).not.toContain('data-testid="settings-slack-require-mention"');
  });

  it("MyImSection gives Slack the same self-serve card as Telegram and Lark", () => {
    const html = renderToString(<MyImSection />);
    expect(html).toContain('data-testid="my-im-slack"');
    // Same three steps as the owner's admin card: create → tokens → allow.
    expect(html).toContain('data-testid="my-im-slack-create-link"');
    expect(html).toContain('id="my-im-slack-bot-token"');
    expect(html).toContain('id="my-im-slack-app-token"');
    expect(html).toContain('data-testid="my-im-slack-save"');
    expect(html).toContain('data-testid="my-im-slack-capture"');
    expect(html).toContain('data-testid="my-im-slack-allowlist-save"');
    expect(html).toContain("Telegram / Lark / Slack");
    expect(html).not.toMatch(/id="my-im-slack-(bot|app)-token"[^>]*value="[^"]+"/);
    // Before `/me/im` answers (effects don't run in SSR) no card has a bot,
    // so no card offers the @-mention switch.
    expect(html).not.toContain("require-mention");
  });

  it("MyImSection guides Telegram and Lark as two separate stepped cards", () => {
    const html = renderToString(<MyImSection />);
    expect(html).toContain('data-testid="settings-my-im"');
    expect(html).toContain("我的 IM bot · My bot");
    // Two independent cards, each with its OWN save button — the old
    // form-wide 保存 that mixed both providers is retired.
    expect(html).toContain('data-testid="my-im-telegram"');
    expect(html).toContain('data-testid="my-im-lark"');
    expect(html).toContain('data-testid="my-im-telegram-save"');
    expect(html).toContain('data-testid="my-im-lark-save"');
    expect(html).not.toContain('type="submit">保存</button>');
    // Each card reads as a numbered two-step flow: credential → binding.
    expect(html).toContain("保存 bot token");
    expect(html).toContain("绑定你的 chat");
    expect(html).toContain("保存 App 凭据");
    expect(html).toContain("允许 open_id");
    // Secrets never pre-filled (red line).
    expect(html).toContain('type="password"');
    expect(html).not.toContain('value="123456');
  });

  it("UserManagementSection (管理员 tab content) renders its testid + heading", () => {
    // Effects don't run under renderToString → the table stays in its
    // "loading…" row; we only assert the section shape here.
    const html = renderToString(<UserManagementSection />);
    expect(html).toContain('data-testid="settings-users"');
    expect(html).toContain("用户管理 · Users");
  });
});

// --------------------------------------------------------------------------
// Require @-mention in groups/channels — the one row on six cards. Mounted
// for real (jsdom) so a click drives the PUT and the switch follows the
// response, not the click.
// --------------------------------------------------------------------------

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const telegramStatus = (require_mention: boolean): NonNullable<ImConfigStatus["telegram"]> => ({
  configured: true,
  bot_token_last4: "…wxyz",
  chat_id_count: 1,
  allowed_chat_ids: ["42"],
  require_mention,
});
const larkStatus = (require_mention: boolean): NonNullable<ImConfigStatus["lark"]> => ({
  configured: true,
  app_id_last4: "…cli9",
  use_feishu: true,
  allowed_user_id_count: 2,
  allowed_user_ids: ["ou_1", "ou_2"],
  require_mention,
});
const slackStatus = (require_mention: boolean): NonNullable<ImConfigStatus["slack"]> => ({
  configured: true,
  bot_token_last4: "…bot1",
  app_token_last4: "…app1",
  allowed_user_ids: ["U0ALICE"],
  require_mention,
});

/** A fetch that answers any require-mention PUT (echoing the body, or `putStatus`
 *  with an `{error}`) and the tenant's `GET /me/im` with `mine`. Everything else
 *  (manifest / candidate polls) hangs — the cards render without them. */
function fetchFor(mine: ImConfigStatus | null, putStatus = 200) {
  return vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "PUT" && url.endsWith("/require-mention")) {
      if (putStatus !== 200) {
        return Promise.resolve(jsonResponse(putStatus, { error: "not configured" }));
      }
      const body = JSON.parse(String(init.body)) as { require_mention: boolean };
      return Promise.resolve(
        jsonResponse(200, {
          ok: true,
          platform: url.split("/").at(-2),
          require_mention: body.require_mention,
          reloaded: true,
          restart_required: false,
          note: "已生效",
        }),
      );
    }
    if (mine && !init?.method && url.endsWith("/api/v1/me/im")) {
      return Promise.resolve(jsonResponse(200, mine));
    }
    return new Promise<Response>(() => {});
  });
}

describe("require @-mention switch", () => {
  let container: HTMLDivElement;
  let root: ReturnType<typeof createRoot>;
  const realFetch = globalThis.fetch;
  const toasts: string[] = [];

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    toasts.length = 0;
    toastBus.handler = {
      push: (m) => toasts.push(m),
      info: (m) => toasts.push(m),
      error: (m) => toasts.push(`error: ${m}`),
    };
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    globalThis.fetch = realFetch;
    toastBus.handler = null;
    vi.restoreAllMocks();
  });

  async function mount(ui: ReactNode) {
    await act(async () => {
      root = createRoot(container);
      root.render(ui);
    });
  }
  const switchOf = (testid: string) =>
    container.querySelector(`[data-testid="${testid}"]`) as HTMLButtonElement | null;
  const putCalls = () =>
    vi.mocked(globalThis.fetch).mock.calls.filter(([, init]) => init?.method === "PUT");

  const ownerCard = {
    telegram: (on: boolean) => (
      <TelegramSection status={telegramStatus(on)} onSaved={() => {}} />
    ),
    lark: (on: boolean) => <LarkSection status={larkStatus(on)} onSaved={() => {}} />,
    slack: (on: boolean) => <SlackSection status={slackStatus(on)} onSaved={() => {}} />,
  };

  it.each(["telegram", "lark", "slack"] as const)(
    "owner %s card: the switch mirrors status.require_mention and PUTs the flip to /config/im",
    async (platform) => {
      globalThis.fetch = fetchFor(null);
      await mount(ownerCard[platform](false));
      const sw = switchOf(`settings-${platform}-require-mention`);
      expect(sw).not.toBeNull();
      expect(sw!.getAttribute("role")).toBe("switch");
      expect(sw!.getAttribute("aria-checked")).toBe("false");
      // The label is wired to the control, so the text is clickable too.
      expect(container.querySelector(`label[for="settings-${platform}-require-mention"]`)).not.toBeNull();

      await act(async () => sw!.click());
      expect(putCalls()).toHaveLength(1);
      const [url, init] = putCalls()[0];
      expect(String(url)).toBe(`/api/v1/config/im/${platform}/require-mention`);
      expect(JSON.parse(String(init?.body))).toEqual({ require_mention: true });
      // Follows the response, and the daemon's note is what the operator sees.
      expect(sw!.getAttribute("aria-checked")).toBe("true");
      expect(sw!.disabled).toBe(false);
      expect(toasts).toEqual(["已生效"]);
    },
  );

  it("owner card already requiring @: renders on, and the flip PUTs false", async () => {
    globalThis.fetch = fetchFor(null);
    await mount(ownerCard.telegram(true));
    const sw = switchOf("settings-telegram-require-mention")!;
    expect(sw.getAttribute("aria-checked")).toBe("true");
    await act(async () => sw.click());
    expect(JSON.parse(String(putCalls()[0][1]?.body))).toEqual({ require_mention: false });
    expect(sw.getAttribute("aria-checked")).toBe("false");
  });

  it("tenant cards: GET /me/im decides which card has the switch; the flip goes to /me/im", async () => {
    globalThis.fetch = fetchFor({
      telegram: telegramStatus(true),
      lark: null,
      slack: null,
      transport_warning: "",
    });
    await mount(<MyImSection />);
    const gets = vi
      .mocked(globalThis.fetch)
      .mock.calls.filter(([input, init]) => String(input) === "/api/v1/me/im" && !init?.method);
    expect(gets).toHaveLength(1);

    // Only the configured bot offers the gate, pre-set from the server.
    const sw = switchOf("my-im-telegram-require-mention");
    expect(sw).not.toBeNull();
    expect(sw!.getAttribute("aria-checked")).toBe("true");
    expect(switchOf("my-im-lark-require-mention")).toBeNull();
    expect(switchOf("my-im-slack-require-mention")).toBeNull();

    await act(async () => sw!.click());
    expect(putCalls()).toHaveLength(1);
    const [url, init] = putCalls()[0];
    expect(String(url)).toBe("/api/v1/me/im/telegram/require-mention");
    expect(JSON.parse(String(init?.body))).toEqual({ require_mention: false });
    expect(sw!.getAttribute("aria-checked")).toBe("false");
    expect(toasts).toEqual(["已生效"]);
  });

  it("tenant cards: every configured bot gets its own switch (lark + slack too)", async () => {
    globalThis.fetch = fetchFor({
      telegram: null,
      lark: larkStatus(false),
      slack: slackStatus(true),
      transport_warning: "",
    });
    await mount(<MyImSection />);
    expect(switchOf("my-im-telegram-require-mention")).toBeNull();
    expect(switchOf("my-im-lark-require-mention")!.getAttribute("aria-checked")).toBe("false");
    expect(switchOf("my-im-slack-require-mention")!.getAttribute("aria-checked")).toBe("true");
    await act(async () => switchOf("my-im-lark-require-mention")!.click());
    expect(String(putCalls()[0][0])).toBe("/api/v1/me/im/lark/require-mention");
  });

  it("tenant cards: a reload shows what is saved — fingerprints and the bound allowlists", async () => {
    globalThis.fetch = fetchFor({
      telegram: telegramStatus(false),
      lark: larkStatus(false),
      slack: slackStatus(false),
      transport_warning: "",
    });
    await mount(<MyImSection />);
    const text = (id: string) => container.querySelector(`[data-testid="${id}"]`)?.textContent ?? "";
    expect(text("my-im-telegram-saved")).toContain("…wxyz");
    expect(text("my-im-lark-saved")).toContain("…cli9");
    expect(text("my-im-slack-saved")).toContain("…bot1");
    expect(text("my-im-slack-saved")).toContain("…app1");
    // The allowlist editors start from the saved lists (the PUTs replace the
    // whole list), and the tokens are still never echoed back.
    expect(text("my-im-telegram-bind")).toContain("42");
    const field = (id: string) => container.querySelector<HTMLTextAreaElement>(`#${id}`)!.value;
    expect(field("my-im-lark-users")).toBe("ou_1\nou_2");
    expect(field("my-im-slack-users")).toBe("U0ALICE");
    expect(field("my-im-slack-bot-token")).toBe("");
    expect(field("my-im-slack-app-token")).toBe("");
  });

  it("tenant cards: nothing saved → no saved line and empty allowlists", async () => {
    globalThis.fetch = fetchFor({ telegram: null, lark: null, slack: null, transport_warning: "" });
    await mount(<MyImSection />);
    for (const p of ["telegram", "lark", "slack"]) {
      expect(container.querySelector(`[data-testid="my-im-${p}-saved"]`)).toBeNull();
    }
    expect(container.querySelector<HTMLTextAreaElement>("#my-im-slack-users")!.value).toBe("");
  });

  it("a failed PUT keeps the old value and toasts the reason (owner and tenant alike)", async () => {
    globalThis.fetch = fetchFor(
      { telegram: null, lark: null, slack: slackStatus(false), transport_warning: "" },
      400,
    );
    await mount(
      <>
        <SlackSection status={slackStatus(false)} onSaved={() => {}} />
        <MyImSection />
      </>,
    );
    for (const id of ["settings-slack-require-mention", "my-im-slack-require-mention"]) {
      const sw = switchOf(id);
      expect(sw).not.toBeNull();
      await act(async () => sw!.click());
      expect(sw!.getAttribute("aria-checked")).toBe("false");
      expect(sw!.disabled).toBe(false);
    }
    expect(putCalls()).toHaveLength(2);
    expect(toasts).toHaveLength(2);
    expect(toasts.every((t) => t.startsWith("error: ") && t.includes("not configured"))).toBe(true);
  });
});
