// Real dogfood: four harnesses each review one face of the hook+flow branch,
// then one merge leaf ranks everything. Run from the repo root:
//   ccteam flow run .agents/flows/branch-review.flow.js --max-cost 3
export const meta = {
  name: 'branch-review',
  description: 'Cross-harness review of the policy-hook + Flow feature branch',
}

phase('Review')
const leaves = [
  {
    label: 'docs-vs-hook-code',
    vendor: 'grok',
    task:
      'Read docs/hook-dynamic-workflows.md section 1 and crates/ccteam-im/src/policy.rs. ' +
      'Verify the documented hook contract (paths, replace-not-merge, exit codes, 3s budget, stdin fields, fail-closed wording) against the code. ' +
      'VERDICT first line: accurate | drift. Then at most 5 discrepancies, one line each. No code dumps.',
  },
  {
    label: 'mcp-client-edges',
    vendor: 'codex',
    task:
      'Review crates/ccteam-flow/src/mcp_client.rs for error-handling edge cases its own tests miss ' +
      '(malformed server JSON, partial turns, cursor misuse, retry windows). ' +
      'VERDICT first line: solid | risky. Then at most 5 findings with file:line, one line each. No code dumps.',
  },
  {
    label: 'examples-vs-api',
    vendor: 'dsh',
    model: 'cs/deepseek-v4-pro',
    task:
      'Read crates/ccteam-flow/src/prelude.js (the script-visible API) and every file under examples/flows/. ' +
      'Flag any example call that would throw at runtime (unknown agent option, wrong global, banned API). ' +
      'VERDICT first line: clean | broken. Then the exact offending lines, if any. No code dumps.',
  },
  {
    label: 'docs-vs-cli',
    vendor: 'claude',
    model: 'sonnet',
    task:
      'Read docs/hook-dynamic-workflows.md sections 2-3 and crates/ccteam-cli/src/flow.rs. ' +
      'Verify every documented flag and behavioral claim (stderr progress, stdout report, resume semantics, run-dir default) against the CLI code. ' +
      'VERDICT first line: accurate | drift. Then at most 5 discrepancies, one line each. No code dumps.',
  },
]
const reviews = await parallel(
  leaves.map((l) => () => agent(l.task, { vendor: l.vendor, model: l.model, label: l.label })),
)
log(`${reviews.filter(Boolean).length}/${leaves.length} reviews returned`)

phase('Merge')
const merged = await agent(
  'Merge these four per-face reviews of one feature branch into a single ranked list ' +
    '(most severe first, drop duplicates, keep each item to one line with its source label):\n' +
    JSON.stringify(
      reviews.map((r, i) => ({ face: leaves[i].label, review: r ?? '(worker failed)' })),
    ),
  { vendor: 'claude', model: 'sonnet', label: 'merge' },
)
return { merged, faces: reviews.filter(Boolean).length }
