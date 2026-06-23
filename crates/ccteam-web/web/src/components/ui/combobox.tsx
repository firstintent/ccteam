// v0.8.19 W3a — a searchable, themeable Combobox. Replaces the raw `<select>`s
// in the marketplace filter bar + the new-session modal.
//
// Modeled on LAP's model-select (search input · keyboard nav · click-away ·
// portal-to-body with flip-up when space below is tight) but rebuilt on
// ccteam's theme tokens (surface-*/text-*/brand-*) — no bare colors, lucide
// icons only.
//
// SSR / progressive-enhancement: alongside the visible trigger button this
// renders a VISUALLY-HIDDEN native `<select>` holding the real options. That
// keeps the control working under bare-node `renderToString` (the env our
// vitest suite runs in — no jsdom) and without JS: the server HTML carries
// `<option value="…">label</option>` for every choice (our smoke tests assert
// on those), and the dropdown menu is a client-only enhancement layered on top.

import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown, Search } from "lucide-react";
import { cn } from "../../lib/utils";

export interface ComboboxOption {
  value: string;
  label: string;
  /** A dim secondary line shown under the label (e.g. a model hint). */
  hint?: string;
}

export interface ComboboxProps {
  value: string;
  onChange: (value: string) => void;
  options: ComboboxOption[];
  placeholder?: string;
  /** Search box placeholder (the in-menu input). */
  searchPlaceholder?: string;
  disabled?: boolean;
  className?: string;
  /** Extra classes on the trigger button (sizing/width overrides). */
  buttonClassName?: string;
  ariaLabel?: string;
  /** Hide the search box (a short, fixed option set doesn't need filtering). */
  searchable?: boolean;
  /** `name` for the hidden native select (form semantics, optional). */
  name?: string;
  "data-testid"?: string;
}

interface MenuPos {
  left: number;
  top: number;
  width: number;
  listMaxHeight: number;
}

