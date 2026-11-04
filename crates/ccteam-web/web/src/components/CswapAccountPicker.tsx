import { useState } from "react";
import { getCswapAccounts, type CswapAccounts } from "../lib/sessionsApi";
import type { Lang } from "../lib/i18n";

/** Optional discovery runs only when the user opens advanced options. */
export function CswapAccountPicker({ value, onChange, lang }: {
  value: string;
  onChange: (value: string) => void;
  lang: Lang;
}) {
  const [accounts, setAccounts] = useState<CswapAccounts | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const en = lang === "en";
  const load = () => {
    if (loading) return;
    setLoading(true);
    setError("");
    void getCswapAccounts().then(setAccounts).catch((error: unknown) => {
      setError(error instanceof Error ? error.message : String(error));
    }).finally(() => setLoading(false));
  };
  return (
    <details className="cswap-options" onToggle={(event) => {
      if (event.currentTarget.open && !accounts && !loading) load();
    }}>
      <summary>{en ? "Advanced options" : "高级选项"}</summary>
      <label>
        {en ? "Claude account" : "Claude 账号"}
        <select aria-label={en ? "Claude account" : "Claude 账号"}
          value={value} onChange={(event) => onChange(event.target.value)}>
          <option value="">{en ? "Default login" : "默认登录"}</option>
          {accounts?.accounts.map((account) => <option key={account.id} value={account.id}>
            {`cswap ${account.slot} · ${account.label}${account.active ? (en ? " · default" : " · 当前默认") : ""}`}
          </option>)}
        </select>
      </label>
      <small role="status">
        {loading ? (en ? "Checking cswap…" : "检查 cswap…")
          : error || (accounts?.available === false
            ? `${en ? "cswap unavailable; default login remains available." : "cswap 不可用，仍可继续使用默认登录。"} ${accounts.message ?? ""}`
            : accounts?.accounts.length === 0
              ? (en ? "No authorized cswap accounts. You can still use default login." : "没有已授权的 cswap 账号，仍可使用默认登录。")
              : en ? "Default accounts follow cswap auto; other accounts use isolated profiles." : "当前默认账号会跟随 cswap auto；其他账号使用独立 profile。")}
      </small>
      {(error || accounts?.available === false) && <button type="button" onClick={load}>
        {en ? "Retry" : "重试"}
      </button>}
    </details>
  );
}
