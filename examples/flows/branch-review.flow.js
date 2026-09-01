// Real dogfood: four harnesses each review one face of the hook+flow branch,
// then one merge leaf ranks everything. Run from the repo root:
//   ccteam flow run .agents/flows/branch-review.flow.js --max-cost 3
export const meta = {
  name: 'branch-review',
  description: 'Cross-harness review of the policy-hook + Flow feature branch',
}

// flow-review EDITs applied after the first run's evaluation pass:
// 1) every leaf carries a schema — four vendors rendered the prose VERDICT
//    convention four different ways and the merge leaf had to guess;
// 2) docs-vs-hook-code moved grok -> claude/sonnet — same task shape as the
//    docs-vs-cli face, where sonnet verified against the built binary with
//    file:line while grok cited nothing;
// 3) retry {max:1} on non-claude leaves — a transient vendor failure used to
//    surface only as "(worker failed)" swallowed into the merge prompt.
const REVIEW_SCHEMA = (verdicts) => ({
  type: 'object',
  required: ['verdict', 'findings'],
  properties: {
    verdict: { enum: verdicts },
    findings: { type: 'array', items: { type: 'string' } },
  },
})
const asJson = (verdicts) =>
  ` Reply with ONLY a JSON object {"verdict": ${JSON.stringify(verdicts)} (pick one), "findings": [each finding as one string, empty when none]}.`

phase('Review')
const leaves = [
  {
    label: 'docs-vs-hook-code',
    vendor: 'claude',
    model: 'sonnet',
    fallback: 'codex',
    verdicts: ['accurate', 'drift'],
    task:
      'Read docs/hook-dynamic-workflows.md section 1 and crates/ccteam-im/src/policy.rs. ' +
      'Verify the documented hook contract (paths, replace-not-merge, exit codes, 3s budget, stdin fields, fail-closed wording) against the code. At most 5 discrepancies.',
  },
  {
    label: 'mcp-client-edges',
    vendor: 'codex',
    verdicts: ['solid', 'risky'],
    retry: { max: 1 },
    task:
      'Review crates/ccteam-flow/src/mcp_client.rs for error-handling edge cases its own tests miss ' +
      '(malformed server JSON, partial turns, cursor misuse, retry windows). At most 5 findings, each with file:line.',
  },
  {
    label: 'examples-vs-api',
    vendor: 'dsh',
    model: 'cs/deepseek-v4-pro',
    verdicts: ['clean', 'broken'],
    retry: { max: 1 },
    task:
      'Read crates/ccteam-flow/src/prelude.js (the script-visible API) and every file under examples/flows/. ' +
      'Flag any example call that would throw at runtime (unknown agent option, wrong global, banned API); exact offending lines as findings.',
  },
  {
    label: 'docs-vs-cli',
    vendor: 'claude',
    model: 'sonnet',
    fallback: 'codex',
    verdicts: ['accurate', 'drift'],
    task:
      'Read docs/hook-dynamic-workflows.md sections 2-3 and crates/ccteam-cli/src/flow.rs. ' +
      'Verify every documented flag and behavioral claim (stderr progress, stdout report, resume semantics, run-dir default) against the CLI code. At most 5 discrepancies.',
  },
]
// Policy-aware leaf: the project's pre-agent hook may refuse a vendor
// outright (live: "claude 5h window at 84% — hire codex or kimi instead",
// which nulled three faces of one run). A refusal resolves to null, so the
// script can catch it DETERMINISTICALLY and re-ask on the fallback vendor —
// the hook constrains, the flow adapts, nothing is silently lost.
const reviewLeaf = (l) => async () => {
  const first = await agent(l.task + asJson(l.verdicts), {
    vendor: l.vendor,
    model: l.model,
    label: l.label,
    schema: REVIEW_SCHEMA(l.verdicts),
    retry: l.retry,
  })
  if (first || !l.fallback) return first
  log(`${l.label}: ${l.vendor} refused (policy?) — falling back to ${l.fallback}`)
  return agent(l.task + asJson(l.verdicts), {
    vendor: l.fallback,
    label: `${l.label}:fallback`,
    schema: REVIEW_SCHEMA(l.verdicts),
    retry: { max: 1 },
  })
}
const reviews = await parallel(leaves.map(reviewLeaf))
log(`${reviews.filter(Boolean).length}/${leaves.length} reviews returned`)

phase('Merge')
const merged = await agent(
  'Merge these four per-face reviews (already structured) into a single ranked list ' +
    '(most severe first, drop duplicates, one line per item with its source label):\n' +
    JSON.stringify(
      reviews.map((r, i) => ({ face: leaves[i].label, ...(r ?? { verdict: 'worker-failed', findings: [] }) })),
    ),
  { vendor: 'claude', model: 'sonnet', label: 'merge' },
)
// Same policy-awareness for the merge seat.
const finalMerged =
  merged ??
  (await agent(
    'Merge these structured per-face reviews into one ranked list (most severe first, one line each, source label kept):\n' +
      JSON.stringify(
        reviews.map((r, i) => ({ face: leaves[i].label, ...(r ?? { verdict: 'worker-failed', findings: [] }) })),
      ),
    { vendor: 'codex', label: 'merge:fallback' },
  ))

return { merged: finalMerged, faces: reviews.filter(Boolean).length }
