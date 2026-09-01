// A flow that evaluates another flow's finished run and proposes script
// improvements — the evaluation loop, itself expressed as a flow.
//
//   ccteam flow eval <run-dir>          # after installing this as the evaluator:
//                                       #   cp examples/flows/flow-review.flow.js \
//                                       #      .agents/flows/_eval.flow.js
//   ccteam flow run examples/flows/flow-review.flow.js \
//     --args '{"run_dir":"/home/you/.ccteam/runs/<run>"}'    # or by hand
//
// Both invocations are the same thing: `flow eval` only resolves WHICH script
// judges the run, then runs it with args.run_dir set. Output is structured on
// purpose — `examples/flows/self-review-loop.sh` gates on it with jq.
export const meta = {
  name: 'flow-review',
  description: "Grade a finished run from its journal, then propose edits to the flow script",
}

const dir = args?.run_dir
if (!dir) return { error: 'pass --args {"run_dir": "..."} — the run to review' }

// Every score is 1-10 and HIGHER IS BETTER, `waste` included: 10 means the run
// wasted nothing. A loop that gates on these has to be able to read one
// direction off the schema, not off each grader's prose.
const GRADE_SCHEMA = {
  type: 'object',
  required: ['scores', 'notes'],
  properties: {
    scores: {
      type: 'object',
      required: ['clarity', 'vendor_fit', 'waste'],
      properties: {
        clarity: { type: 'integer', minimum: 1, maximum: 10 },
        vendor_fit: { type: 'integer', minimum: 1, maximum: 10 },
        waste: { type: 'integer', minimum: 1, maximum: 10 },
      },
    },
    notes: { type: 'array', items: { type: 'string' } },
  },
}

const PATCH_SCHEMA = {
  type: 'object',
  required: ['edits'],
  properties: {
    edits: {
      type: 'array',
      items: {
        type: 'object',
        required: ['what', 'why'],
        properties: { what: { type: 'string' }, why: { type: 'string' } },
      },
    },
  },
}

phase('Grade')
// Scripts have no filesystem on purpose; the AGENT reads the run directory.
const grade = await agent(
  `Read the flow run directory ${dir} (journal.jsonl, results/, the persisted script and args). ` +
    'Establish: agents started, null results and why, cost per leaf, cache hits, the brake if any. ' +
    'Then grade the RUN 1-10 on clarity (were the tasks unambiguous), vendor_fit (was each leaf on ' +
    'the right harness/model) and waste (10 = nothing wasted, 1 = most of the spend bought nothing). ' +
    'Reply with ONLY a JSON object {"scores":{"clarity":n,"vendor_fit":n,"waste":n},' +
    '"notes":[one finding per string, each naming the evidence you scored it on]}.',
  { vendor: 'codex', label: 'grade', schema: GRADE_SCHEMA, retry: { max: 1 } },
)

phase('Improve')
const patch = await agent(
  `Here is a graded assessment of a flow run (scores are 1-10, higher is better):\n` +
    `${JSON.stringify(grade ?? { error: 'grader failed' })}\n\n` +
    `Read the persisted flow script inside ${dir} and propose at most 3 concrete edits ` +
    '(sharper task wording, a better vendor/model for a leaf, a schema where parsing was flaky, ' +
    'tighter brakes). Each edit must be applicable by someone holding only the script. ' +
    'Reply with ONLY a JSON object {"edits":[{"what":"the edit, concretely","why":"the evidence ' +
    'from this run that motivates it"}]}. No code dumps; an empty edits array is a valid answer.',
  { vendor: 'claude', model: 'sonnet', label: 'improve', schema: PATCH_SCHEMA, retry: { max: 1 } },
)

// A worker that never complied yields null (structured output is extraction,
// not enforcement) — the consumer sees the null rather than half-parsed prose.
return { grade, patch }
