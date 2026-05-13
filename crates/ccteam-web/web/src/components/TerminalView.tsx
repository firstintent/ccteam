// V0.3.2 F57 — xterm mount + WS PTY wiring.
//
// AoE's TerminalView wrapped the wterm with an `ensureSession`
// round-trip, "primary" overlays for multi-client coordination, and
// a paired-terminal focus broadcast. None of that survives for
// ccteam: F58 (write actions) owns `lib/api.ts`, F56 has no primary
// concept, and pair-of-terminals is deferred past V0.3.2 (PRD §5).
//
// This component is now a thin shell: it mounts the wterm, runs the
// trimmed `useTerminal` hook against the F56 relay, paints the
// reconnect banner inline, renders the Back-to-live FAB while the
// user is reading scrollback on mobile, and slots in the
// MobileTerminalToolbar on coarse pointers.
//
// Props: `{ slug, sid?, className? }`. The optional `sid` selects a
// flex per-session route (`/ws/<slug>/<sid>/pty`); without it the
// workflow / default project route is used (`/ws/<slug>/pty`).
//
// Embedding: F54 (project detail) and F55 (session detail) own the
// outer pages and decide the slug/sid pairing.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useTerminal } from "../hooks/useTerminal";
import { useMobileKeyboard } from "../hooks/useMobileKeyboard";
import { MobileTerminalToolbar } from "./MobileTerminalToolbar";
import { BackToLiveButton } from "./BackToLiveButton";
import { KeyboardFab } from "./KeyboardFab";
import { ViewportFullscreenFab } from "./ViewportFullscreenFab";
import "@wterm/dom/css";

const SCROLL_HINT_SEEN_KEY = "ccteam-mobile-scroll-hint-seen";
const SCROLL_HINT_TIMEOUT_MS = 8000;

interface Props {
  /** Project slug. Workflow projects: required and sufficient. */
  slug: string;
  /**
   * Flex per-session id. Omit for workflow projects (single shared
   * tmux pane). Required for flex projects (each session has its own
   * tmux pane).
   */
  sid?: string;
  className?: string;
}

