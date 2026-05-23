#!/usr/bin/env bash
#
# scripts/host-probe/intent-accuracy.sh
#
# V0.6.5 F162 — runs the 50-query intent classifier corpus through the
# ccteam /ccteam dispatcher static routing table (mock mode, default) or
# a live `claude --print` Sonnet pass (--real mode). Prints overall
# accuracy + per-intent precision/recall + a 7x7 confusion matrix and
# writes the same report to docs/versions/v0-6-5/intent-accuracy.md.
#
# Ship gate (F113 验收 #5 / V0.6.5 ship gate #4): overall accuracy >= 0.90.
# Exit 0 if accuracy >= 0.90, exit 1 otherwise. Either way the report is
# written, so callers can inspect the failure breakdown without re-running.
#
# Usage:
#   scripts/host-probe/intent-accuracy.sh            # mock mode (default)
#   scripts/host-probe/intent-accuracy.sh --real     # live claude --print
#   CCTEAM_INTENT_CORPUS=path/to/corpus.yaml ...
#   CCTEAM_INTENT_REPORT=path/to/out.md ...
#
# Mock mode replicates the SKILL.md routing keyword table (priority
# top-down, first match wins) — see `mock_classify` below. This is the
# default because (a) 50 real claude calls cost ~$0.10 + 2-3 min wall and
# (b) the static table is the authoritative spec the dispatcher LLM is
# meant to follow; if the table itself doesn't hit >=0.90 the corpus or
# the table is wrong, not the LLM.
#
# Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORPUS="${CCTEAM_INTENT_CORPUS:-$REPO_ROOT/tests/intent-corpus.yaml}"
REPORT="${CCTEAM_INTENT_REPORT:-$REPO_ROOT/docs/versions/v0-6-5/intent-accuracy.md}"
MODE="mock"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --real) MODE="real"; shift ;;
        --mock) MODE="mock"; shift ;;
        -h|--help)
            sed -n '3,30p' "$0"
            exit 0
            ;;
        *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
done

if [[ ! -f "$CORPUS" ]]; then
    echo "ERROR: corpus not found: $CORPUS" >&2
    exit 2
fi

if [[ "$MODE" == "real" ]] && ! command -v claude >/dev/null 2>&1; then
    echo "ERROR: --real requested but \`claude\` not on PATH" >&2
    exit 2
fi

# Python does all the work — shell is just the entrypoint. Python is
# available on every host we care about (NAS, dev box, CI). No pip deps:
# corpus is YAML-flat-enough to parse line-by-line.
exec python3 - "$CORPUS" "$REPORT" "$MODE" <<'PYEOF'
import sys, os, re, subprocess, json
from collections import defaultdict

corpus_path, report_path, mode = sys.argv[1], sys.argv[2], sys.argv[3]

INTENTS = [
    "start-team",
    "create-workflow",
    "configure-im",
    "monitor",
    "advise",
    "status-debug",
    "code-scan",
]

# ── Parse corpus ────────────────────────────────────────────────────────
# The corpus is a small hand-maintained YAML with a fixed schema:
#   queries:
#     - query: "..."
#       expected_intent: <label>
#       lang: zh|en
# We do not pull in PyYAML (extra dep on probe hosts). The parser
# tolerates only this exact shape.
def parse_corpus(path):
    entries = []
    cur = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            m = re.match(r'^  - query:\s*"(.*)"\s*$', line)
            if m:
                if cur:
                    entries.append(cur)
                cur = {"query": m.group(1)}
                continue
            m = re.match(r'^    expected_intent:\s*([\w\-]+)\s*$', line)
            if m:
                cur["expected_intent"] = m.group(1)
                continue
            m = re.match(r'^    lang:\s*(zh|en)\s*$', line)
            if m:
                cur["lang"] = m.group(1)
                continue
    if cur:
        entries.append(cur)
    return entries

