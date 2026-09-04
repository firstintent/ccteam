// ccteam Flow: discover -> parallel audit -> merge.
// Run: ccteam flow run examples/flows/audit-fanout.flow.js --project <slug>
export const meta = {
  name: 'audit-fanout',
  description: 'Discover route files, audit each in parallel on a cheap harness, merge findings',
}

phase('Discover')
const listing = await agent('List every file under src/routes/, one path per line, nothing else.')
const files = (listing ?? '').trim().split('\n').filter(Boolean)
log(`${files.length} files to audit`)

phase('Audit')
// Each thunk is a REAL hire: its own sid on the ledger, guardrails and the
// pre-agent hook included. A failed slot resolves null; the call never rejects.
const audits = await parallel(
  files.map((f) => () =>
    agent(`Audit ${f} for missing auth checks. VERDICT first line: clean | findings.`, {
      vendor: 'kimi',
      label: f,
    })),
)

phase('Merge')
return await agent(
  `Merge these audit reports into one ranked summary, dropping duplicates:\n${JSON.stringify(audits.filter(Boolean))}`,
  { vendor: 'claude' },
)
