// v0.10.2 (WEB-DSH-1) — the persistent keep-alive host for the DSH iframe.
// ChatConsole renders this ONCE inside <main>, outside the mutually-exclusive
// view switch, and ChatConsole itself survives route changes (same element
// type on every route) — so the <iframe> is never unmounted by navigation and
// the DSH SPA inside it keeps its state (running turns, sockets, scroll,
// drafts). Off `/dsh` the stage is hidden with `display:none`; DshView owns
// the status head + empty states in place.
//
// Lazy: renders nothing until the first `/dsh` visit flips `visited` in the
// shared store (zero dsh requests for users who never open the page). And
// `embedSrc` is null while stopped/disabled/starting — stop→start passes src
// through null, so a new instance deliberately gets a fresh iframe.

import { embedSrc } from "../lib/dshApi";
import { useDshStatus } from "../hooks/dshStore";

export default function DshFrameHost({ active }: { active: boolean }) {
  const { visited, status } = useDshStatus();
  const src = embedSrc(status);
  if (!visited || src == null) return null;
  return (
    <div className="dsh-stage dsh-keepalive" data-testid="dsh-frame-host" hidden={!active}>
      <iframe
        className="dsh-frame"
        src={src}
        title="DeepSeek Harness"
        data-testid="dsh-frame"
        // DSH agents run bash under the user's approval — this is the same
        // trust level as the tenant's own chat sessions (redline §五).
        allow="clipboard-read; clipboard-write"
      />
    </div>
  );
}
