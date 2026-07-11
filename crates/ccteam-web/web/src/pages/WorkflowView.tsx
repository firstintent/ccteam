// v0.8.24 Track A — 工作流 top-level view (prototype `#view-flow`):
// a set-nav second column (232px, 「工作流」) with five sub-pages —
// Skills / Roles / MCP Servers / 自进化 / Compare — each a prototype-styled
// detail page (flow-rows lists, stat cards, compare launcher). Data wiring
// unchanged: listProjectRoles / getProjectMarketplace / getEvolution /
// runCompare (lib/workflowApi).

import { useCallback, useEffect, useMemo, useState } from "react";
import { Activity, GitCompareArrows, Package, Server, User } from "lucide-react";
import { listProjectRoles, type RoleSummary } from "../lib/sessionsApi";
import { getProjectMarketplace, type DecoratedPlugin } from "../lib/marketplaceApi";
import {
  getEvolution,
  runCompare,
  type CompareResult,
  type EvolutionSummary,
} from "../lib/workflowApi";
import { fetchDashboard } from "../lib/dashboardApi";
import { makeT, tr, type Lang } from "../lib/i18n";
import { toastBus } from "../lib/toastBus";
import { vendorDotClass } from "../lib/vendors";

type TabId = "skills" | "roles" | "mcp" | "evolution" | "compare";

const TABS: { id: TabId; label: string; labelKey?: string; subKey: string; icon: React.ReactNode }[] = [
  { id: "skills", label: "Skills", subKey: "skillsSub", icon: <Package /> },
  { id: "roles", label: "Roles", subKey: "rolesSub", icon: <User /> },
  { id: "mcp", label: "MCP Servers", subKey: "mcpSub", icon: <Server /> },
  { id: "evolution", label: "自进化", labelKey: "evolve", subKey: "evolveSub", icon: <Activity /> },
  { id: "compare", label: "Compare", subKey: "compareSub", icon: <GitCompareArrows /> },
];

const COMPARE_VENDORS = ["claude", "codex", "grok", "opencode"] as const;

function isTab(v: string | undefined): v is TabId {
  return !!v && TABS.some((t) => t.id === v);
}

