// V0.3.2 F53 + F59 — base shell version of TopBar. The AoE original wired
// up PaletteTriggerPill + OverflowMenu (both deleted with the rest of the
// AoE cockpit / wizard / settings UX). F59 also dropped the import on
// `lib/types.ts` (AoE-orphan) in favour of a couple of structural typedefs
// kept inline. Future ccteam-specific palette / overflow lives here.

/** Minimal AoE-derived shapes the SPA shell still surfaces in the
 *  header. Ccteam doesn't populate these today (App.tsx passes neither
 *  workspace nor session) but the props stay typed so detail pages or
 *  a future workspace picker can drive them. */
interface WorkspaceShape {
  projectPath?: string | null;
  displayName?: string | null;
}
interface SessionShape {
  id?: string;
}

interface Props {
  activeWorkspace?: WorkspaceShape;
  activeSession?: SessionShape | null;
  onToggleSidebar?: () => void;
  onToggleDiff?: () => void;
  diffCollapsed?: boolean;
  isOffline?: boolean;
  onGoDashboard?: () => void;
}

export function TopBar({
  activeWorkspace,
  activeSession,
  onToggleSidebar,
  onToggleDiff,
  diffCollapsed,
  isOffline,
  onGoDashboard,
}: Props) {
  const repoName =
    activeWorkspace?.projectPath?.split("/").filter(Boolean).pop() ?? null;

  return (
    <header className="h-12 bg-surface-800 border-b border-surface-700/20 flex items-center px-3 shrink-0 gap-2">
      {/* LEFT ZONE */}
      <div className="flex items-center gap-2 min-w-0 shrink-0">
        {onToggleSidebar && (
          <button
            onClick={onToggleSidebar}
            className="w-8 h-8 flex items-center justify-center cursor-pointer rounded-md transition-colors text-text-dim hover:text-text-secondary hover:bg-surface-700/50"
            title="Toggle sidebar"
            aria-label="Toggle sidebar"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <rect x="3" y="3" width="18" height="18" rx="2" />
              <line x1="9" y1="3" x2="9" y2="21" />
            </svg>
          </button>
        )}

        <button
          onClick={onGoDashboard}
          className="flex items-center gap-1.5 text-text-muted hover:text-text-secondary transition-colors cursor-pointer"
          aria-label="Go to dashboard"
        >
          <span className="font-mono text-xs leading-none">ccteam</span>
        </button>

        {(repoName || activeWorkspace) && (
          <div className="hidden sm:flex items-center gap-1.5 min-w-0 text-xs font-mono">
            <span className="text-text-dim">/</span>
            {repoName && (
              <span className="text-text-muted truncate max-w-[140px]">
                {repoName}
              </span>
            )}
            {activeWorkspace && activeWorkspace.displayName !== repoName && (
              <>
                <span className="text-text-dim">/</span>
                <span className="text-accent-600 truncate max-w-[200px]">
                  {activeWorkspace.displayName}
                </span>
              </>
            )}
          </div>
        )}
      </div>

      {/* CENTER — F54 will wire the command palette here */}
      <div className="flex-1" />

      {/* RIGHT ZONE */}
      <div className="flex items-center gap-1.5 shrink-0">
        {isOffline && (
          <span
            className="font-mono text-[11px] px-1.5 py-0.5 rounded-full bg-status-error/10 text-status-error flex items-center gap-1.5"
            title="Disconnected from backend"
          >
            <span className="w-1.5 h-1.5 rounded-full bg-status-error animate-pulse" />
            offline
          </span>
        )}

        {activeWorkspace && activeSession && onToggleDiff && (
          <button
            onClick={onToggleDiff}
            className={`w-8 h-8 flex items-center justify-center cursor-pointer rounded-md transition-colors hover:bg-surface-700/50 ${
              diffCollapsed
                ? "text-text-dim hover:text-text-secondary"
                : "text-text-secondary hover:text-text-primary"
            }`}
            title="Toggle diff panel"
            aria-label="Toggle diff panel"
          >
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <rect x="3" y="3" width="18" height="18" rx="2" />
              <line x1="15" y1="3" x2="15" y2="21" />
            </svg>
          </button>
        )}
      </div>
    </header>
  );
}
