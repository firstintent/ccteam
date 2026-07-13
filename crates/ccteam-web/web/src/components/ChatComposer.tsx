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

import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowUp, Hand, Mic, Plus, Square } from "lucide-react";
import { toastBus } from "../lib/toastBus";
import { makeT, type Lang } from "../lib/i18n";
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
}: {
  /** localStorage draft scope — `"home"` or the sid. */
  draftKey: string;
  lang: Lang;
  placeholderKey?: string;
  /** A turn is in flight → the send button morphs into Stop when empty. */
  busy?: boolean;
  disabled?: boolean;
  /** Send the trimmed text. Return `false` to KEEP the draft (validation
   *  failed upstream); anything else clears it. */
  onSend: (text: string) => boolean | void;
  /** Interrupt the running turn (session kept). */
  onStop?: () => void;
  /** The model/effort/protocol/HITL draft the menu edits. */
  draft: ComposerDraft;
  onDraftChange: (next: ComposerDraft) => void;
  /** Conversation override for the model button label (live session model). */
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
}) {
  const t = makeT(lang);
  const [text, setText] = useState(() => loadDraft(draftKey));
  const [menuOpen, setMenuOpen] = useState(false);
  const composingRef = useRef(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const selRef = useRef<HTMLDivElement | null>(null);

  // Persist (or clear) this surface's draft on every change.
  useEffect(() => {
    try {
      if (text) localStorage.setItem(draftStorageKey(draftKey), text);
      else localStorage.removeItem(draftStorageKey(draftKey));
    } catch {
      /* storage disabled — in-memory draft still works */
    }
  }, [draftKey, text]);

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

  // Close the model menu on any outside click.
  useEffect(() => {
    if (!menuOpen) return;
    const close = (e: MouseEvent) => {
      if (selRef.current && e.target instanceof Node && selRef.current.contains(e.target)) return;
      setMenuOpen(false);
    };
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [menuOpen]);

  const send = useCallback(() => {
    const trimmed = text.trim();
    if (!trimmed) {
      toastBus.handler?.info(t("emptyInput"));
      return;
    }
    const keep = onSend(trimmed) === false;
    if (keep) return;
    setText("");
    try {
      localStorage.removeItem(draftStorageKey(draftKey));
    } catch {
      /* ignore */
    }
  }, [text, onSend, draftKey, t]);

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
  const showStop = !!busy && !text.trim() && !!onStop;
  const protocols = visibleProtocols(draft.vendor, isAdmin);

  return (
    <div className={`composer ${text.trim() ? "has-text" : ""}`}>
      {topSlot}
      <textarea
        ref={taRef}
        data-testid="composer-textarea"
        rows={2}
        value={text}
        disabled={disabled}
        placeholder={t(placeholderKey)}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        onCompositionStart={() => {
          composingRef.current = true;
        }}
        onCompositionEnd={() => {
          composingRef.current = false;
        }}
      />
      <div className="composer-row">
        <button type="button" className="icon-btn" title="＋" aria-label="attach">
          <Plus />
        </button>
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
              <span>{modelLabel ?? draft.model}</span>
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
              disabled={disabled}
              title={t("sendTip")}
            >
              <ArrowUp />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
