import { useCallback, useEffect, useMemo, useState } from "react";
import { KeyRound, Link2, MessagesSquare, Network } from "lucide-react";
import { Button, Card, CardContent, CardHeader, CardTitle } from "../components/ui";
import { copyText } from "../lib/clipboard";
import { makeT, type Lang } from "../lib/i18n";
import { getToken } from "../lib/token";
import { getUserLink, listUsers, type TenantView } from "../lib/usersApi";
import { toastBus } from "../lib/toastBus";
import { JoinCard } from "./HostsView";
import SettingsPage from "./SettingsPage";

// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for focused tests.
export function externalMcpConfig(origin: string, token: string): string {
  return JSON.stringify(
    {
      mcpServers: {
        ccteam: {
          transport: "http",
          url: `${origin}/mcp`,
          headers: { Authorization: `Bearer ${token}` },
          disabled: false,
        },
      },
    },
    null,
    2,
  );
}

async function copyWithToast(value: string, success: string) {
  const ok = await copyText(value);
  if (ok) toastBus.handler?.info(success);
  else toastBus.handler?.error("复制失败 / Copy failed");
}

export default function AccessView({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  const origin = typeof window !== "undefined" && window.location ? window.location.origin : "";
  const snippet = useMemo(() => externalMcpConfig(origin, getToken() ?? ""), [origin]);

  return (
    <div data-testid="settings-access" className="flex flex-col gap-5">
      <header>
        <h1>{t("setAccess")}</h1>
        <p>{t("accessDesc")}</p>
      </header>

      <Card data-testid="access-mcp">
        <CardHeader><Network /><CardTitle>{t("accessMcpTitle")}</CardTitle></CardHeader>
        <CardContent className="flex flex-col gap-3">
          <p className="text-xs text-text-muted">{t("accessMcpDesc")}</p>
          <pre data-testid="access-mcp-snippet" className="overflow-x-auto rounded-lg border border-surface-700 bg-surface-950 p-3 text-[11px] text-text-secondary">{snippet}</pre>
          <Button data-testid="access-mcp-copy" variant="outline" size="sm" className="self-start" onClick={() => void copyWithToast(snippet, t("accessCopied"))}>
            {t("accessCopyConfig")}
          </Button>
        </CardContent>
      </Card>

      <section data-testid="access-satellite" className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold"><KeyRound className="mr-2 inline size-4" />{t("accessSatelliteTitle")}</h2>
        <JoinCard lang={lang} />
      </section>

      <section data-testid="access-im" className="flex flex-col gap-2">
        <h2 className="text-sm font-semibold"><MessagesSquare className="mr-2 inline size-4" />{t("accessImTitle")}</h2>
        <SettingsPage />
      </section>

      <LoginLinksCard lang={lang} />
    </div>
  );
}

export function LoginLinksCard({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  const [users, setUsers] = useState<TenantView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    listUsers().then(setUsers).catch((e) => {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      setError(e instanceof Error ? e.message : "load failed");
    });
  }, []);
  useEffect(() => load(), [load]);

  const copyLink = (user: TenantView) => {
    getUserLink(user.id)
      .then((res) => {
        const origin = typeof window !== "undefined" && window.location ? window.location.origin : "";
        return copyWithToast(`${origin}${res.personal_link}`, t("accessCopied"));
      })
      .catch((e) => {
        if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
          toastBus.handler?.error(e instanceof Error ? e.message : "copy failed");
        }
      });
  };

  return (
    <Card data-testid="access-login-links">
      <CardHeader><Link2 /><CardTitle>{t("accessLinksTitle")}</CardTitle></CardHeader>
      <CardContent className="flex flex-col gap-2">
        <p className="text-xs text-text-muted">{t("accessLinksDesc")}</p>
        {error ? <p role="alert" className="text-xs text-status-error">{error}</p> : null}
        {users === null ? <p className="text-xs text-text-dim">{t("loading")}</p> : null}
        {users?.map((user) => <LoginLinkRow key={user.id} user={user} label={t("accessCopyLink")} onCopy={() => copyLink(user)} />)}
      </CardContent>
    </Card>
  );
}

export function LoginLinkRow({ user, label, onCopy }: { user: TenantView; label: string; onCopy: () => void }) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border border-surface-800 px-3 py-2">
      <span className="font-mono text-xs">@{user.handle}</span>
      <Button data-testid={`access-copy-link-${user.id}`} variant="outline" size="sm" onClick={onCopy}>{label}</Button>
    </div>
  );
}
