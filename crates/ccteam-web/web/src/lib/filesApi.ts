import { httpError } from "./httpError";

export interface ProjectFiles {
  root: string;
  paths: string[];
  truncated: boolean;
  git_status: Record<
    string,
    "added" | "deleted" | "ignored" | "modified" | "renamed" | "untracked"
  >;
}
export interface ProjectFile {
  path: string;
  size: number;
  content: string | null;
  binary: boolean;
}
export interface FileTarget {
  path: string;
  line?: number;
}

export function supportsFilePreview(path: string): boolean {
  return /\.(?:html?|md|markdown)$/i.test(path);
}
const projectUrl = (slug: string) =>
  `/api/v1/projects/${encodeURIComponent(slug)}`;

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, init);
  if (!response.ok) throw await httpError(response);
  return response.json() as Promise<T>;
}
export const getProjectFiles = (slug: string, signal?: AbortSignal) =>
  json<ProjectFiles>(`${projectUrl(slug)}/files`, { signal });
export const getProjectFile = (
  slug: string,
  path: string,
  signal?: AbortSignal,
) =>
  json<ProjectFile>(
    `${projectUrl(slug)}/file?${new URLSearchParams({ path })}`,
    { signal },
  );
export const mintFilePreview = (
  slug: string,
  path: string,
  signal?: AbortSignal,
) =>
  json<{ url: string; expires_in: number }>(
    `${projectUrl(slug)}/files/preview`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
      signal,
    },
  );

/** Local Markdown references only; ordinary web/navigation links retain their
 * browser behavior. Project containment is checked again by the server. */
export function fileLinkTarget(href: string): FileTarget | null {
  if (
    /^(?:https?|ftp|javascript|data|blob|mailto|tel|file):/i.test(href) ||
    /^[a-z][a-z\d+.-]*:\/\//i.test(href) ||
    href.startsWith("//") ||
    href.startsWith("#") ||
    /^\/(api|app|ws)\//.test(href)
  )
    return null;
  let path: string;
  try {
    path = decodeURIComponent(href);
  } catch {
    return null;
  }
  const match = /(?::(\d+)(?::\d+)?|#L(\d+)(?:-L?\d+)?)$/.exec(path);
  const line = match ? Number(match[1] ?? match[2]) : undefined;
  if (match) path = path.slice(0, match.index);
  if (/^[a-z][a-z\d+.-]*:/i.test(path)) return null;
  if (
    (!match &&
      !/\.[\w-]+$/.test(path) &&
      !/(?:^|\/)(?:Makefile|Dockerfile|LICENSE|README|Justfile|Gemfile|Cargo\.lock)$/.test(
        path,
      )) ||
    path.includes("?") ||
    path.includes("#")
  )
    return null;
  return { path, ...(line && line > 0 ? { line } : {}) };
}

export interface FileTab {
  path: string;
  pinned: boolean;
}
export function openFileTab(
  tabs: FileTab[],
  path: string,
  pin = false,
): FileTab[] {
  if (tabs.some((t) => t.path === path))
    return tabs.map((t) =>
      t.path === path ? { ...t, pinned: t.pinned || pin } : t,
    );
  return [...tabs.filter((t) => t.pinned), { path, pinned: pin }].slice(-20);
}

export function projectRelativePath(root: string, path: string): string | null {
  const relative = path.startsWith(`${root}/`)
    ? path.slice(root.length + 1)
    : path;
  if (
    relative.startsWith("/") ||
    relative.includes("\\") ||
    relative.split("/").some((s) => s === ".." || !s)
  )
    return null;
  return relative.replace(/^\.\//, "");
}