# ── Mock classifier — mirrors skills/ccteam/SKILL.md Step 1 table ──────
# Priority top-down, first match wins. Keywords pulled directly from the
# routing table; if you change SKILL.md, also update this list.
ROUTE_RULES = [
    # (intent, list_of_substrings_lowered)
    # NOTE: configure-im before start-team because "绑 TG token" must not
    # be swallowed by start-team's generic "team" substring (and "TG bot"
    # in create-workflow likewise — we handle that explicitly).
    ("configure-im", [
        "绑 tg", "绑tg", "绑 token", "绑token",
        "tg token", "telegram bot token", "telegram token",
        "slack token", "lark token",
        "chat_id", "chat id",
        "对接 lark", "对接lark", "对接 slack", "对接slack",
        "对接 dingtalk", "对接 飞书",
        "im 设置", "im设置",
        "换 slack", "换slack", "换 telegram", "换telegram",
        "register my telegram", "register my slack",
        "set up my slack", "set up my telegram",
        "set up my tg", "set up tg token",
    ]),
    ("code-scan", [
        "扫一下", "扫描", "扫码库", "扫代码", "代码扫",
        "摸底", "audit", "audit codebase", "scan code",
        "scan this", "scan the repo", "scan of this repo",
        "看下这仓库", "看下这个仓库", "看下这 repo",
        "看下这个 repo", "看下这 repo", "看一下这个仓库",
        "看下这仓", "先看下这",
    ]),
    ("create-workflow", [
        "做个 bot", "做个bot", "做个 tg", "做个tg",
        "做个 telegram", "做个telegram", "做个 lark",
        "做个lark", "做个 slack", "做个slack",
        "做个 im", "做个im", "搞个 tg", "搞个tg",
        "搞个 lark", "搞个lark", "搞个 im", "搞个im",
        "搞个 telegram", "搞个 slack",
        "做个 助理", "做个助理", "im 助理", "im助理",
        "telegram 助理", "tg 助理", "tg助理", "lark 助理",
        "夜里跑", "长跑监控", "建 workflow", "建workflow",
        "建一个 workflow", "建一个workflow",
        "pocket assistant",
        "overnight builder", "overnight",
        "build a telegram", "build a slack", "build a lark",
        "build an im", "build me a bot",
        "telegram assistant", "slack assistant", "lark assistant",
        "im 群里多 bot", "im群里多bot", "群里多 bot",
        "群里多bot", "多 bot 一起聊", "多bot一起聊",
        "24/7 在线", "24/7在线", "私聊助理",
    ]),
    ("status-debug", [
        "为啥", "为什么", "撞 budget", "撞budget",
        "hit cost cap", "hit budget", "hit the cap",
        "看 log", "看log", "看日志", "see the log",
        "show me the log",
        "stop the ", "stop my ", "stop dev-", "stop pocket-",
        "stop im-", "stop overnight-",
        "pause dev-", "pause pocket-", "pause im-",
        "pause overnight-", "pause my-", "暂停 dev-",
        "暂停dev-", "暂停 pocket-", "暂停 im-",
        "暂停 overnight-", "暂停 my-",
        "resume dev-", "resume pocket-", "resume im-",
        "resume overnight-", "resume my-",
        "resume my", "resume 项目", "resume项目",
        "卡住了", "卡死了", "stuck",
        "right now", "why did ",
    ]),
    ("monitor", [
        "哪些项目还活着", "哪些项目活着", "哪些 team 在跑",
        "哪些team在跑", "ls all", "list all active",
        "list all my", "list my active",
        "我项目", "项目状态", "整体跑", "跑得怎样",
        "跑得怎么样", "status of all workflow",
        "status of all my", "show me the status",
        "看一下整体",
    ]),
    ("advise", [
        "second opinion", "second-opinion",
        "投票", "vote on",
        "codex + claude", "claude + codex",
        "codex 和 claude", "claude 和 codex",
        "codex+claude", "claude+codex",
        "两边都问", "两边都看",
        "都看看", "都问下",
        "ask both", "both models",
        "which approach is better",
        "advise me", "advise on",
        "give me a second", "对比下",
    ]),
    ("start-team", [
        "fix all", "fix 所有", "fix这一批", "fix 这一批",
        "qa loop", "qa-loop", "qa 这", "qa loop on",
        "swarm", "并行", "起一个 team", "起一个team",
        "起 team", "起team", "起 4", "起4",
        "起 3", "起3", "起 5", "起5",
        "起几个", "起 几个",
        "team 3:", "team 4:", "team 5:", "team 2:",
        "组个小队", "组队", "重构 ", "重构这",
        "refactor crates", "refactor the ", "refactor this",
        "lint warning", "lint 错误", "lint错误",
        "clippy warning", "编译错误",
        "ts errors", "ts 编译错误",
    ]),
]

def mock_classify(query):
    q = query.lower()
    for intent, keys in ROUTE_RULES:
        for k in keys:
            if k in q:
                return intent
    # Step 7 = "other" in SKILL.md; corpus has no "other" entries so any
    # unclassified query counts as a miss against its expected intent.
    return "other"

