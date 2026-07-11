// v0.8.24 A3 — Workflow top-level surface (Skills / Roles / MCP / 自进化 / Compare).
// Read-only where data exists; Compare can fan out via POST …/compare.

import { useCallback, useEffect, useMemo, useState } from "react";
import { listProjectRoles, type RoleSummary } from "../lib/sessionsApi";
import { getProjectMarketplace, type DecoratedPlugin } from "../lib/marketplaceApi";
import {
  getEvolution,
  runCompare,
  type CompareResult,
  type EvolutionSummary,
} from "../lib/workflowApi";
import { fetchDashboard } from "../lib/dashboardApi";
import { tr } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { toastBus } from "../lib/toastBus";

type TabId = "skills" | "roles" | "mcp" | "evolution" | "compare";

const TABS: { id: TabId; zh: string; en: string }[] = [
  { id: "skills", zh: "Skills", en: "Skills" },
  { id: "roles", zh: "Roles", en: "Roles" },
  { id: "mcp", zh: "MCP Servers", en: "MCP Servers" },
  { id: "evolution", zh: "自进化", en: "Evolution" },
  { id: "compare", zh: "Compare", en: "Compare" },
];

const COMPARE_VENDORS = ["claude", "codex", "grok", "opencode"] as const;

export default function WorkflowView() {
  const { settings } = useWebSettings();
  const lang = settings.language;
  const [tab, setTab] = useState<TabId>("skills");
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
      .then((rows) => {
        const slugs = rows.map((p) => p.slug).filter(Boolean);
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
    setCompareVendors((prev) =>
      prev.includes(v) ? prev.filter((x) => x !== v) : [...prev, v],
    );
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
        className="h-8 rounded-md bg-surface-800 border border-surface-700 px-2 text-xs"
      >
        {projects.length === 0 ? <option value="">(no projects)</option> : null}
        {projects.map((p) => (
          <option key={p} value={p}>
            {p}
          </option>
        ))}
      </select>
    ),
    [projects, slug],
  );

  return (
    <div className="h-full flex flex-col" data-testid="workflow-view">
      <div className="shrink-0 px-4 pt-3 pb-2 border-b border-surface-700/40 flex flex-wrap items-center gap-2">
        <div className="flex gap-1 flex-wrap">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              data-testid={`workflow-tab-${t.id}`}
              onClick={() => setTab(t.id)}
              className={`h-8 px-3 rounded-md text-xs font-medium ${
                tab === t.id
                  ? "bg-surface-700 text-text-primary"
                  : "text-text-secondary hover:bg-surface-800"
              }`}
            >
              {lang === "en" ? t.en : t.zh}
            </button>
          ))}
        </div>
        <span className="flex-1" />
        <span className="text-[11px] text-text-muted">{tr(lang, "项目", "Project")}</span>
        {projectSelect}
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto p-4">
        {loading ? (
          <p className="text-xs text-text-muted">{tr(lang, "加载中…", "Loading…")}</p>
        ) : null}

        {tab === "skills" ? (
          <section>
            <h3 className="text-sm font-semibold mb-2">
              {tr(lang, "本项目 Skills", "Project skills")}
            </h3>
            {skills.length === 0 ? (
              <p className="text-xs text-text-muted">
                {tr(
                  lang,
                  "暂无已装 skill（可从设置→插件市场安装）。",
                  "No installed skills (install from Settings → Marketplace).",
                )}
              </p>
            ) : (
              <ul className="space-y-1">
                {skills.map((s) => (
                  <li
                    key={s.id}
                    className="rounded-md border border-surface-700/50 px-3 py-2 text-xs"
                  >
                    <span className="font-mono text-brand-400">{s.id}</span>
                    {s.name ? (
                      <span className="text-text-secondary ml-2">{s.name}</span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </section>
        ) : null}

        {tab === "roles" ? (
          <section>
            <h3 className="text-sm font-semibold mb-2">
              {tr(lang, "本项目 Roles", "Project roles")}
            </h3>
            {roles.length === 0 ? (
              <p className="text-xs text-text-muted">
                {tr(lang, "暂无 role 文件。", "No role files.")}
              </p>
            ) : (
              <ul className="space-y-1">
                {roles.map((r) => (
                  <li
                    key={r.role}
                    className="rounded-md border border-surface-700/50 px-3 py-2 text-xs"
                  >
                    <span className="font-mono text-brand-400">{r.role}</span>
                    {r.description ? (
                      <span className="text-text-secondary ml-2">{r.description}</span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </section>
        ) : null}

        {tab === "mcp" ? (
          <section className="space-y-3 max-w-xl">
            <h3 className="text-sm font-semibold">MCP Servers</h3>
            <div className="rounded-md border border-surface-700/50 p-3 text-xs space-y-1">
              <div className="font-mono text-brand-400">ccteam</div>
              <p className="text-text-secondary">
                {tr(
                  lang,
                  "本进程 8 工具（status / chat_send_file / screenshot / session_*）。默认 stream-json 会话可经 curated mcp-config 注入。",
                  "In-process 8 tools (status / chat_send_file / screenshot / session_*). Default stream-json sessions inject curated mcp-config.",
                )}
              </p>
            </div>
            <p className="text-[11px] text-text-muted">
              {tr(
                lang,
                "第三方 MCP 注册入口见设置→主机 / register-mcp（本页只读展示）。",
                "Third-party MCP registration lives under Settings → Hosts / register-mcp.",
              )}
            </p>
          </section>
        ) : null}

        {tab === "evolution" ? (
          <section>
            <h3 className="text-sm font-semibold mb-2">
              {tr(lang, "自进化（只读）", "Evolution (read-only)")}
            </h3>
            {!evolution || evolution.empty ? (
              <p className="text-xs text-text-muted">
                {tr(
                  lang,
                  "尚无 experience 数据（诚实空态）。",
                  "No experience data yet (honest empty state).",
                )}
              </p>
            ) : (
              <div className="space-y-3 text-xs">
                <p className="text-text-secondary">
                  turn records:{" "}
                  <span className="font-mono">{evolution.turn_records}</span>
                </p>
                <div>
                  <div className="text-text-muted mb-1">roles</div>
                  <ul className="space-y-1">
                    {evolution.roles.map((b) => (
                      <li key={`${b.id}-${b.sha}`} className="font-mono">
                        {b.id} · turns={b.turn_count}
                        {b.sha ? ` · ${b.sha}` : ""}
                      </li>
                    ))}
                  </ul>
                </div>
                <div>
                  <div className="text-text-muted mb-1">skills</div>
                  <ul className="space-y-1">
                    {evolution.skills.map((b) => (
                      <li key={`${b.id}-${b.sha}`} className="font-mono">
                        {b.id} · turns={b.turn_count}
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            )}
          </section>
        ) : null}

        {tab === "compare" ? (
          <section className="max-w-3xl space-y-3">
            <h3 className="text-sm font-semibold">Compare</h3>
            <textarea
              value={comparePrompt}
              onChange={(e) => setComparePrompt(e.target.value)}
              rows={3}
              data-testid="compare-prompt"
              placeholder={tr(lang, "同一问题投递给多个 agent…", "Same prompt to multiple agents…")}
              className="w-full rounded-md bg-surface-800 border border-surface-700 px-3 py-2 text-sm outline-none focus:border-brand-500"
            />
            <div className="flex flex-wrap gap-2">
              {COMPARE_VENDORS.map((v) => (
                <label
                  key={v}
                  className="flex items-center gap-1.5 text-xs text-text-secondary cursor-pointer"
                >
                  <input
                    type="checkbox"
                    checked={compareVendors.includes(v)}
                    onChange={() => toggleVendor(v)}
                  />
                  {v}
                </label>
              ))}
            </div>
            <button
              type="button"
              disabled={compareBusy}
              data-testid="compare-run"
              onClick={() => void onCompare()}
              className="h-9 px-4 rounded-md bg-brand-500 text-surface-950 text-xs font-medium hover:bg-brand-400 disabled:opacity-50"
            >
              {compareBusy
                ? tr(lang, "对比中…", "Comparing…")
                : tr(lang, "发起对比", "Run compare")}
            </button>
            {compareResult ? (
              <div className="space-y-3" data-testid="compare-result">
                <p className="text-[11px] font-mono text-text-muted">
                  group={compareResult.compare_group} · cost=
                  {compareResult.cost_subtotal_usd ?? "—"}
                </p>
                {compareResult.slots.map((s) => (
                  <div
                    key={s.sid}
                    className="rounded-md border border-surface-700/50 p-3 text-xs space-y-1"
                  >
                    <div className="font-mono text-brand-400">
                      {s.vendor} · {s.sid} · {s.status}
                    </div>
                    <pre className="whitespace-pre-wrap text-text-secondary font-sans">
                      {s.answer || s.error || "(empty)"}
                    </pre>
                  </div>
                ))}
              </div>
            ) : null}
          </section>
        ) : null}
      </div>
    </div>
  );
}
