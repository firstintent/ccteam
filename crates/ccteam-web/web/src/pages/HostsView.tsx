// v0.8.24 Track A — 主机 panel (设置→主机), prototype `host-card` skin.
//
// Per machine: host-head (status dot · hostname · 本机/远程 badge · os/arch ·
// ccteam version · 重新探测) + one `agent-row` per vendor (dot+name | bin |
// version | MCP badge / register CTA) + the 「连接新主机(卫星节点)」 join
// card. The ONLY write is register-mcp (ccteam's own server into the vendor
// config — never a vendor login, never a CLI install).
//
// Data: GET /api/v1/hosts (registry) fanned into GET /api/v1/hosts/{host};
// a host whose detail probe fails renders as offline (honest state).

import { useCallback, useEffect, useState } from "react";
import {
  getHostDetail,
  getHosts,
  registerMcp,
  type HostDetail,
  type HostSummary,
} from "../lib/hostsApi";
import { makeT, type Lang } from "../lib/i18n";
import { vendorDotClass } from "../lib/vendors";

type HostState =
  | { kind: "ready"; detail: HostDetail }
  | { kind: "offline"; summary: HostSummary };

type LoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; hosts: HostState[] };

const REFRESH = "__refresh__";

async function probeAll(refresh: boolean): Promise<HostState[]> {
  const { hosts } = await getHosts();
  const summaries = hosts.length > 0 ? hosts : null;
  if (!summaries) {
    // No registry rows — probe the implicit local host directly.
    const detail = await getHostDetail("local", refresh);
    return [{ kind: "ready", detail }];
  }
  return Promise.all(
    summaries.map((summary) =>
      getHostDetail(summary.host, refresh)
        .then((detail) => ({ kind: "ready", detail }) as HostState)
        .catch(() => ({ kind: "offline", summary }) as HostState),
    ),
  );
}

