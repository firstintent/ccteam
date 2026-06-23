// v0.8.19 W3a — the one Dialog. A hand-rolled overlay primitive (NOT a vendor
// dependency).
//
// Why hand-rolled, not @base-ui/react: Base UI's Dialog mounts its body through
// a client-only Portal — under bare-node `renderToString` (the env our vitest
// suite runs in: no jsdom) it renders to an EMPTY STRING. Our SSR smoke tests
// (MarketplaceView.test / ChatConsole.modal.test) assert the dialog body's
// content is present in the server HTML, so a portal-only overlay is unworkable
// here. This component renders its panel INLINE on the server (so the body is in
// the SSR output) and only switches to a `createPortal(document.body)` overlay
// once mounted on the client.
//
// Client affordances: backdrop blur + fade/slide animations, focus the panel on
// open + restore focus on close, focus-trap-lite (Tab/Shift+Tab cycle within the
// panel), Escape closes, click-away (backdrop) closes, body-scroll lock, and
// `role="dialog" aria-modal`.

import {
  useCallback,
  useEffect,
  useRef,
  useSyncExternalStore,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { cn } from "../../lib/utils";

// SSR-safe "are we on the client yet?" — the server snapshot is `false`, the
// client snapshot `true`, so the first client paint hydrates with the same
// (inline) tree the server produced, then re-renders to the portal. Avoids a
// setState-in-effect mount flag (which the react-hooks lint rejects).
const noopSubscribe = () => () => {};
const useMounted = () =>
  useSyncExternalStore(
    noopSubscribe,
    () => true,
    () => false,
  );

export interface DialogProps {
  open: boolean;
  onClose: () => void;
  /** Accessible name; rendered as the panel header unless `header` is given. */
  title?: ReactNode;
  /** A fully custom header (replaces the default title + close-button row). */
  header?: ReactNode;
  /** Hide the default top-right close button (e.g. a custom header owns it). */
  hideCloseButton?: boolean;
  /** Where the dialog sits. `center` = a centered card; `end` = a right-edge
   *  drawer (the marketplace detail panel). */
  placement?: "center" | "end";
  /** `aria-label` when `title` isn't a plain string. */
  ariaLabel?: string;
  className?: string;
  /** Extra classes on the backdrop wrapper (e.g. padding tweaks). */
  backdropClassName?: string;
  children: ReactNode;
}

/** Tab-focusable descendants, in DOM order, excluding hidden/disabled ones. */
function focusable(root: HTMLElement): HTMLElement[] {
  const sel =
    'a[href],button:not([disabled]),textarea:not([disabled]),input:not([disabled]),select:not([disabled]),[tabindex]:not([tabindex="-1"])';
  return Array.from(root.querySelectorAll<HTMLElement>(sel)).filter(
    (el) => el.offsetParent !== null || el === document.activeElement,
  );
}

export function Dialog({
  open,
  onClose,
  title,
  header,
  hideCloseButton = false,
  placement = "center",
  ariaLabel,
  className,
  backdropClassName,
  children,
}: DialogProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  // Only portal once mounted in a real DOM — the server render path stays inline
  // so SSR (renderToString, no document) emits the body content.
  const mounted = useMounted();

  // Focus the panel on open + restore focus to the opener on close.
  useEffect(() => {
    if (!open || typeof document === "undefined") return;
    const opener = document.activeElement as HTMLElement | null;
    // Focus the first focusable control, else the panel itself.
    const panel = panelRef.current;
    if (panel) {
      const first = focusable(panel)[0];
      (first ?? panel).focus();
    }
    return () => {
      opener?.focus?.();
    };
  }, [open]);

  // Lock body scroll while open (avoids the page scrolling behind the overlay).
  useEffect(() => {
    if (!open || typeof document === "undefined") return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [open]);

  // Escape + focus-trap-lite (keep Tab inside the panel).
  const onKeyDown = useCallback(
    (e: ReactKeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key !== "Tab" || !panelRef.current) return;
      const items = focusable(panelRef.current);
      const first = items[0];
      const last = items[items.length - 1];
      if (!first || !last) {
        e.preventDefault();
        panelRef.current.focus();
        return;
      }
      const active = document.activeElement;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    },
    [onClose],
  );

  if (!open) return null;

  const labelProp =
    typeof title === "string" ? { "aria-label": title } : ariaLabel ? { "aria-label": ariaLabel } : {};

  const panel = (
    <div
      ref={panelRef}
      role="dialog"
      aria-modal="true"
      tabIndex={-1}
      {...labelProp}
      onClick={(e) => e.stopPropagation()}
      onKeyDown={onKeyDown}
      className={cn(
        "outline-none",
        placement === "end"
          ? "flex h-full w-full max-w-xl flex-col border-l border-surface-700 bg-surface-900 shadow-xl animate-slide-in-right"
          : "w-full max-w-md overflow-hidden rounded-lg border border-surface-700 bg-surface-900 shadow-xl animate-slide-up",
        className,
      )}
    >
      {header ?? (
        <div
          className={cn(
            "flex shrink-0 items-center gap-2 border-b border-surface-700/50 px-4",
            placement === "end" ? "h-12" : "h-11",
          )}
        >
          {title ? (
            <span className="truncate text-sm font-semibold text-text-primary">{title}</span>
          ) : null}
          {!hideCloseButton ? (
            <button
              type="button"
              onClick={onClose}
              aria-label="关闭"
              className="ml-auto grid h-7 w-7 place-items-center rounded-md text-text-dim transition-colors hover:bg-surface-800 hover:text-text-primary"
            >
              <X className="h-4 w-4" />
            </button>
          ) : null}
        </div>
      )}
      {children}
    </div>
  );

  const overlay = (
    <div
      className={cn(
        "fixed inset-0 z-50 flex bg-black/50 backdrop-blur-sm animate-fade-in",
        placement === "end" ? "justify-end" : "place-items-center p-4",
        backdropClassName,
      )}
      onClick={onClose}
    >
      {placement === "center" ? panel : null}
      {placement === "end" ? panel : null}
    </div>
  );

  // On the client, portal to <body> so the overlay escapes any clipped/
  // transformed ancestor. On the server (no document), render inline so the
  // panel body lands in the SSR HTML for the smoke tests.
  if (mounted && typeof document !== "undefined") {
    return createPortal(overlay, document.body);
  }
  return overlay;
}
