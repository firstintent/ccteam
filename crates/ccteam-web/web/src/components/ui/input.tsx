// v0.8.19 W2 — Input. Shared focus ring / aria-invalid / disabled styling;
// replaces SettingsPage's FIELD_CLASS.

import type { InputHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Input({ className, ...props }: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn(
        "h-9 w-full rounded-md border border-surface-700 bg-surface-850 px-3 text-sm text-text-primary outline-none transition-colors placeholder:text-text-dim focus:border-brand-500 focus:ring-1 focus:ring-brand-500/40 disabled:opacity-50 aria-invalid:border-status-error aria-invalid:ring-1 aria-invalid:ring-status-error/40",
        className,
      )}
      {...props}
    />
  );
}
