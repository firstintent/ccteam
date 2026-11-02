import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { FileTree, useFileTree } from "@pierre/trees/react";
import { Group, Panel, Separator } from "react-resizable-panels";
import CodeMirror, { type ReactCodeMirrorRef } from "@uiw/react-codemirror";
import { languages } from "@codemirror/language-data";
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import {
  ExternalLink,
  Files,
  FolderTree,
  Maximize2,
  Minimize2,
  Pin,
  RefreshCw,
  X,
} from "lucide-react";
import {
  getProjectFile,
  getProjectFiles,
  mintFilePreview,
  supportsFilePreview,
  openFileTab,
  projectRelativePath,
  type FileTab,
  type FileTarget,
  type ProjectFile,
  type ProjectFiles,
} from "../lib/filesApi";
import { makeT, type Lang } from "../lib/i18n";
const READER_THEME = EditorView.theme({
  "&": { fontSize: "12px" },
  ".cm-scroller": {
    fontFamily: "var(--font-mono)",
    lineHeight: "1.6",
    overflow: "auto",
  },
});

export interface FileWorkspaceProps {
  project: string;
  lang: Lang;
  onClose: () => void;
  target?: FileTarget | null;
  refreshKey?: string | number | null;
  focusOnMount?: boolean;
  visible?: boolean;
}

function readExpanded(project: string): string[] {
  try {
    const stored: unknown = JSON.parse(
      localStorage.getItem(`ccteam.files.expanded.${project}`) ?? "[]",
    );
    return Array.isArray(stored)
      ? stored
          .filter((path): path is string => typeof path === "string")
          .slice(0, 20_000)
      : [];
  } catch {
    return [];
  }
}

function ProjectTree({
  data,
  project,
  active,
  open,
}: {
  data: ProjectFiles;
  project: string;
  active: string | null;
  open: (path: string, pin?: boolean) => void;
}) {
  const openRef = useRef(open);
  useEffect(() => {
    openRef.current = open;
  }, [open]);
  const { model } = useFileTree({
    paths: data.paths,
    icons: "complete",
    search: true,
    density: "compact",
    stickyFolders: true,
    flattenEmptyDirectories: true,
    initialExpansion: "closed",
    initialExpandedPaths: readExpanded(project),
    gitStatus: Object.entries(data.git_status).map(([path, status]) => ({
      path,
      status,
    })),
    onSelectionChange: (selected) => {
      const path = selected.at(-1);
      if (
        path &&
        !path.endsWith("/") &&
        model.getItem(path)?.isDirectory() === false
      )
        openRef.current(path);
    },
  });
  useEffect(() => {
    model.resetPaths(data.paths, {
      initialExpandedPaths: readExpanded(project),
    });
    model.setGitStatus(
      Object.entries(data.git_status).map(([path, status]) => ({
        path,
        status,
      })),
    );
  }, [data, model, project]);
  useEffect(() => {
    const directories = data.paths
      .filter((p) => p.endsWith("/"))
      .map((p) => p.slice(0, -1));
    let timer: ReturnType<typeof setTimeout> | undefined;
    let previous = JSON.stringify(readExpanded(project).sort());
    const persist = () => {
      const expanded = directories
        .filter((path) => {
          const item = model.getItem(path);
          return item && "isExpanded" in item && item.isExpanded();
        })
        .sort();
      const next = JSON.stringify(expanded);
      if (next === previous) return;
      previous = next;
      try {
        localStorage.setItem(`ccteam.files.expanded.${project}`, next);
      } catch {
        /* storage optional */
      }
    };
    const unsubscribe = model.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(persist, 200);
    });
    return () => {
      clearTimeout(timer);
      unsubscribe();
      persist();
    };
  }, [data.paths, model, project]);
  useEffect(() => {
    if (!active) return;
    const parts = active.split("/");
    for (let count = 1; count < parts.length; count++) {
      const parent = model.getItem(parts.slice(0, count).join("/"));
      if (parent && "expand" in parent) parent.expand();
    }
    if (!model.getSelectedPaths().includes(active))
      model.getItem(active)?.select();
    model.scrollToPath(active, { focus: false, offset: "nearest" });
  }, [active, data, model]);
  const style = {
    height: "100%",
    width: "100%",
    "--trees-bg-override": "var(--bg-side)",
    "--trees-fg-override": "var(--text)",
    "--trees-selected-bg-override": "var(--bg-soft)",
    "--trees-border-color-override": "var(--border-soft)",
  } as CSSProperties;
  return (
    <FileTree
      model={model}
      style={style}
      onDoubleClick={() => {
        const path = model.getFocusedPath();
        if (path && model.getItem(path)?.isDirectory() === false)
          openRef.current(path, true);
      }}
    />
  );
}