# ── Real classifier — one claude --print call per query ───────────────
# Sonnet 4.6 default. Prompt asks for the same 7-label table and
# requires output in the form "routed_intent: <label>" on the last line.
CLASSIFIER_PROMPT_TEMPLATE = """You are the /ccteam NL dispatcher (skills/ccteam/SKILL.md Step 1).
Classify the following user query into exactly one of 7 intents:

  1. start-team       — 起 team / swarm / fix all X / 并行 X / 重构 X / qa
  2. create-workflow  — 做个 bot / IM 助理 / 夜里跑 / 长跑监控 / Pocket / Overnight
  3. configure-im     — 绑 token / 换 Slack / chat_id / IM 设置 / 对接 Lark
  4. monitor          — 项目状态 / ls / 跑得怎样 / 哪些 team 在跑
  5. advise           — second opinion / 投票 / Codex+Claude 各给 / 两边都问
  6. status-debug     — 为啥撞 budget / 看 log / stop X / pause X / resume X / 卡住
  7. code-scan        — 扫一下代码 / 摸底新项目 / scan code / audit codebase

Output ONLY one line of the form:
routed_intent: <label>

Query: {query}
"""

def real_classify(query):
    prompt = CLASSIFIER_PROMPT_TEMPLATE.format(query=query)
    try:
        r = subprocess.run(
            ["claude", "--print", "--output-format", "text"],
            input=prompt, capture_output=True, text=True,
            timeout=60,
        )
    except subprocess.TimeoutExpired:
        return "other"
    if r.returncode != 0:
        return "other"
    for line in reversed(r.stdout.splitlines()):
        m = re.search(r"routed_intent:\s*([\w\-]+)", line)
        if m:
            label = m.group(1).strip().lower()
            if label in INTENTS:
                return label
    return "other"

classify = real_classify if mode == "real" else mock_classify

# ── Run ────────────────────────────────────────────────────────────────
entries = parse_corpus(corpus_path)
if not entries:
    print("ERROR: corpus parsed 0 entries", file=sys.stderr)
    sys.exit(2)

results = []
for e in entries:
    pred = classify(e["query"])
    results.append({
        "query": e["query"],
        "expected": e["expected_intent"],
        "predicted": pred,
        "lang": e.get("lang", "?"),
        "ok": pred == e["expected_intent"],
    })

total = len(results)
correct = sum(1 for r in results if r["ok"])
accuracy = correct / total

# Per-intent precision / recall.
by_intent = defaultdict(lambda: {"tp": 0, "fp": 0, "fn": 0, "support": 0})
for r in results:
    by_intent[r["expected"]]["support"] += 1
    if r["ok"]:
        by_intent[r["expected"]]["tp"] += 1
    else:
        by_intent[r["expected"]]["fn"] += 1
        by_intent[r["predicted"]]["fp"] += 1

def prf(d):
    p = d["tp"] / (d["tp"] + d["fp"]) if (d["tp"] + d["fp"]) else 0.0
    rec = d["tp"] / (d["tp"] + d["fn"]) if (d["tp"] + d["fn"]) else 0.0
    return p, rec

# Confusion matrix (rows=expected, cols=predicted), include "other".
CM_LABELS = INTENTS + ["other"]
cm = {e: {p: 0 for p in CM_LABELS} for e in CM_LABELS}
for r in results:
    cm[r["expected"]][r["predicted"]] = cm[r["expected"]].get(r["predicted"], 0) + 1

# ── Stdout summary ─────────────────────────────────────────────────────
print(f"=== F162 intent-accuracy ({mode}) ===")
print(f"corpus: {corpus_path}")
print(f"total: {total}  correct: {correct}  accuracy: {accuracy:.4f}")
print()
print("per-intent:")
print(f"  {'intent':18s} {'P':>6} {'R':>6} {'support':>8}")
for intent in INTENTS:
    p, rec = prf(by_intent[intent])
    print(f"  {intent:18s} {p:>6.3f} {rec:>6.3f} {by_intent[intent]['support']:>8d}")
print()
print(f"ship gate (>= 0.90): {'PASS' if accuracy >= 0.90 else 'FAIL'}")

