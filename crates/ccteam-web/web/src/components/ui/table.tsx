// v0.8.19 W4a — semantic table primitives. The fleet/host/list surfaces hand-
// rolled `.map` of flex rows; these give them a real <table> with token-styled
// chrome: uppercase tracked `th` (text-text-dim, hairline bottom border),
// hover-lit rows, and tabular-nums numeric cells. `SortableHeader` wraps a
// `<th>`'s label in a sort-toggle button with the lucide chevron set, driven by
// a @tanstack/react-table column's getToggleSortingHandler / getIsSorted.

import type { HTMLAttributes, TdHTMLAttributes, ThHTMLAttributes } from "react";
import { ChevronDown, ChevronsUpDown, ChevronUp } from "lucide-react";
import { cn } from "../../lib/utils";

export function Table({ className, ...props }: HTMLAttributes<HTMLTableElement>) {
  return (
    <div className="w-full overflow-x-auto">
      <table className={cn("w-full border-collapse text-sm", className)} {...props} />
    </div>
  );
}

export function TableHeader({ className, ...props }: HTMLAttributes<HTMLTableSectionElement>) {
  return <thead className={cn("", className)} {...props} />;
}

export function TableBody({ className, ...props }: HTMLAttributes<HTMLTableSectionElement>) {
  return <tbody className={cn("", className)} {...props} />;
}

export function TableRow({ className, ...props }: HTMLAttributes<HTMLTableRowElement>) {
  return (
    <tr
      className={cn(
        "border-b border-surface-800 transition-colors hover:bg-surface-800/40",
        className,
      )}
      {...props}
    />
  );
}

export function TableHead({ className, ...props }: ThHTMLAttributes<HTMLTableCellElement>) {
  return (
    <th
      className={cn(
        "border-b border-surface-700 px-3 py-2 text-left text-[11px] font-medium uppercase tracking-wide text-text-dim",
        className,
      )}
      {...props}
    />
  );
}

export function TableCell({ className, ...props }: TdHTMLAttributes<HTMLTableCellElement>) {
  return <td className={cn("px-3 py-2 align-middle text-text-secondary", className)} {...props} />;
}

/** The sort state a {@link SortableHeader} renders an indicator for. Mirrors
 *  @tanstack/react-table's `Column.getIsSorted()` return (`false` = unsorted). */
export type SortDirection = "asc" | "desc" | false;

/** A clickable `<th>` label with the lucide chevron set: ChevronsUpDown when
 *  unsorted, ChevronUp/ChevronDown for asc/desc. Pass a column's
 *  `getToggleSortingHandler()` to `onSort` and `getIsSorted()` to `sorted`.
 *  Renders inside its own <th> (the `cell` is the visible header content). */
export function SortableHeader({
  children,
  sorted,
  onSort,
  align = "left",
  className,
}: {
  children: React.ReactNode;
  sorted: SortDirection;
  onSort?: ((event: unknown) => void) | undefined;
  align?: "left" | "right";
  className?: string;
}) {
  const Icon = sorted === "asc" ? ChevronUp : sorted === "desc" ? ChevronDown : ChevronsUpDown;
  return (
    <th
      className={cn(
        "border-b border-surface-700 px-3 py-2 text-[11px] font-medium uppercase tracking-wide text-text-dim",
        align === "right" ? "text-right" : "text-left",
        className,
      )}
    >
      <button
        type="button"
        onClick={onSort}
        className={cn(
          "group inline-flex items-center gap-1 uppercase tracking-wide outline-none transition-colors hover:text-text-secondary focus-visible:text-text-secondary",
          align === "right" ? "flex-row-reverse" : "",
          !onSort && "cursor-default",
        )}
        aria-label="排序"
      >
        {children}
        <Icon
          className={cn(
            "size-3 shrink-0",
            sorted ? "text-text-secondary" : "text-text-dim/60 group-hover:text-text-dim",
          )}
          aria-hidden
        />
      </button>
    </th>
  );
}
