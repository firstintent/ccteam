// v0.8.19 W1 — locks the composer's Enter-to-send decision, especially the
// IME guard (the owner-reported #1 bug: pressing Enter to confirm a Chinese
// candidate must NOT send a half-typed message).

import { describe, expect, it, vi, afterEach } from "vitest";

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
import {
  attachmentsBlockSend,
  attachmentsPayload,
  buildScheduleWhen,
  ChatComposer,
  datetimeLocalToMs,
  fetchSkillLists,
  scheduleWhenPreview,
  shouldSubmitOnEnter,
  SkillMenuSections,
  type ComposerAttachment,
} from "./ChatComposer";
import type { LibrarySkillSummary, SkillSummary } from "../lib/attachmentsApi";
import { defaultDraft } from "../lib/vendors";

const base = { key: "Enter", shiftKey: false, isComposing: false, keyCode: 13 };

describe("shouldSubmitOnEnter", () => {
  it("sends on a plain Enter for a finished line", () => {
    expect(shouldSubmitOnEnter(base)).toBe(true);
  });

  it("does NOT send while an IME candidate is composing (the #1 bug)", () => {
    expect(shouldSubmitOnEnter({ ...base, isComposing: true })).toBe(false);
  });

  it("does NOT send on the legacy keyCode 229 (IME in progress)", () => {
    expect(shouldSubmitOnEnter({ ...base, keyCode: 229 })).toBe(false);
  });

  it("does NOT send on Shift+Enter (newline)", () => {
    expect(shouldSubmitOnEnter({ ...base, shiftKey: true })).toBe(false);
  });

  it("ignores non-Enter keys", () => {
    expect(shouldSubmitOnEnter({ ...base, key: "a" })).toBe(false);
  });

  it("still sends Cmd/Ctrl+Enter (no shift, not composing)", () => {
    expect(shouldSubmitOnEnter({ ...base, key: "Enter" })).toBe(true);
  });
});

describe("buildScheduleWhen", () => {
  const noon = new Date(2026, 6, 25, 12, 0, 0, 0).getTime(); // local noon

  it("prefers chips, then free-form hours+minutes as a single +Nm delay", () => {
    expect(buildScheduleWhen({ chip: "+30m", minutes: "5" }, noon)).toBe("+30m");
    expect(buildScheduleWhen({ hours: "1", minutes: "15" }, noon)).toBe("+75m");
    expect(buildScheduleWhen({ minutes: "45" }, noon)).toBe("+45m");
    expect(buildScheduleWhen({ hours: "2" }, noon)).toBe("+120m");
    expect(buildScheduleWhen({ minutes: "0", hours: "0" }, noon)).toBeNull();
    expect(buildScheduleWhen({ minutes: "-3" }, noon)).toBeNull();
  });

  it("maps datetime-local (browser wall clock) to minutes-from-now, not daemon TZ", () => {
    // 12:40 local → +40m from noon
    expect(buildScheduleWhen({ absolute: "2026-07-25T12:40" }, noon)).toBe("+40m");
    // Past absolute is rejected
    expect(buildScheduleWhen({ absolute: "2026-07-25T11:00" }, noon)).toBeNull();
    expect(datetimeLocalToMs("2026-07-25T12:40")).toBe(
      new Date(2026, 6, 25, 12, 40, 0, 0).getTime(),
    );
  });

  it("previews a relative when as a local clock label", () => {
    const label = scheduleWhenPreview("+30m", noon, "en-US");
    expect(label).toBeTruthy();
    // Locale-dependent formatting (12:30 vs 12:30 PM) — only assert the minute.
    expect(label).toMatch(/12:30/);
  });
});

// ── attachment pure helpers ────────────────────────────────────────────────────

const fileChip = (over: Partial<ComposerAttachment> = {}): ComposerAttachment => ({
  id: "att-1",
  kind: "file",
  name: "notes.txt",
  path: "/p/.ccteam/uploads/1-notes.txt",
  status: "ready",
  ...over,
});