export default function FileWorkspace({
  project,
  lang,
  onClose,
  target,
  refreshKey,
  focusOnMount,
  visible = true,
}: FileWorkspaceProps) {
  const t = makeT(lang);
  const [tree, setTree] = useState<ProjectFiles | null>(null);
  const [treeError, setTreeError] = useState<string | null>(null);
  const [tabs, setTabs] = useState<FileTab[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [file, setFile] = useState<ProjectFile | null>(null);
  const [fileFailure, setFileFailure] = useState<{
    path: string;
    message: string;
  } | null>(null);
  const [loading, setLoading] = useState(true);
  const [revision, setRevision] = useState(0);
  const [mode, setMode] = useState<"source" | "preview">("source");
  const [preview, setPreview] = useState<{ path: string; url: string } | null>(
    null,
  );
  const [previewFailure, setPreviewFailure] = useState<{
    path: string;
    message: string;
  } | null>(null);
  const [treeVisible, setTreeVisible] = useState(true);
  const [expanded, setExpanded] = useState(false);
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [copied, setCopied] = useState(false);
  const [appliedTarget, setAppliedTarget] = useState<FileTarget | null>(null);
  const panelRef = useRef<HTMLElement>(null);
  const editorRef = useRef<ReactCodeMirrorRef>(null);
  const scrollCleanup = useRef<(() => void) | null>(null);
  const appliedNavigation = useRef<FileTarget | null>(null);
  const scrollPositions = useRef(new Map<string, number>());
  const canPreview = supportsFilePreview(active ?? "");
  const readerExtensions = useMemo(
    () => [...extensions, READER_THEME],
    [extensions],
  );

  const open = useCallback(
    (path: string, pin = false) => {
      setTabs((old) => openFileTab(old, path, pin));
      setActive(path);
      if (active !== path)
        setMode(supportsFilePreview(path) ? "preview" : "source");
    },
    [active],
  );

  useEffect(() => {
    const controller = new AbortController();
    getProjectFiles(project, controller.signal)
      .then((data) => {
        setTree(data);
        setTreeError(null);
        setLoading(false);
      })
      .catch((e: Error) => {
        if (!controller.signal.aborted) {
          setTreeError(e.message);
          setLoading(false);
        }
      });
    return () => controller.abort();
  }, [project, revision, refreshKey]);

  if (target && tree && target !== appliedTarget) {
    const relative = projectRelativePath(tree.root, target.path);
    setAppliedTarget(target);
    if (relative) {
      setTabs((old) => openFileTab(old, relative, true));
      setActive(relative);
      setMode(
        !target.line && supportsFilePreview(relative) ? "preview" : "source",
      );
    }
  }

  useEffect(() => {
    if (focusOnMount && visible) panelRef.current?.focus();
  }, [focusOnMount, visible]);

  useEffect(() => {
    if (!active) return;
    const controller = new AbortController();
    getProjectFile(project, active, controller.signal)
      .then((data) => {
        setFile(data);
        setFileFailure(null);
      })
      .catch((e: Error) => {
        if (!controller.signal.aborted)
          setFileFailure({ path: active, message: e.message });
      });
    const language = languages.find((l) =>
      l.extensions.includes(active.split(".").at(-1) ?? ""),
    );
    void Promise.resolve(language?.load())
      .then((support) => {
        if (!controller.signal.aborted) setExtensions(support ? [support] : []);
      })
      .catch(() => {
        if (!controller.signal.aborted) setExtensions([]);
      });
    return () => controller.abort();
  }, [project, active, revision, refreshKey]);

  useEffect(() => {
    if (
      !visible ||
      !active ||
      !supportsFilePreview(active) ||
      mode !== "preview"
    )
      return;
    const controller = new AbortController();
    mintFilePreview(project, active, controller.signal)
      .then((data) => {
        setPreview({ path: active, url: data.url });
        setPreviewFailure(null);
      })
      .catch((e: Error) => {
        if (!controller.signal.aborted)
          setPreviewFailure({ path: active, message: e.message });
      });
    return () => controller.abort();
  }, [project, active, mode, revision, refreshKey, visible]);

  // Only render responses for the selected path. A previous file's content
  // must never flash while the next request is in flight.
  const current = file?.path === active ? file : null;
  const applyNavigation = useCallback(
    (view: EditorView) => {
      if (
        !appliedTarget?.line ||
        !tree ||
        !active ||
        appliedNavigation.current === appliedTarget ||
        projectRelativePath(tree.root, appliedTarget.path) !== active
      )
        return false;
      const pos = view.state.doc.line(
        Math.min(appliedTarget.line, view.state.doc.lines),
      ).from;
      view.dispatch({
        selection: { anchor: pos },
        effects: EditorView.scrollIntoView(pos, { y: "center" }),
      });
      appliedNavigation.current = appliedTarget;
      return true;
    },
    [active, appliedTarget, tree],
  );
  useEffect(() => {
    const view = editorRef.current?.view;
    if (view && current) applyNavigation(view);
  }, [applyNavigation, current]);
  useEffect(() => () => scrollCleanup.current?.(), []);
  useEffect(() => {
    if (!active || mode !== "source") {
      scrollCleanup.current?.();
      scrollCleanup.current = null;
    }
  }, [active, mode]);
  const refresh = () => {
    setLoading(true);
    setRevision((v) => v + 1);
  };
  const closeTab = (path: string) => {
    const next = tabs.filter((tab) => tab.path !== path);
    setTabs(next);
    if (active === path) {
      const last = next.at(-1);
      setActive(last?.path ?? null);
      setMode(supportsFilePreview(last?.path ?? "") ? "preview" : "source");
    }
  };
  const freshPreview = preview?.path === active ? preview.url : null;
  const previewError =
    previewFailure?.path === active ? previewFailure.message : null;
  const fileError = fileFailure?.path === active ? fileFailure.message : null;

  return (
    <>
      <button
        className="ctx-backdrop files-backdrop"
        aria-label={t("filesClose")}
        onClick={onClose}
      />
      <aside
        className={`files-workspace${expanded ? " files-expanded" : ""}`}
        ref={panelRef}
        tabIndex={-1}
        aria-label={t("filesTitle")}
        data-testid="file-workspace"
        onKeyDown={(e) => {
          if (e.key === "Escape" && !e.defaultPrevented) {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          }
        }}
      >
        <header className="files-head">
          <button
            className={`icon-btn sm${treeVisible ? " on" : ""}`}
            title={t("filesTree")}
            aria-label={t("filesTree")}
            aria-pressed={treeVisible}
            onClick={() => setTreeVisible((v) => !v)}
          >
            <FolderTree />
          </button>
          <strong>{t("filesTitle")}</strong>
          <span className="files-project" title={tree?.root}>
            {project}
          </span>
          <button
            className="icon-btn sm"
            title={t("filesRefresh")}
            aria-label={t("filesRefresh")}
            aria-busy={loading}
            onClick={refresh}
          >
            <RefreshCw className={loading ? "ctx-spin" : ""} />
          </button>
          <button
            className="icon-btn sm"
            title={expanded ? t("filesRestore") : t("filesExpand")}
            aria-label={expanded ? t("filesRestore") : t("filesExpand")}
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded ? <Minimize2 /> : <Maximize2 />}
          </button>
          <button
            className="icon-btn sm"
            aria-label={t("filesClose")}
            onClick={onClose}
          >
            <X />
          </button>
        </header>
        {treeError ? (
          <div className="files-error" role="alert">
            {treeError}
            <button className="btn ghost mini" onClick={refresh}>
              {t("retry")}
            </button>
          </div>
        ) : null}
        {tree?.truncated ? (
          <div className="files-notice">{t("filesTruncated")}</div>
        ) : null}
        <Group orientation="horizontal" className="files-body">
          {treeVisible ? (
            <>
              <Panel id="file-tree" defaultSize="32%" minSize="130px">
                <div className="files-tree" data-testid="project-file-tree">
                  {tree ? (
                    <ProjectTree
                      key={project}
                      data={tree}
                      project={project}
                      active={active}
                      open={open}
                    />
                  ) : (
                    <div className="files-empty">
                      {loading ? t("filesLoading") : t("filesEmpty")}
                    </div>
                  )}
                </div>
              </Panel>
              <Separator className="workspace-separator" />
            </>
          ) : null}
          <Panel id="file-reader" minSize="180px">
            <div className="files-reader">
              <div
                className="files-tabs"
                role="tablist"
                aria-label={t("filesOpenTabs")}
              >
                {tabs.map((tab) => (
                  <div
                    className={`files-tab${tab.path === active ? " active" : ""}${tab.pinned ? " pinned" : ""}`}
                    key={tab.path}
                  >
                    <button
                      role="tab"
                      aria-selected={tab.path === active}
                      title={tab.path}
                      onClick={() => open(tab.path)}
                      onDoubleClick={() => open(tab.path, true)}
                    >
                      {tab.path.split("/").at(-1)}
                    </button>
                    <button
                      className="files-tab-action"
                      aria-label={`${t("filesCloseTab")} ${tab.path}`}
                      onClick={() => closeTab(tab.path)}
                    >
                      <X size={12} />
                    </button>
                  </div>
                ))}
              </div>
              {active ? (
                <>
                  <div className="files-filebar">
                    <span title={active}>{active}</span>
                    <button
                      className="icon-btn sm"
                      title={copied ? t("filesCopied") : t("filesCopyPath")}
                      aria-label={t("filesCopyPath")}
                      onClick={() => {
                        void navigator.clipboard?.writeText(active).then(() => {
                          setCopied(true);
                          setTimeout(() => setCopied(false), 1200);
                        });
                      }}
                    >
                      <Files />
                    </button>
                    <button
                      className="icon-btn sm"
                      title={t("filesPin")}
                      aria-label={t("filesPin")}
                      onClick={() => open(active, true)}
                    >
                      <Pin />
                    </button>
                  </div>
                  {canPreview ? (
                    <div className="files-modes">
                      <button
                        className={`tab${mode === "preview" ? " active" : ""}`}
                        onClick={() => setMode("preview")}
                      >
                        {t("filesPreview")}
                      </button>
                      <button
                        className={`tab${mode === "source" ? " active" : ""}`}
                        onClick={() => setMode("source")}
                      >
                        {t("filesSource")}
                      </button>
                      {freshPreview ? (
                        <a
                          className="files-open-external"
                          href={freshPreview}
                          target="_blank"
                          rel="noopener noreferrer"
                          title={t("filesNewTab")}
                        >
                          <ExternalLink size={14} />
                          {t("filesNewTab")}
                        </a>
                      ) : null}
                    </div>
                  ) : null}
                  {mode === "preview" && canPreview ? (
                    previewError ? (
                      <div role="alert" className="files-error">
                        {previewError}
                      </div>
                    ) : freshPreview && visible ? (
                      <iframe
                        key={`${freshPreview}:${revision}`}
                        className="files-preview"
                        title={`${t("filesPreview")} ${active}`}
                        src={freshPreview}
                        sandbox="allow-scripts"
                        referrerPolicy="no-referrer"
                      />
                    ) : (
                      <div className="files-empty">{t("filesLoading")}</div>
                    )
                  ) : fileError ? (
                    <div role="alert" className="files-error">
                      {fileError}
                    </div>
                  ) : current ? (
                    current.binary ? (
                      <div className="files-empty">{t("filesBinary")}</div>
                    ) : (
                      <CodeMirror
                        key={active}
                        ref={editorRef}
                        value={current.content ?? ""}
                        height="100%"
                        theme={
                          document.documentElement.classList.contains("dark")
                            ? "dark"
                            : "light"
                        }
                        readOnly
                        editable={false}
                        extensions={readerExtensions}
                        basicSetup={{
                          searchKeymap: true,
                          foldGutter: true,
                          highlightActiveLine: false,
                          highlightActiveLineGutter: false,
                        }}
                        onCreateEditor={(view) => {
                          scrollCleanup.current?.();
                          const saveScroll = () => {
                            if (
                              view.scrollDOM.isConnected &&
                              view.scrollDOM.clientHeight > 0
                            )
                              scrollPositions.current.set(
                                active,
                                view.scrollDOM.scrollTop,
                              );
                          };
                          view.scrollDOM.addEventListener(
                            "scroll",
                            saveScroll,
                            { passive: true },
                          );
                          scrollCleanup.current = () =>
                            view.scrollDOM.removeEventListener(
                              "scroll",
                              saveScroll,
                            );
                          if (!applyNavigation(view)) {
                            const savedTop =
                              scrollPositions.current.get(active) ?? 0;
                            // Restore after CodeMirror measures actual line heights;
                            // restoring against its estimates shifts small scrolls.
                            view.requestMeasure({
                              read: () => savedTop,
                              write: (top) => {
                                view.scrollDOM.scrollTop = top;
                              },
                            });
                          }
                        }}
                      />
                    )
                  ) : (
                    <div className="files-empty">{t("filesLoading")}</div>
                  )}
                </>
              ) : (
                <div className="files-empty">
                  <FolderTree size={28} />
                  <span>{t("filesChoose")}</span>
                  <small>{t("filesChooseHint")}</small>
                </div>
              )}
            </div>
          </Panel>
        </Group>
      </aside>
    </>
  );
}
