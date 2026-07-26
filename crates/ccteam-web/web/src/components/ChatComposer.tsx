// v0.8.24 Track A — the prototype composer (ui-prototype.html `.composer`),
// shared 同构 by Home and Conversation:
//   - auto-growing textarea (52→260px), `has-text` send-button darkening
//   - IME composition guard (the owner's #1 bug: a CJK candidate Enter must
//     never send a half-typed message) + per-key draft persistence
//   - HITL 胶囊 toggle (black-on when armed) with a permission-mode toast
//   - the 3-section model menu: models grouped by vendor · effort · protocol
//     (per-vendor; claude `terminal` admin-only)
//   - Send morphs into a red Stop while a turn is in flight with an empty
//     draft (interrupt keeps the session — never a kill).
//   - attach menu (＋): upload files/photos + attach skills in TWO sections —
//     the project-local list and, admins only, the user-level global library
//     (`GET /api/v1/skills`; a library pick rides the turn as
//     `{kind:"skill", scope:"global"}`, never an install). Picked
//     files upload immediately (chips show progress), paste/drag-drop attach
//     too, and Send names the stored paths in the turn's `attachments[]` —
//     the server weaves them into the turn text (vendor-generic IM grammar).

import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowUp,
  ChevronRight,
  Clock,
  FileText,
  Hand,
  Image as ImageIcon,
  Loader2,
  Mic,
  Plus,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import { toastBus } from "../lib/toastBus";
import { makeT, type Lang } from "../lib/i18n";
import {
  listLibrarySkills,
  listProjectSkills,
  uploadAttachment,
  type LibrarySkillSummary,
  type SkillSummary,
  type TurnAttachment,
} from "../lib/attachmentsApi";
import {
  EFFORT_KEYS,
  normalizeDraft,
  vendorSpec,
  visibleProtocols,
  VENDORS,
  type ComposerDraft,
  type VendorId,
} from "../lib/vendors";

/** Pure decision for the composer's Enter keydown — the IME guard is
 *  unit-testable in the node/SSR test env. Returns false while a CJK
 *  candidate is composing (`isComposing` / legacy keyCode 229), on
 *  Shift+Enter (newline), and for any non-Enter key. */
// eslint-disable-next-line react-refresh/only-export-components -- pure predicate co-located with the composer so it's unit-testable in node.
export function shouldSubmitOnEnter(e: {
  key: string;
  shiftKey: boolean;
  isComposing: boolean;
  keyCode?: number;
}): boolean {
  if (e.key !== "Enter") return false;
  if (e.isComposing || e.keyCode === 229) return false;
  if (e.shiftKey) return false;
  return true;
}

/** Inputs that resolve to a daemon `when` string for delayed send. */
export type ScheduleTimeDraft = {
  /** Quick chip such as `+30m` / `+1h`; wins over free-form numbers when set. */
  chip?: string;
  /** Free-form minutes (integer ≥ 0). Combined with hours as total delay. */
  minutes?: string;
  /** Free-form hours (integer ≥ 0). */
  hours?: string;
  /** `datetime-local` value (`YYYY-MM-DDTHH:MM`) in the **browser** wall clock. */
  absolute?: string;
};

function parseNonNegInt(raw: string | undefined): number | null {
  if (raw == null) return null;
  const t = raw.trim();
  if (t === "" || !/^\d+$/.test(t)) return null;
  const n = Number(t);
  if (!Number.isFinite(n) || n < 0) return null;
  return Math.floor(n);
}

/** Parse a `datetime-local` string as the browser's local wall clock. */
// eslint-disable-next-line react-refresh/only-export-components -- pure parser stays co-located with the scheduling helpers it serves and is exported for vitest.
export function datetimeLocalToMs(value: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value.trim());
  if (!m) return null;
  const [, ys, mos, ds, hs, mins] = m;
  const ms = new Date(
    Number(ys),
    Number(mos) - 1,
    Number(ds),
    Number(hs),
    Number(mins),
    0,
    0,
  ).getTime();
  return Number.isNaN(ms) ? null : ms;
}

/**
 * Build the daemon-facing `when` string.
 *
 * Prefer **relative** `+Nm` so browser timezone and daemon timezone never
 * disagree: absolute `datetime-local` is converted to minutes-from-now using
 * the browser clock (same real-world instant on any NTP-synced host).
 * Returns `null` when nothing valid / not in the future.
 */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper for vitest.
