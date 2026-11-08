// v0.8.19 W1 — renders an assistant message as Markdown (sanitized) inside the
// existing `.cockpit-markdown` prose styles (index.css), and decorates each
// fenced code block with a copy button via a post-render ref effect (the
// button is added to the DOM AFTER the sanitized innerHTML, so it never rides
// through DOMPurify / can't be injected by message content).

import { useEffect, useMemo, useRef, type MouseEvent } from "react";
import { renderMarkdown } from "../lib/markdown";
import { fileLinkTarget, supportsFilePreview, type FileTarget } from "../lib/filesApi";
import { copyText } from "../lib/clipboard";

// Static SVG only; never interpolate message content into button markup.
const COPY_ICON = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
const CHECK_ICON = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>';
const ERROR_ICON = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="m6 6 12 12M6 18 18 6"/></svg>';

export function Markdown({
  content,
  className,
  onFileOpen,
  resolveFilePreview,
}: {
  content: string;
  className?: string;
  onFileOpen?: (target: FileTarget) => void;
  resolveFilePreview?: (target: FileTarget, renew?: boolean) => Promise<string | null>;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const previewLinks = useRef(new WeakMap<Element, FileTarget>());
  const ordinaryLinks = useRef(new WeakSet<Element>());
  const html = useMemo(() => renderMarkdown(content), [content]);
  // React otherwise reapplies a new innerHTML object on parent updates,
  // removing the post-render controls without rerunning the html effect.
  const markup = useMemo(() => ({ __html: html }), [html]);

  useEffect(() => {
    const root = ref.current;
    if (!root) return;
    const timers = new Set<number>();
    const buttons: HTMLButtonElement[] = [];
    root.querySelectorAll("pre").forEach((pre) => {
      if (pre.querySelector(".code-copy")) return;
      pre.classList.add("has-copy");
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "code-copy";
      const label = (text: string, icon: string) => {
        btn.setAttribute("aria-label", text);
        btn.title = text;
        btn.innerHTML = icon;
      };
      label("复制代码", COPY_ICON);
      btn.addEventListener("click", async () => {
        const code = pre.querySelector("code")?.textContent ?? "";
        const copied = await copyText(code);
        if (!root.contains(btn)) return;
        label(copied ? "已复制" : "复制失败，点击重试", copied ? CHECK_ICON : ERROR_ICON);
        const timer = window.setTimeout(() => {
          label("复制代码", COPY_ICON);
          timers.delete(timer);
        }, 1600);
        timers.add(timer);
      });
      pre.appendChild(btn);
      buttons.push(btn);
    });
    return () => {
      timers.forEach(window.clearTimeout);
      buttons.forEach((button) => button.remove());
    };
  }, [html]);

  useEffect(() => {
    const root = ref.current;
    if (!root || !resolveFilePreview) return;
    let active = true;
    root.querySelectorAll<HTMLAnchorElement>("a[href]").forEach((link) => {
      const target = previewLinks.current.get(link) ?? fileLinkTarget(link.getAttribute("href") ?? "", window.location.origin);
      if (!target || target.line || !supportsFilePreview(target.path)) return;
      void resolveFilePreview(target).then((url) => {
        if (!active) return;
        if (!url) { ordinaryLinks.current.add(link); return; }
        previewLinks.current.set(link, target);
        link.setAttribute("href", url);
        link.target = "_blank";
        link.rel = "noopener noreferrer";
      }).catch(() => {
        // A failed preview keeps the ordinary file-workspace click path.
      });
    });
    return () => { active = false; };
  }, [html, resolveFilePreview]);

  const openLink = (event: MouseEvent<HTMLDivElement>) => {
    const link = (event.target as Element).closest("a[href]");
    if (!link || ordinaryLinks.current.has(link)) return;
    const preview = previewLinks.current.get(link);
    if (preview && resolveFilePreview) {
      event.preventDefault();
      // Each activation mints a fresh capability: TTL and daemon restarts
      // invalidate old tickets, including on modified and middle clicks.
      const page = window.open("about:blank", "_blank");
      if (page) page.opener = null;
      void resolveFilePreview(preview, true).then((url) => {
        if (page && url) {
          link.setAttribute("href", url);
          page.location.replace(url);
        } else { page?.close(); onFileOpen?.(preview); }
      }).catch(() => { page?.close(); onFileOpen?.(preview); });
      return;
    }
    if (!onFileOpen || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    const target = fileLinkTarget(link.getAttribute("href") ?? "", window.location.origin);
    if (target) {
      event.preventDefault();
      onFileOpen(target);
    }
  };

  return (
    <div
      ref={ref}
      className={`cockpit-markdown ${className ?? ""}`}
      onClick={openLink}
      onAuxClick={(event) => { if (event.button === 1) openLink(event); }}
      // html is renderMarkdown() output = marked + DOMPurify.sanitize.
      dangerouslySetInnerHTML={markup}
    />
  );
}
