// V0.3.2 F58 — pause / resume buttons.
//
// Two-button pair gated by the project's current pause state. Pause
// sets `state.user_pause_pending=true` (server side via the F52 JSON
// contract — actions::pause in ccteam-core); resume clears it plus
// `user_attached`. Neither kills the tmux session
// (CLAUDE.md §三 red lines).
//
// Optimistic UI: while a request is in flight, both buttons are
// disabled to avoid double-fire. On error, surface via toast and let
// the parent's next state poll correct the visible "paused" badge.
//
// Wired into pages/ProjectDetail.tsx (project-level pause derived
// from `state.user_pause_pending`) and pages/SessionDetail.tsx
// (session-scoped, currently passes `paused=false` since the F52
// SessionDetail JSON doesn't surface a session pause flag yet —
// V0.3.1 F50 keeps the flag project-scoped) by V0.3.2 F59.
//
// Test plan (JSDoc — playwright deferred per V0.3.2 PRD):
//   - PauseResumeButtons: paused=false → Pause active, Resume disabled
//   - PauseResumeButtons: paused=true  → Pause disabled, Resume active
//   - PauseResumeButtons: click Pause → server ok=true → toast
//   - PauseResumeButtons: server 4xx → error toast, no UI lockup
//
import { useState } from "react";
import { postPause, postResume } from "../lib/api";
import { toastBus } from "../lib/toastBus";

interface Props {
  slug: string;
  /** Optional flex session id. When set, routes through the session
   *  pause/resume endpoints (which still apply project-level state
   *  per V0.3.1 F50 — the sid is just validated server-side). */
  sid?: string;
  paused: boolean;
  onSuccess?: () => void;
}

export function PauseResumeButtons({ slug, sid, paused, onSuccess }: Props) {
  const [pending, setPending] = useState(false);

  async function handlePause() {
    if (pending) return;
    setPending(true);
    try {
      await postPause(slug, sid ? { sid } : undefined);
      toastBus.handler?.info("Pause requested");
      onSuccess?.();
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      const msg = err instanceof Error ? err.message : "pause failed";
      toastBus.handler?.error(msg);
    } finally {
      setPending(false);
    }
  }

  async function handleResume() {
    if (pending) return;
    setPending(true);
    try {
      await postResume(slug, sid ? { sid } : undefined);
      toastBus.handler?.info("Resume requested");
      onSuccess?.();
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      const msg = err instanceof Error ? err.message : "resume failed";
      toastBus.handler?.error(msg);
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        onClick={handlePause}
        disabled={pending || paused}
        className="px-3 py-1.5 bg-surface-700 hover:bg-surface-600 text-text-primary text-xs font-medium rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed cursor-pointer"
      >
        Pause
      </button>
      <button
        type="button"
        onClick={handleResume}
        disabled={pending || !paused}
        className="px-3 py-1.5 bg-brand-600 hover:bg-brand-700 text-white text-xs font-medium rounded-lg transition-colors disabled:opacity-40 disabled:cursor-not-allowed cursor-pointer"
      >
        Resume
      </button>
      {paused && (
        <span className="text-xs text-status-warn font-mono">paused</span>
      )}
    </div>
  );
}
