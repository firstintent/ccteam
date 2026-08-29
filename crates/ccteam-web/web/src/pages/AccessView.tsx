import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Braces, KeyRound, Link2, MessageSquare, Network, Send } from "lucide-react";
import {
  Button,
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
  Combobox,
  Input,
  Label,
  type ComboboxOption,
} from "../components/ui";
import { copyText } from "../lib/clipboard";
import { getImConfig, type ImConfigStatus } from "../lib/configApi";
import { useProjectsStore } from "../hooks/useProjectsStore";
import {
  listEnrollments,
  mintProjectEnrollment,
  mintUserEnrollment,
  orderSnippets,
  revokeEnrollment,
  type EnrollCredentialView,
  type MintedEnrollment,
} from "../lib/enrollApi";
import { makeT, type Lang } from "../lib/i18n";
import { getToken } from "../lib/token";
import { getUserLink, listUsers, type TenantView } from "../lib/usersApi";
import { useMe } from "../hooks/useMe";
import { toastBus } from "../lib/toastBus";
import { JoinCard } from "./HostsView";
import { LarkSection, MyImSection, TelegramSection } from "./SettingsPage";

const CODE_PRE_CLASS =
  "max-h-80 overflow-auto rounded-lg border border-surface-700 bg-surface-950 p-3 text-[11px] text-text-secondary";

// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for focused tests.
export function externalRestSnippet(origin: string, token: string, lang: Lang): string {
  const t = makeT(lang);
  return `TOKEN='${token}'
# 1) ${t("accessApiStepCreate")} (vendor: claude|codex|grok|opencode|kimi|pi|dsh) -> {"sid":"s42"}
curl -sX POST ${origin}/api/v1/projects/<project-slug>/sessions \\
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \\
  -d '{"role":"","vendor":"claude"}'
# 2) ${t("accessApiStepSend")} (202, async)
curl -sX POST ${origin}/api/v1/sessions/s42/turn \\
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \\
  -d '{"text":"hello"}'
# 3) ${t("accessApiStepStream")} (SSE)
curl -N ${origin}/api/v1/sessions/s42/events -H "Authorization: Bearer $TOKEN"`;
}

async function copyWithToast(value: string, success: string) {
  const ok = await copyText(value);
  if (ok) toastBus.handler?.info(success);
  else toastBus.handler?.error("复制失败 / Copy failed");
}