export default function HostsView({ lang = "zh" }: { lang?: Lang } = {}) {
  const t = makeT(lang);
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  /** vendor token currently registering (scoped per host:vendor), or REFRESH. */
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(async (refresh: boolean) => {
    try {
      const hosts = await probeAll(refresh);
      setState({ kind: "ready", hosts });
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      const message = e instanceof Error ? e.message : "加载失败";
      setState((prev) => (prev.kind === "ready" ? prev : { kind: "error", message }));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    probeAll(false)
      .then((hosts) => {
        if (!cancelled) setState({ kind: "ready", hosts });
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

  const onRegister = async (host: string, vendor: string) => {
    setActionError(null);
    setBusy(`${host}:${vendor}`);
    try {
      await registerMcp(host, vendor);
      await load(true);
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        setActionError(
          `注册 MCP 失败（${vendor}）: ${e instanceof Error ? e.message : "未知错误"}`,
        );
      }
    } finally {
      setBusy(null);
    }
  };

  const origin =
    typeof window !== "undefined" && window.location ? window.location.origin : "https://<daemon>";

  return (
    <div data-testid="hosts-view" style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <header style={{ display: "flex", alignItems: "flex-start", gap: 14 }}>
        <div style={{ flex: 1 }}>
          <h1>{t("setHosts")}</h1>
          <p>{t("hostsDesc")}</p>
        </div>
        <button
          type="button"
          className="btn ghost"
          data-testid="hosts-refresh"
          onClick={() => void onRefresh()}
          disabled={busy !== null}
        >
          {busy === REFRESH ? t("probing") : t("reprobe")}
        </button>
      </header>

      {actionError ? (
        <div
          data-testid="hosts-action-error"
          role="alert"
          className="badge warn"
          style={{ padding: "8px 12px", borderRadius: 10, fontSize: 12.5 }}
        >
          {actionError}
        </div>
      ) : null}

      {state.kind === "loading" ? (
        <p data-testid="hosts-loading" style={{ fontSize: 13, color: "var(--text-faint)" }}>
          {t("probing")}
        </p>
      ) : state.kind === "error" ? (
        <div
          data-testid="hosts-error"
          role="alert"
          style={{
            border: "1px solid var(--red)",
            background: "var(--red-soft)",
            color: "var(--red-text)",
            borderRadius: "var(--radius-card)",
            padding: "14px 16px",
            fontSize: 13.5,
          }}
        >
          探测主机失败: {state.message}
        </div>
      ) : (
        state.hosts.map((h) =>
          h.kind === "ready" ? (
            <HostDetailCards
              key={h.detail.host}
              host={h.detail}
              busy={busy}
              lang={lang}
              onRegister={(vendor) => void onRegister(h.detail.host, vendor)}
            />
          ) : (
            <div className="host-card offline" key={h.summary.host} data-testid={`host-offline-${h.summary.host}`}>
              <div className="host-head">
                <span className="dot off" />
                <span className="hn">{h.summary.hostname || h.summary.host}</span>
                <span className="badge">{t("remoteBadge")}</span>
                <span className="badge warn">{t("offline")}</span>
                <div className="act">
                  <button type="button" className="btn ghost mini" onClick={() => void onRefresh()}>
                    {t("reconnect")}
                  </button>
                </div>
              </div>
              <div style={{ padding: "16px 20px", color: "var(--text-faint)", fontSize: 13 }}>
                {t("offlineRow")}
              </div>
            </div>
          ),
        )
      )}

      <div className="join-card">
        <h4>{t("joinTitle")}</h4>
        <p>{t("joinDesc")}</p>
        <pre>{`curl -fsSL https://ccteam.dev/install.sh | sh
ccteam host join --daemon ${origin} --token <join-token>`}</pre>
      </div>
    </div>
  );
}

export function HostDetailCards({
  host,
  busy,
  lang = "zh",
  onRegister,
}: {
  host: HostDetail;
  busy: string | null;
  lang?: Lang;
  onRegister: (vendor: string) => void;
}) {
  const t = makeT(lang);
  return (
    <div className="host-card">
      {/* hostname / machine bar */}
      <div className="host-head" data-testid="host-bar">
        <span className="dot on" />
        <span className="hn">{host.hostname}</span>
        {host.is_local ? (
          <span className="badge brand">{t("localBadge")}</span>
        ) : (
          <span className="badge">{t("remoteBadge")}</span>
        )}
        <span className="sys">
          {host.os}/{host.arch} · ccteam {host.ccteam_version}
        </span>
      </div>

      {host.agents.length === 0 ? (
        <div style={{ padding: "16px 20px", color: "var(--text-faint)", fontSize: 13 }}>
          未在 PATH 上发现 claude / codex / grok / opencode。安装后点上方「{t("reprobe")}」。
        </div>
      ) : (
        host.agents.map((agent) => {
          const registering = busy === `${host.host}:${agent.vendor}`;
          return (
            <div
              key={agent.vendor}
              className={`agent-row ${agent.installed ? "" : "absent"}`}
              data-testid={`agent-card-${agent.vendor}`}
            >
              <div className="v">
                <span className={vendorDotClass(agent.vendor)} />
                {agent.vendor}
              </div>
              <div className="bin" title={agent.hint ?? agent.harness_id}>
                {agent.bin}
              </div>
              <div className="ver">{agent.installed ? (agent.version ?? "已安装") : t("notInstalled")}</div>
              <div data-testid={`agent-status-${agent.vendor}`}>
                {!agent.installed ? (
                  <span className="badge">—</span>
                ) : !agent.mcp_registrable ? (
                  <span className="badge ok" title="MCP 随会话协议（无需注册）">
                    就绪 · MCP 随会话协议
                  </span>
                ) : agent.mcp_registered ? (
                  <span className="badge ok">就绪 · {t("mcpOk")}</span>
                ) : (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                    <span className="badge warn">需配置</span>
                    <button
                      type="button"
                      className="btn primary mini"
                      data-testid={`register-mcp-${agent.vendor}`}
                      disabled={busy !== null}
                      onClick={() => onRegister(agent.vendor)}
                    >
                      {registering ? "注册中…" : t("registerMcp")}
                    </button>
                  </span>
                )}
              </div>
            </div>
          );
        })
      )}
    </div>
  );
}
