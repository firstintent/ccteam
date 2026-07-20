// The ccteam brand mark — the README mascot (assets/logo.svg): a juggler bot,
// the claude-colored brain keeping the codex / grok / kimi balls in the air.
// Inlined verbatim so the mark needs no fetch and stays crisp at any size;
// geometry must stay in sync with assets/logo.svg (the single brand source).

export function CcLogo({ className, onClick, title }: { className?: string; onClick?: () => void; title?: string }) {
  return (
    <svg
      className={`${className ?? "logo-mark"}${onClick ? " clickable" : ""}`}
      viewBox="0 0 120 120"
      fill="none"
      onClick={onClick}
      role={onClick ? "button" : "img"}
      aria-label={title ?? "ccteam"}
      style={onClick ? { cursor: "pointer" } : undefined}
    >
      {/* juggling trajectory */}
      <path
        d="M20 44 Q60 -4 100 44"
        stroke="#B9BDC7"
        strokeWidth="2"
        strokeDasharray="1 6"
        strokeLinecap="round"
        opacity=".9"
      />

      {/* the three specialists in the air: codex / grok / kimi */}
      <circle cx="29" cy="35" r="11.5" fill="#10A37F" />
      <g transform="translate(21.8,27.8) scale(0.9)">
        <path fill="#fff" d="M8 1.5 13 4.4v7.2L8 14.5 3 11.6V4.4L8 1.5zm0 2.2L5 5.4v5.2l3 1.7 3-1.7V5.4L8 3.7z" />
      </g>
      <circle cx="60" cy="19" r="11.5" fill="#8B5CF6" />
      <g transform="translate(52.8,11.8) scale(0.9)">
        <path fill="#fff" d="M3.2 2.5h2.4L8 6.2l2.4-3.7h2.4L9.4 8l3.6 5.5h-2.4L8 9.8l-2.6 3.7H3.2L6.6 8 3.2 2.5z" />
      </g>
      <circle cx="91" cy="35" r="11.5" fill="#DB2777" />
      <g transform="translate(83.8,27.8) scale(0.9)">
        <path fill="#fff" d="M10.2 2.2a6.4 6.4 0 1 0 3.4 11.4 5.2 5.2 0 1 1-3.4-11.4z" />
      </g>

      {/* antenna, beaming */}
      <path d="M60 57 V42" stroke="#D97757" strokeWidth="4.5" strokeLinecap="round" />
      <circle cx="60" cy="38.5" r="5" fill="#D97757" />
      <circle cx="60" cy="38.5" r="1.8" fill="#F8D8C8" />
      <path d="M67.5 33 A10.5 10.5 0 0 1 70.5 41" stroke="#B9BDC7" strokeWidth="2.2" strokeLinecap="round" />
      <path d="M72.5 28.5 A16.5 16.5 0 0 1 77 41" stroke="#B9BDC7" strokeWidth="2.2" strokeLinecap="round" opacity=".65" />

      {/* feet */}
      <ellipse cx="49" cy="109" rx="8" ry="4.5" fill="#C46041" />
      <ellipse cx="71" cy="109" rx="8" ry="4.5" fill="#C46041" />

      {/* arms reaching for the balls */}
      <path d="M37 72 Q22 62 26 49" stroke="#D97757" strokeWidth="7.5" strokeLinecap="round" />
      <path d="M83 72 Q98 62 94 49" stroke="#D97757" strokeWidth="7.5" strokeLinecap="round" />

      {/* body (claude, the brain) */}
      <circle cx="60" cy="82" r="27" fill="#D97757" />

      {/* face */}
      <circle cx="51" cy="76" r="3.1" fill="#3B2519" />
      <circle cx="69" cy="76" r="3.1" fill="#3B2519" />
      <circle cx="52.1" cy="74.9" r="1" fill="#fff" />
      <circle cx="70.1" cy="74.9" r="1" fill="#fff" />
      <path d="M52 84 Q60 90.5 68 84" stroke="#3B2519" strokeWidth="2.6" strokeLinecap="round" fill="none" />
      <circle cx="43.5" cy="83.5" r="3.4" fill="#F2A98C" opacity=".75" />
      <circle cx="76.5" cy="83.5" r="3.4" fill="#F2A98C" opacity=".75" />
    </svg>
  );
}
