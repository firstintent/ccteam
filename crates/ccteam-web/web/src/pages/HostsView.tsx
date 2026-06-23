// v0.8.18 柱1 — 主机 / Hosts global view. The host-first setup surface:
// per machine (today one: `local` = this machine) the hostname + os/arch +
// ccteam version, and per agent vendor whether it is installed (+ version),
// whether the ccteam MCP server is registered, and a ready/needs_config/
// not_installed status. The ONLY write is "register MCP" (ccteam's own
// server into the vendor config — never a vendor login, never a CLI install).
//
// Data: `GET /api/v1/hosts/{host}` (hostsApi); `refresh` forces a re-probe.
// Theme tokens only (surface-*/brand-*/text-*/status-* + vendor-*). The
// per-agent cards stay CARDS (not a table): each is a header status badge +
// install/version/MCP detail + an optional register CTA — a card pattern, and
// at most a couple agents per host. They standardize on the Card primitive so
// the surface reads one with Status/Marketplace.

import { useCallback, useEffect, useState } from "react";
import { RefreshCw, ServerOff } from "lucide-react";
import {
  getHostDetail,
  registerMcp,
  type AgentHealth,
  type HostDetail,
} from "../lib/hostsApi";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  EmptyState,
  Skeleton,
  type BadgeProps,
} from "../components/ui";

type LoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; host: HostDetail };

const REFRESH = "__refresh__";

