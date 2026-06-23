// v0.8.19 W2 — Label.

import type { LabelHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Label({ className, ...props }: LabelHTMLAttributes<HTMLLabelElement>) {
  return <label className={cn("block text-xs font-medium text-text-dim", className)} {...props} />;
}
