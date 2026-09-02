// ccteam Flow: pick the harness by remaining quota, review with structured output.
// Run: ccteam flow run examples/flows/quota-routed-review.flow.js --project <slug> \
//        --args '{"file":"src/main.rs"}'
export const meta = {
  name: 'quota-routed-review',
  description: 'Quota-aware vendor choice + schema-validated review',
}

// The same per-harness quota map the status tool reports, handed to the script —
// deterministic scheduling on live account facts.
const u = await usage()
const hot = (u.claude?.windows ?? []).some((w) => w.w === '5h' && (w.pct ?? 0) >= 80)
const vendor = hot ? 'codex' : 'claude'
log(`claude 5h hot=${hot} -> reviewing with ${vendor}`)

const review = await agent(
  `Review ${args?.file ?? 'src/main.rs'} for correctness bugs. Reply with ONLY a JSON object {"verdict":"ship"|"blocked","findings":[...strings]}.`,
  {
    vendor,
    // Extraction + validation + one bounded same-session retry — never injection.
    schema: {
      type: 'object',
      required: ['verdict', 'findings'],
      properties: {
        verdict: { enum: ['ship', 'blocked'] },
        findings: { type: 'array', items: { type: 'string' } },
      },
    },
    retry: { max: 2 },
  },
)

return review ?? { verdict: 'unknown', findings: ['worker failed or never matched the schema'] }
