// v0.8.19 W2 — the standard shadcn class-merge helper. `clsx` resolves
// conditional class lists; `tailwind-merge` then dedupes CONFLICTING Tailwind
// utilities so a caller's `className` reliably overrides a component's defaults
// (e.g. a passed `bg-accent-500` wins over a variant's `bg-brand-500`).

import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
