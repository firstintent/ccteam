// V0.3.2 F58 — `/btw` write-action form.
//
// Posts free-text into a project's (or flex session's) inbox via the
// F52 JSON contract (`POST /api/<slug>[/<sid>]/btw`). Caps text at 4000
// chars client-side (mirrors actions.rs `BTW_MAX`). On success: clears
// the textarea + emits a "BTW submitted" toast. On failure: surfaces
// the server's error string as an error toast and keeps the textarea
// intact so the operator can retry without retyping.
//
// Wired into pages/ProjectDetail.tsx (project-level) and
// pages/SessionDetail.tsx (flex session) by V0.3.2 F59.
//
// Test plan (JSDoc — playwright deferred per V0.3.2 PRD):
//   - BtwForm: enter text, submit, server returns ok=true → textarea
//     cleared + "BTW submitted" toast
//   - BtwForm: server returns ok=false → error toast, textarea preserved
//   - BtwForm: server 401 → TokenEntryGate intercepts (manual)
//   - BtwForm: char counter increments correctly, submit disabled when
//     empty or over cap
//
import { useState } from "react";
import { BTW_MAX, postBtw } from "../lib/api";
import { toastBus } from "../lib/toastBus";

interface Props {
  slug: string;
  /** Optional flex session id. When present, posts go to the session
   *  private inbox (`/api/<slug>/<sid>/btw`). */
  sid?: string;
  /** Optional success hook — detail pages can use this to re-fetch
   *  project state after the orchestrator picks up the inbox file. */
  onSuccess?: () => void;
}

export function BtwForm({ slug, sid, onSuccess }: Props) {
  const [text, setText] = useState("");
  const [pending, setPending] = useState(false);

  const trimmed = text.trim();
  const overCap = text.length > BTW_MAX;
  const canSubmit = !pending && trimmed.length > 0 && !overCap;

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;
    setPending(true);
    try {
      await postBtw(slug, text, sid ? { sid } : undefined);
      toastBus.handler?.info("BTW submitted");
      setText("");
      onSuccess?.();
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") {
        // The interceptor already flipped the gate; don't toast.
        return;
      }
      const msg = err instanceof Error ? err.message : "BTW failed";
      toastBus.handler?.error(msg);
    } finally {
      setPending(false);
    }
  }

  return (
    <form onSubmit={handleSubmit} className="flex flex-col gap-2">
      <label htmlFor={`btw-${slug}-${sid ?? "project"}`} className="text-xs text-text-muted font-medium">
        BTW (1..={BTW_MAX} chars)
      </label>
      <textarea
        id={`btw-${slug}-${sid ?? "project"}`}
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={3}
        disabled={pending}
        spellCheck={false}
        placeholder="Inject a note into the next phase boundary..."
        className="w-full px-3 py-2 bg-surface-900 border border-surface-700/60 rounded-lg text-text-primary text-sm font-mono placeholder:text-text-dim focus:outline-none focus:ring-2 focus:ring-brand-600 focus:border-transparent disabled:opacity-50 resize-y"
      />
      <div className="flex items-center justify-between text-xs">
        <span className={overCap ? "text-status-error font-mono" : "text-text-dim font-mono"}>
          {text.length}/{BTW_MAX}
        </span>
        <button
          type="submit"
          disabled={!canSubmit}
          className="px-3 py-1.5 bg-brand-600 hover:bg-brand-700 text-white text-xs font-medium rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
        >
          {pending ? "Submitting..." : "Submit BTW"}
        </button>
      </div>
    </form>
  );
}
