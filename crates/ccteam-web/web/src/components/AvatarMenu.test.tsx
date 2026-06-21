// v0.8.18 柱2/UI — AvatarMenu / AvatarPopover smoke tests.
//
// AvatarPopover is pure (props-driven) → SSR-renderable directly, so the
// language switch is proven by the labels it emits. The AvatarMenu wrapper
// reaches useWebSettings (localStorage/window) → stub them before imports.

import { describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import { renderToString } from "react-dom/server";
import AvatarMenu, { AvatarPopover } from "./AvatarMenu";

const noop = () => {};

function popover(lang: "zh" | "en", avatar = "🟧") {
  return renderToString(
    <AvatarPopover
      lang={lang}
      displayName="rob"
      avatar={avatar}
      onLanguage={noop}
      onName={noop}
      onAvatar={noop}
      onLogout={noop}
    />,
  );
}

describe("AvatarPopover (pure)", () => {
  it("renders the personal-settings popover in Chinese by default", () => {
    const html = popover("zh");
    expect(html).toContain('data-testid="avatar-popover"');
    expect(html).toContain("个人设置");
    expect(html).toContain("显示名");
    expect(html).toContain("界面语言");
    expect(html).toContain('data-testid="avatar-name-input"');
    expect(html).toContain('data-testid="lang-zh"');
    expect(html).toContain('data-testid="lang-en"');
    expect(html).toContain('data-testid="avatar-logout"');
    expect(html).toContain("登出");
    // A toggle has an active + an inactive side.
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain('aria-pressed="false"');
  });

  it("switches to English labels when lang=en", () => {
    const html = popover("en");
    expect(html).toContain("Personal settings");
    expect(html).toContain("Display name");
    expect(html).toContain("Language");
    expect(html).toContain("Log out");
    // The Chinese popover title is gone.
    expect(html).not.toContain("个人设置");
  });

  it("marks the selected avatar swatch pressed", () => {
    const html = popover("zh", "🟩");
    expect(html).toContain('data-testid="avatar-swatch-🟩"');
    expect(html).toContain('aria-pressed="true"');
  });
});

describe("AvatarMenu (wrapper)", () => {
  it("renders the avatar button with the popover closed by default", () => {
    const html = renderToString(<AvatarMenu />);
    expect(html).toContain('data-testid="avatar-button"');
    // Closed → no popover in the SSR output until clicked.
    expect(html).not.toContain('data-testid="avatar-popover"');
  });
});
