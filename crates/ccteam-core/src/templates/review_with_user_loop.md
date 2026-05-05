# Helper: review-with-user loop

> Embed this with `@~/.ccteam/templates/review-with-user-loop.md` from any
> phase that needs "read an upstream artifact, push back on it through
> multiple rounds with the user, then write a settled review". Designed
> for `decision_mode: hybrid` phases (interfaces §5.6.1) — you may use
> `AskUserQuestion` for a fast back-and-forth or fall back to outbox if
> the user is offline.

## How to use this loop

1. **Read** the artifact named in your phase YAML's `required_inputs`
   that this loop is reviewing. State out loud what it claims.
2. **Identify the three highest-leverage challenge points**. A challenge
   is a place where the artifact is silent, hand-wavy, or makes an
   unstated assumption that the user might disagree with. Don't pad
   with cosmetic nits.
3. **Surface them to the user one round at a time.** Mode picked by
   `decision_mode`:
   - `sync` / `hybrid`: call `AskUserQuestion` with the *single* most
     load-bearing question first. Wait for the answer, then move on.
   - `async`: write `outbox/clarify-<ts>-<n>.md` with the question
     (interfaces §3.4.3, `event_kind: clarify`); continue with whatever
     work you can do without the answer.
4. **Track rounds against `max_clarify_rounds`** (default 3). At the
   cap, write your best-effort review based on what you have and
   ESCALATE `INSUFFICIENT_CLARIFICATION — <last_question>`
   (interfaces §5.6.2). The user picks continue / accept / abort.
5. **Write the settled review** to the path your phase YAML's
   `required_outputs` lists for review (commonly `.ccteam/review.md`).
   Each section ties back to a challenge point and the resolution.

## What goes in the review file

- **Decision log**: each challenge, the user's answer (or your
  best-effort assumption when the user didn't reply), and the resulting
  call.
- **Open risks**: anything the loop did not close. Call out what
  triggers a revisit (e.g. "if data volume exceeds X, re-evaluate").
- **Acceptance signal**: a single line stating whether the upstream
  artifact is fit-for-purpose for the next phase. PASS / CONCERN /
  REJECT (interfaces §5.3 verdict semantics).

## Anti-patterns

- Asking three questions in one CLARIFY message — `AskUserQuestion` and
  outbox both work better with a single load-bearing question per round.
- Looping past `max_clarify_rounds` "just one more time" — the cap is
  there because long clarify chains burn token budget without
  converging. Hit the cap → ESCALATE.
- Writing the review before any challenge round — that's a rubber stamp,
  not a review. If the upstream artifact is so good it needs zero
  challenges, say so explicitly with reasoning.