export default function AccessView({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  const { isAdmin } = useMe();
  const origin = typeof window !== "undefined" && window.location ? window.location.origin : "";
  const token = getToken() ?? "";
  const apiSnippet = useMemo(
    () => externalRestSnippet(origin, token, lang),
    [origin, token, lang],
  );
  const [config, setConfig] = useState<ImConfigStatus | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);

  const reloadConfig = useCallback(() => {
    if (!isAdmin) return;
    getImConfig()
      .then((next) => {
        setConfig(next);
        setConfigError(null);
      })
      .catch((error) => {
        const message = error instanceof Error ? error.message : String(error);
        if (message !== "UNAUTHENTICATED") setConfigError(message);
      });
  }, [isAdmin]);

  useEffect(() => {
    if (!isAdmin) return;
    let cancelled = false;
    getImConfig()
      .then((next) => {
        if (!cancelled) setConfig(next);
      })
      .catch((error) => {
        if (cancelled) return;
        const message = error instanceof Error ? error.message : String(error);
        if (message !== "UNAUTHENTICATED") setConfigError(message);
      });
    return () => {
      cancelled = true;
    };
  }, [isAdmin]);

  return (
    <div data-testid="settings-access" className="flex flex-col gap-7">
      <header>
        <h1>{t("setAccess")}</h1>
        <p>{t("accessDesc")}</p>
      </header>

      <AccessGroup
        testId="access-people"
        contentTestId="access-im"
        label={t("accessPeopleGroup")}
      >
        {isAdmin ? (
          <>
            {config?.transport_warning ? (
              <div
                data-testid="settings-transport-warning"
                role="status"
                className="lg:col-span-2 rounded-lg border border-brand-500/30 bg-brand-500/10 px-3 py-2 font-mono text-[11px] text-brand-400"
              >
                {config.transport_warning}
              </div>
            ) : null}
            {configError ? (
              <div
                data-testid="settings-error"
                role="alert"
                className="lg:col-span-2 rounded-lg border border-status-error/30 bg-status-error/10 px-3 py-2 font-mono text-[11px] text-status-error"
              >
                {t("accessConfigError")}: {configError}
              </div>
            ) : null}
            {config ? (
              <>
                <TelegramSection lang={lang} status={config.telegram} onSaved={reloadConfig} />
                <LarkSection lang={lang} status={config.lark} onSaved={reloadConfig} />
              </>
            ) : (
              <>
                <CredentialPlaceholder
                  testId="settings-telegram"
                  icon={<Send />}
                  title="Telegram"
                  loadingLabel={t("loading")}
                />
                <CredentialPlaceholder
                  testId="settings-lark"
                  icon={<MessageSquare />}
                  title="Lark / 飞书"
                  loadingLabel={t("loading")}
                />
              </>
            )}
            <LoginLinksCard lang={lang} className="lg:col-span-2" />
          </>
        ) : (
          <Card data-testid="access-my-im" className="lg:col-span-2">
            <CardContent>
              <MyImSection />
            </CardContent>
          </Card>
        )}
      </AccessGroup>

      <AccessGroup testId="access-programs" label={t("accessProgramsGroup")}>
        <ExternalAgentCard lang={lang} />

        <Card data-testid="access-api" className="flex h-full flex-col">
          <CardHeader>
            <Braces />
            <CardTitle>{t("accessApiTitle")}</CardTitle>
          </CardHeader>
          <CardContent className="flex flex-1 flex-col gap-3">
            <p className="text-xs text-text-muted">{t("accessApiDesc")}</p>
            <div className="flex items-center justify-between gap-3 rounded-md border border-surface-800 px-3 py-2">
              <div className="min-w-0">
                <div className="text-[10px] uppercase tracking-[0.14em] text-text-dim">
                  {t("accessApiBaseUrl")}
                </div>
                <code className="block truncate text-xs text-text-secondary">{origin}/api/v1</code>
              </div>
              <Button
                data-testid="access-api-base-copy"
                variant="outline"
                size="sm"
                onClick={() => void copyWithToast(`${origin}/api/v1`, t("accessCopied"))}
              >
                {t("accessCopyBaseUrl")}
              </Button>
            </div>
            <pre data-testid="access-api-snippet" className={CODE_PRE_CLASS}>
              {apiSnippet}
            </pre>
            <Button
              data-testid="access-api-copy"
              variant="outline"
              size="sm"
              className="self-start"
              onClick={() => void copyWithToast(apiSnippet, t("accessCopied"))}
            >
              {t("accessCopySnippet")}
            </Button>
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-text-muted">
              <a
                href="/api/docs"
                target="_blank"
                rel="noreferrer"
                className="text-brand-400 hover:underline"
              >
                {t("accessApiReference")}
              </a>
              <span>
                {t("accessApiOpenApi")}: <code>/api/v1/openapi.json</code>
              </span>
            </div>
          </CardContent>
          <CardFooter>{t("accessApiFooter")}</CardFooter>
        </Card>
      </AccessGroup>

      <AccessGroup testId="access-machines" label={t("accessMachinesGroup")}>
        <Card data-testid="access-satellite" className="lg:col-span-2">
          <CardHeader>
            <KeyRound />
            <CardTitle>{t("accessSatelliteTitle")}</CardTitle>
          </CardHeader>
          <CardContent>
            <JoinCard lang={lang} bare />
          </CardContent>
        </Card>
      </AccessGroup>
    </div>
  );
}

