// A flow that evaluates another flow's finished run and proposes script
// improvements — the evaluation loop, itself expressed as a flow.
//   ccteam flow run examples/flows/flow-review.flow.js \
//     --args '{"run_dir":"/home/you/.ccteam/runs/<run>"}'
export const meta = {
  name: 'flow-review',
  description: "Grade a finished run from its journal, then propose edits to the flow script",
}

const dir = args?.run_dir
if (!dir) return { error: 'pass --args {"run_dir": "..."} — the run to review' }

phase('Grade')
// Scripts have no filesystem on purpose; the AGENT reads the run directory.
const grade = await agent(
  `Read the flow run directory ${dir} (journal.jsonl, results/, the persisted script and args). ` +
    'Report: agents started, null results and why, cost per leaf, cache hits, the brake if any. ' +
    'Then grade the RUN 1-10 on: task clarity, vendor fit per leaf, wasted spend. One line per grade with the reason.',
  { vendor: 'codex', label: 'grade' },
)

phase('Improve')
const patch = await agent(
  `Here is a graded assessment of a flow run:\n${grade ?? '(grader failed)'}\n\n` +
    `Read the persisted flow script inside ${dir} and propose at most 3 concrete edits ` +
    '(sharper task wording, a better vendor/model for a leaf, a schema where parsing was flaky, tighter brakes). ' +
    'Output each as: EDIT <n>: <what> — <why>. No code dumps.',
  { vendor: 'claude', model: 'sonnet', label: 'improve' },
)

return { grade, patch }
