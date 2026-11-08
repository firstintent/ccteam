import { useEffect, useRef, useState } from "react";
import { CircleUserRound } from "lucide-react";
import { accountName, getCswapAccounts, type CswapAccount, type CswapAccounts } from "../lib/sessionsApi";
import type { Lang } from "../lib/i18n";

const STATUS: Record<string, [zh: string, en: string]> = {
  relogin_required: ["需重新登录", "re-login needed"],
  token_expired: ["令牌过期", "token expired"],
  no_credentials: ["无凭据", "no credentials"],
  keychain_unavailable: ["钥匙串不可用", "keychain unavailable"],
  foreign_credential: ["凭据不符", "foreign credential"],
  unavailable: ["额度暂不可读", "quota unavailable"],
};

/** 5h / 7d cells in fixed columns so rows line up; a plan without a 5h
 *  window (Max) leaves that cell empty. No reading → cswap's status. */
function Quota({ account, en }: { account: CswapAccount; en: boolean }) {
  const { five_hour_pct: fiveHour, seven_day_pct: sevenDay, usage_status: status } = account;
  if (fiveHour == null && sevenDay == null) {
    const text = status ? (STATUS[status]?.[en ? 1 : 0] ?? status) : "";
    return <span className="quota-status">{text}</span>;
  }
  // A non-ok status next to numbers means they are cswap's last good reading.
  const stale = status ? <span className="quota-stale" title={STATUS[status]?.[en ? 1 : 0] ?? status}>
    {en ? "last" : "上次"}
  </span> : null;
  const cell = (key: string, pct: number | undefined, extra?: React.ReactNode) => <span className="quota">
    {pct == null ? null : <><span className="k">{key}:</span><span className="v">{pct}%</span></>}
    {extra}
  </span>;
  return <>
    {cell("5h", fiveHour)}
    {cell("7d", sevenDay, stale)}
  </>;
}

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
  const label = !value ? (en ? "Default login" : "默认登录") : selected ? accountName(selected) : "cswap";
  const unavailable = accounts?.available === false;
  const status = loading ? (en ? "Checking cswap…" : "检查 cswap…")
    : error ? error
      : unavailable
        ? `${en ? "cswap unavailable; default login remains available." : "cswap 不可用，仍可继续使用默认登录。"} ${accounts.message ?? ""}`
        : accounts?.accounts.length === 0
          ? (en ? "cswap has no accounts yet. You can still use default login." : "cswap 里还没有账号，仍可使用默认登录。")
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
        title={`${title}: ${label}`}
        onClick={() => {
          // Re-read on every open so quota stays current (the daemon caches 30s).
          if (!open && !loading) load();
          setOpen((current) => !current);
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
          className={`sel-item account-row ${value === account.id ? "selected" : ""}`} onClick={() => pick(account.id)}>
          <span className="label">{accountName(account)}</span>
          <Quota account={account} en={en} />
          <span className="sub">{account.active ? (en ? "default" : "当前默认") : null}</span>
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