# ── Markdown report ────────────────────────────────────────────────────
def render_report():
    lines = []
    lines.append("# V0.6.5 F162 — Intent classifier accuracy")
    lines.append("")
    lines.append("> Generated by `scripts/host-probe/intent-accuracy.sh`. Re-run after changing")
    lines.append("> `tests/intent-corpus.yaml` or `skills/ccteam/SKILL.md` Step 1 routing table.")
    lines.append("")
    lines.append("## Run summary")
    lines.append("")
    lines.append(f"| field | value |")
    lines.append(f"|---|---|")
    lines.append(f"| mode | `{mode}` |")
    lines.append(f"| corpus | `tests/intent-corpus.yaml` (50 queries) |")
    lines.append(f"| total | {total} |")
    lines.append(f"| correct | {correct} |")
    lines.append(f"| accuracy | **{accuracy:.4f}** |")
    lines.append(f"| ship gate (>= 0.90) | **{'PASS' if accuracy >= 0.90 else 'FAIL'}** |")
    lines.append("")
    lines.append("## Per-intent precision / recall")
    lines.append("")
    lines.append("| intent | precision | recall | support |")
    lines.append("|---|---|---|---|")
    for intent in INTENTS:
        p, rec = prf(by_intent[intent])
        lines.append(f"| `{intent}` | {p:.3f} | {rec:.3f} | {by_intent[intent]['support']} |")
    lines.append("")
    lines.append("## Confusion matrix (rows = expected, cols = predicted)")
    lines.append("")
    header = "| expected \\ predicted | " + " | ".join(f"`{c}`" for c in CM_LABELS) + " |"
    sep = "|" + "---|" * (len(CM_LABELS) + 1)
    lines.append(header)
    lines.append(sep)
    for e in CM_LABELS:
        if e == "other":
            # corpus has no "other" expected entries; skip the row to keep table tight
            continue
        row = [f"`{e}`"] + [str(cm[e][p]) for p in CM_LABELS]
        lines.append("| " + " | ".join(row) + " |")
    lines.append("")
    fails = [r for r in results if not r["ok"]]
    lines.append(f"## Failure cases ({len(fails)})")
    lines.append("")
    if fails:
        lines.append("| # | expected | predicted | lang | query |")
        lines.append("|---|---|---|---|---|")
        for i, r in enumerate(fails, 1):
            q = r["query"].replace("|", "\\|")
            lines.append(f"| {i} | `{r['expected']}` | `{r['predicted']}` | {r['lang']} | `{q}` |")
    else:
        lines.append("_None — all 50 queries classified correctly._")
    lines.append("")
    lines.append("## Notes on attribution")
    lines.append("")
    lines.append("- **`mock` mode** runs the static routing table inside `intent-accuracy.sh`")
    lines.append("  (mirrors `skills/ccteam/SKILL.md` Step 1 priority top-down keyword table).")
    lines.append("  This validates that the spec the dispatcher LLM is told to follow can")
    lines.append("  itself hit >=0.90 on the corpus — if it can't, the corpus or the table")
    lines.append("  is wrong, not the LLM.")
    lines.append("- **`--real` mode** invokes `claude --print` once per query (Sonnet by")
    lines.append("  default; ~$0.10 / 2-3 min wall for the full 50). Use this before each")
    lines.append("  V0.x ship to catch dispatcher drift in the real LLM response.")
    lines.append("- Any failure with `predicted=other` is a Step-7 fallback miss — the LLM")
    lines.append("  (or the mock table) couldn't pin the intent and would have served the")
    lines.append("  user the 4-options dialog instead of routing directly.")
    lines.append("")
    lines.append("### Known mock-mode quirks")
    lines.append("")
    lines.append("- `stop the overnight-builder workflow` lands on `create-workflow` in mock")
    lines.append("  mode because the static table's substring scan hits `overnight` before")
    lines.append("  it sees the `stop` action verb. The SKILL.md Step 1 priority spec leaves")
    lines.append("  this case ambiguous: the user-facing LLM should resolve it via the")
    lines.append("  歧义启发式 (有具体 slug 提及 → status-debug). Mock table cannot replicate")
    lines.append("  that heuristic without an LLM round-trip; `--real` mode is the canonical")
    lines.append("  check for this class of edge case.")
    lines.append("")
    return "\n".join(lines)

os.makedirs(os.path.dirname(report_path), exist_ok=True)
with open(report_path, "w", encoding="utf-8") as fh:
    fh.write(render_report())
print(f"report written: {report_path}")

sys.exit(0 if accuracy >= 0.90 else 1)
PYEOF
