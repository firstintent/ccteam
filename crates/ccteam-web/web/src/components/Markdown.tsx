// v0.8.19 W1 — renders an assistant message as Markdown (sanitized) inside the
// existing `.cockpit-markdown` prose styles (index.css), and decorates each
// fenced code block with a copy button via a post-render ref effect (the
// button is added to the DOM AFTER the sanitized innerHTML, so it never rides
// through DOMPurify / can't be injected by message content).

import { useEffect, useMemo, useRef } from "react";
import { renderMarkdown } from "../lib/markdown";

export function Markdown({ content, className }: { content: string; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const html = useMemo(() => renderMarkdown(content), [content]);

  useEffect(() => {
    const root = ref.current;
    if (!root) return;
    root.querySelectorAll("pre").forEach((pre) => {
      if (pre.querySelector(".code-copy")) return;
      pre.classList.add("has-copy");
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "code-copy";
      btn.textContent = "复制";
      btn.addEventListener("click", () => {
        const code = pre.querySelector("code")?.textContent ?? pre.textContent ?? "";
        void navigator.clipboard?.writeText(code);
        btn.textContent = "已复制";
        window.setTimeout(() => {
          btn.textContent = "复制";
        }, 1200);
      });
      pre.appendChild(btn);
    });
  }, [html]);

  return (
    <div
      ref={ref}
      className={`cockpit-markdown ${className ?? ""}`}
      // html is renderMarkdown() output = marked + DOMPurify.sanitize.
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