export function buildScheduleWhen(
  draft: ScheduleTimeDraft,
  nowMs: number = Date.now(),
): string | null {
  const chip = draft.chip?.trim() ?? "";
  if (/^\+\d+[mh]$/.test(chip)) return chip;

  const hours = parseNonNegInt(draft.hours);
  const minutes = parseNonNegInt(draft.minutes);
  if (hours !== null || minutes !== null) {
    const total = (hours ?? 0) * 60 + (minutes ?? 0);
    if (total <= 0) return null;
    return `+${total}m`;
  }

  const absolute = draft.absolute?.trim() ?? "";
  if (absolute.includes("T")) {
    const target = datetimeLocalToMs(absolute);
    if (target == null) return null;
    const mins = Math.round((target - nowMs) / 60_000);
    if (mins <= 0) return null;
    return `+${mins}m`;
  }
  return null;
}

/** Human preview of a relative `when` as a local clock time (for the UI). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper for vitest.
export function scheduleWhenPreview(
  when: string,
  nowMs: number = Date.now(),
  locale = "en-US",
): string | null {
  const m = /^\+(\d+)([mh])$/.exec(when.trim());
  if (!m) return null;
  const n = Number(m[1]);
  const ms = m[2] === "h" ? n * 3_600_000 : n * 60_000;
  const at = new Date(nowMs + ms);
  return at.toLocaleString(locale, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** @deprecated use {@link buildScheduleWhen}; kept for older call sites/tests. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper for vitest.
export function scheduleWireWhen(value: string, nowMs: number = Date.now()): string {
  if (value.includes("T")) {
    return buildScheduleWhen({ absolute: value }, nowMs) ?? value.replace("T", " ");
  }
  return value;
}

/** One composer chip: a picked file mid-upload / stored, or an attached
 *  skill. `path` is set once the server stored the file (skills carry the
 *  id in `name` and never upload). `scope === "global"` marks a skill picked
 *  from the user-level global library (project skills leave it undefined). */
export interface ComposerAttachment {
  id: string;
  kind: "image" | "file" | "skill";
  name: string;
  path?: string;
  status: "uploading" | "ready" | "error";
  scope?: "global";
}

/** Pure: the turn-POST `attachments[]` payload for the current chips.
 *  Only `ready` chips ride the turn; call [`attachmentsBlockSend`] first
 *  to keep the draft while uploads are still in flight. A global-library
 *  skill carries `scope:"global"`; a project skill stays byte-compatible
 *  (`{kind, name}`, no scope). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located with the composer so it's unit-testable in node.
export function attachmentsPayload(items: ComposerAttachment[]): TurnAttachment[] {
  return items
    .filter((a) => a.status === "ready")
    .map((a) =>
      a.kind === "skill"
        ? a.scope === "global"
          ? { kind: "skill" as const, name: a.name, scope: "global" as const }
          : { kind: "skill" as const, name: a.name }
        : { kind: a.kind, path: a.path, name: a.name },
    );
}

/** Pure: true while any chip is still uploading (Send must wait). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located with the composer so it's unit-testable in node.
export function attachmentsBlockSend(items: ComposerAttachment[]): boolean {
  return items.some((a) => a.status === "uploading");
}

/** The attach-menu skill lists: the project list always, the user-level
 *  global library ONLY for admins (the endpoint itself is admin-only — a
 *  non-admin must not even fire the request). `global === null` = not asked. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located with the composer so it's unit-testable in node.
export function fetchSkillLists(
  slug: string,
  isAdmin: boolean,
): { project: Promise<SkillSummary[]>; global: Promise<LibrarySkillSummary[]> | null } {
  return {
    project: listProjectSkills(slug),
    global: isAdmin ? listLibrarySkills() : null,
  };
}

/** The attach menu's skill area — two sections: the project-local list
 *  (existing behavior) and, admins only, the user-level Global library.
 *  Picking a library skill attaches `{kind:"skill", scope:"global"}` — it
 *  never installs anything. Extracted as a pure presentational component so
 *  the admin/non-admin split is render-testable in the node vitest env. */
