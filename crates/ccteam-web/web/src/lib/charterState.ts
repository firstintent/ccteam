// v0.9.11 TEAM-2 — pure state machine for the charter editor
// (`pages/CharterPanel.tsx`). Kept in lib/ (like `agentsReducer.ts`) so the
// node-env suite can drive every transition without a DOM:
//
//   loaded(project doc)  → editable draft seeded from the file (clean)
//   loaded(global doc)   → read-only fallback view; 拷入起稿 / 空白起稿 CTAs
//   loaded(none)         → 空白起稿 only
//   start-draft          → dirty draft (copy = fallback content, blank = "")
//   saved                → the doc BECOMES the project file (source flips)

import type { RoutingDoc, RoutingSaveResult } from "./routingApi";

export interface CharterState {
  /** Last GET result (null until the first load resolves). */
  doc: RoutingDoc | null;
  /** Editor buffer; null = not editing (viewing the fallback / empty state). */
  draft: string | null;
  /** Last-saved content the dirty flag compares against; null = the project
   *  file does not exist yet, so ANY draft is unsaved. */
  baseline: string | null;
  dirty: boolean;
  previewing: boolean;
  saving: boolean;
  /** Receipt of the latest PUT (sha256 + mtime), shown next to 保存. */
  saved: RoutingSaveResult | null;
  error: string | null;
  loading: boolean;
}

export type CharterAction =
  | { kind: "reset" } // project switch → back to loading
  | { kind: "loaded"; doc: RoutingDoc }
  | { kind: "load-failed"; error: string }
  | { kind: "start-draft"; from: "copy" | "blank" }
  | { kind: "edit"; content: string }
  | { kind: "toggle-preview" }
  | { kind: "save-begin" }
  | { kind: "saved"; result: RoutingSaveResult }
  | { kind: "save-failed"; error: string };

export const initialCharter: CharterState = {
  doc: null,
  draft: null,
  baseline: null,
  dirty: false,
  previewing: false,
  saving: false,
  saved: null,
  error: null,
  loading: true,
};

export function charterReducer(state: CharterState, action: CharterAction): CharterState {
  switch (action.kind) {
    case "reset":
      return initialCharter;
    case "loaded": {
      const { doc } = action;
      // A project-owned charter opens straight into the editor (clean);
      // global/none stay read-only until a CTA starts a draft.
      const editable = doc.source === "project";
      return {
        ...initialCharter,
        loading: false,
        doc,
        draft: editable ? doc.content : null,
        baseline: editable ? doc.content : null,
      };
    }
    case "load-failed":
      return { ...initialCharter, loading: false, error: action.error };
    case "start-draft": {
      if (!state.doc) return state;
      const draft = action.from === "copy" ? state.doc.content : "";
      return { ...state, draft, dirty: true, previewing: false, saved: null };
    }
    case "edit": {
      const dirty = state.baseline === null || action.content !== state.baseline;
      return { ...state, draft: action.content, dirty };
    }
    case "toggle-preview":
      return { ...state, previewing: !state.previewing };
    case "save-begin":
      return { ...state, saving: true, error: null };
    case "saved": {
      const draft = state.draft ?? "";
      // The project file now exists with the draft's content — flip the doc
      // so provenance is honest without a refetch.
      const doc: RoutingDoc | null = state.doc
        ? {
            ...state.doc,
            exists: true,
            source: "project",
            fallback_path: null,
            content: draft,
            sha256: action.result.sha256,
            updated_at: action.result.updated_at,
          }
        : state.doc;
      return {
        ...state,
        doc,
        baseline: draft,
        dirty: false,
        saving: false,
        saved: action.result,
      };
    }
    case "save-failed":
      return { ...state, saving: false, error: action.error };
  }
}
