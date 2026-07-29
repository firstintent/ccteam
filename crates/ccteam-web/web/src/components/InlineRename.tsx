// Session rename, one behaviour for every surface that offers it (the
// conversation header title, the sidebar rows). Keeping the edit affordance
// in ONE component is what stops the two from drifting on the details users
// actually feel: what Enter/Esc/blur do, whether a blank title clears a name
// (it must not), and whether re-typing the same title fires a pointless
// request (it must not).
//
// The decision itself is a pure function (`resolveRenameInput`) so it is unit
// tested directly — this repo has no DOM-interaction harness.

import { useEffect, useRef, useState } from "react";

/** What an edit session should do when the user commits the field. */
export type RenameDecision =
  | { action: "submit"; title: string }
  | { action: "cancel" };

/** Decide what a committed input means, given what the title was before:
 *  - blank (or whitespace-only) → cancel; a rename never CLEARS a title
 *    (the server rejects a blank one anyway — this keeps the UI from making
 *    a request it knows will 400).
 *  - unchanged after trimming → cancel; no request, no toast.
 *  - otherwise → submit the trimmed text (the server still applies its own
 *    rule-based collapse/truncation — this is not a second implementation of
 *    that, just an obvious-no-op filter). */
// eslint-disable-next-line react-refresh/only-export-components -- pure decision helper co-located with its only consumer for unit tests.
export function resolveRenameInput(raw: string, previous: string): RenameDecision {
  const title = raw.trim();
  if (!title) return { action: "cancel" };
  if (title === previous.trim()) return { action: "cancel" };
  return { action: "submit", title };
}

/** An inline text field that replaces a label while editing.
 *
 *  Keys: Enter commits, Escape cancels (and never commits), blur commits —
 *  the same triad every inline rename in a file manager / chat app uses, so
 *  nobody has to learn ours. `onSubmit` only fires for a real change. */
export function InlineRename({
  initial,
  onSubmit,
  onCancel,
  ariaLabel,
  className,
  maxLength = 120,
}: {
  initial: string;
  onSubmit: (title: string) => void;
  onCancel: () => void;
  ariaLabel: string;
  className?: string;
  maxLength?: number;
}) {
  const [value, setValue] = useState(initial);
  const ref = useRef<HTMLInputElement | null>(null);
  // Enter and Escape both END the edit, which unmounts this input — and an
  // unmounting input can still fire `blur`. Latch the first outcome so that
  // blur can't re-commit it (two PATCHes + two toasts for one Enter) or undo
  // an Escape.
  const doneRef = useRef(false);

  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  const commit = () => {
    if (doneRef.current) return;
    doneRef.current = true;
    const decision = resolveRenameInput(value, initial);
    if (decision.action === "submit") onSubmit(decision.title);
    else onCancel();
  };

  return (
    <input
      ref={ref}
      className={className}
      type="text"
      aria-label={ariaLabel}
      value={value}
      maxLength={maxLength}
      onChange={(e) => setValue(e.target.value)}
      onBlur={commit}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        // The rail row and the composer both listen for these keys; an edit
        // in progress owns them.
        e.stopPropagation();
        if (e.key === "Enter") {
          e.preventDefault();
          commit();
        } else if (e.key === "Escape") {
          e.preventDefault();
          if (doneRef.current) return;
          doneRef.current = true;
          onCancel();
        }
      }}
    />
  );
}
