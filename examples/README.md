# Examples — policy hooks, ccteam flows, Claude-native bridging

Runnable companions to [docs/hook-dynamic-workflows.md](../docs/hook-dynamic-workflows.md) (中文: [-cn](../docs/hook-dynamic-workflows-cn.md)).

| dir | what | install / run |
|---|---|---|
| `hooks/` | pre-agent policy scripts (deny with a reason on stderr, exit 2) | `cp examples/hooks/quota-route.sh <project>/.ccteam/hooks/pre-agent && chmod +x <project>/.ccteam/hooks/pre-agent` — takes effect on the very next `agent` call |
| `flows/` | ccteam **Flow** scripts (deterministic JS driving real cross-harness hires) | `ccteam flow run examples/flows/audit-fanout.flow.js --project <slug>`; keep your own under `.agents/flows/` (git-shared) — **not** `.ccteam/`, which ccteam gitignores |
| `claude-native/` | Claude Code *native* dynamic-workflow scripts whose leaves hire ccteam agents over MCP | save into your project's `.claude/workflows/` and run as `/<name>`, or ask Claude for a workflow and point it at the file |

Hooks decide **whether** a hire happens; a Flow decides **what** happens next; the Claude-native bridge gives you Claude Code's workflow UI with ccteam's cross-harness leaves. All three compose: every hire in every mode still passes the pre-agent hook.
