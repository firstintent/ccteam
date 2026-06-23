// v0.8.19 W2 — Card family. The hairline `ring-1 ring-surface-700/50` reads
// subtler than the solid borders StatusView/HostsView hand-roll, and the
// Header/Title/Content/Footer slots standardize the card-with-action pattern
// both pages re-derive.

import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Card({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("overflow-hidden rounded-lg bg-surface-900 ring-1 ring-surface-700/50", className)}
      {...props}
    />
  );
}

export function CardHeader({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("flex items-center gap-2 border-b border-surface-800 px-4 py-3", className)}
      {...props}
    />
  );
}

export function CardTitle({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("text-sm font-semibold text-text-primary", className)} {...props} />;
}

export function CardContent({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("px-4 py-3", className)} {...props} />;
}

export function CardFooter({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "border-t border-surface-800 bg-surface-950/40 px-4 py-2 text-xs text-text-dim",
        className,
      )}
      {...props}
    />
  );
}