export function Combobox({
  value,
  onChange,
  options,
  placeholder = "选择…",
  searchPlaceholder = "搜索…",
  disabled = false,
  className,
  buttonClassName,
  ariaLabel,
  searchable = true,
  name,
  "data-testid": testId,
}: ComboboxProps) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [rawActiveIndex, setActiveIndex] = useState(0);
  const [pos, setPos] = useState<MenuPos | null>(null);

  const containerRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listId = useId();

  const selected = useMemo(() => options.find((o) => o.value === value), [options, value]);

  const q = search.trim().toLowerCase();
  const filtered = useMemo(
    () =>
      q
        ? options.filter(
            (o) => o.label.toLowerCase().includes(q) || o.value.toLowerCase().includes(q),
          )
        : options,
    [options, q],
  );

  // A disabled control is never open (derived, so no setState-in-effect needed
  // to force-close it mid-flight).
  const isOpen = open && !disabled;
  // Clamp the active highlight into the (possibly shrunk-by-search) range at
  // use-time rather than syncing it via an effect.
  const activeIndex = Math.min(rawActiveIndex, Math.max(0, filtered.length - 1));

  // Open the menu, resetting the search + highlighting the current value. The
  // reset happens HERE (in the open transition handler) rather than in an
  // effect — that keeps the lint clean (no setState-in-effect) and is the
  // idiomatic place to seed state for a just-opened popup.
  const openMenu = useCallback(() => {
    setSearch("");
    const idx = options.findIndex((o) => o.value === value);
    setActiveIndex(idx >= 0 ? idx : 0);
    setOpen(true);
  }, [options, value]);

  // Focus the search box once the menu is open (focusing a DOM node is an
  // external-system sync, not a React state update).
  useEffect(() => {
    if (!isOpen || !searchable) return;
    const t = setTimeout(() => searchRef.current?.focus(), 0);
    return () => clearTimeout(t);
  }, [isOpen, searchable]);

  // Position the portal menu under (or above, when space is tight) the trigger;
  // re-measure on scroll/resize while open. While CLOSED we intentionally leave
  // any stale `pos` untouched (no setState in the effect body) — the menu render
  // is gated on `isOpen && pos`, so a stale value is inert, and the next open
  // re-measures synchronously below.
  useEffect(() => {
    if (!isOpen || typeof window === "undefined") return;
    const measure = () => {
      const rect = buttonRef.current?.getBoundingClientRect();
      if (!rect) return;
      const gap = 4;
      const pad = 8;
      const chrome = searchable ? 44 : 6; // search box height (or just padding)
      const preferred = 280;
      const below = window.innerHeight - rect.bottom - gap - pad;
      const above = rect.top - gap - pad;
      const flipUp = below < 180 && above > below;
      const avail = flipUp ? above : below;
      const listMaxHeight = Math.max(96, Math.min(preferred, avail - chrome));
      const width = rect.width;
      const left = Math.min(Math.max(pad, rect.left), window.innerWidth - width - pad);
      const top = flipUp ? rect.top - gap - chrome - listMaxHeight : rect.bottom + gap;
      setPos({ left, top, width, listMaxHeight });
    };
    measure();
    window.addEventListener("resize", measure);
    window.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [isOpen, searchable]);

  // Click-away (mousedown outside both the trigger container and the portal).
  useEffect(() => {
    if (!isOpen || typeof document === "undefined") return;
    const handler = (e: MouseEvent) => {
      const t = e.target as Node;
      if (!containerRef.current?.contains(t) && !menuRef.current?.contains(t)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [isOpen]);

  const pick = useCallback(
    (v: string) => {
      onChange(v);
      setOpen(false);
      buttonRef.current?.focus();
    },
    [onChange],
  );

  const onTriggerKeyDown = (e: ReactKeyboardEvent) => {
    if (disabled) return;
    if (!open && (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      openMenu();
    }
  };

  const onMenuKeyDown = (e: ReactKeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      buttonRef.current?.focus();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      // Base navigation on the clamped (displayed) index, not raw state.
      setActiveIndex(Math.min(activeIndex + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex(Math.max(activeIndex - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const opt = filtered[activeIndex];
      if (opt) pick(opt.value);
    }
  };

  const menu =
    isOpen && pos && typeof document !== "undefined" ? (
      <div
        ref={menuRef}
        className="fixed z-[1000] overflow-hidden rounded-md border border-surface-700 bg-surface-850 shadow-xl animate-fade-in"
        style={{ left: pos.left, top: pos.top, width: pos.width }}
        onKeyDown={onMenuKeyDown}
      >
        {searchable ? (
          <div className="flex items-center gap-2 border-b border-surface-700/60 px-2.5 py-2">
            <Search className="h-3.5 w-3.5 shrink-0 text-text-dim" />
            <input
              ref={searchRef}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={searchPlaceholder}
              className="flex-1 bg-transparent text-xs text-text-primary outline-none placeholder:text-text-dim"
            />
          </div>
        ) : null}
        <div
          id={listId}
          role="listbox"
          className="overflow-y-auto py-1"
          style={{ maxHeight: pos.listMaxHeight }}
        >
          {filtered.length === 0 ? (
            <div className="px-3 py-2 text-xs text-text-dim">无匹配项</div>
          ) : (
            filtered.map((opt, i) => {
              const isSel = opt.value === value;
              const isActive = i === activeIndex;
              return (
                <button
                  key={opt.value}
                  type="button"
                  role="option"
                  aria-selected={isSel}
                  onMouseEnter={() => setActiveIndex(i)}
                  onClick={() => pick(opt.value)}
                  className={cn(
                    "flex w-full items-start gap-2 px-2.5 py-1.5 text-left text-xs transition-colors",
                    isActive ? "bg-surface-800 text-text-primary" : "text-text-secondary",
                  )}
                >
                  <Check
                    className={cn(
                      "mt-0.5 h-3 w-3 shrink-0 text-brand-400",
                      isSel ? "opacity-100" : "opacity-0",
                    )}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate">{opt.label}</span>
                    {opt.hint ? (
                      <span className="block truncate text-[10px] text-text-dim">{opt.hint}</span>
                    ) : null}
                  </span>
                </button>
              );
            })
          )}
        </div>
      </div>
    ) : null;

  return (
    <div ref={containerRef} className={cn("relative", className)}>
      {/* Visually-hidden native select: SSR/no-JS fallback + form semantics. It
          carries every option so renderToString emits `value`/label text the
          smoke tests assert on; the styled trigger + menu enhance it on the
          client. `sr-only`-style clipping keeps it out of the visual flow but
          still in the a11y tree for non-pointer users. */}
      <select
        aria-hidden="true"
        tabIndex={-1}
        name={name}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        className="absolute h-px w-px overflow-hidden opacity-0"
        style={{ clip: "rect(0 0 0 0)", clipPath: "inset(50%)" }}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>

      <button
        ref={buttonRef}
        type="button"
        disabled={disabled}
        onClick={() => (open ? setOpen(false) : openMenu())}
        onKeyDown={onTriggerKeyDown}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        aria-controls={isOpen ? listId : undefined}
        aria-label={ariaLabel}
        data-testid={testId}
        className={cn(
          "flex h-9 w-full items-center justify-between gap-1.5 rounded-md border border-surface-700 bg-surface-850 px-3 text-sm text-text-primary outline-none transition-colors hover:border-surface-700 focus:border-brand-500 focus:ring-1 focus:ring-brand-500/40 disabled:cursor-not-allowed disabled:opacity-40",
          buttonClassName,
        )}
      >
        <span className={cn("truncate text-left", !selected && "text-text-dim")}>
          {selected ? selected.label : placeholder}
        </span>
        <ChevronDown className="h-4 w-4 shrink-0 text-text-dim" />
      </button>

      {menu ? createPortal(menu, document.body) : null}
    </div>
  );
}
