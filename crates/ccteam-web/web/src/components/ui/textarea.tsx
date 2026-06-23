// v0.8.19 W2 — Textarea. `field-sizing-content` auto-grows to content with
// zero JS (capped by the caller's max-h).

import type { TextareaHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Textarea({ className, ...props }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        "field-sizing-content min-h-16 w-full rounded-md border border-surface-700 bg-surface-850 px-3 py-2 text-sm text-text-primary outline-none transition-colors placeholder:text-text-dim focus:border-brand-500 focus:ring-1 focus:ring-brand-500/40 disabled:opacity-50",
        className,
      )}
      {...props}
    />
  );
}
