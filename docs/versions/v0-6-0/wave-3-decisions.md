# V0.6.0 Wave 3 (F112 codex-exec-impl) — decisions log

> Wave 3 fills the V0.6 `HarnessAdapter` Codex side (`CodexExecAdapter`
> + `CodexAppServerAdapter` + UDS JSON-RPC client) plus operator-
> facing `doctor` flags + per-vendor budget caps. This document
> records the design choices and ambiguities resolved at impl time so
> Wave 4 has a paper trail. Source of truth for live behavior remains
> the code; this doc is **decisions**, not API reference.

## §1 Scope landed

| Component | File | Status |
|---|---|---|
| `codex_jsonrpc::CodexJsonRpcClient` | `crates/ccteam-core/src/execution/codex_jsonrpc.rs` | new (5 unit tests + 4 in `tests/codex_jsonrpc_test.rs`) |
| `codex_app_server::CodexAppServerAdapter` | `crates/ccteam-core/src/execution/codex_app_server.rs` | new (`HarnessAdapter` impl: start/submit/events/resume/close) |
| `codex_exec::CodexExecAdapter::{submit_turn, resume_thread}` | `crates/ccteam-core/src/execution/codex_exec.rs` | filled (Wave 1 `NotImplemented` stubs gone) |
| Per-vendor `agent_done.vendor` tag | `crates/ccteam-core/src/orchestrator.rs` | added at both emit points |
| `queries::CostSummary.{cost_24h,cost_total}_by_vendor` | `crates/ccteam-core/src/queries.rs` | additive fields, `#[serde(default)]` for legacy events |
| `enforce_budget` per-vendor cap | `crates/ccteam-core/src/orchestrator.rs` | reads `budgets_v060.{claude,codex}.max_cost_usd_per_24h` BEFORE legacy aggregate `budget` |
| `web cost_summary` per-vendor field | `crates/ccteam-web/src/routes/api_v1.rs` | `ProjectSummary.cost_24h_by_vendor` (BTreeMap) |
| `ccteam doctor --check-codex-version` | `crates/ccteam-cli/src/{main,commands}.rs` | parses `codex --version`, classifies vs `0.131` minimum |
| `ccteam doctor --check-codex-auth` | `crates/ccteam-cli/src/{main,commands}.rs` | parses `codex login status`, branches `LoggedIn / LoggedOut / Unknown` |

## §2 Decisions made at impl time

### D1. `codex_jsonrpc` wire framing = line-delimited JSON, NOT strict JSON-RPC 2.0

References: `references/codex/codex-rs/app-server-protocol/src/jsonrpc_lite.rs:1-2`
explicitly acknowledges the dialect omits `"jsonrpc": "2.0"`. Each
line is a complete JSON object. The reader-task loop in
`run_reader_loop` uses `BufReader::lines` and parses each line
independently. Garbage lines are logged at WARN and skipped (covered
by `malformed_lines_are_tolerated` test).

### D2. JSON-RPC client uses channel-based writer task, not `Mutex<WriteHalf>`

First impl tried `Box<dyn AsyncWriteExt + Unpin + Send>` but the EXT
trait isn't dyn-compatible (return types reference `Self`). Rewrote
with an mpsc<Vec<u8>> queue feeding a single writer task that owns
the concrete `OwnedWriteHalf`. Bonus: callers don't lock when the
kernel buffer back-pressures.

### D3. Notifications fan out via `tokio::sync::broadcast` (not mpsc)

Multiple subscribers per connection are useful when the adapter's
`events()` is called multiple times for the same thread (e.g. one
caller for `progress.jsonl` ingest, one for SSE). Broadcast supports
arbitrary subscriber count; the `Lagged(n)` outcome is downgraded to
`tracing::warn!` and the loop continues (event drop > stall).

### D4. `CodexExecAdapter` keeps tmux from Wave 1; per-turn `codex exec --json` is **separate**

Rationale: V0.5.1 parity for the cost-status pane (the
`CODEX_STATUS:` marker line + tmux session container). Wave 3's
per-turn `codex exec --json` subprocess runs as an INDEPENDENT child
process unrelated to the tmux pane. The `ThreadHandle.identity`
points at the tmux session (for `close_thread` parity); per-turn
event delivery goes through a per-thread
`broadcast::Sender<ThreadEvent>` keyed on `identity`.

