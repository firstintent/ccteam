# v0.8.10 dogfood bugs (post-build, core-loop)

> Newly-surfaced core-loop bugs found by dogfooding after the v0.8.10 build, recorded for v0.8.10 final-review / a follow-up fix. **file:line as of dev `a18ed68` — re-grep (dev moves).** Author = docs-only (diagnosis + record; a dev session fixes).

## DF-1 · HITL approval never reaches IM (`permission/ask` resolves dest from the registry, not the live session) — HIGH

**Symptom** (user TG 2435, live hitl session): a non-allowlist tool (`WebSearch`) in a hitl session was denied with `用户未批准该工具调用（或审批通道不可用）。Tool call not approved by the user.` / `Denied by PermissionRequest hook` — and **no `[同意][拒绝]` prompt appeared in Telegram**.

**Root cause** (confirmed on dev `a18ed68`): the `PermissionRequest` hook fires correctly (WebSearch is non-allowlist → needs approval) and forwards `permission/ask` to the daemon, but the daemon handler `execute_permission_ask` (`crates/ccteam-cli/src/main.rs:2701`) resolves the prompt destination via the **on-disk bot registry only** — `ccteam_im::resolve_home_chat(slug, role, &bots)` (`crates/ccteam-im/src/lib.rs:576`). For an IM-driven hitl session whose `(slug,role)` was never explicitly `register_bot`'d, this returns `None` → daemon returns JSON-RPC error `"permission/ask: no registered chat for {slug}/{role}"` → the hook (`crates/ccteam-hooks/src/permission_request.rs::try_permission_request` → `None`) **fail-safe denies** → no IM prompt is ever shown.

**Smoking gun** — its two siblings already got the live-session-first fix; `permission/ask` was left behind:
- `chat_send_file` (main.rs:2388): `reply_target_for(sid).or_else(|| resolve_home_chat(...))` ✅
- `interaction/ask` (main.rs:2516–2527): `guard.reply_target_for(session_sid)` then fallback `resolve_home_chat` ✅
- `permission/ask` (main.rs:2701): **registry-only — no `reply_target_for`** ← the bug.

**Fix (trivial, by analogy)**: make `execute_permission_ask` (main.rs:2701) mirror `interaction/ask` (2516–2527) — try `gateway.reply_target_for(session_sid)` **first** (it returns `(channel, chat_id)` from the live session map, no registry — `crates/ccteam-im/src/gateway.rs:2149`), fall back to `resolve_home_chat`. The handler already has `gateway` (param, 2658) + `session_sid` (2717) in scope; ~3 lines + a test (mirror the existing interaction/ask live-resolution test).

**Class**: identical to the file-send registry gap (D6) — an outbound destination must resolve from the **live session reply target**, not the registry. So this is a **D6 miss**: D6 fixed file-send + outbound-ledger but did not sweep `permission/ask`.

**Severity HIGH**: HITL approval silently fails for the common IM-driven case (the entire point of a hitl session). The deny is at least fail-safe (no un-approved tool runs — red line held), but the user has no way to approve, so the turn stalls on denials with no visible prompt = exactly the "零静默失败" / HITL-reliability target of v0.8.10.

**Workaround** (not the fix): explicitly `register_bot` for that `(slug,role)` so `resolve_home_chat` finds it → the prompt appears. Proper fix = the live-target resolution above.
