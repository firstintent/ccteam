// v0.8.8 F4 — Settings section smoke tests.
//
// AccessView owns the admin's masked IM-status fetch and composes the named
// Telegram/Lark sections. User management stays on the 管理员 · Admin tab.
//
// No DOM env (no jsdom): use React's `renderToString` to assert each named
// section's initial HTML shape, mirroring SessionsListPage.test.tsx. We assert:
//   - configured sections default to compact masked summaries
//   - unconfigured sections default to empty forms, and that the
//     masked status NEVER echoes a plaintext secret (red-line guard)
// Interactive paths (token save → chat_id poll loop, overwrite confirm) are
// covered by configApi.test.ts + manual / Playwright host E2E.

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import {
  LarkSection,
  MyImSection,
  TelegramSection,
  UserManagementSection,
} from "./SettingsPage";

describe("Settings sections", () => {
  it("TelegramSection (configured) defaults to its compact masked summary", () => {
    const html = renderToString(
      <TelegramSection
        status={{ configured: true, bot_token_last4: "…wxyz", chat_id_count: 1 }}
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
    expect(html).toContain('data-testid="settings-lark-summary"');
    expect(html).toContain("…cli9");
    expect(html).toContain("Feishu (CN)");
    expect(html).not.toContain('type="password"');
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
