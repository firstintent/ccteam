// v0.8.19 W2 — Badge. Centralizes the ~6 differently-spelled status-pill
// implementations (Marketplace source/installed, StatusView activity, Hosts
// ready/needs-config) into one variant prop.

import { cva, type VariantProps } from "class-variance-authority";
import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

// eslint-disable-next-line react-refresh/only-export-components -- CVA variants intentionally co-located with the component (shadcn convention).
export const badgeVariants = cva(
  "inline-flex items-center gap-1 whitespace-nowrap rounded-full px-2 py-0.5 text-[11px] font-medium leading-tight",
  {
    variants: {
      variant: {
        default: "bg-surface-800 text-text-secondary",
        running: "bg-status-running/15 text-status-running",
        waiting: "bg-status-waiting/15 text-status-waiting",
        idle: "bg-surface-700/40 text-text-muted",
        error: "bg-status-error/15 text-status-error",
        brand: "bg-brand-500/15 text-brand-400",
        accent: "bg-accent-500/15 text-accent-500",
        outline: "border border-surface-700 text-text-secondary",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

export type BadgeProps = HTMLAttributes<HTMLSpanElement> & VariantProps<typeof badgeVariants>;

export function Badge({ className, variant, ...props }: BadgeProps) {
  return <span className={cn(badgeVariants({ variant }), className)} {...props} />;
}
