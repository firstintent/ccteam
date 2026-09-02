// ccteam Flow: the classic fix-until-green loop, with honest brakes.
// Run: ccteam flow run examples/flows/ralph-until-green.flow.js --project <slug> --budget 5
export const meta = {
  name: 'ralph-until-green',
  description: 'Keep fixing ONE error per round until the check passes or progress stalls',
}

let stalled = 0
let round = 0
while (stalled < 2 && round < 12 && (budget.total === null || budget.remaining() > 0.5)) {
  round += 1
  phase(`Round ${round}`)
  const report = await agent(
    'Run `npx tsc --noEmit`. If it passes, reply exactly PASS. Otherwise fix exactly ONE ' +
      'reported error, run the check again, and reply "fixed: <the error you fixed>".',
    { vendor: 'codex' },
  )
  if (report === null) { stalled += 1; continue }          // worker-side failure
  if (report.trim().startsWith('PASS')) return { rounds: round, green: true }
  stalled = report.includes('fixed:') ? 0 : stalled + 1
  log(`round ${round}: ${report.slice(0, 80)}`)
}
return { rounds: round, green: false, stopped_by: stalled >= 2 ? 'no progress' : 'round/budget cap' }
