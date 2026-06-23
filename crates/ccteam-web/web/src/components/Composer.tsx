// v0.8.19 W1 — the message composer, extracted from SessionView's inline
// textarea. Fixes the owner-reported #1 bug: pressing Enter while an IME
// (Chinese/Japanese) candidate is being composed used to SEND a half-typed
// message. The keydown now guards on `isComposing` so Enter only confirms the
// candidate mid-composition and only sends a finished line.
//
// Also: Shift+Enter inserts a newline, Cmd/Ctrl+Enter sends, the textarea
// auto-grows (field-sizing), the draft persists per-sid (so switching sessions
// never loses an unsent message), and the send button morphs into a red Stop
// while a turn is in flight with an empty draft (so you can either queue a
// follow-up or interrupt).

import { useCallback, useEffect, useRef, useState } from "react";
import { Send, Square } from "lucide-react";

/** Pure decision for the composer's Enter keydown — extracted so the IME guard
 *  (the owner's #1 bug) is unit-testable in the node/SSR test env. Returns
 *  false while a CJK candidate is composing (`isComposing` / legacy keyCode
 *  229), on Shift+Enter (newline), and for any non-Enter key; true for a plain
 *  Enter or Cmd/Ctrl+Enter on a finished line. */
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

/** Per-sid draft key — an unsent message survives a session switch (SessionView
 *  is keyed by sid and unmounts on switch, so without this the draft is lost). */
const draftKey = (sid: string) => `ccteam.draft.v1.${sid}`;

function loadDraft(sid: string): string {
  try {
    return localStorage.getItem(draftKey(sid)) ?? "";
  } catch {
    return "";
  }
}

export function Composer({
  sid,
  busy,
  onSubmit,
  onStop,
  placeholder,
}: {
  sid: string;
  busy?: boolean;
  onSubmit: (text: string) => void;
  onStop?: () => void;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState(() => loadDraft(sid));
  // True between compositionstart/end — the belt to `isComposing`'s suspenders.
  const composingRef = useRef(false);

  // Persist (or clear) this sid's draft on every change.
  useEffect(() => {
    try {
      if (draft) localStorage.setItem(draftKey(sid), draft);
      else localStorage.removeItem(draftKey(sid));
    } catch {
      // storage disabled — in-memory draft still works.
    }
  }, [sid, draft]);

  const send = useCallback(() => {
    const text = draft.trim();
    if (!text) return;
    onSubmit(text);
    setDraft("");
    try {
      localStorage.removeItem(draftKey(sid));
    } catch {
      // ignore
    }
  }, [draft, onSubmit, sid]);

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      // IME guard — `isComposing` (+ the `composingRef` belt) keeps Enter from
      // submitting while a CJK candidate is being confirmed.
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

  const showStop = !!busy && !draft.trim() && !!onStop;

  return (
    <div className="border-t border-surface-700/40 p-3">
      <div className="flex items-end gap-1 rounded-md border border-surface-700 bg-surface-800 transition-colors focus-within:border-brand-500">
        <textarea
          data-testid="composer-textarea"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKeyDown}
          onCompositionStart={() => {
            composingRef.current = true;
          }}
          onCompositionEnd={() => {
            composingRef.current = false;
          }}
          rows={1}
          className="field-sizing-content max-h-40 min-h-9 flex-1 resize-none bg-transparent px-3 py-2 text-sm outline-none"
          placeholder={placeholder ?? "发消息 / 命令(/compact /clear /model …)…"}
        />
        {showStop ? (
          <button
            type="button"
            data-testid="composer-stop"
            onClick={onStop}
            title="停止当前回合"
            className="m-1 grid h-9 w-9 shrink-0 place-items-center rounded-md bg-status-error text-white hover:opacity-90"
          >
            <Square className="h-4 w-4" />
          </button>
        ) : (
          <button
            type="button"
            data-testid="composer-send"
            onClick={send}
            disabled={!draft.trim()}
            title="发送 (Enter)"
            className="m-1 grid h-9 w-9 shrink-0 place-items-center rounded-md bg-brand-500 text-surface-950 hover:bg-brand-400 disabled:opacity-40"
          >
            <Send className="h-4 w-4" />
          </button>
        )}
      </div>
      <div className="mt-1 px-1 text-[10px] text-text-dim">
        Enter 发送 · Shift+Enter 换行 · 输入法候选回车不误发
      </div>
    </div>
  );
}
