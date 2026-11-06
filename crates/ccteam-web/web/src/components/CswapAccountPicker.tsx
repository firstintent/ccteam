import { useEffect, useRef, useState } from "react";
import { CircleUserRound } from "lucide-react";
import { getCswapAccounts, type CswapAccounts } from "../lib/sessionsApi";
import type { Lang } from "../lib/i18n";

/** New-session Claude account menu, a sibling of the model menu. Optional
 *  cswap discovery runs only when the user first opens it. */
export function CswapAccountPicker({ value, onChange, lang }: {
  value: string;
  onChange: (value: string) => void;
  lang: Lang;
}) {
  const [open, setOpen] = useState(false);
  const [accounts, setAccounts] = useState<CswapAccounts | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const ref = useRef<HTMLDivElement | null>(null);
  const en = lang === "en";
  const load = () => {
    if (loading) return;
    setLoading(true);
    setError("");
    void getCswapAccounts().then(setAccounts).catch((error: unknown) => {
      setError(error instanceof Error ? error.message : String(error));
    }).finally(() => setLoading(false));
  };
  // pointerdown, not click: the neighbouring model button stops its click
  // from propagating, which would otherwise leave both menus open.
  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      if (event.target instanceof Node && ref.current?.contains(event.target)) return;
      setOpen(false);
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [open]);
  const pick = (id: string) => {
    onChange(id);
    setOpen(false);
  };
  const title = en ? "Claude account" : "Claude 账号";
  const selected = accounts?.accounts.find((account) => account.id === value);
  const label = !value ? (en ? "Default login" : "默认登录") : selected ? `cswap ${selected.slot}` : "cswap";
  const unavailable = accounts?.available === false;
  const status = loading ? (en ? "Checking cswap…" : "检查 cswap…")
    : error ? error
      : unavailable && accounts.reason === "not_granted"
        ? (en ? "No cswap accounts are granted to you yet; an administrator can grant them in Settings → Admin → Users. Default login remains available."
          : "管理员尚未给你授权 cswap 账号（设置 → 管理员 → 用户），仍可使用默认登录。")
        : unavailable
          ? `${en ? "cswap unavailable; default login remains available." : "cswap 不可用，仍可继续使用默认登录。"} ${accounts.message ?? ""}`
          : accounts?.accounts.length === 0
            ? (en ? "No authorized cswap accounts. You can still use default login." : "没有已授权的 cswap 账号，仍可使用默认登录。")
            : accounts
              ? (en ? "Default accounts follow cswap auto; other accounts use isolated profiles." : "当前默认账号会跟随 cswap auto；其他账号使用独立 profile。")
              : "";
  return (
    <div className={`sel account-sel ${open ? "open" : ""}`} ref={ref}>
      <button
        type="button"
        className="model-btn account-btn"
        data-testid="claude-account-btn"
        aria-label={`${title}: ${label}`}
        aria-haspopup="menu"
        aria-expanded={open}
        title={`${title}: ${selected ? `cswap ${selected.slot} · ${selected.label}` : label}`}
        onClick={() => {
          setOpen((current) => !current);
          if (!accounts && !loading) load();
        }}
      >
        <CircleUserRound />
        <span className="account-label">{label}</span>
      </button>
      <div className="sel-menu drop-up align-right account-menu" style={{ minWidth: 280 }}
        data-testid="claude-account-menu" role="menu" aria-label={title}>
        <div className="sel-group">{title}</div>
        <button type="button" role="menuitemradio" aria-checked={!value}
          className={`sel-item ${!value ? "selected" : ""}`} onClick={() => pick("")}>
          {en ? "Default login" : "默认登录"}
          <span className="check">✓</span>
        </button>
        {accounts?.accounts.map((account) => <button key={account.id} type="button" role="menuitemradio"
          aria-checked={value === account.id}
          className={`sel-item ${value === account.id ? "selected" : ""}`} onClick={() => pick(account.id)}>
          <span className="label">{`cswap ${account.slot} · ${account.label}`}</span>
          {account.active ? <span className="sub">{en ? "default" : "当前默认"}</span> : null}
          <span className="check">✓</span>
        </button>)}
        {status ? <p className="account-note" role="status">{status}</p> : null}
        {(error || unavailable) && !loading ? <button type="button" className="sel-item account-retry" onClick={load}>
          {en ? "Retry" : "重试"}
        </button> : null}
      </div>
    </div>
  );
}