Consequence: `start_thread` is still tmux-bound and may be called
on hosts without `codex exec --json` support. `submit_turn`'s
failure modes are independent — the subprocess errors don't kill the
tmux container.

### D5. `submit_turn` pipes prompt via stdin, not argv

`codex exec --json -` accepts the prompt via stdin, avoiding shell
escaping headaches when the prompt contains shell metacharacters or
spans many lines. Argv shape: `codex exec --json -` (or
`codex resume <id> --json -` for the resume branch).

### D6. `resume_thread` synthesises handle; deferred lookup happens at `submit_turn`

`resume_thread` does NOT verify the persistent id exists in
`~/.codex/sessions/`. It synthesises a `ThreadHandle` with
`raw_extras.resumed = true` + `raw_extras.thread_id = <id>`. The
*next* `submit_turn` call switches to the `codex resume <id>` argv
shape. Rationale: avoids a redundant FS probe + matches codex's own
lazy validation. Empty-string ids are rejected up-front.

### D7. `CodexAppServerAdapter` does NOT auto-spawn the daemon

The adapter dials `$CODEX_HOME/app-server-control/app-server-control.sock`
(overridable via `CCTEAM_CODEX_APP_SERVER_SOCKET` for tests). If the
socket is missing, `start_thread` returns `SpawnFailed` with a clear
message. Auto-spawning `codex app-server daemon start` was considered
and rejected — the daemon has a lifecycle the user should manage
through `codex app-server daemon {start,stop,restart}` directly.
`ccteam doctor --check-codex-version` is the operator's entry point
to see whether the daemon socket is reachable.

### D8. `thread/close` → `thread/archive` + `thread/unsubscribe` (NOT `thread/close`)

There is no `thread/close` method in codex's v2 protocol; the
release-resources operations are `thread/archive` (server-side state
GC) + `thread/unsubscribe` (stop receiving notifications). The
adapter calls both as best-effort and never escalates failures
(matches V0.5.x close idempotency).

### D9. `event` translation handles unknown methods via `None`

Codex's v2 protocol has dozens of notification methods (plan deltas,
file-change patch updates, MCP progress, etc.). Wave 3 propagates
only the ones that map cleanly to `ThreadEvent` (thread.started,
turn.started, turn.completed, turn.failed, item.started/updated/
completed, item/agentMessage/delta, error). Everything else returns
`None` — the orchestrator's `progress.jsonl` poller still owns state
transitions (Wave 1 contract). Adding more event types is a future
PR; the translate fn is exported `pub` so SSE / debug surfaces can
consume notifications directly.

### D10. Per-vendor cap precedence: v060 caps checked BEFORE legacy flat `budget`

`enforce_budget` reads `budgets_v060.{claude,codex}.max_cost_usd_per_24h`
first. If a per-vendor cap trips, we emit
`budget_exceeded {kind: "cost_24h_per_vendor", vendor: <key>}` and
auto-disable. Only after both per-vendor checks pass do we fall
through to the legacy `spec.budget.max_cost_usd_per_24h` flat check.
Rationale: per-vendor is the V0.6+ intended path; the flat cap stays
as a compatibility fallback for projects that haven't migrated YAML.

### D11. `agent_done` vendor derivation: `SessionHandle.harness` prefix

The poller path (`poll_completions`) only sees `SessionHandle`, not
`AgentVendor`. We derive vendor by string-prefix:
`harness.starts_with("codex")` → "codex" else "claude". Exported as
`pub fn vendor_from_harness` for testability + future call sites.
Translation path (`translate_thread_event`) takes the `AgentVendor`
directly since it already has the trait-level type.

### D12. `ccteam_cost::estimate_cost` model fallback

`translate_thread_event` doesn't know the model id (`ThreadEvent`
omits it). We pass `""` and rely on `ccteam_cost::estimate_cost`'s
unknown-model fallback (per-vendor default price). The pricing crate
already emits a WARN-once when this triggers. Wave 4 should plumb
the model through `SpawnCtx` so per-event pricing is exact.

### D13. Doctor flags: stdout-only, never fail the exit code

Both `--check-codex-version` and `--check-codex-auth` emit
human-readable reports and return `Ok(())`. Even an absent codex
binary surfaces as `[ERROR]` in stdout but doesn't propagate up to
`anyhow::bail!` — same convention as V0.3.1 F47's informational
`which codex` line. Rationale: doctor is for operators; CI should
have its own gates.