export default function HostsView() {
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  /** vendor token currently registering, or REFRESH while re-probing. */
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // Shared fetch for the manual re-probe + post-register reload (event
  // handlers, so a setState after `await` is fine here — not an effect).
  const load = useCallback(async (refresh: boolean) => {
    try {
      const host = await getHostDetail("local", refresh);
      setState({ kind: "ready", host });
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      const message = e instanceof Error ? e.message : "加载失败";
      // Don't blank an already-rendered host on a transient re-probe failure.
      setState((prev) => (prev.kind === "ready" ? prev : { kind: "error", message }));
    }
  }, []);

  // Initial probe on mount. Mirrors StatusView: the fetch resolves in a
  // `.then` callback (never a synchronous setState in the effect body), and a
  // cancel guard drops a late resolve after unmount.
  useEffect(() => {
    let cancelled = false;
    getHostDetail("local", false)
      .then((host) => {
        if (!cancelled) setState({ kind: "ready", host });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
        const message = e instanceof Error ? e.message : "加载失败";
        setState((prev) => (prev.kind === "ready" ? prev : { kind: "error", message }));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const onRefresh = async () => {
    setActionError(null);
    setBusy(REFRESH);
    await load(true);
    setBusy(null);
  };

  const onRegister = async (vendor: string) => {
    setActionError(null);
    setBusy(vendor);
    try {
      await registerMcp("local", vendor);
      await load(true);
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        setActionError(`注册 MCP 失败（${vendor}）: ${e instanceof Error ? e.message : "未知错误"}`);
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <div data-testid="hosts-view" className="p-6 max-w-[880px] mx-auto">
      <div className="flex items-center gap-3">
        <div>
          <h2 className="text-base font-semibold text-text-primary">主机 · Hosts</h2>
          <p className="mt-1 text-sm text-text-secondary">
            这台机器装了哪些 agent、登录/MCP 注册状态。机器为主轴，将来分布式会列出每台。
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          data-testid="hosts-refresh"
          onClick={onRefresh}
          disabled={busy !== null}
          className="ml-auto shrink-0"
        >
          <RefreshCw className={busy === REFRESH ? "animate-spin" : ""} />
          {busy === REFRESH ? "重探中…" : "重新探测"}
        </Button>
      </div>

      {actionError ? (
        <div
          data-testid="hosts-action-error"
          role="alert"
          className="mt-3 rounded-md border border-status-error/40 bg-status-error/10 px-3 py-2 text-xs text-status-error"
        >
          {actionError}
        </div>
      ) : null}

      <div className="mt-4 space-y-3">
        {state.kind === "loading" ? (
          <div data-testid="hosts-loading" className="space-y-3">
            <Skeleton className="h-12 w-full" />
            <Skeleton className="h-24 w-full" />
            <Skeleton className="h-24 w-full" />
          </div>
        ) : state.kind === "error" ? (
          <div
            data-testid="hosts-error"
            role="alert"
            className="rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-4 text-sm text-status-error"
          >
            探测主机失败: {state.message}
          </div>
        ) : (
          <HostDetailCards host={state.host} busy={busy} onRegister={onRegister} />
        )}
      </div>
    </div>
  );
}

export function HostDetailCards({
  host,
  busy,
  onRegister,
}: {
  host: HostDetail;
  busy: string | null;
  onRegister: (vendor: string) => void;
}) {
  return (
    <>
      {/* hostname / machine bar */}
      <div
        data-testid="host-bar"
        className="rounded-lg bg-surface-900 ring-1 ring-surface-700/50 px-4 py-3 flex items-center gap-2.5 flex-wrap"
      >
        <span className="h-2.5 w-2.5 rounded-full bg-status-running" aria-hidden />
        <span className="text-sm font-semibold text-text-primary">{host.hostname}</span>
        <Badge variant="brand">{host.is_local ? "本机 local" : host.host}</Badge>
        <span className="ml-auto font-mono text-[11px] text-text-dim tabular-nums">
          {host.os}/{host.arch} · ccteam {host.ccteam_version}
        </span>
      </div>

      {/* agents */}
      {host.agents.length === 0 ? (
        <EmptyState
          icon={ServerOff}
          title="这台主机没有探测到 agent"
          description="未在 PATH 上发现 claude / codex。安装后点上方「重新探测」。"
        />
      ) : (
        host.agents.map((agent) => (
          <AgentCard
            key={agent.vendor}
            agent={agent}
            busy={busy === agent.vendor}
            disabled={busy !== null}
            onRegister={() => onRegister(agent.vendor)}
          />
        ))
      )}
    </>
  );
}

function AgentCard({
  agent,
  busy,
  disabled,
  onRegister,
}: {
  agent: AgentHealth;
  busy: boolean;
  disabled: boolean;
  onRegister: () => void;
}) {
  const sev = statusMeta(agent.status);
  const vendorClass = agent.vendor === "claude" ? "text-vendor-claude" : "text-vendor-codex";
  return (
    <Card data-testid={`agent-card-${agent.vendor}`}>
      <CardHeader>
        <CardTitle className={vendorClass}>{agent.vendor}</CardTitle>
        <span className="text-[11px] text-text-dim">{agent.harness_id}</span>
        <Badge
          variant={sev.variant}
          className={sev.className}
          data-testid={`agent-status-${agent.vendor}`}
        >
          {sev.label}
        </Badge>
      </CardHeader>
      <CardContent>
        <div className="flex items-center gap-2 text-xs text-text-secondary flex-wrap">
          <span>{agent.installed ? "已安装" : "未安装"}</span>
          {agent.version ? <span className="font-mono text-text-dim">{agent.version}</span> : null}
          <span className="text-text-dim">·</span>
          <span>{agent.mcp_registered ? "MCP 已注册" : "MCP 未注册"}</span>
        </div>

        {agent.hint ? <div className="mt-2 text-[11px] text-text-dim">{agent.hint}</div> : null}

        {agent.installed && !agent.mcp_registered ? (
          <Button
            size="sm"
            data-testid={`register-mcp-${agent.vendor}`}
            onClick={onRegister}
            disabled={disabled}
            className="mt-2 border border-brand-500/40 bg-brand-500/10 text-brand-400 hover:bg-brand-500/20"
          >
            {busy ? "注册中…" : "注册 ccteam MCP"}
          </Button>
        ) : null}
      </CardContent>
    </Card>
  );
}

function statusMeta(status: string): {
  label: string;
  variant: BadgeProps["variant"];
  className?: string;
} {
  switch (status) {
    case "ready":
      return { label: "就绪", variant: "running" };
    case "needs_config":
      return { label: "需配置", variant: "waiting" };
    case "not_installed":
      return { label: "未安装", variant: "idle", className: "bg-surface-700/50 text-text-dim" };
    default:
      return { label: status, variant: "idle", className: "bg-surface-700/50 text-text-dim" };
  }
}