export function SkillMenuSections({
  lang,
  isAdmin,
  skills,
  globalSkills,
  attachments,
  onToggleSkill,
}: {
  lang: Lang;
  isAdmin: boolean;
  /** `null` = still loading. */
  skills: SkillSummary[] | null;
  /** `null` = still loading (or never fetched — non-admin). */
  globalSkills: LibrarySkillSummary[] | null;
  attachments: ComposerAttachment[];
  onToggleSkill: (name: string, scope: "project" | "global") => void;
}) {
  const t = makeT(lang);
  return (
    <>
      <div className="sel-group" data-testid="skill-section-project">
        {t("project")}
      </div>
      {skills === null ? (
        <div className="sel-item muted skill-row">…</div>
      ) : skills.length === 0 ? (
        <div className="sel-item muted skill-row">{t("noSkills")}</div>
      ) : (
        skills.map((s) => {
          const on = attachments.some(
            (a) => a.kind === "skill" && a.name === s.skill && a.scope !== "global",
          );
          return (
            <button
              key={s.skill}
              type="button"
              className={`sel-item skill-row ${on ? "selected" : ""}`}
              onClick={() => onToggleSkill(s.skill, "project")}
              title={s.description || s.skill}
            >
              {s.skill}
              <span className="check">✓</span>
            </button>
          );
        })
      )}
      {isAdmin ? (
        <>
          <div className="sel-group" data-testid="skill-section-global">
            {t("attachSkillGlobal")}
          </div>
          {globalSkills === null ? (
            <div className="sel-item muted skill-row">…</div>
          ) : globalSkills.length === 0 ? (
            <div className="sel-item muted skill-row">{t("noGlobalSkills")}</div>
          ) : (
            globalSkills.map((g) => {
              const on = attachments.some(
                (a) => a.kind === "skill" && a.name === g.id && a.scope === "global",
              );
              return (
                <button
                  key={g.id}
                  type="button"
                  className={`sel-item skill-row ${on ? "selected" : ""}`}
                  data-testid={`skill-global-${g.id}`}
                  onClick={() => onToggleSkill(g.id, "global")}
                  title={g.description || g.id}
                >
                  {g.id}
                  <span className="check">✓</span>
                </button>
              );
            })
          )}
        </>
      ) : null}
    </>
  );
}

/** Per-surface draft key — an unsent message survives a reload / a session
 *  switch (the conversation view is keyed by sid and remounts on switch). */
const draftStorageKey = (key: string) => `ccteam.draft.v1.${key}`;

function loadDraft(key: string): string {
  try {
    return localStorage.getItem(draftStorageKey(key)) ?? "";
  } catch {
    return "";
  }
}

let attachmentSeq = 0;
const nextAttachmentId = () => `att-${++attachmentSeq}`;

