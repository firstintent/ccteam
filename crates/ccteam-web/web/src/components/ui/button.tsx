// v0.8.19 W2 — the one Button. Replaces the per-page PRIMARY_BTN_CLASS /
// GHOST_BTN_CLASS / inline button class strings. CVA variants + the
// active:translate-y-px press, focus-visible ring, disabled, and auto
// svg-sizing that demos skip.

import { cva, type VariantProps } from "class-variance-authority";
import type { ButtonHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

// eslint-disable-next-line react-refresh/only-export-components -- CVA variants intentionally co-located with the component (shadcn convention).
export const buttonVariants = cva(
  "inline-flex select-none cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-md font-medium outline-none transition-colors active:translate-y-px focus-visible:ring-2 focus-visible:ring-brand-500/60 disabled:pointer-events-none disabled:opacity-40 [&_svg]:shrink-0",
  {
    variants: {
      variant: {
        default: "bg-brand-500 text-surface-950 hover:bg-brand-400",
        outline: "border border-surface-700 text-text-primary hover:bg-surface-800",
        ghost: "text-text-secondary hover:bg-surface-800 hover:text-text-primary",
        destructive:
          "border border-status-error/35 bg-status-error/15 text-status-error hover:bg-status-error/25",
        link: "text-brand-500 underline-offset-4 hover:underline",
      },
      size: {
        sm: "h-7 px-2.5 text-xs [&_svg]:size-3.5",
        default: "h-9 px-3.5 text-sm [&_svg]:size-4",
        lg: "h-10 px-5 text-sm [&_svg]:size-4",
        icon: "h-9 w-9 [&_svg]:size-4",
        "icon-sm": "h-7 w-7 [&_svg]:size-3.5",
      },
    },
    defaultVariants: { variant: "default", size: "default" },
  },
);

export type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> &
  VariantProps<typeof buttonVariants>;

export function Button({ className, variant, size, type = "button", ...props }: ButtonProps) {
  return (
    <button type={type} className={cn(buttonVariants({ variant, size }), className)} {...props} />
  );
}
