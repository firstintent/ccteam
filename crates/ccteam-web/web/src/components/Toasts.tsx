// V0.3.2 F59 — trimmed `Toasts` to the ccteam-relevant subset.
//
// The AoE original wired a service-worker `message` listener so push
// notifications could surface as in-app toasts (with a sessionId →
// `requestOpenSession` jump on tap). ccteam ships no service worker
// and no web-push surface (V0.4 deferred per V0.3.2 PRD §5), so the
// SW handler and the sessionId tap path were stripped along with the
// `lib/sessionRoute.ts` orphan.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { toastBus, type ToastApi, type ToastKind } from "../lib/toastBus";

interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
}

const ToastContext = createContext<ToastApi | null>(null);

const TOAST_LIFETIME_MS = 6000;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(1);

  const dismiss = useCallback((id: number) => {
    setToasts((t) => t.filter((toast) => toast.id !== id));
  }, []);

  const push = useCallback(
    (message: string, kind: ToastKind = "info") => {
      const id = nextId.current++;
      setToasts((t) => [...t, { id, kind, message }]);
      setTimeout(() => dismiss(id), TOAST_LIFETIME_MS);
    },
    [dismiss],
  );

  const api = useMemo<ToastApi>(
    () => ({
      push,
      error: (m: string) => push(m, "error"),
      info: (m: string) => push(m, "info"),
    }),
    [push],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      {/* v0.8.24 Track A — prototype `#toast`: bottom-center ink pill.
          Errors keep a red wash so failures stay distinguishable. */}
      <div
        className="fixed bottom-[26px] left-1/2 -translate-x-1/2 z-[200] flex flex-col items-center gap-2 max-w-[90vw]"
        style={{ pointerEvents: "none" }}
      >
        {toasts.map((t) => (
          <div
            key={t.id}
            role={t.kind === "error" ? "alert" : "status"}
            className="flex items-start gap-2 animate-slide-up"
            style={{
              pointerEvents: "auto",
              background: t.kind === "error" ? "var(--red)" : "var(--ink)",
              color: t.kind === "error" ? "#fff" : "var(--on-ink)",
              padding: "10px 18px",
              borderRadius: 10,
              fontSize: 13.5,
              boxShadow: "var(--shadow-menu)",
              maxWidth: "90vw",
            }}
          >
            <span className="flex-1 break-words">{t.message}</span>
            <button
              onClick={() => dismiss(t.id)}
              className="cursor-pointer opacity-70 hover:opacity-100"
              aria-label="Dismiss"
            >
              &times;
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}

/**
 * Hook that wires the React ToastProvider into the module-level toastBus so
 * non-React callers (like the fetch interceptor) can surface errors as toasts.
 * Keep this component-local: it is only safe to call inside ToastProvider.
 */
export function ToastBusBridge() {
  const ctx = useContext(ToastContext);
  useEffect(() => {
    if (!ctx) return;
    toastBus.handler = ctx;
    return () => {
      if (toastBus.handler === ctx) toastBus.handler = null;
    };
  }, [ctx]);
  return null;
}