/** The one-click MCP config an EXTERNAL agent pastes.
 *
 *  What it hands out is a SCOPED credential, minted by the daemon, not the
 *  operator's own login token: a snippet is meant to be given away, so its
 *  safety has to come from scope (one workspace) rather than secrecy. That is
 *  why the default scope is a project and why the machine-user option is
 *  spelled out as "all my projects" — the trade is visible at the moment of
 *  choosing. The snippet bodies are rendered SERVER-side by the same writers
 *  that register ccteam in a vendor's real config, so what is copied here is
 *  byte-identical to what `ccteam config` would have written. */
export function ExternalAgentCard({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  const { projects: projectRows, loading: projectsLoading } = useProjectsStore();
  const projects = useMemo(
    () => projectRows?.map((project) => project.slug) ?? (projectsLoading ? null : []),
    [projectRows, projectsLoading],
  );
  const [selectedScope, setSelectedScope] = useState("");
  const validProjectScopes = useMemo(
    () => new Set((projects ?? []).map((slug) => `project:${slug}`)),
    [projects],
  );
  const scope =
    selectedScope === "user" || validProjectScopes.has(selectedScope)
      ? selectedScope
      : projects?.[0]
        ? `project:${projects[0]}`
        : projects === null
          ? ""
          : "user";
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [minted, setMinted] = useState<MintedEnrollment | null>(null);
  const [shown, setShown] = useState("");
  const [credentials, setCredentials] = useState<EnrollCredentialView[] | null>(null);

  const swallowUnauthenticated = (e: unknown, fallback: string) => {
    if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
    setError(e instanceof Error ? e.message : fallback);
  };

  useEffect(() => {
    let cancelled = false;
    listEnrollments()
      .then((rows) => {
        if (!cancelled) setCredentials(rows);
      })
      .catch(() => {
        if (!cancelled) setCredentials([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const scopeOptions: ComboboxOption[] = useMemo(
    () => [
      ...(projects ?? []).map((slug) => ({
        value: `project:${slug}`,
        label: slug,
        hint: t("accessMcpScopeProjectHint"),
      })),
      { value: "user", label: t("accessMcpScopeUser"), hint: t("accessMcpScopeUserHint") },
    ],
    [projects, t],
  );

  const snippets = useMemo(() => orderSnippets(minted?.snippets ?? []), [minted]);
  const current = snippets.find((s) => s.vendor === shown) ?? snippets[0] ?? null;

  // The unlabelled MACHINE-USER slot is the daemon's own credential (the one
  // written into the harness global configs), so a user-scoped mint has to name
  // its slot. The API refuses it either way — this just stops the form from
  // offering a request that cannot succeed.
  const labelRequired = scope === "user";
  const canMint = Boolean(scope) && (!labelRequired || label.trim().length > 0);

  const onMint = async () => {
    if (!canMint) return;
    setBusy(true);
    setError(null);
    const trimmed = label.trim();
    const req = trimmed ? { label: trimmed } : {};
    // The scope choice picks the ROUTE, not a body field: a project-scoped mint
    // is addressed as `/projects/<slug>/enroll` so the daemon's project ACL
    // gates it by path shape.
    try {
      const res =
        scope === "user"
          ? // `canMint` above is what guarantees the non-empty label the route
            // (and this function's type) demands.
            await mintUserEnrollment({ label: trimmed })
          : await mintProjectEnrollment(scope.replace(/^project:/, ""), req);
      setMinted(res);
      setShown(res.snippets[0]?.vendor ?? "");
      setCredentials((prev) => [res.credential, ...(prev ?? [])]);
      setLabel("");
    } catch (e) {
      swallowUnauthenticated(e, t("accessMcpMintFailed"));
    } finally {
      setBusy(false);
    }
  };

  const onRevoke = async (id: string) => {
    setError(null);
    try {
      await revokeEnrollment(id);
      setCredentials((prev) => (prev ?? []).filter((c) => c.id !== id));
      // A revoked credential's snippet is dead — stop offering it for copy.
      setMinted((prev) => (prev?.credential.id === id ? null : prev));
    } catch (e) {
      swallowUnauthenticated(e, t("accessMcpRevokeFailed"));
    }
  };

  return (
    <Card data-testid="access-mcp" className="flex h-full flex-col">
      <CardHeader>
        <Network />
        <CardTitle>{t("accessMcpTitle")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-1 flex-col gap-3">
        <p className="text-xs text-text-muted">{t("accessMcpDesc")}</p>

        <div className="grid gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1">
            {/* No htmlFor: the Combobox's native select is aria-hidden (the
                styled trigger is the control), so it carries `ariaLabel`. */}
            <Label>{t("accessMcpScopeLabel")}</Label>
            <Combobox
              data-testid="access-mcp-scope"
              name="enroll-scope"
              ariaLabel={t("accessMcpScopeLabel")}
              value={scope}
              onChange={setSelectedScope}
              options={scopeOptions}
              searchable={false}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="enroll-label">
              {t(labelRequired ? "accessMcpLabelLabelRequired" : "accessMcpLabelLabel")}
            </Label>
            <Input
              id="enroll-label"
              data-testid="access-mcp-label"
              value={label}
              required={labelRequired}
              placeholder={t("accessMcpLabelPh")}
              onChange={(e) => setLabel(e.target.value)}
            />
            {labelRequired ? (
              <p data-testid="access-mcp-label-why" className="text-xs text-text-dim">
                {t("accessMcpLabelWhy")}
              </p>
            ) : null}
          </div>
        </div>
        {projects?.length === 0 ? (
          <p className="text-xs text-text-dim">{t("accessMcpNoProjects")}</p>
        ) : null}

        <Button
          data-testid="access-mcp-mint"
          size="sm"
          className="self-start"
          disabled={busy || !canMint}
          onClick={() => void onMint()}
        >
          {busy ? t("accessMcpMinting") : t("accessMcpMint")}
        </Button>

        {error ? (
          <p role="alert" data-testid="access-mcp-error" className="text-xs text-status-error">
            {error}
          </p>
        ) : null}

        {minted && current ? (
          <>
            <div
              role="status"
              data-testid="access-mcp-warning"
              className="rounded-lg border border-brand-500/30 bg-brand-500/10 px-3 py-2 text-[11px] text-brand-400"
            >
              {t("accessMcpWarn")}
              {minted.insecure_transport ? ` ${t("accessMcpWarnPlainHttp")}` : null}
            </div>
            <div className="text-[10px] uppercase tracking-[0.14em] text-text-dim">
              {t("accessMcpEndpoint")}: <code className="normal-case">{minted.url}</code>
            </div>
            <div className="flex flex-wrap gap-2">
              {snippets.map((s) => (
                <Button
                  key={s.vendor}
                  data-testid={`access-mcp-copy-${s.vendor}`}
                  variant={s.vendor === current.vendor ? "default" : "outline"}
                  size="sm"
                  onClick={() => {
                    setShown(s.vendor);
                    void copyWithToast(s.body, t("accessCopied"));
                  }}
                >
                  {s.vendor}
                </Button>
              ))}
            </div>
            <div className="text-[11px] text-text-muted">
              {t("accessMcpSnippetFor")} <code>{current.path}</code> ({current.format})
            </div>
            <pre data-testid="access-mcp-snippet" className={CODE_PRE_CLASS}>
              {current.body}
            </pre>
          </>
        ) : null}

        <div className="flex flex-col gap-2">
          <div className="text-[10px] uppercase tracking-[0.14em] text-text-dim">
            {t("accessMcpExisting")}
          </div>
          {credentials === null ? <p className="text-xs text-text-dim">{t("loading")}</p> : null}
          {credentials?.length === 0 ? (
            <p className="text-xs text-text-dim">{t("accessMcpExistingEmpty")}</p>
          ) : null}
          {credentials?.map((c) => (
            <div
              key={c.id}
              data-testid={`access-mcp-cred-${c.id}`}
              className="flex min-w-0 items-center justify-between gap-3 rounded-md border border-surface-800 px-3 py-2"
            >
              <div className="min-w-0">
                <div className="truncate text-xs text-text-secondary">
                  {c.label ?? c.id}
                  <span className="ml-2 text-text-dim">
                    {c.scope === "project" ? c.project : t("accessMcpScopeAll")}
                  </span>
                </div>
                <code className="block truncate text-[10px] text-text-dim">
                  {c.bearer_prefix}…
                </code>
              </div>
              <Button
                data-testid={`access-mcp-revoke-${c.id}`}
                variant="outline"
                size="sm"
                onClick={() => void onRevoke(c.id)}
              >
                {t("accessMcpRevoke")}
              </Button>
            </div>
          ))}
        </div>
      </CardContent>
      <CardFooter>{t("accessMcpFooter")}</CardFooter>
    </Card>
  );
}

function AccessGroup({
  testId,
  contentTestId,
  label,
  children,
}: {
  testId: string;
  contentTestId?: string;
  label: string;
  children: ReactNode;
}) {
  return (
    <section data-testid={testId} className="flex flex-col gap-2">
      <h2 className="text-[11px] font-semibold uppercase tracking-[0.16em] text-text-dim">
        {label}
      </h2>
      <div data-testid={contentTestId} className="grid gap-4 lg:grid-cols-2">
        {children}
      </div>
    </section>
  );
}

function CredentialPlaceholder({
  testId,
  icon,
  title,
  loadingLabel,
}: {
  testId: string;
  icon: ReactNode;
  title: string;
  loadingLabel: string;
}) {
  return (
    <Card data-testid={testId} aria-busy="true">
      <CardHeader>
        {icon}
        <CardTitle>{title}</CardTitle>
      </CardHeader>
      <CardContent className="text-xs text-text-dim">{loadingLabel}</CardContent>
    </Card>
  );
}

export function LoginLinksCard({ lang, className }: { lang: Lang; className?: string }) {
  const t = makeT(lang);
  const [users, setUsers] = useState<TenantView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    listUsers()
      .then(setUsers)
      .catch((e) => {
        if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
        setError(e instanceof Error ? e.message : "load failed");
      });
  }, []);
  useEffect(() => load(), [load]);

  const copyLink = (user: TenantView) => {
    getUserLink(user.id)
      .then((res) => {
        const linkOrigin =
          typeof window !== "undefined" && window.location ? window.location.origin : "";
        return copyWithToast(`${linkOrigin}${res.personal_link}`, t("accessCopied"));
      })
      .catch((e) => {
        if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
          toastBus.handler?.error(e instanceof Error ? e.message : "copy failed");
        }
      });
  };

  return (
    <Card data-testid="access-login-links" className={className}>
      <CardHeader>
        <Link2 />
        <CardTitle>{t("accessLinksTitle")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        <p className="text-xs text-text-muted">{t("accessLinksDesc")}</p>
        {error ? (
          <p role="alert" className="text-xs text-status-error">
            {error}
          </p>
        ) : null}
        {users === null ? <p className="text-xs text-text-dim">{t("loading")}</p> : null}
        {users?.length === 0 ? (
          <p className="text-xs text-text-dim">{t("accessLinksEmpty")}</p>
        ) : null}
        {users && users.length > 0 ? (
          <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
            {users.map((user) => (
              <LoginLinkRow
                key={user.id}
                user={user}
                label={t("accessCopyLink")}
                onCopy={() => copyLink(user)}
              />
            ))}
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}

export function LoginLinkRow({
  user,
  label,
  onCopy,
}: {
  user: TenantView;
  label: string;
  onCopy: () => void;
}) {
  return (
    <div className="flex min-w-0 items-center justify-between gap-3 rounded-md border border-surface-800 px-3 py-2">
      <span className="truncate font-mono text-xs">@{user.handle}</span>
      <Button
        data-testid={`access-copy-link-${user.id}`}
        variant="outline"
        size="sm"
        onClick={onCopy}
      >
        {label}
      </Button>
    </div>
  );
}