export default function WorkflowView({
  tab: routeTab,
  onNav,
  onOpenMarket,
  lang: langProp,
}: {
  tab?: string;
  onNav?: (tab: TabId) => void;
  onOpenMarket?: () => void;
  lang?: Lang;
} = {}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  const zh = lang !== "en";
  const [localTab, setLocalTab] = useState<TabId>("skills");
  const tab: TabId = isTab(routeTab) ? routeTab : localTab;
  const setTab = (next: TabId) => {
    setLocalTab(next);
    onNav?.(next);
  };

  const [projects, setProjects] = useState<string[]>([]);
  const [slug, setSlug] = useState("");
  const [roles, setRoles] = useState<RoleSummary[]>([]);
  const [skills, setSkills] = useState<DecoratedPlugin[]>([]);
  const [evolution, setEvolution] = useState<EvolutionSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [comparePrompt, setComparePrompt] = useState("");
  const [compareVendors, setCompareVendors] = useState<string[]>(["claude", "codex"]);
  const [compareResult, setCompareResult] = useState<CompareResult | null>(null);
  const [compareBusy, setCompareBusy] = useState(false);

  useEffect(() => {
    void fetchDashboard()
      .then((rowsRes) => {
        const slugs = rowsRes.map((p) => p.slug).filter(Boolean);
        setProjects(slugs);
        setSlug((cur) => cur || slugs[0] || "");
      })
      .catch(() => setProjects([]));
  }, []);

  const refreshTab = useCallback(async () => {
    if (!slug) return;
    setLoading(true);
    try {
      if (tab === "roles") {
        setRoles(await listProjectRoles(slug));
      } else if (tab === "skills") {
        const idx = await getProjectMarketplace(slug);
        setSkills((idx.plugins ?? []).filter((p) => p.type === "skill"));
      } else if (tab === "evolution") {
        setEvolution(await getEvolution(slug));
      }
    } catch (e) {
      toastBus.handler?.error(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [slug, tab]);

  useEffect(() => {
    void refreshTab();
  }, [refreshTab]);

  const toggleVendor = (v: string) => {
    setCompareVendors((prev) => (prev.includes(v) ? prev.filter((x) => x !== v) : [...prev, v]));
  };

  const onCompare = async () => {
    if (!slug || !comparePrompt.trim() || compareVendors.length < 2) {
      toastBus.handler?.error(
        tr(lang, "请选择项目、至少 2 个 vendor，并填写问题", "Pick project, ≥2 vendors, and a prompt"),
      );
      return;
    }
    setCompareBusy(true);
    setCompareResult(null);
    try {
      const res = await runCompare(slug, comparePrompt.trim(), compareVendors);
      setCompareResult(res);
    } catch (e) {
      toastBus.handler?.error(e instanceof Error ? e.message : String(e));
    } finally {
      setCompareBusy(false);
    }
  };

  const projectSelect = useMemo(
    () => (
      <select
        value={slug}
        onChange={(e) => setSlug(e.target.value)}
        data-testid="workflow-project"
        className="btn ghost"
        style={{ padding: "6px 10px", fontSize: 12.5 }}
      >
        {projects.length === 0 ? <option value="">{tr(lang, "(无项目)", "(no projects)")}</option> : null}
        {projects.map((p) => (
          <option key={p} value={p}>
            {p}
          </option>
        ))}
      </select>
    ),
    [projects, slug, lang],
  );

  const detailHeader = (title: React.ReactNode, desc: React.ReactNode) => (
    <header style={{ display: "flex", alignItems: "flex-start", gap: 14 }}>
      <div style={{ flex: 1 }}>
        <h1>{title}</h1>
        <p>{desc}</p>
      </div>
      {projectSelect}
    </header>
  );

  const flowRow = (name: React.ReactNode, desc: React.ReactNode, end: React.ReactNode, key: string) => (
    <div className="flow-row" key={key}>
      <span className="n">{name}</span>
      <span className="d">{desc}</span>
      <span className="end">{end}</span>
    </div>
  );

  return (
    <section className="view active row" data-testid="workflow-view">
      <div className="set-nav" data-testid="flow-nav">
        <h2>{t("flowTitle")}</h2>
        {TABS.map((it) => (
          <button
            key={it.id}
            type="button"
            data-testid={`workflow-tab-${it.id}`}
            className={`set-item ${tab === it.id ? "active" : ""}`}
            onClick={() => setTab(it.id)}
          >
            {it.icon}
            {it.labelKey ? t(it.labelKey) : it.label}
            <span className="sub">{t(it.subKey)}</span>
          </button>
        ))}
      </div>

      <div className="set-detail">
        <div className="set-detail-inner fade-in" key={tab}>
          {loading ? <p style={{ color: "var(--text-faint)", fontSize: 13 }}>{t("loading")}</p> : null}

          {tab === "skills" ? (
            <>
              {detailHeader(
                "Skills",
                zh ? (
                  <>
                    当前项目的技能库(<code>.claude/skills/</code>)—— 会话内按触发词自动调用;可从插件市场安装。
                  </>
                ) : (
                  <>
                    This project&apos;s skill library (<code>.claude/skills/</code>) — auto-triggered in
                    sessions; installable from the marketplace.
                  </>
                ),
              )}
              {skills.length === 0 && !loading ? (
                <p style={{ fontSize: 13, color: "var(--text-faint)" }}>
                  {zh
                    ? "暂无已装 skill(可从设置→插件市场安装)。"
                    : "No installed skills (install from Settings → Marketplace)."}
                </p>
              ) : (
                <div className="flow-rows">
                  {skills.map((s) =>
                    flowRow(
                      s.id,
                      s.name || s.description || "",
                      s.installed_status === "installed" ? (
                        <span className="badge ok">{t("installed")}</span>
                      ) : (
                        <button
                          type="button"
                          className="btn primary mini"
                          onClick={() => onOpenMarket?.()}
                        >
                          {t("goMarket")}
                        </button>
                      ),
                      s.id,
                    ),
                  )}
                </div>
              )}
              <div>
                <button type="button" className="btn ghost" onClick={() => onOpenMarket?.()}>
                  {t("browseMarket")}
                </button>
              </div>
            </>
          ) : null}

          {tab === "roles" ? (
            <>
              {detailHeader(
                "Roles",
                zh ? (
                  <>
                    角色库(<code>.claude/agents/&lt;role&gt;.md</code>)—— spawn 时绑 <code>--agent</code>
                    ,会话内 <code>/role</code> 原地切换。
                  </>
                ) : (
                  <>
                    Role library (<code>.claude/agents/&lt;role&gt;.md</code>) — bound at spawn via{" "}
                    <code>--agent</code>; switch in-session with <code>/role</code>.
                  </>
                ),
              )}
              {roles.length === 0 && !loading ? (
                <p style={{ fontSize: 13, color: "var(--text-faint)" }}>
                  {zh ? "暂无 role 文件。" : "No role files."}
                </p>
              ) : (
                <div className="flow-rows">
                  {roles.map((r) =>
                    flowRow(
                      r.role,
                      r.description || "",
                      r.role === "cto" ? (
                        <span className="badge brand">built-in</span>
                      ) : (
                        <span className="badge ok">{t("installed")}</span>
                      ),
                      r.role,
                    ),
                  )}
                </div>
              )}
              <div>
                <button type="button" className="btn ghost" onClick={() => onOpenMarket?.()}>
                  {t("installMarket")}
                </button>
              </div>
            </>
          ) : null}

          {tab === "mcp" ? (
            <>
              {detailHeader(
                "MCP Servers",
                zh ? (
                  <>
                    注册进各 vendor 配置的工具服务器;ccteam 自身 = 8 个 <code>mcp__ccteam__*</code> 工具,默认
                    stream-json 会话经 curated mcp-config 注入。
                  </>
                ) : (
                  <>
                    Tool servers registered into each vendor&apos;s config; ccteam itself = 8{" "}
                    <code>mcp__ccteam__*</code> tools, injected into stream-json sessions via the curated
                    mcp-config.
                  </>
                ),
              )}
              <div className="flow-rows">
                {flowRow(
                  "ccteam",
                  zh
                    ? "8 tools · status / chat_send_file / screenshot / session_* · doctor --verify-mcp 自检"
                    : "8 tools · status / chat_send_file / screenshot / session_* · doctor --verify-mcp",
                  <span className="badge ok">{t("mcpOk")}</span>,
                  "ccteam",
                )}
              </div>
              <p style={{ fontSize: 12.5, color: "var(--text-faint)" }}>
                {zh
                  ? "第三方 MCP server 注册走 设置→主机 的 register-mcp(幂等写 vendor 配置);本页只读展示。"
                  : "Third-party MCP registration lives under Settings → Hosts (idempotent register-mcp); this page is read-only."}
              </p>
            </>
          ) : null}

          {tab === "evolution" ? (
            <>
              {detailHeader(
                t("evolve"),
                zh
                  ? "v0.9 经验底座:每个 turn 落 turn record,role / skill 指纹随使用进化,后续 spawn 自动携带 —— 团队越用越懂你的项目。本版只读。"
                  : "v0.9 experience substrate: every turn writes a turn record; role / skill fingerprints evolve with use. Read-only this version.",
              )}
              {!evolution || evolution.empty ? (
                !loading ? (
                  <p style={{ fontSize: 13, color: "var(--text-faint)" }} data-testid="evolution-empty">
                    {zh ? "尚无 experience 数据(诚实空态)。" : "No experience data yet (honest empty state)."}
                  </p>
                ) : null
              ) : (
                <>
                  <div className="stat-grid">
                    <div className="stat">
                      <span className="k">turn records</span>
                      <span className="v">{evolution.turn_records}</span>
                      <span className="k">
                        {zh ? "verdicts" : "verdicts"} {evolution.verdict_records}
                      </span>
                    </div>
                    <div className="stat">
                      <span className="k">role {zh ? "指纹" : "fingerprints"}</span>
                      <span className="v">{evolution.roles.length}</span>
                      <span className="k">{evolution.roles.map((b) => b.id).join(" · ") || "—"}</span>
                    </div>
                    <div className="stat">
                      <span className="k">skill {zh ? "指纹" : "fingerprints"}</span>
                      <span className="v">{evolution.skills.length}</span>
                      <span className="k">{evolution.skills.map((b) => b.id).join(" · ") || "—"}</span>
                    </div>
                  </div>
                  <div className="flow-rows">
                    {[...evolution.roles, ...evolution.skills].map((b) =>
                      flowRow(
                        `${b.kind}:${b.id}`,
                        `turns=${b.turn_count}${b.sha ? ` · ${b.sha.slice(0, 10)}` : ""}`,
                        <span className="badge ok">{zh ? "只读" : "read-only"}</span>,
                        `${b.kind}-${b.id}-${b.sha}`,
                      ),
                    )}
                  </div>
                </>
              )}
            </>
          ) : null}

          {tab === "compare" ? (
            <>
              {detailHeader(
                "Compare",
                zh
                  ? "同一道题多 agent 并行(/compare),对比产出并给结论 —— 选型、方案评审用。"
                  : "Run the same task across agents in parallel (/compare), compare outputs.",
              )}
              <div className="form">
                <textarea
                  value={comparePrompt}
                  onChange={(e) => setComparePrompt(e.target.value)}
                  rows={3}
                  data-testid="compare-prompt"
                  placeholder={zh ? "同一问题投递给多个 agent…" : "Same prompt to multiple agents…"}
                  style={{
                    border: "1px solid var(--border)",
                    borderRadius: 12,
                    padding: "10px 12px",
                    fontSize: 14,
                    fontFamily: "inherit",
                    color: "var(--text)",
                    background: "var(--bg-card)",
                    outline: "none",
                    resize: "vertical",
                  }}
                />
                <div style={{ display: "flex", gap: 14, flexWrap: "wrap" }}>
                  {COMPARE_VENDORS.map((v) => (
                    <label
                      key={v}
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 6,
                        fontSize: 13,
                        color: "var(--text-muted)",
                        cursor: "pointer",
                      }}
                    >
                      <input
                        type="checkbox"
                        checked={compareVendors.includes(v)}
                        onChange={() => toggleVendor(v)}
                      />
                      <span className={vendorDotClass(v)} />
                      {v}
                    </label>
                  ))}
                </div>
                <div>
                  <button
                    type="button"
                    className="btn primary"
                    disabled={compareBusy}
                    data-testid="compare-run"
                    style={compareBusy ? { opacity: 0.5 } : undefined}
                    onClick={() => void onCompare()}
                  >
                    {compareBusy ? (zh ? "对比中…" : "Comparing…") : t("newCompare")}
                  </button>
                </div>
              </div>
              {compareResult ? (
                <div className="form" data-testid="compare-result">
                  <p className="mono" style={{ fontSize: 11.5, color: "var(--text-faint)" }}>
                    group={compareResult.compare_group} · cost={compareResult.cost_subtotal_usd ?? "—"}
                  </p>
                  {compareResult.slots.map((s) => (
                    <div
                      key={s.sid}
                      style={{
                        border: "1px solid var(--border)",
                        borderRadius: "var(--radius-card)",
                        padding: "12px 16px",
                        background: "var(--bg-card)",
                      }}
                    >
                      <div className="mono" style={{ fontSize: 12, marginBottom: 6, display: "flex", alignItems: "center", gap: 8 }}>
                        <span className={vendorDotClass(s.vendor)} />
                        {s.vendor} · {s.sid} · {s.status}
                      </div>
                      <pre style={{ whiteSpace: "pre-wrap", fontSize: 13, fontFamily: "inherit", color: "var(--text-muted)" }}>
                        {s.answer || s.error || "(empty)"}
                      </pre>
                    </div>
                  ))}
                </div>
              ) : null}
            </>
          ) : null}
        </div>
      </div>
    </section>
  );
}
