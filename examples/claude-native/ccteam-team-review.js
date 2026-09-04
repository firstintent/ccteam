// Claude Code NATIVE dynamic workflow whose leaves are ccteam hires (bridge mode).
// Save into YOUR project's .claude/workflows/ and run as /ccteam-team-review,
// in a session where the ccteam MCP server is connected.
export const meta = {
  name: 'ccteam-team-review',
  description: 'Claude-native workflow driving a cross-harness ccteam review',
  phases: [{ title: 'Survey' }, { title: 'Review' }, { title: 'Merge' }],
}

// Each agent() below is a Claude subagent acting as a BRIDGE: it loads the
// ccteam MCP tools and hires a real session, so the leaf work runs on the
// harness you name while Claude Code renders /workflows progress for the run.

const files = await agent(
  'Run `git diff --name-only dev...HEAD` and return one changed path per line, nothing else.',
  { phase: 'Survey' },
)

const reviews = await pipeline(
  files.trim().split('\n').filter(Boolean),
  (f) =>
    agent(
      'Load the ccteam tools with ToolSearch (select:mcp__ccteam__agent,mcp__ccteam__agent_read). ' +
        `Then hire codex: mcp__ccteam__agent{task:"Review ${f} for correctness bugs. VERDICT first line.", vendor:"codex", wait:240}. ` +
        'If the reply is pending, poll mcp__ccteam__agent_read{sid, wait:240} until the answer lands. ' +
        "Return ONLY the worker's final text.",
      { label: f, phase: 'Review' },
    ),
)

return await agent(
  `Merge these per-file reviews into one ranked, deduplicated list:\n${JSON.stringify(reviews.filter(Boolean))}`,
  { phase: 'Merge' },
)