describe("attachmentsPayload", () => {
  it("maps ready file/image chips to {kind, path, name}", () => {
    expect(attachmentsPayload([fileChip()])).toEqual([
      { kind: "file", path: "/p/.ccteam/uploads/1-notes.txt", name: "notes.txt" },
    ]);
  });

  it("maps a skill chip to {kind, name} — the server resolves the path", () => {
    expect(
      attachmentsPayload([
        fileChip({ id: "att-2", kind: "skill", name: "deep-research", path: undefined }),
      ]),
    ).toEqual([{ kind: "skill", name: "deep-research" }]);
  });

  it("drops chips that are still uploading or errored", () => {
    expect(
      attachmentsPayload([
        fileChip({ status: "uploading", path: undefined }),
        fileChip({ id: "att-3", status: "error", path: undefined }),
      ]),
    ).toEqual([]);
  });

  // v0.9.9 — global skill library: a library pick carries scope:"global";
  // a project pick stays byte-compatible with older clients.
  it("maps a global-library skill chip to {kind, name, scope:'global'}", () => {
    expect(
      attachmentsPayload([
        fileChip({
          id: "att-g1",
          kind: "skill",
          name: "baoyu-skills/baoyu-comic",
          path: undefined,
          scope: "global",
        }),
      ]),
    ).toEqual([{ kind: "skill", name: "baoyu-skills/baoyu-comic", scope: "global" }]);
  });

  it("keeps a project skill chip byte-compatible (no scope key on the wire)", () => {
    const payload = attachmentsPayload([
      fileChip({ id: "att-p1", kind: "skill", name: "deep-research", path: undefined }),
    ]);
    expect(payload).toEqual([{ kind: "skill", name: "deep-research" }]);
    expect(Object.keys(payload[0]!)).not.toContain("scope");
  });
});

describe("attachmentsBlockSend", () => {
  it("blocks while any chip uploads; clear once all settle", () => {
    expect(attachmentsBlockSend([fileChip({ status: "uploading" })])).toBe(true);
    expect(attachmentsBlockSend([fileChip(), fileChip({ id: "att-4", status: "error" })])).toBe(
      false,
    );
    expect(attachmentsBlockSend([])).toBe(false);
  });
});

// ── model button label (vendor always spelled out) ────────────────────────────

describe("model button label", () => {
  const renderBtn = (over: Partial<Parameters<typeof ChatComposer>[0]> = {}) =>
    renderToString(
      <ChatComposer
        draftKey="test"
        lang="zh"
        draft={defaultDraft()}
        onDraftChange={() => {}}
        onSend={() => {}}
        {...over}
      />,
    );

  it("new-session draft: the vendor prefixes the default/picked model", () => {
    expect(renderBtn()).toContain("<span>claude · 默认</span>");
    expect(renderBtn({ draft: { ...defaultDraft(), model: "opus" } })).toContain(
      "<span>claude · opus</span>",
    );
    expect(
      renderBtn({
        draft: { ...defaultDraft(), vendor: "codex", model: "默认", protocol: "app-server" },
      }),
    ).toContain("<span>codex · 默认</span>");
  });

  it("conversation: the live model rides after the vendor; an unreported model leaves the vendor alone", () => {
    expect(
      renderBtn({
        modelLabel: "gpt-5.5",
        draft: { ...defaultDraft(), vendor: "codex", model: "", protocol: "app-server" },
      }),
    ).toContain("<span>codex · gpt-5.5</span>");
    const html = renderBtn({ modelLabel: "" });
    expect(html).toContain("<span>claude</span>");
    expect(html).not.toContain("<span>claude · ");
  });

  it("conversation: a live kimi session shows the same vendor · model · effort shape", () => {
    const html = renderBtn({
      modelLabel: "kimi-code/k3",
      draft: { ...defaultDraft(), vendor: "kimi", model: "", protocol: "acp" },
    });
    expect(html).toContain("<span>Kimi · kimi-code/k3</span>");
    // Effort renders as the vendor's own token, never a translated word —
    // identical in zh and en (see `effortLabel`).
    expect(html).toContain('<span class="eff">default</span>');
  });
});