export function TerminalView({ slug, sid, className }: Props) {
  const {
    containerRef,
    termRef,
    state,
    manualReconnect,
    sendData,
    exitScrollback,
    ctrlActiveRef,
    clearCtrlRef,
  } = useTerminal(slug, sid, true);
  const { isMobile, keyboardOpen, keyboardHeight, reservedKeyboardHeight } =
    useMobileKeyboard();
  const [ctrlActive, setCtrlActive] = useState(false);
  const [termFocused, setTermFocused] = useState(false);
  // Default behavior on mobile: pad the viewport by reservedKeyboardHeight
  // so the wterm container stays the same size whether the soft keyboard
  // is up or not. Toggle this on (via the FAB) to release the reservation
  // and use the full viewport. Each toggle is one explicit PTY resize.
  const [viewportFullscreen, setViewportFullscreen] = useState(false);
  const toggleViewportFullscreen = useCallback(() => {
    setViewportFullscreen((v) => !v);
  }, []);
  // The actual padding applied. On desktop reservedKeyboardHeight stays 0
  // and this is a no-op. On mobile in fullscreen mode it's also 0.
  // Otherwise we apply the latched reservation.
  const appliedKeyboardPadding = viewportFullscreen
    ? 0
    : reservedKeyboardHeight;

  // Sync React state → hook ref in an effect. The mobile toolbar toggles
  // `ctrlActive` but the wterm native onData callback reads the ref to
  // decide whether to transform the next keystroke. Writing refs during
  // render trips react-hooks/refs; a commit-phase effect does the same
  // work without tripping the lint.
  useEffect(() => {
    ctrlActiveRef.current = ctrlActive;
  });
  useEffect(() => {
    clearCtrlRef.current = () => setCtrlActive(false);
  }, [clearCtrlRef]);

  const [hintDismissed, setHintDismissed] = useState(() => {
    try {
      return localStorage.getItem(SCROLL_HINT_SEEN_KEY) === "1";
    } catch {
      return true;
    }
  });
  const showScrollHint = isMobile && state.connected && !hintDismissed;

  // The terminal container shrinks when appliedKeyboardPadding changes
  // (first keyboard open of the session, orientation flip, or fullscreen
  // toggle). wterm's ResizeObserver fires and checks _isScrolledToBottom()
  // BEFORE the DOM has reflowed, sees the reduced clientHeight while
  // scrollTop/scrollHeight are stale, and concludes "not at bottom." This
  // makes it skip _scrollToBottom() after the resize, leaving the cursor
  // off-screen.
  //
  // Fix: force a scroll-to-bottom via double-rAF (fires after wterm's own
  // rAF render) on every padding change, plus a debounced final scroll
  // after the animation settles.
  const resizeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const scrollRafRef = useRef(0);
  useLayoutEffect(() => {
    if (resizeTimerRef.current) clearTimeout(resizeTimerRef.current);

    cancelAnimationFrame(scrollRafRef.current);
    scrollRafRef.current = requestAnimationFrame(() => {
      scrollRafRef.current = requestAnimationFrame(() => {
        const el = termRef.current?.element;
        if (el) el.scrollTop = el.scrollHeight;
      });
    });

    resizeTimerRef.current = setTimeout(() => {
      resizeTimerRef.current = null;
      window.dispatchEvent(new Event("resize"));
      const el = termRef.current?.element;
      if (el) el.scrollTop = el.scrollHeight;
    }, 150);
    return () => {
      if (resizeTimerRef.current) clearTimeout(resizeTimerRef.current);
      cancelAnimationFrame(scrollRafRef.current);
    };
  }, [appliedKeyboardPadding, termRef]);

  // wterm sometimes resets scrollTop=0 mid-session when its renderer
  // redraws (observed on backspace), and its post-render scroll-to-
  // bottom skips because _isScrolledToBottom() reads stale dimensions
  // on the same task. Result: scrollHeight grows past clientHeight
  // while scrollTop stays at 0, so the cursor falls below the visible
  // region until the next keyboard open/close kicks the fix above.
  const isInScrollbackRef = useRef(state.isInScrollback);
  useEffect(() => {
    isInScrollbackRef.current = state.isInScrollback;
  }, [state.isInScrollback]);
  useEffect(() => {
    let raf = 0;
    const onScroll = (e: Event) => {
      const el = termRef.current?.element;
      if (!el) return;
      const target = e.target as Node | null;
      if (target !== el && !(target && el.contains(target))) return;
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        if (isInScrollbackRef.current) return;
        const elNow = termRef.current?.element;
        if (!elNow) return;
        const max = Math.max(0, elNow.scrollHeight - elNow.clientHeight);
        if (elNow.scrollTop < max - 1) {
          elNow.scrollTop = elNow.scrollHeight;
        }
      });
    };
    document.addEventListener("scroll", onScroll, {
      passive: true,
      capture: true,
    });
    return () => {
      document.removeEventListener("scroll", onScroll, true);
      cancelAnimationFrame(raf);
    };
  }, [termRef]);

  // On initial connect, auto-open the soft keyboard on mobile.
  useEffect(() => {
    if (!isMobile || !state.connected) return;
    const term = termRef.current;
    if (!term) return;
    // Retry a few times: wterm's textarea may not exist immediately.
    const delays = [50, 200, 500];
    const timers = delays.map((ms) =>
      setTimeout(() => {
        const ta = term.element.querySelector("textarea");
        if (ta instanceof HTMLElement) ta.focus();
      }, ms),
    );
    return () => timers.forEach(clearTimeout);
  }, [isMobile, state.connected, termRef]);

  // Toggle keyboard: focus/blur MUST be the first thing in this handler
  // so iOS considers it part of the user-gesture chain. Anything before
  // focus() (even a synchronous ws.send) can break iOS keyboard display.
  const toggleKeyboard = useCallback(() => {
    const term = termRef.current;
    if (!term) return;
    const ta = term.element.querySelector("textarea");
    if (keyboardOpen) {
      ta?.blur();
    } else if (ta instanceof HTMLElement) {
      ta.focus();
    }
  }, [termRef, keyboardOpen]);

  // Dismiss scroll hint on first touch or timeout.
  useEffect(() => {
    if (!showScrollHint) return;
    const markSeen = () => {
      setHintDismissed(true);
      try {
        localStorage.setItem(SCROLL_HINT_SEEN_KEY, "1");
      } catch {
        // ignore
      }
    };
    const t = setTimeout(markSeen, SCROLL_HINT_TIMEOUT_MS);
    const c = containerRef.current;
    c?.addEventListener("touchmove", markSeen, { once: true });
    return () => {
      clearTimeout(t);
      c?.removeEventListener("touchmove", markSeen);
    };
  }, [showScrollHint, containerRef]);

  // Pad the viewport by the latched reservation, not the live keyboard
  // height. The pane stays the "keyboard is here" size whether the
  // keyboard is currently up or not, so showing/hiding it stops sending
  // SIGWINCH and stops claude from re-rendering into the scrollback.
  // The fullscreen FAB releases the reservation when the user wants the
  // full viewport (one explicit resize per toggle).
  const rootStyle = {
    paddingBottom:
      appliedKeyboardPadding > 0 ? appliedKeyboardPadding : undefined,
  } as const;
  const rootClass = [
    "flex-1 flex flex-col overflow-hidden relative md:bg-surface-800 md:pb-1.5",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div className={rootClass} style={rootStyle}>
      {!state.connected && state.reconnecting && (
        <div className="bg-status-waiting/15 border-b border-status-waiting/30 px-4 py-1.5 flex items-center gap-2 shrink-0">
          <span className="text-xs text-status-waiting">
            Reconnecting in {state.retryCountdown}s... ({state.retryCount}/7)
          </span>
        </div>
      )}
      {!state.connected && !state.reconnecting && state.retryCount >= 7 && (
        <div className="bg-status-error/10 border-b border-status-error/30 px-4 py-1.5 flex items-center gap-2 shrink-0">
          <span className="text-xs text-status-error">Connection lost</span>
          <button
            onClick={manualReconnect}
            className="text-xs text-brand-500 hover:text-brand-400 cursor-pointer underline"
          >
            Retry
          </button>
        </div>
      )}

      <div
        data-term="agent"
        className={`flex-1 overflow-hidden bg-surface-950 relative md:rounded-lg term-panel${termFocused ? " term-focused" : ""}`}
        onFocus={() => setTermFocused(true)}
        onBlur={() => setTermFocused(false)}
      >
        <div ref={containerRef} className="absolute inset-0" />

        {showScrollHint && (
          <div
            aria-hidden="true"
            className="absolute left-0 right-0 top-3 flex justify-center pointer-events-none motion-safe:animate-[fadeIn_300ms_ease-out]"
          >
            <span className="flex items-center gap-2 font-mono text-[13px] text-text-primary bg-surface-800/95 border border-surface-700 rounded-md px-3 py-2 shadow-lg backdrop-blur-sm">
              <span aria-hidden="true" className="text-base leading-none">
                {"⇅"}
              </span>
              Swipe to scroll
            </span>
          </div>
        )}

        {isMobile && state.isInScrollback && (
          <BackToLiveButton onClick={exitScrollback} topOffset="top-3" />
        )}

        {isMobile && state.connected && (
          <KeyboardFab keyboardOpen={keyboardOpen} onToggle={toggleKeyboard} />
        )}

        {isMobile && state.connected && reservedKeyboardHeight > 0 && (
          <ViewportFullscreenFab
            fullscreen={viewportFullscreen}
            onToggle={toggleViewportFullscreen}
          />
        )}
      </div>

      {isMobile && state.connected && (
        <MobileTerminalToolbar
          sendData={sendData}
          termRef={termRef}
          keyboardHeight={keyboardHeight}
          reservedKeyboardHeight={reservedKeyboardHeight}
          ctrlActive={ctrlActive}
          onCtrlToggle={() => setCtrlActive((v) => !v)}
        />
      )}
    </div>
  );
}
