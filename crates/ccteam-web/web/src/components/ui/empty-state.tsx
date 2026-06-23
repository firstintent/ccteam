// v0.8.19 W4a — the shared empty-state shape. The global views each hand-rolled
// a dashed-box text-only "nothing here" message; this centers a lucide icon +
// title + optional description and an optional CTA, so Status / Hosts /
// Marketplace read consistent when empty. Theme tokens only.

import type { LucideIcon } from "lucide-react";
import { cn } from "../../lib/utils";

export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  className,
  ...rest
}: {
  icon: LucideIcon;
  title: string;
  description?: string;
  action?: React.ReactNode;
  className?: string;
} & { "data-testid"?: string }) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-surface-700/60 bg-surface-900/40 px-4 py-10 text-center",
        className,
      )}
      {...rest}
    >
      <Icon className="size-7 text-text-dim" aria-hidden />
      <div className="text-sm font-medium text-text-secondary">{title}</div>
      {description ? <div className="max-w-sm text-xs text-text-dim">{description}</div> : null}
      {action ? <div className="mt-1">{action}</div> : null}
    </div>
  );
}