// ── prefill (Home 快速开始 templates) ──────────────────────────────────────────

describe("prefill", () => {
  const renderComposer = (prefill?: { text: string; nonce: number }) =>
    renderToString(
      <ChatComposer
        draftKey="test"
        lang="zh"
        draft={defaultDraft()}
        onDraftChange={() => {}}
        onSend={() => {}}
        prefill={prefill}
      />,
    );

  it("a bumped nonce replaces the draft text", () => {
    expect(renderComposer({ text: "帮我修一个 bug。", nonce: 1 })).toContain("帮我修一个 bug。");
  });

  it("nonce 0 means no prefill (draft untouched)", () => {
    expect(renderComposer({ text: "不该出现", nonce: 0 })).not.toContain("不该出现");
  });
});

// ── v0.9.9 — global skill library: two-section attach menu (admin-gated) ─────

const projectSkills: SkillSummary[] = [
  { skill: "deep-research", description: "fan-out research harness" },
];
const librarySkills: LibrarySkillSummary[] = [
  {
    id: "grill-me",
    description: "decision-tree griller",
    path: "/home/u/.ccteam/skills/grill-me/SKILL.md",
    source: "hub",
  },
  {
    id: "baoyu-skills/baoyu-comic",
    description: "comic renderer",
    path: "/home/u/.ccteam/skills/baoyu-skills/baoyu-comic/SKILL.md",
  },
];

describe("SkillMenuSections (two-section attach menu)", () => {
  const renderSections = (over: Partial<Parameters<typeof SkillMenuSections>[0]> = {}) =>
    renderToString(
      <SkillMenuSections
        lang="zh"
        skills={projectSkills}
        globalSkills={librarySkills}
        attachments={[]}
        onToggleSkill={() => {}}
        {...over}
      />,
    );

  it("every caller sees BOTH the Project section and the Global library section", () => {
    const html = renderSections();
    expect(html).toContain('data-testid="skill-section-project"');
    expect(html).toContain('data-testid="skill-section-global"');
    expect(html).toContain("项目");
    expect(html).toContain("全局库");
    // project-local rows AND library rows (nested ids render verbatim)
    expect(html).toContain("deep-research");
    expect(html).toContain("grill-me");
    expect(html).toContain("baoyu-skills/baoyu-comic");
  });

  it("renders the section headers in English when lang=en", () => {
    const html = renderSections({ lang: "en" });
    expect(html).toContain("Project");
    expect(html).toContain("Global library");
  });

  it("global rows keep their distinct test ids and nested library names", () => {
    const html = renderSections();
    expect(html).toContain('data-testid="skill-section-project"');
    expect(html).toContain('data-testid="skill-section-global"');
    expect(html).toContain("deep-research");
    expect(html).toContain('data-testid="skill-global-grill-me"');
    expect(html).toContain('data-testid="skill-global-baoyu-skills/baoyu-comic"');
  });

  it("empty library gets the i18n empty hint (admin)", () => {
    expect(renderSections({ globalSkills: [] })).toContain("全局库为空");
    expect(renderSections({ globalSkills: [], lang: "en" })).toContain("Global library is empty");
  });
});

describe("fetchSkillLists", () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  it("fetches the project list AND /api/v1/skills", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify([]), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ skills: [] }), { status: 200 }));
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    const lists = fetchSkillLists("demo");
    expect(lists.global).not.toBeNull();
    await lists.project;
    await lists.global;
    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls).toContain("/api/v1/projects/demo/skills");
    expect(urls).toContain("/api/v1/skills");
  });

  it("always returns a real global-library promise", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify([]), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ skills: [] }), { status: 200 }));
    globalThis.fetch = fetchMock as unknown as typeof fetch;
    const lists = fetchSkillLists("demo");
    expect(lists.global).not.toBeNull();
    await lists.project;
    await lists.global;
    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls).toEqual(["/api/v1/projects/demo/skills", "/api/v1/skills"]);
  });
});
