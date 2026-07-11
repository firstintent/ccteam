// v0.8.24 Track A — the prototype's logo mark (双 C 组队 + 在线点), inlined
// verbatim from ui-prototype.html's <symbol id="cclogo">. Each instance keeps
// its own gradient id so multiple mounts (expanded + mini rail) never clash.

import { useId } from "react";

export function CcLogo({ className, onClick, title }: { className?: string; onClick?: () => void; title?: string }) {
  const gid = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  return (
    <svg
      className={className ?? "logo-mark"}
      viewBox="0 0 48 48"
      onClick={onClick}
      role={onClick ? "button" : undefined}
      aria-label={title ?? "ccteam"}
      style={onClick ? { cursor: "pointer" } : undefined}
    >
      <defs>
        <linearGradient id={`ccg-${gid}`} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" stopColor="#6D6AF6" />
          <stop offset="100%" stopColor="#22D3EE" />
        </linearGradient>
      </defs>
      <rect x="2" y="2" width="44" height="44" rx="13" fill={`url(#ccg-${gid})`} />
      <path
        d="M27.5 16.6a10 10 0 1 0 0 14.8"
        fill="none"
        stroke="#fff"
        strokeWidth="4.6"
        strokeLinecap="round"
      />
      <path
        d="M38.6 20.9a7 7 0 1 0 0 6.2"
        fill="none"
        stroke="#fff"
        strokeWidth="3.4"
        strokeLinecap="round"
        opacity=".82"
      />
      <circle cx="37" cy="34.5" r="4.2" fill="#22C55E" stroke="#fff" strokeWidth="2.2" />
    </svg>
  );
}
