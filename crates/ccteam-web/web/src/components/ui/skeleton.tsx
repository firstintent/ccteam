// v0.8.19 W4a — loading skeletons. Replaces the "加载…中" text placeholders
// the global views show before a fetch resolves with a content-shaped pulse
// (motion-reduce honours the accessibility opt-out). `SkeletonRows` stacks a
// few card-height bars for a list/table loading state.

import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Skeleton({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "animate-pulse rounded bg-surface-800 motion-reduce:animate-none",
        className,
      )}
      {...props}
    />
  );
}

/** A stack of `rows` full-width skeleton bars (default 3), for a list/table
 *  loading placeholder. `rowClassName` tunes each bar's height/shape. */
export function SkeletonRows({
  rows = 3,
  className,
  rowClassName,
}: {
  rows?: number;
  className?: string;
  rowClassName?: string;
}) {
  return (
    <div className={cn("space-y-3", className)}>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className={cn("h-16 w-full", rowClassName)} />
      ))}
    </div>
  );
}
