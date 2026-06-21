import { useCallback, useSyncExternalStore } from "react";

const STORAGE_KEY = "aoe-web-settings";

export interface WebSettings {
  mobileFontSize: number;
  desktopFontSize: number;
  autoOpenKeyboard: boolean;
  diffViewMode: "flat" | "tree";
  collapsedDiffDirs: string[];
  // v0.8.18 柱2/UI — personal settings (the avatar popover). Per-browser in
  // 档0 (no per-user web identity yet); 档1 ties them to the server identity.
  /** Interface language. `zh` (中文, default) | `en` (English). Full UI i18n
   *  is staged — this drives the nav + key labels now. */
  language: "zh" | "en";
  /** Display name shown on the avatar (empty ⇒ a generic glyph). */
  displayName: string;
  /** Avatar swatch (an emoji from a small fixed palette). */
  avatar: string;
}

function getDefaults(): WebSettings {
  return {
    mobileFontSize: 8,
    desktopFontSize: 14,
    autoOpenKeyboard: true,
    diffViewMode: window.innerWidth < 768 ? "flat" : "tree",
    collapsedDiffDirs: [],
    language: "zh",
    displayName: "",
    avatar: "🟧",
  };
}

function getSnapshot(): WebSettings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return { ...getDefaults(), ...JSON.parse(raw) };
  } catch {
    // ignore
  }
  return getDefaults();
}

// Subscribers for useSyncExternalStore
let listeners: Array<() => void> = [];

function subscribe(listener: () => void) {
  listeners = [...listeners, listener];
  return () => {
    listeners = listeners.filter((l) => l !== listener);
  };
}

function emitChange() {
  for (const l of listeners) l();
}

// Cache snapshot to return stable reference when nothing changed
let cachedRaw: string | null = null;
let cachedSettings: WebSettings = getDefaults();

function getStableSnapshot(): WebSettings {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw !== cachedRaw) {
    cachedRaw = raw;
    cachedSettings = getSnapshot();
  }
  return cachedSettings;
}

export function useWebSettings() {
  // `getDefaults` as the server snapshot keeps `useSyncExternalStore` SSR-safe:
  // the node-env vitest suite renders shells via `renderToString`, where there
  // is no localStorage — so the server falls back to defaults (language 中文).
  const settings = useSyncExternalStore(subscribe, getStableSnapshot, getDefaults);

  const update = useCallback((patch: Partial<WebSettings>) => {
    const current = getSnapshot();
    const next = { ...current, ...patch };
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    } catch (err) {
      console.warn("aoe-web-settings: failed to persist", err);
    }
    cachedRaw = null;
    emitChange();
  }, []);

  return { settings, update };
}