### D14. `classify_codex_auth` checks "not logged in" BEFORE "logged in"

"Not logged in" CONTAINS "logged in" as a substring. The classifier
must short-circuit on the negative branch first. Discovered via test
`check_codex_auth_warns_when_not_logged_in` flake during impl;
captured in inline comments on `classify_codex_auth`.

## §3 Test scaffolding

| File | Tests | Hermetic? |
|---|---|---|
| `crates/ccteam-core/tests/codex_jsonrpc_test.rs` | 5 | Yes (tokio duplex peer) |
| `crates/ccteam-core/tests/codex_app_server_test.rs` | 6 | Yes (UDS socket in tempdir, `#[serial]` for env-var tests) |
| `crates/ccteam-core/tests/codex_exec_wave3_test.rs` | 8 | Yes (fake codex bash script under tempdir, `CCTEAM_CODEX_BIN` env override) |
| `crates/ccteam-core/tests/per_vendor_budget_test.rs` | 5 | Yes (pure-event-slice helper) |
| `crates/ccteam-cli/tests/doctor_codex_test.rs` | 4 | Yes (PATH manipulated to fake codex tempdir) |

Tests that mutate env-vars are either `#[serial]`-gated (Tokio
adapter tests) or use `PATH` overrides (binary surface tests). The
`harness_trait_test.rs` Wave 1 NotImplemented stub assertion was
updated to assert Wave 3 behavior (synthetic TurnId via fake codex
bin = `/bin/true`).

## §4 Known limitations / Wave 4 follow-ups

1. **`codex resume` argv unverified against real codex 0.131** — the
   resume branch (`codex resume <id> --json -`) is sketched per docs
   but not E2E-tested in this wave. Wave 4 should integration-test
   the resume path against a real codex install with a known thread
   id in `~/.codex/sessions/`.

2. **`CodexAppServerAdapter` notification → progress.jsonl bridge
   missing.** The adapter emits `ThreadEvent`s into its broadcast
   subscriber stream, but the orchestrator does not yet drive them
   into `progress.jsonl`. e2e-wiring teammate's mode-3 codex bot
   dispatch will land that bridge (Wave 1 noted mode-3 codex is
   not user-configurable today; mode-3 claude is the only path).

3. **Daemon auto-spawn deferred** — adapters fail loud when the UDS
   socket is missing. Operator runs `codex app-server daemon start`
   manually before enabling mode-3 codex. Could change in Wave 4 if
   demand warrants.

4. **`thread/start` params minimal** — we send `cwd`,
   `session_source: "ccteam"`, `service_name`, and
   `developer_instructions`. Codex's v2 `ThreadStartParams` has 20+
   fields (sandbox, approval_policy, permissions, model overrides
   etc.). Future PRs should plumb workflow.yaml-level codex config
   through `SpawnCtx`.

5. **Pricing model id pass-through** — see D12. Cost numbers in
   `agent_done` for codex events fall back to per-vendor default
   pricing rather than per-model. Plumbing model id costs ~3 fields
   on `SpawnCtx` + adapter side.

## §5 Acceptance script results

- baseline `cargo test --workspace --locked --no-fail-fast` → **1243
  passed / 1 failed** (the 1 failure is the V0.6.0 baseline
  `workflow_summary_reflects_agent_spawn_and_done_events` running_count
  flake, NOT regression). +42 over Wave 2 baseline 1201/1.
- clippy: 17 ccteam-core warnings (all pre-existing doc-list drift +
  one `type_complexity`); 0 new warnings. Below the 19-cap.
- NotImplemented stubs removed from `codex_exec.rs` (Wave 3 fills
  both `submit_turn` and `resume_thread`).
- New files: `codex_app_server.rs`, `codex_jsonrpc.rs`, plus 5 test
  files (`codex_jsonrpc_test`, `codex_app_server_test`,
  `codex_exec_wave3_test`, `per_vendor_budget_test`,
  `doctor_codex_test`).
- Doctor flags: `--check-codex-version` + `--check-codex-auth`
  wired through clap + DoctorOptions + run_doctor; their reports
  appear in stdout.
- Per-vendor budget: `cost_24h_by_vendor` field on `CostSummary` +
  `ProjectSummary` (web API); enforce_budget checks per-vendor
  before falling through to legacy `spec.budget`.
