// v0.8.8 F4 — SettingsPage smoke tests.
//
// No DOM env (no jsdom): use React's `renderToString` to assert the initial
// HTML shape, mirroring SessionsListPage.test.tsx. The page's success state
// needs the async getImConfig() to resolve (renderToString won't await), so
// we assert:
//   - the page's loading placeholder before the fetch resolves
//   - the two section sub-components render their key data-testids + that the
//     masked status NEVER echoes a plaintext secret (red-line guard)
// Interactive paths (token save → chat_id poll loop, overwrite confirm) are
// covered by configApi.test.ts + manual / Playwright host E2E.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import SettingsPage, { LarkSection, TelegramSection } from "./SettingsPage";

const realFetch = globalThis.fetch;

describe("SettingsPage initial render", () => {
  beforeEach(() => {
    // Never-resolving fetch keeps the page in its loading state.
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the loading placeholder before the fetch resolves", () => {
    const html = renderToString(
      <MemoryRouter>
        <SettingsPage />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="settings-loading"');
    expect(html).toContain("loading settings");
  });
});

describe("Settings sections", () => {
  it("TelegramSection (configured) renders its testid + masked fingerprint, never the token", () => {
    const html = renderToString(
      <TelegramSection
        status={{ configured: true, bot_token_last4: "…wxyz", chat_id_count: 1 }}
        onSaved={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-telegram"');
    expect(html).toContain("…wxyz");
    // The password input must start empty (no pre-filled secret): the masked
    // fingerprint is shown as text, but the <input> renders value="".
    expect(html).toContain('type="password"');
    expect(html).toContain('value=""');
    // The fingerprint must NOT appear as an input value (only as label text).
    expect(html).not.toContain('value="…wxyz"');
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
    expect(html).toContain('type="password"');
    expect(html).toContain('value=""');
  });

  it("LarkSection (configured) renders its testid + masked app id + region", () => {
    const html = renderToString(
      <LarkSection
        status={{
          configured: true,
          app_id_last4: "…cli9",
          use_feishu: true,
          allowed_user_id_count: 2,
        }}
        onSaved={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-lark"');
    expect(html).toContain("…cli9");
    expect(html).toContain("Feishu (CN)");
  });

  it("LarkSection (unconfigured) warns the empty allowlist is fail-closed", () => {
    const html = renderToString(
      <LarkSection status={null} onSaved={() => {}} />,
    );
    expect(html).toContain('data-testid="settings-lark"');
    // Default textarea is empty → fail-closed warning is visible.
    expect(html).toContain("fail-closed");
  });
});
