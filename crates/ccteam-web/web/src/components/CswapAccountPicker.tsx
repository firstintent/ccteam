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

/** Claude account menu, a sibling of the model menu. New session: picks the
 *  account to start with; optional cswap discovery runs only when the user
 *  first opens it. Running session (`session` set): names the account the
 *  session runs on — the default login by its current cswap account — and a
 *  pick switches the session (#236); it hides itself on a machine without
 *  cswap unless the session is bound to an account. */
export function CswapAccountPicker({ value, onChange, lang, session }: {
  value: string;
  /** May return a promise (a session switch): the menu stays open showing
   *  the server's reason when it rejects. */
  onChange: (value: string) => void | Promise<void>;
  lang: Lang;
  session?: { sid: string; bound: CswapAccount | null };
}) {
  const [open, setOpen] = useState(false);
  const [accounts, setAccounts] = useState<CswapAccounts | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [switching, setSwitching] = useState(false);
  const [switchError, setSwitchError] = useState("");
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
  // A running session names its account up front (one cached discovery per
  // session opened, never a poll). Callers key the picker by sid, so a new
  // session starts from fresh state.
  const sid = session?.sid;
  useEffect(() => {
    if (!sid) return;
    let alive = true;
    void getCswapAccounts().then((found) => {
      if (alive) setAccounts(found);
    }).catch(() => {});
    return () => { alive = false; };
  }, [sid]);
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
    if (switching) return;
    if (id === value) {
      setOpen(false);
      return;
    }
    setSwitchError("");
    const pending = onChange(id);
    if (!pending) {
      setOpen(false);
      return;
    }
    setSwitching(true);
    void pending.then(() => setOpen(false)).catch((error: unknown) => {
      setSwitchError(error instanceof Error ? error.message : String(error));
    }).finally(() => setSwitching(false));
  };
  const title = en ? "Claude account" : "Claude 账号";
  const defaultAccount = accounts?.accounts.find((account) => account.active);
  const selected = accounts?.accounts.find((account) => account.id === value)
    ?? (session?.bound && session.bound.id === value ? session.bound : undefined);
  const defaultLabel = defaultAccount
    ? `${en ? "Default" : "默认"} · ${accountName(defaultAccount)}`
    : (en ? "Default login" : "默认登录");
  const label = !value ? defaultLabel : selected ? accountName(selected) : "cswap";
  const unavailable = accounts?.available === false;
  if (session && unavailable && !value) return null;
  const guide = session
    ? (en ? "Switching ends this session's process; your next message continues the same conversation on the new account (its prompt cache is rebuilt)."
      : "切换会结束当前进程，下一条消息在同一会话里按新账号继续（提示缓存会重建）。")
    : (en ? "Default login follows cswap auto; a chosen account stays on that account." : "默认登录会跟随 cswap auto 切换；指定账号固定使用该账号。");
  const status = switching ? (en ? "Switching…" : "切换中…")
    : switchError ? switchError
      : loading ? (en ? "Checking cswap…" : "检查 cswap…")
        : error ? error
          : unavailable
            ? `${en ? "cswap unavailable; default login remains available." : "cswap 不可用，仍可继续使用默认登录。"} ${accounts.message ?? ""}`
            : accounts?.accounts.length === 0
              ? (en ? "cswap has no accounts yet. You can still use default login." : "cswap 里还没有账号，仍可使用默认登录。")
              : accounts ? guide : "";
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
        data-testid="claude-account-menu" role="menu" aria-label={title} aria-busy={switching}>
        <div className="sel-group">{title}</div>
        <button type="button" role="menuitemradio" aria-checked={!value} disabled={switching}
          className={`sel-item ${!value ? "selected" : ""}`} onClick={() => pick("")}>
          {en ? "Default login" : "默认登录"}
          <span className="sub">{defaultAccount ? accountName(defaultAccount) : null}</span>
          <span className="check">✓</span>
        </button>
        {accounts?.accounts.map((account) => <button key={account.id} type="button" role="menuitemradio"
          aria-checked={value === account.id} disabled={switching}
          className={`sel-item account-row ${value === account.id ? "selected" : ""}`} onClick={() => pick(account.id)}>
          <span className="label">{accountName(account)}</span>
          <Quota account={account} en={en} />
          <span className="sub">{account.active ? (en ? "default" : "当前默认") : null}</span>
          <span className="check">✓</span>
        </button>)}
        {status ? <p className={`account-note${switchError ? " err" : ""}`} role="status">{status}</p> : null}
        {(error || unavailable) && !loading ? <button type="button" className="sel-item account-retry" onClick={load}>
          {en ? "Retry" : "重试"}
        </button> : null}
      </div>
    </div>
  );
}