export function ChatComposer({
  draftKey,
  lang,
  placeholderKey = "convPh",
  busy,
  disabled,
  onSend,
  onStop,
  draft,
  onDraftChange,
  modelLabel,
  locked,
  allowedVendors,
  isAdmin,
  topSlot,
  sendTestId = "composer-send",
  uploadSlug,
  prefill,
  onSchedule,
  scheduleTimezone,
}: {
  /** localStorage draft scope — `"home"` or the sid. */
  draftKey: string;
  lang: Lang;
  placeholderKey?: string;
  /** A turn is in flight → the send button morphs into Stop when empty. */
  busy?: boolean;
  disabled?: boolean;
  /** Send the trimmed text (+ any ready attachments). Return `false` to KEEP
   *  the draft (validation failed upstream); anything else clears it. */
  onSend: (text: string, attachments: TurnAttachment[]) => boolean | void;
  /** Interrupt the running turn (session kept). */
  onStop?: () => void;
  /** The model/effort/protocol/HITL draft the menu edits. */
  draft: ComposerDraft;
  onDraftChange: (next: ComposerDraft) => void;
  /** Conversation override for the button's model segment (live session
   *  model). The vendor name always prefixes it; an empty/omitted model
   *  segment leaves the vendor name standing alone. */
  modelLabel?: string;
  /** Conversation: spawn parameters are fixed → picking toasts instead. */
  locked?: boolean;
  /** Vendors installed on the target host (Home 主机绑定 vendor) — the menu
   *  only offers these. Omit to offer the full registry. */
  allowedVendors?: VendorId[];
  isAdmin: boolean;
  /** Home's inline new-project row renders inside the composer card. */
  topSlot?: React.ReactNode;
  sendTestId?: string;
  /** Project slug uploads + the skill picker target. Omit (e.g. while the
   *  Home new-project row is open) to disable attaching with a hint. */
  uploadSlug?: string;
  /** Home 快速开始 templates: bump `nonce` to replace the draft text with
   *  `text` and focus the textarea (nonce 0 = no prefill). */
  prefill?: { text: string; nonce: number };
  /** Conversation-only delayed send. Omit on Home to hide schedule mode. */
  onSchedule?: (text: string, when: string) => boolean | void;
  /** Label reported by the daemon (`PDT (UTC-07:00)`, etc.). */
  scheduleTimezone?: string;
}) {
  const t = makeT(lang);
  const [text, setText] = useState(() => loadDraft(draftKey));
  const [menuOpen, setMenuOpen] = useState(false);
  const [attachOpen, setAttachOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [attachments, setAttachments] = useState<ComposerAttachment[]>([]);
  const [skills, setSkills] = useState<SkillSummary[] | null>(null);
  const [globalSkills, setGlobalSkills] = useState<LibrarySkillSummary[] | null>(null);
  const [scheduleMode, setScheduleMode] = useState(false);
  /** Quick chip (`+30m`) — exclusive with free-form numbers / absolute. */
  const [scheduleChip, setScheduleChip] = useState("");
  const [scheduleMinutes, setScheduleMinutes] = useState("");
  const [scheduleHours, setScheduleHours] = useState("");
  /** `datetime-local` value in the browser wall clock. */
  const [scheduleAbsolute, setScheduleAbsolute] = useState("");
  const composingRef = useRef(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const selRef = useRef<HTMLDivElement | null>(null);
  const attachRef = useRef<HTMLDivElement | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  // Persist (or clear) this surface's draft on every change.
  useEffect(() => {
    try {
      if (text) localStorage.setItem(draftStorageKey(draftKey), text);
      else localStorage.removeItem(draftStorageKey(draftKey));
    } catch {
      /* storage disabled — in-memory draft still works */
    }
  }, [draftKey, text]);

  // Uploads are project-scoped: switching the target project orphans any
  // already-stored files, so reset the chips + the skills cache instead of
  // letting the next send 400 on a cross-project path. Render-phase derived
  // reset (the React-endorsed alternative to setState-in-effect).
  const [attachSlug, setAttachSlug] = useState(uploadSlug);
  if (attachSlug !== uploadSlug) {
    setAttachSlug(uploadSlug);
    setSkills(null);
    setGlobalSkills(null);
    if (attachments.length > 0) setAttachments([]);
  }

  // Quick-start template pick: a bumped nonce replaces the draft text
  // (render-phase derived state, same pattern as the attachSlug reset above).
  const [appliedPrefill, setAppliedPrefill] = useState(0);
  if (prefill && prefill.nonce !== appliedPrefill) {
    setAppliedPrefill(prefill.nonce);
    setText(prefill.text);
  }
  useEffect(() => {
    if (prefill?.nonce) taRef.current?.focus();
  }, [prefill?.nonce]);

  // Auto-grow the textarea (52 → 260px, then scroll).
  const autoGrow = useCallback(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(260, Math.max(52, ta.scrollHeight))}px`;
  }, []);
  useEffect(() => {
    autoGrow();
  }, [text, autoGrow]);

  // Close either menu on any outside click.
  useEffect(() => {
    if (!menuOpen && !attachOpen) return;
    const close = (e: MouseEvent) => {
      const target = e.target instanceof Node ? e.target : null;
      if (target && selRef.current?.contains(target)) return;
      if (target && attachRef.current?.contains(target)) return;
      setMenuOpen(false);
      setAttachOpen(false);
      setSkillsOpen(false);
    };
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [menuOpen, attachOpen]);

  // ---- attachments -----------------------------------------------------------

  const attachFiles = useCallback(
    (files: FileList | File[] | null | undefined) => {
      if (scheduleMode) return;
      const picked = Array.from(files ?? []);
      if (picked.length === 0) return;
      if (!uploadSlug) {
        toastBus.handler?.info(t("attachNeedProject"));
        return;
      }
      const slug = uploadSlug;
      for (const file of picked) {
        const id = nextAttachmentId();
        const kind = file.type.startsWith("image/") ? "image" : "file";
        setAttachments((current) => [
          ...current,
          { id, kind, name: file.name || "upload.bin", status: "uploading" },
        ]);
        uploadAttachment(slug, file)
          .then((stored) => {
            setAttachments((current) =>
              current.map((a) =>
                a.id === id
                  ? { ...a, kind: stored.kind, name: stored.name, path: stored.path, status: "ready" }
                  : a,
              ),
            );
          })
          .catch((e) => {
            setAttachments((current) => current.filter((a) => a.id !== id));
            if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
            toastBus.handler?.error(
              `${t("uploadFailed")}: ${e instanceof Error ? e.message : "unknown"}`,
            );
          });
      }
    },
    [uploadSlug, scheduleMode, t],
  );

  const toggleSkill = useCallback((skill: string, scope: "project" | "global" = "project") => {
    setAttachments((current) => {
      const existing = current.find(
        (a) =>
          a.kind === "skill" &&
          a.name === skill &&
          (scope === "global" ? a.scope === "global" : a.scope !== "global"),
      );
      if (existing) return current.filter((a) => a.id !== existing.id);
      return [
        ...current,
        {
          id: nextAttachmentId(),
          kind: "skill",
          name: skill,
          status: "ready",
          ...(scope === "global" ? { scope: "global" as const } : {}),
        },
      ];
    });
  }, []);

  const removeAttachment = useCallback((id: string) => {
    setAttachments((current) => current.filter((a) => a.id !== id));
  }, []);

  const openAttachMenu = useCallback(() => {
    if (!uploadSlug) {
      toastBus.handler?.info(t("attachNeedProject"));
      return;
    }
    setSkillsOpen(false);
    setAttachOpen((open) => !open);
  }, [uploadSlug, t]);

  /** Expand/collapse the folded skills submenu; fetch the lists lazily on
   *  first expand — the project list always, the global library only for
   *  admins (the render-phase reset clears both caches on a project switch). */
  const toggleSkillsOpen = useCallback(() => {
    setSkillsOpen((open) => {
      const next = !open;
      if (next && uploadSlug && (skills === null || (isAdmin && globalSkills === null))) {
        const lists = fetchSkillLists(uploadSlug, isAdmin);
        // One list may already be cached (e.g. /me resolved admin AFTER the
        // first expand) — refresh only the stale one, and settle the other so
        // its promise can't go unhandled.
        if (skills === null) {
          lists.project.then(setSkills).catch(() => setSkills([]));
        } else {
          lists.project.catch(() => {});
        }
        if (lists.global && globalSkills === null) {
          lists.global.then(setGlobalSkills).catch(() => setGlobalSkills([]));
        }
      }
      return next;
    });
  }, [skills, globalSkills, uploadSlug, isAdmin]);

  // ---- send ------------------------------------------------------------------

  const resolvedScheduleWhen = scheduleMode
    ? buildScheduleWhen({
        chip: scheduleChip,
        minutes: scheduleMinutes,
        hours: scheduleHours,
        absolute: scheduleAbsolute,
      })
    : null;

  const clearScheduleFields = useCallback(() => {
    setScheduleChip("");
    setScheduleMinutes("");
    setScheduleHours("");
    setScheduleAbsolute("");
  }, []);

  const send = useCallback(() => {
    const trimmed = text.trim();
    if (scheduleMode) {
      if (!trimmed) {
        toastBus.handler?.info(t("emptyInput"));
        return;
      }
      const when = buildScheduleWhen({
        chip: scheduleChip,
        minutes: scheduleMinutes,
        hours: scheduleHours,
        absolute: scheduleAbsolute,
      });
      if (!when) {
        toastBus.handler?.info(t("schedulePickTime"));
        return;
      }
      const keep = onSchedule?.(trimmed, when) === false;
      if (keep) return;
      setText("");
      clearScheduleFields();
      setScheduleMode(false);
      try {
        localStorage.removeItem(draftStorageKey(draftKey));
      } catch {
        /* ignore */
      }
      return;
    }
    if (!trimmed && attachments.length === 0) {
      toastBus.handler?.info(t("emptyInput"));
      return;
    }
    if (attachmentsBlockSend(attachments)) {
      toastBus.handler?.info(t("uploadingWait"));
      return;
    }
    const keep = onSend(trimmed, attachmentsPayload(attachments)) === false;
    if (keep) return;
    setText("");
    setAttachments([]);
    try {
      localStorage.removeItem(draftStorageKey(draftKey));
    } catch {
      /* ignore */
    }
  }, [
    text,
    attachments,
    onSend,
    onSchedule,
    scheduleMode,
    scheduleChip,
    scheduleMinutes,
    scheduleHours,
    scheduleAbsolute,
    clearScheduleFields,
    draftKey,
    t,
  ]);

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (
        shouldSubmitOnEnter({
          key: e.key,
          shiftKey: e.shiftKey,
          isComposing: e.nativeEvent.isComposing || composingRef.current,
          keyCode: e.keyCode,
        })
      ) {
        e.preventDefault();
        send();
      }
    },
    [send],
  );

  const toggleHitl = () => {
    if (locked) {
      toastBus.handler?.info(
        lang === "en"
          ? "Permission mode is fixed at spawn — start a new session to change it."
          : "批准模式在 spawn 时固定 —— 新建会话可更换。",
      );
      return;
    }
    const next = { ...draft, hitl: !draft.hitl };
    onDraftChange(next);
    toastBus.handler?.info(next.hitl ? t("hitlOn") : t("hitlOff"));
  };

  const pickModel = (vendor: string, model: string) => {
    if (locked) {
      toastBus.handler?.info(
        lang === "en"
          ? "Model/protocol are fixed for this session — use /model or a new session."
          : "本会话模型/协议已固定 —— 可发 /model 或新建会话。",
      );
      setMenuOpen(false);
      return;
    }
    const spec = vendorSpec(vendor);
    onDraftChange(
      normalizeDraft({
        ...draft,
        vendor: spec.id,
        model,
        protocol: draft.vendor === spec.id ? draft.protocol : spec.protocols[0]!.id,
      }),
    );
    setMenuOpen(false);
  };

  const pickEffort = (key: ComposerDraft["effortKey"]) => {
    if (locked) {
      setMenuOpen(false);
      return;
    }
    onDraftChange({ ...draft, effortKey: key });
    setMenuOpen(false);
  };

  const pickProtocol = (id: string) => {
    if (locked) {
      setMenuOpen(false);
      return;
    }
    onDraftChange({ ...draft, protocol: id });
    setMenuOpen(false);
    toastBus.handler?.info(`${t("protoToast")}${id}`);
  };

  const spec = vendorSpec(draft.vendor);
  // Vendor is ALWAYS spelled out next to the model — a bare "默认"/"opus"
  // plus a colored dot left the harness unreadable (owner feedback).
  const modelText = modelLabel ?? draft.model;
  const showStop = !!busy && !scheduleMode && !text.trim() && !!onStop;
  const protocols = visibleProtocols(draft.vendor, isAdmin);
  const sendable = scheduleMode
    ? !!text.trim() && !!resolvedScheduleWhen
    : !!text.trim() || attachments.length > 0;
  /* eslint-disable react-hooks/purity -- the preview intentionally tracks the wall clock on each render and does not mutate component state. */
  const schedulePreview = resolvedScheduleWhen
    ? scheduleWhenPreview(resolvedScheduleWhen, Date.now(), lang === "en" ? "en-US" : "zh-CN")
    : null;
  /* eslint-enable react-hooks/purity */

  return (
    <div
      className={`composer ${sendable ? "has-text" : ""}`}
      onDragOver={(e) => {
        if (e.dataTransfer?.types?.includes("Files")) e.preventDefault();
      }}
      onDrop={(e) => {
        if (!e.dataTransfer?.files?.length) return;
        e.preventDefault();
        attachFiles(e.dataTransfer.files);
      }}
    >
      {topSlot}
      {scheduleMode ? (
        <div className="schedule-controls" data-testid="schedule-controls">
          <div className="schedule-row">
            <span className="schedule-label">{t("scheduleIn")}</span>
            <label className="schedule-num">
              <input
                type="number"
                min={0}
                step={1}
                inputMode="numeric"
                placeholder="0"
                data-testid="schedule-minutes"
                aria-label={t("scheduleMinutes")}
                value={scheduleMinutes}
                onChange={(e) => {
                  setScheduleMinutes(e.currentTarget.value);
                  setScheduleChip("");
                  setScheduleAbsolute("");
                }}
              />
              <span>{t("scheduleMinutes")}</span>
            </label>
            <label className="schedule-num">
              <input
                type="number"
                min={0}
                step={1}
                inputMode="numeric"
                placeholder="0"
                data-testid="schedule-hours"
                aria-label={t("scheduleHours")}
                value={scheduleHours}
                onChange={(e) => {
                  setScheduleHours(e.currentTarget.value);
                  setScheduleChip("");
                  setScheduleAbsolute("");
                }}
              />
              <span>{t("scheduleHours")}</span>
            </label>
            {(["+15m", "+30m", "+1h", "+2h"] as const).map((chip) => (
              <button
                key={chip}
                type="button"
                className={scheduleChip === chip ? "selected" : ""}
                data-testid={`schedule-chip-${chip}`}
                onClick={() => {
                  setScheduleChip(chip);
                  setScheduleMinutes("");
                  setScheduleHours("");
                  setScheduleAbsolute("");
                }}
              >
                {chip}
              </button>
            ))}
          </div>
          <div className="schedule-row">
            <span className="schedule-label">{t("scheduleOrAt")}</span>
            <input
              type="datetime-local"
              data-testid="schedule-absolute"
              value={scheduleAbsolute}
              aria-label={t("scheduleDateTime")}
              onChange={(event) => {
                setScheduleAbsolute(event.currentTarget.value);
                setScheduleChip("");
                setScheduleMinutes("");
                setScheduleHours("");
              }}
            />
          </div>
          <div className="schedule-hint" data-testid="schedule-hint">
            {schedulePreview && resolvedScheduleWhen ? (
              <span>
                {t("schedulePreview")}: {schedulePreview}{" "}
                <code>{resolvedScheduleWhen}</code>
              </span>
            ) : (
              <span>{t("schedulePickTime")}</span>
            )}
            <span className="schedule-tz-note" title={scheduleTimezone || undefined}>
              {t("scheduleTzNote")}
              {scheduleTimezone ? ` (${scheduleTimezone})` : ""}
            </span>
          </div>
        </div>
      ) : null}
      {attachments.length > 0 ? (
        <div className="att-chips" data-testid="att-chips">
          {attachments.map((a) => (
            <span
              key={a.id}
              className={`att-chip ${a.kind} ${a.status}`}
              title={a.kind === "skill" ? `skill: ${a.name}` : a.name}
            >
              {a.status === "uploading" ? (
                <Loader2 className="spin" />
              ) : a.kind === "skill" ? (
                <Sparkles />
              ) : a.kind === "image" ? (
                <ImageIcon />
              ) : (
                <FileText />
              )}
              <span className="att-name">{a.name}</span>
              <button
                type="button"
                className="att-x"
                aria-label={`remove ${a.name}`}
                onClick={() => removeAttachment(a.id)}
              >
                <X />
              </button>
            </span>
          ))}
        </div>
      ) : null}
      <textarea
        ref={taRef}
        data-testid="composer-textarea"
        rows={2}
        value={text}
        disabled={disabled}
        placeholder={t(placeholderKey)}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        onPaste={(e) => {
          if (e.clipboardData?.files?.length) {
            e.preventDefault();
            attachFiles(e.clipboardData.files);
          }
        }}
        onCompositionStart={() => {
          composingRef.current = true;
        }}
        onCompositionEnd={() => {
          composingRef.current = false;
        }}
      />
      <input
        ref={fileRef}
        type="file"
        multiple
        hidden
        data-testid="composer-file-input"
        onChange={(e) => {
          attachFiles(e.currentTarget.files);
          e.currentTarget.value = "";
        }}
      />
      <div className="composer-row">
        {onSchedule ? (
          <button
            type="button"
            className={`icon-btn ${scheduleMode ? "schedule-on" : ""}`}
            data-testid="schedule-toggle"
            title={t("scheduleToggle")}
            aria-label={t("scheduleToggle")}
            onClick={() => {
              setScheduleMode((current) => {
                const next = !current;
                if (next) {
                  setAttachments([]);
                  setAttachOpen(false);
                } else {
                  clearScheduleFields();
                }
                return next;
              });
            }}
          >
            <Clock />
          </button>
        ) : null}
        {!scheduleMode ? <div className={`sel ${attachOpen ? "open" : ""}`} ref={attachRef}>
          <button
            type="button"
            className="icon-btn"
            data-testid="attach-btn"
            title={t("attachTip")}
            aria-label="attach"
            onClick={(e) => {
              e.stopPropagation();
              openAttachMenu();
            }}
          >
            <Plus />
          </button>
          <div
            className="sel-menu drop-up attach-menu"
            style={{ minWidth: 240 }}
            data-testid="attach-menu"
          >
            <button
              type="button"
              className="sel-item"
              data-testid="attach-files"
              onClick={() => {
                setAttachOpen(false);
                fileRef.current?.click();
              }}
            >
              <ImageIcon />
              {t("attachFiles")}
            </button>
            {/* Skills fold into a collapsed submenu row (claude.ai-style
                "Skills ›"); names only — the long model-trigger descriptions
                stay off the menu (hover a name for the full text). */}
            <button
              type="button"
              className="sel-item"
              data-testid="attach-skills"
              onClick={toggleSkillsOpen}
            >
              <Sparkles />
              {t("attachSkillGroup")}
              <ChevronRight className={`chev ${skillsOpen ? "open" : ""}`} />
            </button>
            {skillsOpen ? (
              <SkillMenuSections
                lang={lang}
                isAdmin={isAdmin}
                skills={skills}
                globalSkills={globalSkills}
                attachments={attachments}
                onToggleSkill={toggleSkill}
              />
            ) : null}
          </div>
        </div> : null}
        <button
          type="button"
          data-testid="hitl-toggle"
          className={`hitl-btn ${draft.hitl ? "on" : ""}`}
          onClick={toggleHitl}
          title={draft.hitl ? t("hitlOn") : t("hitlOff")}
        >
          <Hand />
          <span>{t("approve")}</span>
        </button>
        <div className="right">
          <div className={`sel ${menuOpen ? "open" : ""}`} ref={selRef}>
            <button
              type="button"
              className="model-btn"
              data-testid="model-btn"
              onClick={(e) => {
                e.stopPropagation();
                setMenuOpen((o) => !o);
              }}
            >
              <span className={`dot ${spec.id}`} />
              <span>{modelText ? `${spec.label} · ${modelText}` : spec.label}</span>
              <span className="eff">{t(draft.effortKey)}</span>
            </button>
            <div className="sel-menu drop-up align-right" style={{ minWidth: 280 }} data-testid="model-menu">
              {(allowedVendors
                ? VENDORS.filter((v) => allowedVendors.includes(v.id))
                : VENDORS
              ).map((v) => (
                <div key={v.id}>
                  <div className="sel-group">{v.label}</div>
                  {v.models.map((m) => (
                    <button
                      key={`${v.id}-${m}`}
                      type="button"
                      className={`sel-item ${draft.vendor === v.id && draft.model === m ? "selected" : ""}`}
                      onClick={() => pickModel(v.id, m)}
                    >
                      <span className={`dot ${v.id}`} />
                      {m}
                      <span className="check">✓</span>
                    </button>
                  ))}
                </div>
              ))}
              <div className="sel-group">{t("effort")}</div>
              {EFFORT_KEYS.map((k) => (
                <button
                  key={k}
                  type="button"
                  className={`sel-item ${draft.effortKey === k ? "selected" : ""}`}
                  onClick={() => pickEffort(k)}
                >
                  {t(k)}
                  <span className="check">✓</span>
                </button>
              ))}
              <div className="sel-group">
                {t("protocol")}({spec.label})
              </div>
              {protocols.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className={`sel-item ${draft.protocol === p.id ? "selected" : ""}`}
                  onClick={() => pickProtocol(p.id)}
                >
                  {p.label}
                  <span className="sub">{p.sub}</span>
                  <span className="check">✓</span>
                </button>
              ))}
            </div>
          </div>
          <button type="button" className="icon-btn" title="mic" aria-label="mic">
            <Mic />
          </button>
          {showStop ? (
            <button
              type="button"
              data-testid="composer-stop"
              className="send-btn stop"
              onClick={onStop}
              title={t("stopTurnTip")}
            >
              <Square />
            </button>
          ) : (
            <button
              type="button"
              data-testid={sendTestId}
              className="send-btn"
              onClick={send}
              disabled={disabled || (scheduleMode && !resolvedScheduleWhen)}
              title={scheduleMode ? t("scheduleSend") : t("sendTip")}
            >
              {scheduleMode ? <Clock /> : <ArrowUp />}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
